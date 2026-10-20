//! Update (`Shift+U`): check, explicitly confirm installation, then reload only the UI.
use super::runtime::AppEvent;
use super::types::{AppMode, UpgradeDialog, UpgradeMessage};
use super::{app_version, App};
use crate::updater::{InstallMethod, UpdateState};

pub fn install_method_display(method: InstallMethod) -> &'static str {
    match method {
        InstallMethod::Binary => "GitHub releases",
        InstallMethod::Npm => "npm",
        InstallMethod::Homebrew => "Homebrew",
        InstallMethod::Source => "source build",
        InstallMethod::Unknown => "manual installation",
    }
}

impl App {
    /// Cached availability is a hint, never authorization to install on startup.
    pub fn load_update_state(&mut self) {
        let Some(path) = self.update_state_path() else {
            return;
        };
        match UpdateState::load_from(&path) {
            Ok(state) if state.pending_upgrade => {
                if let Some(version) = state.latest_version.filter(|version| {
                    crate::updater::compare_versions(app_version(), version)
                        .is_ok_and(|order| order.is_lt())
                }) {
                    self.pending_update_version = Some(version);
                    self.update_install_method = state
                        .install_method
                        .as_deref()
                        .map(InstallMethod::from_string);
                }
            }
            Ok(_) => {}
            Err(error) => self.set_status(format!("Could not read update status: {error}")),
        }
    }

    pub fn open_upgrade_dialog(&mut self) {
        if matches!(&self.mode, AppMode::Upgrade(dialog) if dialog.installing) {
            return;
        }
        self.update_generation = self.update_generation.wrapping_add(1);
        self.mode = AppMode::Upgrade(UpgradeDialog {
            current_version: app_version().into(),
            latest_version: None,
            install_method: None,
            checking: true,
            installing: false,
            update_available: false,
            error_message: None,
            info_message: None,
            selection: 0,
        });
        let updater = self.updater.clone();
        let generation = self.update_generation;
        self.exec.job(move || AppEvent::Upgrade {
            generation,
            message: match updater.check(app_version()) {
                Ok(Some((latest_version, method))) => UpgradeMessage::CheckAvailable {
                    latest_version,
                    method,
                },
                Ok(None) => UpgradeMessage::CheckNoUpdate,
                Err(error) => UpgradeMessage::CheckError(format!("{error:#}")),
            },
        });
    }

    pub(super) fn close_upgrade_dialog(&mut self) {
        self.update_generation = self.update_generation.wrapping_add(1);
        self.mode = AppMode::Normal;
    }

    pub(super) fn start_upgrade_install(&mut self, latest_version: String, method: InstallMethod) {
        let updater = self.updater.clone();
        let generation = self.update_generation;
        self.exec.job(move || AppEvent::Upgrade {
            generation,
            message: match updater.install(&latest_version, method) {
                Ok(launcher) => UpgradeMessage::InstallSuccess { launcher },
                Err(error) => UpgradeMessage::InstallError(format!("{error:#}")),
            },
        });
    }

    pub(super) fn handle_upgrade_message(&mut self, message: UpgradeMessage) {
        match message {
            UpgradeMessage::CheckAvailable {
                latest_version,
                method,
            } => {
                self.pending_update_version = Some(latest_version.clone());
                self.update_install_method = Some(method);
                self.save_update_state();
                if let AppMode::Upgrade(dialog) = &mut self.mode {
                    dialog.checking = false;
                    dialog.update_available = true;
                    dialog.latest_version = Some(latest_version.clone());
                    dialog.install_method = Some(method);
                    dialog.error_message = None;
                    dialog.info_message = Some(format!(
                        "Version {latest_version} available via {}.",
                        install_method_display(method)
                    ));
                    dialog.selection = 0;
                }
            }
            UpgradeMessage::CheckNoUpdate => {
                self.pending_update_version = None;
                self.update_install_method = None;
                self.save_update_state();
                if let AppMode::Upgrade(dialog) = &mut self.mode {
                    dialog.checking = false;
                    dialog.update_available = false;
                    dialog.latest_version = None;
                    dialog.install_method = None;
                    dialog.info_message = Some("You're already on the latest version.".into());
                }
            }
            UpgradeMessage::CheckError(error) => {
                if let AppMode::Upgrade(dialog) = &mut self.mode {
                    dialog.checking = false;
                    dialog.error_message = Some(error.clone());
                }
                self.set_status(format!("Update check failed: {error}"));
            }
            UpgradeMessage::InstallSuccess { launcher } => {
                self.pending_update_version = None;
                self.update_install_method = None;
                self.save_update_state();
                self.restart_launcher = launcher;
                self.restart_requested = true;
                self.set_status("Update installed. Restarting only the UI; sessions keep running.");
                if let AppMode::Upgrade(dialog) = &mut self.mode {
                    dialog.installing = false;
                    dialog.update_available = false;
                    dialog.info_message = Some("Update installed. Restarting the UI…".into());
                }
            }
            UpgradeMessage::InstallError(error) => {
                if let AppMode::Upgrade(dialog) = &mut self.mode {
                    dialog.installing = false;
                    dialog.error_message = Some(error.clone());
                    dialog.info_message = None;
                }
                self.set_status(format!("Update failed: {error}"));
            }
        }
    }

    fn update_state_path(&self) -> Option<std::path::PathBuf> {
        self.paths
            .as_ref()
            .map(|paths| paths.home.join("update_state.json"))
    }

    fn save_update_state(&mut self) {
        let Some(path) = self.update_state_path() else {
            return;
        };
        let mut state = UpdateState::default();
        if let (Some(version), Some(method)) =
            (&self.pending_update_version, self.update_install_method)
        {
            state.mark_update_available(version.clone(), method);
        }
        state.update_last_check();
        if let Err(error) = state.save_to(&path) {
            self.set_status(format!("Could not save update status: {error}"));
        }
    }
}
