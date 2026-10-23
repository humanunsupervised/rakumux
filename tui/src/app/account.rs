//! The Rakumux account and AI session descriptions (`crate::account`).
//! Checked once the daemon connects; signed in, every live session shown
//! gets a short AI description under its name, refreshed every few seconds.
//! Settings → Account opens the account dialog: sign in with a code
//! approved in any browser, see who is signed in, sign out.

use std::time::{Duration, Instant};

use rakumux_sensagent::owner::protocol::{SignInView, StatusLine};
use session_core::model::SessionStatus;

use super::runtime::AppEvent;
use super::types::{AccountDialog, AccountDialogState, AccountStatus, AppMode};
use super::App;
use crate::account::{self, Account, LineState, MAX_SESSIONS};

/// How often descriptions are read while signed in.
const DESCRIPTIONS_EVERY: Duration = Duration::from_secs(2);
/// How often the account is checked again while signed in (the AI session
/// summaries switch, a sign-out elsewhere).
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
            AccountStatus::SignedIn(_) => {
                self.account_check_at = Some(Instant::now() + ACCOUNT_EVERY);
                self.descriptions_at.get_or_insert_with(Instant::now);
            }
            _ => self.forget_descriptions(),
        }
        self.account = status;
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
                self.set_status("Signed in. AI descriptions appear under each session.");
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
                self.forget_descriptions();
                if let AppMode::Account(dialog) = &mut self.mode {
                    dialog.state = AccountDialogState::SignedOut;
                }
                self.set_status("Signed out. AI descriptions stopped.");
            }
            Err(error) => match &mut self.mode {
                AppMode::Account(dialog) => dialog.state = AccountDialogState::Failed(error),
                _ => self.set_status(format!("Couldn't sign out: {error}")),
            },
        }
    }

    /// The tick's account work: a sign-in's progress, the periodic account
    /// check and reading descriptions.
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
        if self.descriptions_at.is_some_and(|at| now >= at) && !self.descriptions_busy {
            self.read_descriptions(now);
        }
    }

    fn read_descriptions(&mut self, now: Instant) {
        self.descriptions_at = Some(now + DESCRIPTIONS_EVERY);
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
        self.descriptions_busy = true;
        let generation = self.descriptions_generation;
        let access = self.account_access.clone();
        self.exec.job(move || AppEvent::Descriptions {
            generation,
            result: access.descriptions(&sessions),
        });
    }

    pub(super) fn on_descriptions(
        &mut self,
        generation: u64,
        result: Result<Vec<StatusLine>, String>,
    ) {
        if generation != self.descriptions_generation {
            return;
        }
        self.descriptions_busy = false;
        // A failure (the service restarting) keeps what's shown.
        let Ok(lines) = result else { return };
        for line in lines {
            match (line.state, line.text) {
                (LineState::Ready, Some(text)) => {
                    self.descriptions.insert(line.session, text);
                }
                // Refreshing, or nothing new to say: the last one stays.
                (
                    LineState::Pending
                    | LineState::Stale
                    | LineState::Missing
                    | LineState::NoContext
                    | LineState::Ready
                    | LineState::Unknown,
                    _,
                ) => {}
                // Not described: off, signed out, private, ended.
                _ => {
                    self.descriptions.remove(&line.session);
                }
            }
        }
    }

    fn forget_descriptions(&mut self) {
        self.descriptions.clear();
        self.descriptions_at = None;
        self.descriptions_busy = false;
        self.descriptions_generation += 1;
        self.account_check_at = None;
    }

    /// The session's AI description, when signed in and one was written.
    pub fn description(&self, session: &session_core::ids::SessionId) -> Option<&str> {
        self.descriptions.get(session).map(String::as_str)
    }
}
