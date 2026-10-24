use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use rakumux_sensagent::feed::{FeedEntry, FeedPage, FeedStatus, FeedTurn, FeedTurnStatus};
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

use super::{FeedStore, MAX_ENTRIES};
use crate::app::grouping::{Grouping, ListTab};
use crate::app::testing::{options, scope, start, view, FakeAccount, FakeApi, Harness};
use crate::app::types::Focus;
use crate::layout::SessionLayout;

pub fn entry(id: &str, session: &str, finished_at: i64, seq: Option<u64>) -> FeedEntry {
    FeedEntry {
        id: id.into(),
        seq,
        machine: "studio".into(),
        session_id: session.into(),
        session_name: format!("session {session}"),
        folder: Some("repo".into()),
        agent: "claude-code".into(),
        conversation_id: None,
        text: format!("Reply {id}"),
        truncated: false,
        finished_at,
        position: None,
    }
}

pub fn page(generation: u64, revision: u64, entries: Vec<FeedEntry>, more: bool) -> FeedPage {
    FeedPage {
        status: FeedStatus::Ready,
        generation,
        revision,
        entries,
        more,
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

#[tokio::test]
async fn the_feed_comes_to_the_front_when_its_switch_is_first_seen_on() {
    let (h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    assert_eq!(h.app.list_tab(), ListTab::Feed);
    // Asked at once, with nothing held yet.
    assert_eq!(account.feed_asks(), [(None, None)]);
    assert_eq!(h.app.feed.status(), Some(FeedStatus::Ready));

    // Off: the start stays under Status and nothing is asked.
    let (h, account) = feed_harness(FakeAccount::signed_in(), SessionLayout::Sidebar);
    assert_eq!(h.app.list_tab(), ListTab::Grouped(Grouping::Status));
    assert!(account.feed_asks().is_empty());
}

#[tokio::test]
async fn a_chosen_tab_is_kept_when_the_switch_turns_on_later() {
    let (mut h, account) = feed_harness(FakeAccount::signed_in(), SessionLayout::Sidebar);
    h.press(KeyCode::Right).await;
    assert_eq!(h.app.list_tab(), ListTab::Grouped(Grouping::Folder));
    *account.account.lock().unwrap() = FakeAccount::with_feed().account.lock().unwrap().clone();
    tick(&mut h, Duration::from_secs(120));
    assert_eq!(h.app.list_tab(), ListTab::Grouped(Grouping::Folder));
    assert!(account.feed_asks().is_empty(), "not asked while hidden");
}

#[tokio::test]
async fn the_feed_is_asked_every_two_seconds_only_while_shown_and_again_while_more() {
    let (mut h, account) = feed_harness(FakeAccount::signed_in(), SessionLayout::Sidebar);
    account.queue_feed(page(7, 1, vec![entry("A", "S", 1, Some(1))], true));
    account.queue_feed(page(7, 2, vec![entry("B", "S", 2, Some(2))], false));
    h.press(KeyCode::Left).await;
    assert_eq!(h.app.list_tab(), ListTab::Feed);
    // The first page said more: asked again at once from where it ended.
    assert_eq!(account.feed_asks(), [(None, None), (Some(1), Some(7))]);
    assert_eq!(ids(&h.app.feed), ["B", "A"]);
    tick(&mut h, Duration::from_millis(500));
    assert_eq!(account.feed_asks().len(), 2, "not before 2 s");
    tick(&mut h, Duration::from_secs(3));
    assert_eq!(account.feed_asks().last(), Some(&(Some(2), Some(7))));
    // Hidden: no more questions.
    h.press(KeyCode::Right).await;
    let asked = account.feed_asks().len();
    tick(&mut h, Duration::from_secs(10));
    assert_eq!(account.feed_asks().len(), asked);
}

#[tokio::test]
async fn arrows_reach_the_feed_only_where_it_can_show() {
    // Grid shows sessions: the tabs stop at Status (Tabs' selector too).
    let (mut h, _) = feed_harness(FakeAccount::signed_in(), SessionLayout::Grid);
    h.app.switch_tab(-1);
    assert_eq!(h.app.list_tab(), ListTab::Grouped(Grouping::Status));
    let (mut h, _) = feed_harness(FakeAccount::signed_in(), SessionLayout::Sidebar);
    h.press(KeyCode::Left).await;
    assert_eq!(h.app.list_tab(), ListTab::Feed);
    // `h`/`l` go along the tabs too: the feed has no top row to select.
    h.press(KeyCode::Char('l')).await;
    assert_eq!(h.app.list_tab(), ListTab::Grouped(Grouping::Status));
    h.press(KeyCode::Left).await;
    h.press(KeyCode::Char('h')).await;
    assert_eq!(h.app.list_tab(), ListTab::Feed);
}

#[tokio::test]
async fn enter_on_a_reply_goes_into_its_session_here_or_says_where_it_is() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    let mut remote = entry("R", "elsewhere", 2_000, Some(2));
    remote.machine = "lab".into();
    account.queue_feed(page(
        1,
        2,
        vec![entry("L", "B", 1_000, Some(1)), remote],
        false,
    ));
    tick(&mut h, Duration::from_secs(3));
    assert_eq!(ids(&h.app.feed), ["R", "L"]);
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.focus, Focus::List);
    assert_eq!(h.app.list_tab(), ListTab::Feed);
    let status = h.app.status().unwrap_or_default().to_owned();
    assert!(status.contains("lab"), "{status}");
    h.press(KeyCode::Down).await;
    h.press(KeyCode::Enter).await;
    assert_eq!(h.app.list_tab(), ListTab::Grouped(Grouping::Status));
    assert_eq!(h.selected().as_deref(), Some("B"));
}

#[tokio::test]
async fn on_a_phone_a_reply_from_elsewhere_fills_the_screen_until_esc() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    h.app.resize(45, 40);
    let mut remote = entry("R", "elsewhere", 2_000, Some(2));
    remote.machine = "lab".into();
    account.queue_feed(page(1, 1, vec![remote], false));
    tick(&mut h, Duration::from_secs(3));
    h.press(KeyCode::Enter).await;
    assert!(h.app.feed.reading);
    h.press(KeyCode::Esc).await;
    assert!(!h.app.feed.reading);
    assert_eq!(h.app.focus, Focus::List);
}

#[tokio::test]
async fn signing_out_empties_the_feed() {
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    account.queue_feed(page(1, 1, vec![entry("A", "S", 1, Some(1))], false));
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
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
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
    assert!(at("› Fix the flaky test") < at("It depends on map order."));
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
    let (mut h, account) = feed_harness(FakeAccount::with_feed(), SessionLayout::Sidebar);
    account.queue_feed(page(1, 1, vec![entry("A", "S", 1_000, Some(1))], false));
    account
        .turns
        .lock()
        .unwrap()
        .push_back(FeedTurn::failed(rakumux_sensagent::feed::FeedFailure::Off));
    tick(&mut h, Duration::from_secs(3));
    let text = screen(&mut h);
    assert!(text.contains("Reply A"), "{text}");
    assert!(!text.contains("Loading the whole turn"), "{text}");
}
