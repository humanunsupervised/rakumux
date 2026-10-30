//! The Rakumux account and AI session titles (`crate::account`). Checked
//! once the daemon connects. Signed in with AI session titles on, the account
//! service names each agent chat's session by itself (a name the user gave
//! stays); after that a new title is made only when asked: `s`, or the ↻
//! after a hovered session's name. The live sessions shown are read from
//! the service every few seconds for which have one being made (a spinner
//! after the name).
//! Settings → Account opens the account dialog: sign in with a code
//! approved in any browser, see who is signed in, sign out. AI session
//! titles and the agent feed are on for every signed-in account.

use std::time::{Duration, Instant};

use rakumux_sensagent::owner::protocol::{SignInView, StatusLine};
use session_core::ids::SessionId;
use session_core::model::SessionStatus;

use super::runtime::AppEvent;
use super::types::{AccountDialog, AccountDialogState, AccountStatus, AppMode};
use super::App;
use crate::account::{self, Account, MAX_SESSIONS};

/// How often the sessions' AI status is read while signed in.
const STATUSES_EVERY: Duration = Duration::from_secs(2);
/// How often the account is checked again while signed in (what the
/// account has on, a sign-out elsewhere).
const ACCOUNT_EVERY: Duration = Duration::from_secs(60);
/// How often a sign-in in progress is checked.
const SIGN_IN_EVERY: Duration = Duration::from_secs(1);

impl App {
    /// Ask who is signed in (starting the service when this home has a
    /// saved account).
    pub(super) fn check_account(&mut self) {
        if self.account_checking {
            return;
        }
        self.account_checking = true;
        self.account_check_at = None;
        let access = self.account_access.clone();
        self.exec
            .job(move || AppEvent::AccountChecked(access.account()));
    }

    pub(super) fn on_account_checked(&mut self, result: Result<Option<Account>, String>) {
        self.account_checking = false;
        let status = match result {
            Ok(Some(account)) => AccountStatus::SignedIn(account),
            Ok(None) => AccountStatus::SignedOut,
            Err(error) => {
                if let AppMode::Account(dialog) = &mut self.mode {
                    dialog.state = AccountDialogState::Failed(error);
                }
                self.account_check_at = Some(Instant::now() + ACCOUNT_EVERY);
                return;
            }
        };
        if let AppMode::Account(dialog) = &mut self.mode {
            // A sign-in under way keeps its dialog.
            if !dialog.signing_in() {
                dialog.state = match &status {
                    AccountStatus::SignedIn(account) => {
                        AccountDialogState::SignedIn(account.clone())
                    }
                    _ => AccountDialogState::SignedOut,
                };
            }
        }
        match &status {
            AccountStatus::SignedIn(account) => {
                self.account_check_at = Some(Instant::now() + ACCOUNT_EVERY);
                self.statuses_at.get_or_insert_with(Instant::now);
                if account.titles == Some(false) {
                    self.titles_refreshing.clear();
                }
            }
            _ => self.forget_statuses(),
        }
        self.account = status;
        self.on_feed_switch();
    }

    /// Settings → Account.
    pub fn open_account_dialog(&mut self) {
        let state = match &self.account {
            AccountStatus::SignedIn(account) => AccountDialogState::SignedIn(account.clone()),
            AccountStatus::SignedOut => AccountDialogState::SignedOut,
            AccountStatus::Unknown => {
                self.check_account();
                AccountDialogState::Loading
            }
        };
        self.mode = AppMode::Account(AccountDialog { state });
    }

    /// `a`: signed out, ask for a sign-in code; otherwise the account dialog.
    pub fn account_key(&mut self) {
        match self.account {
            AccountStatus::SignedOut => self.start_sign_in(),
            _ => self.open_account_dialog(),
        }
    }

    /// `c` in the sign-in dialog: the link to approve the code, on the
    /// clipboard of the terminal this window runs in (OSC 52, also over SSH
    /// where the terminal allows it).
    pub fn copy_sign_in_link(&mut self) {
        use base64::Engine;
        let AppMode::Account(dialog) = &self.mode else {
            return;
        };
        let AccountDialogState::Waiting(view) = &dialog.state else {
            return;
        };
        let Some(link) = account::approval_link(view) else {
            return;
        };
        let data = base64::engine::general_purpose::STANDARD.encode(link);
        self.terminal_output
            .extend_from_slice(format!("\x1b]52;c;{data}\x07").as_bytes());
        self.set_status("Copied the sign-in link.");
    }

    /// Ask for a sign-in code (the service starts if it isn't running).
    pub fn start_sign_in(&mut self) {
        self.mode = AppMode::Account(AccountDialog {
            state: AccountDialogState::Starting,
        });
        self.sign_in_polling = true;
        self.sign_in_poll_at = None;
        let access = self.account_access.clone();
        self.exec.job(move || AppEvent::SignIn(access.sign_in()));
    }

    pub(super) fn on_sign_in(&mut self, result: Result<SignInView, String>) {
        self.sign_in_polling = false;
        let AppMode::Account(dialog) = &mut self.mode else {
            // Closed meanwhile (closing cancels the sign-in).
            return;
        };
        if !dialog.signing_in() {
            return;
        }
        let poll = Some(Instant::now() + SIGN_IN_EVERY);
        match result {
            Err(error) => dialog.state = AccountDialogState::Failed(error),
            Ok(view @ SignInView::Waiting { .. }) => {
                dialog.state = AccountDialogState::Waiting(view);
                self.sign_in_poll_at = poll;
            }
            Ok(SignInView::Starting) => self.sign_in_poll_at = poll,
            Ok(SignInView::SignedIn) => {
                dialog.state = AccountDialogState::Loading;
                self.account_checking = false;
                self.check_account();
                self.set_status("Signed in.");
            }
            Ok(SignInView::Failed { reason }) => {
                dialog.state = AccountDialogState::Failed(account::failure_text(reason).into())
            }
            Ok(SignInView::Idle | SignInView::Unknown) => {
                dialog.state = AccountDialogState::Failed("The sign-in stopped.".into())
            }
        }
    }

    /// Close the account dialog, abandoning a sign-in under way.
    pub fn close_account_dialog(&mut self) {
        if let AppMode::Account(dialog) = &self.mode {
            if dialog.signing_in() {
                let access = self.account_access.clone();
                self.exec
                    .job(move || AppEvent::SignInCancelled(access.cancel_sign_in()));
            }
        }
        self.sign_in_poll_at = None;
        self.mode = AppMode::Normal;
    }

    pub fn sign_out(&mut self) {
        self.mode = AppMode::Account(AccountDialog {
            state: AccountDialogState::SigningOut,
        });
        let access = self.account_access.clone();
        self.exec
            .job(move || AppEvent::SignedOut(access.sign_out()));
    }

    pub(super) fn on_signed_out(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.account = AccountStatus::SignedOut;
                self.forget_statuses();
                self.on_feed_switch();
                if let AppMode::Account(dialog) = &mut self.mode {
                    dialog.state = AccountDialogState::SignedOut;
                }
                self.set_status("Signed out.");
            }
            Err(error) => match &mut self.mode {
                AppMode::Account(dialog) => dialog.state = AccountDialogState::Failed(error),
                _ => self.set_status(format!("Couldn't sign out: {error}")),
            },
        }
    }

    /// The tick's account work: a sign-in's progress, the periodic account
    /// check and reading which titles are being made.
    pub(super) fn account_timers(&mut self, now: Instant) {
        if self.sign_in_poll_at.is_some_and(|at| now >= at) && !self.sign_in_polling {
            self.sign_in_poll_at = None;
            if matches!(&self.mode, AppMode::Account(dialog) if dialog.signing_in()) {
                self.sign_in_polling = true;
                let access = self.account_access.clone();
                self.exec
                    .job(move || AppEvent::SignIn(access.sign_in_status()));
            }
        }
        if self.account_check_at.is_some_and(|at| now >= at) {
            self.check_account();
        }
        if self.statuses_at.is_some_and(|at| now >= at) && !self.statuses_busy {
            self.read_statuses(now);
        }
    }

    fn read_statuses(&mut self, now: Instant) {
        self.statuses_at = Some(now + STATUSES_EVERY);
        if !self.titles_on() {
            return;
        }
        let sessions: Vec<_> = self
            .list
            .visible()
            .iter()
            .filter(|session| session.status == SessionStatus::Live)
            .map(|session| session.id.clone())
            .take(MAX_SESSIONS)
            .collect();
        if sessions.is_empty() {
            return;
        }
        self.statuses_busy = true;
        let generation = self.statuses_generation;
        let access = self.account_access.clone();
        self.exec.job(move || AppEvent::Statuses {
            generation,
            result: access.statuses(&sessions),
        });
    }

    pub(super) fn on_statuses(&mut self, generation: u64, result: Result<Vec<StatusLine>, String>) {
        if generation != self.statuses_generation {
            return;
        }
        self.statuses_busy = false;
        // A failure (the service restarting) keeps what's shown.
        let Ok(lines) = result else { return };
        for line in lines {
            if line.refreshing {
                self.titles_refreshing.insert(line.session);
            } else {
                self.titles_refreshing.remove(&line.session);
            }
        }
    }

    /// `s`: a new AI title of the selected session (live ones only).
    pub fn retitle_selected(&mut self) {
        let Some(session) = self.list.selected_session() else {
            self.set_status("Select a session to title.");
            return;
        };
        if session.status != SessionStatus::Live {
            self.set_status("Only running sessions get AI titles.");
            return;
        }
        let id = session.id.clone();
        self.retitle(id);
    }

    /// `s`, or a click on the ↻ after a session's name: the account service
    /// makes a new AI title of `session` (from its agent's latest prompt and
    /// outputs since, else its screen) and names the session with it,
    /// whatever its name. Signed out, says where to sign in; while one is
    /// being made, does nothing. True when asked.
    pub fn retitle(&mut self, session: SessionId) -> bool {
        if !self.titles_on() {
            self.set_status(match self.account {
                AccountStatus::SignedIn(_) => "AI session titles aren't on for your account yet.",
                _ => "Sign in for AI session titles: Settings → Account.",
            });
            return false;
        }
        if self.title_refreshing(&session) {
            return false;
        }
        self.title_asks.insert(session.clone());
        let access = self.account_access.clone();
        self.exec.job(move || {
            let result = access.summarize(&session);
            AppEvent::TitleAsked { session, result }
        });
        true
    }

    pub(super) fn on_title_asked(&mut self, session: SessionId, result: Result<(), String>) {
        self.title_asks.remove(&session);
        match result {
            Ok(()) => {
                self.titles_refreshing.insert(session);
                // A read under way predates the request: read again now.
                self.statuses_generation += 1;
                self.statuses_busy = false;
                self.statuses_at = Some(Instant::now());
            }
            Err(error) => self.set_status(format!("Couldn't make a title: {error}")),
        }
    }

    /// A new AI title of `session` was asked for or is being made.
    pub fn title_refreshing(&self, session: &SessionId) -> bool {
        self.title_asks.contains(session) || self.titles_refreshing.contains(session)
    }

    fn forget_statuses(&mut self) {
        self.titles_refreshing.clear();
        self.title_asks.clear();
        self.statuses_at = None;
        self.statuses_busy = false;
        self.statuses_generation += 1;
        self.account_check_at = None;
    }

    /// Signed in with AI session titles on (or not yet read): the service
    /// titles sessions, the ↻ and `s` make new titles, and the TUI's own AI
    /// naming waits.
    pub fn titles_on(&self) -> bool {
        matches!(
            &self.account,
            AccountStatus::SignedIn(account) if account.titles != Some(false)
        )
    }
}
