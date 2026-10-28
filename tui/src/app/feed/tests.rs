use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use rakumux_sensagent::feed::{
    FeedEntry, FeedPage, FeedReply, FeedStatus, FeedTurn, FeedTurnStatus, ReplyStatus, ReplyWrite,
    ReplyWriteStatus,
};
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

use super::{FeedStore, ThreadItem, MAX_ENTRIES};
use crate::app::grouping::Grouping;
use crate::app::testing::{options, scope, start, view, FakeAccount, FakeApi, Harness};
use crate::app::types::{AppMode, Focus};
use crate::app::Hit;
use crate::layout::{self, SessionLayout};

pub fn entry(id: &str, session: &str, finished_at: i64, seq: Option<u64>) -> FeedEntry {
    FeedEntry {
        id: id.into(),
        seq,
        machine: "studio".into(),
        session_id: session.into(),
        session_name: format!("session {session}"),
        folder: Some("repo".into()),
        branch: None,
        agent: "claude-code".into(),
        conversation_id: None,
        text: format!("Reply {id}"),
        truncated: false,
        finished_at,
        position: None,
        reply_to: None,
        thread: None,
        prompt: None,
    }
}

pub fn page(generation: u64, revision: u64, entries: Vec<FeedEntry>, more: bool) -> FeedPage {
    FeedPage {
        status: FeedStatus::Ready,
        generation,
        revision,
        entries,
        replies: Vec::new(),
        more,
        open: None,
        machine: None,
    }
}

fn ids(store: &FeedStore) -> Vec<&str> {
    store.entries().iter().map(|e| e.id.as_str()).collect()
}

#[test]
fn entries_are_newest_first_and_the_selection_stays_on_its_reply() {
    let mut store = FeedStore::new("studio".into());
    assert_eq!(store.status(), None);
    assert_eq!(store.cursor(), (None, None));
    store.apply(page(
        1,
        2,
        vec![
            entry("A", "S", 1_000, Some(1)),
            entry("B", "S", 3_000, Some(2)),
        ],
        false,
    ));
    assert_eq!(ids(&store), ["B", "A"]);
    assert_eq!(store.cursor(), (Some(2), Some(1)));
    assert_eq!(store.selected().unwrap().id, "B", "the newest by default");
    store.move_selection(1);
    assert_eq!(store.selected().unwrap().id, "A");
    // A newer reply arrives on top; the selection stays on A. A pending
    // one of the same moment goes first; a changed entry is replaced.
    let mut confirmed = entry("A", "S", 1_000, Some(1));
    confirmed.text = "Edited".into();
    store.apply(page(
        1,
        4,
        vec![
            entry("C", "S", 5_000, Some(3)),
            entry("D", "S", 5_000, None),
            confirmed,
        ],
        false,
    ));
    assert_eq!(ids(&store), ["D", "C", "B", "A"]);
    assert_eq!(store.selected().unwrap().text, "Edited");
    store.move_selection(-10);
    assert_eq!(store.selected_index(), Some(0));
    store.move_selection(10);
    assert_eq!(store.selected_index(), Some(3));
}

#[test]
fn another_generation_replaces_everything_held() {
    let mut store = FeedStore::new("studio".into());
    store.apply(page(1, 1, vec![entry("A", "S", 1, Some(1))], false));
    store.scroll = 5;
    store.apply(page(2, 1, vec![entry("B", "S", 2, Some(1))], false));
    assert_eq!(ids(&store), ["B"]);
    assert_eq!(store.scroll, 0);
    assert_eq!(store.cursor(), (Some(1), Some(2)));
}

#[test]
fn the_feed_keeps_the_newest_five_hundred() {
    let mut store = FeedStore::new("studio".into());
    let entries = (0..MAX_ENTRIES as i64 + 20)
        .map(|i| entry(&format!("E{i}"), "S", i, Some(i as u64 + 1)))
        .collect();
    store.apply(page(1, 1, entries, false));
    assert_eq!(store.entries().len(), MAX_ENTRIES);
    assert_eq!(store.entries()[0].id, format!("E{}", MAX_ENTRIES + 19));
    assert_eq!(store.entries().last().unwrap().id, "E20");
}

#[test]
fn text_is_stripped_of_what_a_terminal_would_act_on() {
    assert_eq!(super::plain("a\u{1b}[31mb\u{202e}c\td\ne"), "a[31mbc d\ne");
    assert_eq!(super::agent_name("claude-code"), "claude");
    assert_eq!(super::agent_name("codex"), "codex");
}

fn feed_harness(account: FakeAccount, layout: SessionLayout) -> (Harness, Arc<FakeAccount>) {
    let fake = FakeApi::new(vec![view("A", "api", 20), view("B", "docs", 10)]);
    let dir = tempfile::tempdir().unwrap();
    let account = Arc::new(account);
    let mut options = options(&fake, scope(), &dir);
    options.account = account.clone();
    options.settings.sessions.layout = layout;
    (start(options, fake, dir), account)
}

fn tick(h: &mut Harness, after: Duration) {
    h.app.on_tick(Instant::now() + after);
    h.app.process_pending();
}

/// The feed shown with `f`, with the keys.
async fn show_feed(h: &mut Harness) {
    h.press(KeyCode::Char('f')).await;
    assert!(h.app.feed_has_keys(), "the feed has the keys");
    assert!(h.app.feed_shown(), "the feed shows");
}

/// Entries that finished after this window started (so they can be new).
fn fresh(id: &str, session: &str, seq: u64) -> FeedEntry {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    entry(id, session, now + seq as i64, Some(seq))
}

/// Draw the screen at the harness's size, for the click targets.
fn draw(h: &mut Harness) {
    let area = h.app.area;
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
}

/// A left click at `(column, row)`.
fn click_at(h: &mut Harness, column: u16, row: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        h.app.handle_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }
    h.app.process_pending();
}

/// Where `hit` was last drawn.
fn target(h: &mut Harness, hit: Hit) -> ratatui::layout::Rect {
    draw(h);
    h.app
        .hits
        .iter()
        .find(|(_, candidate)| *candidate == hit)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("{hit:?} on screen"))
}

/// Click the feed's card `index` where it was last drawn.
fn click_card(h: &mut Harness, index: usize) {
    let card = target(h, Hit::FeedEntry(index));
    click_at(h, card.x + 2, card.y + 1);
}

/// A reply made here in session B (newest, so selected first) and one
/// from the machine `lab`.
fn here_and_elsewhere(account: &FakeAccount) {
    let mut remote = entry("R", "elsewhere", 1_000, Some(1));
    remote.machine = "lab".into();
    account.queue_feed(page(
        1,
        2,
        vec![entry("L", "B", 2_000, Some(2)), remote],
        false,
    ));
}

#[tokio::test]
async fn nothing_shows_the_feed_by_itself() {
    for layout in SessionLayout::ALL {
        let (mut h, account) = feed_harness(FakeAccount::with_feed(), layout);
        assert_eq!(h.app.list.grouping(), Grouping::Status);
        // The feed is kept in the background to count what's new, asked
        // without counting as shown, and learning the account's Feed switch
        // is on brings nothing on screen.
        for after in [10, 11_000, 22_000] {
            tick(&mut h, Duration::from_millis(after));
            h.app.check_account();
            h.app.process_pending();
        }
        assert_eq!(account.feed_asks()[0], (None, None), "{layout:?}");
        assert!(
            account.feed_shown().iter().all(|shown| !shown),
            "{layout:?}"
        );
        assert_eq!(h.app.feed.status(), Some(FeedStatus::Ready));
        assert!(!h.app.feed_open(), "{layout:?}");
        assert!(h.app.screen().feed.is_none(), "{layout:?}");
        assert!(!h.app.feed_tab_shown(), "{layout:?}");
        // Nor on a phone.
        h.app.resize(45, 40);
        tick(&mut h, Duration::from_secs(40));
        assert!(h.app.screen().feed.is_none(), "{layout:?}");
    }
    // Off: nothing is asked while it's hidden.
    let (mut h, account) = feed_harness(FakeAccount::signed_in(), SessionLayout::Sidebar);
    tick(&mut h, Duration::from_secs(120));
    assert!(account.feed_asks().is_empty());
    assert!(!h.app.feed_open());
}

#[tokio::test]
async fn f_docks_the_feed_on_the_right_and_esc_and_f_move_the_keys() {
    let (mut h, _) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    let before = h.app.screen();
    let detail = before.detail.unwrap();
    show_feed(&mut h).await;
    // Session list | session | feed: the list keeps its width, the session
    // narrows, the feed takes the right edge.
    let screen = h.app.screen();
    let feed = screen.feed.unwrap();
    assert_eq!(feed.right(), 160);
    assert_eq!(feed.width, layout::default_feed_width(160));
    assert_eq!(screen.feed_divider.unwrap().x, feed.x - 1);
    assert_eq!(screen.list, before.list);
    let narrowed = screen.detail.unwrap();
    assert_eq!(narrowed.x, detail.x);
    assert_eq!(narrowed.right(), feed.x - 1);
    assert!(narrowed.width >= layout::FEED_SESSION_MIN_COLS);
    // ←/→ and `h` do nothing while the feed has the keys.
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Char('h')] {
        h.press(code).await;
        assert!(h.app.feed_has_keys(), "{code:?}");
    }
    assert_eq!(h.app.list.grouping(), Grouping::Status);
    // Esc cancels a pending key first, then gives the list the keys back;
    // the feed stays.
    h.press(KeyCode::Char('g')).await;
    h.press(KeyCode::Esc).await;
    assert!(h.app.feed_has_keys());
    h.press(KeyCode::Esc).await;
    assert!(!h.app.feed_has_keys());
    assert!(h.app.feed_shown());
    assert_eq!(h.app.focus, Focus::List);
    h.press(KeyCode::Right).await;
    assert_eq!(h.app.list.grouping(), Grouping::Folder);
    // `f` gives the feed the keys, then hides it.
    h.press(KeyCode::Char('f')).await;
    assert!(h.app.feed_has_keys());
    h.press(KeyCode::Char('f')).await;
    assert!(!h.app.feed_open());
    assert_eq!(h.app.screen(), before);
    // Picking a grouping (P, a tab's modal) gives the list the keys.
    show_feed(&mut h).await;
    h.app.show_tab(Grouping::App);
    assert!(!h.app.feed_has_keys());
    assert!(h.app.feed_open());
}

#[tokio::test]
async fn the_session_takes_the_size_the_feed_leaves_it() {
    use session_core::api::StreamCommand;
    use session_core::client::AttachOutgoing;
    let (mut h, _) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    let (index, _) = h.last_attach();
    h.fake.outgoing(index);
    let wide = h.app.terminal_size();
    // Observed in Rakumux mode: it's fitted to the narrower pane.
    show_feed(&mut h).await;
    let narrow = h.app.terminal_size();
    assert_eq!(narrow.0, h.app.screen().detail.unwrap().width);
    assert!(narrow.0 < wide.0);
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Command(StreamCommand::Fit {
            cols: narrow.0,
            rows: narrow.1
        })]
    );
    // Working in it, the feed stays beside it and the list gets the keys
    // when Rakumux mode comes back.
    h.press(KeyCode::Esc).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert!(h.app.feed_shown());
    assert!(!h.app.feed_has_keys());
    assert_eq!(h.app.screen().terminal.unwrap().width, narrow.0);
    h.rakumux().await;
    assert!(!h.app.feed_has_keys());
    // Hidden: the session gets its columns back.
    h.fake.outgoing(index);
    h.press(KeyCode::Char('f')).await;
    h.press(KeyCode::Char('f')).await;
    assert!(!h.app.feed_open());
    assert_eq!(h.app.terminal_size(), wide);
    assert_eq!(
        h.fake.outgoing(index),
        vec![AttachOutgoing::Command(StreamCommand::Fit {
            cols: wide.0,
            rows: wide.1
        })]
    );
}

#[tokio::test]
async fn dragging_the_line_left_of_the_feed_resizes_it_and_saves_its_width() {
    let (mut h, _) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    show_feed(&mut h).await;
    let line = h.app.screen().feed_divider.unwrap();
    let mouse = |kind, column| MouseEvent {
        kind,
        column,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    h.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), line.x));
    assert!(h.app.feed_divider.dragging);
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 120));
    assert_eq!(h.app.screen().feed.unwrap().width, 39);
    assert_eq!(h.app.terminal_size().0, 120 - 49);
    // Never narrower than its minimum, nor so wide the session can't keep
    // its columns beside the list.
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 150));
    assert_eq!(h.app.screen().feed.unwrap().width, layout::FEED_MIN_COLS);
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 20));
    let screen = h.app.screen();
    assert_eq!(screen.feed.unwrap().width, 50);
    assert_eq!(screen.detail.unwrap().width, layout::FEED_SESSION_MIN_COLS);
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 120));
    h.app
        .handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 120));
    assert!(!h.app.feed_divider.dragging);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.home.path().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(saved["sessions"]["feed_width"], 39);
    // The feed keeps the keys it had; the list's divider still keeps the
    // docked session's columns.
    assert!(h.app.feed_has_keys());
    let divider = h.app.screen().divider.unwrap();
    h.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider.x));
    h.app
        .handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 110));
    h.app
        .handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 110));
    let screen = h.app.screen();
    assert_eq!(screen.detail.unwrap().width, layout::FEED_SESSION_MIN_COLS);
    assert_eq!(screen.feed.unwrap().width, 39);
}

#[tokio::test]
async fn a_click_gives_the_keys_to_the_side_it_lands_on_and_a_second_opens_a_card() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    here_and_elsewhere(&account);
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    assert_eq!(ids(&h.app.feed), ["L", "R"]);
    // The list's row: the list has the keys; the feed's band: the feed.
    let row = target(&mut h, Hit::ListRow(2));
    click_at(&mut h, row.x + 4, row.y);
    assert!(!h.app.feed_has_keys());
    assert_eq!(h.app.focus, Focus::List);
    let feed = h.app.screen().feed.unwrap();
    click_at(&mut h, feed.x + 10, feed.y + 1);
    assert!(h.app.feed_has_keys());
    // A card: selected; again: read whole in the cards' place.
    click_card(&mut h, 1);
    assert_eq!(h.app.feed.selected_index(), Some(1));
    assert!(!h.app.feed.reading);
    click_card(&mut h, 1);
    assert!(h.app.feed.reading);
    // The band's ‹ goes back to the cards.
    let back = target(&mut h, Hit::FeedBack);
    click_at(&mut h, back.x + 3, back.y + 1);
    assert!(!h.app.feed.reading);
    // From a session a click on a card gives Rakumux mode the feed's keys,
    // and selects it.
    h.press(KeyCode::Esc).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    click_card(&mut h, 0);
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.feed_has_keys());
    assert_eq!(h.app.feed.selected_index(), Some(0));
    assert!(!h.app.feed.reading);
}

#[tokio::test]
async fn enter_reads_a_reply_in_the_pane_and_again_goes_into_its_session() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    let mut remote = entry("R", "elsewhere", 2_000, Some(2));
    remote.machine = "lab".into();
    account.queue_feed(page(
        1,
        2,
        vec![entry("L", "B", 1_000, Some(1)), remote],
        false,
    ));
    // The sessions were last grouped by app.
    h.app.show_tab(Grouping::App);
    show_feed(&mut h).await;
    assert_eq!(ids(&h.app.feed), ["R", "L"]);
    // Enter: the reply whole in the cards' place; ↓ scrolls it, gg back.
    h.press(KeyCode::Enter).await;
    assert!(h.app.feed.reading);
    assert!(h.app.feed_reply_shown());
    h.press(KeyCode::Down).await;
    assert_eq!(h.app.feed.scroll, 1);
    assert_eq!(h.app.feed.selected_index(), Some(0));
    h.press(KeyCode::Char('g')).await;
    h.press(KeyCode::Char('g')).await;
    assert_eq!(h.app.feed.scroll, 0);
    // Enter again: it runs elsewhere, which it says.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::List);
    let status = h.app.status().unwrap_or_default().to_owned();
    assert!(status.contains("lab"), "{status}");
    // Esc: the cards again, where ↓ moves.
    h.press(KeyCode::Esc).await;
    assert!(!h.app.feed.reading);
    assert!(h.app.feed_has_keys());
    h.press(KeyCode::Down).await;
    assert_eq!(h.app.feed.selected_index(), Some(1));
    // A reply made here: into its session, grouped as it was, the feed
    // beside it.
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.selected().as_deref(), Some("B"));
    assert_eq!(h.app.list.grouping(), Grouping::App, "as they were");
    assert!(h.app.feed_shown());
    assert!(!h.app.feed_has_keys());
    // Back in Rakumux mode the list has the keys.
    h.rakumux().await;
    assert_eq!(h.app.focus, Focus::List);
    assert!(!h.app.feed_has_keys());
    assert!(h.app.feed_shown());
}

#[tokio::test]
async fn in_tabs_the_feed_is_a_tab_of_its_own() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    account.queue_feed(page(1, 1, vec![entry("L", "B", 1_000, Some(1))], false));
    let before = h.app.screen();
    let selected = h.selected();
    assert!(!h.app.feed_tab_shown());
    show_feed(&mut h).await;
    assert!(h.app.feed_tab_shown());
    // The tab bar stays; the feed fills the rest, its cards beside the
    // selected reply.
    let screen = h.app.screen();
    assert_eq!(screen.tabs, before.tabs);
    assert_eq!(screen.feed, before.detail);
    assert!(screen.detail.is_none() && screen.terminal.is_none());
    assert!(h.app.feed_columns().is_some());
    assert!(h.app.feed_reply_shown());
    // Esc: the session's tab you were on; the Feed tab stays.
    h.press(KeyCode::Esc).await;
    assert!(!h.app.feed_has_keys());
    assert!(h.app.feed_tab_shown());
    assert_eq!(h.app.screen(), before);
    assert_eq!(h.selected(), selected);
    // `f` selects it again, then hides it with its tab.
    h.press(KeyCode::Char('f')).await;
    assert!(h.app.feed_has_keys());
    h.press(KeyCode::Char('f')).await;
    assert!(!h.app.feed_tab_shown());
    assert_eq!(h.app.screen(), before);
    assert_eq!(h.selected(), selected);
    // A click on the Feed tab selects it; one on a session's tab goes back
    // to the sessions.
    show_feed(&mut h).await;
    h.press(KeyCode::Esc).await;
    let tab = target(&mut h, Hit::FeedTab);
    click_at(&mut h, tab.x + 1, tab.y);
    assert!(h.app.feed_has_keys());
    draw(&mut h);
    let (_, first) = h.app.tab_hits[0];
    click_at(&mut h, first.x + 1, first.y);
    assert!(!h.app.feed_has_keys());
    assert!(h.app.screen().detail.is_some());
    // Enter on a reply made here: into its session, under its tab.
    h.press(KeyCode::Char('f')).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.selected().as_deref(), Some("B"));
    assert!(h.app.feed_tab_shown());
    assert!(!h.app.feed_has_keys());
    // In session mode the Feed tab stays; a click on it gives Rakumux mode
    // the feed.
    let tab = target(&mut h, Hit::FeedTab);
    click_at(&mut h, tab.x + 1, tab.y);
    assert_eq!(h.app.focus, Focus::List);
    assert!(h.app.feed_has_keys());
    // Hiding the feed while its tab is selected: the session you were on.
    h.press(KeyCode::Char('f')).await;
    assert!(!h.app.feed_tab_shown());
    assert_eq!(h.selected().as_deref(), Some("B"));
    assert!(h.app.screen().detail.is_some());
}

#[tokio::test]
async fn in_grid_the_feed_is_a_pane_beside_the_cards_and_never_touches_them() {
    use session_core::api::{Role, StreamCommand};
    use session_core::client::AttachOutgoing;
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Grid);
    account.queue_feed(page(
        1,
        1,
        vec![
            entry("L", "B", 1_000, Some(1)),
            entry("M", "A", 500, Some(2)),
        ],
        false,
    ));
    let grid = h.app.screen().grid.unwrap();
    assert!(h.app.browsing_grid());
    show_feed(&mut h).await;
    let screen = h.app.screen();
    let feed = screen.feed.unwrap();
    assert_eq!(screen.grid.unwrap().right() + 1, feed.x);
    assert_eq!(feed.right(), grid.right());
    assert!(h.app.browsing_grid());
    assert!(screen.terminal.is_none());
    // Moving through the feed, reading a reply, giving the keys back and
    // hiding it send the cards' sessions nothing.
    for index in 0..h.fake.attach_count() {
        h.fake.outgoing(index);
    }
    for code in [
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Enter,
        KeyCode::Down,
        KeyCode::Esc,
        KeyCode::Esc,
        KeyCode::Char('f'),
        KeyCode::Char('f'),
    ] {
        h.press(code).await;
    }
    assert!(!h.app.feed_open());
    for index in 0..h.fake.attach_count() {
        assert_eq!(h.fake.attach_request(index).role, Role::Observer);
        for message in h.fake.outgoing(index) {
            assert_eq!(message, AttachOutgoing::Command(StreamCommand::Detach));
        }
    }
    assert!(!h.fake.calls().iter().any(|c| c.starts_with("touch ")));
    // A session opened from Grid: under its tabs, the pane beside it.
    show_feed(&mut h).await;
    h.press(KeyCode::Enter).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert_eq!(h.selected().as_deref(), Some("B"));
    let screen = h.app.screen();
    assert_eq!(screen.tabs.unwrap().right() + 1, feed.x);
    assert_eq!(screen.feed, Some(feed));
    assert_eq!(h.app.terminal_size().0, feed.x - 1);
}

#[tokio::test]
async fn on_a_phone_the_feed_fills_the_screen_while_it_has_the_keys() {
    for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
        let (mut h, account) = feed_harness(FakeAccount::with_feed(), layout);
        h.app.resize(45, 40);
        here_and_elsewhere(&account);
        show_feed(&mut h).await;
        tick(&mut h, Duration::from_secs(3));
        let screen = h.app.screen();
        assert_eq!(screen.feed, Some(ratatui::layout::Rect::new(0, 0, 45, 39)));
        assert!(screen.list.is_none() && screen.tabs.is_none(), "{layout:?}");
        assert!(screen.detail.is_none(), "{layout:?}");
        assert!(!h.app.feed_tab_shown(), "{layout:?}");
        // Enter reads a reply; Esc goes back to the cards, then hides the
        // feed.
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Enter).await;
        assert!(h.app.feed.reading);
        h.press(KeyCode::Esc).await;
        assert!(!h.app.feed.reading);
        assert!(h.app.feed_has_keys());
        h.press(KeyCode::Esc).await;
        assert!(!h.app.feed_open(), "{layout:?}");
        assert!(h.app.screen().feed.is_none());
        // A tap on a card reads it at once; Enter goes into its session,
        // full screen.
        show_feed(&mut h).await;
        click_card(&mut h, 0);
        assert!(h.app.feed.reading);
        h.press(KeyCode::Enter).await;
        assert_eq!(h.app.focus, Focus::Session, "{layout:?}");
        assert_eq!(h.selected().as_deref(), Some("B"));
        assert!(h.app.screen().feed.is_none(), "{layout:?}");
    }
}

#[tokio::test]
async fn too_narrow_to_dock_it_the_feed_takes_the_sessions_place_in_rakumux_mode() {
    let (mut h, _) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    h.app.resize(120, 40);
    let before = h.app.screen();
    let size = h.app.terminal_size();
    show_feed(&mut h).await;
    // The list stays; the feed is where the session was.
    let screen = h.app.screen();
    assert_eq!(screen.list, before.list);
    assert_eq!(screen.feed, before.detail);
    assert!(screen.terminal.is_none() && screen.feed_divider.is_none());
    assert_eq!(h.app.terminal_size(), size);
    // The list takes the keys back; the feed stays.
    h.press(KeyCode::Esc).await;
    assert!(!h.app.feed_has_keys());
    assert_eq!(h.app.screen().feed, before.detail);
    // A session keeps every column: no feed beside it.
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::Session);
    assert!(h.app.screen().feed.is_none());
    assert_eq!(h.app.screen().terminal, before.detail);
    h.rakumux().await;
    // Hiding the list makes room: the feed docks beside the session.
    h.press(KeyCode::Char('b')).await;
    let screen = h.app.screen();
    assert!(screen.rail.is_some() && screen.feed_divider.is_some());
    assert!(screen.detail.unwrap().width >= layout::FEED_SESSION_MIN_COLS);
    assert_eq!(screen.feed.unwrap().right(), 120);
    // Stacked (a narrow window): under the list, where the session was.
    h.press(KeyCode::Char('b')).await;
    h.app.resize(90, 40);
    let screen = h.app.screen();
    assert!(screen.stacked);
    assert_eq!(screen.feed.unwrap().y, screen.divider.unwrap().bottom());
    assert!(screen.detail.is_none());
    // Grid that narrow: the cards give way while the feed has the keys,
    // and Esc hides it.
    h.app.set_layout(SessionLayout::Grid);
    h.press(KeyCode::Char('f')).await;
    assert!(h.app.feed_has_keys());
    let screen = h.app.screen();
    assert!(screen.grid.is_none());
    assert_eq!(screen.feed, Some(ratatui::layout::Rect::new(0, 0, 90, 39)));
    assert!(!h.app.browsing_grid());
    h.press(KeyCode::Esc).await;
    assert!(!h.app.feed_open());
    assert!(h.app.browsing_grid());
}

#[tokio::test]
async fn the_feed_is_asked_every_two_seconds_while_shown_and_every_ten_while_kept() {
    let (mut h, account) = feed_harness(FakeAccount::signed_in(), SessionLayout::Sidebar);
    account.queue_feed(page(7, 1, vec![entry("A", "S", 1, Some(1))], true));
    account.queue_feed(page(7, 2, vec![entry("B", "S", 2, Some(2))], false));
    show_feed(&mut h).await;
    // The first page said more: asked again at once from where it ended.
    assert_eq!(account.feed_asks(), [(None, None), (Some(1), Some(7))]);
    assert_eq!(account.feed_shown(), [true, true]);
    assert_eq!(ids(&h.app.feed), ["B", "A"]);
    tick(&mut h, Duration::from_millis(500));
    assert_eq!(account.feed_asks().len(), 2, "not before 2 s");
    tick(&mut h, Duration::from_secs(3));
    assert_eq!(account.feed_asks().last(), Some(&(Some(2), Some(7))));
    // Hidden with the switch off: no more questions.
    h.press(KeyCode::Char('f')).await;
    let asked = account.feed_asks().len();
    tick(&mut h, Duration::from_secs(30));
    assert_eq!(account.feed_asks().len(), asked);
    // With it on: every 10 s, not counting as shown.
    *account.account.lock().unwrap() = FakeAccount::with_feed().account.lock().unwrap().clone();
    h.app.check_account();
    h.app.process_pending();
    tick(&mut h, Duration::from_secs(30));
    assert_eq!(account.feed_asks().len(), asked + 1);
    assert_eq!(account.feed_shown().last(), Some(&false));
    tick(&mut h, Duration::from_secs(35));
    assert_eq!(account.feed_asks().len(), asked + 1, "not before 10 s");
    tick(&mut h, Duration::from_secs(41));
    assert_eq!(account.feed_asks().len(), asked + 2);
}

#[tokio::test]
async fn replies_that_came_while_the_feed_was_hidden_are_counted_until_it_shows() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    // What finished before this window started isn't new.
    account.queue_feed(page(1, 1, vec![entry("old", "S", 1_000, Some(1))], false));
    tick(&mut h, Duration::from_millis(10));
    assert_eq!(ids(&h.app.feed), ["old"]);
    assert_eq!(h.app.feed_unseen(), 0);
    // Two arrive while the sessions show.
    account.queue_feed(page(
        1,
        3,
        vec![fresh("N1", "S", 2), fresh("N2", "S", 3)],
        false,
    ));
    tick(&mut h, Duration::from_secs(11));
    assert_eq!(h.app.feed_unseen(), 2);
    // Shown: none is new, and they stay seen once it's hidden again.
    show_feed(&mut h).await;
    assert_eq!(h.app.feed_unseen(), 0);
    h.press(KeyCode::Char('f')).await;
    assert_eq!(h.app.feed_unseen(), 0);
    // One more, then the account signs out and in: what was seen stays seen.
    account.queue_feed(page(1, 4, vec![fresh("N3", "S", 4)], false));
    tick(&mut h, Duration::from_secs(30));
    assert_eq!(h.app.feed_unseen(), 1);
    h.app.feed.clear();
    account.queue_feed(page(
        2,
        1,
        vec![
            fresh("N1", "S", 2),
            fresh("N2", "S", 3),
            fresh("N3", "S", 4),
        ],
        false,
    ));
    tick(&mut h, Duration::from_secs(60));
    assert_eq!(ids(&h.app.feed).len(), 3);
    assert_eq!(h.app.feed_unseen(), 1);
}

#[tokio::test]
async fn signing_out_empties_the_feed() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    account.queue_feed(page(1, 1, vec![entry("A", "S", 1, Some(1))], false));
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    assert_eq!(h.app.feed.entries().len(), 1);
    h.app.sign_out();
    h.app.process_pending();
    assert!(h.app.feed.entries().is_empty());
}

fn said(position: u64, role: TranscriptRole, parts: Vec<TranscriptPart>) -> TranscriptMessage {
    TranscriptMessage {
        id: position.to_string(),
        position,
        role,
        at: None,
        parts,
    }
}

fn turn(messages: Vec<TranscriptMessage>, next: Option<u64>) -> FeedTurn {
    FeedTurn {
        status: FeedTurnStatus::Ready,
        messages,
        next,
    }
}

// The reply's own tests read it in the Feed tab (Tabs), where it shows
// beside the cards, as wide as the session would be.

/// The cells of the row that shows `needle`, left to right, that have the
/// prompt bar's background (the storm `surface` or, selected, `selection`).
fn bar_cells(h: &mut Harness, needle: &str) -> Vec<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let p = crate::theme::palette();
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer.get(x, y).symbol().to_string())
            .collect();
        if let Some(at) = row.find(needle) {
            let column = row[..at].chars().count() as u16;
            if column > 48 {
                continue;
            }
            return (0..48)
                .filter(|x| {
                    let bg = buffer.get(*x, y).bg;
                    bg == p.surface || bg == p.selection
                })
                .map(|x| buffer.get(x, y).symbol().to_string())
                .collect();
        }
    }
    panic!("{needle:?} not on screen");
}

fn screen(h: &mut Harness) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer.get(x, y).symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_shown_reply_brings_its_whole_turn_in_order() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    let mut remote = entry("R", "elsewhere", 2_000, Some(2));
    remote.machine = "lab".into();
    account.queue_feed(page(1, 1, vec![remote], false));
    account.turns.lock().unwrap().extend([
        turn(
            vec![
                said(
                    0,
                    TranscriptRole::User,
                    vec![TranscriptPart::Text {
                        text: "Fix the flaky test".into(),
                        truncated: false,
                    }],
                ),
                said(
                    5,
                    TranscriptRole::Assistant,
                    vec![
                        TranscriptPart::Reasoning {
                            text: "It depends on map order.".into(),
                            truncated: false,
                        },
                        TranscriptPart::ToolCall {
                            call_id: Some("c1".into()),
                            name: "Bash".into(),
                            input: r#"{"command":"cargo test","description":"Run the tests"}"#
                                .into(),
                            truncated: false,
                        },
                    ],
                ),
            ],
            Some(5),
        ),
        turn(
            vec![
                said(
                    9,
                    TranscriptRole::Tool,
                    vec![TranscriptPart::ToolResult {
                        call_id: Some("c1".into()),
                        output: (1..=9).map(|n| format!("line {n}\n")).collect(),
                        error: false,
                        truncated: false,
                    }],
                ),
                said(
                    12,
                    TranscriptRole::Assistant,
                    vec![TranscriptPart::Text {
                        text: "Fixed: the test sorts the keys.".into(),
                        truncated: false,
                    }],
                ),
            ],
            None,
        ),
    ]);
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    // Asked for once it's shown, then the rest at once.
    assert_eq!(
        *account.turn_asks.lock().unwrap(),
        [("R".to_owned(), None), ("R".to_owned(), Some(5))]
    );
    let text = screen(&mut h);
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in\n{text}"))
    };
    assert!(at(" Fix the flaky test") < at("It depends on map order."));
    assert!(at("It depends on map order.") < at("● Bash Run the tests"));
    assert!(at("● Bash Run the tests") < at("⎿ line 1"));
    assert!(at("line 6") < at("… 3 more lines"));
    assert!(!text.contains("line 7"));
    assert!(at("… 3 more lines") < at("Fixed: the test sorts the keys."));
    // Kept: nothing asked again on the next ticks.
    tick(&mut h, Duration::from_secs(6));
    assert_eq!(account.turn_asks.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_turn_that_cant_be_read_shows_the_reply_alone() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    account.queue_feed(page(1, 1, vec![entry("A", "S", 1_000, Some(1))], false));
    account
        .turns
        .lock()
        .unwrap()
        .push_back(FeedTurn::failed(rakumux_sensagent::feed::FeedFailure::Off));
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    let text = screen(&mut h);
    assert!(text.contains("Reply A"), "{text}");
    assert!(!text.contains("Loading the whole turn"), "{text}");
}

fn written(id: &str, entry: &str, thread: &str, created_at: i64, status: ReplyStatus) -> FeedReply {
    FeedReply {
        seq: 1,
        id: id.into(),
        entry: entry.into(),
        thread: thread.into(),
        machine: "studio".into(),
        session_id: "S".into(),
        session_name: "session S".into(),
        folder: Some("repo".into()),
        agent: "claude-code".into(),
        conversation_id: "c".into(),
        text: format!("Reply back {id}"),
        attachments: Vec::new(),
        from: Some(rakumux_sensagent::feed::machine_name()),
        status,
        failure: None,
        response: None,
        created_at,
        status_at: created_at,
    }
}

#[test]
fn a_thread_is_its_first_entry_the_replies_and_their_answers_in_order() {
    let mut store = FeedStore::new("studio".into());
    let root = entry("E1", "S", 1_000, Some(1));
    let mut answer = entry("E2", "S", 3_000, Some(3));
    answer.reply_to = Some("R1".into());
    answer.thread = Some("E1".into());
    let other = entry("E3", "S", 4_000, Some(4));
    let mut p = page(1, 2, vec![root, answer, other], false);
    p.replies = vec![written("R1", "E1", "E1", 2_000, ReplyStatus::Delivered)];
    store.apply(p);
    let steps = |store: &FeedStore, id: &str| -> Vec<String> {
        let entry = store.entries().iter().find(|e| e.id == id).unwrap().clone();
        store
            .thread(&entry)
            .into_iter()
            .map(|item| match item {
                ThreadItem::Entry(entry) => entry.id.clone(),
                ThreadItem::Reply(reply) => reply.id.clone(),
            })
            .collect()
    };
    assert_eq!(steps(&store, "E1"), ["E1", "R1", "E2"]);
    assert_eq!(steps(&store, "E2"), ["E1", "R1", "E2"]);
    assert!(steps(&store, "E3").is_empty(), "nobody replied");
    // A newer status replaces the held one; an older doesn't.
    let mut answered = written("R1", "E1", "E1", 2_000, ReplyStatus::Delivered);
    answered.seq = 5;
    answered.response = Some("E2".into());
    store.take_reply(answered);
    store.take_reply(written("R1", "E1", "E1", 2_000, ReplyStatus::Pending));
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "E1")
        .unwrap()
        .clone();
    let thread = store.thread(&entry);
    assert!(
        matches!(thread[1], ThreadItem::Reply(reply) if reply.response.as_deref() == Some("E2"))
    );
}

fn conversing(id: &str) -> FeedEntry {
    let mut entry = entry(id, "S", 1_000, Some(1));
    entry.conversation_id = Some("c".into());
    entry
}

#[tokio::test]
async fn r_writes_back_to_a_reply_which_then_shows_in_its_thread() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    account.queue_feed(page(1, 1, vec![conversing("A")], false));
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    account.reply_answers.lock().unwrap().push_back(ReplyWrite {
        status: ReplyWriteStatus::Sent,
        reply: Some(written("R1", "A", "A", 2_000, ReplyStatus::Pending)),
    });
    h.press(KeyCode::Char('r')).await;
    assert!(matches!(h.app.mode, AppMode::FeedReply(_)));
    // Empty, it isn't sent.
    h.press(KeyCode::Enter).await;
    assert!(account.replies_written.lock().unwrap().is_empty());
    h.type_text("Now add a test").await;
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    assert_eq!(
        *account.replies_written.lock().unwrap(),
        [("A".to_owned(), "Now add a test".to_owned())]
    );
    assert!(matches!(h.app.mode, AppMode::Normal));
    let status = h.app.status().unwrap_or_default().to_owned();
    assert!(status.contains("Sent"), "{status}");
    let text = screen(&mut h);
    assert!(text.contains("Thread"), "{text}");
    assert!(text.contains("↩ you · "), "{text}");
    assert!(text.contains("waiting for its agent"), "{text}");
    assert!(text.contains("Reply back R1"), "{text}");
}

#[tokio::test]
async fn only_a_sessions_newest_message_can_be_answered() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    let mut newer = conversing("B");
    newer.finished_at = 5_000;
    newer.seq = Some(2);
    account.queue_feed(page(1, 1, vec![conversing("A"), newer], false));
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    // The newest, B, has the button; A, older in the same session, doesn't.
    assert!(screen(&mut h).contains("↩ Reply  r"));
    h.press(KeyCode::Down).await;
    assert!(!screen(&mut h).contains("↩ Reply"));
    h.press(KeyCode::Char('r')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h
        .app
        .status()
        .unwrap_or_default()
        .contains("newest message"));
    // A click on the button opens the box, as `r` does.
    h.press(KeyCode::Up).await;
    let _ = screen(&mut h);
    let button = h
        .app
        .hits
        .iter()
        .find(|(_, hit)| *hit == Hit::FeedReply)
        .map(|(rect, _)| *rect)
        .expect("a Reply button");
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        h.app.handle_mouse(MouseEvent {
            kind,
            column: button.x,
            row: button.y,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert!(matches!(h.app.mode, AppMode::FeedReply(_)));
}

#[tokio::test]
async fn a_reply_that_isnt_stored_keeps_its_box_open_and_says_why() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    let unknown = entry("U", "T", 2_000, Some(2));
    account.queue_feed(page(1, 1, vec![conversing("A"), unknown], false));
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    // The newest has no known chat: nothing to write back to.
    h.press(KeyCode::Char('r')).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
    assert!(h
        .app
        .status()
        .unwrap_or_default()
        .contains("can't be answered"));

    h.press(KeyCode::Down).await;
    account.reply_answers.lock().unwrap().push_back(ReplyWrite {
        status: ReplyWriteStatus::Off,
        reply: None,
    });
    h.press(KeyCode::Char('r')).await;
    h.type_text("Try again").await;
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    let AppMode::FeedReply(dialog) = &h.app.mode else {
        panic!("the box closed");
    };
    assert!(dialog
        .error_message
        .as_deref()
        .unwrap_or_default()
        .contains("feed is off"));
    assert_eq!(dialog.input.value(), "Try again");
    h.press(KeyCode::Esc).await;
    assert!(matches!(h.app.mode, AppMode::Normal));
}

#[tokio::test]
async fn a_reply_sent_again_keeps_its_id_until_its_text_changes() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    account.queue_feed(page(1, 1, vec![conversing("A")], false));
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    // rakumux.com couldn't be reached twice (the second time, it may have
    // stored the reply before the answer was lost), then it takes it.
    for status in [ReplyWriteStatus::Unavailable, ReplyWriteStatus::Unavailable] {
        account.reply_answers.lock().unwrap().push_back(ReplyWrite {
            status,
            reply: None,
        });
    }
    h.press(KeyCode::Char('r')).await;
    h.type_text("Run it").await;
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    // Changed, it's another reply.
    h.type_text(" again").await;
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    assert!(matches!(h.app.mode, AppMode::Normal));
    let ids = account.reply_ids.lock().unwrap().clone();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids[0], ids[1], "the same text keeps its id");
    assert_ne!(ids[1], ids[2], "a changed text gets a new one");
    assert!(ids
        .iter()
        .all(|id| rakumux_sensagent::feed::is_entry_id(id)));
    // A new reply box makes a new id.
    account.queue_feed(page(2, 1, vec![conversing("A")], false));
    h.press(KeyCode::Char('r')).await;
    h.type_text("Run it").await;
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    let ids = account.reply_ids.lock().unwrap().clone();
    assert!(!ids[..3].contains(&ids[3]));
}

#[tokio::test]
async fn replies_show_their_markdown_drawn_not_its_markers() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    let mut done = conversing("A");
    done.text = "## Fixed\n\nThe **parser** passes now:\n\n- `cargo test`\n- the e2e run".into();
    account.queue_feed(page(1, 1, vec![done], false));
    account
        .turns
        .lock()
        .unwrap()
        .push_back(FeedTurn::failed(rakumux_sensagent::feed::FeedFailure::Off));
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    let text = screen(&mut h);
    assert!(text.contains("• cargo test"), "{text}");
    assert!(text.contains("The parser passes now:"), "{text}");
    assert!(!text.contains("**"), "{text}");
    assert!(!text.contains("## Fixed"), "{text}");
}

fn asked(id: &str, session: &str, at: i64, text: &str) -> rakumux_sensagent::feed::FeedPrompt {
    let _ = session;
    rakumux_sensagent::feed::FeedPrompt {
        id: id.into(),
        text: text.into(),
        truncated: false,
        at: Some(at),
    }
}

fn open_turn(
    prompt: &str,
    session: &str,
    at: i64,
    text: &str,
) -> rakumux_sensagent::feed::OpenTurn {
    rakumux_sensagent::feed::OpenTurn {
        machine: "studio".into(),
        session_id: session.into(),
        session_name: format!("session {session}"),
        folder: Some("repo".into()),
        branch: Some("main".into()),
        agent: "claude-code".into(),
        conversation_id: "c".into(),
        prompt: asked(prompt, session, at, text),
    }
}

#[test]
fn a_prompt_is_a_card_until_its_answer_takes_it_over() {
    let mut store = FeedStore::new("studio".into());
    let mut first = page(1, 1, vec![conversing("A")], false);
    first.open = Some(vec![open_turn("p2", "S", 2_000, "Now add a test")]);
    store.apply(first);
    let keys =
        |store: &FeedStore| -> Vec<String> { store.cards().iter().map(super::Card::key).collect() };
    // The waiting prompt is newest: it leads, and is selected.
    assert_eq!(keys(&store), ["open:p2", "A"]);
    assert_eq!(store.selected_card().unwrap().key(), "open:p2");
    assert!(store.selected().is_none(), "no entry yet");

    // Its answer arrives with the same prompt: one card, still selected.
    let mut answer = conversing("B");
    answer.finished_at = 3_000;
    answer.seq = Some(2);
    answer.prompt = Some(asked("p2", "S", 2_000, "Now add a test"));
    let mut second = page(1, 2, vec![answer], false);
    second.open = Some(Vec::new());
    store.apply(second);
    assert_eq!(keys(&store), ["B", "A"]);
    assert_eq!(store.selected().unwrap().id, "B");
}

#[test]
fn this_machines_answer_not_yet_uploaded_borrows_its_prompt() {
    let mut store = FeedStore::new("studio".into());
    let mut pending = conversing("L");
    pending.seq = None;
    pending.finished_at = 5_000;
    let mut read = page(1, 1, vec![pending], false);
    read.open = Some(vec![open_turn("p1", "S", 4_000, "Fix the parser")]);
    store.apply(read);
    let cards = store.cards();
    assert_eq!(cards.len(), 1, "the prompt's card is the answer's");
    assert_eq!(cards[0].prompt().unwrap().text, "Fix the parser");
}

#[tokio::test]
async fn cards_show_the_prompt_then_the_answer_or_that_it_is_coming() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    let mut done = conversing("A");
    done.prompt = Some(asked("p1", "S", 500, "Fix the flaky parser test"));
    let mut read = page(1, 1, vec![done], false);
    read.open = Some(vec![open_turn("p2", "T", 2_000, "Now the docs")]);
    account.queue_feed(read);
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    let text = screen(&mut h);
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in\n{text}"))
    };
    // Newest first: the waiting prompt, then the finished turn.
    assert!(at("│  Now the docs") < at("waiting for the answer…"));
    assert!(at("waiting for the answer…") < at("│  Fix the flaky parser test"));
    assert!(at("│  Fix the flaky parser test") < at("Reply A"));
    assert!(
        !text.contains("›"),
        "prompts sit on a bar, not behind a chevron"
    );
    // The prompt's bar runs from the card's border to its border.
    let bar = bar_cells(&mut h, "Fix the flaky parser test");
    assert!(bar.len() >= 40, "{bar:?}");
    assert!(text.contains(" ago · working"), "{text}");
    // The waiting prompt's detail says its answer comes there.
    assert!(text.contains("Waiting for the answer…"), "{text}");
}

#[test]
fn a_waiting_prompt_replies_to_its_chats_newest_entry_else_to_its_session() {
    use super::{Card, ReplyTo};
    let reply_to = |store: &FeedStore, key: &str| {
        let card = store
            .cards()
            .into_iter()
            .find(|card| card.key() == key)
            .unwrap_or_else(|| panic!("no card {key}"));
        store.reply_to(card)
    };
    let to_session = |session: &str| ReplyTo::Session {
        machine: "studio".into(),
        session: session.into(),
    };

    // A prompt waiting in the chat of the session's newest entry: the reply
    // goes to that entry, so its thread goes on; the entry's own card, no
    // longer the session's newest, takes none.
    let mut store = FeedStore::new("studio".into());
    let mut read = page(1, 1, vec![conversing("A")], false);
    read.open = Some(vec![open_turn("p2", "S", 2_000, "Now add a test")]);
    store.apply(read);
    assert_eq!(
        reply_to(&store, "open:p2"),
        Some(ReplyTo::Entry("A".into()))
    );
    assert_eq!(reply_to(&store, "A"), None);

    // In a new chat, or a session with no entry yet: the session itself.
    let mut store = FeedStore::new("studio".into());
    let mut fresh_chat = open_turn("p3", "S", 2_000, "Start on the parser");
    fresh_chat.conversation_id = "new".into();
    let mut read = page(1, 1, vec![conversing("A")], false);
    read.open = Some(vec![fresh_chat, open_turn("p4", "T", 2_500, "Hello")]);
    store.apply(read);
    assert_eq!(reply_to(&store, "open:p3"), Some(to_session("S")));
    assert_eq!(reply_to(&store, "open:p4"), Some(to_session("T")));

    // The session's newest entry not uploaded yet: rakumux.com doesn't know
    // it, so the session itself.
    let mut store = FeedStore::new("studio".into());
    let mut pending = conversing("L");
    pending.seq = None;
    pending.finished_at = 1_500;
    let mut read = page(1, 1, vec![pending], false);
    read.open = Some(vec![open_turn("p5", "S", 2_000, "And the docs")]);
    store.apply(read);
    assert_eq!(reply_to(&store, "open:p5"), Some(to_session("S")));

    // A prompt older than the session's newest entry (in another chat) isn't
    // the session's newest card: the entry takes the reply.
    let mut store = FeedStore::new("studio".into());
    let mut newer = conversing("B");
    newer.finished_at = 3_000;
    newer.seq = Some(2);
    let mut other_chat = open_turn("p6", "S", 2_000, "Old question");
    other_chat.conversation_id = "other".into();
    let mut read = page(1, 1, vec![newer], false);
    read.open = Some(vec![other_chat]);
    store.apply(read);
    assert_eq!(reply_to(&store, "open:p6"), None);
    assert_eq!(reply_to(&store, "B"), Some(ReplyTo::Entry("B".into())));
    assert!(matches!(store.cards()[0], Card::Entry(_)));
}

#[tokio::test]
async fn r_on_a_working_card_replies_to_its_chat_or_writes_to_its_session() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Tabs);
    let mut read = page(1, 1, vec![conversing("A")], false);
    read.open = Some(vec![open_turn("p2", "S", 2_000, "Now add a test")]);
    account.queue_feed(read);
    show_feed(&mut h).await;
    tick(&mut h, Duration::from_secs(3));
    // The waiting prompt is the session's newest card: it has the button,
    // and the older entry no longer does.
    assert!(screen(&mut h).contains("↩ Reply  r"));
    assert!(h.app.hits.iter().any(|(_, hit)| *hit == Hit::FeedReply));
    h.press(KeyCode::Down).await;
    assert!(!screen(&mut h).contains("↩ Reply"));
    h.press(KeyCode::Up).await;
    // In the entry's chat: the reply goes to the entry.
    h.press(KeyCode::Char('r')).await;
    let AppMode::FeedReply(dialog) = &h.app.mode else {
        panic!("no reply box");
    };
    assert_eq!(dialog.answering, "Now add a test");
    h.type_text("Use the fixtures").await;
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    assert_eq!(
        *account.replies_written.lock().unwrap(),
        [("A".to_owned(), "Use the fixtures".to_owned())]
    );
    assert!(account.messages_written.lock().unwrap().is_empty());
    assert!(matches!(h.app.mode, AppMode::Normal));

    // A prompt in a new chat: a session message to its session.
    let mut fresh_chat = open_turn("p3", "S", 4_000, "Start on the parser");
    fresh_chat.conversation_id = "new".into();
    let mut read = page(1, 2, Vec::new(), false);
    read.open = Some(vec![fresh_chat]);
    account.queue_feed(read);
    tick(&mut h, Duration::from_secs(6));
    h.press(KeyCode::Home).await;
    assert_eq!(h.app.feed.selected_card().unwrap().key(), "open:p3");
    h.press(KeyCode::Char('r')).await;
    h.type_text("Only the lexer").await;
    h.press(KeyCode::Enter).await;
    h.app.process_pending();
    assert_eq!(
        *account.messages_written.lock().unwrap(),
        [(
            "studio".to_owned(),
            "S".to_owned(),
            "Only the lexer".to_owned()
        )]
    );
    assert_eq!(account.replies_written.lock().unwrap().len(), 1);
    assert!(matches!(h.app.mode, AppMode::Normal));
    let status = h.app.status().unwrap_or_default().to_owned();
    assert!(status.contains("Sent"), "{status}");
}

#[test]
fn the_services_name_for_this_machine_replaces_the_hostname() {
    let mut store = FeedStore::new("studio".into());
    store.apply(page(1, 1, vec![], false));
    assert_eq!(store.machine(), "studio", "an older service names none");
    store.apply(FeedPage {
        machine: Some("studio-6m8b".into()),
        ..page(1, 2, vec![], false)
    });
    assert_eq!(store.machine(), "studio-6m8b");
}
