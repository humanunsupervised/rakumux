//! The Rakumux account, AI session descriptions and the agent feed. A
//! per-home service (`rakumux status-service`, `rakumux_sensagent::service`)
//! holds the sign-in, follows the daemon's sessions, describes them and
//! keeps the feed; the TUI and `rakumux account` are its clients. It starts
//! on demand: when someone signs in, or when this home has a saved account.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use rakumux_sensagent::feed::{FeedPage, FeedStatus, FeedTurn};
use rakumux_sensagent::owner::protocol::{OwnerState, SignInReason, SignInView, StatusLine};
use rakumux_sensagent::owner::{OwnerClient, OwnerError, OwnerLayout};
use rakumux_sensagent::service::{self, ServiceConfig, ServiceError};
use rakumux_sensagent::source::TurnSettings;
use session_core::ids::SessionId;
use session_core::paths::Layout;

pub use rakumux_sensagent::owner::protocol::{LineState, MAX_SESSIONS};

/// The signed-in account, as the service reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub email: Option<String>,
    /// The provider's stable id, shown when there's no email.
    pub subject: String,
    /// The account's AI session summaries switch (rakumux.com/features);
    /// `None` until read.
    pub summaries: Option<bool>,
    /// The account's Feed switch (rakumux.com/features); `None` until read.
    pub feed: Option<bool>,
    /// The last change to the switches couldn't be saved to the account.
    pub features_unsaved: bool,
}

impl Account {
    fn from_state(state: OwnerState) -> Option<Self> {
        let (summaries, feed) = (state.summaries, state.feed);
        let features_unsaved = state.features_unsaved;
        state.account.map(|account| Self {
            email: account.email,
            subject: account.subject,
            summaries,
            feed,
            features_unsaved,
        })
    }

    /// The email, else the provider's id.
    pub fn name(&self) -> &str {
        self.email.as_deref().unwrap_or(&self.subject)
    }
}

/// What the TUI does with the account and its AI descriptions; blocking, so
/// the TUI runs it off the UI thread. Tests use a fake.
pub trait AccountAccess: Send + Sync {
    /// The signed-in account; `None` when signed out. Starts the service
    /// only when this home has a saved account.
    fn account(&self) -> Result<Option<Account>, String>;
    /// Start a device sign-in (starting the service when needed).
    fn sign_in(&self) -> Result<SignInView, String>;
    /// Where the sign-in is up to.
    fn sign_in_status(&self) -> Result<SignInView, String>;
    /// Abandon an unfinished sign-in.
    fn cancel_sign_in(&self) -> Result<(), String>;
    /// Stop describing sessions and remove this machine's sign-in.
    fn sign_out(&self) -> Result<(), String>;
    /// The AI descriptions of `sessions` (at most [`MAX_SESSIONS`]), in
    /// order; empty when the service isn't running.
    fn descriptions(&self, sessions: &[SessionId]) -> Result<Vec<StatusLine>, String>;
    /// The feed entries that changed after `after` for a client holding
    /// `generation`. Asking says the feed is shown (the service reads it
    /// from rakumux.com more often). Starts the service only when this home
    /// has a saved account; signed out without one.
    fn feed(&self, after: Option<u64>, generation: Option<u64>) -> Result<FeedPage, String>;
    /// Make a new AI summary of `session` now (the refresh icon or `s`). Starts
    /// the service only when this home has a saved account; signed out without
    /// one, it does nothing.
    fn summarize(&self, session: &SessionId) -> Result<(), String>;
    /// Turn the account's AI session summaries or Agent feed switch on or
    /// off (`None` leaves one as it is), saved to the account as on
    /// rakumux.com/features. The service applies it at once and saves it
    /// next; [`Account::features_unsaved`] says when that failed.
    fn set_features(&self, summaries: Option<bool>, feed: Option<bool>) -> Result<(), String>;
    /// The whole turn feed entry `entry` finished, past `after` when given:
    /// the prompt, the agent's text, reasoning and tool calls, and the tools'
    /// output, in order, as rakumux.com keeps it.
    fn feed_turn(&self, entry: &str, after: Option<u64>) -> Result<FeedTurn, String>;
}

/// The feed of a home whose service isn't running: signed out.
pub fn signed_out_feed() -> FeedPage {
    FeedPage {
        status: FeedStatus::SignedOut,
        generation: 0,
        revision: 0,
        entries: Vec::new(),
        more: false,
    }
}

/// The service of this machine's Rakumux home, started from `program`
/// (this binary) when needed. One connection, reopened after a failure.
pub struct LocalAccount {
    layout: Layout,
    program: PathBuf,
    client_name: &'static str,
    /// This build's version: an older running service is replaced.
    version: &'static str,
    client: Mutex<Option<OwnerClient>>,
}

impl LocalAccount {
    pub fn new(
        layout: Layout,
        program: PathBuf,
        client_name: &'static str,
        version: &'static str,
    ) -> Self {
        Self {
            layout,
            program,
            client_name,
            version,
            client: Mutex::new(None),
        }
    }

    /// Run `op` on the service: connected before, else connecting, and
    /// starting it when `start`. `None`: not running and not started.
    fn with<T>(
        &self,
        start: bool,
        op: impl Fn(&OwnerClient) -> Result<T, OwnerError>,
    ) -> Result<Option<T>, String> {
        let mut cached = self
            .client
            .lock()
            .map_err(|_| "the account connection is unusable".to_string())?;
        if let Some(client) = cached.as_ref() {
            match op(client) {
                Err(OwnerError::Connection) => *cached = None,
                result => return result.map(Some).map_err(|error| error.to_string()),
            }
        }
        let client = if start {
            service::connect_or_start(&self.layout, &self.program, self.client_name, self.version)
                .map_err(|error| error.to_string())?
        } else {
            match OwnerClient::connect(&OwnerLayout::new(&self.layout), self.client_name) {
                Ok(client) => client,
                Err(OwnerError::NotRunning) => return Ok(None),
                Err(error) => return Err(error.to_string()),
            }
        };
        let result = op(&client).map_err(|error| error.to_string());
        *cached = Some(client);
        result.map(Some)
    }

    fn has_saved_account(&self) -> bool {
        service::has_saved_account(&self.layout)
    }
}

impl AccountAccess for LocalAccount {
    fn account(&self) -> Result<Option<Account>, String> {
        let state = self.with(self.has_saved_account(), OwnerClient::state)?;
        Ok(state.and_then(Account::from_state))
    }

    fn sign_in(&self) -> Result<SignInView, String> {
        self.with(true, OwnerClient::sign_in_start)?
            .ok_or_else(|| "the account service didn't start".into())
    }

    fn sign_in_status(&self) -> Result<SignInView, String> {
        Ok(self
            .with(false, OwnerClient::sign_in_status)?
            .unwrap_or(SignInView::Idle))
    }

    fn cancel_sign_in(&self) -> Result<(), String> {
        self.with(false, OwnerClient::sign_in_cancel).map(|_| ())
    }

    fn sign_out(&self) -> Result<(), String> {
        // Credentials are removed by the service, so start it for them.
        self.with(self.has_saved_account(), OwnerClient::sign_out)
            .map(|_| ())
    }

    fn descriptions(&self, sessions: &[SessionId]) -> Result<Vec<StatusLine>, String> {
        let sessions = &sessions[..sessions.len().min(MAX_SESSIONS)];
        Ok(self
            .with(false, |client| client.statuses(sessions))?
            .unwrap_or_default())
    }

    fn feed(&self, after: Option<u64>, generation: Option<u64>) -> Result<FeedPage, String> {
        Ok(self
            .with(self.has_saved_account(), |client| {
                client.feed(after, generation)
            })?
            .unwrap_or_else(signed_out_feed))
    }

    fn summarize(&self, session: &SessionId) -> Result<(), String> {
        self.with(self.has_saved_account(), |client| client.summarize(session))
            .map(|_| ())
    }

    fn set_features(&self, summaries: Option<bool>, feed: Option<bool>) -> Result<(), String> {
        self.with(self.has_saved_account(), |client| {
            client.set_features(summaries, feed)
        })?
        .ok_or_else(|| "the account service isn't running".into())
    }

    fn feed_turn(&self, entry: &str, after: Option<u64>) -> Result<FeedTurn, String> {
        self.with(false, |client| client.feed_turn(entry, after))?
            .ok_or_else(|| "the account service isn't running".into())
    }
}

/// Why a sign-in ended, for people.
pub fn failure_text(reason: SignInReason) -> &'static str {
    match reason {
        SignInReason::Denied => "The sign-in was declined.",
        SignInReason::Expired => "The code expired before it was approved.",
        SignInReason::Unavailable => "Couldn't reach the sign-in service. Check the connection.",
        SignInReason::Canceled => "The sign-in was cancelled.",
        SignInReason::Failed | SignInReason::Unknown => "The sign-in didn't complete.",
    }
}

/// The link to open for a waiting sign-in: rakumux.com's sign-in-then-approve
/// page, else the provider's page with the code filled in, else its page.
pub fn approval_link(view: &SignInView) -> Option<&str> {
    match view {
        SignInView::Waiting {
            verification_uri,
            verification_uri_complete,
            connect_url,
            ..
        } => Some(
            connect_url
                .as_deref()
                .or(verification_uri_complete.as_deref())
                .unwrap_or(verification_uri),
        ),
        _ => None,
    }
}

// ---------------------------------------------------------------- the service

static STOPPING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop_signal(_: libc::c_int) {
    STOPPING.store(true, Ordering::SeqCst);
}

/// `rakumux status-service`: serve this home until SIGTERM, SIGINT or SIGHUP.
pub fn serve(layout: Layout, version: &'static str) -> Result<(), ServiceError> {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(
                signal,
                on_stop_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
    let settings = layout.home.join("config.json");
    let config = ServiceConfig::from_env()?.with_turn_settings(move || turn_settings(&settings));
    // Its HTTP clients block, which a thread of the async runtime may not do.
    std::thread::Builder::new()
        .name("rakumux-status-service".into())
        .spawn(move || {
            service::serve(&layout, config, version, &|| {
                STOPPING.load(Ordering::SeqCst)
            })
        })
        .map_err(|_| ServiceError::Start)?
        .join()
        .map_err(|_| ServiceError::Start)?
}

/// `sessions.ai_summary` of `config.json`: the defaults when it can't be read.
fn turn_settings(path: &std::path::Path) -> TurnSettings {
    let summary = crate::app::config::Settings::load_from(path)
        .map(|settings| settings.sessions.ai_summary)
        .unwrap_or_default();
    TurnSettings {
        every: summary.every_messages,
        recent: summary.recent_messages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(connect: Option<&str>, complete: Option<&str>) -> SignInView {
        SignInView::Waiting {
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://accounts.example/device".into(),
            verification_uri_complete: complete.map(Into::into),
            connect_url: connect.map(Into::into),
            expires_in_seconds: 600,
        }
    }

    #[test]
    fn the_approval_link_prefers_the_website() {
        assert_eq!(
            approval_link(&waiting(
                Some("https://site.example/connect"),
                Some("https://accounts.example/device?code=1")
            )),
            Some("https://site.example/connect")
        );
        assert_eq!(
            approval_link(&waiting(
                None,
                Some("https://accounts.example/device?code=1")
            )),
            Some("https://accounts.example/device?code=1")
        );
        assert_eq!(
            approval_link(&waiting(None, None)),
            Some("https://accounts.example/device")
        );
        assert_eq!(approval_link(&SignInView::Starting), None);
    }

    #[test]
    fn a_stopped_service_is_not_started_without_a_saved_account() {
        let home = tempfile::tempdir().unwrap();
        let account = LocalAccount::new(
            Layout::with_home(home.path().to_path_buf()),
            PathBuf::from("/nonexistent/rakumux"),
            "test",
            "0.0.0",
        );
        assert_eq!(account.account(), Ok(None));
        assert_eq!(
            account.descriptions(&[SessionId::from_string("a")]),
            Ok(vec![])
        );
        assert_eq!(account.sign_in_status(), Ok(SignInView::Idle));
        assert_eq!(account.feed(None, None), Ok(signed_out_feed()));
    }
}
