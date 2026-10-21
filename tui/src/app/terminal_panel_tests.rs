use super::*;
use crate::app::testing::{harness, view, Harness};
use crossterm::event::{Event, KeyModifiers, MouseButton};
use ratatui::{backend::TestBackend, Terminal};
use session_core::client::AttachOutgoing;

fn pane_index(h: &Harness) -> usize {
    h.fake.pane_count() - 1
}

fn render(h: &mut Harness, width: u16, height: u16) -> String {
    h.app.resize(width, height);
    h.app.process_pending();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut h.app))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[tokio::test]
async fn terminal_panel_opens_in_the_current_directory_without_creating_a_session() {
    let mut agent = view("A", "agent", 1);
    agent.cwd = "/repo/src".into();
    let mut h = harness(vec![agent]);
    let main = h.last_attach().0;
    let selected = h.app.list.selected_id().cloned();
    h.fake.outgoing(main);
    h.press(KeyCode::Char('T')).await;
    assert_eq!(h.app.focus, Focus::Terminal);
    assert!(h.app.terminal_panel.open);
    assert_eq!(h.app.list.selected_id(), selected.as_ref());
    let state = h.fake.state.lock().unwrap();
    assert!(state.creates.is_empty());
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.terminal_opens.len(), 1);
    assert_eq!(state.terminal_opens[0].cwd, PathBuf::from("/repo/src"));
    drop(state);
    let panel = pane_index(&h);
    h.fake.pane_outgoing(panel);
    h.press(KeyCode::Char('x')).await;
    h.app
        .handle_terminal_event(Event::Paste("echo private".into()))
        .await;
    assert_eq!(
        h.fake.pane_outgoing(panel),
        vec![
            AttachOutgoing::Input(b"x".to_vec()),
            AttachOutgoing::Input(b"echo private".to_vec())
        ]
    );
    assert!(
        h.fake.outgoing(main).is_empty(),
        "pane input reached the agent"
    );
    assert!(h.app.input_is_private());
    assert!(h.app.input_state().starts_with("terminal "));
    assert!(render(&mut h, 160, 48).contains("Terminal"));
}

#[tokio::test]
async fn shell_exit_closes_the_pane_and_leaves_no_session_to_delete() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    let before = h.app.screen();
    assert!(before.terminal_panel.is_some());
    h.fake.pane_outgoing(panel);
    h.fake.send_pane(
        panel,
        AttachEvent::Event(StreamEvent::Exited { code: Some(0) }),
    );
    h.app.process_pending();
    assert!(!h.app.terminal_panel.open);
    assert!(h.app.terminal_panel.id.is_none());
    assert!(h.app.terminal_panel.terminal.is_none());
    assert!(h.app.screen().terminal_panel.is_none());
    assert_eq!(h.app.focus, Focus::List);
    let state = h.fake.state.lock().unwrap();
    assert!(state.panes.is_empty());
    assert!(state.creates.is_empty());
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].id.as_str(), "A");
    drop(state);
    assert!(!h
        .fake
        .calls()
        .iter()
        .any(|call| call.starts_with("delete ") || call.starts_with("kill ")));
    assert!(h
        .fake
        .pane_outgoing(panel)
        .contains(&AttachOutgoing::Command(StreamCommand::Detach)));
}

#[tokio::test]
async fn hiding_retains_the_stream_and_reopening_reuses_the_same_terminal() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let id = h.app.terminal_panel.id.clone();
    let panel = pane_index(&h);
    h.fake.pane_outgoing(panel);
    h.rakumux().await;
    h.press(KeyCode::Char('T')).await;
    assert!(!h.app.terminal_panel.open);
    assert!(h.app.terminal_panel.attachment.is_some());
    let outgoing = h.fake.pane_outgoing(panel);
    assert!(outgoing.contains(&AttachOutgoing::Command(StreamCommand::Demote)));
    assert!(!outgoing.contains(&AttachOutgoing::Command(StreamCommand::Detach)));
    h.press(KeyCode::Char('T')).await;
    assert_eq!(h.app.terminal_panel.id, id);
    assert_eq!(h.fake.state.lock().unwrap().terminal_opens.len(), 1);
    assert_eq!(h.fake.pane_count(), 1);
    assert_eq!(h.app.focus, Focus::Terminal);
}

#[tokio::test]
async fn a_hidden_shell_exit_is_cleaned_up_and_the_next_open_uses_a_fresh_id() {
    let mut h = harness(vec![]);
    h.press(KeyCode::Char('T')).await;
    let first = h.app.terminal_panel.id.clone();
    let panel = pane_index(&h);
    h.rakumux().await;
    h.press(KeyCode::Char('T')).await;
    h.fake.send_pane(
        panel,
        AttachEvent::Event(StreamEvent::Exited { code: Some(7) }),
    );
    h.app.process_pending();
    assert!(h.app.terminal_panel.id.is_none());
    h.press(KeyCode::Char('T')).await;
    assert_ne!(h.app.terminal_panel.id, first);
    assert!(h.fake.creates().is_empty());
    assert!(h.app.list.visible().is_empty());
    assert!(h.app.detail.session_id().is_none());
}

#[tokio::test]
async fn hiding_during_open_keeps_one_request_and_releases_hidden_input_focus() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.app.toggle_terminal_panel();
    assert!(h.app.terminal_panel.busy());
    h.app.hide_terminal_panel();
    h.app.process_pending();
    assert_eq!(h.fake.state.lock().unwrap().terminal_opens.len(), 1);
    assert!(!h.app.terminal_panel.open);
    assert_eq!(
        h.app.terminal_panel.attachment.as_ref().unwrap().role,
        Role::Observer
    );
    h.app.toggle_terminal_panel();
    h.app.process_pending();
    assert_eq!(h.fake.pane_count(), 1);
}

#[tokio::test]
async fn old_exit_events_cannot_close_a_new_pane() {
    let mut h = harness(vec![]);
    h.press(KeyCode::Char('T')).await;
    let old = h.app.terminal_panel.epoch;
    h.fake
        .send_pane(0, AttachEvent::Event(StreamEvent::Exited { code: Some(0) }));
    h.app.process_pending();
    h.press(KeyCode::Char('T')).await;
    let current = h.app.terminal_panel.id.clone();
    h.app.on_panel_attach_event(
        old,
        AttachEvent::Event(StreamEvent::Exited { code: Some(0) }),
    );
    assert!(h.app.terminal_panel.open);
    assert_eq!(h.app.terminal_panel.id, current);
}

#[tokio::test]
async fn unsupported_daemons_do_not_fall_back_to_session_creation() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.fake.fail_next(
        "open_terminal",
        ApiError::new(ErrorCode::Unsupported, "old daemon"),
    );
    h.press(KeyCode::Char('T')).await;
    assert!(h.fake.creates().is_empty());
    assert!(h
        .app
        .terminal_panel
        .message
        .as_ref()
        .unwrap()
        .contains("in-place restart"));
    assert!(h.app.input_is_private());
    assert_eq!(h.fake.pane_count(), 0);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.fake.pane_count(), 1);
    assert!(h.fake.creates().is_empty());
}

#[tokio::test]
async fn reconnect_only_reattaches_the_existing_pane_as_an_observer() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let id = h.app.terminal_panel.id.clone();
    h.app.terminal_panel_disconnected();
    h.app.sync_terminal_panel();
    h.app.process_pending();
    assert_eq!(h.app.terminal_panel.id, id);
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(
        h.app.terminal_panel.attachment.as_ref().unwrap().role,
        Role::Observer
    );
    let state = h.fake.state.lock().unwrap();
    assert_eq!(state.terminal_opens.len(), 1);
    assert_eq!(state.terminal_attaches.len(), 1);
    assert_eq!(state.terminal_attaches[0].role, Role::Observer);
    assert!(state.creates.is_empty());
    drop(state);
    assert!(h.app.input_is_private());
    h.press(KeyCode::Char('T')).await;
    assert_eq!(h.app.focus, Focus::Terminal);
    assert_eq!(
        h.app.terminal_panel.attachment.as_ref().unwrap().role,
        Role::Controller
    );
}

#[tokio::test]
async fn a_pane_lost_during_disconnect_closes_without_implicitly_starting_another() {
    let mut h = harness(vec![]);
    h.press(KeyCode::Char('T')).await;
    h.fake.state.lock().unwrap().panes.clear();
    h.app.terminal_panel_disconnected();
    h.app.sync_terminal_panel();
    h.app.process_pending();
    assert!(!h.app.terminal_panel.open);
    assert!(h.app.terminal_panel.id.is_none());
    assert_eq!(h.fake.state.lock().unwrap().terminal_opens.len(), 1);
    assert!(h.fake.creates().is_empty());
}

#[tokio::test]
async fn lag_requests_a_passive_checkpoint_and_pauses_input_until_it_arrives() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.fake.pane_outgoing(panel);
    h.fake.send_pane(
        panel,
        AttachEvent::Output {
            offset: 10,
            bytes: b"missing".to_vec(),
        },
    );
    h.app.process_pending();
    h.press(KeyCode::Char('x')).await;
    assert_eq!(
        h.fake.pane_outgoing(panel),
        vec![AttachOutgoing::Command(StreamCommand::Checkpoint)]
    );
    h.fake.send_pane(
        panel,
        AttachEvent::Event(StreamEvent::Resized {
            rows: 12,
            cols: 80,
            checkpoint: String::new(),
            offset: 17,
        }),
    );
    h.app.process_pending();
    assert!(!h.app.terminal_panel.awaiting_checkpoint);
    h.press(KeyCode::Char('x')).await;
    assert_eq!(
        h.fake.pane_outgoing(panel),
        vec![AttachOutgoing::Input(b"x".to_vec())]
    );
}

#[tokio::test]
async fn pane_focus_and_resize_do_not_claim_the_agents_size() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Enter).await;
    let main = h.last_attach().0;
    h.app.toggle_terminal_panel();
    h.app.process_pending();
    h.fake.outgoing(main);
    let panel = pane_index(&h);
    h.fake.pane_outgoing(panel);
    h.app.handle_terminal_event(Event::Resize(120, 30)).await;
    assert!(h.fake.outgoing(main).is_empty());
    assert!(h
        .fake
        .pane_outgoing(panel)
        .iter()
        .any(|event| matches!(event, AttachOutgoing::Command(StreamCommand::Resize { .. }))));
    assert_eq!(
        h.app.detail.attachment.as_ref().unwrap().role,
        Role::Observer
    );
    h.rakumux().await;
    let output = h.fake.pane_outgoing(panel);
    assert!(output.contains(&AttachOutgoing::Command(StreamCommand::Demote)));
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Terminal);
}

#[tokio::test]
async fn tiny_layouts_keep_the_pane_usable_and_exit_restores_the_main_geometry() {
    for layout in crate::layout::SessionLayout::ALL {
        for (width, height) in [(40, 10), (40, 20), (90, 24), (160, 48)] {
            let mut h = harness(vec![view("A", "agent", 1)]);
            h.app.set_layout(layout);
            render(&mut h, width, height);
            h.press(KeyCode::Char('T')).await;
            let panel = pane_index(&h);
            assert!(render(&mut h, width, height).contains("Terminal"));
            assert!(h.app.screen().terminal_panel.is_some());
            h.fake.send_pane(
                panel,
                AttachEvent::Event(StreamEvent::Exited { code: Some(0) }),
            );
            h.app.process_pending();
            assert!(h.app.screen().terminal_panel.is_none());
            assert!(h.fake.creates().is_empty());
        }
    }
}

#[tokio::test]
async fn hide_button_hides_without_detaching_and_hidden_output_keeps_updating() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    render(&mut h, 160, 48);
    let hide = h
        .app
        .hits
        .iter()
        .find_map(|(area, hit)| (*hit == crate::app::Hit::TerminalPanelHide).then_some(*area))
        .unwrap();
    h.fake.pane_outgoing(panel);
    h.app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hide.x,
        row: hide.y,
        modifiers: KeyModifiers::NONE,
    });
    h.app.process_pending();
    assert!(!h.app.terminal_panel.open);
    assert!(!h
        .fake
        .pane_outgoing(panel)
        .contains(&AttachOutgoing::Command(StreamCommand::Detach)));
    h.fake.send_pane(
        panel,
        AttachEvent::Output {
            offset: 0,
            bytes: b"hidden output".to_vec(),
        },
    );
    h.app.process_pending();
    assert!(h.app.terminal_panel.terminal.is_some());
    h.press(KeyCode::Char('T')).await;
    assert_eq!(h.fake.pane_count(), 1);
}

#[tokio::test]
async fn dropping_the_ui_detaches_the_pane_but_never_kills_a_session() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.fake.pane_outgoing(panel);
    let fake = h.fake.clone();
    drop(h);
    assert!(fake
        .pane_outgoing(panel)
        .contains(&AttachOutgoing::Command(StreamCommand::Detach)));
    assert!(!fake
        .calls()
        .iter()
        .any(|call| call.starts_with("kill ") || call.starts_with("delete ")));
}

#[tokio::test]
async fn an_unfocused_shell_exit_does_not_fit_or_resize_the_agent() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    let main = h.last_attach().0;
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.rakumux().await;
    h.fake.outgoing(main);
    h.fake.send_pane(
        panel,
        AttachEvent::Event(StreamEvent::Exited { code: Some(0) }),
    );
    h.app.process_pending();
    assert!(!h.app.terminal_panel.open);
    assert!(
        h.fake.outgoing(main).is_empty(),
        "background exit claimed the agent terminal's size"
    );
}

#[test]
fn terminal_pane_history_stays_within_its_cell_budget() {
    for (rows, cols) in [(2, 2), (30, 120), (200, 1000), (500, 1000)] {
        assert!((usize::from(rows) + panel_history(rows, cols)) * usize::from(cols) <= MAX_CELLS);
    }
    assert!(panel_terminal(0, 0, "").is_err());
    assert!(panel_terminal(1000, 1000, "").is_err());
    assert!(panel_terminal(24, 80, "not base64").is_err());
}
