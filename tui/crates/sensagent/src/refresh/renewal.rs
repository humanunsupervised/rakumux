use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::auth::{
    AccountError, AccountService, AccountSession, ClerkDeviceClient, CredentialRevision,
    CredentialStore, WallClock,
};

/// Renew no later than this before the access token expires.
const MIN_LEAD: Duration = Duration::from_secs(60);
/// The first wait after a failed renewal; it doubles up to `MAX_RETRY`.
const FIRST_RETRY: Duration = Duration::from_secs(60);
const MAX_RETRY: Duration = Duration::from_secs(30 * 60);

/// When to renew the adopted account's access token: once a fifth of its remaining
/// lifetime is left (at least a minute before it expires), and again after a
/// failure with a growing delay. One renewal runs at a time.
#[derive(Default)]
pub(super) struct Renewal {
    revision: Option<CredentialRevision>,
    due: Option<SystemTime>,
    running: bool,
    failures: u32,
    /// Renew at the next tick, whatever the token's lifetime.
    now: bool,
}

/// A finished renewal, reported to the owner thread.
pub(super) enum RenewalEvent {
    /// The renewed session; `None` when the account changed meanwhile.
    Renewed(Option<AccountSession>),
    Failed(AccountError),
}

impl Renewal {
    /// Plan for the adopted `session`, once per credential revision.
    pub(super) fn follow(&mut self, session: Option<&AccountSession>, now: SystemTime) {
        let revision = session.map(AccountSession::revision);
        if revision == self.revision {
            return;
        }
        self.revision = revision;
        self.failures = 0;
        self.due = session.map(|session| due(session.expires_at(), now));
    }

    /// Renew at the next tick (a user asked).
    pub(super) fn request(&mut self) {
        self.now = true;
    }

    /// Whether a renewal should start now; it is then running until `finished`.
    pub(super) fn start(&mut self, now: SystemTime) -> bool {
        let due = self.now || self.due.is_some_and(|due| now >= due);
        if self.running || self.revision.is_none() || !due {
            return false;
        }
        self.running = true;
        self.now = false;
        true
    }

    /// A renewal ended. A success is followed through the new session's revision;
    /// a failure is retried later.
    pub(super) fn finished(&mut self, succeeded: bool, now: SystemTime) {
        self.running = false;
        if succeeded {
            return;
        }
        let delay = FIRST_RETRY
            .saturating_mul(2u32.saturating_pow(self.failures))
            .min(MAX_RETRY);
        self.failures = self.failures.saturating_add(1);
        self.due = Some(now + delay);
    }
}

/// When a token expiring at `expires_at` (Unix seconds) is renewed.
fn due(expires_at: u64, now: SystemTime) -> SystemTime {
    let expires = UNIX_EPOCH + Duration::from_secs(expires_at);
    let remaining = expires.duration_since(now).unwrap_or(Duration::ZERO);
    let lead = (remaining / 5).max(MIN_LEAD);
    expires.checked_sub(lead).unwrap_or(now).max(now)
}

/// Renew `session` on its own thread: discovery and the refresh grant can take as
/// long as their timeouts, so they never occupy a refresh worker.
pub(super) fn spawn<A, C>(
    account: Arc<AccountService<A, C>>,
    session: AccountSession,
    report: impl FnOnce(RenewalEvent) + Send + 'static,
) -> std::io::Result<()>
where
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    thread::Builder::new()
        .name("rakumux-status-renewal".into())
        .spawn(move || {
            let renewed = ClerkDeviceClient::discover(account.config().clone())
                .map_err(AccountError::from)
                .and_then(|client| account.renew(&session, &client));
            report(match renewed {
                Ok(session) => RenewalEvent::Renewed(session),
                Err(error) => RenewalEvent::Failed(error),
            });
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn renewal_is_due_when_a_fifth_of_the_lifetime_is_left() {
        // A day-long token: renewed with 4.8 hours left.
        assert_eq!(due(86_400, at(0)), at(86_400 - 17_280));
        // A short one: at least a minute before it expires.
        assert_eq!(due(200, at(0)), at(140));
        // Expired or nearly: at once.
        assert_eq!(due(30, at(0)), at(0));
        assert_eq!(due(10, at(50)), at(50));
    }

    #[test]
    fn failures_back_off_and_one_renewal_runs_at_a_time() {
        let mut renewal = Renewal {
            revision: None,
            due: Some(at(0)),
            ..Renewal::default()
        };
        // Signed out: nothing to renew.
        assert!(!renewal.start(at(10)));
        renewal.revision = Some(CredentialRevision::default());
        assert!(renewal.start(at(10)));
        assert!(!renewal.start(at(10)));
        renewal.finished(false, at(10));
        assert!(!renewal.start(at(69)));
        assert!(renewal.start(at(70)));
        renewal.finished(false, at(70));
        assert!(!renewal.start(at(189)));
        assert!(renewal.start(at(190)));
        // A request starts one at once.
        renewal.finished(false, at(190));
        renewal.request();
        assert!(renewal.start(at(191)));
    }
}
