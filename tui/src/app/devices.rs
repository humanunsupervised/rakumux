//! Settings → Paired devices (`rakumux devices`): the browsers and phones
//! that sign what rakumux.com asks of this machine, read from the account
//! service; pairing one (a code, its link and QR code, then the device sent
//! with it to confirm by name and fingerprint); forgetting one.

use std::time::{Duration, Instant};

use rakumux_sensagent::feed::{DeviceView, PairingAction, PairingView};
use rakumux_sensagent::signing::pairing::PairingState as Signer;

use super::runtime::AppEvent;
use super::types::{AppMode, DevicesDialog, DevicesState};
use super::App;

/// How often a pairing under way is read.
const PAIRING_EVERY: Duration = Duration::from_secs(1);

impl App {
    /// Settings → Paired devices.
    pub fn open_devices_dialog(&mut self) {
        self.mode = AppMode::Devices(DevicesDialog {
            state: DevicesState::Loading,
            devices: Vec::new(),
            selected: 0,
            removing: false,
        });
        self.devices_poll_at = None;
        let access = self.account_access.clone();
        self.exec
            .job(move || AppEvent::Devices(access.devices(None)));
    }

    pub(super) fn on_devices(&mut self, result: Result<Vec<DeviceView>, String>) {
        let AppMode::Devices(dialog) = &mut self.mode else {
            return;
        };
        match result {
            Ok(devices) => {
                dialog.selected = dialog.selected.min(devices.len().saturating_sub(1));
                dialog.devices = devices;
                dialog.removing = false;
                if matches!(dialog.state, DevicesState::Loading) {
                    dialog.state = DevicesState::List;
                }
            }
            Err(error) => dialog.state = DevicesState::Failed(error),
        }
    }

    /// `n`: open a pairing (its code, link and QR code).
    pub fn start_device_pairing(&mut self) {
        if let AppMode::Devices(dialog) = &mut self.mode {
            dialog.state = DevicesState::Starting;
        }
        self.pairing_job(PairingAction::Start);
    }

    /// `y`/`n` to the device proposed.
    pub fn answer_device(&mut self, yes: bool) {
        self.pairing_job(PairingAction::Answer { yes });
    }

    /// `d` twice: forget the selected device.
    pub fn forget_device(&mut self) {
        let AppMode::Devices(dialog) = &mut self.mode else {
            return;
        };
        let Some(device) = dialog.devices.get(dialog.selected) else {
            return;
        };
        let key_id = device.key_id.clone();
        let name = device.name.clone();
        dialog.removing = false;
        let access = self.account_access.clone();
        self.exec
            .job(move || AppEvent::Devices(access.devices(Some(&key_id))));
        self.set_status(format!("Forgot {name}."));
    }

    /// Close the dialog, cancelling a pairing under way.
    pub fn close_devices_dialog(&mut self) {
        if matches!(&self.mode, AppMode::Devices(dialog) if dialog.pairing_live()) {
            let access = self.account_access.clone();
            self.exec.job(move || {
                let _ = access.pairing(PairingAction::Cancel);
                AppEvent::DevicePairing(Ok(None))
            });
        }
        self.devices_poll_at = None;
        self.mode = AppMode::Normal;
    }

    fn pairing_job(&mut self, action: PairingAction) {
        self.devices_polling = true;
        let access = self.account_access.clone();
        self.exec
            .job(move || AppEvent::DevicePairing(access.pairing(action)));
    }

    pub(super) fn on_device_pairing(&mut self, result: Result<Option<PairingView>, String>) {
        self.devices_polling = false;
        let AppMode::Devices(dialog) = &mut self.mode else {
            return;
        };
        if matches!(dialog.state, DevicesState::List | DevicesState::Loading) {
            return;
        }
        match result {
            Ok(Some(view)) => {
                let paired = matches!(view.state, Signer::Paired { .. });
                let live = matches!(view.state, Signer::Open | Signer::Proposed { .. });
                dialog.state = DevicesState::Pairing(view);
                self.devices_poll_at = live.then(|| Instant::now() + PAIRING_EVERY);
                if paired {
                    let access = self.account_access.clone();
                    self.exec
                        .job(move || AppEvent::Devices(access.devices(None)));
                }
            }
            Ok(None) => {
                dialog.state = DevicesState::Failed("Sign in first (Settings → Account).".into());
            }
            Err(error) => dialog.state = DevicesState::Failed(error),
        }
    }

    /// The tick's pairing work: read a pairing under way every second.
    pub(super) fn devices_timers(&mut self, now: Instant) {
        if self.devices_poll_at.is_some_and(|at| now >= at) && !self.devices_polling {
            self.devices_poll_at = None;
            if matches!(&self.mode, AppMode::Devices(dialog) if dialog.pairing_live()) {
                self.pairing_job(PairingAction::Show);
            }
        }
    }
}
