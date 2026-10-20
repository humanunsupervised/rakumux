use super::*;
use crate::app::testing::{harness, scope, view, Harness};
use crossterm::event::{Event, KeyModifiers, MouseButton};
use ratatui::{backend::TestBackend, Terminal};
use session_core::client::AttachOutgoing;

fn pane_index(h: &Harness) -> usize {
    h.fake.attach_count() - 1
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
async fn terminal_panel_opens_an_independent_folder_shell_and_routes_private_input() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    let main = h.last_attach().0;
    let selected = h.app.list.selected_id().cloned();
    h.fake.outgoing(main);
    h.press(KeyCode::Char('T')).await;
    assert_eq!(h.app.focus, Focus::Terminal);
    assert!(h.app.terminal_panel.open);
    let id = h.app.terminal_panel.session_id().unwrap().clone();
    assert_ne!(Some(&id), selected.as_ref());
    assert_eq!(h.app.list.selected_id(), selected.as_ref());
    let created = h.fake.state.lock().unwrap().creates[0].clone();
    assert_eq!(created.scope, Some(scope()));
    assert_eq!(created.cwd.as_deref(), Some(scope().path()));
    assert!(created.argv.is_none() && created.startup_cmds.is_empty());
    let panel = pane_index(&h);
    assert_eq!(h.fake.attach_request(panel).role, Role::Controller);
    let screen = h.app.screen();
    assert!(screen.detail.unwrap().bottom() < screen.terminal_panel.unwrap().y);
    assert!(h.app.input_is_private());
    assert!(h.app.input_state().starts_with("terminal "));
    h.fake.outgoing(panel);
    h.press(KeyCode::Char('x')).await;
    h.app
        .handle_terminal_event(Event::Paste("echo private".into()))
        .await;
    assert_eq!(
        h.fake.outgoing(panel),
        vec![
            AttachOutgoing::Input(b"x".to_vec()),
            AttachOutgoing::Input(b"echo private".to_vec())
        ]
    );
    assert!(
        h.fake.outgoing(main).is_empty(),
        "panel input or sizing escaped into the agent"
    );
    assert!(render(&mut h, 160, 48).contains("Terminal"));
}

#[tokio::test]
async fn hiding_the_panel_preserves_its_session_and_reopening_reuses_it() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let id = h.app.terminal_panel.session_id().unwrap().clone();
    let panel = pane_index(&h);
    h.fake.outgoing(panel);
    h.rakumux().await;
    assert!(h.app.came_from_terminal);
    h.press(KeyCode::Char('T')).await;
    assert!(!h.app.terminal_panel.open);
    assert!(h
        .fake
        .state
        .lock()
        .unwrap()
        .sessions
        .iter()
        .any(|session| session.id == id && session.status == SessionStatus::Live));
    assert!(!h.fake.calls().iter().any(|call| call.starts_with("kill ")
        || call.starts_with("delete ")
        || call == "shutdown"));
    let output = h.fake.outgoing(panel);
    assert!(output.contains(&AttachOutgoing::Command(StreamCommand::Demote)));
    assert!(output.contains(&AttachOutgoing::Command(StreamCommand::Detach)));
    h.press(KeyCode::Char('T')).await;
    assert_eq!(h.app.terminal_panel.session_id(), Some(&id));
    assert_eq!(h.fake.state.lock().unwrap().creates.len(), 1);
    assert_eq!(h.app.focus, Focus::Terminal);
}

#[tokio::test]
async fn focus_moves_between_agent_panel_and_rakumux_without_typing_into_both() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Enter).await;
    let main = h.last_attach().0;
    h.fake.outgoing(main);
    h.app.toggle_terminal_panel();
    h.app.process_pending();
    let panel = pane_index(&h);
    assert_eq!(
        h.app.detail.attachment.as_ref().unwrap().role,
        Role::Observer
    );
    assert!(h
        .fake
        .outgoing(main)
        .contains(&AttachOutgoing::Command(StreamCommand::Demote)));
    h.rakumux().await;
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Terminal);
    h.rakumux().await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(
        h.app
            .terminal_panel
            .detail
            .attachment
            .as_ref()
            .unwrap()
            .role,
        Role::Observer
    );
    h.fake.outgoing(main);
    h.fake.outgoing(panel);
    h.press(KeyCode::Char('T')).await;
    assert_eq!(
        h.fake.outgoing(main),
        vec![AttachOutgoing::Input(b"T".to_vec())]
    );
    assert!(h.fake.outgoing(panel).is_empty());
}

#[tokio::test]
async fn another_client_taking_control_makes_the_panel_read_only() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.attach_event(
        panel,
        AttachEvent::Event(StreamEvent::RoleChanged {
            role: Role::Observer,
        }),
    );
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(
        h.app
            .terminal_panel
            .detail
            .attachment
            .as_ref()
            .unwrap()
            .role,
        Role::Observer
    );
    h.fake.outgoing(panel);
    h.app
        .handle_terminal_panel_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(h.fake.outgoing(panel).is_empty());
    h.app.focus_terminal_panel();
    assert_eq!(h.app.focus, Focus::Terminal);
    assert!(h.fake.outgoing(panel).iter().any(|message| matches!(
        message,
        AttachOutgoing::Command(StreamCommand::Promote { .. })
    )));
}

#[tokio::test]
async fn panel_output_and_queries_stay_in_the_panel_and_only_active_clipboard_is_forwarded() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    let main = h.last_attach().0;
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.fake.outgoing(main);
    h.fake.outgoing(panel);
    h.attach_event(
        panel,
        AttachEvent::Output {
            offset: 100,
            bytes: b"\x1b[HPANEL_OUTPUT\x1b[6n\x1b[c".to_vec(),
        },
    );
    assert!(render(&mut h, 160, 48).contains("PANEL_OUTPUT"));
    assert!(h.fake.outgoing(main).is_empty());
    assert!(
        h.fake.outgoing(panel).is_empty(),
        "passive mirrors must not answer queries"
    );
    h.attach_event(
        panel,
        AttachEvent::Event(StreamEvent::Clipboard {
            selection: "c".into(),
            data: "eA==".into(),
        }),
    );
    assert!(!h.app.take_terminal_output().is_empty());
    h.rakumux().await;
    h.attach_event(
        panel,
        AttachEvent::Event(StreamEvent::Clipboard {
            selection: "c".into(),
            data: "eQ==".into(),
        }),
    );
    assert!(h.app.take_terminal_output().is_empty());
}

#[tokio::test]
async fn panel_exit_keeps_its_screen_and_enter_revives_only_that_session() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let id = h.app.terminal_panel.session_id().unwrap().clone();
    let panel = pane_index(&h);
    h.attach_event(
        panel,
        AttachEvent::Output {
            offset: 100,
            bytes: b"\x1b[HFINAL_PANEL_OUTPUT".to_vec(),
        },
    );
    h.attach_event(
        panel,
        AttachEvent::Event(StreamEvent::Exited { code: Some(0) }),
    );
    h.attach_event(panel, AttachEvent::Closed(None));
    assert!(matches!(
        h.app.terminal_panel.detail.view,
        DetailView::Saved { .. }
    ));
    assert!(render(&mut h, 160, 48).contains("FINAL_PANEL_OUTPUT"));
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.terminal_panel.session_id(), Some(&id));
    assert!(h
        .fake
        .calls()
        .iter()
        .any(|call| call.starts_with(&format!("revive {id} "))));
    assert!(!h
        .fake
        .calls()
        .iter()
        .any(|call| call.starts_with("revive A ")));
}

#[tokio::test]
async fn panel_creation_failures_are_visible_and_keys_do_not_reach_the_agent() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    let main = h.last_attach().0;
    h.fake.outgoing(main);
    h.fake.fail_next(
        "create",
        ApiError::new(ErrorCode::PermissionDenied, "not allowed"),
    );
    h.press(KeyCode::Char('T')).await;
    assert!(h
        .app
        .terminal_panel
        .message
        .as_deref()
        .unwrap()
        .contains("not allowed"));
    h.press(KeyCode::Char('x')).await;
    assert!(h.fake.outgoing(main).is_empty());
    h.press(KeyCode::Enter).await;
    assert!(h.app.terminal_panel.detail.attachment.is_some());
}

#[tokio::test]
async fn panel_resize_changes_only_the_focused_shell_and_dragging_keeps_panes_tiled() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    let main = h.last_attach().0;
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.fake.outgoing(main);
    h.fake.outgoing(panel);
    h.app.resize(140, 40);
    assert!(h.fake.outgoing(main).is_empty());
    assert!(h.fake.outgoing(panel).iter().any(|message| matches!(
        message,
        AttachOutgoing::Command(StreamCommand::Resize { .. })
    )));
    let divider = h.app.screen().terminal_panel_divider.unwrap();
    render(&mut h, 140, 40);
    h.fake.outgoing(main);
    h.fake.outgoing(panel);
    let old_percent = h.app.terminal_panel.height_percent;
    h.app
        .handle_terminal_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: divider.x + 1,
            row: divider.y,
            modifiers: KeyModifiers::NONE,
        }))
        .await;
    assert!(h.app.terminal_panel.dragging);
    for kind in [
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        h.app
            .handle_terminal_event(Event::Mouse(MouseEvent {
                kind,
                column: divider.x + 1,
                row: divider.y.saturating_sub(3),
                modifiers: KeyModifiers::NONE,
            }))
            .await;
    }
    assert_ne!(h.app.terminal_panel.height_percent, old_percent);
    assert!(!h.app.terminal_panel.dragging);
    assert!(h.fake.outgoing(main).is_empty());
    assert!(h.fake.outgoing(panel).iter().any(|message| matches!(
        message,
        AttachOutgoing::Command(StreamCommand::Resize { .. })
    )));
    let screen = h.app.screen();
    assert!(screen.detail.unwrap().bottom() < screen.terminal_panel.unwrap().y);
    assert_eq!(
        screen.terminal_panel.unwrap().bottom(),
        screen.footer.unwrap().y
    );
}

#[tokio::test]
async fn panel_claims_its_size_only_after_user_activity_not_on_an_external_resize() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.fake.outgoing(panel);
    h.app
        .terminal_panel
        .detail
        .attachment
        .as_mut()
        .unwrap()
        .last_claim = None;
    h.attach_event(
        panel,
        AttachEvent::Event(StreamEvent::Resized {
            rows: 10,
            cols: 60,
            checkpoint: String::new(),
            offset: 100,
        }),
    );
    assert!(
        h.fake.outgoing(panel).is_empty(),
        "observer/other-window resize must not cause a sizing fight"
    );
    h.app
        .handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )))
        .await;
    h.app.process_pending();
    assert!(h.fake.outgoing(panel).iter().any(|message| matches!(
        message,
        AttachOutgoing::Command(StreamCommand::Resize { .. })
    )));
}

#[tokio::test]
async fn panel_self_and_folder_guards_reject_reusing_the_wrong_terminal() {
    for self_session in [false, true] {
        let mut h = harness(vec![view("A", "agent", 1)]);
        h.app
            .terminal_panel
            .bindings
            .insert(scope(), SessionId::from_string("A"));
        if self_session {
            h.app.self_session = Some(SessionId::from_string("A"));
        } else {
            h.fake.state.lock().unwrap().sessions[0].scope =
                ScopeRoot::from_canonical("/another-folder".into());
        }
        let attaches = h.fake.attach_count();
        h.press(KeyCode::Char('T')).await;
        assert_eq!(h.fake.attach_count(), attaches);
        assert!(h
            .app
            .terminal_panel
            .message
            .as_ref()
            .unwrap()
            .contains("cannot be nested"));
        assert!(h.app.terminal_panel.detail.attachment.is_none());
    }
}

#[tokio::test]
async fn an_empty_main_view_does_not_automatically_duplicate_the_new_utility_terminal() {
    let mut h = harness(vec![]);
    h.press(KeyCode::Char('T')).await;
    assert!(h.app.detail.session_id().is_none());
    assert!(h.app.terminal_panel.session_id().is_some());
    assert_eq!(h.fake.attach_count(), 1);
    assert_eq!(h.fake.attach_request(0).role, Role::Controller);
}

#[tokio::test]
async fn panel_headers_focus_and_hide_with_mouse_without_terminating_the_shell() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    h.rakumux().await;
    render(&mut h, 160, 48);
    let pane = h.app.screen().terminal_panel.unwrap();
    h.app
        .handle_terminal_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.x + 2,
            row: pane.y,
            modifiers: KeyModifiers::NONE,
        }))
        .await;
    assert_eq!(h.app.focus, Focus::Terminal);
    render(&mut h, 160, 48);
    let pane = h.app.screen().terminal_panel.unwrap();
    let id = h.app.terminal_panel.session_id().unwrap().clone();
    h.app
        .handle_terminal_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.right() - 3,
            row: pane.y,
            modifiers: KeyModifiers::NONE,
        }))
        .await;
    assert!(!h.app.terminal_panel.open);
    assert!(h
        .fake
        .state
        .lock()
        .unwrap()
        .sessions
        .iter()
        .any(|session| session.id == id));
    assert!(!h
        .fake
        .calls()
        .iter()
        .any(|call| call.starts_with("kill ") || call.starts_with("delete ")));
}

#[tokio::test]
async fn closing_during_creation_and_reopening_does_not_create_duplicate_shells() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.app.toggle_terminal_panel();
    assert!(h.app.terminal_panel.busy());
    h.app.hide_terminal_panel();
    h.app.toggle_terminal_panel();
    h.app.process_pending();
    assert_eq!(h.fake.state.lock().unwrap().creates.len(), 1);
    assert_eq!(h.app.focus, Focus::Terminal);
    assert!(h.app.terminal_panel.detail.attachment.is_some());
    h.app.hide_terminal_panel();
    assert!(!h
        .fake
        .calls()
        .iter()
        .any(|call| call.starts_with("kill ") || call.starts_with("delete ")));
}

#[tokio::test]
async fn a_stale_attached_stream_is_dropped_when_the_panel_was_hidden() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.app.toggle_terminal_panel();
    let resolved = h.app.rx.as_mut().unwrap().try_recv().unwrap();
    assert!(matches!(resolved, AppEvent::PanelResolved { .. }));
    h.app.handle_event(resolved);
    let count = h.fake.attach_count();
    h.app.hide_terminal_panel();
    h.app.process_pending();
    assert!(!h.app.terminal_panel.open);
    assert!(h.app.terminal_panel.detail.attachment.is_none());
    assert_eq!(h.fake.attach_count(), count);
    assert_eq!(h.app.focus, Focus::List);
}

#[tokio::test]
async fn a_panel_stream_disconnect_preserves_input_privacy_and_does_not_retake_control() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let id = h.app.terminal_panel.session_id().unwrap().clone();
    let panel = pane_index(&h);
    h.attach_event(panel, AttachEvent::Closed(Some("test stream loss".into())));
    assert_eq!(h.app.focus, Focus::List);
    assert!(
        h.app.input_is_private(),
        "remaining password characters must not enter debug logs"
    );
    h.app.terminal_panel.retry_at = None;
    h.app.sync_terminal_panel();
    h.app.process_pending();
    assert_eq!(h.app.terminal_panel.session_id(), Some(&id));
    assert_eq!(
        h.app
            .terminal_panel
            .detail
            .attachment
            .as_ref()
            .unwrap()
            .role,
        Role::Observer
    );
    assert_eq!(h.fake.state.lock().unwrap().creates.len(), 1);
    assert!(h.app.input_is_private());
}

#[tokio::test]
async fn reconnect_restores_the_existing_shell_as_observer_until_user_refocuses_it() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let id = h.app.terminal_panel.session_id().unwrap().clone();
    h.app.handle_event(AppEvent::SubscriptionLost {
        generation: h.app.sync_generation,
        error: "test connection loss".into(),
    });
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.terminal_panel.detail.attachment.is_none());
    h.app.handle_event(AppEvent::Connected(h.fake.clone()));
    h.app.process_pending();
    assert_eq!(h.app.terminal_panel.session_id(), Some(&id));
    assert_eq!(h.fake.state.lock().unwrap().creates.len(), 1);
    assert_eq!(
        h.app
            .terminal_panel
            .detail
            .attachment
            .as_ref()
            .unwrap()
            .role,
        Role::Observer
    );
    h.press(KeyCode::Esc).await;
    assert_eq!(h.app.focus, Focus::Terminal);
    assert_eq!(
        h.app
            .terminal_panel
            .detail
            .attachment
            .as_ref()
            .unwrap()
            .role,
        Role::Controller
    );
}

#[tokio::test]
async fn settings_can_open_the_terminal_panel_and_small_layouts_remain_usable() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('o')).await;
    for _ in 0..crate::app::types::SettingsItem::TerminalPanel.index() {
        h.press(KeyCode::Down).await;
    }
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Terminal);
    assert!(matches!(h.app.mode, AppMode::Normal));
    for layout in crate::layout::SessionLayout::ALL {
        h.app.set_layout(layout);
        for (width, height) in [(40, 10), (40, 20), (90, 40), (160, 48)] {
            let output = render(&mut h, width, height);
            assert!(h.app.screen().terminal_panel.is_some());
            assert!(output.contains("Terminal"));
            assert!(output.contains("TERMINAL"));
        }
    }
}

#[test]
fn panel_visible_screen_and_history_share_a_bounded_cell_budget() {
    for (rows, cols) in [(2, u16::MAX), (24, 80), (100, 1000), (5000, 100)] {
        assert!(valid_size(rows, cols));
        assert!((usize::from(rows) + panel_history(rows, cols)) * usize::from(cols) <= MAX_CELLS);
        assert!(panel_history(rows, cols) <= SCROLLBACK);
    }
    assert!(!valid_size(u16::MAX, u16::MAX));
    assert!(panel_terminal(24, 80, "not base64!").is_err());
}

#[tokio::test]
async fn terminal_opened_from_grid_keeps_the_agent_above_it_as_an_observer() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.app.set_layout(crate::layout::SessionLayout::Grid);
    h.app.process_pending();
    for index in 0..h.fake.attach_count() {
        h.fake.outgoing(index);
    }
    h.press(KeyCode::Char('T')).await;
    assert_eq!(
        h.app.detail.session_id(),
        Some(&SessionId::from_string("A"))
    );
    assert_eq!(
        h.app.detail.attachment.as_ref().unwrap().role,
        Role::Observer
    );
    assert!(h.app.screen().detail.is_some() && h.app.screen().terminal_panel.is_some());
    for index in 0..h.fake.attach_count() {
        if h.fake.attach_request(index).id == SessionId::from_string("A") {
            assert_eq!(h.fake.attach_request(index).role, Role::Observer);
            assert!(h
                .fake
                .outgoing(index)
                .iter()
                .all(|message| *message == AttachOutgoing::Command(StreamCommand::Detach)));
        }
    }
}

#[tokio::test]
async fn panel_reuses_its_shell_per_folder_within_the_window() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('T')).await;
    let first = h.app.terminal_panel.session_id().unwrap().clone();
    h.rakumux().await;
    h.press(KeyCode::Char('T')).await;
    let mut second = view("B", "second-agent", 2);
    second.scope = ScopeRoot::from_canonical("/second".into());
    second.cwd = "/second".into();
    h.fake.state.lock().unwrap().sessions.push(second.clone());
    h.app.list.upsert(second);
    h.app.select_session(SessionId::from_string("B"));
    h.press(KeyCode::Char('T')).await;
    let other = h.app.terminal_panel.session_id().unwrap().clone();
    assert_ne!(first, other);
    assert_eq!(
        h.fake.state.lock().unwrap().creates[1]
            .scope
            .as_ref()
            .unwrap()
            .path(),
        std::path::Path::new("/second")
    );
    h.rakumux().await;
    h.press(KeyCode::Char('T')).await;
    h.app.select_session(SessionId::from_string("A"));
    h.press(KeyCode::Char('T')).await;
    assert_eq!(h.app.terminal_panel.session_id(), Some(&first));
    assert_eq!(h.fake.state.lock().unwrap().creates.len(), 2);
}

#[tokio::test]
async fn scrolling_an_unfocused_panel_does_not_claim_the_agents_terminal_size() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    let main = h.last_attach().0;
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.rakumux().await;
    h.fake.outgoing(main);
    h.fake.outgoing(panel);
    let pane = h.app.screen().terminal_panel.unwrap();
    h.app
        .handle_terminal_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: pane.x + 1,
            row: pane.y + 2,
            modifiers: KeyModifiers::NONE,
        }))
        .await;
    assert!(h.fake.outgoing(main).is_empty());
    assert!(h.fake.outgoing(panel).is_empty());
}

#[tokio::test]
async fn terminal_shortcut_does_not_run_inside_a_filter_or_the_shell() {
    let mut h = harness(vec![view("A", "agent", 1)]);
    h.press(KeyCode::Char('/')).await;
    h.press(KeyCode::Char('T')).await;
    assert_eq!(h.app.list.filter.value(), "T");
    assert!(!h.app.terminal_panel.open);
    h.press(KeyCode::Esc).await;
    h.press(KeyCode::Char('T')).await;
    let panel = pane_index(&h);
    h.fake.outgoing(panel);
    h.press(KeyCode::Char('T')).await;
    assert_eq!(
        h.fake.outgoing(panel),
        vec![AttachOutgoing::Input(b"T".to_vec())]
    );
}
