//! Phone access: remote access for the Rakumux app on a phone
//! (`crate::remote_access`). Checked at startup; offered once ("Use
//! Rakumux from your phone?") when Tailscale runs here and it was never
//! set up; and the pairing dialog, with the QR code the app scans.
//! `rakumux remote` turns it on and off any time.

use super::runtime::AppEvent;
use super::types::{AppMode, Focus, PairingDialog, PairingState};
use super::App;
use crate::remote_access::Pairing;

impl App {
    /// Startup: whether phone access is on, and whether to offer it (not
    /// on, never offered, never set up, Tailscale here).
    pub(super) fn check_phone_access(&mut self) {
        let phone = self.phone.clone();
        let offered = self.settings.sessions.phone_offer_seen;
        self.exec.job(move || {
            let enabled = phone.enabled();
            let offer = !enabled && !offered && phone.worth_offering();
            AppEvent::PhoneChecked { enabled, offer }
        });
    }

    pub(super) fn on_phone_checked(&mut self, enabled: bool, offer: bool) {
        if self.phone_enabled.is_none() {
            self.phone_enabled = Some(enabled);
        }
        self.phone_offer_pending = offer;
        self.offer_phone_access();
    }

    /// Show the pending offer once nothing else is: Rakumux mode, no
    /// dialog (so after the welcome tour). It's recorded as offered as it
    /// opens, so it never comes back. Returns whether it opened.
    pub fn offer_phone_access(&mut self) -> bool {
        if !self.phone_offer_pending
            || self.creating_session
            || self.pending_focus.is_some()
            || !matches!(self.mode, AppMode::Normal)
            || self.focus != Focus::List
        {
            return false;
        }
        self.phone_offer_pending = false;
        self.mode = AppMode::PhoneOffer;
        self.settings.sessions.phone_offer_seen = true;
        if let Err(error) = self.save_settings() {
            self.set_status(format!("Couldn't save settings: {error}"));
        }
        true
    }

    /// The offer was declined.
    pub fn decline_phone_access(&mut self) {
        self.mode = AppMode::Normal;
        self.set_status("Phone access stays off. `rakumux remote on` turns it on.");
    }

    /// Turn phone access on, then show how to pair.
    pub fn turn_phone_access_on(&mut self) {
        self.mode = AppMode::Pairing(PairingDialog::loading(true));
        let phone = self.phone.clone();
        self.exec
            .job(move || AppEvent::PhonePairing(phone.enable().map(Some)));
    }

    /// `x` in the pairing dialog.
    pub fn turn_phone_access_off(&mut self) {
        if let AppMode::Pairing(dialog) = &mut self.mode {
            dialog.state = PairingState::TurningOff;
        }
        let phone = self.phone.clone();
        self.exec.job(move || AppEvent::PhoneOff(phone.disable()));
    }

    pub(super) fn on_phone_pairing(&mut self, result: Result<Option<Pairing>, String>) {
        let state = match result {
            Ok(Some(pairing)) => {
                self.phone_enabled = Some(true);
                PairingState::Ready(pairing)
            }
            Ok(None) => {
                self.phone_enabled = Some(false);
                PairingState::Off
            }
            Err(error) => PairingState::Failed(error),
        };
        match &mut self.mode {
            AppMode::Pairing(dialog) => dialog.state = state,
            // Closed meanwhile.
            _ => match state {
                PairingState::Ready(_) => self.set_status(
                    "Phone access is on. `rakumux remote pair` shows the pairing code.",
                ),
                PairingState::Failed(error) => self.set_status(format!("Phone access: {error}")),
                _ => {}
            },
        }
    }

    pub(super) fn on_phone_off(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.phone_enabled = Some(false);
                if matches!(self.mode, AppMode::Pairing(_)) {
                    self.close_pairing();
                }
                self.set_status("Phone access is off.");
            }
            Err(error) => match &mut self.mode {
                AppMode::Pairing(dialog) => dialog.state = PairingState::Failed(error),
                _ => self.set_status(format!("Couldn't turn phone access off: {error}")),
            },
        }
    }

    /// Close the pairing dialog.
    pub fn close_pairing(&mut self) {
        self.mode = AppMode::Normal;
    }
}
