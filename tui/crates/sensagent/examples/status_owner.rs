//! Developer-only AI-status owner for one Rakumux home. Requires explicit public
//! configuration; nothing here is installed, spawned by the TUI or enabled by default.

use std::error::Error;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::sync::Arc;

use rakumux_sensagent::auth::{
    AccountService, ClerkDeviceClient, DeviceAuthConfig, FileCredentialStore,
};
use rakumux_sensagent::cloud::{CloudAIClient, CloudAIConfig};
use rakumux_sensagent::dispatch::StatusDispatchService;
use rakumux_sensagent::owner::protocol::FolderChoice;
use rakumux_sensagent::owner::{OwnerClaim, OwnerClient, OwnerLayout, OwnerServer};
use rakumux_sensagent::refresh::{RefreshRunner, StatusRefresher};
use rakumux_sensagent::status::SqliteStatusStore;
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use session_core::paths::Layout;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "AI-status owner developer example (not installed, not the TUI)\n\
Usage:\n\
  status_owner serve --issuer HTTPS_URL --client-id PUBLIC_ID --verification-uri HTTPS_DEVICE_PAGE --cloud-origin HTTPS_ORIGIN [--userinfo-uri HTTPS_URL]\n\
  status_owner state\n\
  status_owner statuses SESSION_ID...\n\
  status_owner enable [--output] FOLDER...\n\
  status_owner disable\n\
  status_owner sign-out\n\
  status_owner renew\n\
\n\
Uses $RAKUMUX_HOME (default ~/.rakumux): set it to a temporary directory to stay\n\
away from your real sessions. `serve` needs a running rakumux-ptyd for that home\n\
and restores credentials saved there by the device_login example\n\
(--credentials-dir $RAKUMUX_HOME/sensagent/credentials). Sharing starts only after\n\
`enable`, which shows the account, destination and consent revision and asks first.\n\
Production sign-in and inference are not enabled by this example.\n";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("status_owner: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        io::stdout().write_all(HELP.as_bytes())?;
        return Ok(());
    };
    let home = Layout::from_env()?;
    let layout = OwnerLayout::new(&home);
    match command.as_str() {
        "-h" | "--help" => io::stdout().write_all(HELP.as_bytes())?,
        "serve" => serve(&home, &layout, rest)?,
        "state" => state(&connect(&layout)?)?,
        "statuses" => statuses(&connect(&layout)?, rest)?,
        "enable" => enable(&connect(&layout)?, rest)?,
        "disable" => {
            let client = connect(&layout)?;
            let revision = client.state()?.consent.revision;
            let consent = client.disable(revision)?;
            println!("Sharing off (consent revision {}).", consent.revision);
        }
        "sign-out" => {
            connect(&layout)?.sign_out()?;
            println!("Signed out; sharing stopped.");
        }
        "renew" => {
            connect(&layout)?.renew()?;
            println!("Renewing the sign-in; `state` shows the new expiry.");
        }
        _ => return Err(io::Error::other("unknown command; use --help").into()),
    }
    Ok(())
}

fn connect(layout: &OwnerLayout) -> Result<OwnerClient, Box<dyn Error>> {
    Ok(OwnerClient::connect(layout, "status_owner-example")?)
}

fn serve(home: &Layout, layout: &OwnerLayout, args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut issuer = None;
    let mut client_id = None;
    let mut verification_uri = None;
    let mut cloud_origin = None;
    let mut userinfo_uri = None;
    let (pairs, remainder) = args.as_chunks::<2>();
    for pair in pairs {
        let slot = match pair[0].as_str() {
            "--issuer" => &mut issuer,
            "--client-id" => &mut client_id,
            "--verification-uri" => &mut verification_uri,
            "--cloud-origin" => &mut cloud_origin,
            "--userinfo-uri" => &mut userinfo_uri,
            _ => return Err(io::Error::other("unrecognized option; use --help").into()),
        };
        if slot.replace(pair[1].clone()).is_some() {
            return Err(io::Error::other("duplicate option; use --help").into());
        }
    }
    if !remainder.is_empty() {
        return Err(io::Error::other("missing option value; use --help").into());
    }
    let required = |value: Option<String>| {
        value.ok_or_else(|| {
            io::Error::other("explicit public configuration is required; use --help")
        })
    };
    let mut auth = DeviceAuthConfig::new(
        &required(issuer)?,
        &required(client_id)?,
        &required(verification_uri)?,
        &["openid", "email", "offline_access", "ai:session-status"],
    )?;
    if let Some(endpoint) = userinfo_uri {
        auth = auth.with_userinfo_endpoint(&endpoint)?;
    }
    let cloud = CloudAIClient::new(CloudAIConfig::new(&required(cloud_origin)?)?)?;

    let mut claim = OwnerClaim::claim(layout, VERSION)?;
    let store = SqliteStatusStore::open(layout.status_dir())?;
    let account = Arc::new(AccountService::new(
        auth.clone(),
        FileCredentialStore::new(layout.credentials_dir()),
    ));
    let mut service = StatusDispatchService::new(account.clone(), store, cloud);
    // Restoring verifies the saved account online; offline, the owner runs signed out.
    let session = match ClerkDeviceClient::discover(auth) {
        Ok(client) => account.restore(&client).unwrap_or_else(|error| {
            eprintln!("Continuing signed out: {error}");
            None
        }),
        Err(error) => {
            eprintln!("Continuing signed out: {error}");
            None
        }
    };
    if session.is_none() {
        println!("Signed out. Sign in with the device_login example, then restart.");
    }
    service.set_account_session(session)?;

    let api: Arc<dyn SessionApi> = Arc::new(Client::connect(ClientConfig {
        layout: home.clone(),
        client_name: "sensagent".into(),
        version: VERSION.into(),
        auth: AuthMode::User,
        spawn: false,
        ptyd_path: None,
    })?);
    let runner = RefreshRunner::spawn(StatusRefresher::new(service), api)?;
    let server = OwnerServer::start(&mut claim, runner.client(), VERSION)?;
    println!("Serving AI status for this Rakumux home. Press Enter to stop.");
    let _ = io::stdin().lock().lines().next();
    server.stop();
    runner.stop();
    drop(claim);
    Ok(())
}

fn state(client: &OwnerClient) -> Result<(), Box<dyn Error>> {
    let state = client.state()?;
    match &state.account {
        Some(account) => println!(
            "Account: {} (token expires at {})",
            account.email.as_deref().unwrap_or("(no email)"),
            account
                .expires_at
                .map_or_else(|| "?".into(), |at| at.to_string())
        ),
        None => println!("Account: signed out"),
    }
    println!(
        "Sharing: {} ({} folders, consent revision {})",
        if state.consent.enabled { "on" } else { "off" },
        state.consent.folders.len(),
        state.consent.revision
    );
    println!(
        "Daemon: {}{}{}",
        if state.connected {
            "followed"
        } else {
            "not connected"
        },
        if state.rate_limited {
            "; rate limited"
        } else {
            ""
        },
        if state.unauthorized {
            "; account refused, sign in again"
        } else {
            ""
        },
    );
    Ok(())
}

fn statuses(client: &OwnerClient, ids: &[String]) -> Result<(), Box<dyn Error>> {
    let ids: Vec<SessionId> = ids.iter().map(SessionId::from_string).collect();
    for line in client.statuses(&ids)? {
        println!(
            "{}: {:?} {}{}",
            line.session,
            line.state,
            line.text.as_deref().unwrap_or(""),
            line.failure
                .map(|failure| format!(" (last refresh: {failure:?})"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn enable(client: &OwnerClient, args: &[String]) -> Result<(), Box<dyn Error>> {
    let include_terminal_output = args.iter().any(|arg| arg == "--output");
    let mut folders = Vec::new();
    for path in args.iter().filter(|arg| *arg != "--output") {
        folders.push(FolderChoice {
            folder: ScopeRoot::resolve(std::path::Path::new(path))?,
            include_terminal_output,
        });
    }
    if folders.is_empty() {
        return Err(io::Error::other("name at least one folder; use --help").into());
    }
    let offer = client.consent_offer()?;
    println!(
        "Account: {}",
        offer
            .account
            .email
            .as_deref()
            .unwrap_or(&offer.account.subject)
    );
    println!("Destination: {}", offer.origin);
    println!("Consent revision: {}", offer.consent_revision);
    println!(
        "Share {} for {} folder(s) with this AI service? Type yes:",
        if include_terminal_output {
            "session reports and recent terminal output"
        } else {
            "session reports only"
        },
        folders.len()
    );
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    if answer.trim() != "yes" {
        println!("Nothing changed.");
        return Ok(());
    }
    let consent = client.enable(&offer.id, folders)?;
    println!("Sharing on (consent revision {}).", consent.revision);
    Ok(())
}
