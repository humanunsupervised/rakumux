use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::auth::{
    AccountService, AccountSession, AccountWebsite, AuthService, AuthStatus, ClerkDeviceClient,
    CredentialStore, LoginTicket, PollCompletion, VerificationPrompt, WallClock,
};

/// The longest wait between cancellation checks while waiting to poll.
const CHECK: Duration = Duration::from_millis(250);

/// Where a device sign-in is up to, for the TUI to show. Holds no device code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInState {
    /// No sign-in has been started in this service.
    Idle,
    /// Asking the provider for a code.
    Starting,
    /// Waiting for the user to approve `prompt` in a browser.
    Waiting(SignInPrompt),
    /// The account is signed in and adopted.
    SignedIn,
    /// The attempt ended without signing in.
    Failed(SignInFailure),
}

/// What the user needs to approve a sign-in in any browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInPrompt {
    /// The public code to match on the approval page.
    pub user_code: String,
    /// The provider's approval page.
    pub verification_uri: String,
    /// The approval page with the code filled in, when the provider gives one.
    pub verification_uri_complete: Option<String>,
    /// The Rakumux website's sign-in-then-approve link, when configured.
    pub connect_url: Option<String>,
    /// Seconds the code is valid for, from when it was issued.
    pub expires_in_seconds: u64,
}

/// Why a sign-in ended. Static: no provider or network text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInFailure {
    /// The user denied the request.
    Denied,
    /// The code expired before it was approved.
    Expired,
    /// The provider couldn't be reached.
    Unavailable,
    /// A newer sign-in or a cancel replaced this one.
    Canceled,
    /// The provider's answer couldn't be verified or saved.
    Failed,
}

/// Progress reported from a sign-in thread to the owner thread.
pub(super) enum SignInEvent {
    Prompt(SignInPrompt),
    SignedIn(AccountSession),
    Failed(SignInFailure),
}

/// Run one device sign-in on its own thread: a sign-in can wait up to the code's
/// lifetime (ten minutes), so it never occupies a refresh worker.
pub(super) fn spawn<A, C>(
    account: Arc<AccountService<A, C>>,
    website: Option<AccountWebsite>,
    cancel: Arc<AtomicBool>,
    report: impl Fn(SignInEvent) + Send + 'static,
) -> std::io::Result<()>
where
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    thread::Builder::new()
        .name("rakumux-status-sign-in".into())
        .spawn(move || {
            let ticket = match account.begin_login() {
                Ok(ticket) => ticket,
                Err(_) => return report(SignInEvent::Failed(SignInFailure::Failed)),
            };
            match sign_in(&account, &ticket, website.as_ref(), &cancel, &report) {
                Ok(session) => report(SignInEvent::SignedIn(session)),
                Err(failure) => {
                    // An unfinished login never disturbs an existing stored account.
                    let _ = account.cancel_login(&ticket);
                    report(SignInEvent::Failed(failure));
                }
            }
        })
        .map(|_| ())
}

fn sign_in<A: CredentialStore, C: WallClock>(
    account: &AccountService<A, C>,
    ticket: &LoginTicket,
    website: Option<&AccountWebsite>,
    cancel: &AtomicBool,
    report: &impl Fn(SignInEvent),
) -> Result<AccountSession, SignInFailure> {
    let client = ClerkDeviceClient::discover(account.config().clone())
        .map_err(|_| SignInFailure::Unavailable)?;
    canceled(cancel)?;
    let mut auth = AuthService::default();
    let attempt = auth.start().map_err(|_| SignInFailure::Failed)?;
    let clock = Instant::now();
    auth.complete_start(attempt, client.start(), clock.elapsed());
    let prompt = match auth.status(clock.elapsed()) {
        AuthStatus::AwaitingApproval { prompt, .. } => prompt,
        status => return Err(failure(&status)),
    };
    report(SignInEvent::Prompt(SignInPrompt {
        user_code: prompt.user_code().to_owned(),
        verification_uri: prompt.verification_uri().to_string(),
        verification_uri_complete: prompt.verification_uri_complete().map(ToString::to_string),
        connect_url: connect_url(website, &prompt),
        expires_in_seconds: match auth.status(clock.elapsed()) {
            AuthStatus::AwaitingApproval { expires_at, .. } => {
                expires_at.saturating_sub(clock.elapsed()).as_secs()
            }
            _ => 0,
        },
    }));
    loop {
        canceled(cancel)?;
        let now = clock.elapsed();
        match auth.status(now) {
            AuthStatus::AwaitingApproval { next_poll_at, .. } if now < next_poll_at => {
                thread::sleep((next_poll_at - now).min(CHECK));
                continue;
            }
            AuthStatus::AwaitingApproval { .. } => {}
            status => return Err(failure(&status)),
        }
        let Some(request) = auth.next_poll(now) else {
            thread::sleep(CHECK);
            continue;
        };
        let outcome = client.poll(&request);
        canceled(cancel)?;
        match auth.complete_poll(request.id(), outcome, clock.elapsed()) {
            PollCompletion::Authorized(grant) => {
                let verified = client
                    .verify_account(grant)
                    .map_err(|_| SignInFailure::Failed)?;
                canceled(cancel)?;
                return account
                    .commit_login(ticket, verified)
                    .map_err(|_| SignInFailure::Failed);
            }
            PollCompletion::Applied => {}
            PollCompletion::Ignored | PollCompletion::DiscardedGrant(_) => {
                return Err(SignInFailure::Failed);
            }
        }
    }
}

fn connect_url(website: Option<&AccountWebsite>, prompt: &VerificationPrompt) -> Option<String> {
    website?.connect_url(prompt).ok().map(|url| url.to_string())
}

fn canceled(cancel: &AtomicBool) -> Result<(), SignInFailure> {
    if cancel.load(Ordering::Acquire) {
        Err(SignInFailure::Canceled)
    } else {
        Ok(())
    }
}

fn failure(status: &AuthStatus) -> SignInFailure {
    match status {
        AuthStatus::Denied => SignInFailure::Denied,
        AuthStatus::Expired => SignInFailure::Expired,
        AuthStatus::Cancelled => SignInFailure::Canceled,
        _ => SignInFailure::Failed,
    }
}
