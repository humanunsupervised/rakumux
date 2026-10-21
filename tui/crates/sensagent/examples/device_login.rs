//! Developer-only native device login. Requires explicit public configuration.
//! No browser opener, daemon launch, default home or provider setup is performed.

use rakumux_sensagent::auth::{
    AccountService, AccountWebsite, AuthService, AuthStatus, ClerkDeviceClient, DeviceAuthConfig,
    FileCredentialStore, LoginTicket, PollCompletion,
};
use std::error::Error;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const HELP: &str = "Native device-login developer example (not the TUI)\n\
Usage: device_login --issuer HTTPS_URL --client-id PUBLIC_ID --verification-uri HTTPS_DEVICE_PAGE --credentials-dir PRIVATE_DIRECTORY\n\
Optional: --userinfo-uri HTTPS_USERINFO_URL (reviewed provider settings)\n\
Optional: --website-origin HTTPS_ORIGIN (reviewed Rakumux account website)\n\
\n\
Uses Clerk Device Grant; the optional branded link retains the direct Clerk fallback.\n\
The credential directory's trusted parent must already exist.\n\
No credentials are printed. A real attempt requires an already configured public\n\
OAuth test client supporting openid email offline_access ai:session-status.\n";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Device login failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|value| value == "--help" || value == "-h") {
        io::stdout().write_all(HELP.as_bytes())?;
        return Ok(());
    }
    let mut issuer = None;
    let mut client_id = None;
    let mut verification_uri = None;
    let mut directory = None;
    let mut userinfo_uri = None;
    let mut website_origin = None;
    let (pairs, remainder) = args.as_chunks::<2>();
    for pair in pairs {
        let slot = match pair[0].as_str() {
            "--issuer" => &mut issuer,
            "--client-id" => &mut client_id,
            "--verification-uri" => &mut verification_uri,
            "--credentials-dir" => &mut directory,
            "--userinfo-uri" => &mut userinfo_uri,
            "--website-origin" => &mut website_origin,
            _ => {
                return Err(
                    io::Error::other("unrecognized configuration option; use --help").into(),
                )
            }
        };
        if slot.replace(pair[1].clone()).is_some() {
            return Err(io::Error::other("duplicate configuration option; use --help").into());
        }
    }
    if !remainder.is_empty() {
        return Err(io::Error::other("missing configuration value; use --help").into());
    }
    let required = |value: Option<String>| {
        value.ok_or_else(|| {
            io::Error::other("explicit public configuration is required; use --help")
        })
    };
    let mut config = DeviceAuthConfig::new(
        &required(issuer)?,
        &required(client_id)?,
        &required(verification_uri)?,
        &["openid", "email", "offline_access", "ai:session-status"],
    )?;
    if let Some(endpoint) = userinfo_uri {
        config = config.with_userinfo_endpoint(&endpoint)?;
    }
    let website = website_origin
        .as_deref()
        .map(AccountWebsite::new)
        .transpose()?;
    let store = FileCredentialStore::new(PathBuf::from(required(directory)?));
    let account = AccountService::new(config.clone(), store);
    let ticket = account.begin_login()?;
    let result = login(config, &account, &ticket, website.as_ref());
    if result.is_err() {
        if let Err(error) = account.cancel_login(&ticket) {
            eprintln!("Unable to invalidate the unfinished local login: {error}");
        }
    }
    result
}

fn login(
    config: DeviceAuthConfig,
    account: &AccountService<FileCredentialStore>,
    ticket: &LoginTicket,
    website: Option<&AccountWebsite>,
) -> Result<(), Box<dyn Error>> {
    let client = ClerkDeviceClient::discover(config)?;
    let mut auth = AuthService::default();
    let attempt = auth.start()?;
    let clock = Instant::now();
    auth.complete_start(attempt, client.start(), clock.elapsed());
    let AuthStatus::AwaitingApproval { prompt, .. } = auth.status(clock.elapsed()) else {
        return Err(status_error(auth.status(clock.elapsed())).into());
    };
    let mut output = io::stdout();
    writeln!(output, "Open: {}", prompt.verification_uri())?;
    writeln!(output, "Confirm this public code: {}", prompt.user_code())?;
    if let Some(url) = prompt.verification_uri_complete() {
        writeln!(output, "Convenience link: {url}")?;
    }
    if let Some(website) = website {
        match website.connect_url(&prompt) {
            Ok(url) => writeln!(output, "Rakumux account link: {url}")?,
            Err(error) => writeln!(
                output,
                "Branded link unavailable: {error}. Use the Clerk link and code above."
            )?,
        }
    }
    writeln!(
        output,
        "Approve only the code from this terminal. Waiting for Clerk; Ctrl-C exits."
    )?;
    output.flush()?;
    loop {
        let now = clock.elapsed();
        match auth.status(now) {
            AuthStatus::AwaitingApproval {
                next_poll_at,
                expires_at,
                ..
            } => {
                if now < next_poll_at {
                    std::thread::sleep(
                        (next_poll_at - now)
                            .min(expires_at - now)
                            .min(Duration::from_millis(500)),
                    );
                    continue;
                }
            }
            status => return Err(status_error(status).into()),
        }
        let Some(request) = auth.next_poll(clock.elapsed()) else {
            continue;
        };
        let outcome = client.poll(&request);
        match auth.complete_poll(request.id(), outcome, clock.elapsed()) {
            PollCompletion::Authorized(grant) => {
                let verified = client.verify_account(grant)?;
                let session = account.commit_login(ticket, verified)?;
                writeln!(output, "Signed in as {}", session.account().subject())?;
                return Ok(());
            }
            PollCompletion::Applied => {}
            PollCompletion::Ignored | PollCompletion::DiscardedGrant(_) => {
                return Err(
                    io::Error::other("authorization became obsolete; start a new attempt").into(),
                );
            }
        }
    }
}

fn status_error(status: AuthStatus) -> io::Error {
    match status {
        AuthStatus::Failed(error) => io::Error::other(error),
        AuthStatus::Denied => io::Error::other("the user denied authorization"),
        AuthStatus::Expired => io::Error::other("the pending authorization expired"),
        AuthStatus::Cancelled => io::Error::other("authorization was cancelled"),
        _ => io::Error::other("unexpected authorization state; start a new attempt"),
    }
}
