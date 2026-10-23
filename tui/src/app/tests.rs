//! App-level tests against the in-memory `FakeApi` with the inline
//! executor: startup, live list, key routing, focus, dialogs, layouts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use session_core::api::{ApiError, ErrorCode, Role, SessionEvent, StreamCommand, StreamEvent};
use session_core::client::{AttachEvent, AttachOutgoing, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, ScopeRoot, SessionStatus};

use super::testing::{
    aged, folder, harness, harness_with_folders, on_branch, options, scope, start, view, view_in,
    FakeApi, Harness,
};
use super::types::{
    AppMode, AppResult, ConfirmAction, DaemonRestartDialog, Focus, NewFolder, PickerRow,
    PickerStage, SettingsItem,
};
use super::*;
use crate::help::{KeyAction, ModalState, ModalTab};
use crate::layout::SessionLayout;

fn id(s: &str) -> SessionId {
    SessionId::from_string(s)
}

// ---------------------------------------------------------------- startup

#[tokio::test]
async fn unread_completion_is_not_acknowledged_by_selection_or_passive_previews() {
    for layout in SessionLayout::ALL {
        let mut session = view("A", "agent", 10);
        session.lifecycle = Lifecycle::Done;
        let mut h = harness_with_folders(vec![session.clone(), view("B", "other", 1)], &[], layout);
        h.event(
            "A",
            SessionEvent::Updated {
                session: session.clone(),
            },
        );
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Up).await;
        assert!(
            !h.fake.calls().iter().any(|call| call.starts_with("touch ")),
            "{layout:?}"
        );
        h.app.list.select(Selection::Session(id("A")));
        h.app.show_selected();
        h.app.process_pending();
        h.press(KeyCode::Enter).await;
        assert!(
            h.fake.calls().contains(&"touch A".to_string()),
            "{layout:?}"
        );
    }
}

#[tokio::test]
async fn unread_completion_is_acknowledged_only_in_the_foreground_open_conversation() {
    let mut h = harness(vec![view("A", "agent", 10)]);
    h.press(KeyCode::Enter).await;
    let touches = |h: &Harness| {
        h.fake
            .calls()
            .iter()
            .filter(|call| *call == "touch A")
            .count()
    };
    let before = touches(&h);
    h.app.handle_terminal_event(Event::FocusLost).await;
    let mut session = view("A", "agent", 10);
    session.lifecycle = Lifecycle::Done;
    h.event(
        "A",
        SessionEvent::Updated {
            session: session.clone(),
        },
    );
    assert_eq!(
        touches(&h),
        before,
        "a background controller has not read the reply"
    );
    h.app.handle_terminal_event(Event::FocusGained).await;
    h.app.process_pending();
    assert_eq!(touches(&h), before + 1);
    session.lifecycle = Lifecycle::Working;
    h.event(
        "A",
        SessionEvent::Updated {
            session: session.clone(),
        },
    );
    let before = touches(&h);
    session.lifecycle = Lifecycle::Done;
    h.event("A", SessionEvent::Updated { session });
    assert_eq!(
        touches(&h),
        before + 1,
        "the user is already watching this reply"
    );
}

#[tokio::test]
async fn quiet_working_agents_keep_the_ui_animating_until_they_need_input() {
    let mut session = view("A", "agent", 1);
    session.lifecycle = Lifecycle::Working;
    let mut harness = harness(vec![session.clone()]);
    assert!(harness.app.is_animating());
    session.lifecycle = Lifecycle::Blocked;
    harness.event("A", SessionEvent::Updated { session });
    assert!(!harness.app.is_animating());
}

#[tokio::test]
async fn startup_selects_the_most_recent_session_and_observes_it() {
    let h = harness(vec![view("A", "older", 10), view("B", "newer", 30)]);
    assert!(h.app.connection.is_connected());
    assert_eq!(h.selected().as_deref(), Some("B"));
    // The launch folder is registered, then every folder is watched and
    // listed.
    let calls = h.fake.calls();
    assert_eq!(
        calls[..4],
        [
            "add_project /repo",
            "projects",
            "subscribe all",
            "list_all dead=true"
        ],
        "{calls:?}"
    );
    let (_, request) = h.last_attach();
    assert_eq!(request.id, id("B"));
    assert_eq!(request.role, Role::Observer);
    assert_eq!((request.cols, request.rows), h.app.terminal_size());
    assert_eq!(h.app.detail.view, DetailView::Live(id("B")));
    assert_eq!(h.app.focus, Focus::List);
}

#[tokio::test]
async fn empty_scope_shows_the_new_session_row() {
    let h = harness(vec![]);
    assert_eq!(h.app.list.selection(), &session_list::Selection::NewSession);
    assert_eq!(h.app.detail.view, DetailView::Empty);
    assert_eq!(h.fake.attach_count(), 0);
}

#[tokio::test]
async fn failed_connect_shows_the_error_and_ctrl_r_retries() {
    let fake = FakeApi::new(vec![view("A", "a", 1)]);
    let dir = tempfile::tempdir().unwrap();
    let mut options = options(&fake, scope(), &dir);
    let attempts = Arc::new(AtomicUsize::new(0));
    let api: Arc<dyn SessionApi> = fake.clone();
    let counter = attempts.clone();
    options.connector = Arc::new(move || {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(ApiError::new(ErrorCode::DaemonUnavailable, "no daemon"))
        } else {
            Ok(api.clone())
        }
    });
    let mut h = start(options, fake, dir);
    assert!(matches!(h.app.connection, Connection::Failed(_)));
    assert!(matches!(h.app.mode, AppMode::Normal));
    h.ctrl('r').await;
    assert!(h.app.connection.is_connected());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(h.selected().as_deref(), Some("A"));
}

#[tokio::test]
async fn protocol_mismatch_asks_before_restarting_the_daemon() {
    let fake = FakeApi::new(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let mut options = options(&fake, scope(), &dir);
    let attempts = Arc::new(AtomicUsize::new(0));
    let api: Arc<dyn SessionApi> = fake.clone();
    let counter = attempts.clone();
    options.connector = Arc::new(move || {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(ApiError::new(
                ErrorCode::ProtocolMismatch,
                "daemon speaks v0",
            ))
        } else {
            Ok(api.clone())
        }
    });
    let mut h = start(options, fake, dir);
    assert!(matches!(h.app.mode, AppMode::DaemonRestart(_)));
    // Declining keeps the error state.
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(matches!(h.app.connection, Connection::Failed(_)));
    // Retrying offers the restart again; consenting stops the (absent)
    // daemon and reconnects.
    h.app.connection = Connection::Failed(ApiError::new(ErrorCode::ProtocolMismatch, "v0"));
    h.app.mode = AppMode::DaemonRestart(DaemonRestartDialog {
        daemon_version: None,
        in_place: false,
        detail: String::new(),
        missing: Vec::new(),
        restarting: false,
        error: None,
        requested: false,
    });
    h.press(KeyCode::Char('y')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.connection.is_connected());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(h
        .app
        .status()
        .unwrap()
        .starts_with("Restarted the Rakumux server"));
}

// ---------------------------------------------------------------- live list

#[tokio::test]
async fn events_from_other_clients_keep_the_list_live() {
    let mut h = harness(vec![view("A", "a", 10), view("B", "b", 20)]).on_folders_tab();
    h.event(
        "C",
        SessionEvent::Created {
            session: view("C", "from-cli", 5),
        },
    );
    assert!(h.app.list.get(&id("C")).is_some());
    let mut renamed = view("A", "renamed-elsewhere", 10);
    renamed.lifecycle = Lifecycle::Working;
    h.event("A", SessionEvent::Updated { session: renamed });
    assert_eq!(h.app.list.get(&id("A")).unwrap().name, "renamed-elsewhere");

    // The selected session (B) is deleted elsewhere: the next one is shown.
    let attaches = h.fake.attach_count();
    h.event("B", SessionEvent::Removed);
    assert!(h.app.list.get(&id("B")).is_none());
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(h.fake.attach_count(), attaches + 1);
    assert_eq!(h.app.detail.view, DetailView::Live(id("A")));
}

#[tokio::test]
async fn lost_subscription_reconnects_and_resyncs() {
    let mut h = harness(vec![view("A", "a", 10)]);
    h.fake.clear_calls();
    h.fake.drop_subscribers();
    h.app.process_pending();
    assert!(matches!(h.app.connection, Connection::Failed(_)));
    let later = std::time::Instant::now() + std::time::Duration::from_secs(30);
    h.app.on_tick(later);
    h.app.process_pending();
    assert!(h.app.connection.is_connected());
    assert!(h.fake.calls().contains(&"list_all dead=true".to_string()));
}

#[tokio::test]
async fn reconnecting_keeps_a_focused_session_in_control() {
    let mut h = harness(vec![view("A", "a", 10)]);
    h.press(KeyCode::Enter).await;
    let before = h.fake.attach_count();
    h.fake.drop_subscribers();
    h.app.process_pending();
    let later = std::time::Instant::now() + std::time::Duration::from_secs(30);
    h.app.on_tick(later);
    h.app.process_pending();
    assert_eq!(h.fake.attach_count(), before + 1);
    assert_eq!(h.last_attach().1.role, Role::Controller);
    assert_eq!(h.app.focus, Focus::Session);
}

#[tokio::test]
async fn a_restart_goes_back_into_the_session_being_typed_in() {
    let mut h = harness(vec![view("A", "a", 10), view("B", "b", 20)]);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    let shown = h.app.detail.session_id().cloned().unwrap();
    // The server restarts: the attach stream ends, then the subscription.
    let (index, _) = h.last_attach();
    h.attach_event(index, AttachEvent::Closed(None));
    assert_eq!(h.app.focus, Focus::List);
    h.fake.drop_subscribers();
    h.app.process_pending();
    let soon = std::time::Instant::now() + std::time::Duration::from_secs(1);
    h.app.on_tick(soon);
    h.app.process_pending();
    assert!(h.app.connection.is_connected());
    assert_eq!(h.last_attach().1.role, Role::Controller);
    assert_eq!(h.last_attach().1.id, shown);
    assert_eq!(h.app.focus, Focus::Session);
}

#[tokio::test]
async fn an_older_server_that_restarts_in_place_is_restarted_with_enter() {
    let fake = FakeApi::without_all(vec![view("A", "a", 10)]);
    fake.state.lock().unwrap().restarts_in_place = true;
    let dir = tempfile::tempdir().unwrap();
    let options = options(&fake, scope(), &dir);
    let mut h = start(options, fake, dir);
    match &h.app.mode {
        AppMode::DaemonRestart(dialog) => assert!(dialog.in_place),
        mode => panic!("no restart offer: {mode:?}"),
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("keep running"), "{text}");
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    assert!(
        h.fake
            .calls()
            .iter()
            .any(|c| c.starts_with("restart_daemon")),
        "{:?}",
        h.fake.calls()
    );
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.connection.is_connected());
    assert_eq!(
        h.app.status(),
        Some("Restarted the Rakumux server; your sessions kept running.")
    );
}

#[tokio::test]
async fn s_and_settings_restart_the_server_in_place_after_asking() {
    let fake = FakeApi::new(vec![view("A", "a", 10)]);
    fake.state.lock().unwrap().restarts_in_place = true;
    let dir = tempfile::tempdir().unwrap();
    let options = options(&fake, scope(), &dir);
    let mut h = start(options, fake, dir);
    assert!(matches!(h.app.mode, AppMode::Normal));
    // `S` asks first; `n` (or `q`, which doesn't quit here) keeps it.
    h.press(KeyCode::Char('S')).await;
    match &h.app.mode {
        AppMode::DaemonRestart(dialog) => assert!(dialog.requested && dialog.in_place),
        mode => panic!("no restart dialog: {mode:?}"),
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("Restart the Rakumux server"), "{text}");
    assert!(text.contains("keep running"), "{text}");
    assert!(!text.contains("[q] Quit"), "{text}");
    h.press(KeyCode::Char('n')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    h.press(KeyCode::Char('S')).await;
    assert_eq!(h.press(KeyCode::Char('q')).await, AppResult::Continue);
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(!h
        .fake
        .calls()
        .iter()
        .any(|c| c.starts_with("restart_daemon")));
    // Enter restarts it in place; the sessions keep running.
    h.press(KeyCode::Char('S')).await;
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    assert!(
        h.fake
            .calls()
            .iter()
            .any(|c| c.starts_with("restart_daemon")),
        "{:?}",
        h.fake.calls()
    );
    assert!(h.app.connection.is_connected());
    assert_eq!(
        h.app.status(),
        Some("Restarted the Rakumux server; your sessions kept running.")
    );
    // Settings has both restarts, the server's with its version.
    h.app.server_version = Some("0.1.8-dev".into());
    let rows = h.app.modal_context(false).general;
    let row = |label: &str| rows.iter().find(|r| r.label == label).cloned().unwrap();
    assert_eq!(row("Restart UI").value, "R");
    assert_eq!(
        row("Restart server").value,
        format!("0.1.8-dev → {} · S", app_version())
    );
    h.app.mode = settings_on_actions(false);
    let AppMode::Modal { state, .. } = &mut h.app.mode else {
        unreachable!();
    };
    state.selected = SettingsItem::RestartServer.index();
    h.press(KeyCode::Enter).await;
    assert!(matches!(&h.app.mode, AppMode::DaemonRestart(d) if d.requested));
}

#[tokio::test]
async fn a_server_that_cant_restart_in_place_needs_y_to_restart() {
    let mut h = harness(vec![view("A", "a", 10)]);
    h.press(KeyCode::Char('S')).await;
    assert!(matches!(&h.app.mode, AppMode::DaemonRestart(d) if !d.in_place));
    // Enter alone never ends the running sessions.
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::DaemonRestart(_)));
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.status(), Some("The server keeps running as it is."));
}

#[test]
fn a_panicking_client_ends_in_a_visible_error() {
    let fake = FakeApi::new(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let mut options = options(&fake, scope(), &dir);
    options.connector = Arc::new(|| panic!("client bug"));
    let h = start(options, fake, dir);
    match &h.app.connection {
        Connection::Failed(error) => assert_eq!(error.code, ErrorCode::Internal),
        _ => panic!("expected a failed connection"),
    }
}

// ---------------------------------------------------------------- focus and keys

#[tokio::test]
async fn an_observer_does_not_fit_back_after_another_window_resizes() {
    let mut h = harness(vec![view("A", "a", 10)]);
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    // Another window fitted the uncontrolled session to its own pane.
    h.attach_event(
        index,
        AttachEvent::Event(StreamEvent::Resized {
            cols: 71,
            rows: 27,
            checkpoint: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"x"),
            offset: 0,
        }),
    );
    // Session updates used to make this window fit it back, and the two
    // windows resized it back and forth.
    h.event(
        "A",
        SessionEvent::Updated {
            session: view("A", "a", 20),
        },
    );
    assert_eq!(h.fake.outgoing(index), vec![]);
    // Our own pane changing still fits it.
    h.app.resize(150, 45);
    let (cols, rows) = h.app.terminal_size();
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Command(StreamCommand::Fit { cols, rows })]
    );
}

#[tokio::test]
async fn the_window_being_used_takes_the_session_size_back() {
    use crossterm::event::{Event, MouseEvent, MouseEventKind};
    let mut h = harness(vec![view("A", "a", 10)]);
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    // The phone fitted the session to itself.
    h.attach_event(
        index,
        AttachEvent::Event(StreamEvent::Resized {
            cols: 45,
            rows: 38,
            checkpoint: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"x"),
            offset: 0,
        }),
    );
    // A mouse merely passing over this window doesn't take it back…
    let moved = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    h.app.handle_terminal_event(moved).await;
    assert_eq!(h.fake.outgoing(index), vec![]);
    // …but the window coming to the front does, once.
    let (cols, rows) = h.app.terminal_size();
    h.app.handle_terminal_event(Event::FocusGained).await;
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Command(StreamCommand::Fit { cols, rows })]
    );
    h.app.handle_terminal_event(Event::FocusGained).await;
    assert_eq!(h.fake.outgoing(index), vec![]);
}

#[tokio::test]
async fn keys_with_a_stray_ctrl_or_alt_act_as_the_plain_key_in_rakumux_mode() {
    let mut h = harness(vec![view("A", "a", 10), view("B", "b", 20)]);
    let selected = h.selected();
    // Ctrl-↓ (a phone key bar's ctrl, a held modifier): moves like ↓.
    h.key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL))
        .await;
    assert_ne!(h.selected(), selected);
    let moved = h.selected();
    h.key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL))
        .await;
    assert_eq!(h.selected(), selected);
    h.key(KeyEvent::new(
        KeyCode::Down,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .await;
    assert_eq!(h.selected(), moved);
    // Shortcuts keep their meaning: Ctrl-c quits, Alt-↑ cycles.
    assert!(matches!(
        h.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT))
            .await,
        AppResult::Exit
    ));
    assert!(matches!(
        h.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL))
            .await,
        AppResult::Exit
    ));
    assert!(matches!(
        h.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .await,
        AppResult::Exit
    ));
}

#[tokio::test]
async fn enter_promotes_and_the_rakumux_key_demotes() {
    let mut h = harness(vec![view("A", "a", 10)]);
    let (index, _) = h.last_attach();
    let (cols, rows) = h.app.terminal_size();
    // Observing a session nobody controls fits it to this pane.
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Command(StreamCommand::Fit { cols, rows })]
    );
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Command(StreamCommand::Promote {
            cols,
            rows
        })]
    );
    assert!(h.fake.calls().contains(&"touch A".to_string()));

    // Plain keys, even Rakumux mode keys, go to the session.
    h.press(KeyCode::Char('l')).await;
    h.press(KeyCode::Char('q')).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(
        h.fake.outgoing(index),
        vec![
            AttachOutgoing::Input(b"l".to_vec()),
            AttachOutgoing::Input(b"q".to_vec()),
            AttachOutgoing::Input(b"\r".to_vec()),
        ]
    );
    assert_eq!(h.app.focus, Focus::Session);

    h.rakumux().await;
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Command(StreamCommand::Demote)]
    );
}

#[tokio::test]
async fn esc_and_the_rakumux_key_go_back_to_the_session_you_came_from() {
    let mut h = harness(vec![aged("A", "a", 1, 30), aged("B", "b", 2, 20)]);
    let (index, _) = h.last_attach();
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    h.fake.outgoing(index);
    // Inside the session Tab and Esc belong to the program.
    h.press(KeyCode::Tab).await;
    h.press(KeyCode::Esc).await;
    assert_eq!(
        h.fake.outgoing(index),
        vec![
            AttachOutgoing::Input(b"\t".to_vec()),
            AttachOutgoing::Input(b"\x1b".to_vec())
        ]
    );
    // Rakumux mode, look at B, then Esc: back in A, not B.
    h.rakumux().await;
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(h.app.came_from, Some(id("A")));
    h.press(KeyCode::Char('j')).await;
    assert_eq!(h.selected().as_deref(), Some("B"));
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(h.app.came_from, None);
    // The Rakumux key goes back too, even with a filter typed.
    // A filter hiding it is cleared on the way.
    h.rakumux().await;
    h.press(KeyCode::Char('/')).await;
    h.type_text("b").await;
    h.press(KeyCode::Enter).await;
    h.rakumux().await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert!(h.app.list.filter.is_empty());
    // Esc cancels first: a filter is cleared, then Esc goes back.
    h.rakumux().await;
    h.press(KeyCode::Char('/')).await;
    h.type_text("b").await;
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.list.filter.is_empty());
    // ... and a pending `g`.
    h.press(KeyCode::Char('g')).await;
    assert_eq!(h.app.pending_key, Some('g'));
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.pending_key, None);
    assert_eq!(h.app.focus, Focus::List);
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Session);
    // Started in Rakumux mode, Esc goes into the session in view.
    let mut fresh = harness(vec![view("A", "a", 30)]);
    fresh.press(KeyCode::Esc).await;
    assert_eq!(fresh.app.focus, Focus::Session);
}

#[tokio::test]
async fn tab_numbers_and_enter_go_into_sessions() {
    let mut h = harness(vec![
        aged("A", "a", 1, 30),
        aged("B", "b", 2, 20),
        aged("C", "c", 3, 10),
    ]);
    h.press(KeyCode::Tab).await;
    assert!(h.app.status().unwrap().contains("No other session"));
    h.press(KeyCode::Char('i')).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.last_attach().1.id, id("A"));
    // 3 goes straight into the third session.
    h.prefix('3').await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.last_attach().1.id, id("C"));
    assert_eq!(h.last_attach().1.role, Role::Controller);
    // Tab: the session before that.
    h.prefix_tab().await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.selected().as_deref(), Some("A"));
    h.prefix_tab().await;
    assert_eq!(h.selected().as_deref(), Some("C"));
}

#[tokio::test]
async fn rakumux_mode_shows_the_mode_the_selected_session_help_and_settings_in_the_footer() {
    let mut h = harness(vec![view("A", "a", 30)]);
    h.press(KeyCode::Enter).await;
    h.rakumux().await;
    h.app
        .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(5));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let buffer = terminal.backend().buffer();
    let footer: String = (0..120)
        .map(|x| buffer.get(x, 39).symbol().to_string())
        .collect();
    assert!(footer.starts_with(" RAKUMUX  "), "the mode: {footer:?}");
    // A in the middle, then the buttons.
    let at = footer.find("session: ● a ").expect("the session");
    let label = crate::layout::str_width("session: ● a");
    assert!(
        crate::layout::str_width(&footer[..at]).abs_diff((120 - label) / 2) <= 1,
        "{footer:?}"
    );
    assert!(footer.ends_with(" ? Help   , Settings  "), "{footer:?}");
    let screen: String = buffer.content().iter().map(|c| c.symbol()).collect();
    for hint in ["go in", "Esc back", "last session"] {
        assert!(!screen.contains(hint), "{hint}");
    }
}

#[tokio::test]
async fn option_arrows_in_sessions_is_a_general_setting() {
    let mut h = harness(vec![view("A", "a", 30)]);
    assert_eq!(h.app.modal_context(false).general[2].value, "On [Off]");
    h.press(KeyCode::Char(',')).await;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Enter).await;
    assert!(h.app.settings.sessions.cycle_keys_in_session);
    assert_eq!(h.app.modal_context(false).general[2].value, "[On] Off");
    let saved = Settings::load_from(&h.home.path().join("config.json")).unwrap();
    assert!(saved.sessions.cycle_keys_in_session);
}

#[tokio::test]
async fn vim_keys_move_and_gg_goes_to_the_top() {
    let mut h = harness(vec![
        aged("A", "a", 1, 30),
        aged("B", "b", 2, 20),
        aged("C", "c", 3, 10),
    ])
    .on_folders_tab();
    h.press(KeyCode::Char('G')).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::NewSession);
    h.press(KeyCode::Char('g')).await;
    h.press(KeyCode::Char('g')).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::Folders);
    // Half a page is more than these three sessions.
    h.ctrl('d').await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::NewSession);
    h.ctrl('u').await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::Folders);
    // Past the folder's header to the second session.
    h.press(KeyCode::Char('j')).await;
    h.press(KeyCode::Char('j')).await;
    h.press(KeyCode::Char('j')).await;
    assert_eq!(h.selected().as_deref(), Some("B"));
    // One g waits for the second; another key drops it.
    h.press(KeyCode::Char('g')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    h.press(KeyCode::Char('k')).await;
    assert_eq!(h.app.pending_key, None);
    assert_eq!(h.selected().as_deref(), Some("A"));
}

#[tokio::test]
async fn dragging_the_divider_resizes_the_list_and_saves_the_width() {
    let mut h = harness(vec![view("A", "a", 10)]);
    h.app.resize(160, 48);
    let divider = h.app.screen().divider.unwrap();
    let mouse = |kind, column| MouseEvent {
        kind,
        column,
        row: divider.y + 3,
        modifiers: KeyModifiers::NONE,
    };
    h.app.handle_mouse(mouse(MouseEventKind::Moved, divider.x));
    assert!(h.app.divider.hover);
    h.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider.x));
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 70));
    assert_eq!(h.app.screen().list.unwrap().width, 70);
    assert_eq!(h.app.screen().divider.unwrap().x, 70);
    h.app
        .handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 70));
    assert!(!h.app.divider.dragging);
    let saved = std::fs::read_to_string(h.home.path().join("config.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["sessions"]["sidebar_width"], 70);
    // A drag far past the edge leaves the detail its minimum.
    h.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 70));
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 159));
    assert_eq!(
        h.app.screen().detail.unwrap().width,
        crate::layout::DETAIL_MIN_COLS
    );
}

#[tokio::test]
async fn dragging_the_stacked_divider_resizes_the_list_and_saves_its_height() {
    let mut h = harness(vec![view("A", "a", 10)]);
    // A narrow window: the list above the session.
    h.app.resize(90, 40);
    let screen = h.app.screen();
    assert!(screen.stacked);
    let divider = screen.divider.unwrap();
    assert_eq!((divider.y, divider.height, divider.width), (15, 1, 90));
    let size = h.app.terminal_size();
    let mouse = |kind, row| MouseEvent {
        kind,
        column: 20,
        row,
        modifiers: KeyModifiers::NONE,
    };
    h.app.handle_mouse(mouse(MouseEventKind::Moved, divider.y));
    assert!(h.app.divider.hover);
    h.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider.y));
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 25));
    let screen = h.app.screen();
    assert_eq!(screen.list.unwrap().height, 25);
    assert_eq!(screen.divider.unwrap().y, 25);
    assert_eq!(screen.detail.unwrap().y, 26);
    // The session-mode pane, and so the terminal's size, stays.
    assert_eq!(h.app.terminal_size(), size);
    h.app
        .handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25));
    assert!(!h.app.divider.dragging);
    let saved = std::fs::read_to_string(h.home.path().join("config.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["sessions"]["stacked_list_percent"], 64);
    assert!(saved["sessions"].get("sidebar_width").is_none(), "{saved}");
    // Past either end, both panes keep their minimum.
    for (row, list, preview) in [(38, 34, 4), (1, 10, 28)] {
        let divider = h.app.screen().divider.unwrap().y;
        h.app
            .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider));
        h.app
            .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), row));
        h.app
            .handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), row));
        let screen = h.app.screen();
        assert_eq!(screen.list.unwrap().height, list, "row {row}");
        assert_eq!(screen.detail.unwrap().height, preview, "row {row}");
    }
    // The setting comes back on the next start.
    let saved = Settings::load_from(&h.home.path().join("config.json")).unwrap();
    // Dragged past the top: the share of the smallest list.
    assert_eq!(saved.sessions.stacked_list_percent, Some(26));
    // Wide screens ignore it.
    h.app.resize(160, 48);
    assert_eq!(h.app.screen().list.unwrap().height, 47);
}

#[tokio::test]
async fn from_a_session_the_rakumux_key_then_one_key_does_everything() {
    let mut h = harness(vec![view("A", "a", 10)]);
    let (index, _) = h.last_attach();
    h.press(KeyCode::Enter).await;
    h.fake.outgoing(index);

    // Ctrl-b, the old prefix, is the program's now.
    h.ctrl('b').await;
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Input(vec![0x02])]
    );
    // Kitty-protocol terminals report Ctrl-\ as itself.
    h.ctrl('\\').await;
    assert_eq!(h.app.focus, Focus::List);
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Session);
    h.fake.outgoing(index);

    // `\` sends the Rakumux key itself and goes back in.
    h.prefix('\\').await;
    assert_eq!(h.app.focus, Focus::Session);
    assert!(h
        .fake
        .outgoing(index)
        .contains(&AttachOutgoing::Input(vec![0x1c])));

    // z: work in the session full screen; the Rakumux key brings the
    // list back.
    h.prefix('z').await;
    assert!(h.app.zoomed);
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.app.terminal_size(), (160, 47));
    h.rakumux().await;
    assert!(!h.app.zoomed);
    assert_eq!(h.app.focus, Focus::List);
    h.press(KeyCode::Esc).await;

    // Help and Settings.
    h.prefix('?').await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Overview));
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.focus, Focus::List);
    h.press(KeyCode::Char(',')).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Settings));
    h.press(KeyCode::Char(',')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Session);

    // Rename still uses a dialog.
    h.prefix('r').await;
    assert!(matches!(h.app.mode, AppMode::Renaming(_)));
    h.press(KeyCode::Esc).await;
    // Close asks first, then kills.
    h.press(KeyCode::Char('x')).await;
    match &h.app.mode {
        AppMode::Confirm(dialog) => assert_eq!(
            dialog.action,
            ConfirmAction::CloseSession {
                id: id("A"),
                name: "a".into()
            }
        ),
        _ => panic!("expected a confirmation"),
    }
    h.press(KeyCode::Char('y')).await;
    assert!(h.fake.calls().contains(&"kill A tree=false".to_string()));
    // Nothing but the literal key reached the program.
    assert!(h
        .fake
        .outgoing(index)
        .iter()
        .all(|m| !matches!(m, AttachOutgoing::Input(_))));
}

#[tokio::test]
async fn cycle_keys_move_in_rakumux_mode_and_only_on_request_in_sessions() {
    let mut h = harness(vec![
        aged("A", "a", 1, 30),
        aged("B", "b", 2, 20),
        aged("C", "c", 3, 10),
    ]);
    assert_eq!(h.selected().as_deref(), Some("A"));
    h.alt(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("B"));
    assert_eq!(h.last_attach().1.role, Role::Observer);
    h.alt(KeyCode::Up).await;
    h.alt(KeyCode::Up).await;
    assert_eq!(h.selected().as_deref(), Some("C"), "wraps around");
    h.alt(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("A"));

    // In a session they go to the program by default (OpenCode, Pi and
    // Claude Code use them).
    h.press(KeyCode::Enter).await;
    let (a_index, _) = h.last_attach();
    h.fake.outgoing(a_index);
    h.alt(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(
        h.fake.outgoing(a_index),
        vec![AttachOutgoing::Input(b"\x1b[1;3B".to_vec())]
    );

    // Turned on, they switch sessions from inside one, and never reach the
    // program.
    h.app.settings.sessions.cycle_keys_in_session = true;
    h.alt(KeyCode::Down).await;
    assert!(h
        .fake
        .outgoing(a_index)
        .iter()
        .all(|m| !matches!(m, AttachOutgoing::Input(_))));
    let (_, request) = h.last_attach();
    assert_eq!((request.id.as_str(), request.role), ("B", Role::Controller));
    assert_eq!(h.app.focus, Focus::Session);
    // Going in touches B, which moves nothing: the next step goes on to C,
    // and the order is the same back in Rakumux mode.
    h.event(
        "B",
        SessionEvent::Updated {
            session: aged("B", "b", 2, 99),
        },
    );
    h.alt(KeyCode::Down).await;
    assert_eq!(h.last_attach().1.id, id("C"));
    h.rakumux().await;
    assert_eq!(visible_ids(&h), ["A", "B", "C"]);
}

#[tokio::test]
async fn cycle_keys_can_be_turned_off() {
    let fake = FakeApi::new(vec![view("A", "a", 30), view("B", "b", 20)]);
    let dir = tempfile::tempdir().unwrap();
    let mut options = options(&fake, scope(), &dir);
    options.settings.sessions.cycle_keys.previous.clear();
    options.settings.sessions.cycle_keys.next.clear();
    let mut h = start(options, fake, dir);
    assert!(h.app.cycle_keys.is_none());
    assert_eq!(h.app.modal_context(false).cycle_keys, "");
    // Inside a session the key now reaches the program.
    h.press(KeyCode::Enter).await;
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    h.alt(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Input(b"\x1b[1;3B".to_vec())]
    );
}

#[tokio::test]
async fn nesting_guard_never_attaches_the_session_we_run_in() {
    let fake = FakeApi::new(vec![aged("A", "self", 1, 30), aged("B", "other", 2, 20)]);
    let dir = tempfile::tempdir().unwrap();
    let mut options = options(&fake, scope(), &dir);
    options.self_session = Some(id("A"));
    let mut h = start(options, fake, dir);
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(h.app.detail.view, DetailView::SelfSession(id("A")));
    assert_eq!(h.fake.attach_count(), 0);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(h.fake.attach_count(), 0);
    assert!(h.app.status().unwrap().contains("runs in"));
    // Cycling from inside another session skips into Rakumux mode, not
    // into the guarded session.
    h.app.settings.sessions.cycle_keys_in_session = true;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    h.alt(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.fake.calls().iter().all(|c| !c.starts_with("attach A")));
}

#[tokio::test]
async fn another_client_taking_control_returns_focus_to_the_list() {
    let mut h = harness(vec![view("A", "a", 10)]);
    let (index, _) = h.last_attach();
    h.press(KeyCode::Enter).await;
    h.attach_event(
        index,
        AttachEvent::Event(StreamEvent::RoleChanged {
            role: Role::Observer,
        }),
    );
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.status().unwrap().contains("took control"));
    assert_eq!(
        h.app.detail.attachment.as_ref().unwrap().role,
        Role::Observer
    );
}

#[tokio::test]
async fn going_straight_back_in_isnt_mistaken_for_another_client() {
    let mut h = harness(vec![view("A", "a", 10)]);
    let (index, _) = h.last_attach();
    // Promote, Demote, Promote before the daemon's answers arrive.
    h.press(KeyCode::Enter).await;
    h.rakumux().await;
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Session);
    for role in [Role::Controller, Role::Observer, Role::Controller] {
        h.attach_event(index, AttachEvent::Event(StreamEvent::RoleChanged { role }));
    }
    assert_eq!(h.app.focus, Focus::Session);
    assert!(h.app.status().is_none(), "{:?}", h.app.status());
    assert_eq!(
        h.app.detail.attachment.as_ref().unwrap().role,
        Role::Controller
    );
    // A change nobody here asked for is still another client's.
    h.attach_event(
        index,
        AttachEvent::Event(StreamEvent::RoleChanged {
            role: Role::Observer,
        }),
    );
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.status().unwrap().contains("took control"));
}

#[tokio::test]
async fn output_is_fed_in_order_and_gaps_reattach() {
    let mut h = harness(vec![view("A", "a", 10)]);
    let (index, _) = h.last_attach();
    // The fake's checkpoint ends at offset 100.
    h.attach_event(
        index,
        AttachEvent::Output {
            offset: 100,
            bytes: b" more".to_vec(),
        },
    );
    h.attach_event(
        index,
        AttachEvent::Output {
            offset: 98,
            bytes: b"re".to_vec(),
        },
    );
    let attachment = h.app.detail.attachment.as_ref().unwrap();
    assert_eq!(attachment.offset, 105);
    let attaches = h.fake.attach_count();
    h.attach_event(
        index,
        AttachEvent::Output {
            offset: 200,
            bytes: b"x".to_vec(),
        },
    );
    assert_eq!(h.fake.attach_count(), attaches + 1, "a gap re-attaches");
}

/// The style of the first cell of `text` on the screen drawn at 120×40.
fn cell_of(h: &mut Harness, text: &str) -> ratatui::buffer::Cell {
    cell_within(h, text, 120)
}

/// The style of the first cell of `text` within the first `columns`
/// columns of the screen drawn at 120×40 (e.g. only the session list).
fn cell_within(h: &mut Harness, text: &str, columns: u16) -> ratatui::buffer::Cell {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    for y in 0..40 {
        let row: String = (0..columns)
            .map(|x| buffer.get(x, y).symbol().to_string())
            .collect();
        if let Some(x) = row.find(text) {
            let x = row[..x].chars().count() as u16;
            return buffer.get(x, y).clone();
        }
    }
    panic!("{text:?} isn't on the screen");
}

#[tokio::test]
async fn the_session_list_is_never_faded() {
    // Switching input focus never changes the session list's styling.
    let mut h = harness(vec![view("A", "zebra", 10)]);
    let width = h.app.screen().list.unwrap().width;
    let rakumux = cell_within(&mut h, "zebra", width);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(cell_within(&mut h, "zebra", width), rakumux);
}

#[tokio::test]
async fn live_previews_recede_in_rakumux_mode_and_more_under_dialogs() {
    use ratatui::style::{Color, Modifier};

    for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
        let mut h = harness(vec![view("A", "a", 10), view("B", "b", 20)]);
        h.app.set_layout(layout);
        let output = b"\r\n\x1b[1;38;2;200;120;80;48;2;40;50;60mpreview-color\x1b[0m";
        let (index, _) = h.last_attach();
        h.attach_event(
            index,
            AttachEvent::Output {
                offset: 100,
                bytes: output.to_vec(),
            },
        );
        let p = crate::theme::palette();
        // Browsing visibly fades the preview without losing boldness or colours.
        assert_eq!(h.app.focus, Focus::List);
        let preview = cell_of(&mut h, "preview-color");
        match p.faded(
            Color::Rgb(200, 120, 80),
            crate::components::utils::PREVIEW_FADE,
        ) {
            Some(fg) => {
                assert_eq!(preview.fg, fg);
                assert!(!preview.modifier.contains(Modifier::DIM));
            }
            None => assert!(preview.modifier.contains(Modifier::DIM)),
        }
        assert_eq!(
            preview.bg,
            p.faded(
                Color::Rgb(40, 50, 60),
                crate::components::utils::PREVIEW_FADE
            )
            .unwrap_or(Color::Rgb(40, 50, 60))
        );
        assert!(preview.modifier.contains(Modifier::BOLD));
        // Enter restores the program's exact colours; leaving fades only once.
        h.press(KeyCode::Enter).await;
        assert_eq!(h.app.focus, Focus::Session);
        let active = cell_of(&mut h, "preview-color");
        assert_eq!(active.fg, Color::Rgb(200, 120, 80));
        assert_eq!(active.bg, Color::Rgb(40, 50, 60));
        assert!(active.modifier.contains(Modifier::BOLD));
        assert!(!active.modifier.contains(Modifier::DIM));
        h.rakumux().await;
        assert_eq!(cell_of(&mut h, "preview-color"), preview);
        // Dialogs still fade the terminal behind them.
        h.press(KeyCode::Char('?')).await;
        let obscured = cell_of(&mut h, "preview-color");
        match p.faded(active.fg, crate::components::utils::FADE_TEXT) {
            Some(faded) => {
                assert_eq!(obscured.fg, faded);
                assert_ne!(obscured.fg, preview.fg);
            }
            None => assert!(obscured.modifier.contains(Modifier::DIM)),
        }
        h.press(KeyCode::Esc).await;
        assert_eq!(cell_of(&mut h, "preview-color"), preview);
        // Selecting another session applies the same fade to its output.
        assert_eq!(h.selected().as_deref(), Some("B"));
        h.press(KeyCode::Up).await;
        assert_eq!(h.selected().as_deref(), Some("A"));
        let (index, _) = h.last_attach();
        h.attach_event(
            index,
            AttachEvent::Output {
                offset: 100,
                bytes: output.to_vec(),
            },
        );
        assert_eq!(cell_of(&mut h, "preview-color"), preview);
    }
}

#[tokio::test]
async fn the_folder_filter_and_new_session_keep_the_session_in_view() {
    let mut h = harness(vec![view("A", "a", 10), view("B", "b", 20)]);
    let shown = h.app.detail.view.session_id().cloned();
    assert!(shown.is_some());
    let attaches = h.fake.attach_count();
    // + New session at the bottom, then the folder filter at the top.
    h.press(KeyCode::Char('G')).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::NewSession);
    assert_eq!(h.app.detail.view.session_id(), shown.as_ref());
    h.press(KeyCode::Char('g')).await;
    h.press(KeyCode::Char('g')).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::Folders);
    // The pane went A → (last session) → … but never empty, and moving
    // between rows that aren't sessions doesn't attach again.
    assert!(h.app.detail.view.session_id().is_some());
    let before = h.fake.attach_count();
    h.press(KeyCode::Char('G')).await;
    h.press(KeyCode::Char('g')).await;
    h.press(KeyCode::Char('g')).await;
    assert_eq!(h.fake.attach_count(), before);
    assert!(h.fake.attach_count() >= attaches);
}

// ---------------------------------------------------------------- exited and dead

#[tokio::test]
async fn esc_with_no_session_to_go_back_to_goes_into_the_one_in_view() {
    // A fresh start (or a reloaded UI) is in Rakumux mode without a session
    // it came from: Esc still leaves Rakumux mode, into the session shown.
    let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]);
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.came_from.is_none());
    let shown = h.app.detail.session_id().cloned().unwrap();
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.app.detail.session_id(), Some(&shown));
    // With no running session in view it says so instead.
    h.rakumux().await;
    h.app.came_from = None;
    h.app.list.select(session_list::Selection::NewSession);
    h.app.reconcile_detail();
    h.app.detail.view = DetailView::Empty;
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.status().unwrap().contains("No session to go back to"));
}

#[tokio::test]
async fn exiting_the_session_you_work_in_deletes_it() {
    let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    let (index, request) = h.last_attach();
    assert_eq!(request.id.as_str(), "A");
    assert_eq!(
        h.app.detail.attachment.as_ref().unwrap().role,
        Role::Controller
    );
    h.attach_event(
        index,
        AttachEvent::Event(StreamEvent::Exited { code: Some(0) }),
    );
    assert!(h.fake.calls().iter().any(|c| c == "delete A"));
    assert!(h.app.list.get(&id("A")).is_none(), "gone from the list");
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(h.selected().as_deref(), Some("B"));
    assert_ne!(h.app.detail.session_id(), Some(&id("A")));
    assert_eq!(h.app.status(), Some("Deleted a"));
}

#[tokio::test]
async fn a_session_ending_in_the_background_stays_and_enter_restarts() {
    let mut h = harness(vec![view("A", "a", 10)]);
    // Previewed in Rakumux mode, not worked in.
    let (index, _) = h.last_attach();
    h.attach_event(
        index,
        AttachEvent::Event(StreamEvent::Exited { code: Some(3) }),
    );
    assert!(!h.fake.calls().iter().any(|c| c.starts_with("delete")));
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(
        h.app.detail.view,
        DetailView::Saved {
            id: id("A"),
            status: SessionStatus::Exited,
            exit_code: Some(3)
        }
    );
    assert!(h.app.detail.terminal.is_some(), "final screen kept");
    h.press(KeyCode::Enter).await;
    assert!(h
        .fake
        .calls()
        .iter()
        .any(|c| c.starts_with("revive A resume=false")));
    let (_, request) = h.last_attach();
    assert_eq!((request.id.as_str(), request.role), ("A", Role::Controller));
    assert_eq!(h.app.focus, Focus::Session);
}

#[tokio::test]
async fn dead_session_shows_saved_output_and_enter_revives() {
    let mut dead = view("D", "dead-one", 10);
    dead.status = SessionStatus::Dead;
    dead.terminal = None;
    let fake = FakeApi::new(vec![dead]);
    fake.set_read("D", "line one\nline two");
    let dir = tempfile::tempdir().unwrap();
    let options = options(&fake, scope(), &dir);
    let mut h = start(options, fake, dir);
    assert!(h
        .fake
        .calls()
        .iter()
        .any(|c| c.starts_with("read D Recent")));
    assert!(matches!(
        h.app.detail.view,
        DetailView::Saved {
            status: SessionStatus::Dead,
            ..
        }
    ));
    assert_eq!(h.fake.attach_count(), 0);
    h.press(KeyCode::Enter).await;
    assert!(h
        .fake
        .calls()
        .iter()
        .any(|c| c.starts_with("revive D resume=true")));
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.app.detail.view, DetailView::Live(id("D")));
}

// ---------------------------------------------------------------- dialogs

#[tokio::test]
async fn new_session_creates_immediately_and_focuses_the_shell_without_a_dialog() {
    let mut h = harness(vec![view("A", "a", 10)]);

    h.press(KeyCode::Char('n')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));

    // The folder and the login shell: no command, no startup input.
    let creates = h.fake.creates();
    assert_eq!(creates.len(), 1);
    let create = &creates[0];
    assert_eq!(create.scope, Some(scope()));
    assert_eq!(create.name, None);
    assert_eq!(create.cwd.as_deref(), Some(scope().path()));
    assert_eq!(create.argv, None);
    assert!(create.startup_cmds.is_empty());
    assert_eq!((create.cols, create.rows), h.app.terminal_size());

    let new_id = h.app.list.selected_id().cloned().unwrap();
    assert_eq!(new_id.as_str(), "N1");
    assert!(h.app.list.get(&new_id).unwrap().auto_named);
    let (_, request) = h.last_attach();
    assert_eq!(
        (request.id.clone(), request.role),
        (new_id, Role::Controller)
    );
    assert_eq!(h.app.focus, Focus::Session);
}

#[tokio::test]
async fn enter_on_new_session_generates_a_name_and_goes_straight_into_session_mode() {
    let mut h = harness(vec![]);
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    let create = h.fake.creates().pop().unwrap();
    assert_eq!(create.name, None);
    assert_eq!(create.cwd.as_deref(), Some(scope().path()));
    let new_id = h.app.list.selected_id().cloned().unwrap();
    assert!(h.app.list.get(&new_id).unwrap().auto_named);

    assert_eq!(h.app.focus, Focus::Session);
    // Names remain editable after creation with the existing rename action.
    h.rakumux().await;
    h.press(KeyCode::Char('r')).await;
    assert!(matches!(h.app.mode, AppMode::Renaming(_)));
    assert_eq!(h.fake.creates().len(), 1);
}

#[tokio::test]
async fn pending_session_creation_cannot_duplicate_creates_or_send_input_to_the_previous_session() {
    let mut h = harness(vec![view("A", "a", 1)]);
    h.press(KeyCode::Enter).await;
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    h.fake.clear_calls();
    h.app.creating_session = true;
    h.app.phone_offer_pending = true;
    h.app.focus = Focus::List;
    assert!(
        !h.app.offer_phone_access(),
        "onboarding must not interrupt immediate session creation"
    );
    h.app.focus = Focus::Session;
    for key in ['n', 'x', 'q'] {
        assert_eq!(h.press(KeyCode::Char(key)).await, AppResult::Continue);
    }
    h.app.handle_paste("must not reach the old shell");
    assert!(h.fake.creates().is_empty());
    assert!(h.fake.outgoing(index).is_empty());
    assert!(h.app.input_is_private());
    h.app.creating_session = false;
}

#[tokio::test]
async fn create_errors_are_shown_without_a_modal_and_allow_retry() {
    let dir = tempfile::tempdir().unwrap();
    let scope = ScopeRoot::resolve(dir.path()).unwrap();
    let fake = FakeApi::new(vec![]);
    fake.fail_next(
        "create",
        ApiError::new(ErrorCode::InvalidArgument, "name taken"),
    );
    let home = tempfile::tempdir().unwrap();
    let options = options(&fake, scope, &home);
    let mut h = start(options, fake, home);
    h.press(KeyCode::Enter).await; // "+ New session" row
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.status().unwrap().contains("name taken"));
    assert!(!h.app.creating_session);
    h.press(KeyCode::Char('n')).await;
    assert_eq!(h.app.focus, Focus::Session);
}

#[tokio::test]
async fn rename_dialog_renames_and_reports_errors() {
    let mut h = harness(vec![view("A", "old", 10)]);
    h.press(KeyCode::Char('r')).await;
    match &h.app.mode {
        AppMode::Renaming(dialog) => assert_eq!(dialog.input.value(), "old"),
        _ => panic!("no rename dialog"),
    }
    h.fake.fail_next(
        "rename",
        ApiError::new(ErrorCode::InvalidArgument, "name taken"),
    );
    h.ctrl('u').await;
    h.type_text("api work").await;
    h.press(KeyCode::Enter).await;
    match &h.app.mode {
        AppMode::Renaming(dialog) => {
            assert!(dialog
                .error_message
                .as_deref()
                .unwrap()
                .contains("name taken"))
        }
        _ => panic!("dialog closed on error"),
    }
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.fake.calls().contains(&"rename A api work".to_string()));
    assert_eq!(h.app.list.get(&id("A")).unwrap().name, "api work");
}

#[tokio::test]
async fn d_and_dd_delete_only_after_confirmation_in_both_layouts() {
    for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
        for keys in ["d", "dd"] {
            for confirm in [KeyCode::Enter, KeyCode::Char('y')] {
                let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]);
                h.app.settings.sessions.layout = layout;
                for cancel in [KeyCode::Char('n'), KeyCode::Esc] {
                    h.type_text(keys).await;
                    assert!(matches!(
                        &h.app.mode,
                        AppMode::Confirm(dialog)
                            if matches!(&dialog.action, ConfirmAction::DeleteSession { id: target, .. } if *target == id("A"))
                    ));
                    assert_eq!(h.app.pending_key, None);
                    assert!(!h.fake.calls().iter().any(|c| c.starts_with("delete")));
                    h.press(cancel).await;
                    assert!(matches!(h.app.mode, AppMode::Normal));
                }
                h.type_text(keys).await;
                h.press(confirm).await;
                assert_eq!(
                    h.fake.calls().iter().filter(|c| *c == "delete A").count(),
                    1
                );
                assert!(h.app.list.get(&id("A")).is_none());
                assert_eq!(h.selected().as_deref(), Some("B"));
                assert_eq!(h.app.detail.view, DetailView::Live(id("B")));
            }
        }
    }
}

#[tokio::test]
async fn d_types_in_filters_and_sessions_without_opening_delete_confirmation() {
    let mut h = harness(vec![view("A", "dialog", 10)]);
    h.press(KeyCode::Char('/')).await;
    h.press(KeyCode::Char('d')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.list.filter.value(), "d");
    h.press(KeyCode::Esc).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    h.type_text("dd").await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(
        h.fake.outgoing(index),
        vec![
            AttachOutgoing::Input(b"d".to_vec()),
            AttachOutgoing::Input(b"d".to_vec()),
        ]
    );
    assert!(!h.fake.calls().iter().any(|c| c.starts_with("delete")));
}

/// The open modal's tab (Settings or Help), or `None` when none is open.
/// Settings with its first action selected, below the settings.
fn settings_on_actions(in_session: bool) -> AppMode {
    let rows = SettingsItem::ALL.len();
    AppMode::Modal {
        state: ModalState::new(ModalTab::Settings, rows).with_selected(rows),
        in_session,
    }
}

fn settings_tab(h: &Harness) -> Option<ModalTab> {
    match &h.app.mode {
        AppMode::Modal { state, .. } => Some(state.tab),
        _ => None,
    }
}

#[tokio::test]
async fn help_opens_closes_and_lets_cycle_keys_through() {
    let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]);
    h.press(KeyCode::Char('?')).await;
    assert!(matches!(
        h.app.mode,
        AppMode::Modal {
            in_session: false,
            ..
        }
    ));
    assert_eq!(settings_tab(&h), Some(ModalTab::Overview));
    h.press(KeyCode::Tab).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Sessions));
    h.alt(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("B"));
    assert!(matches!(h.app.mode, AppMode::Modal { .. }));
    h.press(KeyCode::Char('q')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    // q in the list quits.
    assert_eq!(h.press(KeyCode::Char('q')).await, AppResult::Exit);
}

#[tokio::test]
async fn settings_and_help_open_from_the_list_with_their_keys() {
    let mut h = harness(vec![view("A", "a", 20)]);
    h.press(KeyCode::Char('o')).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Settings));
    // The key that opened it closes it.
    h.press(KeyCode::Char('o')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    h.press(KeyCode::Char('?')).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Overview));
    h.press(KeyCode::Char('5')).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::License));
    h.press(KeyCode::Char('?')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    // `,` opens Settings too; the Rakumux key closes a modal.
    h.press(KeyCode::Char(',')).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Settings));
    // One page: number keys and Tab have nothing to switch to.
    for key in [KeyCode::Char('2'), KeyCode::Tab] {
        h.press(key).await;
        assert_eq!(settings_tab(&h), Some(ModalTab::Settings));
    }
    h.rakumux().await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.focus, Focus::List);
}

#[tokio::test]
async fn settings_runs_the_selected_action() {
    let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]);
    let open_keys = |h: &mut Harness| {
        h.app.mode = settings_on_actions(h.app.focus == Focus::Session);
    };
    let row = |action: crate::help::KeyAction| {
        crate::help::KeyAction::ALL
            .iter()
            .position(|a| *a == action)
            .unwrap()
    };
    // New session.
    open_keys(&mut h);
    for _ in 0..row(crate::help::KeyAction::NewSession) {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.focus, Focus::Session);
    h.rakumux().await;
    // Next session, from Rakumux mode.
    open_keys(&mut h);
    for _ in 0..row(crate::help::KeyAction::NextSession) {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.selected().as_deref(), Some("B"));
    // Help opens Help; Quit quits.
    open_keys(&mut h);
    for _ in 0..row(crate::help::KeyAction::Help) {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Overview));
    h.press(KeyCode::Esc).await;
    open_keys(&mut h);
    h.press(KeyCode::End).await;
    assert_eq!(h.press(KeyCode::Enter).await, AppResult::Exit);
    // From a session: "Rakumux mode" leaves it, "Back to the session"
    // returns.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    open_keys(&mut h);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::List);
    open_keys(&mut h);
    for _ in 0..row(crate::help::KeyAction::BackToSession) {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
}

#[tokio::test]
async fn shift_r_asks_before_restarting_only_the_ui() {
    for confirm in [KeyCode::Enter, KeyCode::Char('y')] {
        let mut h = harness(vec![view("A", "a", 20)]);
        let (index, _) = h.last_attach();
        h.fake.outgoing(index);
        h.fake.clear_calls();
        // Repeating the shortcut must not confirm the restart.
        h.type_text("RR").await;
        assert!(!h.app.restart_requested);
        let AppMode::Confirm(dialog) = &h.app.mode else {
            panic!("no restart warning")
        };
        assert_eq!(dialog.action, ConfirmAction::RestartUi);
        assert_eq!(dialog.title, "Restart Rakumux?");
        assert!(dialog
            .message
            .contains("all running sessions will keep running"));
        h.press(confirm).await;
        assert!(h.app.restart_requested);
        assert!(h.fake.calls().is_empty());
        assert!(h.fake.outgoing(index).is_empty());
        assert_eq!(
            h.app.list.get(&id("A")).unwrap().status,
            SessionStatus::Live
        );
    }
}

#[tokio::test]
async fn restart_confirmation_can_be_cancelled_without_affecting_sessions() {
    for cancel in [KeyCode::Esc, KeyCode::Char('n'), KeyCode::Char('q')] {
        let mut h = harness(vec![view("A", "a", 20)]);
        h.fake.clear_calls();
        h.press(KeyCode::Char('R')).await;
        h.press(cancel).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        assert!(!h.app.restart_requested);
        assert!(h.fake.calls().is_empty());
    }
}

#[tokio::test]
async fn a_ui_restart_does_not_require_a_daemon_connection() {
    let mut h = harness(vec![]);
    h.app.connection = Connection::Connecting;
    h.press(KeyCode::Char('R')).await;
    assert!(!h.app.restart_requested);
    h.press(KeyCode::Enter).await;
    assert!(h.app.restart_requested);
}

#[tokio::test]
async fn shift_r_still_types_in_a_session_and_in_the_list_filter() {
    let mut h = harness(vec![view("A", "a", 20)]);
    h.press(KeyCode::Enter).await;
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    h.press(KeyCode::Char('R')).await;
    assert!(!h.app.restart_requested);
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Input(b"R".to_vec())]
    );
    h.rakumux().await;
    h.press(KeyCode::Char('/')).await;
    h.press(KeyCode::Char('R')).await;
    assert!(!h.app.restart_requested);
    assert_eq!(h.app.list.filter.value(), "R");
}

#[tokio::test]
async fn shift_u_updates_after_confirmation_and_restarts_only_the_ui() {
    let mut h = harness(vec![view("A", "a", 20)]);
    let updater = Arc::new(testing::FakeUpdater::default());
    *updater.latest.lock().unwrap() = Some("99.1.0".into());
    *updater.launcher.lock().unwrap() = Some("/updated/rakumux.sh".into());
    h.app.updater = updater.clone();
    let calls_before = h.fake.calls();
    h.key(crossterm::event::KeyEvent::new(
        KeyCode::Char('U'),
        KeyModifiers::SHIFT,
    ))
    .await;
    assert!(
        matches!(&h.app.mode, AppMode::Upgrade(dialog) if dialog.update_available && !dialog.installing)
    );
    assert_eq!(*updater.calls.lock().unwrap(), ["check"]);
    assert!(
        !h.app.restart_requested,
        "a version check is not install authorization"
    );
    h.press(KeyCode::Enter).await;
    assert_eq!(*updater.calls.lock().unwrap(), ["check", "install 99.1.0"]);
    assert!(h.app.restart_requested);
    assert_eq!(
        h.app.restart_launcher.as_deref(),
        Some(std::path::Path::new("/updated/rakumux.sh"))
    );
    assert_eq!(
        h.fake.calls(),
        calls_before,
        "updates never stop/restart the daemon or mutate sessions"
    );
}

#[tokio::test]
async fn update_cancel_errors_and_cached_availability_never_install_implicitly() {
    let mut h = harness(vec![view("A", "a", 20)]);
    let updater = Arc::new(testing::FakeUpdater::default());
    *updater.latest.lock().unwrap() = Some("99.1.0".into());
    h.app.updater = updater.clone();
    h.press(KeyCode::Char('U')).await;
    h.press(KeyCode::Esc).await;
    h.app.load_update_state();
    assert_eq!(*updater.calls.lock().unwrap(), ["check"]);
    assert!(!h.app.restart_requested);
    *updater.install_error.lock().unwrap() =
        Some("SHA-256 checksum mismatch; update was not installed".into());
    h.press(KeyCode::Char('U')).await;
    h.press(KeyCode::Enter).await;
    assert!(
        matches!(&h.app.mode, AppMode::Upgrade(dialog) if dialog.error_message.as_deref().is_some_and(|text| text.contains("checksum mismatch")))
    );
    assert!(!h.app.restart_requested);
    h.press(KeyCode::Esc).await;
    *updater.check_error.lock().unwrap() = Some("Source build: rebuild with ./build.sh".into());
    h.press(KeyCode::Char('U')).await;
    assert!(
        matches!(&h.app.mode, AppMode::Upgrade(dialog) if !dialog.update_available && dialog.error_message.as_ref().unwrap().contains("Source build"))
    );
    assert!(!h.app.restart_requested);
}

#[tokio::test]
async fn stale_update_jobs_are_ignored_and_uppercase_u_still_types_in_sessions() {
    let mut h = harness(vec![view("A", "a", 20)]);
    h.press(KeyCode::Char('U')).await;
    let generation = h.app.update_generation;
    h.press(KeyCode::Esc).await;
    h.app.handle_event(AppEvent::Upgrade {
        generation,
        message: types::UpgradeMessage::InstallSuccess { launcher: None },
    });
    assert!(!h.app.restart_requested);
    h.press(KeyCode::Enter).await;
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    h.press(KeyCode::Char('U')).await;
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Input(b"U".to_vec())]
    );
    assert!(matches!(h.app.mode, AppMode::Normal));
    h.rakumux().await;
    h.press(KeyCode::Char('/')).await;
    h.press(KeyCode::Char('U')).await;
    assert_eq!(h.app.list.filter.value(), "U");
    assert!(matches!(h.app.mode, AppMode::Normal));
}

#[tokio::test]
async fn settings_has_an_update_action() {
    let mut h = harness(vec![view("A", "a", 20)]);
    h.press(KeyCode::Char('o')).await;
    for _ in 0..SettingsItem::Update.index() {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert!(
        matches!(&h.app.mode, AppMode::Upgrade(dialog) if !dialog.checking && !dialog.update_available)
    );
}

#[tokio::test]
async fn installation_cannot_be_cancelled_or_started_twice_by_more_keys() {
    let mut h = harness(vec![view("A", "a", 20)]);
    let updater = Arc::new(testing::FakeUpdater::default());
    h.app.updater = updater.clone();
    h.press(KeyCode::Char('U')).await;
    if let AppMode::Upgrade(dialog) = &mut h.app.mode {
        dialog.installing = true;
    }
    for code in [
        KeyCode::Esc,
        KeyCode::Char('q'),
        KeyCode::Char('U'),
        KeyCode::Enter,
    ] {
        assert_eq!(h.press(code).await, AppResult::Continue);
        assert!(matches!(&h.app.mode, AppMode::Upgrade(dialog) if dialog.installing));
    }
    assert_eq!(*updater.calls.lock().unwrap(), ["check"]);
    assert!(!h.app.restart_requested);
}

#[tokio::test]
async fn settings_can_restart_only_the_ui_from_either_focus() {
    for focus_session in [false, true] {
        let mut h = harness(vec![view("A", "a", 20)]);
        if focus_session {
            h.press(KeyCode::Enter).await;
        }
        h.app.mode = settings_on_actions(focus_session);
        let AppMode::Modal { state, .. } = &mut h.app.mode else {
            unreachable!();
        };
        state.select_action(KeyAction::Restart);
        h.fake.clear_calls();
        h.press(KeyCode::Enter).await;
        assert!(
            matches!(&h.app.mode, AppMode::Confirm(dialog) if dialog.action == ConfirmAction::RestartUi)
        );
        assert!(!h.app.restart_requested);
        h.press(KeyCode::Enter).await;
        assert!(h.app.restart_requested);
        assert!(h.fake.calls().is_empty());
        assert_eq!(
            h.app.list.get(&id("A")).unwrap().status,
            SessionStatus::Live
        );
    }
}

#[tokio::test]
async fn filter_narrows_the_list() {
    let mut h = harness(vec![view("A", "api", 20), view("B", "docs", 10)]);
    h.press(KeyCode::Char('/')).await;
    h.type_text("doc").await;
    assert_eq!(h.app.list.visible().len(), 1);
    assert_eq!(h.selected().as_deref(), Some("B"));
    h.press(KeyCode::Enter).await;
    assert!(!h.app.list.filter_editing);
    assert_eq!(h.app.list.filter.value(), "doc");
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.list.visible().len(), 2);
}

#[tokio::test]
async fn settings_switch_layout_live_without_saving_it() {
    let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]);
    h.press(KeyCode::Char('o')).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Settings));
    h.press(KeyCode::Enter).await; // Layout is the first entry
    assert_eq!(settings_tab(&h), Some(ModalTab::Settings));
    assert_eq!(h.app.session_layout(), SessionLayout::Grid);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.session_layout(), SessionLayout::Tabs);
    // Tabs keep creation order: B (created at 10) is tab 1.
    assert_eq!(h.app.list.visible()[0].id, id("B"));
    // Not saved: the next start opens Master detail.
    let saved = Settings::load_from(&h.home.path().join("config.json")).unwrap();
    assert_eq!(saved.sessions.layout, SessionLayout::Sidebar);
    h.press(KeyCode::Esc).await;
    // Number keys go into that tab's session.
    h.press(KeyCode::Char('1')).await;
    assert_eq!(h.selected().as_deref(), Some("B"));
    assert_eq!(h.app.focus, Focus::Session);
    h.prefix('2').await;
    assert_eq!(h.selected().as_deref(), Some("A"));
}

#[tokio::test]
async fn auto_named_sessions_are_named_when_they_settle() {
    let dir = tempfile::tempdir().unwrap();
    let scope = ScopeRoot::resolve(dir.path()).unwrap();
    let fake = FakeApi::new(vec![view_in(&scope, "A", "user-named", 10)]);
    fake.set_read("N1", "$ cargo test\nok");
    let home = tempfile::tempdir().unwrap();
    let options = options(&fake, scope.clone(), &home);
    let mut h = start(options, fake, home);
    h.press(KeyCode::Char('n')).await;
    h.press(KeyCode::Enter).await;
    assert!(h.app.list.get(&id("N1")).unwrap().auto_named);

    // Working: nothing yet.
    h.fake.emit_in(
        &scope,
        "N1",
        SessionEvent::Lifecycle {
            lifecycle: Lifecycle::Working,
        },
    );
    h.app.process_pending();
    assert!(!h.fake.calls().iter().any(|c| c.starts_with("rename")));
    // Idle: named once.
    h.fake.emit_in(
        &scope,
        "N1",
        SessionEvent::Lifecycle {
            lifecycle: Lifecycle::Idle,
        },
    );
    h.app.process_pending();
    assert!(h.fake.calls().contains(&"rename N1 fixed_name".to_string()));
    assert_eq!(h.app.list.get(&id("N1")).unwrap().name, "fixed_name");
    h.fake.clear_calls();
    h.fake.emit_in(
        &scope,
        "N1",
        SessionEvent::Lifecycle {
            lifecycle: Lifecycle::Done,
        },
    );
    h.app.process_pending();
    assert!(!h.fake.calls().iter().any(|c| c.starts_with("rename")));

    // Sessions the user named are never touched.
    h.fake.emit_in(
        &scope,
        "A",
        SessionEvent::Lifecycle {
            lifecycle: Lifecycle::Idle,
        },
    );
    h.app.process_pending();
    assert!(!h.fake.calls().iter().any(|c| c.starts_with("rename A")));
}

#[test]
fn naming_input_reads_history_and_the_visible_screen() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::with_home(dir.path().to_path_buf());
    let session = id("S1");
    let history = layout.history_file(&session);
    std::fs::create_dir_all(history.parent().unwrap()).unwrap();
    std::fs::write(&history, "1\tcargo build\n2\tcargo test\n").unwrap();
    let fake = FakeApi::new(vec![view("S1", "s", 1)]);
    fake.set_read("S1", "test result: ok");
    let input = auto_name::gather_input(fake.as_ref(), Some(&history), &session);
    assert_eq!(input.history, ["cargo build", "cargo test"]);
    assert_eq!(input.screen, "test result: ok");
    assert!(fake
        .calls()
        .iter()
        .any(|c| c.starts_with("read S1 Visible")));
    // A missing history file is no history.
    let input = auto_name::gather_input(fake.as_ref(), Some(&dir.path().join("nope")), &session);
    assert!(input.history.is_empty());
}

// ---------------------------------------------------------------- mouse and paste

#[tokio::test]
async fn clicking_new_session_creates_once_and_enters_the_shell_without_a_keyboard_action() {
    for layout in SessionLayout::ALL {
        for (width, height) in [(40, 20), (80, 24), (90, 24), (160, 48)] {
            // Tabs has no New session item (`n` makes one); a phone has no
            // Grid.
            if layout == SessionLayout::Tabs || (layout == SessionLayout::Grid && width < 80) {
                continue;
            }
            for existing in [false, true] {
                let mut h = harness(if existing {
                    vec![view("A", "agent", 10)]
                } else {
                    vec![]
                });
                h.app.resize(width, height);
                h.app.set_layout(layout);
                // On short grids the action may be on a later page, and
                // below a short list's fold.
                h.app.list.select(Selection::NewSession);
                h.app.process_pending();
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut h.app))
                    .unwrap();
                let new_row = h.app.list.row_count() - 1;
                let new_card = h.app.list.visible().len();
                let area = h.app.hits.iter().find_map(|(area, hit)| {
                    (*hit == Hit::ListRow(new_row) || *hit == Hit::GridCell(new_card)).then_some(*area)
                }).unwrap_or_else(|| panic!("New session hit target missing: {layout:?} {width}x{height} existing={existing}"));
                for index in 0..h.fake.attach_count() {
                    h.fake.outgoing(index);
                }
                h.app.handle_mouse(mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    area.x + 3,
                    area.y + 1,
                ));
                assert!(h.app.creating_session);
                // Repeated presses while the create result is pending cannot
                // create duplicates or route input to the previous session.
                h.app.handle_mouse(mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    area.x + 3,
                    area.y + 1,
                ));
                h.app.handle_mouse(mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    area.x + 3,
                    area.y + 1,
                ));
                h.app.process_pending();
                h.app.handle_mouse(mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    area.x + 3,
                    area.y + 1,
                ));
                h.app.process_pending();

                assert_eq!(
                    h.fake.creates().len(),
                    1,
                    "{layout:?} {width}x{height} existing={existing}"
                );
                assert_eq!(h.selected().as_deref(), Some("N1"));
                assert_eq!(h.app.focus, Focus::Session);
                assert!(matches!(h.app.mode, AppMode::Normal));
                assert!(h.app.drag.is_none());
                let (index, request) = h.last_attach();
                assert_eq!(request.id, id("N1"));
                assert_eq!(request.role, Role::Controller);
                h.fake.outgoing(index);
                h.press(KeyCode::Char('x')).await;
                assert_eq!(
                    h.fake.outgoing(index),
                    vec![AttachOutgoing::Input(b"x".to_vec())]
                );
                for previous in 0..index {
                    assert!(h
                        .fake
                        .outgoing(previous)
                        .iter()
                        .all(|event| !matches!(event, AttachOutgoing::Input(_))));
                }
            }
        }
    }
}

#[tokio::test]
async fn clicking_a_row_selects_it_and_paste_goes_to_the_focused_session() {
    let mut h = harness(vec![aged("A", "a", 1, 20), aged("B", "b", 2, 10)]).on_folders_tab();
    let backend = ratatui::backend::TestBackend::new(160, 48);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let (row_area, _) = *h
        .app
        .hits
        .iter()
        .find(|(_, hit)| *hit == Hit::ListRow(3))
        .unwrap();
    h.app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row_area.x + 2,
        row: row_area.y,
        modifiers: KeyModifiers::NONE,
    });
    h.app.process_pending();
    assert_eq!(h.selected().as_deref(), Some("B"));

    h.press(KeyCode::Enter).await;
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    h.app.handle_paste("echo hi");
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Input(b"echo hi".to_vec())]
    );

    // A click on the list from inside B switches to Rakumux mode like
    // the Rakumux key: Esc goes back into B.
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let row_of = |h: &Harness, id: &str| {
        let row = h
            .app
            .list
            .rows()
            .iter()
            .position(|r| matches!(r, ListRow::Session(s) if s.id.as_str() == id))
            .unwrap();
        h.app
            .hits
            .iter()
            .find(|(_, hit)| *hit == Hit::ListRow(row))
            .map(|(area, _)| *area)
            .unwrap()
    };
    let a = row_of(&h, "A");
    h.app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: a.x + 2,
        row: a.y,
        modifiers: KeyModifiers::NONE,
    });
    h.app.process_pending();
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(h.app.came_from, Some(id("B")));
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.selected().as_deref(), Some("B"));
}

// ---------------------------------------------------------------- order

/// Draw at the harness's 160×48, as the event loop does after each input,
/// so the click targets are current.
fn draw(h: &mut Harness) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(160, 48);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    terminal.backend().buffer().clone()
}

/// The list row of session `id`, from the last draw.
fn list_row(h: &Harness, id: &str) -> ratatui::layout::Rect {
    let row = h
        .app
        .list
        .rows()
        .iter()
        .position(|r| matches!(r, ListRow::Session(s) if s.id.as_str() == id))
        .unwrap();
    h.app
        .hits
        .iter()
        .find(|(_, hit)| *hit == Hit::ListRow(row))
        .map(|(area, _)| *area)
        .unwrap()
}

/// The left button pressed, dragged or released at `column`, `row`.
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// Press the left button at `(column, row)`, drag through `path` (drawing
/// after each step) and release at its end.
fn drag(h: &mut Harness, (column, row): (u16, u16), path: &[(u16, u16)]) {
    h.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row));
    h.app.process_pending();
    draw(h);
    let mut end = (column, row);
    for &(column, row) in path {
        h.app
            .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), column, row));
        draw(h);
        end = (column, row);
    }
    h.app
        .handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end.0, end.1));
}

/// `sessions.session_order` in the saved config.json.
fn saved_order(h: &Harness) -> serde_json::Value {
    let saved = std::fs::read_to_string(h.home.path().join("config.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
    saved["sessions"]["session_order"].clone()
}

async fn shift(h: &mut Harness, c: char) {
    h.key(crossterm::event::KeyEvent::new(
        KeyCode::Char(c),
        KeyModifiers::SHIFT,
    ))
    .await;
}

#[tokio::test]
async fn dragging_a_session_moves_it_live_and_saves_the_order_on_release() {
    let mut h = harness(vec![
        view("A", "alpha", 10),
        view("B", "bravo", 20),
        view("C", "charlie", 30),
    ])
    .on_folders_tab();
    draw(&mut h);
    let (a, b, c) = (list_row(&h, "A"), list_row(&h, "B"), list_row(&h, "C"));
    let x = a.x + 4;
    h.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, a.y + 1));
    assert_eq!(h.selected().as_deref(), Some("A"));
    // Not dragged yet: no drag bar (the pointer's row shows its pin).
    let buffer = draw(&mut h);
    assert_ne!(buffer.get(a.x + 1, a.y + 1).symbol(), "▎");
    // Over B: A takes its place, marked as dragged, still selected.
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), x, b.y + 1));
    assert_eq!(visible_ids(&h), ["B", "A", "C"]);
    assert_eq!(h.selected().as_deref(), Some("A"));
    let buffer = draw(&mut h);
    let mark = buffer.get(b.x + 1, b.y + 1);
    assert_eq!(mark.symbol(), "▎");
    assert_eq!(mark.fg, crate::theme::palette().accent_primary);
    assert_eq!(buffer.get(b.x + 1, b.y + 2).symbol(), "▎");
    // Nothing is saved while it's held.
    assert!(!h.home.path().join("config.json").exists());
    // Over C, then back up past the top: clamped to the first place.
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), x, c.y + 2));
    assert_eq!(visible_ids(&h), ["B", "C", "A"]);
    draw(&mut h);
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), x, 0));
    assert_eq!(visible_ids(&h), ["A", "B", "C"]);
    draw(&mut h);
    // Down past the bottom: the last place. Released: saved, mark gone.
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), x, 46));
    h.app
        .handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), x, 46));
    assert_eq!(visible_ids(&h), ["B", "C", "A"]);
    assert_eq!(
        saved_order(&h),
        serde_json::json!({"/repo": ["B", "C", "A"]})
    );
    assert_eq!(h.app.drag, None);
    let buffer = draw(&mut h);
    let a = list_row(&h, "A");
    assert_eq!(buffer.get(a.x + 1, a.y + 1).symbol(), " ");
    // Activity doesn't undo it.
    h.event(
        "B",
        SessionEvent::Updated {
            session: view("B", "bravo", 99),
        },
    );
    assert_eq!(visible_ids(&h), ["B", "C", "A"]);
}

#[tokio::test]
async fn a_click_without_moving_still_selects_and_goes_in() {
    let mut h = harness(vec![view("A", "a", 10), view("B", "b", 20)]);
    draw(&mut h);
    let a = list_row(&h, "A");
    // Pressed and released on the same row (jitter within it included).
    drag(&mut h, (a.x + 4, a.y + 1), &[(a.x + 6, a.y + 2)]);
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(visible_ids(&h), ["A", "B"]);
    assert!(!h.home.path().join("config.json").exists());
    // A click on the session works in it.
    let pane = h.app.screen().terminal.unwrap();
    h.app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        pane.x + 2,
        pane.y + 2,
    ));
    h.app.process_pending();
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.last_attach().1.id, id("A"));
    // The divider still drags.
    let divider = h.app.screen().divider.unwrap();
    drag(&mut h, (divider.x, divider.y + 3), &[(70, divider.y + 3)]);
    assert_eq!(h.app.screen().list.unwrap().width, 70);
    assert_eq!(visible_ids(&h), ["A", "B"]);
}

#[tokio::test]
async fn a_drag_stays_in_its_folder_group() {
    let mut h = folders_harness();
    assert_eq!(visible_ids(&h), ["R1", "R2", "A1", "W1"]);
    draw(&mut h);
    let (r1, a1) = (list_row(&h, "R1"), list_row(&h, "A1"));
    let x = r1.x + 4;
    // Down into /api: R1 stops at the bottom of /repo.
    drag(
        &mut h,
        (x, r1.y + 1),
        &[(x, a1.y + 1), (x, a1.y + 2), (x, 46)],
    );
    assert_eq!(visible_ids(&h), ["R2", "R1", "A1", "W1"]);
    assert_eq!(saved_order(&h), serde_json::json!({"/repo": ["R2", "R1"]}));
    // A1 dragged up over /repo stays the first (and only) of /api.
    draw(&mut h);
    let (r2, a1) = (list_row(&h, "R2"), list_row(&h, "A1"));
    drag(&mut h, (x, a1.y + 1), &[(x, r2.y + 1), (x, 0)]);
    assert_eq!(visible_ids(&h), ["R2", "R1", "A1", "W1"]);
}

#[tokio::test]
async fn dragging_a_tab_reorders_the_tabs() {
    let mut h = harness_with_folders(
        vec![
            view("A", "alpha", 10),
            view("B", "bravo", 20),
            view("C", "charlie", 30),
        ],
        &[],
        SessionLayout::Tabs,
    );
    draw(&mut h);
    let tab = |h: &Harness, index: usize| {
        h.app
            .tab_hits
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, area)| *area)
            .unwrap()
    };
    let (first, third) = (tab(&h, 0), tab(&h, 2));
    let y = first.y;
    // alpha onto charlie, through bravo: it ends last.
    let path: Vec<(u16, u16)> = (first.x + 1..third.x + 2).map(|x| (x, y)).collect();
    drag(&mut h, (first.x + 1, y), &path);
    assert_eq!(visible_ids(&h), ["B", "C", "A"]);
    assert_eq!(h.selected().as_deref(), Some("A"));
    assert_eq!(
        saved_order(&h),
        serde_json::json!({"/repo": ["B", "C", "A"]})
    );
    // Left of every tab (over the folder filter): the first place.
    draw(&mut h);
    let last = tab(&h, 2);
    drag(&mut h, (last.x + 1, y), &[(last.x, y), (1, y)]);
    assert_eq!(visible_ids(&h), ["A", "B", "C"]);
}

#[tokio::test]
async fn shift_j_and_k_move_the_selected_session_and_the_order_survives_a_restart() {
    let mut h = harness(vec![
        view("A", "a", 10),
        view("B", "b", 20),
        view("C", "c", 30),
    ])
    .on_folders_tab();
    // The newest is last and selected (most recently used).
    assert_eq!(h.selected().as_deref(), Some("C"));
    shift(&mut h, 'K').await;
    assert_eq!(visible_ids(&h), ["A", "C", "B"]);
    assert_eq!(
        saved_order(&h),
        serde_json::json!({"/repo": ["A", "C", "B"]})
    );
    shift(&mut h, 'K').await;
    shift(&mut h, 'K').await;
    assert_eq!(visible_ids(&h), ["C", "A", "B"], "stops at the top");
    shift(&mut h, 'J').await;
    assert_eq!(visible_ids(&h), ["A", "C", "B"]);
    assert_eq!(h.selected().as_deref(), Some("C"));
    // Pinned sessions stay above the others.
    h.press(KeyCode::Char('k')).await;
    h.press(KeyCode::Char('p')).await;
    assert_eq!(visible_ids(&h), ["A", "C", "B"]);
    h.press(KeyCode::Char('j')).await;
    shift(&mut h, 'K').await;
    assert_eq!(visible_ids(&h), ["A", "C", "B"]);
    assert_eq!(h.selected().as_deref(), Some("C"));
    // On the folder filter there's nothing to move.
    h.press(KeyCode::Home).await;
    shift(&mut h, 'J').await;
    assert_eq!(h.app.status(), Some("Select a session to move."));
    // Inside a session J is the program's (past the folder's header).
    h.press(KeyCode::Char('j')).await;
    h.press(KeyCode::Char('j')).await;
    h.press(KeyCode::Enter).await;
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    shift(&mut h, 'J').await;
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Input(b"J".to_vec())]
    );
    assert_eq!(visible_ids(&h), ["A", "C", "B"]);

    // The next start reads it back; a session made since goes last.
    let Harness { fake, home, .. } = h;
    fake.state.lock().unwrap().sessions.push(view("D", "d", 1));
    let mut options = options(&fake, scope(), &home);
    options.settings = Settings::load_from(&home.path().join("config.json")).unwrap();
    let h = start(options, fake, home).on_folders_tab();
    assert_eq!(visible_ids(&h), ["A", "C", "B", "D"]);
}

#[tokio::test]
async fn j_and_k_leave_the_done_section_in_the_order_it_was_read() {
    let read = |id: &str, at: i64| {
        let mut session = view(id, &id.to_lowercase(), at);
        session.lifecycle = Lifecycle::Idle;
        session
    };
    let mut h = harness(vec![read("A", 10), read("B", 20)]);
    assert_eq!(h.selected().as_deref(), Some("B"));
    assert_eq!(visible_ids(&h), ["B", "A"]);
    shift(&mut h, 'J').await;
    assert_eq!(visible_ids(&h), ["B", "A"]);
    assert_eq!(
        h.app.status(),
        Some("Done lists sessions by when you last read them.")
    );
    assert!(!h.home.path().join("config.json").exists(), "nothing saved");
}

#[tokio::test]
async fn done_periods_have_headers_with_counts_and_enter_opens_the_status_modal_on_done() {
    use grouping::{Grouping, ReadPeriod, Section, StatusGroup};
    let now = crate::components::format::now_secs();
    let read = |id: &str, at: i64| {
        let mut session = view(id, &id.to_lowercase(), at);
        session.lifecycle = Lifecycle::Idle;
        session
    };
    let mut h = harness(vec![read("A", now - 30), read("B", now - 3 * 86_400)]);
    let screen = screen_rows(&mut h, 160, 48).join("\n");
    assert!(screen.contains("   ◷ last hour ─"), "{screen}");
    assert!(screen.contains("─ 1 session"), "{screen}");
    assert!(screen.contains("   ◷ older ─"), "{screen}");
    h.app
        .list
        .select(session_list::Selection::ReadPeriodHeader(ReadPeriod::Older));
    h.press(KeyCode::Enter).await;
    assert_eq!(section_picker(&h).grouping, Grouping::Status);
    assert_eq!(
        section_picker(&h).current_row(),
        Some(&types::SectionRow::Section(Section::Status(
            StatusGroup::Ready
        )))
    );
}

#[tokio::test]
async fn the_keys_tab_lists_and_runs_the_move_actions() {
    let mut h = harness(vec![view("A", "a", 10), view("B", "b", 20)]).on_folders_tab();
    let row = |action: crate::help::KeyAction| {
        crate::help::KeyAction::ALL
            .iter()
            .position(|a| *a == action)
            .unwrap()
    };
    let up = row(crate::help::KeyAction::MoveUp);
    assert_eq!(row(crate::help::KeyAction::MoveDown) + 1, up);
    assert_eq!(
        crate::help::KeyAction::MoveUp.section(),
        "Sessions and folders"
    );
    // From a session too: it moves in Rakumux mode.
    h.press(KeyCode::Enter).await;
    h.app.mode = settings_on_actions(true);
    for _ in 0..up {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(visible_ids(&h), ["B", "A"]);
    assert_eq!(saved_order(&h), serde_json::json!({"/repo": ["B", "A"]}));
}

// ---------------------------------------------------------------- folders

/// `/repo` (launch) with two sessions, `/web` with one, `/api` with one;
/// `/empty` is known but has none.
fn folders_harness() -> Harness {
    harness_with_folders(
        vec![
            view("R1", "repo-old", 10),
            view("R2", "repo-new", 20),
            view_in(&folder("/web"), "W1", "web-one", 50),
            view_in(&folder("/api"), "A1", "api-one", 40),
        ],
        &[("/web", on_branch("main")), ("/empty", None)],
        SessionLayout::Sidebar,
    )
}

fn picker(h: &Harness) -> &types::FolderPicker {
    match &h.app.mode {
        AppMode::Folders(picker) => picker,
        _ => panic!("the Folders modal is closed"),
    }
}

/// The modal's rows: `all`, folder paths, `+ path` for a folder to add,
/// `+new path` for one to create, `> value` for a folder completing the
/// typed path.
fn picker_rows(h: &Harness) -> Vec<String> {
    picker(h)
        .rows
        .iter()
        .map(|row| match row {
            PickerRow::All => "all".to_string(),
            PickerRow::Folder(p) => p.path().display().to_string(),
            PickerRow::Add(NewFolder::Existing(p)) => format!("+ {}", p.display()),
            PickerRow::Add(NewFolder::Create(p)) => format!("+new {}", p.display()),
            PickerRow::Suggestion(folder) => format!("> {}", folder.value),
        })
        .collect()
}

/// Put the modal's cursor on the row for `path`.
fn cursor_to(h: &mut Harness, path: &str) {
    let row = picker(h)
        .rows
        .iter()
        .position(|r| matches!(r, PickerRow::Folder(p) if p == &folder(path)))
        .unwrap_or_else(|| panic!("no {path} row"));
    if let AppMode::Folders(picker) = &mut h.app.mode {
        picker.cursor = row;
    }
}

fn visible_ids(h: &Harness) -> Vec<String> {
    h.app
        .list
        .visible()
        .iter()
        .map(|s| s.id.to_string())
        .collect()
}

/// Calls that list sessions or open a subscription.
fn list_calls(h: &Harness) -> Vec<String> {
    h.fake
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("list") || c.starts_with("subscribe"))
        .collect()
}

#[tokio::test]
async fn every_folder_is_listed_and_watched_from_the_start() {
    let mut h = folders_harness();
    let calls = h.fake.calls();
    assert_eq!(
        calls[..4],
        [
            "add_project /repo",
            "projects",
            "subscribe all",
            "list_all dead=true"
        ],
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.starts_with("list_in")), "{calls:?}");
    // All folders, under headers: the launch folder first, then by name.
    assert!(h.app.list.folder_filter().is_empty());
    assert_eq!(h.app.folder_label(), "Folders");
    assert!(h.app.list.is_grouped());
    assert_eq!(
        h.app.list.groups(),
        [scope(), folder("/api"), folder("/web")]
    );
    assert_eq!(visible_ids(&h), ["R1", "R2", "A1", "W1"]);
    // A session another client starts in a folder never seen before (a new
    // worktree) appears live, and the TUI asks about the folder.
    let fresh = folder("/repo-wt");
    h.fake.clear_calls();
    h.fake.emit_in(
        &fresh,
        "N1",
        SessionEvent::Created {
            session: view_in(&fresh, "N1", "worktree", 60),
        },
    );
    h.app.process_pending();
    assert!(h.app.list.get(&id("N1")).is_some());
    assert_eq!(
        h.app.list.groups(),
        [scope(), folder("/api"), folder("/repo-wt"), folder("/web")]
    );
    assert!(h.fake.calls().contains(&"projects".to_string()));
    assert!(list_calls(&h).is_empty(), "no resubscribe");
}

#[tokio::test]
async fn an_older_daemon_without_sessions_all_lists_its_folders_by_name() {
    let fake = FakeApi::without_all(vec![
        view("R1", "repo-old", 10),
        view_in(&folder("/web"), "W1", "web-one", 50),
    ]);
    fake.add_known_folder(&folder("/empty"), None);
    let dir = tempfile::tempdir().unwrap();
    let options = options(&fake, scope(), &dir);
    let mut h = start(options, fake, dir);
    let calls = h.fake.calls();
    assert!(
        calls.contains(&"subscribe /repo,/web,/empty".to_string()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"list_in /repo,/web,/empty dead=true".to_string()),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.contains("all")), "{calls:?}");
    assert_eq!(h.app.folders.support, folders::FolderSupport::Known);
    assert_eq!(visible_ids(&h), ["R1", "W1"]);
    // The restart notice says what's missing.
    match &h.app.mode {
        AppMode::DaemonRestart(dialog) => assert_eq!(
            dialog.missing,
            [
                "new folders live",
                "the terminal panel",
                "the branch an agent works on"
            ]
        ),
        mode => panic!("no restart offer: {mode:?}"),
    }
    h.press(KeyCode::Char('n')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    // Filtering is local here too.
    h.fake.clear_calls();
    h.app.show_folders(vec![folder("/web")]);
    assert_eq!(visible_ids(&h), ["W1"]);
    assert!(list_calls(&h).is_empty());
    // A folder it isn't watching yet (added since) is listed and watched
    // on demand.
    h.fake.add_known_folder(&folder("/new"), None);
    h.app.show_folders(vec![folder("/new")]);
    assert_eq!(
        list_calls(&h),
        [
            "subscribe /repo,/web,/empty,/new",
            "list_in /repo,/web,/empty,/new dead=true"
        ]
    );
}

#[tokio::test]
async fn cycling_and_movement_run_across_folder_groups() {
    let mut h = folders_harness();
    // The most recent session overall is selected first: web-one.
    assert_eq!(h.selected().as_deref(), Some("W1"));
    h.alt(KeyCode::Down).await;
    assert_eq!(
        h.selected().as_deref(),
        Some("R1"),
        "wraps to the first group"
    );
    h.alt(KeyCode::Up).await;
    assert_eq!(h.selected().as_deref(), Some("W1"));
    // Arrow keys stop on the headers; the folder filter is first and
    // "+ New session" last.
    h.press(KeyCode::Home).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::Folders);
    h.press(KeyCode::Down).await;
    assert_eq!(
        h.app.list.selection(),
        &session_list::Selection::FolderHeader(scope())
    );
    h.press(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("R1"));
    h.press(KeyCode::End).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::NewSession);
    h.press(KeyCode::Up).await;
    h.press(KeyCode::Up).await;
    assert_eq!(
        h.app.list.selection(),
        &session_list::Selection::FolderHeader(folder("/web"))
    );
    // `{` and `}` jump between folders.
    h.press(KeyCode::Char('{')).await;
    assert_eq!(h.selected().as_deref(), Some("A1"));
    h.press(KeyCode::Char('{')).await;
    assert_eq!(h.selected().as_deref(), Some("R1"));
    h.press(KeyCode::Char('}')).await;
    assert_eq!(h.selected().as_deref(), Some("A1"));
}

#[tokio::test]
async fn enter_on_the_folder_filter_opens_the_modal_and_enter_shows_one_folder() {
    let mut h = folders_harness();
    let shown = h.app.detail.view.session_id().cloned();
    assert_eq!(shown, Some(id("W1")));
    // The folder filter keeps the session in view; Enter opens the modal.
    h.press(KeyCode::Home).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::Folders);
    assert_eq!(h.app.detail.view.session_id(), shown.as_ref());
    h.press(KeyCode::Enter).await;
    // All folders (ticked: there's no filter), then every folder with
    // sessions; /empty has none, so it isn't listed.
    assert_eq!(picker_rows(&h), ["all", "/repo", "/api", "/web"]);
    assert!(picker(&h).is_ticked(&PickerRow::All));
    assert_eq!(picker(&h).cursor, 0);
    assert!(h.fake.calls().iter().filter(|c| *c == "projects").count() >= 2);
    // Typing filters by name, path or branch.
    h.type_text("we").await;
    assert_eq!(picker_rows(&h), ["/web"]);
    h.ctrl('u').await;
    h.type_text("main").await;
    assert_eq!(picker_rows(&h), ["/web"]);
    h.ctrl('u').await;
    h.type_text("all").await;
    assert_eq!(picker_rows(&h), ["all"]);
    h.ctrl('u').await;
    h.type_text("zzz").await;
    assert!(picker_rows(&h).is_empty());
    h.press(KeyCode::Enter).await;
    assert!(picker(&h).error.as_deref().unwrap().contains("No folder"));
    h.ctrl('u').await;
    assert_eq!(picker_rows(&h).len(), 4);
    // Enter shows the folder under the cursor alone, without listing or
    // subscribing again.
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Down).await;
    h.fake.clear_calls();
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.list.folder_filter(), [folder("/api")]);
    assert_eq!(h.app.folder_label(), "api");
    assert!(list_calls(&h).is_empty(), "{:?}", h.fake.calls());
    assert_eq!(visible_ids(&h), ["A1"]);
    // One folder, under its header.
    assert!(h.app.list.is_grouped());
    // The cursor leaves the folder filter for the folder's session.
    assert_eq!(h.selected().as_deref(), Some("A1"));
    assert_eq!(h.app.detail.view.session_id(), Some(&id("A1")));
    assert_eq!(h.app.status(), Some("Showing api"));
    // Enter on All folders shows every folder again.
    h.press(KeyCode::Char('P')).await;
    assert_eq!(picker(&h).cursor, 2, "on the filter's folder");
    h.press(KeyCode::Up).await;
    h.press(KeyCode::Up).await;
    h.press(KeyCode::Enter).await;
    assert!(h.app.list.folder_filter().is_empty());
    assert_eq!(visible_ids(&h), ["R1", "R2", "A1", "W1"]);
    assert_eq!(h.app.status(), Some("Showing all folders"));
    assert!(list_calls(&h).is_empty());
}

#[tokio::test]
async fn the_modal_ticks_folders_and_enter_shows_them() {
    let mut h = folders_harness();
    h.press(KeyCode::Char('P')).await;
    // All folders is ticked while nothing else is; Tab there changes
    // nothing.
    h.press(KeyCode::Tab).await;
    assert!(picker(&h).checked.is_empty());
    assert!(!picker(&h).ticks_changed);
    // Tick /repo (Tab) and /api (Space), then untick /repo.
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Tab).await;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Char(' ')).await;
    assert_eq!(picker(&h).checked, [scope(), folder("/api")]);
    assert!(!picker(&h).is_ticked(&PickerRow::All));
    h.press(KeyCode::Up).await;
    h.press(KeyCode::Tab).await;
    assert_eq!(picker(&h).checked, [folder("/api")]);
    assert!(picker(&h).ticks_changed);
    // Nothing changes until Enter. Enter on a folder that isn't ticked
    // shows it with the ticked ones.
    assert!(h.app.list.folder_filter().is_empty());
    cursor_to(&mut h, "/web");
    h.fake.clear_calls();
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.list.folder_filter(), [folder("/api"), folder("/web")]);
    assert_eq!(h.app.folder_label(), "2 folders");
    assert!(list_calls(&h).is_empty());
    assert_eq!(visible_ids(&h), ["A1", "W1"]);
    assert!(h.app.list.is_grouped());
    assert_eq!(h.selected().as_deref(), Some("W1"));
    assert_eq!(h.app.status(), Some("Showing 2 folders"));

    // Opened again, it's ticked like the filter; Space on All folders
    // clears the ticks, and Enter shows every folder.
    h.press(KeyCode::Char('P')).await;
    assert_eq!(picker(&h).checked, [folder("/api"), folder("/web")]);
    assert_eq!(picker(&h).cursor, 2, "on the filter's first folder");
    if let AppMode::Folders(picker) = &mut h.app.mode {
        picker.cursor = 0;
    }
    h.press(KeyCode::Char(' ')).await;
    assert!(picker(&h).checked.is_empty());
    h.press(KeyCode::Enter).await;
    assert!(h.app.list.folder_filter().is_empty());

    // The filter isn't saved: the next start shows All folders.
    h.app.show_folders(vec![folder("/web")]);
    let fake = h.fake.clone();
    let mut options = options(&fake, scope(), &h.home);
    options.settings = Settings::load_from(&h.home.path().join("config.json")).unwrap();
    let mut app = App::new(options);
    app.start();
    app.process_pending();
    assert!(app.list.folder_filter().is_empty());
    assert_eq!(app.folder_label(), "Folders");
}

#[tokio::test]
async fn an_old_config_with_selected_projects_starts_on_all_folders_and_drops_it() {
    let fake = FakeApi::new(vec![
        view("R1", "repo-old", 10),
        view_in(&folder("/web"), "W1", "web-one", 50),
    ]);
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.json");
    std::fs::write(
        &config,
        r#"{"sessions":{"selected_projects":["/web"],"layout":"sidebar"}}"#,
    )
    .unwrap();
    let mut options = options(&fake, scope(), &home);
    options.settings = Settings::load_from(&config).unwrap();
    let h = start(options, fake, home);
    assert!(h.app.list.folder_filter().is_empty());
    assert_eq!(visible_ids(&h), ["R1", "W1"]);
    // The next save leaves it out.
    h.app.save_settings().unwrap();
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert!(raw["sessions"].get("selected_projects").is_none(), "{raw}");
}

#[tokio::test]
async fn esc_closes_the_modal_without_changes() {
    let mut h = folders_harness();
    h.press(KeyCode::Char('P')).await;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Tab).await;
    // Letters are typed, not commands.
    h.type_text("Pq").await;
    assert!(matches!(h.app.mode, AppMode::Folders(_)));
    assert_eq!(picker(&h).query.value(), "Pq");
    h.fake.clear_calls();
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.list.folder_filter().is_empty());
    assert_eq!(h.app.status(), None);
    // Enter on All folders, already shown, changes nothing either.
    h.press(KeyCode::Char('P')).await;
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.list.folder_filter().is_empty());
    assert_eq!(h.app.status(), None);
    assert!(list_calls(&h).is_empty());
}

#[tokio::test]
async fn typing_a_folder_adds_or_creates_it_and_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("other")).unwrap();
    std::fs::write(dir.path().join("file"), "x").unwrap();
    let launch = ScopeRoot::resolve(dir.path()).unwrap();
    let fake = FakeApi::new(vec![
        view_in(&launch, "L1", "here", 10),
        view_in(&folder("/busy"), "B1", "busy", 5),
    ]);
    let home = tempfile::tempdir().unwrap();
    let options = options(&fake, launch.clone(), &home);
    let mut h = start(options, fake, home);
    let other = ScopeRoot::from_canonical(launch.path().join("other"));
    let fresh = launch.path().join("fresh");

    h.press(KeyCode::Char('P')).await;
    // Not a folder: no row to add, and Enter says why.
    h.type_text("./file").await;
    assert!(picker_rows(&h).is_empty());
    assert!(picker(&h)
        .folder_note
        .as_deref()
        .unwrap()
        .contains("not a folder"));
    h.press(KeyCode::Enter).await;
    assert!(picker(&h)
        .error
        .as_deref()
        .unwrap()
        .contains("not a folder"));
    // A folder relative to the launch folder, completed with Tab: the
    // typed name doesn't exist, so the folder it completes to is chosen.
    h.ctrl('u').await;
    h.type_text("./oth").await;
    assert_eq!(
        picker_rows(&h),
        [
            format!("+new {}", launch.path().join("oth").display()),
            "> ./other/".into()
        ]
    );
    assert_eq!(picker(&h).cursor, 1);
    h.press(KeyCode::Tab).await;
    assert_eq!(picker(&h).query.value(), "./other/");
    assert_eq!(picker_rows(&h), [format!("+ {}", other.path().display())]);
    // Enter adds it and shows it alone: an empty folder to start in.
    h.fake.clear_calls();
    h.press(KeyCode::Enter).await;
    assert!(h
        .fake
        .calls()
        .contains(&format!("add_project {}", other.path().display())));
    assert!(list_calls(&h).is_empty());
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.list.folder_filter(), std::slice::from_ref(&other));
    assert!(h.app.list.visible().is_empty());
    assert_eq!(h.app.detail.view, DetailView::Empty);
    h.press(KeyCode::Char('n')).await;
    assert_eq!(created_folder(&h), other);
    h.rakumux().await;

    // A folder that doesn't exist is created after `y`, added and shown.
    h.press(KeyCode::Char('P')).await;
    h.type_text("./fresh").await;
    assert_eq!(picker_rows(&h), [format!("+new {}", fresh.display())]);
    h.press(KeyCode::Enter).await;
    assert_eq!(picker(&h).stage, PickerStage::Creating(fresh.clone()));
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::Esc).await;
    assert_eq!(picker(&h).stage, PickerStage::Browse);
    assert!(!fresh.exists());
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::Char('y')).await;
    assert!(fresh.is_dir());
    assert!(h
        .fake
        .calls()
        .contains(&format!("add_project {}", fresh.display())));
    assert_eq!(
        h.app.list.folder_filter(),
        [ScopeRoot::from_canonical(fresh.clone())]
    );
    // Typed again, it's known: listed, not offered.
    h.press(KeyCode::Char('P')).await;
    h.type_text("./fresh").await;
    assert_eq!(picker_rows(&h), [fresh.display().to_string()]);
    // Forgetting a folder is for the command line now.
    h.ctrl('x').await;
    assert!(matches!(h.app.mode, AppMode::Folders(_)));
    assert!(!h.fake.calls().iter().any(|c| c.starts_with("remove")));
}

#[tokio::test]
async fn typing_a_path_offers_the_folders_that_complete_it() {
    // The launch folder holds code/{alpha, beta, .hidden, web} and a file;
    // the daemon knows code/web already.
    let dir = tempfile::tempdir().unwrap();
    for name in ["code/alpha", "code/beta", "code/.hidden", "code/web"] {
        std::fs::create_dir_all(dir.path().join(name)).unwrap();
    }
    std::fs::write(dir.path().join("code/afile"), "x").unwrap();
    let launch = ScopeRoot::resolve(dir.path()).unwrap();
    let code = launch.path().join("code");
    let alpha = ScopeRoot::from_canonical(code.join("alpha"));
    let web = ScopeRoot::from_canonical(code.join("web"));
    let fake = FakeApi::new(vec![view_in(&launch, "L1", "here", 10)]);
    fake.add_known_folder(&web, None);
    let home = tempfile::tempdir().unwrap();
    let options = options(&fake, launch.clone(), &home);
    let mut h = start(options, fake, home);

    h.press(KeyCode::Char('P')).await;
    h.type_text("./code/").await;
    // The typed folder, then the folders in it (no files, no dot-folders);
    // web is listed once, as a folder.
    assert_eq!(
        picker_rows(&h),
        [
            format!("+ {}", code.display()),
            "> ./code/alpha/".into(),
            "> ./code/beta/".into(),
            "> ./code/web/".into(),
        ]
    );
    assert_eq!(picker(&h).cursor, 0);
    // The folders share no more of their names: Tab chooses the first,
    // and puts the chosen one in.
    h.press(KeyCode::Tab).await;
    assert_eq!(picker(&h).query.value(), "./code/");
    assert_eq!(picker(&h).cursor, 1);
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Tab).await;
    assert_eq!(picker(&h).query.value(), "./code/beta/");
    assert_eq!(
        picker_rows(&h),
        [format!("+ {}", code.join("beta").display())]
    );
    // Dot-folders once the name starts with a dot.
    h.ctrl('u').await;
    h.type_text("./code/.").await;
    assert_eq!(picker_rows(&h)[1], "> ./code/.hidden/");

    // A name that doesn't exist yet: the folder it completes to is chosen.
    h.ctrl('u').await;
    h.type_text("./code/al").await;
    assert_eq!(
        picker_rows(&h),
        [
            format!("+new {}", code.join("al").display()),
            "> ./code/alpha/".into()
        ]
    );
    assert_eq!(picker(&h).cursor, 1);
    // Tab on "+ Create folder" completes the typed name; → at the end of
    // the query does too. Space types in a path.
    h.press(KeyCode::Up).await;
    h.press(KeyCode::Tab).await;
    assert_eq!(picker(&h).query.value(), "./code/alpha/");
    h.ctrl('u').await;
    h.type_text("./code/b").await;
    h.press(KeyCode::Right).await;
    assert_eq!(picker(&h).query.value(), "./code/beta/");
    h.press(KeyCode::Char(' ')).await;
    assert_eq!(picker(&h).query.value(), "./code/beta/ ");
    // A path without a leading `.` or `/` starts at the launch folder.
    h.ctrl('u').await;
    h.type_text("code/al").await;
    assert_eq!(picker_rows(&h)[1..], ["> code/alpha/".to_string()]);

    // Enter on a chosen folder adds it and shows it.
    h.ctrl('u').await;
    h.type_text("./code/al").await;
    h.press(KeyCode::Enter).await;
    assert!(h
        .fake
        .calls()
        .contains(&format!("add_project {}", alpha.path().display())));
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.list.folder_filter(), std::slice::from_ref(&alpha));

    // A folder the daemon knows already is shown, not added.
    h.press(KeyCode::Char('P')).await;
    h.type_text("./code/w").await;
    assert_eq!(picker_rows(&h)[1..], ["> ./code/web/".to_string()]);
    h.fake.clear_calls();
    h.press(KeyCode::Enter).await;
    assert!(!h
        .fake
        .calls()
        .contains(&format!("add_project {}", web.path().display())));
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.list.folder_filter(), std::slice::from_ref(&web));

    // A click on a folder puts it in the query.
    h.press(KeyCode::Char('P')).await;
    h.type_text("./code/").await;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let area = h
        .app
        .hits
        .iter()
        .find(|(_, hit)| *hit == Hit::FolderRow(2))
        .map(|(area, _)| *area)
        .unwrap();
    h.app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 1,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(picker(&h).query.value(), "./code/beta/");
    // Esc closes, as ever.
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
}

#[tokio::test]
async fn p_pins_and_unpins_and_pinned_sessions_lead_their_folder() {
    let mut h = harness(vec![
        view("A", "newest", 30),
        view("B", "middle", 20),
        view("C", "oldest", 10),
    ]);
    // The newest session sits last, just above "+ New session".
    assert_eq!(visible_ids(&h), ["C", "B", "A"]);
    h.press(KeyCode::End).await;
    h.press(KeyCode::Up).await;
    assert_eq!(h.selected().as_deref(), Some("A"));
    h.press(KeyCode::Char('p')).await;
    assert!(h.fake.calls().contains(&"pin A true".to_string()));
    assert!(h.app.list.get(&id("A")).unwrap().pinned);
    assert_eq!(visible_ids(&h), ["A", "C", "B"]);
    assert_eq!(h.selected().as_deref(), Some("A"), "the selection follows");
    assert_eq!(h.app.status(), Some("Pinned newest"));
    h.press(KeyCode::Char('p')).await;
    assert!(h.fake.calls().contains(&"pin A false".to_string()));
    assert_eq!(visible_ids(&h), ["C", "B", "A"]);
    // A pin made elsewhere arrives as an update.
    let mut pinned = view("B", "middle", 20);
    pinned.pinned = true;
    h.event("B", SessionEvent::Updated { session: pinned });
    assert_eq!(visible_ids(&h), ["B", "C", "A"]);
    // "+ New session" selected: nothing to pin.
    h.press(KeyCode::End).await;
    h.fake.clear_calls();
    h.press(KeyCode::Char('p')).await;
    assert!(h.fake.calls().is_empty());
}

#[tokio::test]
async fn an_older_daemon_falls_back_to_the_launch_folder() {
    let fake = FakeApi::without_folders(vec![
        view("A", "a", 10),
        view_in(&folder("/web"), "W1", "w", 20),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let options = options(&fake, scope(), &dir);
    let mut h = start(options, fake, dir);
    let calls = h.fake.calls();
    assert!(calls.contains(&"subscribe /repo".to_string()), "{calls:?}");
    assert!(calls.contains(&"list dead=true".to_string()), "{calls:?}");
    assert!(!calls.iter().any(|c| c.starts_with("list_")), "{calls:?}");
    assert_eq!(h.app.folders.support, folders::FolderSupport::LaunchOnly);
    assert!(h.app.list.folder_filter().is_empty());
    assert_eq!(visible_ids(&h), ["A"]);
    assert_eq!(h.selected().as_deref(), Some("A"));
    // The older daemon is explained, with an offer to restart it that a
    // stray Enter doesn't take.
    match &h.app.mode {
        AppMode::DaemonRestart(dialog) => assert_eq!(
            dialog.missing,
            [
                "git branches and worktrees",
                "folders",
                "pins",
                "new folders live",
                "the terminal panel",
                "the branch an agent works on"
            ]
        ),
        mode => panic!("no restart offer: {mode:?}"),
    }
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::DaemonRestart(_)));
    h.press(KeyCode::Char('n')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.status().unwrap().contains("Kept the older server"));
    // Asking for folders or a pin explains, and nothing else happens.
    h.press(KeyCode::Char('P')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.status().unwrap().contains("doesn't support folders"));
    h.press(KeyCode::Char('p')).await;
    assert!(h.app.status().unwrap().contains("doesn't support pinning"));
    assert!(!h.app.list.get(&id("A")).unwrap().pinned);
    assert!(!h.home.path().join("config.json").exists());
    // A refresh against an upgraded daemon shows every folder.
    {
        let mut state = h.fake.state.lock().unwrap();
        state.folders_unsupported = false;
        state.all_unsupported = false;
    }
    h.ctrl('r').await;
    assert_eq!(h.app.folders.support, folders::FolderSupport::All);
    assert_eq!(visible_ids(&h), ["A", "W1"]);
}

/// The folder used by the last immediate creation request.
fn created_folder(h: &Harness) -> ScopeRoot {
    h.fake.creates().last().unwrap().scope.clone().unwrap()
}

#[tokio::test]
async fn new_sessions_go_in_the_current_folder() {
    let mut h = folders_harness();
    let (web, api) = (folder("/web"), folder("/api"));
    // 1. The selected session's folder: web-one is the most recent.
    assert_eq!(h.selected().as_deref(), Some("W1"));
    h.press(KeyCode::Char('n')).await;
    assert_eq!(created_folder(&h), web);
    let create = h.fake.creates().pop().unwrap();
    assert_eq!(create.scope, Some(web.clone()));
    assert_eq!(create.cwd.as_deref(), Some(web.path()));
    assert_eq!(create.argv, None);
    assert!(create.startup_cmds.is_empty());
    let new_id = h.app.list.selected_id().cloned().unwrap();
    assert_eq!(h.app.list.get(&new_id).unwrap().scope, web);
    assert_eq!(h.app.focus, Focus::Session);
    h.rakumux().await;

    // A session of the launch folder: that folder.
    h.press(KeyCode::Home).await;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("R1"));
    h.press(KeyCode::Char('n')).await;
    assert_eq!(created_folder(&h), scope());
    h.rakumux().await;
    // 2. On a folder's header, that folder: `n` or Enter.
    h.app
        .list
        .select(session_list::Selection::FolderHeader(api.clone()));
    h.press(KeyCode::Char('n')).await;
    assert_eq!(created_folder(&h), api);
    h.rakumux().await;
    h.app
        .list
        .select(session_list::Selection::FolderHeader(api.clone()));
    h.press(KeyCode::Enter).await;
    assert_eq!(created_folder(&h), api);
    h.rakumux().await;
    // 4. Neutral rows retain the last selected section, not the launch folder.
    h.press(KeyCode::Home).await;
    h.press(KeyCode::Char('n')).await;
    assert_eq!(created_folder(&h), api);
    h.rakumux().await;
    h.press(KeyCode::End).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::NewSession);
    h.press(KeyCode::Enter).await;
    assert_eq!(created_folder(&h), api);
    h.rakumux().await;

    // 3. The filter's only folder.
    h.app.show_folders(vec![web.clone()]);
    h.press(KeyCode::End).await;
    h.press(KeyCode::Char('n')).await;
    assert_eq!(created_folder(&h), web);
    h.rakumux().await;
    // Several folders shown: keep the current section, not the hidden launch folder.
    h.app.show_folders(vec![api.clone(), web.clone()]);
    h.press(KeyCode::End).await;
    h.press(KeyCode::Char('n')).await;
    assert_eq!(created_folder(&h), web);
    let new_id = h.app.list.selected_id().cloned().unwrap();
    assert_eq!(h.app.list.get(&new_id).unwrap().scope, web);
    assert_eq!(h.app.list.folder_filter(), [api, web]);
    assert!(h.app.list.visible_position(&new_id).is_some());
}

#[tokio::test]
async fn new_session_folder_follows_keyboard_navigation_in_both_layouts() {
    for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
        // Starting on web, cycle through repo's sessions, api, and back to web.
        for (index, expected) in [scope(), scope(), folder("/api"), folder("/web")]
            .into_iter()
            .enumerate()
        {
            let mut h = folders_harness();
            h.app.set_layout(layout);
            for _ in 0..=index {
                h.alt(KeyCode::Down).await;
            }
            assert_eq!(h.app.current_folder(), expected);
            h.press(KeyCode::Char('n')).await;
            assert_eq!(created_folder(&h), expected);
            assert_eq!(h.app.focus, Focus::Session);
        }
        let mut h = folders_harness();
        h.app.set_layout(layout);
        if layout == SessionLayout::Sidebar {
            h.press(KeyCode::Down).await;
            assert_eq!(h.app.list.selection(), &Selection::NewSession);
            h.press(KeyCode::Enter).await;
        } else {
            h.press(KeyCode::Char('n')).await;
        }
        let create = h.fake.creates().pop().unwrap();
        assert_eq!(create.scope, Some(folder("/web")));
        assert_eq!(create.cwd.as_deref(), Some(folder("/web").path()));
    }
}

#[tokio::test]
async fn clicking_new_session_keeps_the_clicked_sections_folder() {
    let mut h = folders_harness();
    draw(&mut h);
    let area = list_row(&h, "A1");
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        h.app.handle_mouse(mouse(kind, area.x + 3, area.y + 1));
        h.app.process_pending();
    }
    assert_eq!(h.app.current_folder(), folder("/api"));
    // The sidebar is clickable while an existing session has input focus.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    draw(&mut h);
    let row = h.app.list.row_count() - 1;
    let area = h
        .app
        .hits
        .iter()
        .find(|(_, hit)| *hit == Hit::ListRow(row))
        .map(|(area, _)| *area)
        .unwrap();
    h.app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x + 3,
        area.y + 1,
    ));
    h.app.process_pending();
    assert_eq!(h.fake.creates().len(), 1);
    assert_eq!(h.selected().as_deref(), Some("N1"));
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(created_folder(&h), folder("/api"));
    let create = h.fake.creates().pop().unwrap();
    assert_eq!(create.scope, Some(folder("/api")));
    assert_eq!(create.cwd.as_deref(), Some(folder("/api").path()));
}

#[tokio::test]
async fn empty_folder_header_sets_new_session_target_without_changing_the_preview() {
    let mut h = folders_harness();
    let empty = folder("/empty");
    h.app.show_folders(vec![folder("/web"), empty.clone()]);
    let shown = h.app.detail.session_id().cloned();
    h.press(KeyCode::Home).await;
    h.press(KeyCode::Down).await;
    assert_eq!(
        h.app.list.selection(),
        &Selection::FolderHeader(empty.clone())
    );
    assert_eq!(h.app.detail.session_id(), shown.as_ref());
    h.press(KeyCode::End).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(created_folder(&h), empty);
    let create = h.fake.creates().pop().unwrap();
    assert_eq!(create.scope, Some(empty.clone()));
    assert_eq!(create.cwd.as_deref(), Some(empty.path()));
}

#[tokio::test]
async fn the_session_pane_keeps_only_a_session_the_filter_shows_in_view() {
    let mut h = folders_harness();
    // Work in web-one, then back to Rakumux mode on the folder filter.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.detail.view, DetailView::Live(id("W1")));
    h.rakumux().await;
    assert_eq!(h.app.came_from, Some(id("W1")));
    h.press(KeyCode::Home).await;
    assert_eq!(h.app.detail.view.session_id(), Some(&id("W1")));
    // Showing /api: the pane moves to a session it shows.
    h.app.show_folders(vec![folder("/api")]);
    assert_eq!(h.app.detail.view.session_id(), Some(&id("A1")));
    h.press(KeyCode::Home).await;
    assert_eq!(h.app.detail.view.session_id(), Some(&id("A1")));
    // A folder without sessions: nothing in view.
    h.app.show_folders(vec![folder("/empty")]);
    assert_eq!(h.app.detail.view, DetailView::Empty);
    // Esc goes back to web-one, and shows its folder again.
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.selected().as_deref(), Some("W1"));
    assert_eq!(
        h.app.list.folder_filter(),
        [folder("/empty"), folder("/web")]
    );
}

#[tokio::test]
async fn clicks_open_the_modal_pick_rows_and_close_it() {
    let mut h = folders_harness();
    let backend = ratatui::backend::TestBackend::new(160, 48);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let click = |area: ratatui::layout::Rect| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 1,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    };
    let target = |h: &Harness, wanted: Hit| {
        h.app
            .hits
            .iter()
            .find(|(_, hit)| *hit == wanted)
            .map(|(area, _)| *area)
            .unwrap()
    };
    h.app.handle_mouse(click(target(&h, Hit::FolderSelector)));
    h.app.process_pending();
    assert!(matches!(h.app.mode, AppMode::Folders(_)));
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    // Outside the modal: closed, nothing applied.
    h.app
        .handle_mouse(click(ratatui::layout::Rect::new(1, 46, 1, 1)));
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.list.folder_filter().is_empty());
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    // A click on a folder shows it alone (rows: All, /repo, /api, /web).
    h.app.handle_mouse(click(target(&h, Hit::FolderSelector)));
    h.app.process_pending();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    h.app.handle_mouse(click(target(&h, Hit::FolderRow(2))));
    h.app.process_pending();
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.list.folder_filter(), [folder("/api")]);
    // A click on All folders clears the filter.
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    h.app.handle_mouse(click(target(&h, Hit::FolderSelector)));
    h.app.process_pending();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    h.app.handle_mouse(click(target(&h, Hit::FolderRow(0))));
    h.app.process_pending();
    assert!(h.app.list.folder_filter().is_empty());
}

// ---------------------------------------------------------------- list tabs

/// `/repo` (launch): Claude Code working and waiting for you, and a shell;
/// `/web`: Codex with an unread reply, the most recent.
fn agents_harness() -> Harness {
    let agent = |id: &str, kind: &str, lifecycle: Lifecycle, at: i64| {
        let mut session = view(id, &id.to_lowercase(), at);
        session.agent = Some(session_core::model::AgentInfo {
            kind: kind.into(),
            ..Default::default()
        });
        session.lifecycle = lifecycle;
        session
    };
    let mut codex = agent("W1", "codex", Lifecycle::Done, 40);
    codex.scope = folder("/web");
    codex.cwd = "/web".into();
    harness_with_folders(
        vec![
            agent("R1", "claude", Lifecycle::Working, 10),
            agent("R2", "claude", Lifecycle::Blocked, 20),
            view("R3", "r3", 30),
            codex,
        ],
        &[("/web", on_branch("main"))],
        SessionLayout::Sidebar,
    )
}

fn section_picker(h: &Harness) -> &types::SectionPicker {
    match &h.app.mode {
        AppMode::Sections(picker) => picker,
        _ => panic!("the App or Status modal is closed"),
    }
}

/// The App or Status modal's rows: `all` or a name, after `[x]` when
/// ticked, and `>` before the one under the cursor.
fn section_rows(h: &Harness) -> Vec<String> {
    let picker = section_picker(h);
    picker
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let name = match row {
                types::SectionRow::All => "all",
                types::SectionRow::Section(section) => section.label(),
            };
            let tick = if picker.is_ticked(row) { "[x]" } else { "[ ]" };
            let cursor = if index == picker.cursor { ">" } else { " " };
            format!("{cursor}{tick} {name}")
        })
        .collect()
}

fn app_section(name: &str) -> grouping::Section {
    grouping::Section::App(grouping::AppKey::new(name))
}

#[tokio::test]
async fn every_start_opens_the_status_tab_on_the_most_recent_session() {
    use grouping::Grouping;
    let mut h = harness(vec![view("A", "a", 10), view("B", "b", 20)]);
    assert_eq!(h.app.list.grouping(), Grouping::Status);
    assert_eq!(h.selected().as_deref(), Some("B"));
    // The tab isn't saved: after another was picked, the next start (a
    // restart or a UI reload) is under Status again.
    h.app.show_tab(Grouping::App);
    shift(&mut h, 'K').await;
    let Harness { fake, home, .. } = h;
    let mut options = options(&fake, scope(), &home);
    options.settings = Settings::load_from(&home.path().join("config.json")).unwrap();
    let h = start(options, fake, home);
    assert_eq!(h.app.list.grouping(), Grouping::Status);
    assert_eq!(h.selected().as_deref(), Some("B"));
}

#[tokio::test]
async fn arrows_on_the_top_row_switch_tabs_and_enter_opens_the_active_ones_modal() {
    use grouping::Grouping;
    let mut h = agents_harness();
    assert_eq!(h.app.list.grouping(), Grouping::Folder);
    h.press(KeyCode::Home).await;
    assert_eq!(h.app.list.selection(), &session_list::Selection::Folders);
    // → and l go along the tabs (Status, Folders, App) and stop at the
    // last; the top row stays selected; ← and h come back.
    h.press(KeyCode::Right).await;
    assert_eq!(h.app.list.grouping(), Grouping::App);
    assert_eq!(h.app.list.selection(), &session_list::Selection::Folders);
    assert!(h.app.list.is_grouped());
    h.press(KeyCode::Char('l')).await;
    assert_eq!(h.app.list.grouping(), Grouping::App);
    h.press(KeyCode::Left).await;
    h.press(KeyCode::Char('h')).await;
    assert_eq!(h.app.list.grouping(), Grouping::Status);
    h.press(KeyCode::Left).await;
    assert_eq!(h.app.list.grouping(), Grouping::Status);
    h.press(KeyCode::Right).await;
    h.press(KeyCode::Right).await;
    assert_eq!(h.app.list.grouping(), Grouping::App);
    // Enter: the active tab's modal; Esc leaves everything as it was.
    h.press(KeyCode::Enter).await;
    assert_eq!(section_picker(&h).grouping, Grouping::App);
    assert_eq!(
        section_rows(&h),
        [">[x] all", " [ ] claude", " [ ] codex", " [ ] shell"]
    );
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.app.list.grouping(), Grouping::App);
    assert!(h.app.list.section_filter(Grouping::App).is_empty());
    // All folders' modal is the Folders modal.
    h.press(KeyCode::Char('h')).await;
    assert_eq!(h.app.list.grouping(), Grouping::Folder);
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Folders(_)));
    h.press(KeyCode::Esc).await;
    // Off the top row ← and → switch tabs too, the session staying
    // selected; Enter (or `l`) goes in.
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Down).await;
    let selected = h.app.list.selected_id().cloned().expect("a session");
    h.press(KeyCode::Left).await;
    assert_eq!(h.app.list.grouping(), Grouping::Status);
    h.press(KeyCode::Right).await;
    assert_eq!(h.app.list.grouping(), Grouping::Folder);
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(h.app.list.selected_id(), Some(&selected));
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
}

#[tokio::test]
async fn the_app_modal_ticks_several_enter_shows_them_and_esc_changes_nothing() {
    use grouping::Grouping;
    let mut h = agents_harness();
    h.app.show_tab(Grouping::App);
    h.app.list.select(session_list::Selection::Folders);
    h.press(KeyCode::Enter).await;
    // Tab and Space tick; Esc forgets them.
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Tab).await;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Char(' ')).await;
    assert_eq!(
        section_rows(&h),
        [" [ ] all", " [x] claude", ">[x] codex", " [ ] shell"]
    );
    h.press(KeyCode::Esc).await;
    assert!(h.app.list.section_filter(Grouping::App).is_empty());
    assert_eq!(visible_ids(&h), ["R1", "R2", "W1", "R3"]);
    // Enter after ticking: the ticked ones and the one under the cursor.
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Char(' ')).await;
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(
        h.app.list.section_filter(Grouping::App),
        [app_section("claude"), app_section("codex")]
    );
    assert_eq!(visible_ids(&h), ["R1", "R2", "W1"]);
    assert_eq!(h.app.tab_label(Grouping::App), "2 apps");
    assert_eq!(h.app.status(), Some("Showing 2 apps"));
    // Opened again: ticked like the filter, the cursor on its first app.
    h.app.list.select(session_list::Selection::Folders);
    h.press(KeyCode::Enter).await;
    assert_eq!(
        section_rows(&h),
        [" [ ] all", ">[x] claude", " [x] codex", " [ ] shell"]
    );
    // Enter on an app without ticking: it alone.
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(
        h.app.list.section_filter(Grouping::App),
        [app_section("codex")]
    );
    assert_eq!(h.app.tab_label(Grouping::App), "codex");
    assert_eq!(visible_ids(&h), ["W1"]);
    // All apps clears it.
    h.app.list.select(session_list::Selection::Folders);
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::PageUp).await;
    assert_eq!(section_rows(&h)[0], ">[ ] all");
    h.press(KeyCode::Enter).await;
    assert!(h.app.list.section_filter(Grouping::App).is_empty());
    assert_eq!(h.app.tab_label(Grouping::App), "App");
}

#[tokio::test]
async fn typing_narrows_the_status_modal_and_space_types_once_something_is_typed() {
    use grouping::{Grouping, Section, StatusGroup};
    let mut h = agents_harness();
    h.app.show_tab(Grouping::Status);
    h.app.list.select(session_list::Selection::Folders);
    h.press(KeyCode::Enter).await;
    assert_eq!(section_picker(&h).grouping, Grouping::Status);
    // Every status but unknown, sessions or not.
    assert_eq!(section_rows(&h).len(), 9);
    h.type_text("wait").await;
    assert_eq!(
        section_rows(&h),
        [">[ ] waiting for you", " [ ] waiting for response"]
    );
    // Something is typed: Space goes in the query.
    h.type_text("ing f").await;
    assert_eq!(section_picker(&h).query.value(), "waiting f");
    assert_eq!(section_rows(&h).len(), 2);
    h.type_text("or y").await;
    assert_eq!(section_rows(&h), [">[ ] waiting for you"]);
    h.app.handle_paste("ou");
    assert_eq!(section_rows(&h), [">[ ] waiting for you"]);
    h.press(KeyCode::Enter).await;
    assert_eq!(
        h.app.list.section_filter(Grouping::Status),
        [Section::Status(StatusGroup::WaitingForYou)]
    );
    assert_eq!(visible_ids(&h), ["R2"]);
    assert_eq!(h.app.tab_label(Grouping::Status), "waiting for you");
    // Nothing matches: Enter does nothing.
    h.app.list.select(session_list::Selection::Folders);
    h.press(KeyCode::Enter).await;
    h.type_text("zzz").await;
    assert!(section_rows(&h).is_empty());
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Sections(_)));
}

#[tokio::test]
async fn clicks_switch_tabs_open_the_active_ones_modal_and_pick_its_rows() {
    use grouping::Grouping;
    let mut h = agents_harness();
    let click = |h: &mut Harness, wanted: Hit| {
        draw(h);
        let area = h
            .app
            .hits
            .iter()
            .find(|(_, hit)| *hit == wanted)
            .map(|(area, _)| *area)
            .unwrap_or_else(|| panic!("no {wanted:?}"));
        let row = area.y + area.height / 2;
        h.app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 1,
            row,
        ));
        h.app.process_pending();
    };
    // From a session, a click on another tab: Rakumux mode, that tab.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    click(&mut h, Hit::ListTab(Grouping::App));
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(h.app.list.grouping(), Grouping::App);
    assert_eq!(h.app.list.selection(), &session_list::Selection::Folders);
    assert!(matches!(h.app.mode, AppMode::Normal));
    // On the active tab: its modal; a row picks it (All, claude, codex).
    click(&mut h, Hit::ListTab(Grouping::App));
    assert_eq!(section_picker(&h).grouping, Grouping::App);
    click(&mut h, Hit::SectionRow(2));
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(
        h.app.list.section_filter(Grouping::App),
        [app_section("codex")]
    );
    // Another modal, closed by a click outside it: nothing changes.
    click(&mut h, Hit::ListTab(Grouping::Status));
    click(&mut h, Hit::ListTab(Grouping::Status));
    assert_eq!(section_picker(&h).grouping, Grouping::Status);
    h.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 46));
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.list.section_filter(Grouping::Status).is_empty());
    // The band beside the tabs opens the active tab's modal too.
    click(&mut h, Hit::FolderSelector);
    assert_eq!(section_picker(&h).grouping, Grouping::Status);
    h.press(KeyCode::Esc).await;
    // All folders' tab, clicked twice: the Folders modal, as before.
    click(&mut h, Hit::ListTab(Grouping::Folder));
    assert_eq!(h.app.list.grouping(), Grouping::Folder);
    click(&mut h, Hit::ListTab(Grouping::Folder));
    assert!(matches!(h.app.mode, AppMode::Folders(_)));
}

#[tokio::test]
async fn the_folders_modal_from_another_tab_shows_folders_under_all_folders() {
    use grouping::Grouping;
    let mut h = agents_harness();
    h.app
        .show_sections(Grouping::App, vec![app_section("claude")]);
    assert_eq!(visible_ids(&h), ["R1", "R2"]);
    // `P` still opens the Folders modal; a folder picked there makes All
    // folders the active tab.
    h.press(KeyCode::Char('P')).await;
    cursor_to(&mut h, "/web");
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.list.grouping(), Grouping::Folder);
    assert_eq!(h.app.list.folder_filter(), [folder("/web")]);
    assert_eq!(visible_ids(&h), ["W1"]);
    assert_eq!(h.app.tab_label(Grouping::App), "App");
    // The app filter waited for its tab.
    h.app.show_tab(Grouping::App);
    assert_eq!(visible_ids(&h), ["R1", "R2"]);
    assert_eq!(h.app.tab_label(Grouping::App), "claude");
    assert_eq!(h.app.tab_label(Grouping::Folder), "Folders");
}

#[tokio::test]
async fn a_session_that_leaves_the_filter_stays_while_it_is_selected() {
    use grouping::{Grouping, Section, StatusGroup};
    let mut h = agents_harness();
    h.app.show_sections(
        Grouping::Status,
        vec![Section::Status(StatusGroup::WaitingForYou)],
    );
    assert_eq!(h.selected().as_deref(), Some("R2"));
    // Working in it: answering the question doesn't take it away.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    h.event(
        "R2",
        SessionEvent::Lifecycle {
            lifecycle: Lifecycle::Working,
        },
    );
    assert_eq!(h.selected().as_deref(), Some("R2"));
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.app.detail.session_id(), Some(&id("R2")));
    // Once something else is selected it goes.
    h.rakumux().await;
    h.press(KeyCode::Home).await;
    assert!(visible_ids(&h).is_empty());
    // A new session (a shell) the filter doesn't show is shown while
    // selected, in the current folder.
    h.press(KeyCode::Char('n')).await;
    let new_id = h.app.list.selected_id().cloned().unwrap();
    assert_eq!(created_folder(&h), scope());
    assert_eq!(visible_ids(&h), [new_id.to_string()]);
}

/// The welcome tour's card and box, or `None` when it isn't open.
fn welcome(h: &Harness) -> Option<(usize, bool)> {
    match h.app.mode {
        AppMode::Welcome(state) => Some((state.card, state.dont_show)),
        _ => None,
    }
}

#[tokio::test]
async fn the_welcome_tour_opens_from_help_and_can_stop_showing_on_startup() {
    let mut h = harness(vec![view("A", "a", 20)]);
    assert!(!h.app.settings.sessions.welcome_seen);
    // `t` in Help opens it from the first card.
    h.press(KeyCode::Char('?')).await;
    h.press(KeyCode::Char('t')).await;
    assert_eq!(welcome(&h), Some((0, false)));
    h.press(KeyCode::Right).await;
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::Left).await;
    assert_eq!(welcome(&h), Some((1, false)));
    // Space ticks the box on any card, and Esc there keeps it: you can opt
    // out without reading to the end.
    h.press(KeyCode::Char(' ')).await;
    assert_eq!(welcome(&h), Some((1, true)));
    h.press(KeyCode::Char(' ')).await;
    assert_eq!(welcome(&h), Some((1, false)));
    for _ in 0..10 {
        h.press(KeyCode::Right).await;
    }
    let last = crate::components::welcome::CARDS - 1;
    assert_eq!(welcome(&h), Some((last, false)));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        screen.contains("[ ] Don't show this on startup"),
        "{screen}"
    );
    h.press(KeyCode::Char(' ')).await;
    assert_eq!(welcome(&h), Some((last, true)));
    // Enter on the last card finishes and saves the box.
    h.press(KeyCode::Enter).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h.app.settings.sessions.welcome_seen);
    let saved = Settings::load_from(&h.home.path().join("config.json")).unwrap();
    assert!(saved.sessions.welcome_seen);

    // Opened again, the box starts ticked; Esc without changing it keeps it.
    h.app.open_welcome();
    assert_eq!(welcome(&h), Some((0, true)));
    h.press(KeyCode::Esc).await;
    assert!(h.app.settings.sessions.welcome_seen);
    // Unticking it shows the tour on startup again.
    h.app.open_welcome();
    for _ in 0..10 {
        h.press(KeyCode::Char('l')).await;
    }
    h.press(KeyCode::Char('d')).await;
    h.press(KeyCode::Esc).await;
    let saved = Settings::load_from(&h.home.path().join("config.json")).unwrap();
    assert!(!saved.sessions.welcome_seen);
}

/// Every card of the welcome tour, in order, drawn at 80×24: which known
/// title it shows and whether it fits (the opt-out box and the keys row
/// shown, nothing cut off).
async fn welcome_cards_at_80x24(h: &mut Harness) -> Vec<(String, bool)> {
    welcome_cards_at(h, 80, 24).await
}

/// [`welcome_cards_at_80x24`] at `width`×`height`. A card fits when both
/// borders show, its last words, the opt-out box and the keys.
async fn welcome_cards_at(h: &mut Harness, width: u16, height: u16) -> Vec<(String, bool)> {
    // The last word of each card's text.
    const ENDS: [&str; crate::components::welcome::CARDS] =
        ["minute.", "again.", "SESSION.", "one.", "key.", "again."];
    h.app.open_welcome();
    let mut cards = Vec::new();
    for last_word in ENDS {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
        let rows: Vec<String> = terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect();
        let title = rows
            .iter()
            .find_map(|row| {
                [
                    "Welcome to Rakumux",
                    "Folders and sessions",
                    "Two modes",
                    "Moving around",
                    "The main shortcuts",
                    "Help is always there",
                ]
                .into_iter()
                .find(|t| row.contains(t))
            })
            .unwrap_or_default()
            .to_string();
        // The popup grows to the tallest card (tighter when that doesn't
        // fit); both its borders show unless it's cut off.
        let top = rows.iter().any(|row| row.contains("╭ Rakumux tour"));
        let bottom = rows.iter().any(|row| row.contains("╰──"));
        let end = rows.iter().any(|row| row.contains(last_word));
        let keys = rows.iter().any(|row| row.contains("Esc close"));
        let opt_out = rows.iter().any(|row| row.contains("[ ] Don't show"));
        let footer = rows.last().is_some_and(|row| row.starts_with(" RAKUMUX "));
        let palette = crate::theme::palette();
        assert!((0..width).all(
            |column| terminal.backend().buffer().get(column, height - 1).bg
                == palette.accent_orange
        ));
        cards.push((title, keys && opt_out && top && bottom && end && footer));
        h.press(KeyCode::Right).await;
    }
    cards
}

#[tokio::test]
async fn the_welcome_tour_fits_a_phone() {
    let mut h = harness(vec![view("A", "a", 20)]);
    for (width, height) in [(45, 40), (40, 20), (40, 19)] {
        let cards = welcome_cards_at(&mut h, width, height).await;
        assert_eq!(cards.len(), crate::components::welcome::CARDS);
        assert!(
            cards.iter().all(|(title, fits)| !title.is_empty() && *fits),
            "{width}x{height}: {cards:?}"
        );
        h.press(KeyCode::Esc).await;
    }
}

#[tokio::test]
async fn the_welcome_tour_explains_folders_and_sessions_before_the_modes() {
    let mut h = harness(vec![view("A", "a", 20)]);
    let cards = welcome_cards_at_80x24(&mut h).await;
    let titles: Vec<&str> = cards.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Welcome to Rakumux",
            "Folders and sessions",
            "Two modes",
            "Moving around",
            "The main shortcuts",
            "Help is always there",
        ]
    );
    assert!(cards.iter().all(|(_, keys)| *keys), "{cards:?}");
}

#[tokio::test]
async fn the_welcome_tour_can_be_turned_off_from_its_first_card() {
    let mut h = harness(vec![view("A", "a", 20)]);
    h.app.open_welcome();
    h.press(KeyCode::Char(' ')).await;
    assert_eq!(welcome(&h), Some((0, true)));
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    let saved = Settings::load_from(&h.home.path().join("config.json")).unwrap();
    assert!(saved.sessions.welcome_seen);
}

#[tokio::test]
async fn closing_the_welcome_tour_early_shows_it_again_next_start() {
    let mut h = harness(vec![view("A", "a", 20)]);
    h.app.open_welcome();
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(!h.app.settings.sessions.welcome_seen);
}

#[tokio::test]
async fn settings_lists_and_runs_the_welcome_tour() {
    let mut h = harness(vec![view("A", "a", 20)]);
    let row = crate::help::KeyAction::ALL
        .iter()
        .position(|a| *a == crate::help::KeyAction::Tour)
        .unwrap();
    assert_eq!(crate::help::KeyAction::Tour.section(), "Help and app");
    h.press(KeyCode::Char(',')).await;
    assert_eq!(settings_tab(&h), Some(ModalTab::Settings));
    // The actions follow the settings.
    for _ in 0..SettingsItem::ALL.len() + row {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert_eq!(welcome(&h), Some((0, false)));
}

// ---------------------------------------------------------------- phone access

/// Started with a [`FakePhone`](super::testing::FakePhone) that offers phone
/// access (Tailscale here, never set up) unless `offer` is false.
fn phone_harness(offer: bool, seen: bool) -> (Harness, Arc<super::testing::FakePhone>) {
    let fake = FakeApi::new(vec![view("A", "a", 20)]);
    let dir = tempfile::tempdir().unwrap();
    let phone = Arc::new(super::testing::FakePhone {
        offer,
        ..Default::default()
    });
    let mut options = options(&fake, scope(), &dir);
    options.phone = phone.clone();
    options.settings.sessions.phone_offer_seen = seen;
    (start(options, fake, dir), phone)
}

fn pairing_state(h: &Harness) -> Option<super::types::PairingState> {
    match &h.app.mode {
        AppMode::Pairing(dialog) => Some(dialog.state.clone()),
        _ => None,
    }
}

/// The screen drawn at `width`×`height`, one string per row.
fn screen_rows(h: &mut Harness, width: u16, height: u16) -> Vec<String> {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|c| c.symbol()).collect())
        .collect()
}

#[tokio::test]
async fn phone_access_is_offered_once_and_pairs_with_a_qr_code() {
    let (mut h, phone) = phone_harness(true, false);
    assert!(matches!(h.app.mode, AppMode::PhoneOffer));
    assert_eq!(h.app.phone_enabled, Some(false));
    // Recorded as offered as it opens: it never comes back.
    let saved = Settings::load_from(&h.home.path().join("config.json")).unwrap();
    assert!(saved.sessions.phone_offer_seen);
    let rows = screen_rows(&mut h, 160, 48).join("\n");
    assert!(rows.contains("Use Rakumux from your phone?"), "{rows}");
    assert!(rows.contains("Turn on and pair"), "{rows}");

    h.press(KeyCode::Char('y')).await;
    assert_eq!(phone.calls(), vec!["worth_offering", "enable"]);
    assert_eq!(h.app.phone_enabled, Some(true));
    assert_eq!(
        pairing_state(&h),
        Some(super::types::PairingState::Ready(
            super::testing::FakePhone::pairing()
        ))
    );
    let rows = screen_rows(&mut h, 160, 48);
    let text = rows.join("\n");
    assert!(text.contains("Pair your phone"), "{text}");
    assert!(text.contains("http://box.tail1234.ts.net:7337"), "{text}");
    assert!(text.contains("abcdefghijklmnopqrstuvwx27"), "{text}");
    // The QR code of the link: its rows of `▀`, whole.
    let link = super::testing::FakePhone::pairing().link().unwrap();
    let code = crate::qr::Qr::encode(&link).unwrap();
    let code_rows: Vec<&String> = rows.iter().filter(|r| r.contains("▀▀▀▀▀▀▀▀▀▀")).collect();
    assert_eq!(code_rows.len(), usize::from(code.height()), "{text}");
    assert!(code_rows
        .iter()
        .all(|r| r.chars().filter(|c| *c == '▀').count() == usize::from(code.width())));

    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
}

#[tokio::test]
async fn the_phone_offer_waits_for_the_welcome_tour_and_session_mode() {
    let fake = FakeApi::new(vec![view("A", "a", 20)]);
    let dir = tempfile::tempdir().unwrap();
    let phone = Arc::new(super::testing::FakePhone {
        offer: true,
        ..Default::default()
    });
    let mut options = options(&fake, scope(), &dir);
    options.phone = phone.clone();
    let mut app = App::new(options);
    app.resize(160, 48);
    app.open_welcome();
    app.start();
    app.process_pending();
    let mut h = super::testing::Harness {
        app,
        fake,
        home: dir,
    };
    assert!(matches!(h.app.mode, AppMode::Welcome(_)));
    h.press(KeyCode::Esc).await;
    // In a session: keys go to the program, so no dialog takes them.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert!(!h.app.on_tick(std::time::Instant::now()));
    assert!(matches!(h.app.mode, AppMode::Normal));
    h.rakumux().await;
    assert!(h.app.on_tick(std::time::Instant::now()));
    assert!(matches!(h.app.mode, AppMode::PhoneOffer));
    // Declined: off, said where to turn it on, never offered again.
    h.press(KeyCode::Char('n')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(phone.calls().iter().all(|c| *c != "enable"));
    assert!(h.app.settings.sessions.phone_offer_seen);
    assert!(!h.app.on_tick(std::time::Instant::now()));
}

#[tokio::test]
async fn phone_access_is_not_offered_twice_or_without_tailscale() {
    let (h, phone) = phone_harness(true, true);
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(phone.calls().is_empty(), "not even checked once offered");
    let (h, phone) = phone_harness(false, false);
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(phone.calls(), vec!["worth_offering"]);
    assert!(!h.app.settings.sessions.phone_offer_seen);
}

#[tokio::test]
async fn the_pairing_dialog_shows_the_code_and_turns_phone_access_off() {
    let (mut h, phone) = phone_harness(false, false);
    // Settings has no phone row: `rakumux remote` and the offer cover it.
    let rows = h.app.modal_context(false).general;
    assert!(!rows.iter().any(|row| row.label.contains("Phone")));
    h.app.turn_phone_access_on();
    h.app.process_pending();
    assert!(matches!(
        pairing_state(&h),
        Some(super::types::PairingState::Ready(_))
    ));
    assert_eq!(h.app.phone_enabled, Some(true));
    h.press(KeyCode::Char('x')).await;
    assert_eq!(phone.calls().last(), Some(&"disable"));
    assert_eq!(h.app.phone_enabled, Some(false));
    assert!(matches!(h.app.mode, AppMode::Normal));
}

#[tokio::test]
async fn phone_access_errors_stay_in_the_dialog() {
    let (mut h, phone) = phone_harness(false, false);
    *phone.fail_enable.lock().unwrap() = Some("rakumux-api not found".into());
    h.app.turn_phone_access_on();
    h.app.process_pending();
    assert_eq!(
        pairing_state(&h),
        Some(super::types::PairingState::Failed(
            "rakumux-api not found".into()
        ))
    );
    let text = screen_rows(&mut h, 120, 40).join("\n");
    assert!(text.contains("✕ rakumux-api not found"), "{text}");
    assert_eq!(h.app.phone_enabled, Some(false));
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
}

#[tokio::test]
async fn a_small_window_shows_the_link_instead_of_the_code() {
    let (mut h, _phone) = phone_harness(true, false);
    h.press(KeyCode::Enter).await;
    let text = screen_rows(&mut h, 70, 24).join("\n");
    assert!(!text.contains('▀'), "{text}");
    assert!(text.contains("shows a QR code to scan"), "{text}");
    assert!(text.contains("rakumux://connect?url="), "{text}");
    assert!(text.contains("abcdefghijklmnopqrstuvwx27"), "{text}");
}

// ---------------------------------------------------------------- account

fn account_harness(account: testing::FakeAccount) -> (Harness, Arc<testing::FakeAccount>) {
    let fake = FakeApi::new(vec![view("A", "a", 20), view("B", "b", 10)]);
    let dir = tempfile::tempdir().unwrap();
    let account = Arc::new(account);
    let mut options = options(&fake, scope(), &dir);
    options.account = account.clone();
    (start(options, fake, dir), account)
}

fn account_state(h: &Harness) -> Option<super::types::AccountDialogState> {
    match &h.app.mode {
        AppMode::Account(dialog) => Some(dialog.state.clone()),
        _ => None,
    }
}

/// A tick far enough ahead for every account timer.
fn account_tick(h: &mut Harness) {
    h.app
        .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(120));
    h.app.process_pending();
}

#[tokio::test]
async fn signing_in_from_settings_shows_a_code_then_descriptions_under_sessions() {
    let (mut h, account) = account_harness(testing::FakeAccount::default());
    assert_eq!(h.app.account, super::types::AccountStatus::SignedOut);
    assert_eq!(account.calls(), ["account"]);
    // Signed out: no descriptions are asked for.
    account_tick(&mut h);
    assert!(!account.calls().contains(&"descriptions"));

    h.press(KeyCode::Char('o')).await;
    for _ in 0..SettingsItem::Account.index() {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert_eq!(
        account_state(&h),
        Some(super::types::AccountDialogState::SignedOut)
    );
    h.press(KeyCode::Enter).await;
    assert!(matches!(
        account_state(&h),
        Some(super::types::AccountDialogState::Waiting(_))
    ));
    let rows = screen_rows(&mut h, 160, 48).join("\n");
    assert!(rows.contains("WDJB-MJHT"), "{rows}");
    assert!(
        rows.contains("https://site.example/connect?code=WDJB-MJHT"),
        "{rows}"
    );
    assert!(rows.contains("▀▀▀▀▀▀▀▀▀▀"), "{rows}");

    // Not approved yet: still waiting.
    account_tick(&mut h);
    assert!(matches!(
        account_state(&h),
        Some(super::types::AccountDialogState::Waiting(_))
    ));
    account.approve();
    account_tick(&mut h);
    assert_eq!(
        account_state(&h),
        Some(super::types::AccountDialogState::SignedIn(
            testing::FakeAccount::person()
        ))
    );
    assert_eq!(
        h.app.account,
        super::types::AccountStatus::SignedIn(testing::FakeAccount::person())
    );
    let rows = screen_rows(&mut h, 160, 48).join("\n");
    assert!(rows.contains("Signed in as person@example.com."), "{rows}");
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));

    account.descriptions.lock().unwrap().insert(
        SessionId::from_string("A"),
        "Fixing the flaky login test".into(),
    );
    account_tick(&mut h);
    assert_eq!(
        h.app.description(&SessionId::from_string("A")),
        Some("Fixing the flaky login test")
    );
    assert_eq!(h.app.description(&SessionId::from_string("B")), None);
    let rows = screen_rows(&mut h, 160, 48).join("\n");
    assert!(rows.contains("Fixing the flaky login test"), "{rows}");
}

#[tokio::test]
async fn closing_the_dialog_abandons_the_sign_in() {
    let (mut h, account) = account_harness(testing::FakeAccount::default());
    h.app.open_account_dialog();
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(account.calls().contains(&"cancel_sign_in"));
    account.approve();
    account_tick(&mut h);
    assert!(!account.calls().contains(&"sign_in_status"));
}

#[tokio::test]
async fn signing_out_stops_the_descriptions() {
    let (mut h, account) = account_harness(testing::FakeAccount::signed_in());
    account
        .descriptions
        .lock()
        .unwrap()
        .insert(SessionId::from_string("A"), "Reviewing the parser".into());
    account_tick(&mut h);
    assert_eq!(
        h.app.description(&SessionId::from_string("A")),
        Some("Reviewing the parser")
    );
    h.app.open_account_dialog();
    h.press(KeyCode::Char('o')).await;
    assert!(account.calls().contains(&"sign_out"));
    assert_eq!(h.app.account, super::types::AccountStatus::SignedOut);
    assert_eq!(
        account_state(&h),
        Some(super::types::AccountDialogState::SignedOut)
    );
    assert_eq!(h.app.description(&SessionId::from_string("A")), None);
}

#[tokio::test]
async fn signed_out_the_footer_offers_sign_in_and_a_tap_starts_it() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let footer = |h: &Harness| -> String {
        crate::components::footer::footer_line(&h.app, 120)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    };
    let (mut h, account) = account_harness(testing::FakeAccount::default());
    assert!(
        footer(&h).ends_with(" , Settings   a Sign in  "),
        "{:?}",
        footer(&h)
    );
    // Drawn at the bottom right, and a tap there asks for a code at once.
    screen_rows(&mut h, 160, 48);
    let (area, _) = *h
        .app
        .hits
        .iter()
        .find(|(_, hit)| *hit == Hit::FooterKey(FooterKey::SignIn))
        .expect("the Sign in button");
    assert_eq!(area.y, 47);
    assert_eq!(area.x + area.width, 159);
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        h.app.handle_mouse(MouseEvent {
            kind,
            column: area.x + 1,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        });
    }
    h.app.process_pending();
    assert!(account.calls().contains(&"sign_in"));
    assert!(matches!(
        account_state(&h),
        Some(super::types::AccountDialogState::Waiting(_))
    ));
    // Signed in (or not yet checked) there's no button.
    let (h, _) = account_harness(testing::FakeAccount::signed_in());
    assert!(
        footer(&h).ends_with(" ? Help   , Settings  "),
        "{:?}",
        footer(&h)
    );
}

#[tokio::test]
async fn hovering_a_session_shows_a_pin_that_a_click_toggles() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mouse = |h: &mut Harness, kind, (column, row): (u16, u16)| {
        h.app.handle_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
        h.app.process_pending();
    };
    let target = |h: &Harness, wanted: Hit| -> Option<(u16, u16)> {
        h.app
            .hits
            .iter()
            .find(|(_, hit)| *hit == wanted)
            .map(|(area, _)| (area.x + 1, area.y + area.height - 1))
    };
    let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]).on_folders_tab();
    let selected = h.app.list.selection().clone();
    screen_rows(&mut h, 160, 48);
    let row = h
        .app
        .list
        .rows()
        .iter()
        .position(|row| matches!(row, ListRow::Session(s) if s.name == "b"))
        .unwrap();
    assert_eq!(target(&h, Hit::PinToggle(row)), None);
    // The pointer over B's row: its pin shows left of its name.
    let over = target(&h, Hit::ListRow(row)).unwrap();
    mouse(&mut h, MouseEventKind::Moved, over);
    assert_eq!(h.app.hovered_row, Some(row));
    let screen = screen_rows(&mut h, 160, 48);
    let pin = target(&h, Hit::PinToggle(row)).expect("the pin");
    assert!(
        screen[usize::from(pin.1)].starts_with("   ☆ "),
        "{:?}",
        screen[usize::from(pin.1)]
    );
    // A click on it pins B without selecting it.
    mouse(&mut h, MouseEventKind::Down(MouseButton::Left), pin);
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), pin);
    assert!(h.fake.calls().contains(&"pin B true".to_string()));
    assert_eq!(h.app.list.selection(), &selected);
    // Pinned, it moves; hovered again, its pin is the gold one, which unpins.
    screen_rows(&mut h, 160, 48);
    let row = h
        .app
        .list
        .rows()
        .iter()
        .position(|row| matches!(row, ListRow::Session(s) if s.name == "b"))
        .unwrap();
    let over = target(&h, Hit::ListRow(row)).unwrap();
    mouse(&mut h, MouseEventKind::Moved, over);
    let screen = screen_rows(&mut h, 160, 48);
    let pin = target(&h, Hit::PinToggle(row)).expect("the pin");
    assert!(screen[usize::from(pin.1)].starts_with("   ★ "));
    mouse(&mut h, MouseEventKind::Down(MouseButton::Left), pin);
    assert!(h.fake.calls().contains(&"pin B false".to_string()));
    // Off the list, no pin shows.
    mouse(&mut h, MouseEventKind::Moved, (100, 2));
    assert_eq!(h.app.hovered_row, None);
    screen_rows(&mut h, 160, 48);
    assert_eq!(target(&h, Hit::PinToggle(row)), None);

    // The pin is in the column of done's `★ pinned` header's star.
    let now = crate::components::format::now_secs();
    let done = |id: &str, pinned: bool| {
        let mut session = view(id, &id.to_lowercase(), now - 30);
        session.lifecycle = Lifecycle::Idle;
        session.pinned = pinned;
        session
    };
    let mut h = harness(vec![done("A", true), done("B", false)]);
    screen_rows(&mut h, 160, 48);
    let rows = h.app.list.rows();
    let header = rows
        .iter()
        .position(|row| matches!(row, ListRow::ReadPeriodHeader(_)))
        .unwrap();
    let row = rows
        .iter()
        .position(|row| matches!(row, ListRow::Session(s) if s.name == "b"))
        .unwrap();
    let over = target(&h, Hit::ListRow(row)).unwrap();
    mouse(&mut h, MouseEventKind::Moved, over);
    let screen = screen_rows(&mut h, 160, 48);
    let pin = target(&h, Hit::PinToggle(row)).expect("the pin");
    let header_line = target(&h, Hit::ListRow(header)).unwrap().1;
    let column = |line: &str, mark: char| line.chars().position(|c| c == mark);
    assert!(screen[usize::from(header_line)].contains("★ pinned"));
    assert_eq!(
        column(&screen[usize::from(pin.1)], '☆'),
        column(&screen[usize::from(header_line)], '★')
    );
}

#[tokio::test]
async fn a_signs_in_and_c_copies_the_sign_in_link() {
    use base64::Engine;
    let (mut h, account) = account_harness(testing::FakeAccount::default());
    h.press(KeyCode::Char('a')).await;
    assert!(account.calls().contains(&"sign_in"));
    assert!(matches!(
        account_state(&h),
        Some(super::types::AccountDialogState::Waiting(_))
    ));
    let rows = screen_rows(&mut h, 160, 48).join("\n");
    assert!(rows.contains("Copy link"), "{rows}");
    h.app.take_terminal_output();
    h.press(KeyCode::Char('c')).await;
    let link = base64::engine::general_purpose::STANDARD
        .encode("https://site.example/connect?code=WDJB-MJHT");
    assert_eq!(
        String::from_utf8(h.app.take_terminal_output()).unwrap(),
        format!("\x1b]52;c;{link}\x07")
    );
    assert_eq!(h.app.status(), Some("Copied the sign-in link."));
    // Signed in, `a` shows the account.
    let (mut h, account) = account_harness(testing::FakeAccount::signed_in());
    h.press(KeyCode::Char('a')).await;
    assert!(!account.calls().contains(&"sign_in"));
    assert!(matches!(
        account_state(&h),
        Some(super::types::AccountDialogState::SignedIn(_))
    ));
}
