use super::*;
use crate::app::testing::{aged, folder, options, scope, start, view, view_in, FakeApi, Harness};
use crate::app::types::AppMode;
use base64::Engine;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};
use session_core::api::{Attached, SessionEvent};
use session_core::client::AttachOutgoing;

fn harness(count: usize) -> Harness {
    let sessions = (0..count)
        .map(|i| {
            aged(
                &format!("S{i}"),
                &format!("session-{i}"),
                i as i64,
                if i == 0 { 10000 } else { i as i64 },
            )
        })
        .collect();
    let fake = FakeApi::new(sessions);
    fake.state.lock().unwrap().checkpoint.clear();
    let dir = tempfile::tempdir().unwrap();
    let mut config = options(&fake, scope(), &dir);
    config.settings.sessions.layout = SessionLayout::Grid;
    start(config, fake, dir)
}

fn text(terminal: &TerminalView) -> String {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
    terminal.render(
        buffer.area,
        &mut buffer,
        false,
        crate::terminal_view::DefaultColors {
            fg: ratatui::style::Color::White,
            bg: ratatui::style::Color::Black,
        },
    );
    buffer.content().iter().map(|c| c.symbol()).collect()
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    app.resize(width, height);
    app.process_pending();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}

fn outgoing_only_detaches(h: &Harness) {
    for i in 0..h.fake.attach_count() {
        assert_eq!(h.fake.attach_request(i).role, Role::Observer);
        for message in h.fake.outgoing(i) {
            assert_eq!(message, AttachOutgoing::Command(StreamCommand::Detach));
        }
    }
    assert!(!h.fake.calls().iter().any(|c| c.starts_with("touch ")));
}

#[test]
fn every_quiet_working_grid_card_has_the_shared_animated_status_marker() {
    use session_core::model::Lifecycle;
    let mut h = harness(2);
    for index in 0..2 {
        let id = format!("S{index}");
        let mut session = h
            .app
            .list
            .get(&SessionId::from_string(&id))
            .unwrap()
            .clone();
        session.lifecycle = Lifecycle::Working;
        h.event(&id, SessionEvent::Updated { session });
    }
    assert!(
        h.app.is_animating(),
        "quiet PTYs still need spinner redraws"
    );
    let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut h.app))
        .unwrap();
    for (index, area) in h.app.grid_page().cards {
        if index >= 2 {
            continue;
        }
        assert!((area.x..area.right()).any(|x|
            "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(terminal.backend().buffer().get(x, area.y).symbol())), "working card {index} has no spinner");
    }
    for index in 0..2 {
        let id = format!("S{index}");
        let mut session = h
            .app
            .list
            .get(&SessionId::from_string(&id))
            .unwrap()
            .clone();
        session.lifecycle = Lifecycle::Blocked;
        h.event(&id, SessionEvent::Updated { session });
    }
    assert!(!h.app.is_animating(), "attention markers stay static");
    outgoing_only_detaches(&h);
}

#[tokio::test]
async fn grid_distinct_live_observers_never_write_resize_or_touch() {
    let mut h = harness(3);
    assert_eq!(h.fake.attach_count(), 3);
    assert!(h.app.detail.attachment.is_none());
    for i in 0..3 {
        let request = h.fake.attach_request(i);
        assert_eq!((request.cols, request.rows), (80, 24));
        h.attach_event(
            i,
            AttachEvent::Output {
                offset: 100,
                bytes: format!("\x1b[Hlive-output-for-{}\x1b[6n\x1b[c\x07", request.id)
                    .into_bytes(),
            },
        );
        h.attach_event(
            i,
            AttachEvent::Event(StreamEvent::Clipboard {
                selection: "c".into(),
                data: "secret".into(),
            }),
        );
        h.attach_event(i, AttachEvent::Event(StreamEvent::Bell));
    }
    let output = render(&mut h.app, 160, 48);
    for i in 0..3 {
        assert!(
            output.contains(&format!("live-output-for-S{i}")),
            "{output}"
        );
    }
    assert!(output.contains("observing · read-only"));
    assert!(h.app.take_terminal_output().is_empty());
    for event in [
        Event::FocusGained,
        Event::Resize(120, 30),
        Event::Paste("not-input".into()),
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }),
        Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
    ] {
        h.app.handle_terminal_event(event).await;
        h.app.process_pending();
    }
    h.press(KeyCode::PageDown).await;
    h.press(KeyCode::PageUp).await;
    outgoing_only_detaches(&h);
    assert!(h.app.screen().terminal.is_none());
}

#[tokio::test]
async fn grid_paging_is_bounded_and_cancels_offscreen_and_layout_watchers() {
    let mut h = harness(14);
    assert_eq!(h.app.grid.cards.len(), MAX_CARDS);
    assert_eq!(h.fake.attach_count(), MAX_CARDS);
    let first_ids: Vec<_> = h.app.grid.cards.keys().cloned().collect();
    h.press(KeyCode::PageDown).await;
    assert_eq!(h.selected().as_deref(), Some("S6"));
    assert_eq!(h.app.grid_page().start, 6);
    assert_eq!(h.app.grid.cards.len(), MAX_CARDS);
    assert!(first_ids
        .iter()
        .all(|id| !h.app.grid.cards.contains_key(id)));
    for i in 0..6 {
        assert_eq!(
            h.fake.outgoing(i),
            vec![AttachOutgoing::Command(StreamCommand::Detach)]
        );
        assert!(h.fake.state.lock().unwrap().attaches[i]
            .peer
            .events
            .send(AttachEvent::Output {
                offset: 100,
                bytes: b"stale".to_vec()
            })
            .is_err());
    }
    h.app.set_layout(SessionLayout::Tabs);
    h.app.process_pending();
    assert!(h.app.grid.cards.is_empty());
    for i in 6..12 {
        assert_eq!(
            h.fake.outgoing(i),
            vec![AttachOutgoing::Command(StreamCommand::Detach)]
        );
    }
}

#[tokio::test]
async fn grid_navigation_activation_return_and_v_routing() {
    let mut h = harness(9);
    h.press(KeyCode::Right).await;
    assert_eq!(h.selected().as_deref(), Some("S1"));
    h.press(KeyCode::Down).await;
    assert_eq!(h.selected().as_deref(), Some("S4"));
    h.press(KeyCode::Left).await;
    assert_eq!(h.selected().as_deref(), Some("S3"));
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert!(h.app.grid.cards.is_empty());
    assert!(h.app.screen().terminal.is_some());
    let (index, request) = h.last_attach();
    assert_eq!(request.role, Role::Controller);
    h.fake.outgoing(index);
    h.press(KeyCode::Char('v')).await;
    assert_eq!(h.app.session_layout(), SessionLayout::Grid);
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Input(b"v".to_vec())]
    );
    h.rakumux().await;
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.detail.attachment.is_none());
    assert_eq!(h.app.grid.cards.len(), 6);
    assert!(h.app.screen().terminal.is_none());
    let from = h.fake.attach_count();
    h.app.handle_terminal_event(Event::FocusGained).await;
    for i in from - 6..from {
        assert!(h.fake.outgoing(i).is_empty());
    }
    h.press(KeyCode::Char('v')).await;
    assert_eq!(h.app.session_layout(), SessionLayout::Tabs);
    h.press(KeyCode::Char('v')).await;
    assert_eq!(h.app.session_layout(), SessionLayout::Sidebar);
    h.press(KeyCode::Char('v')).await;
    assert_eq!(h.app.session_layout(), SessionLayout::Grid);
    // Not saved: every start opens Master detail.
    let saved = crate::app::Settings::load_from(&h.home.path().join("config.json")).unwrap();
    assert_eq!(saved.sessions.layout, SessionLayout::Sidebar);
}

#[tokio::test]
async fn grid_filters_order_self_guard_and_immediate_creation() {
    let web = folder("/web");
    let mut pinned = view("P", "pinned", 0);
    pinned.pinned = true;
    let mut current = view("SELF", "self", 10);
    current.is_self = true;
    let fake = FakeApi::new(vec![
        view("A", "alpha", 1),
        pinned,
        current,
        view_in(&web, "W", "web", 20),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut config = options(&fake, scope(), &dir);
    config.settings.sessions.layout = SessionLayout::Grid;
    let mut h = start(config, fake, dir);
    assert_eq!(h.app.list.visible()[0].id, SessionId::from_string("P"));
    assert!(!h
        .app
        .grid
        .cards
        .contains_key(&SessionId::from_string("SELF")));
    assert!(render(&mut h.app, 160, 48).contains("cannot be nested"));
    h.app.show_folders(vec![web.clone()]);
    h.app.process_pending();
    assert_eq!(h.app.grid.cards.len(), 1);
    assert!(h.app.grid.get(&SessionId::from_string("W")).is_some());
    h.press(KeyCode::Char('/')).await;
    h.type_text("nomatch").await;
    assert!(h.app.grid.cards.is_empty());
    assert!(render(&mut h.app, 80, 24).contains("No matching sessions"));
    h.press(KeyCode::Esc).await;
    h.press(KeyCode::End).await;
    assert_eq!(h.app.list.selection(), &Selection::NewSession);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert_eq!(h.fake.creates().last().unwrap().scope, Some(web));
    assert!(h.fake.creates().last().unwrap().name.is_none());
}

#[test]
fn grid_offsets_duplicates_gaps_resize_and_stale_events() {
    let mut h = harness(1);
    let id = SessionId::from_string("S0");
    h.attach_event(
        0,
        AttachEvent::Output {
            offset: 100,
            bytes: b"alpha".to_vec(),
        },
    );
    h.attach_event(
        0,
        AttachEvent::Output {
            offset: 100,
            bytes: b"alpha".to_vec(),
        },
    );
    h.attach_event(
        0,
        AttachEvent::Output {
            offset: 103,
            bytes: b"ha-beta".to_vec(),
        },
    );
    assert!(text(h.app.grid.get(&id).unwrap().terminal.as_ref().unwrap()).contains("alpha-beta"));
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"resized");
    h.attach_event(
        0,
        AttachEvent::Event(StreamEvent::Resized {
            rows: 30,
            cols: 100,
            checkpoint: encoded,
            offset: 120,
        }),
    );
    let card = h.app.grid.get(&id).unwrap();
    assert_eq!(card.terminal.as_ref().unwrap().size(), (30, 100));
    assert_eq!(card.offset, 120);
    let old_generation = card.generation;
    h.attach_event(
        0,
        AttachEvent::Output {
            offset: 125,
            bytes: b"gap".to_vec(),
        },
    );
    assert_eq!(h.fake.attach_count(), 2);
    assert_ne!(h.app.grid.get(&id).unwrap().generation, old_generation);
    h.app.on_preview_event(
        &id,
        old_generation,
        AttachEvent::Output {
            offset: 100,
            bytes: b"stale".to_vec(),
        },
    );
    assert!(!text(h.app.grid.get(&id).unwrap().terminal.as_ref().unwrap()).contains("stale"));
    outgoing_only_detaches(&h);
}

#[test]
fn grid_saved_exit_revive_removal_and_stale_reads() {
    let mut h = harness(2);
    let id = SessionId::from_string("S0");
    let old_generation = h.app.grid.get(&id).unwrap().generation;
    h.attach_event(
        0,
        AttachEvent::Output {
            offset: 100,
            bytes: b"final output".to_vec(),
        },
    );
    h.attach_event(0, AttachEvent::Event(StreamEvent::Exited { code: Some(0) }));
    assert_eq!(h.app.grid.get(&id).unwrap().state, PreviewState::Saved);
    assert!(text(h.app.grid.get(&id).unwrap().terminal.as_ref().unwrap()).contains("final output"));
    let mut revived = view("S0", "session-0", 10000);
    revived.created_at = 0;
    h.event("S0", SessionEvent::Updated { session: revived });
    assert_eq!(h.app.grid.get(&id).unwrap().state, PreviewState::Live);
    h.app.on_preview_saved(
        id.clone(),
        old_generation,
        Ok(ReadResult {
            text: "stale read".into(),
            rows: 24,
            cols: 80,
            cursor: (0, 0),
            truncated: false,
            untrusted: true,
        }),
    );
    assert_eq!(h.app.grid.get(&id).unwrap().state, PreviewState::Live);
    h.event("S0", SessionEvent::Removed);
    assert!(h.app.grid.get(&id).is_none());
}

#[test]
fn grid_dead_saved_error_and_small_empty_phone_rendering() {
    let fake = FakeApi::new(vec![{
        let mut dead = view("D", "dead", 0);
        dead.status = SessionStatus::Dead;
        dead.terminal = None;
        dead
    }]);
    fake.set_read("D", "saved terminal text");
    let dir = tempfile::tempdir().unwrap();
    let mut config = options(&fake, scope(), &dir);
    config.settings.sessions.layout = SessionLayout::Grid;
    let mut h = start(config, fake, dir);
    assert_eq!(h.fake.attach_count(), 0);
    assert!(render(&mut h.app, 80, 24).contains("saved terminal text"));
    assert!(render(&mut h.app, 80, 24).contains("saved · Enter to revive"));
    let mut empty = harness(0);
    for (width, height) in [(160, 48), (80, 24), (45, 40), (40, 20), (1, 1), (5, 3)] {
        let output = render(&mut empty.app, width, height);
        assert!(empty.app.screen().terminal.is_none());
        assert_eq!(empty.app.screen().footer.unwrap().height, 1);
        if width >= 80 {
            assert!(output.contains("No sessions yet"), "{output}");
        } else if width >= 40 {
            // A phone has no Grid: the list, ready for a session.
            assert!(output.contains("+ New session"), "{output}");
        }
    }
    assert!(empty.app.grid.cards.is_empty());
}

#[test]
fn grid_saved_single_line_and_empty_metadata_are_valid() {
    let mut h = harness(1);
    let id = SessionId::from_string("S0");
    let mut dead = view("S0", "saved", 0);
    dead.status = SessionStatus::Dead;
    dead.terminal = None;
    h.event("S0", SessionEvent::Updated { session: dead });
    let generation = h.app.grid.get(&id).unwrap().generation;
    h.app.on_preview_saved(
        id.clone(),
        generation,
        Ok(ReadResult {
            text: "one saved row".into(),
            rows: 1,
            cols: 13,
            cursor: (0, 0),
            truncated: false,
            untrusted: true,
        }),
    );
    assert_eq!(h.app.grid.get(&id).unwrap().state, PreviewState::Saved);
    assert!(
        text(h.app.grid.get(&id).unwrap().terminal.as_ref().unwrap()).contains("one saved row")
    );
    h.app.on_preview_saved(
        id.clone(),
        generation,
        Ok(ReadResult {
            text: String::new(),
            rows: 0,
            cols: 0,
            cursor: (0, 0),
            truncated: false,
            untrusted: true,
        }),
    );
    assert_eq!(h.app.grid.get(&id).unwrap().state, PreviewState::Saved);
    assert!(h.app.grid.get(&id).unwrap().terminal.is_none());
}

#[tokio::test]
async fn a_phone_has_no_grid_and_v_skips_it_there() {
    let mut h = harness(2);
    // Grid chosen on a wide screen, then the window narrows to a phone:
    // Master detail's list, no cards, nothing left attached for them.
    render(&mut h.app, 160, 48);
    assert!(h.app.screen().grid.is_some());
    render(&mut h.app, 45, 40);
    h.app.on_tick(Instant::now());
    h.app.process_pending();
    assert_eq!(h.app.session_layout(), SessionLayout::Sidebar);
    assert!(h.app.screen().grid.is_none());
    assert!(h.app.screen().list.is_some());
    assert!(!h
        .app
        .hits
        .iter()
        .any(|(_, hit)| matches!(hit, crate::app::Hit::GridCell(_))));
    // `v` goes between Master detail and Tabs only.
    h.press(KeyCode::Char('v')).await;
    assert_eq!(h.app.session_layout(), SessionLayout::Tabs);
    h.press(KeyCode::Char('v')).await;
    assert_eq!(h.app.session_layout(), SessionLayout::Sidebar);
    // Back on a wide screen Grid is offered again.
    render(&mut h.app, 160, 48);
    h.press(KeyCode::Char('v')).await;
    assert_eq!(h.app.session_layout(), SessionLayout::Grid);
    assert!(h.app.drag.is_none());
}

#[test]
fn grid_stale_attach_result_is_dropped_and_shutdown_cancels() {
    let mut h = harness(1);
    let id = SessionId::from_string("S0");
    let generation = h.app.grid.get(&id).unwrap().generation;
    let attached = Attached {
        session: view("S0", "session-0", 0),
        role: Role::Observer,
        cols: 80,
        rows: 24,
        checkpoint: String::new(),
        offset: 100,
    };
    let (stream, peer) = AttachStream::pair(attached);
    h.app.list.filter.set("no-match");
    h.app.sync_grid();
    h.app.on_preview_attached(id, generation, Ok(stream));
    assert!(h.app.grid.cards.is_empty());
    assert!(peer.events.send(AttachEvent::Closed(None)).is_err());
    assert!(peer.outgoing.try_recv().is_err());
    h.app.list.filter.set("");
    h.app.sync_grid();
    h.app.process_pending();
    let index = h.fake.attach_count() - 1;
    let fake = h.fake.clone();
    drop(h);
    assert_eq!(
        fake.outgoing(index),
        vec![AttachOutgoing::Command(StreamCommand::Detach)]
    );
}

#[test]
fn grid_disconnect_cancels_and_reconnect_rebuilds_observers() {
    let mut h = harness(3);
    h.fake.drop_subscribers();
    h.app.process_pending();
    assert!(h.app.grid.cards.is_empty());
    assert!(!h.app.connection.is_connected());
    for i in 0..3 {
        assert_eq!(
            h.fake.outgoing(i),
            vec![AttachOutgoing::Command(StreamCommand::Detach)]
        );
    }
    h.app.refresh();
    h.app.process_pending();
    assert!(h.app.connection.is_connected());
    assert_eq!(h.app.grid.cards.len(), 3);
    outgoing_only_detaches(&h);
}

#[test]
fn grid_mailbox_and_terminal_memory_are_bounded() {
    let mailbox = PreviewMailbox::default();
    assert!(mailbox.push(AttachEvent::Output {
        offset: 0,
        bytes: vec![b'x'; MAX_BYTES]
    }));
    assert!(!mailbox.push(AttachEvent::Output {
        offset: MAX_BYTES as u64,
        bytes: vec![b'x'; MAX_BYTES]
    }));
    let events = mailbox.drain();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        AttachEvent::Event(StreamEvent::Lagged { .. })
    ));
    assert!(!mailbox.push(AttachEvent::Event(StreamEvent::Clipboard {
        selection: "c".into(),
        data: "ignored".into()
    })));
    assert!(mailbox.drain().is_empty());
    assert!(preview_terminal(65535, 65535, "").is_err());
    let checkpoint = base64::engine::general_purpose::STANDARD.encode(b"line\r\n".repeat(1000));
    let mut terminal = preview_terminal(24, 80, &checkpoint).unwrap();
    assert_eq!(terminal.history_len(), 0);
    terminal.reset_screen_to_text(24, 80, &"saved row\n".repeat(1000));
    assert_eq!(terminal.history_len(), 0);
}

#[tokio::test]
async fn grid_hides_old_detail_without_resize_or_controller_claims() {
    let mut h = crate::app::testing::harness(vec![view("A", "alpha", 1), view("B", "beta", 2)]);
    let detail = h.fake.attach_count() - 1;
    h.fake.outgoing(detail); // Existing Master detail observer's initial Fit.
    h.fake.clear_calls();
    h.app.set_layout(SessionLayout::Grid);
    h.app.process_pending();
    assert_eq!(
        h.fake.outgoing(detail),
        vec![AttachOutgoing::Command(StreamCommand::Detach)]
    );
    assert!(h.app.detail.attachment.is_none());
    h.app.handle_terminal_event(Event::Resize(100, 24)).await;
    h.app.handle_terminal_event(Event::FocusGained).await;
    h.app
        .handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)))
        .await;
    h.app.process_pending();
    outgoing_only_detaches(&h);
}

#[test]
fn grid_error_states_lag_and_unsolicited_control_are_safe() {
    let mut h = harness(1);
    let id = SessionId::from_string("S0");
    h.attach_event(0, AttachEvent::Event(StreamEvent::Lagged { offset: 120 }));
    assert_eq!(h.fake.attach_count(), 2);
    h.attach_event(
        1,
        AttachEvent::Event(StreamEvent::RoleChanged {
            role: Role::Controller,
        }),
    );
    assert!(matches!(
        h.app.grid.get(&id).unwrap().state,
        PreviewState::Error(_)
    ));
    assert!(render(&mut h.app, 160, 48).contains("no longer an observer"));
    outgoing_only_detaches(&h);
    h.app.grid.clear();
    h.fake.fail_next(
        "attach",
        ApiError::new(ErrorCode::NotFound, "session vanished"),
    );
    h.app.sync_grid();
    h.app.process_pending();
    assert!(render(&mut h.app, 160, 48).contains("session vanished"));
    h.app.grid.clear();
    let mut dead = view("S0", "dead", 0);
    dead.status = SessionStatus::Dead;
    dead.terminal = None;
    h.fake
        .fail_next("read", ApiError::new(ErrorCode::NotFound, "no archive"));
    h.event("S0", SessionEvent::Updated { session: dead });
    let output = render(&mut h.app, 160, 48);
    assert!(
        output.contains("No saved output") && output.contains("no archive"),
        "{output}"
    );
}

#[test]
fn grid_pending_jobs_remain_bounded_across_page_changes() {
    let mut h = harness(14);
    h.app.grid.clear();
    let count = h.fake.attach_count();
    // Model six blocking requests from a cancelled page that haven't
    // returned yet. No new request may start until a slot is released.
    h.app.grid.pending.store(MAX_CARDS, Ordering::Relaxed);
    h.app.move_grid_page(1);
    h.app.process_pending();
    assert_eq!(h.fake.attach_count(), count);
    assert!(h.app.grid.cards.values().all(|card| !card.started));
    h.app.grid.pending.store(0, Ordering::Relaxed);
    h.app.sync_grid();
    h.app.process_pending();
    assert_eq!(h.fake.attach_count(), count + MAX_CARDS);
    assert_eq!(h.app.grid.cards.len(), MAX_CARDS);
    assert_eq!(h.app.grid.pending.load(Ordering::Relaxed), 0);
}

#[test]
fn grid_drawing_never_opens_or_writes_streams() {
    let mut h = harness(10);
    let count = h.fake.attach_count();
    let mut terminal = Terminal::new(TestBackend::new(90, 40)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    assert_eq!(h.fake.attach_count(), count);
    for i in 0..count {
        assert!(h.fake.outgoing(i).is_empty());
    }
    h.app.on_tick(Instant::now());
    h.app.process_pending();
    assert_eq!(h.app.grid.cards.len(), h.app.grid_page().cards.len());
    assert!(h.app.grid.cards.len() < 10);
    outgoing_only_detaches(&h);
}
