use super::*;
use crate::app::testing::{self, harness, view, FakeUpdater, Harness};
use crate::app::types::AppResult;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;

const NEWER: &str = "99.1.0";

/// A window whose updater is `updater`, before its first tick.
fn window(updater: &Arc<FakeUpdater>) -> Harness {
    let mut h = harness(vec![view("A", "a", 20)]);
    h.app.updater = updater.clone();
    h
}

/// Another window of the same Rakumux home as `h`.
fn other_window(h: &Harness, updater: &Arc<FakeUpdater>) -> crate::app::App {
    let mut options = testing::options(&h.fake, testing::scope(), &h.home);
    options.updater = updater.clone();
    let mut app = crate::app::App::new(options);
    app.resize(160, 48);
    app.start();
    app.process_pending();
    app
}

fn tick_app(app: &mut crate::app::App, at: Instant) {
    app.on_tick(at);
    app.process_pending();
}

fn tick(h: &mut Harness, at: Instant) {
    tick_app(&mut h.app, at);
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn footer(h: &Harness) -> String {
    crate::components::footer::footer_line(&h.app, 160)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// The window drawn at `width` × `height`, as text.
fn screen(app: &mut crate::app::App, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer.get(x, y).symbol());
        }
        out.push('\n');
    }
    out
}

fn state(h: &Harness) -> UpdateState {
    UpdateState::load_from(&h.home.path().join("update_state.json")).unwrap()
}

/// A key typed into the window as the terminal sends it.
async fn type_key(h: &mut Harness, code: KeyCode) {
    let result = h
        .app
        .handle_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        .await;
    assert_eq!(result, AppResult::Continue);
    h.app.process_pending();
}

/// A window that installed [`NEWER`] by itself and waits to reload.
fn installed(updater: &Arc<FakeUpdater>) -> (Harness, Instant) {
    *updater.launcher.lock().unwrap() = Some("/updated/rakumux.sh".into());
    let mut h = window(updater);
    let start = Instant::now();
    tick(&mut h, start);
    assert_eq!(h.app.update_ready(), Some(NEWER));
    (h, start)
}

#[tokio::test]
async fn a_new_release_installs_without_asking_and_reloads_once_the_window_is_idle() {
    let updater = FakeUpdater::with_latest(NEWER);
    let (mut h, start) = installed(&updater);
    let daemon_calls = h.fake.calls();
    assert_eq!(updater.calls(), ["check", "install 99.1.0"]);
    assert!(matches!(h.app.mode, AppMode::Normal), "never a prompt");
    assert!(h.app.status().is_none());
    assert!(
        footer(&h).contains("Updated to 99.1.0 · reloads when idle (R now)"),
        "{}",
        footer(&h)
    );
    // Within a minute of the window starting (or being used): not yet.
    tick(&mut h, start + secs(30));
    assert!(!h.app.restart_requested);
    tick(&mut h, start + RELOAD_IDLE + secs(1));
    assert!(h.app.restart_requested);
    assert_eq!(
        h.app.restart_launcher.as_deref(),
        Some(std::path::Path::new("/updated/rakumux.sh"))
    );
    assert_eq!(updater.calls(), ["check", "install 99.1.0"]);
    assert_eq!(
        h.fake.calls(),
        daemon_calls,
        "the daemon and the sessions are left alone"
    );
    // The release stays on record for the windows still on the older one.
    assert!(state(&h).pending_upgrade);
}

#[tokio::test]
async fn no_reload_while_someone_types() {
    let updater = FakeUpdater::with_latest(NEWER);
    let (mut h, _) = installed(&updater);
    type_key(&mut h, KeyCode::Down).await;
    let typed = Instant::now();
    tick(&mut h, typed + secs(59));
    assert!(!h.app.restart_requested);
    type_key(&mut h, KeyCode::Up).await;
    let typed = Instant::now();
    tick(&mut h, typed + secs(59));
    assert!(!h.app.restart_requested);
    tick(&mut h, typed + RELOAD_IDLE + secs(1));
    assert!(h.app.restart_requested);
}

#[tokio::test]
async fn no_reload_in_session_mode_a_dialog_a_half_typed_filter_or_with_a_panel_shell() {
    let later = || Instant::now() + secs(3600);
    // Session mode: keys go to the program.
    let updater = FakeUpdater::with_latest(NEWER);
    let (mut h, _) = installed(&updater);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    tick(&mut h, later());
    assert!(!h.app.restart_requested);
    h.rakumux().await;
    tick(&mut h, later());
    assert!(h.app.restart_requested, "back in Rakumux mode and idle");

    // A dialog: Settings.
    let (mut h, _) = installed(&updater);
    h.press(KeyCode::Char(',')).await;
    assert!(!matches!(h.app.mode, AppMode::Normal));
    tick(&mut h, later());
    assert!(!h.app.restart_requested);
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    tick(&mut h, later());
    assert!(h.app.restart_requested);

    // The list filter being typed.
    let (mut h, _) = installed(&updater);
    h.press(KeyCode::Char('/')).await;
    assert!(h.app.list.filter_editing);
    tick(&mut h, later());
    assert!(!h.app.restart_requested);

    // The terminal panel: taking keys, and hidden (its shell still runs).
    let (mut h, _) = installed(&updater);
    h.app.show_terminal_panel();
    h.app.process_pending();
    assert_eq!(h.app.focus, Focus::Terminal);
    tick(&mut h, later());
    assert!(!h.app.restart_requested);
    h.app.hide_terminal_panel();
    assert_eq!(h.app.focus, Focus::List);
    tick(&mut h, later());
    assert!(
        !h.app.restart_requested,
        "a reload would end the panel's shell"
    );
}

#[tokio::test]
async fn r_reloads_into_the_new_version_at_once() {
    let updater = FakeUpdater::with_latest(NEWER);
    let (mut h, _) = installed(&updater);
    h.press(KeyCode::Char('R')).await;
    h.press(KeyCode::Enter).await;
    assert!(h.app.restart_requested);
    assert_eq!(
        h.app.restart_launcher.as_deref(),
        Some(std::path::Path::new("/updated/rakumux.sh"))
    );
}

#[tokio::test]
async fn with_the_setting_off_a_new_release_is_told_once_and_u_still_asks() {
    let updater = FakeUpdater::with_latest(NEWER);
    let mut h = window(&updater);
    h.app.settings.sessions.auto_update = false;
    let start = Instant::now();
    tick(&mut h, start);
    assert_eq!(updater.calls(), ["check"]);
    assert_eq!(h.app.update_ready(), None);
    assert_eq!(
        h.app.status(),
        Some("Update 99.1.0 available · U to install")
    );
    // Then the update modal asks; `Later` leaves it for that version.
    tick(&mut h, start + secs(10));
    assert!(
        matches!(&h.app.mode, AppMode::Upgrade(dialog) if dialog.prompted && !dialog.installed),
        "{:?}",
        h.app.mode
    );
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(state(&h).dismissed_version.as_deref(), Some(NEWER));
    // Once the message is gone, the footer keeps a quiet line.
    assert!(h.app.status().is_none());
    assert!(
        footer(&h).contains("Update 99.1.0 available · U to install"),
        "{}",
        footer(&h)
    );
    // Not told again: not each minute, nor by another window or a restart.
    for minute in 1..=5 {
        tick(&mut h, start + secs(60 * minute + 1));
        assert!(h.app.status().is_none(), "minute {minute}");
        assert!(matches!(h.app.mode, AppMode::Normal), "minute {minute}");
    }
    let mut other = other_window(&h, &updater);
    other.settings.sessions.auto_update = false;
    tick_app(&mut other, Instant::now());
    tick_app(&mut other, Instant::now() + secs(1));
    assert!(other.status().is_none());
    assert!(matches!(other.mode, AppMode::Normal), "answered Later");
    assert_eq!(other.pending_update_version.as_deref(), Some(NEWER));
    assert_eq!(updater.calls(), ["check"], "checked once an hour");
    // `U` asks before installing, as ever.
    h.press(KeyCode::Char('U')).await;
    assert!(matches!(&h.app.mode, AppMode::Upgrade(dialog) if dialog.update_available));
    assert_eq!(updater.calls(), ["check", "check"]);
    h.press(KeyCode::Esc).await;
    // A newer release is told about too.
    let mut stale = state(&h);
    stale.last_check = Some(0);
    stale
        .save_to(&h.home.path().join("update_state.json"))
        .unwrap();
    *updater.latest.lock().unwrap() = Some("99.2.0".into());
    let next_day = Instant::now() + secs(3600);
    tick(&mut h, next_day);
    assert_eq!(
        h.app.status(),
        Some("Update 99.2.0 available · U to install")
    );
    assert_eq!(state(&h).notified_version.as_deref(), Some("99.2.0"));
}

/// A newer release this window doesn't install by itself opens the update
/// modal in Rakumux mode: what's new from its release notes, then `Update
/// now` (installed, then the one restart: UI, server and services) or
/// `Later`. Never over session mode or another dialog.
#[tokio::test]
async fn a_new_release_opens_the_update_modal_with_whats_new_and_update_now_installs() {
    let updater = FakeUpdater::with_latest(NEWER);
    *updater.notes.lock().unwrap() = Some("## Session list\n\n- Repositories first.".into());
    *updater.launcher.lock().unwrap() = Some("/updated/rakumux.sh".into());
    let mut h = window(&updater);
    h.app.settings.sessions.auto_update = false;
    // In session mode the program has the keys: no modal.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    let start = Instant::now();
    tick(&mut h, start);
    tick(&mut h, start + secs(1));
    assert!(matches!(h.app.mode, AppMode::Normal));
    h.rakumux().await;
    tick(&mut h, start + secs(2));
    match &h.app.mode {
        AppMode::Upgrade(dialog) => {
            assert!(dialog.prompted && dialog.update_available && !dialog.installed);
            assert_eq!(dialog.latest_version.as_deref(), Some(NEWER));
            assert_eq!(
                dialog.notes.as_deref(),
                Some("## Session list\n\n- Repositories first.")
            );
        }
        mode => panic!("no update modal: {mode:?}"),
    }
    assert_eq!(*updater.notes_read.lock().unwrap(), [NEWER]);
    let screen = screen(&mut h.app, 100, 40);
    assert!(screen.contains("Rakumux 99.1.0 is out"), "{screen}");
    assert!(screen.contains("Repositories first."), "{screen}");
    assert!(
        screen.contains("Update now") && screen.contains("Later"),
        "{screen}"
    );
    // Update now: installed, then the one restart.
    h.press(KeyCode::Enter).await;
    assert_eq!(updater.calls(), ["check", "install 99.1.0"]);
    assert!(h.app.restart_requested && h.app.restart_after_update);
    assert_eq!(
        h.app.restart_launcher.as_deref(),
        Some(std::path::Path::new("/updated/rakumux.sh"))
    );
}

/// With automatic updates on, the release installs by itself; the modal
/// then offers the restart into it: `Restart now` is the one restart, `Later`
/// leaves it to the idle reload and isn't asked again for that version.
#[tokio::test]
async fn an_automatic_install_offers_the_restart_now_or_later() {
    let updater = FakeUpdater::with_latest(NEWER);
    let (mut h, start) = installed(&updater);
    tick(&mut h, start + secs(5));
    assert!(
        matches!(&h.app.mode, AppMode::Upgrade(dialog) if dialog.prompted && dialog.installed),
        "{:?}",
        h.app.mode
    );
    let screen = screen(&mut h.app, 100, 40);
    assert!(screen.contains("Updated to 99.1.0"), "{screen}");
    assert!(screen.contains("Restart now"), "{screen}");
    // Restart now.
    h.press(KeyCode::Enter).await;
    assert!(h.app.restart_requested && h.app.restart_after_update);
    assert_eq!(
        updater.calls(),
        ["check", "install 99.1.0"],
        "installed once"
    );

    // Later: no modal again, and the idle reload still comes.
    let (mut h, start) = installed(&updater);
    tick(&mut h, start + secs(5));
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(!h.app.restart_requested);
    assert_eq!(state(&h).dismissed_installed.as_deref(), Some(NEWER));
    tick(&mut h, start + secs(10));
    assert!(matches!(h.app.mode, AppMode::Normal), "not asked again");
    tick(&mut h, Instant::now() + RELOAD_IDLE + secs(1));
    assert!(h.app.restart_requested, "the idle reload");

    // Left open, the modal doesn't hold the idle reload back.
    let (mut h, start) = installed(&updater);
    let mut fresh = state(&h);
    fresh.dismissed_installed = None;
    fresh
        .save_to(&h.home.path().join("update_state.json"))
        .unwrap();
    tick(&mut h, start + secs(5));
    assert!(matches!(h.app.mode, AppMode::Upgrade(_)));
    tick(&mut h, start + RELOAD_IDLE + secs(1));
    assert!(h.app.restart_requested);
}

#[tokio::test]
async fn npm_homebrew_and_source_builds_are_told_how_to_update_and_never_installed() {
    for (method, told, hint) in [
        (
            InstallMethod::Npm,
            "Update 99.1.0 available: npm install -g rakumux@latest",
            true,
        ),
        (
            InstallMethod::Homebrew,
            "Update 99.1.0 available: brew upgrade rakumux",
            true,
        ),
        // A source build's version says nothing: told once, no lasting line.
        (
            InstallMethod::Source,
            "Update 99.1.0 available: rebuild your checkout",
            false,
        ),
    ] {
        let updater = FakeUpdater::with_latest(NEWER);
        *updater.method.lock().unwrap() = Some(method);
        let mut h = window(&updater);
        let start = Instant::now();
        tick(&mut h, start);
        assert_eq!(updater.calls(), ["check"], "{method:?}");
        assert_eq!(h.app.status(), Some(told), "{method:?}");
        tick(&mut h, start + secs(61));
        assert!(h.app.status().is_none(), "{method:?}: told once");
        assert_eq!(
            footer(&h).contains(told),
            hint,
            "{method:?}: {}",
            footer(&h)
        );
        assert_eq!(h.app.update_ready(), None);
        assert!(!h.app.restart_requested);
    }
}

#[tokio::test]
async fn a_release_the_updater_refuses_as_unsigned_is_never_reloaded_into() {
    // The signature check is binary::install's (updater/binary_tests.rs):
    // automatic installs go through it like U, so an unsigned release fails.
    let updater = FakeUpdater::with_latest(NEWER);
    let refused = "release v99.1.0 has no signature that could be downloaded \
                   (checksums.txt.minisig); update was not installed";
    *updater.install_error.lock().unwrap() = Some(refused.into());
    let mut h = window(&updater);
    let start = Instant::now();
    tick(&mut h, start);
    assert_eq!(
        h.app.status(),
        Some(format!("Couldn't update to 99.1.0: {refused}").as_str())
    );
    tick(&mut h, start + RELOAD_IDLE + secs(61));
    assert_eq!(h.app.update_ready(), None);
    assert!(!h.app.restart_requested);
    assert_eq!(
        updater.calls(),
        ["check", "install 99.1.0"],
        "not tried again"
    );
}

#[tokio::test]
async fn a_failed_install_keeps_the_old_one_says_so_once_and_waits_for_the_next_check() {
    let updater = FakeUpdater::with_latest(NEWER);
    *updater.install_error.lock().unwrap() =
        Some("SHA-256 checksum mismatch; update was not installed".into());
    let mut h = window(&updater);
    let start = Instant::now();
    tick(&mut h, start);
    assert_eq!(updater.calls(), ["check", "install 99.1.0"]);
    assert_eq!(
        h.app.status(),
        Some("Couldn't update to 99.1.0: SHA-256 checksum mismatch; update was not installed")
    );
    assert_eq!(h.app.update_ready(), None);
    assert!(!h.app.restart_requested);
    assert_eq!(state(&h).failed_version.as_deref(), Some(NEWER));
    // Not tried again, nor said again, until the next daily check.
    for minute in 1..=3 {
        tick(&mut h, start + secs(60 * minute + 1));
        assert!(h.app.status().is_none());
    }
    assert_eq!(updater.calls(), ["check", "install 99.1.0"]);
    assert!(
        footer(&h).contains("Update 99.1.0 available · U to install"),
        "{}",
        footer(&h)
    );
    // The next day's check tries again; failing again, it stays quiet.
    let mut stale = state(&h);
    stale.last_check = Some(0);
    stale
        .save_to(&h.home.path().join("update_state.json"))
        .unwrap();
    tick(&mut h, start + secs(3600));
    assert_eq!(
        updater.calls(),
        ["check", "install 99.1.0", "check", "install 99.1.0"]
    );
    assert!(h.app.status().is_none());
    // And it succeeds once it can.
    *updater.install_error.lock().unwrap() = None;
    let mut stale = state(&h);
    stale.last_check = Some(0);
    stale
        .save_to(&h.home.path().join("update_state.json"))
        .unwrap();
    tick(&mut h, start + secs(7200));
    assert_eq!(h.app.update_ready(), Some(NEWER));
}

#[tokio::test]
async fn only_one_window_installs_and_the_others_reload_into_its_install() {
    let updater = FakeUpdater::with_latest(NEWER);
    let (h, _) = installed(&updater);
    // Another window, while the first still holds the installation's lock.
    let busy = Arc::new(FakeUpdater::default());
    *busy.busy.lock().unwrap() = true;
    let mut other = other_window(&h, &busy);
    let start = Instant::now();
    tick_app(&mut other, start);
    // The day's check was made; it learns of the release from the first.
    assert_eq!(busy.calls(), ["install 99.1.0"]);
    assert_eq!(other.update_ready(), None);
    assert!(
        other.status().is_none(),
        "another window's install isn't a failure"
    );
    assert_eq!(state(&h).failed_version, None);
    // Once the lock is free, its install finds the new version there.
    *busy.busy.lock().unwrap() = false;
    tick_app(&mut other, start + secs(30));
    assert_eq!(busy.calls().len(), 1, "looked at again a minute later");
    tick_app(&mut other, start + secs(61));
    assert_eq!(busy.calls(), ["install 99.1.0", "install 99.1.0"]);
    assert_eq!(other.update_ready(), Some(NEWER));
    tick_app(&mut other, start + secs(200));
    assert!(other.restart_requested);
}

#[tokio::test]
async fn prereleases_and_versions_not_newer_are_never_installed_or_told() {
    for latest in ["0.0.1", crate::app::app_version(), "99.0.0-rc.1"] {
        for auto in [true, false] {
            let updater = FakeUpdater::with_latest(latest);
            let mut h = window(&updater);
            h.app.settings.sessions.auto_update = auto;
            tick(&mut h, Instant::now());
            assert_eq!(updater.calls(), ["check"], "{latest}");
            assert!(h.app.status().is_none(), "{latest}");
            assert_eq!(h.app.update_hint(), None, "{latest}");
            assert_eq!(h.app.update_ready(), None);
        }
    }
    // Nor one another window recorded.
    let updater = Arc::new(FakeUpdater::default());
    let mut h = window(&updater);
    let mut recorded = UpdateState::default();
    recorded.mark_update_available("99.0.0-rc.1".into(), InstallMethod::Binary);
    recorded.update_last_check();
    recorded
        .save_to(&h.home.path().join("update_state.json"))
        .unwrap();
    tick(&mut h, Instant::now());
    assert!(updater.calls().is_empty());
}

#[tokio::test]
async fn turning_the_setting_on_installs_a_release_already_known() {
    let updater = FakeUpdater::with_latest(NEWER);
    let mut h = window(&updater);
    let row = crate::app::types::SettingsItem::AutoUpdate.index();
    assert_eq!(h.app.modal_context(false).general[row].value, "[On] Off");
    h.press(KeyCode::Char(',')).await;
    for _ in 0..row {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert!(!h.app.settings.sessions.auto_update);
    assert_eq!(h.app.modal_context(false).general[row].value, "On [Off]");
    let saved = crate::app::Settings::load_from(&h.home.path().join("config.json")).unwrap();
    assert!(!saved.sessions.auto_update);
    let start = Instant::now();
    tick(&mut h, start);
    assert_eq!(updater.calls(), ["check"]);
    h.press(KeyCode::Enter).await;
    assert!(h.app.settings.sessions.auto_update);
    tick(&mut h, start + secs(1));
    assert_eq!(updater.calls(), ["check", "install 99.1.0"]);
    assert_eq!(h.app.update_ready(), Some(NEWER));
}

#[tokio::test]
async fn a_check_that_fails_is_quiet_and_tried_again_later() {
    let updater = Arc::new(FakeUpdater::default());
    *updater.check_error.lock().unwrap() = Some("cannot reach GitHub releases".into());
    let mut h = window(&updater);
    let start = Instant::now();
    tick(&mut h, start);
    assert!(h.app.status().is_none());
    tick(&mut h, start + secs(61));
    assert_eq!(updater.calls(), ["check"], "not every minute");
    tick(&mut h, start + secs(3601));
    assert_eq!(updater.calls(), ["check", "check"]);
}
