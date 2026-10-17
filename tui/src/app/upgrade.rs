//! Upgrade dialog (`u`): check for a newer release and install it with the
//! method Orchestra was installed by, then restart.

use crate::updater::InstallMethod;

use super::runtime::AppEvent;
use super::types::{AppMode, UpgradeDialog, UpgradeMessage};
use super::{app_version, App};

pub fn install_method_display(method: InstallMethod) -> &'static str {
    match method {
        InstallMethod::Npm => "npm",
        InstallMethod::Homebrew => "Homebrew",
        InstallMethod::Source => "source build",
        InstallMethod::Unknown => "manual",
    }
}

impl App {
    /// Read a pending upgrade noted by an earlier background check.
    pub fn load_update_state(&mut self) {
        if let Ok(state) = crate::updater::UpdateState::load() {
            if state.pending_upgrade {
                self.pending_update_version = state.latest_version.clone();
                self.update_install_method = state
                    .install_method
                    .as_deref()
                    .map(InstallMethod::from_string);
            }
        }
    }

    pub fn open_upgrade_dialog(&mut self) {
        self.mode = AppMode::Upgrade(UpgradeDialog {
            current_version: app_version().to_string(),
            latest_version: None,
            install_method: None,
            checking: true,
            installing: false,
            update_available: false,
            error_message: None,
            info_message: None,
            selection: 0,
        });
        let current_version = app_version().to_string();
        self.exec.task(async move {
            AppEvent::Upgrade(
                match crate::updater::check_for_updates(&current_version).await {
                    Ok(Some((latest_version, method))) => UpgradeMessage::CheckAvailable {
                        latest_version,
                        method,
                    },
                    Ok(None) => UpgradeMessage::CheckNoUpdate,
                    Err(err) => UpgradeMessage::CheckError(err.to_string()),
                },
            )
        });
    }

    pub(super) fn start_upgrade_install(&mut self, latest_version: String, method: InstallMethod) {
        self.exec.job(move || {
            AppEvent::Upgrade(match crate::updater::perform_upgrade(method) {
                Ok(()) => UpgradeMessage::InstallSuccess,
                Err(err) => UpgradeMessage::InstallError(err.to_string()),
            })
        });
        // The footer shows the pending restart until the install finishes.
        self.pending_update_version = Some(latest_version);
        self.update_install_method = Some(method);
    }

    pub(super) fn handle_upgrade_message(&mut self, message: UpgradeMessage) {
        let dialog = match &mut self.mode {
            AppMode::Upgrade(dialog) => Some(dialog),
            _ => None,
        };
        match message {
            UpgradeMessage::CheckAvailable {
                latest_version,
                method,
            } => {
                if let Ok(mut state) = crate::updater::UpdateState::load() {
                    state.mark_update_available(latest_version.clone(), method);
                }
                let info = format!(
                    "Version {} available via {}",
                    latest_version,
                    install_method_display(method)
                );
                self.pending_update_version = Some(latest_version.clone());
                self.update_install_method = Some(method);
                match dialog {
                    Some(dialog) => {
                        dialog.checking = false;
                        dialog.installing = false;
                        dialog.update_available = true;
                        dialog.latest_version = Some(latest_version);
                        dialog.install_method = Some(method);
                        dialog.error_message = None;
                        dialog.info_message = Some(info);
                        dialog.selection = 0;
                    }
                    None => self.set_status(info),
                }
            }
            UpgradeMessage::CheckNoUpdate => {
                if let Ok(mut state) = crate::updater::UpdateState::load() {
                    state.clear_pending_upgrade();
                    state.update_last_check();
                }
                let info = "You're already on the latest version.".to_string();
                self.pending_update_version = None;
                self.update_install_method = None;
                match dialog {
                    Some(dialog) => {
                        dialog.checking = false;
                        dialog.installing = false;
                        dialog.update_available = false;
                        dialog.latest_version = None;
                        dialog.install_method = None;
                        dialog.error_message = None;
                        dialog.info_message = Some(info);
                    }
                    None => self.set_status(info),
                }
            }
            UpgradeMessage::CheckError(err) => {
                if let Some(dialog) = dialog {
                    dialog.checking = false;
                    dialog.installing = false;
                    dialog.error_message = Some(err.clone());
                }
                self.set_status(format!("Upgrade check failed: {err}"));
            }
            UpgradeMessage::InstallSuccess => {
                if let Ok(mut state) = crate::updater::UpdateState::load() {
                    state.clear_pending_upgrade();
                }
                if let Some(dialog) = dialog {
                    dialog.installing = false;
                    dialog.checking = false;
                    dialog.update_available = false;
                    dialog.error_message = None;
                    dialog.info_message = Some("Upgrade complete! Restarting...".to_string());
                }
                self.pending_update_version = None;
                self.update_install_method = None;
                self.set_status("Upgrade installed successfully. Restarting...");
                self.restart_requested = true;
            }
            UpgradeMessage::InstallError(err) => {
                if let Some(dialog) = dialog {
                    dialog.installing = false;
                    dialog.checking = false;
                    dialog.error_message = Some(format!("Upgrade failed: {err}"));
                    dialog.info_message = None;
                }
                self.set_status(format!("Upgrade failed: {err}"));
            }
        }
    }
}
