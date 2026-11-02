//! Tips in the footer: one key (or gesture, or command) and what it does, a
//! different one each day (the local date picks it), so it's the same all
//! day. A click on it shows the next one. Nothing is fetched or stored but
//! the setting that turns them off (`sessions.tips`); the clicks are this
//! window's.

use chrono::Datelike;

use super::types::AccountStatus;
use super::App;
use crate::help::KeyAction;
use crate::layout::str_width;

/// A tip as the footer shows it: its keys, drawn as a button's are, then
/// what they do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tip {
    pub keys: String,
    pub text: &'static str,
}

impl Tip {
    /// Columns it takes: `keys text`.
    pub fn width(&self) -> usize {
        str_width(&self.keys) + 1 + str_width(self.text)
    }
}

/// Where a tip's keys come from, so a test can check each one works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum Source {
    /// A Rakumux mode action: its keys are among Settings' keys for it.
    Key(KeyAction),
    /// A `rakumux` command line.
    Command(&'static str),
    /// Dragging over a session's terminal.
    Drag,
}

/// A tip's keys: fixed, or the configured Rakumux key.
#[derive(Debug, Clone, Copy)]
enum Keys {
    Fixed(&'static str),
    RakumuxKey,
}

struct Entry {
    keys: Keys,
    text: &'static str,
    #[cfg_attr(not(test), allow(dead_code))]
    source: Source,
    /// Only while signed out (signing in).
    signed_out: bool,
}

const fn tip(keys: &'static str, text: &'static str, source: Source) -> Entry {
    Entry {
        keys: Keys::Fixed(keys),
        text,
        source,
        signed_out: false,
    }
}

/// Every tip, in the order the days go through them.
const TIPS: &[Entry] = &[
    tip("T", "terminal panel", Source::Key(KeyAction::TerminalPanel)),
    tip("f", "every agent's replies", Source::Key(KeyAction::Feed)),
    tip("t", "rtop: what sessions use", Source::Key(KeyAction::Rtop)),
    Entry {
        keys: Keys::RakumuxKey,
        text: "session or Rakumux mode",
        source: Source::Key(KeyAction::RakumuxMode),
        signed_out: false,
    },
    tip(
        "U",
        "update (sessions stay up)",
        Source::Key(KeyAction::CheckForUpdate),
    ),
    tip(
        "w",
        "where it changed files",
        Source::Key(KeyAction::Places),
    ),
    tip(
        "v",
        "Master detail, Grid, Tabs",
        Source::Key(KeyAction::CycleLayout),
    ),
    tip("p", "pin to the top", Source::Key(KeyAction::Pin)),
    tip("Tab", "last session", Source::Key(KeyAction::LastSession)),
    Entry {
        keys: Keys::Fixed("a"),
        text: "answer agents from the web",
        source: Source::Key(KeyAction::Account),
        signed_out: true,
    },
    tip("drag", "copies a session's text", Source::Drag),
    tip(
        "rakumux remote on",
        "pairs a phone",
        Source::Command("remote on"),
    ),
];

/// Today in local time, as a day number: the tip changes at midnight.
pub fn today() -> i32 {
    chrono::Local::now().date_naive().num_days_from_ce()
}

impl App {
    /// The tip the footer shows on `day`: the day's among those that apply
    /// now (signing in only while signed out), as many on as it was
    /// clicked. `None` while tips are off.
    pub fn tip(&self, day: i32) -> Option<Tip> {
        if !self.settings.sessions.tips {
            return None;
        }
        let tips: Vec<&Entry> = TIPS
            .iter()
            .filter(|entry| self.tip_applies(entry))
            .collect();
        let count = tips.len();
        if count == 0 {
            return None;
        }
        let today = usize::try_from(i64::from(day).rem_euclid(i64::try_from(count).ok()?)).ok()?;
        let entry = tips.get((today + self.tip_offset % count) % count)?;
        let keys = match entry.keys {
            Keys::Fixed(keys) => keys.to_string(),
            Keys::RakumuxKey => self.rakumux_key.label(),
        };
        Some(Tip {
            keys,
            text: entry.text,
        })
    }

    /// Show the next tip (a click on it).
    pub fn next_tip(&mut self) {
        self.tip_offset = self.tip_offset.wrapping_add(1);
    }

    fn tip_applies(&self, entry: &Entry) -> bool {
        !entry.signed_out || matches!(self.account, AccountStatus::SignedOut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::{harness, view, FakeAccount};

    fn all_tips(h: &crate::app::testing::Harness) -> Vec<Tip> {
        (0..TIPS.len() as i32)
            .filter_map(|day| h.app.tip(day))
            .collect()
    }

    #[test]
    fn a_day_shows_the_same_tip_and_the_next_day_the_next() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.app.account = AccountStatus::SignedOut;
        let day = 739_000;
        assert_eq!(h.app.tip(day), h.app.tip(day));
        assert_ne!(h.app.tip(day), h.app.tip(day + 1));
        // Every tip comes round, one a day.
        let shown: std::collections::HashSet<_> = (day..day + TIPS.len() as i32)
            .filter_map(|day| h.app.tip(day))
            .map(|tip| tip.text)
            .collect();
        assert_eq!(shown.len(), TIPS.len());
        assert_eq!(h.app.tip(day), h.app.tip(day + TIPS.len() as i32));
        // A date's day number, as `today` counts.
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        assert_eq!(date.num_days_from_ce(), 739_922);
        assert!(today() > 739_000);
        // Signed in, the days go through the others.
        h.app.account = AccountStatus::SignedIn(FakeAccount::person());
        let count = TIPS.len() as i32 - 1;
        let shown: std::collections::HashSet<_> = (day..day + count)
            .filter_map(|day| h.app.tip(day))
            .map(|tip| tip.text)
            .collect();
        assert_eq!(shown.len(), TIPS.len() - 1);
    }

    #[test]
    fn a_click_shows_the_next_tip_and_skips_signing_in_while_signed_in() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.app.account = AccountStatus::SignedOut;
        let day = 739_922;
        let first = h.app.tip(day).unwrap();
        h.app.next_tip();
        let second = h.app.tip(day).unwrap();
        assert_ne!(first, second);
        // The one the next day would show (signed out, none is skipped).
        let mut fresh = harness(vec![view("A", "api", 10)]);
        fresh.app.account = AccountStatus::SignedOut;
        assert_eq!(fresh.app.tip(day + 1), Some(second));
        // Clicked round, every tip shows once.
        let mut seen = vec![first.text];
        for _ in 1..TIPS.len() {
            seen.push(h.app.tip(day).unwrap().text);
            h.app.next_tip();
        }
        assert_eq!(h.app.tip(day), Some(first));
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), TIPS.len());
        // Signed in, the sign-in tip never shows, and a click always
        // moves on.
        h.app.account = AccountStatus::SignedIn(FakeAccount::person());
        let shown = all_tips(&h);
        assert!(shown.iter().all(|tip| tip.keys != "a"), "{shown:?}");
        for _ in 0..TIPS.len() * 2 {
            let before = h.app.tip(day);
            h.app.next_tip();
            assert_ne!(h.app.tip(day), before);
        }
    }

    #[test]
    fn the_setting_hides_tips() {
        let mut h = harness(vec![view("A", "api", 10)]);
        assert!(h.app.settings.sessions.tips, "on by default");
        h.app.settings.sessions.tips = false;
        assert_eq!(h.app.tip(739_922), None);
        h.app.next_tip();
        assert_eq!(h.app.tip(739_922), None);
    }

    #[test]
    fn the_modes_tip_names_the_configured_rakumux_key() {
        let mut h = harness(vec![view("A", "api", 10)]);
        let modes = |h: &crate::app::testing::Harness| {
            all_tips(h)
                .into_iter()
                .find(|tip| tip.text == "session or Rakumux mode")
                .unwrap()
        };
        assert_eq!(modes(&h).keys, "Ctrl-\\");
        h.app.rakumux_key = crate::app::keys::KeyBinding::parse("ctrl+g").unwrap();
        assert_eq!(modes(&h).keys, h.app.rakumux_key.label());
        assert_ne!(modes(&h).keys, "Ctrl-\\");
    }

    /// Every tip's keys do what it says: a Rakumux mode key is among
    /// Settings' keys for its action (the table the key handlers follow;
    /// `app::tests` checks the handlers), a command parses, and a drag
    /// copies (`app::tests`' `a_drag_over_a_session_copies_its_text_and_a_click_goes_in`).
    #[test]
    fn every_tip_s_keys_are_real() {
        let h = harness(vec![view("A", "api", 10)]);
        let ctx = h.app.modal_context(false);
        for entry in TIPS {
            let keys = match entry.keys {
                Keys::Fixed(keys) => keys.to_string(),
                Keys::RakumuxKey => h.app.rakumux_key.label(),
            };
            match entry.source {
                Source::Key(action) => {
                    let table = action.keys_for(&ctx);
                    assert!(
                        table.split(", ").any(|key| key == keys),
                        "{keys:?} isn't among {action:?}'s keys {table:?}"
                    );
                }
                Source::Command(line) => {
                    assert_eq!(keys, format!("rakumux {line}"));
                    assert!(crate::cli::is_command(line), "{line:?}");
                }
                Source::Drag => assert_eq!(keys, "drag"),
            }
            assert!(
                str_width(&keys) + 1 + str_width(entry.text) <= 32,
                "{keys} {}: too long for the footer",
                entry.text
            );
        }
        assert!((10..=12).contains(&TIPS.len()));
    }
}
