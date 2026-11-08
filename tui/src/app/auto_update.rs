//! Automatic updates (`sessions.auto_update`, Settings → Update
//! automatically). Every minute the window reads the home's shared
//! `update_state.json`: once a day one window checks for a release, and a
//! newer one, found here or by another window, is installed without asking
//! by a binary installation, through the same verified path as `U`; the
//! window then reloads into it once nobody uses it. npm, Homebrew and source
//! builds, a binary installation with the setting off and one whose install
//! failed are told instead, once per version. Never stops or restarts the
//! daemon, the API or a session.

use std::time::{Duration, Instant};

use super::runtime::AppEvent;
use super::types::{AppMode, AutoUpdateMessage, Focus, InstallFailure};
use super::{app_version, App};
use crate::updater::{is_newer, InstallMethod, UpdateBusy, UpdateState};

/// How often the shared update state is read: a check due, or a release
/// another window found.
const LOOK_EVERY: Duration = Duration::from_secs(60);
/// No key, click, paste or focus for this long before reloading by itself.
pub(super) const RELOAD_IDLE: Duration = Duration::from_secs(60);
/// A check that couldn't be made (offline) is tried again this much later.
const CHECK_RETRY: Duration = Duration::from_secs(3600);

/// This window's part of automatic updates.
pub struct AutoUpdate {
    /// When to read the shared state next; `None`: at the next tick.
    look_at: Option<Instant>,
    /// Not before then: the last check couldn't be made.
    check_after: Option<Instant>,
    checking: bool,
    installing: bool,
    /// Installed: the version this window reloads into once idle.
    ready: Option<String>,
    /// When someone last used this window.
    last_input: Instant,
}

impl AutoUpdate {
    pub fn new(now: Instant) -> Self {
        Self {
            look_at: None,
            check_after: None,
            checking: false,
            installing: false,
            ready: None,
            last_input: now,
        }
    }
}

/// How to get `version`, in one line: what the footer says about it.
pub fn update_notice(version: &str, method: InstallMethod) -> String {
    match method {
        InstallMethod::Binary => format!("Update {version} available · U to install"),
        InstallMethod::Npm => format!("Update {version} available: npm install -g rakumux@latest"),
        InstallMethod::Homebrew => format!("Update {version} available: brew upgrade rakumux"),
        InstallMethod::Source => format!("Update {version} available: rebuild your checkout"),
        InstallMethod::Unknown => {
            format!("Update {version} available: rerun rakumux.com/install.sh")
        }
    }
}

impl App {
    /// Someone used this window: a key, a click, a paste, focus.
    pub(super) fn note_input(&mut self, now: Instant) {
        self.auto_update.last_input = now;
    }

    /// The version installed for this window to reload into.
    pub fn update_ready(&self) -> Option<&str> {
        self.auto_update.ready.as_deref()
    }

    /// The footer's quiet line about updates: installed and waiting to
    /// reload, else a newer release and how to get it.
    pub fn update_hint(&self) -> Option<String> {
        if let Some(version) = &self.auto_update.ready {
            return Some(if self.settings.sessions.auto_update {
                format!("Updated to {version} · reloads when idle (R now)")
            } else {
                format!("Updated to {version} · R to reload")
            });
        }
        let version = self.pending_update_version.as_ref()?;
        Some(update_notice(
            version,
            self.update_install_method.unwrap_or(InstallMethod::Unknown),
        ))
    }

    /// An update was installed and the UI waits to reload into it.
    pub(super) fn update_installed(&self) -> bool {
        self.auto_update.ready.is_some()
    }

    /// Read the shared state at the next tick (the setting changed).
    pub(super) fn look_for_update_soon(&mut self) {
        self.auto_update.look_at = None;
    }

    /// Timers: the shared state every minute, and the reload once idle.
    /// Returns whether to redraw.
    pub(super) fn update_timers(&mut self, now: Instant) -> bool {
        if self.auto_update.ready.is_some() {
            if self.settings.sessions.auto_update
                && !self.restart_requested
                && self.safe_to_reload(now)
            {
                // Through the launcher the install gave (`restart_launcher`);
                // the new UI restarts the server and its services too.
                self.restart_after_update = true;
                self.restart_requested = true;
                return true;
            }
            return false;
        }
        if self.auto_update.look_at.is_some_and(|at| now < at) {
            return false;
        }
        self.auto_update.look_at = Some(now + LOOK_EVERY);
        self.look_for_update(now)
    }

    /// Nothing would be lost or interrupted by reloading now: Rakumux mode
    /// with nothing open or half typed, no terminal panel shell (a reload
    /// ends it), and nobody has used the window for [`RELOAD_IDLE`].
    fn safe_to_reload(&self, now: Instant) -> bool {
        self.focus == Focus::List
            && matches!(self.mode, AppMode::Normal)
            && !self.list.filter_editing
            && self.pending_key.is_none()
            && !self.creating_session
            && !self.awaiting_focus()
            && !self.has_terminal_shells()
            && now.saturating_duration_since(self.auto_update.last_input) >= RELOAD_IDLE
    }

    fn look_for_update(&mut self, now: Instant) -> bool {
        if self.auto_update.checking || self.auto_update.installing {
            return false;
        }
        let Some(path) = self.update_state_path() else {
            return false;
        };
        let state = UpdateState::load_from(&path).unwrap_or_default();
        if state.should_check_for_updates()
            && self.auto_update.check_after.is_none_or(|at| now >= at)
        {
            self.auto_update.checking = true;
            let updater = self.updater.clone();
            self.exec.job(move || {
                AppEvent::AutoUpdate(AutoUpdateMessage::Checked(
                    updater
                        .check(app_version())
                        .map_err(|error| format!("{error:#}")),
                ))
            });
            return false;
        }
        self.consider_update(state)
    }

    pub(super) fn on_auto_update(&mut self, message: AutoUpdateMessage) {
        match message {
            AutoUpdateMessage::Checked(result) => self.on_update_checked(result),
            AutoUpdateMessage::Installed { version, result } => {
                self.on_auto_installed(version, result)
            }
        }
    }

    fn on_update_checked(&mut self, result: Result<Option<(String, InstallMethod)>, String>) {
        self.auto_update.checking = false;
        let Some(path) = self.update_state_path() else {
            return;
        };
        let mut state = UpdateState::load_from(&path).unwrap_or_default();
        match result {
            Ok(Some((version, method))) => {
                state.mark_update_available(version, method);
                // A new day, a new try for a version whose install failed.
                state.failed_version = None;
            }
            Ok(None) => state.no_update_for(app_version()),
            // Offline, or an installation that can't tell: quietly, later.
            Err(_) => {
                self.auto_update.check_after = Some(Instant::now() + CHECK_RETRY);
                return;
            }
        }
        state.update_last_check();
        if state.save_to(&path).is_err() {
            self.auto_update.check_after = Some(Instant::now() + CHECK_RETRY);
        }
        self.consider_update(state);
    }

    /// The newer release `state` records, if any, noted for the footer and
    /// Settings (not for a source build, whose version says nothing).
    pub(super) fn note_available(&mut self, state: &UpdateState) -> Option<String> {
        let method = self.updater.method();
        let version = state
            .pending_upgrade
            .then(|| state.latest_version.clone())
            .flatten()
            .filter(|version| is_newer(app_version(), version));
        match &version {
            Some(version) if method != InstallMethod::Source => {
                self.pending_update_version = Some(version.clone());
                self.update_install_method = Some(method);
            }
            _ => {
                self.pending_update_version = None;
                self.update_install_method = None;
            }
        }
        version
    }

    /// What a newer release means for this window: install it, or tell the
    /// user once. Returns whether to redraw.
    fn consider_update(&mut self, mut state: UpdateState) -> bool {
        let shown = self.pending_update_version.clone();
        let Some(version) = self.note_available(&state) else {
            return shown.is_some();
        };
        let installing_by_hand =
            matches!(&self.mode, AppMode::Upgrade(dialog) if dialog.installing);
        if self.auto_update.ready.is_some() || self.auto_update.installing || installing_by_hand {
            return true;
        }
        let method = self.updater.method();
        if self.settings.sessions.auto_update
            && method == InstallMethod::Binary
            && state.failed_version.as_deref() != Some(version.as_str())
        {
            self.install_automatically(version);
            return true;
        }
        if state.notified_version.as_deref() != Some(version.as_str()) {
            self.set_status(update_notice(&version, method));
            state.notified_version = Some(version);
            self.save_shared_state(&state);
        }
        true
    }

    fn install_automatically(&mut self, version: String) {
        self.auto_update.installing = true;
        let updater = self.updater.clone();
        self.exec.job(move || {
            let result = updater
                .install(&version, InstallMethod::Binary)
                .map_err(|error| {
                    if UpdateBusy::is(&error) {
                        InstallFailure::Busy
                    } else {
                        InstallFailure::Failed(format!("{error:#}"))
                    }
                });
            AppEvent::AutoUpdate(AutoUpdateMessage::Installed { version, result })
        });
    }

    fn on_auto_installed(
        &mut self,
        version: String,
        result: Result<Option<std::path::PathBuf>, InstallFailure>,
    ) {
        self.auto_update.installing = false;
        match result {
            Ok(launcher) => {
                self.restart_launcher = launcher;
                self.auto_update.ready = Some(version);
                self.pending_update_version = None;
                self.update_install_method = None;
            }
            // Another window installs it: the next look finds it installed.
            Err(InstallFailure::Busy) => {}
            Err(InstallFailure::Failed(error)) => {
                let Some(path) = self.update_state_path() else {
                    return;
                };
                let mut state = UpdateState::load_from(&path).unwrap_or_default();
                state.failed_version = Some(version.clone());
                if state.notified_version.as_deref() != Some(version.as_str()) {
                    self.set_status(format!("Couldn't update to {version}: {error}"));
                    state.notified_version = Some(version);
                }
                self.save_shared_state(&state);
            }
        }
    }

    fn save_shared_state(&mut self, state: &UpdateState) {
        if let Some(path) = self.update_state_path() {
            // Unsaved, the user may be told once more; nothing else.
            let _ = state.save_to(&path);
        }
    }
}

#[cfg(test)]
#[path = "auto_update_tests.rs"]
mod tests;
