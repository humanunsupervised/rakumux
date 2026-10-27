//! The Rakumux account and AI session descriptions (`crate::account`).
//! Checked once the daemon connects; signed in, live sessions shown get a
//! short AI description under them, read from the account service every few
//! seconds. The service makes one per agent chat by itself; after that a new
//! one is made only when asked: `s`, or the ↻ shown over a hovered
//! description.
//! Settings → Account opens the account dialog: sign in with a code
//! approved in any browser, see who is signed in, sign out. Settings →
//! Available on sign-in turns the account's AI summaries and Agent feed on
//! and off, saved to the account as on rakumux.com/features.

use std::time::{Duration, Instant};

use rakumux_sensagent::owner::protocol::{SignInView, StatusLine};
use session_core::ids::SessionId;
use session_core::model::SessionStatus;

use super::runtime::AppEvent;
use super::types::{AccountDialog, AccountDialogState, AccountFeature, AccountStatus, AppMode};
use super::App;
use crate::account::{self, Account, LineState, MAX_SESSIONS};

/// How often descriptions are read while signed in.
const DESCRIPTIONS_EVERY: Duration = Duration::from_secs(2);
/// How often the account is checked again while signed in (the AI session
/// summaries switch, a sign-out elsewhere).
const ACCOUNT_EVERY: Duration = Duration::from_secs(60);
/// How often a sign-in in progress is checked.
const SIGN_IN_EVERY: Duration = Duration::from_secs(1);
/// How soon the account is checked after a switch changed, to see that it
/// was saved.
const SAVED_AFTER: Duration = Duration::from_secs(3);

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
                self.descriptions_at.get_or_insert_with(Instant::now);
                if std::mem::take(&mut self.features_saving) && account.features_unsaved {
                    self.set_status("Couldn't save that to your account. Try again.");
                }
                if account.summaries == Some(false) {
                    self.descriptions.clear();
                }
            }
            _ => self.forget_descriptions(),
        }
        self.account = status;
        self.on_feed_switch();
    }

    /// Settings → Available on sign-in: turn `feature` on or off for the
    /// account. It shows changed at once; the account service saves it.
    pub fn toggle_feature(&mut self, feature: AccountFeature) {
        let AccountStatus::SignedIn(account) = &mut self.account else {
            self.set_status(format!(
                "Sign in to use {}: Settings → Account.",
                feature.label()
            ));
            return;
        };
        let Some(on) = feature.of(account) else {
            self.set_status("Still reading your account's settings…");
            return;
        };
        let on = !on;
        let (summaries, feed) = match feature {
            AccountFeature::AiSummaries => {
                account.summaries = Some(on);
                (Some(on), None)
            }
            AccountFeature::AgentFeed => {
                account.feed = Some(on);
                (None, Some(on))
            }
        };
        account.features_unsaved = false;
        if summaries == Some(false) {
            self.descriptions.clear();
        }
        self.on_feed_switch();
        self.features_saving = true;
        let state = if on { "on" } else { "off" };
        self.set_status(format!("{}: {state}", feature.label()));
        let access = self.account_access.clone();
        self.exec
            .job(move || AppEvent::FeaturesSet(access.set_features(summaries, feed)));
    }

    pub(super) fn on_features_set(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.features_saving = false;
            self.set_status(format!("Couldn't change it: {error}"));
        }
        // Read back what the account has (and whether it was saved).
        self.account_check_at = Some(Instant::now() + SAVED_AFTER);
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
                self.on_feed_switch();
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
        if !self.summaries_on() {
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
            if line.refreshing {
                self.summaries_refreshing.insert(line.session.clone());
            } else {
                self.summaries_refreshing.remove(&line.session);
            }
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

    /// `s`: a new AI summary of the selected session (live ones only).
    pub fn summarize_selected(&mut self) {
        let Some(session) = self.list.selected_session() else {
            self.set_status("Select a session to summarise.");
            return;
        };
        if session.status != SessionStatus::Live {
            self.set_status("Only running sessions are summarised.");
            return;
        }
        let (id, name) = (session.id.clone(), session.name.clone());
        if self.summarize(id.clone()) && self.description(&id).is_none() {
            self.set_status(format!("Summarising {name}…"));
        }
    }

    /// `s`, or a click on a description's ↻: the account service makes a new
    /// AI summary of `session` (its agent's latest prompt and outputs since,
    /// else its screen). Signed out or with summaries off, says how to turn
    /// them on; while one is being made, does nothing. True when asked.
    pub fn summarize(&mut self, session: SessionId) -> bool {
        let on = matches!(
            &self.account,
            AccountStatus::SignedIn(account) if account.summaries != Some(false)
        );
        if !on {
            self.set_status("Sign in and turn on AI summaries: Settings → Available on sign-in.");
            return false;
        }
        if self.summary_refreshing(&session) {
            return false;
        }
        self.summary_asks.insert(session.clone());
        let access = self.account_access.clone();
        self.exec.job(move || {
            let result = access.summarize(&session);
            AppEvent::Summarized { session, result }
        });
        true
    }

    pub(super) fn on_summarized(&mut self, session: SessionId, result: Result<(), String>) {
        self.summary_asks.remove(&session);
        match result {
            Ok(()) => {
                self.summaries_refreshing.insert(session);
                // A read under way predates the request: read again now.
                self.descriptions_generation += 1;
                self.descriptions_busy = false;
                self.descriptions_at = Some(Instant::now());
            }
            Err(error) => self.set_status(format!("Couldn't summarise: {error}")),
        }
    }

    /// A new AI summary of `session` was asked for or is being made.
    pub fn summary_refreshing(&self, session: &SessionId) -> bool {
        self.summary_asks.contains(session) || self.summaries_refreshing.contains(session)
    }

    fn forget_descriptions(&mut self) {
        self.descriptions.clear();
        self.summaries_refreshing.clear();
        self.summary_asks.clear();
        self.descriptions_at = None;
        self.descriptions_busy = false;
        self.descriptions_generation += 1;
        self.account_check_at = None;
    }

    /// The session's AI description, when signed in with AI summaries on
    /// and one was written.
    pub fn description(&self, session: &session_core::ids::SessionId) -> Option<&str> {
        self.descriptions.get(session).map(String::as_str)
    }

    /// Signed in with AI summaries on (or not yet read): a description's ↻
    /// and `s` make new ones.
    pub fn summaries_on(&self) -> bool {
        matches!(
            &self.account,
            AccountStatus::SignedIn(account) if account.summaries != Some(false)
        )
    }
}
