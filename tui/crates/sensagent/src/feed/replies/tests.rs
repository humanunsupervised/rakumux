use std::sync::atomic::Ordering;

use super::*;
use session_core::ids::ReplyId;
use session_core::model::AgentTurn;

use crate::source::fake::live_view;

const NOW: Duration = Duration::from_secs(100);
const CHAT: &str = "5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23";
/// A current daemon: it refuses to type into an agent that asks something
/// (`session.submit.blocked`), so replies go in mid-turn.
const GUARDED: bool = true;
/// An older daemon, without that guard: replies wait for the agent to wait.
const UNGUARDED: bool = false;

fn view(id: &str, lifecycle: Lifecycle, conversation: Option<&str>) -> SessionView {
    let mut view = live_view(&SessionId::from_string(id), "/home/me/rakumux", 1);
    view.lifecycle = lifecycle;
    view.agent_turn = Some(AgentTurn {
        id: 1,
        messages: 3,
        conversation_id: conversation.map(Into::into),
    });
    view
}

fn reply(id: &str, seq: u64, from: Option<&str>) -> FeedReply {
    FeedReply {
        seq,
        id: id.into(),
        entry: "01K8A0RZ0V3X5Q2N9T7W4Y6M8B".into(),
        thread: "01K8A0RZ0V3X5Q2N9T7W4Y6M8B".into(),
        machine: "studio".into(),
        session_id: "S".into(),
        session_name: "fix-tests".into(),
        folder: Some("rakumux".into()),
        agent: "claude-code".into(),
        conversation_id: CHAT.into(),
        text: "Now add a test.".into(),
        attachments: Vec::new(),
        from: from.map(Into::into),
        status: ReplyStatus::Pending,
        failure: None,
        response: None,
        created_at: 1_761_220_000_000,
        status_at: 1_761_220_000_000,
    }
}

fn read(replies: Vec<FeedReply>) -> Result<FeedResult, FeedFailure> {
    let latest_seq = replies.iter().map(|reply| reply.seq).max().unwrap_or(0);
    page_of(replies, latest_seq, latest_seq, false)
}

/// A page of a read: `replies` kept, its rows up to `page_seq`, the account's
/// newest seq `latest_seq`.
fn page_of(
    replies: Vec<FeedReply>,
    page_seq: u64,
    latest_seq: u64,
    more: bool,
) -> Result<FeedResult, FeedFailure> {
    Ok(FeedResult::Replies(RepliesRead {
        replies,
        messages: vec![],
        page_seq,
        latest_seq,
        more,
        poll: None,
        resources_wanted: false,
        new_sessions: vec![],
    }))
}

fn on(views: &[SessionView]) -> ReplySync {
    let mut replies = ReplySync::new("studio".into(), None);
    replies.set_enabled(true, NOW);
    replies.connected(views, GUARDED);
    replies
}

fn read_after(work: Option<ReplyWork>) -> u64 {
    match work {
        Some(ReplyWork::Request(FeedRequest::Replies { after, machine, .. })) => {
            assert_eq!(machine, "studio", "the read names this machine");
            after
        }
        other => panic!("expected a read, got {other:?}"),
    }
}

fn report_of(work: Option<ReplyWork>) -> serde_json::Value {
    match work {
        Some(ReplyWork::Request(FeedRequest::ReplyStatus { body })) => {
            serde_json::from_slice(&body).unwrap()
        }
        other => panic!("expected a report, got {other:?}"),
    }
}

/// Read `pending`, then return what comes next.
fn after_reading(replies: &mut ReplySync, pending: Vec<FeedReply>) -> Option<ReplyWork> {
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read(pending), NOW);
    replies.next_work(NOW)
}

/// A turn id no view had yet.
fn new_turn() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1_000);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Session S's view with a new prompt in its chat, `prompt` its first line.
fn prompted(lifecycle: Lifecycle, conversation: Option<&str>, prompt: &str) -> SessionView {
    let mut view = view("S", lifecycle, conversation);
    view.agent_turn.as_mut().unwrap().id = new_turn();
    view.user_prompt = Some(prompt.into());
    view
}

/// Session S's agent is seen working on a new prompt, what was typed in:
/// it took it.
fn taken(replies: &mut ReplySync) {
    let sent = replies.deliveries[&SessionId::from_string("S")]
        .last()
        .unwrap()
        .sent
        .clone();
    replies.observe(&prompted(Lifecycle::Working, Some(CHAT), &sent));
}

fn answer(conversation: Option<&str>, finished_at: i64) -> AgentReply {
    AgentReply {
        id: ReplyId::generate(),
        session_id: SessionId::from_string("S"),
        agent: "claude-code".into(),
        conversation_id: conversation.map(Into::into),
        text: "Added it.".into(),
        truncated: false,
        finished_at,
        position: None,
    }
}

#[test]
fn a_reply_written_here_is_typed_reported_and_answered() {
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let pending = reply("01K8A1", 4, Some("studio"));
    let work = after_reading(&mut replies, vec![pending]);
    assert_eq!(
        work,
        Some(ReplyWork::Submit {
            reply: "01K8A1".into(),
            session: SessionId::from_string("S"),
            text: "Now add a test.".into(),
            foreground: Some("synthetic-agent".into()),
        })
    );
    // Typed once, whatever is read meanwhile.
    assert_eq!(replies.next_work(NOW), None);
    replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
    // Delivered once its agent is seen to take it.
    assert_eq!(replies.next_work(NOW), None);
    taken(&mut replies);
    let body = report_of(replies.next_work(NOW));
    assert_eq!(body["id"], "01K8A1");
    assert_eq!(body["machine"], "studio");
    assert_eq!(body["status"], "delivered");
    assert!(body.get("failure").is_none());
    let mut delivered = reply("01K8A1", 5, Some("studio"));
    delivered.status = ReplyStatus::Delivered;
    let shown = replies.request_done(true, Ok(FeedResult::Reply(delivered.clone())), NOW);
    assert_eq!(shown, vec![delivered]);

    // A reply from another chat, or from before, isn't the answer.
    assert_eq!(replies.answered(&answer(Some("other"), 3_000)), None);
    assert_eq!(replies.answered(&answer(Some(CHAT), 1_000)), None);
    assert_eq!(
        replies.answered(&answer(Some(CHAT), 3_000)),
        Some("01K8A1".into())
    );
    assert_eq!(replies.answered(&answer(Some(CHAT), 4_000)), None);
}

#[test]
fn a_reply_to_github_copilot_is_typed_while_it_runs_and_confirmed_by_its_prompt() {
    // Copilot runs as `node` (npm's loader) or its native binary (`MainThread`
    // on Linux, `copilot` on macOS).
    for foreground in ["node", "MainThread", "copilot"] {
        let mut shown = view("S", Lifecycle::Done, Some(CHAT));
        shown.terminal.as_mut().unwrap().foreground_process = Some(foreground.into());
        let mut replies = on(&[shown]);
        let mut pending = reply("01K8A1", 4, Some("studio"));
        pending.agent = "copilot".into();
        assert_eq!(
            after_reading(&mut replies, vec![pending]),
            Some(ReplyWork::Submit {
                reply: "01K8A1".into(),
                session: SessionId::from_string("S"),
                text: "Now add a test.".into(),
                foreground: Some(foreground.into()),
            })
        );
        replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
        // Copilot logs the paste as the next turn's prompt.
        replies.observe(&prompted(Lifecycle::Working, Some(CHAT), "Now add a test."));
        let body = report_of(replies.next_work(NOW));
        assert_eq!(body["status"], "delivered", "{foreground}");
    }
}

#[test]
fn a_busy_or_blocked_agent_is_waited_for() {
    for lifecycle in [Lifecycle::Working, Lifecycle::Blocked, Lifecycle::Unknown] {
        let mut replies = on(&[view("S", lifecycle, Some(CHAT))]);
        let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
        assert_eq!(work, None, "{lifecycle:?}");
        replies.observe(&view("S", Lifecycle::Idle, Some(CHAT)));
        assert!(matches!(
            replies.next_work(NOW),
            Some(ReplyWork::Submit { .. })
        ));
    }
    // A chat not known yet is waited for too.
    let mut replies = on(&[view("S", Lifecycle::Done, None)]);
    let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    assert_eq!(work, None);
}

#[test]
fn one_reply_at_a_time_goes_into_a_session() {
    let mut replies = on(&[view("S", Lifecycle::Idle, Some(CHAT))]);
    let first = reply("01K8A1", 4, Some("studio"));
    let second = reply("01K8A2", 5, Some("studio"));
    let work = after_reading(&mut replies, vec![second, first]);
    assert!(matches!(work, Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"));
    replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
    // Its report goes once its agent took it; the second waits for the agent
    // to wait again.
    taken(&mut replies);
    report_of(replies.next_work(NOW));
    assert_eq!(replies.next_work(NOW), None);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A2"
    ));
}

#[test]
fn a_session_being_typed_into_takes_nothing_else_meanwhile() {
    let mut replies = on(&[view("S", Lifecycle::Idle, Some(CHAT))]);
    let first = reply("01K8A1", 4, Some("studio"));
    let second = reply("01K8A2", 5, Some("studio"));
    let work = after_reading(&mut replies, vec![second, first]);
    assert!(matches!(work, Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"));
    // The service asks again before the daemon has answered the first.
    assert!(!matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { .. })
    ));
    replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
    taken(&mut replies);
    report_of(replies.next_work(NOW));
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A2"
    ));
}

#[test]
fn a_session_someone_is_typing_in_is_tried_again_once_they_pause() {
    let mut replies = on(&[view("S", Lifecycle::Idle, Some(CHAT))]);
    let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    assert!(matches!(work, Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"));
    // The daemon refused: someone typed there. It stays pending, unreported…
    replies.typing("01K8A1".into(), NOW);
    let meanwhile = replies.next_work(NOW + TYPING_HOLD / 2);
    assert!(!matches!(meanwhile, Some(ReplyWork::Submit { .. })));
    assert!(!matches!(
        meanwhile,
        Some(ReplyWork::Request(FeedRequest::ReplyStatus { .. }))
    ));
    // …and is typed in once the hold is over.
    assert!(matches!(
        replies.next_work(NOW + TYPING_HOLD),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"
    ));
}

#[test]
fn replies_are_typed_wherever_they_were_written() {
    // On rakumux.com, on another machine, or in this machine's TUI.
    let views = [view("S", Lifecycle::Done, Some(CHAT))];
    for from in [None, Some("laptop"), Some("studio")] {
        let mut replies = on(&views);
        let work = after_reading(&mut replies, vec![reply("01K8A1", 4, from)]);
        assert!(
            matches!(work, Some(ReplyWork::Submit { ref reply, .. }) if reply == "01K8A1"),
            "{from:?}: {work:?}"
        );
    }
}

#[test]
fn a_reply_that_cant_be_typed_fails_with_why() {
    let cases = [
        (vec![], "session_gone"),
        (
            vec![{
                let mut view = view("S", Lifecycle::Done, Some(CHAT));
                view.status = SessionStatus::Exited;
                view
            }],
            "session_gone",
        ),
        (
            vec![{
                let mut view = view("S", Lifecycle::Done, Some(CHAT));
                view.private = true;
                view
            }],
            "private",
        ),
        (
            vec![view("S", Lifecycle::Done, Some("another"))],
            "conversation_changed",
        ),
        (
            vec![view("S", Lifecycle::None, None)],
            "conversation_changed",
        ),
    ];
    for (views, failure) in cases {
        let mut replies = on(&views);
        let body = report_of(after_reading(
            &mut replies,
            vec![reply("01K8A1", 4, Some("studio"))],
        ));
        assert_eq!(body["status"], "failed");
        assert_eq!(body["failure"], failure);
    }

    // The daemon refused the text.
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Err(ErrorCode::SessionNotLive), NOW, 2_000);
    assert_eq!(report_of(replies.next_work(NOW))["failure"], "refused");
    assert_eq!(replies.answered(&answer(Some(CHAT), 3_000)), None);
}

#[test]
fn only_this_machines_pending_replies_are_typed_and_never_twice() {
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let mut elsewhere = reply("01K8A1", 4, Some("studio"));
    elsewhere.machine = "laptop".into();
    let mut delivered = reply("01K8A2", 5, Some("studio"));
    delivered.status = ReplyStatus::Delivered;
    // Both are kept for TUIs' threads; neither is typed.
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let shown = replies.request_done(true, read(vec![elsewhere, delivered]), NOW);
    assert_eq!(shown.len(), 2);
    assert_eq!(replies.next_work(NOW), None);

    // Typed once read, then read again still pending before the report landed.
    let pending = reply("01K8A3", 6, Some("studio"));
    replies.written(&pending, NOW);
    assert_eq!(read_after(replies.next_work(NOW)), 5);
    replies.request_done(true, read(vec![pending.clone()]), NOW);
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { .. })
    ));
    replies.submitted("01K8A3".into(), Ok(()), NOW, 2_000);
    taken(&mut replies);
    report_of(replies.next_work(NOW));
    replies.request_done(true, Err(FeedFailure::Unavailable(None)), NOW);
    let later = NOW + Duration::from_secs(60);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    // The report is retried, and a stale read doesn't type the reply again.
    report_of(replies.next_work(later));
    let mut stored = pending.clone();
    stored.status = ReplyStatus::Delivered;
    replies.request_done(true, Ok(FeedResult::Reply(stored)), later);
    assert_eq!(read_after(replies.next_work(later)), 6);
    replies.request_done(true, read(vec![pending]), later);
    assert_eq!(replies.next_work(later), None);
}

#[test]
fn reads_follow_the_cursor_and_back_off_after_failures() {
    let mut replies = on(&[]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    assert_eq!(replies.next_work(NOW), None);
    replies.request_done(true, page_of(vec![], 7, 12, true), NOW);
    // More waits: at once, after this page (not the account's newest seq,
    // which is on a later one).
    assert_eq!(read_after(replies.next_work(NOW)), 7);
    // The last page: after the account's newest.
    replies.request_done(true, page_of(vec![], 0, 12, false), NOW);
    assert_eq!(replies.next_work(NOW), None);
    assert_eq!(read_after(replies.next_work(NOW + READ_EVERY)), 12);
    replies.request_done(true, Err(FeedFailure::Unavailable(None)), NOW + READ_EVERY);
    assert_eq!(
        replies.next_work(NOW + READ_EVERY + Duration::from_secs(4)),
        None
    );
    assert_eq!(
        read_after(replies.next_work(NOW + READ_EVERY + RETRY_MIN)),
        12
    );
    // A read made for an account signed out meanwhile changes nothing.
    replies.request_done(false, read(vec![reply("01K8A1", 40, Some("studio"))]), NOW);
    replies.set_enabled(false, NOW);
    assert_eq!(replies.next_work(NOW), None);
    replies.set_enabled(true, NOW);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
}

#[test]
fn reports_and_reads_wait_out_their_own_failures() {
    let hour = Duration::from_secs(3_600);
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    // rakumux.com asks to wait an hour (until its quota's window ends): reads
    // hold back a minute at most, as a refused read isn't counted and
    // pending replies expire within the hour.
    replies.request_done(true, Err(FeedFailure::RateLimited(Some(hour))), NOW);
    assert_eq!(replies.next_work(NOW + READ_EVERY), None);

    // A reply written here meanwhile waits for a read only so long, and,
    // typed in, is reported at once.
    let later = NOW + Duration::from_secs(30);
    replies.written(&reply("01K8A1", 4, Some("studio")), later);
    assert_eq!(replies.next_work(later), None);
    let later = later + ORDER_WAIT;
    assert!(matches!(
        replies.next_work(later),
        Some(ReplyWork::Submit { .. })
    ));
    replies.submitted("01K8A1".into(), Ok(()), later, 2_000);
    taken(&mut replies);
    assert_eq!(report_of(replies.next_work(later))["status"], "delivered");
    assert_eq!(replies.next_work(later), None, "one request at a time");
    let mut delivered = reply("01K8A1", 5, Some("studio"));
    delivered.status = ReplyStatus::Delivered;
    replies.request_done(true, Ok(FeedResult::Reply(delivered)), later);
    assert_eq!(
        replies.next_work(NOW + RATE_LIMIT_MAX - Duration::from_secs(1)),
        None
    );
    let read_at = NOW + RATE_LIMIT_MAX;
    assert_eq!(read_after(replies.next_work(read_at)), 0);
    replies.request_done(true, read(vec![]), read_at);

    // Reports rate-limited in turn don't hold back reads.
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    replies.written(&reply("01K8A2", 6, Some("studio")), read_at);
    assert_eq!(read_after(replies.next_work(read_at)), 0);
    replies.request_done(
        true,
        read(vec![reply("01K8A2", 6, Some("studio"))]),
        read_at,
    );
    assert!(matches!(
        replies.next_work(read_at),
        Some(ReplyWork::Submit { .. })
    ));
    replies.submitted("01K8A2".into(), Ok(()), read_at, 3_000);
    taken(&mut replies);
    report_of(replies.next_work(read_at));
    replies.request_done(true, Err(FeedFailure::RateLimited(Some(hour))), read_at);
    assert_eq!(
        read_after(replies.next_work(read_at + READ_EVERY)),
        6,
        "reads go on"
    );
    replies.request_done(true, read(vec![]), read_at + READ_EVERY);
    // The report goes again a minute later, not an hour.
    assert_eq!(
        report_of(replies.next_work(read_at + RATE_LIMIT_MAX))["id"],
        "01K8A2"
    );
}

#[test]
fn the_waits_rakumux_com_asks_for_are_capped() {
    let mut backoff = Backoff::new(NOW);
    backoff.failed(
        FeedFailure::RateLimited(Some(Duration::from_secs(3_600))),
        NOW,
    );
    assert!(!backoff.ready(NOW + RATE_LIMIT_MAX - Duration::from_secs(1)));
    assert!(backoff.ready(NOW + RATE_LIMIT_MAX));
    backoff.failed(
        FeedFailure::Unavailable(Some(Duration::from_secs(86_400))),
        NOW,
    );
    assert!(!backoff.ready(NOW + RETRY_MAX - Duration::from_secs(1)));
    assert!(backoff.ready(NOW + RETRY_MAX));
    // A shorter wait is taken as asked.
    backoff.failed(FeedFailure::RateLimited(Some(Duration::from_secs(2))), NOW);
    assert!(backoff.ready(NOW + Duration::from_secs(2)));
}

#[test]
fn a_pending_reply_on_a_later_page_is_typed_in() {
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    // The first page: replies delivered long ago. The account's newest, the
    // pending one, is on a later page.
    let mut old = reply("01K8A0", 100, Some("studio"));
    old.status = ReplyStatus::Delivered;
    replies.request_done(true, page_of(vec![old], 100, 250, true), NOW);
    assert_eq!(read_after(replies.next_work(NOW)), 100);
    let pending = reply("01K8A1", 250, Some("studio"));
    replies.request_done(true, page_of(vec![pending], 250, 250, false), NOW);
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"
    ));
}

#[test]
fn a_page_whose_rows_were_all_left_out_still_moves_reads_on() {
    let mut replies = on(&[]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, page_of(vec![], 100, 300, true), NOW);
    assert_eq!(read_after(replies.next_work(NOW)), 100);
}

#[test]
fn reads_come_as_soon_as_rakumux_com_says() {
    let paced = |poll: Option<Duration>| {
        Ok(FeedResult::Replies(RepliesRead {
            replies: vec![],
            messages: vec![],
            page_seq: 0,
            latest_seq: 0,
            more: false,
            poll,
            resources_wanted: false,
            new_sessions: vec![],
        }))
    };
    let mut replies = on(&[]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    // A reply waits for this machine, or the website is open: every second.
    replies.request_done(true, paced(Some(Duration::from_secs(1))), NOW);
    assert!(replies.watched(), "chats then upload as they grow");
    assert_eq!(replies.next_work(NOW + Duration::from_millis(999)), None);
    let soon = NOW + Duration::from_secs(1);
    assert_eq!(read_after(replies.next_work(soon)), 0);
    // Nothing going on: less often.
    replies.request_done(true, paced(Some(Duration::from_secs(15))), soon);
    assert!(!replies.watched());
    assert_eq!(replies.next_work(soon + Duration::from_secs(14)), None);
    let later = soon + Duration::from_secs(15);
    assert_eq!(read_after(replies.next_work(later)), 0);
    // Without a hint, its own pace.
    replies.request_done(true, paced(None), later);
    assert_eq!(
        replies.next_work(later + READ_EVERY - Duration::from_millis(1)),
        None
    );
    assert_eq!(read_after(replies.next_work(later + READ_EVERY)), 0);
}

fn message(id: &str, seq: u64, conversation: Option<&str>) -> SessionMessage {
    SessionMessage {
        seq,
        id: id.into(),
        machine: "studio".into(),
        session_id: "S".into(),
        conversation_id: conversation.map(Into::into),
        text: "Start on the parser.".into(),
        attachments: Vec::new(),
        from: None,
        status: ReplyStatus::Pending,
        answer: None,
    }
}

fn read_messages(messages: Vec<SessionMessage>) -> Result<FeedResult, FeedFailure> {
    let latest_seq = messages
        .iter()
        .map(|message| message.seq)
        .max()
        .unwrap_or(0);
    Ok(FeedResult::Replies(RepliesRead {
        replies: vec![],
        messages,
        page_seq: latest_seq,
        latest_seq,
        more: false,
        poll: None,
        resources_wanted: false,
        new_sessions: vec![],
    }))
}

#[test]
fn a_session_message_waits_for_an_agent_then_goes_into_whatever_chat_it_shows() {
    // A shell: nothing is typed into it.
    let mut replies = on(&[view("S", Lifecycle::None, None)]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let shown = replies.request_done(
        true,
        read_messages(vec![message("01K8A2", 6, Some(CHAT))]),
        NOW,
    );
    assert!(shown.is_empty(), "session messages never reach TUIs");
    assert_eq!(replies.next_work(NOW), None);
    // Its agent works: still waiting.
    replies.observe(&view("S", Lifecycle::Working, Some("another")));
    assert_eq!(replies.next_work(NOW), None);
    // It waits for input, in another chat than the one written to: typed.
    replies.observe(&view("S", Lifecycle::Idle, Some("another")));
    assert_eq!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit {
            reply: "01K8A2".into(),
            session: SessionId::from_string("S"),
            text: "Start on the parser.".into(),
            foreground: Some("synthetic-agent".into()),
        })
    );
    replies.submitted("01K8A2".into(), Ok(()), NOW, 2_000);
    replies.observe(&prompted(
        Lifecycle::Working,
        Some("another"),
        "Start on the parser.",
    ));
    let body = report_of(replies.next_work(NOW));
    assert_eq!(body["id"], "01K8A2");
    assert_eq!(body["status"], "delivered");
    let mut delivered = message("01K8A2", 7, Some(CHAT));
    delivered.status = ReplyStatus::Delivered;
    assert!(replies
        .request_done(true, Ok(FeedResult::Message(delivered)), NOW)
        .is_empty());
    // Answered in the chat the session showed when it was typed.
    assert_eq!(replies.answered(&answer(Some(CHAT), 3_000)), None);
    assert_eq!(
        replies.answered(&answer(Some("another"), 3_000)),
        Some("01K8A2".into())
    );
}

#[test]
fn a_session_message_written_here_is_typed_once_read_when_its_agent_waits() {
    // Its own TUI's message goes in once a read reaches it.
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let mut written = message("01K8A4", 9, Some(CHAT));
    written.from = Some("studio".into());
    replies.written_message(&written, NOW);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![written.clone()]), NOW);
    assert_eq!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit {
            reply: "01K8A4".into(),
            session: SessionId::from_string("S"),
            text: "Start on the parser.".into(),
            foreground: Some("synthetic-agent".into()),
        })
    );
    assert_eq!(
        ReplyWrite::of_message(Ok(())),
        ReplyWrite {
            status: ReplyWriteStatus::Sent,
            reply: None
        }
    );
    assert_eq!(
        ReplyWrite::of_message(Err(FeedFailure::Off)).status,
        ReplyWriteStatus::Off
    );
}

#[test]
fn a_session_message_follows_the_same_rules_for_where_it_may_go() {
    // Written on rakumux.com: typed here as one written in this machine's TUI is.
    let mut replies = on(&[view("S", Lifecycle::Idle, None)]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A3", 8, None)]), NOW);
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A3"
    ));

    // A session that ended, or a private one, can't take it.
    let mut private = view("S", Lifecycle::Idle, None);
    private.private = true;
    let mut replies = on(&[private]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A4", 9, None)]), NOW);
    assert_eq!(report_of(replies.next_work(NOW))["failure"], "private");

    // With no chat to name, the agent's next reply answers it, whichever chat.
    let mut replies = on(&[view("S", Lifecycle::Done, None)]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A5", 10, None)]), NOW);
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { .. })
    ));
    replies.submitted("01K8A5".into(), Ok(()), NOW, 2_000);
    assert_eq!(
        replies.answered(&answer(Some(CHAT), 3_000)),
        Some("01K8A5".into())
    );
}

// Files attached to replies.

use super::super::uploads::{saved, Uploads};
use crate::cloud::FeedHttpReply;

/// A reply with files: its id a ULID, as saving them needs.
const ATTACHED: &str = "01K8A1B2C3D4E5F6G7H8J9K0MN";

fn file(last: char, name: &str, size: u64) -> ReplyAttachment {
    ReplyAttachment {
        id: format!("01K8A1C0FFEE000000000000F{last}"),
        name: name.into(),
        media_type: "application/octet-stream".into(),
        size,
    }
}

fn with_files(id: &str, files: Vec<ReplyAttachment>) -> FeedReply {
    let mut reply = reply(id, 4, Some("studio"));
    reply.attachments = files;
    reply
}

fn uploads() -> (tempfile::TempDir, Uploads) {
    let home = tempfile::tempdir().unwrap();
    let uploads = Uploads::new(home.path().join("uploads"));
    (home, uploads)
}

fn fetch_of(work: Option<ReplyWork>) -> Fetch {
    match work {
        Some(ReplyWork::Fetch(fetch)) => fetch,
        other => panic!("expected a fetch, got {other:?}"),
    }
}

fn typed_text(work: Option<ReplyWork>) -> String {
    match work {
        Some(ReplyWork::Submit { text, .. }) => text,
        other => panic!("expected typing, got {other:?}"),
    }
}

/// rakumux.com answers `fetch`'s download with `status` and `body`, and the
/// worker saves what came, as the runner's does.
fn served(fetch: &Fetch, uploads: &Uploads, status: u16, body: &[u8]) -> Fetched {
    let answer = FeedHttpReply {
        status,
        retry_after: None,
        body: body.to_vec(),
    };
    let outcome = wire::download(answer, fetch.attachment.size).map(FeedResult::Download);
    saved(outcome, fetch, uploads)
}

const GONE: &[u8] = br#"{"error":{"code":"invalid_request","message":"Not found."}}"#;
const DOWN: &[u8] = br#"{"error":{"code":"unavailable","message":"Try later."}}"#;

#[test]
fn a_replys_files_are_fetched_then_its_text_typed_with_their_paths() {
    let (home, uploads) = uploads();
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let files = vec![file('A', "shot.png", 4), file('B', "Shot.PNG", 3)];
    let first = fetch_of(after_reading(
        &mut replies,
        vec![with_files(ATTACHED, files)],
    ));
    assert!(first.first && first.taken.is_empty());
    assert_eq!(
        first.request(),
        FeedRequest::Attachment {
            id: "01K8A1C0FFEE000000000000FA".into(),
            size: 4
        }
    );
    // One file at a time, and nothing typed meanwhile.
    assert_eq!(replies.next_work(NOW), None);
    let fetched = served(&first, &uploads, 200, b"\x89PNG");
    replies.fetched(true, ATTACHED.into(), fetched, NOW);
    let second = fetch_of(replies.next_work(NOW));
    assert!(!second.first);
    assert_eq!(second.taken, ["shot.png"]);
    let fetched = served(&second, &uploads, 200, b"GIF");
    replies.fetched(true, ATTACHED.into(), fetched, NOW);

    let dir = home.path().join("uploads").join(ATTACHED);
    assert_eq!(
        typed_text(replies.next_work(NOW)),
        format!(
            "Now add a test.\n\n{}\n{}",
            dir.join("shot.png").display(),
            dir.join("Shot (2).PNG").display()
        )
    );
    assert_eq!(std::fs::read(dir.join("Shot (2).PNG")).unwrap(), b"GIF");
    replies.submitted(ATTACHED.into(), Ok(()), NOW, 2_000);
    taken(&mut replies);
    assert_eq!(report_of(replies.next_work(NOW))["status"], "delivered");
    // Typed in: its files stay for the agent.
    assert_eq!(replies.next_work(NOW), None);
    assert!(dir.join("shot.png").exists());
}

#[test]
fn files_are_fetched_only_once_the_reply_could_go_in_and_never_twice() {
    let (_home, uploads) = uploads();
    // A busy agent: nothing is fetched for a reply that may wait an hour.
    let mut replies = on(&[view("S", Lifecycle::Working, Some(CHAT))]);
    let pending = with_files(ATTACHED, vec![file('A', "log.txt", 2)]);
    assert_eq!(after_reading(&mut replies, vec![pending]), None);
    replies.observe(&view("S", Lifecycle::Idle, Some(CHAT)));
    let fetch = fetch_of(replies.next_work(NOW));
    replies.fetched(
        true,
        ATTACHED.into(),
        served(&fetch, &uploads, 200, b"ok"),
        NOW,
    );

    // Busy again before it went in: it waits with its files.
    replies.observe(&view("S", Lifecycle::Working, Some(CHAT)));
    assert_eq!(replies.next_work(NOW), None);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert!(typed_text(replies.next_work(NOW)).ends_with("/log.txt"));
    // Someone was typing there: tried again later, without fetching again.
    replies.typing(ATTACHED.into(), NOW);
    assert_eq!(replies.next_work(NOW + TYPING_HOLD / 2), None);
    assert!(typed_text(replies.next_work(NOW + TYPING_HOLD)).ends_with("/log.txt"));
}

#[test]
fn a_file_thats_gone_or_not_its_size_fails_the_reply_and_types_nothing() {
    for (status, body) in [
        (404, GONE),
        (200, &b"short"[..]),
        (200, &b"too long!!"[..]),
        (400, GONE),
    ] {
        let (home, uploads) = uploads();
        let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
        let files = vec![file('A', "a.txt", 1), file('B', "b.txt", 8)];
        let first = fetch_of(after_reading(
            &mut replies,
            vec![with_files(ATTACHED, files)],
        ));
        replies.fetched(
            true,
            ATTACHED.into(),
            served(&first, &uploads, 200, b"a"),
            NOW,
        );
        let second = fetch_of(replies.next_work(NOW));
        let fetched = served(&second, &uploads, status, body);
        let expected = if status == 400 {
            Fetched::Failed(FeedFailure::Invalid)
        } else {
            Fetched::Unavailable
        };
        assert_eq!(fetched, expected);
        replies.fetched(true, ATTACHED.into(), fetched, NOW);
        // What was saved of it goes, and rakumux.com hears why.
        assert_eq!(
            replies.next_work(NOW),
            Some(ReplyWork::Discard(ATTACHED.into()))
        );
        uploads.discard(ATTACHED);
        assert!(!home.path().join("uploads").join(ATTACHED).exists());
        let body = report_of(replies.next_work(NOW));
        assert_eq!(body["status"], "failed", "{status}");
        assert_eq!(body["failure"], "attachment_unavailable");
        assert_eq!(replies.next_work(NOW), None);
    }
}

#[test]
fn a_download_that_fails_for_now_is_tried_again_and_nothing_goes_in_meanwhile() {
    let (_home, uploads) = uploads();
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let files = vec![file('A', "a.txt", 1), file('B', "b.txt", 1)];
    let first = fetch_of(after_reading(
        &mut replies,
        vec![with_files(ATTACHED, files)],
    ));
    let fetched = served(&first, &uploads, 503, DOWN);
    assert_eq!(fetched, Fetched::Failed(FeedFailure::Unavailable(None)));
    replies.fetched(true, ATTACHED.into(), fetched, NOW);
    // Pending still: neither typed nor reported, until the wait is over.
    assert_eq!(replies.next_work(NOW), None);
    let retry = NOW + RETRY_MIN;
    assert_eq!(replies.next_work(retry - Duration::from_secs(1)), None);
    let again = fetch_of(replies.next_work(retry));
    assert_eq!(again, first, "the same file, its directory afresh");
    replies.fetched(
        true,
        ATTACHED.into(),
        served(&again, &uploads, 200, b"a"),
        retry,
    );

    // The second fails in turn: only it is fetched again, the first kept.
    let second = fetch_of(replies.next_work(retry));
    let limited = br#"{"error":{"code":"rate_limited","message":"x","retry_after_seconds":60}}"#;
    let fetched = served(&second, &uploads, 429, limited);
    replies.fetched(true, ATTACHED.into(), fetched, retry);
    // Reads go on meanwhile.
    let minute = retry + Duration::from_secs(60);
    let before = minute - Duration::from_secs(1);
    read_after(replies.next_work(before));
    replies.request_done(true, read(vec![]), before);
    assert_eq!(replies.next_work(before), None);
    let second = fetch_of(replies.next_work(minute));
    assert!(!second.first && second.taken == ["a.txt"]);
    replies.fetched(
        true,
        ATTACHED.into(),
        served(&second, &uploads, 200, b"b"),
        minute,
    );
    let text = typed_text(replies.next_work(minute));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert!(lines[2].ends_with("/a.txt") && lines[3].ends_with("/b.txt"));
}

#[test]
fn a_replys_files_go_when_it_wont_be_typed_in() {
    let saved_reply = |views: &[SessionView]| {
        let (home, uploads) = uploads();
        let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
        let files = vec![file('A', "a.txt", 1)];
        let fetch = fetch_of(after_reading(
            &mut replies,
            vec![with_files(ATTACHED, files)],
        ));
        replies.fetched(
            true,
            ATTACHED.into(),
            served(&fetch, &uploads, 200, b"a"),
            NOW,
        );
        replies.connected(views, GUARDED);
        (home, replies)
    };
    // Its session ended before it went in: rakumux.com hears why, and its
    // files go.
    let (_home, mut replies) = saved_reply(&[]);
    assert_eq!(report_of(replies.next_work(NOW))["failure"], "session_gone");
    assert_eq!(
        replies.next_work(NOW),
        Some(ReplyWork::Discard(ATTACHED.into()))
    );

    // The daemon refused the text.
    let (_home, mut replies) = saved_reply(&[view("S", Lifecycle::Done, Some(CHAT))]);
    typed_text(replies.next_work(NOW));
    replies.submitted(ATTACHED.into(), Err(ErrorCode::SessionNotLive), NOW, 2_000);
    assert_eq!(
        replies.next_work(NOW),
        Some(ReplyWork::Discard(ATTACHED.into()))
    );
    assert_eq!(report_of(replies.next_work(NOW))["failure"], "refused");

    // It expired while its agent was busy.
    let (_home, mut replies) = saved_reply(&[view("S", Lifecycle::Working, Some(CHAT))]);
    assert_eq!(read_after(replies.next_work(NOW + READ_EVERY)), 4);
    let mut expired = with_files(ATTACHED, vec![file('A', "a.txt", 1)]);
    expired.seq = 5;
    expired.status = ReplyStatus::Expired;
    replies.request_done(true, read(vec![expired]), NOW + READ_EVERY);
    assert_eq!(
        replies.next_work(NOW + READ_EVERY),
        Some(ReplyWork::Discard(ATTACHED.into()))
    );

    // The feed was turned off.
    let (_home, mut replies) = saved_reply(&[view("S", Lifecycle::Working, Some(CHAT))]);
    replies.set_enabled(false, NOW);
    assert_eq!(
        replies.next_work(NOW),
        Some(ReplyWork::Discard(ATTACHED.into()))
    );
    assert_eq!(replies.next_work(NOW), None);
}

#[test]
fn later_replies_to_a_session_wait_behind_one_fetching_its_files() {
    let (_home, uploads) = uploads();
    let mut other = view("T", Lifecycle::Idle, Some(CHAT));
    other.name = "other".into();
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT)), other]);
    let later = reply("01K8A1B2C3D4E5F6G7H8J9K0MP", 5, Some("studio"));
    let mut elsewhere = reply("01K8A1B2C3D4E5F6G7H8J9K0MQ", 6, Some("studio"));
    elsewhere.session_id = "T".into();
    let first = fetch_of(after_reading(
        &mut replies,
        vec![
            with_files(ATTACHED, vec![file('A', "a.txt", 1)]),
            later,
            elsewhere,
        ],
    ));
    // Another session's reply goes in meanwhile; this one's later reply waits.
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1B2C3D4E5F6G7H8J9K0MQ"
    ));
    assert_eq!(replies.next_work(NOW), None);
    // Still behind it while its download waits to be tried again.
    replies.fetched(
        true,
        ATTACHED.into(),
        served(&first, &uploads, 503, DOWN),
        NOW,
    );
    assert_eq!(replies.next_work(NOW), None);
    let retry = NOW + RETRY_MIN;
    let first = fetch_of(replies.next_work(retry));
    replies.fetched(
        true,
        ATTACHED.into(),
        served(&first, &uploads, 200, b"a"),
        retry,
    );
    assert!(matches!(
        replies.next_work(retry),
        Some(ReplyWork::Submit { reply, .. }) if reply == ATTACHED
    ));
}

// A session's queue: one reply at a time, in order, each seen taken by its
// agent before the next goes in.

/// Session S with its agent in `lifecycle`, in CHAT, on turn `turn`.
fn on_turn(lifecycle: Lifecycle, turn: u64) -> SessionView {
    let mut view = view("S", lifecycle, Some(CHAT));
    if let Some(agent_turn) = view.agent_turn.as_mut() {
        agent_turn.id = turn;
    }
    view
}

/// Two replies to S, read together; the first is typed in.
fn two_replies() -> ReplySync {
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let first = reply("01K8A1", 4, Some("studio"));
    let second = reply("01K8A2", 5, Some("studio"));
    let work = after_reading(&mut replies, vec![second, first]);
    assert!(matches!(work, Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"));
    replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
    replies
}

/// What comes next at `at`, reads answered with nothing new.
fn next_at(replies: &mut ReplySync, at: Duration) -> Option<ReplyWork> {
    loop {
        match replies.next_work(at) {
            Some(ReplyWork::Request(FeedRequest::Replies { .. })) => {
                replies.request_done(true, read(vec![]), at);
            }
            other => return other,
        }
    }
}

fn submitted_reply(work: Option<ReplyWork>) -> Option<String> {
    match work {
        Some(ReplyWork::Submit { reply, .. }) => Some(reply),
        _ => None,
    }
}

#[test]
fn a_view_still_showing_the_agent_waiting_doesnt_let_the_next_reply_in() {
    let mut replies = two_replies();
    // The daemon's next views, published before its screen showed the agent
    // working (its lifecycle lags), still say it waits: the first wasn't
    // typed into a prompt that took it yet, as far as anyone can tell.
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert_eq!(replies.next_work(NOW), None);
    replies.observe(&view("S", Lifecycle::Idle, Some(CHAT)));
    assert_eq!(next_at(&mut replies, NOW + Duration::from_secs(5)), None);
    // Its agent works: the first is delivered; the second waits for it to finish.
    taken(&mut replies);
    let body = report_of(replies.next_work(NOW));
    assert_eq!(
        (&body["id"], &body["status"]),
        (&"01K8A1".into(), &"delivered".into())
    );
    replies.request_done(true, Ok(FeedResult::Reply(reply("01K8A1", 6, None))), NOW);
    assert_eq!(replies.next_work(NOW), None);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A2")
    );
}

#[test]
fn a_new_prompt_in_its_chat_shows_a_reply_was_taken() {
    let mut replies = two_replies();
    // The agent's conversation shows a new prompt, its lifecycle not yet working.
    replies.observe(&on_turn(Lifecycle::Done, 2));
    assert_eq!(report_of(replies.next_work(NOW))["status"], "delivered");
    replies.request_done(true, Ok(FeedResult::Reply(reply("01K8A1", 6, None))), NOW);
    // Still shown waiting, which may lag: the next waits a while.
    assert_eq!(
        next_at(&mut replies, NOW + SETTLE - Duration::from_secs(1)),
        None
    );
    assert_eq!(
        submitted_reply(next_at(&mut replies, NOW + SETTLE)).as_deref(),
        Some("01K8A2")
    );

    // Shown working meanwhile, it waits for the turn to finish however long.
    let mut replies = two_replies();
    replies.observe(&on_turn(Lifecycle::Done, 2));
    report_of(replies.next_work(NOW));
    replies.request_done(true, Ok(FeedResult::Reply(reply("01K8A1", 6, None))), NOW);
    replies.observe(&on_turn(Lifecycle::Working, 2));
    let later = NOW + Duration::from_secs(600);
    assert_eq!(next_at(&mut replies, later), None);
    replies.observe(&on_turn(Lifecycle::Done, 2));
    assert_eq!(
        submitted_reply(next_at(&mut replies, later)).as_deref(),
        Some("01K8A2")
    );
}

#[test]
fn a_reply_its_chat_never_shows_as_a_prompt_fails_and_its_session_moves_on() {
    let mut replies = two_replies();
    // Even shown working: its chat has no new prompt yet.
    replies.observe(&view("S", Lifecycle::Working, Some(CHAT)));
    let before = NOW + TAKE_WITHIN - Duration::from_secs(1);
    assert_eq!(next_at(&mut replies, before), None);
    // No new prompt in its chat a while after it was typed: its Enter was
    // lost, or it went into a menu. Not taken, and its session moves on once
    // its agent waits.
    let bound = NOW + TAKE_WITHIN;
    let body = report_of(next_at(&mut replies, bound));
    assert_eq!(
        (&body["id"], &body["status"], &body["failure"]),
        (&"01K8A1".into(), &"failed".into(), &"not_taken".into())
    );
    replies.request_done(true, Ok(FeedResult::Reply(reply("01K8A1", 6, None))), bound);
    assert_eq!(next_at(&mut replies, bound), None, "its agent works");
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert_eq!(
        submitted_reply(next_at(&mut replies, bound)).as_deref(),
        Some("01K8A2")
    );
}

#[test]
fn a_new_prompt_is_the_replys_only_when_it_begins_it_or_holds_its_start() {
    let typed_in = |text: &str| {
        let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
        let mut pending = reply("01K8A1", 4, Some("studio"));
        pending.text = text.into();
        submitted_reply(after_reading(&mut replies, vec![pending]));
        replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
        replies
    };
    let long = "Now add a test for the parser's error path, then run the whole suite \
        and tell me what fails. "
        .repeat(4);
    for (text, prompt, taken) in [
        // As the daemon reads it: marks, spaces and case may differ.
        ("Now add a **test**.", "now add a test.", true),
        ("## Plan\n\nShip it", "Plan", true),
        ("/review src", "/review src", true),
        // A long reply's first line, cut at 240 characters.
        (long.as_str(), &long[..240], true),
        // Typed after a draft on the same line: it went in.
        ("Run the tests", "also check theRun the tests", true),
        // Another prompt: someone else's, or a draft on a line of its own.
        ("Run the tests", "Fix the docs", false),
        ("Run the tests", "also check the", false),
    ] {
        let mut replies = typed_in(text);
        replies.observe(&prompted(Lifecycle::Working, Some(CHAT), prompt));
        let body = report_of(replies.next_work(NOW));
        if taken {
            assert_eq!(body["status"], "delivered", "{text:?} / {prompt:?}");
        } else {
            assert_eq!(
                (&body["status"], &body["failure"]),
                (&"failed".into(), &"not_taken".into()),
                "{text:?} / {prompt:?}"
            );
        }
    }
}

/// Session S running `agent` as it starts: waiting, with no chat yet (its
/// log isn't written until its first prompt).
fn chatless(agent: Option<&str>, lifecycle: Lifecycle) -> SessionView {
    let mut view = view("S", lifecycle, None);
    view.agent_turn = None;
    view.agent = agent.map(|kind| AgentInfo {
        kind: kind.into(),
        ..AgentInfo::default()
    });
    view
}

/// A session message typed into `start` (S, read with `guarded`), whose
/// agent is then seen working with no chat.
fn typed_then_busy(start: SessionView, guarded: bool) -> ReplySync {
    let mut replies = ReplySync::new("studio".into(), None);
    replies.set_enabled(true, NOW);
    replies.connected(std::slice::from_ref(&start), guarded);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A2", 6, None)]), NOW);
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A2")
    );
    replies.submitted("01K8A2".into(), Ok(()), NOW, 2_000);
    let mut busy = start;
    busy.lifecycle = Lifecycle::Working;
    replies.observe(&busy);
    replies
}

#[test]
fn an_agent_whose_turns_the_daemon_doesnt_follow_takes_a_session_message_once_seen_busy() {
    // No agent named, one the daemon doesn't follow, or a daemon from before
    // it followed them all (no `session.submit.blocked`): its being busy is
    // all there is to go on, as before.
    for (agent, guarded) in [
        (None, GUARDED),
        (Some("gemini"), GUARDED),
        (Some("claude"), UNGUARDED),
    ] {
        let mut replies = typed_then_busy(chatless(agent, Lifecycle::Idle), guarded);
        assert_eq!(
            report_of(replies.next_work(NOW))["status"],
            "delivered",
            "{agent:?}"
        );
    }
}

#[test]
fn a_session_message_to_an_agent_just_started_is_delivered_only_once_its_chat_shows_it() {
    // A new session from rakumux.com (2026-10-31): its first prompt typed
    // into Claude Code as it started, the Enter lost. It went busy starting
    // up, and no chat ever showed the prompt: not delivered.
    for agent in ["claude", "codex", "kimi", "amp"] {
        let mut replies = typed_then_busy(chatless(Some(agent), Lifecycle::Idle), GUARDED);
        let before = NOW + TAKE_WITHIN - Duration::from_secs(1);
        assert_eq!(
            next_at(&mut replies, before),
            None,
            "{agent}: busy proves nothing"
        );
        replies.observe(&chatless(Some(agent), Lifecycle::Idle));
        let body = report_of(next_at(&mut replies, NOW + TAKE_WITHIN));
        assert_eq!(
            (&body["status"], &body["failure"]),
            (&"failed".into(), &"not_taken".into()),
            "{agent}"
        );
    }
    // Its first chat shows it as the prompt: delivered.
    let mut replies = typed_then_busy(chatless(Some("claude"), Lifecycle::Idle), GUARDED);
    assert_eq!(replies.next_work(NOW), None);
    let mut started = prompted(Lifecycle::Working, Some(CHAT), "Start on the parser.");
    started.agent = chatless(Some("claude"), Lifecycle::Idle).agent;
    replies.observe(&started);
    assert_eq!(report_of(replies.next_work(NOW))["status"], "delivered");
}

#[test]
fn a_reply_typed_in_fails_when_its_session_ends_or_its_chat_changes_first() {
    let cases = [
        (vec![], "session_gone"),
        (
            vec![view("S", Lifecycle::Idle, Some("another"))],
            "conversation_changed",
        ),
    ];
    for (views, failure) in cases {
        let mut replies = two_replies();
        replies.connected(&views, GUARDED);
        let body = report_of(replies.next_work(NOW));
        assert_eq!(
            (&body["id"], &body["status"]),
            (&"01K8A1".into(), &"failed".into())
        );
        assert_eq!(body["failure"], failure);
    }
    // Taken before its chat changed: delivered.
    let mut replies = two_replies();
    taken(&mut replies);
    replies.observe(&view("S", Lifecycle::Idle, Some("another")));
    assert_eq!(report_of(replies.next_work(NOW))["status"], "delivered");
}

#[test]
fn a_reply_typed_in_keeps_its_files_whatever_becomes_of_it() {
    let (home, uploads) = uploads();
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let files = vec![file('A', "shot.png", 4)];
    let fetch = fetch_of(after_reading(
        &mut replies,
        vec![with_files(ATTACHED, files)],
    ));
    replies.fetched(
        true,
        ATTACHED.into(),
        served(&fetch, &uploads, 200, b"\x89PNG"),
        NOW,
    );
    typed_text(replies.next_work(NOW));
    replies.submitted(ATTACHED.into(), Ok(()), NOW, 2_000);
    // Expired on rakumux.com, then its session ended, before it was seen taken.
    let mut expired = with_files(ATTACHED, vec![file('A', "shot.png", 4)]);
    expired.seq = 5;
    expired.status = ReplyStatus::Expired;
    assert_eq!(read_after(replies.next_work(NOW + READ_EVERY)), 4);
    replies.request_done(true, read(vec![expired]), NOW + READ_EVERY);
    replies.connected(&[], GUARDED);
    assert_eq!(
        report_of(next_at(&mut replies, NOW + READ_EVERY))["failure"],
        "session_gone"
    );
    assert_eq!(next_at(&mut replies, NOW + READ_EVERY), None, "no discard");
    assert!(home
        .path()
        .join("uploads")
        .join(ATTACHED)
        .join("shot.png")
        .exists());
}

#[test]
fn a_sessions_replies_and_messages_go_in_in_the_order_they_were_written() {
    // Its chat isn't known yet: the reply to it waits, and the session
    // message written after it waits behind it.
    let mut replies = on(&[view("S", Lifecycle::Done, None)]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let read = Ok(FeedResult::Replies(RepliesRead {
        replies: vec![reply("01K8A9", 4, None)],
        messages: vec![message("01K8A1", 5, None)],
        page_seq: 5,
        latest_seq: 5,
        more: false,
        poll: None,
        resources_wanted: false,
        new_sessions: vec![],
    }));
    replies.request_done(true, read, NOW);
    assert_eq!(replies.next_work(NOW), None);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A9")
    );
    replies.submitted("01K8A9".into(), Ok(()), NOW, 2_000);
    taken(&mut replies);
    report_of(replies.next_work(NOW));
    replies.request_done(true, Ok(FeedResult::Reply(reply("01K8A9", 6, None))), NOW);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A1")
    );
}

#[test]
fn a_reply_written_here_goes_in_behind_one_written_before_it_elsewhere_not_read_yet() {
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    // A read goes out; meanwhile a reply is written on rakumux.com, then one
    // in this machine's TUI, and the read comes back without either.
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let earlier = reply("01K8A1", 7, None);
    let mine = reply("01K8A2", 8, Some("studio"));
    replies.written(&mine, NOW);
    assert_eq!(replies.next_work(NOW), None, "not before a read reaches it");
    replies.request_done(true, read(vec![]), NOW);
    // Read again at once: the one written first goes in first.
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read(vec![earlier, mine]), NOW);
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A1")
    );
    replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
    taken(&mut replies);
    report_of(replies.next_work(NOW));
    replies.request_done(true, Ok(FeedResult::Reply(reply("01K8A1", 9, None))), NOW);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A2")
    );
}

#[test]
fn someone_typing_or_a_refusal_frees_the_session_for_its_next_reply() {
    // Refused by the daemon: failed, and the next goes in.
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let first = reply("01K8A1", 4, Some("studio"));
    let second = reply("01K8A2", 5, Some("studio"));
    after_reading(&mut replies, vec![first, second]);
    replies.submitted("01K8A1".into(), Err(ErrorCode::SessionNotLive), NOW, 2_000);
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A2")
    );
    assert_eq!(report_of(replies.next_work(NOW))["failure"], "refused");

    // Someone typing: the same reply again once they pause, still first.
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let first = reply("01K8A1", 4, Some("studio"));
    let second = reply("01K8A2", 5, Some("studio"));
    after_reading(&mut replies, vec![first, second]);
    replies.typing("01K8A1".into(), NOW);
    assert_eq!(replies.next_work(NOW), None);
    assert_eq!(
        submitted_reply(next_at(&mut replies, NOW + TYPING_HOLD)).as_deref(),
        Some("01K8A1")
    );
}

/// Replies for studio keeping what's typed in in `path`, with session S.
fn kept_at(path: &std::path::Path, views: &[SessionView]) -> ReplySync {
    let mut replies = ReplySync::new("studio".into(), Some(path.to_path_buf()));
    replies.set_enabled(true, NOW);
    replies.connected(views, GUARDED);
    replies
}

#[test]
fn a_reply_handed_to_the_daemon_is_never_typed_again_after_a_restart_and_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let views = [view("S", Lifecycle::Done, Some(CHAT))];
    let pending = reply("01K8A1", 4, Some("studio"));
    {
        let mut replies = kept_at(&path, &views);
        let work = after_reading(&mut replies, vec![pending.clone()]);
        assert!(matches!(work, Some(ReplyWork::Submit { .. })));
        // The service stops (an update replaces it) before the daemon
        // answered, or before the agent was seen to take it.
    }
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut replies = kept_at(&path, &views);
    // Read again, still pending: never typed again, and followed as before…
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read(vec![pending.clone()]), NOW);
    assert_eq!(replies.next_work(NOW), None);
    // …so it's reported delivered once its agent is seen to take it.
    taken(&mut replies);
    let body = report_of(replies.next_work(NOW));
    assert_eq!(
        (&body["id"], &body["status"]),
        (&"01K8A1".into(), &"delivered".into())
    );
    let mut delivered = pending.clone();
    delivered.status = ReplyStatus::Delivered;
    replies.request_done(true, Ok(FeedResult::Reply(delivered)), NOW);
    // A read from the start that still says pending types nothing.
    drop(replies);
    let mut replies = kept_at(&path, &views);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read(vec![pending]), NOW);
    assert_eq!(replies.next_work(NOW), None);
}

#[test]
fn reports_not_yet_taken_survive_the_switch_going_off_and_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let mut replies = kept_at(&path, &[]);
    // Its session is gone: decided at once, but rakumux.com can't be reached.
    let body = report_of(after_reading(
        &mut replies,
        vec![reply("01K8A1", 4, Some("studio"))],
    ));
    assert_eq!(body["failure"], "session_gone");
    replies.request_done(true, Err(FeedFailure::Unavailable(None)), NOW);
    // The switch goes off and on again: the report still goes.
    replies.set_enabled(false, NOW);
    replies.set_enabled(true, NOW);
    assert_eq!(report_of(replies.next_work(NOW))["id"], "01K8A1");
    drop(replies);
    // Restarted before it was taken: it goes again; taken, never again.
    let mut replies = kept_at(&path, &[]);
    assert_eq!(report_of(replies.next_work(NOW))["id"], "01K8A1");
    let mut failed = reply("01K8A1", 5, Some("studio"));
    failed.status = ReplyStatus::Failed;
    replies.request_done(true, Ok(FeedResult::Reply(failed)), NOW);
    assert!(replies.kept.reports().is_empty());
    drop(replies);
    let mut replies = kept_at(&path, &[]);
    assert_eq!(read_after(replies.next_work(NOW)), 0, "no report left");
}

#[test]
fn an_older_file_naming_replies_written_here_still_loads() {
    // Older releases kept the ids of replies written here (`own`), for a
    // setting that's gone: ignored when read, dropped when saved.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    std::fs::write(
        &path,
        format!(r#"{{"version":1,"own":{{"01K8A1":{at}}},"handled":{{"01K8A2":{at}}}}}"#),
    )
    .unwrap();
    let mut replies = kept_at(&path, &[view("S", Lifecycle::Done, Some(CHAT))]);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(saved.get("own").is_none(), "{saved}");
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let pending = vec![reply("01K8A1", 4, Some("studio")), reply("01K8A2", 5, None)];
    replies.request_done(true, read(pending), NOW);
    // The one handled before isn't typed again; the other is.
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"
    ));
}

#[test]
fn a_renamed_machine_reads_its_replies_from_the_start_under_its_new_name() {
    let mut replies = on(&[]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let mut old = reply("01K8A1", 4, None);
    old.status = ReplyStatus::Delivered;
    replies.request_done(true, read(vec![old]), NOW);
    replies.rename("studio-6m8b".into(), NOW);
    match replies.next_work(NOW) {
        Some(ReplyWork::Request(FeedRequest::Replies { after, machine, .. })) => {
            assert_eq!((after, machine.as_str()), (0, "studio-6m8b"));
        }
        other => panic!("expected a read, got {other:?}"),
    }
}

#[test]
fn a_reply_is_typed_only_while_the_program_seen_is_in_its_sessions_foreground() {
    let mut replies = on(&[view("S", Lifecycle::Done, Some(CHAT))]);
    let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    assert!(
        matches!(&work, Some(ReplyWork::Submit { foreground: Some(program), .. }) if program == "synthetic-agent")
    );
    // The daemon found another program there (the agent exited): it stays
    // pending, unreported, until the session's view says what runs there.
    replies.foreground_changed("01K8A1".into(), NOW);
    let meanwhile = replies.next_work(NOW + TYPING_HOLD / 2);
    assert!(!matches!(meanwhile, Some(ReplyWork::Submit { .. })));
    assert!(!matches!(
        meanwhile,
        Some(ReplyWork::Request(FeedRequest::ReplyStatus { .. }))
    ));
    if matches!(
        meanwhile,
        Some(ReplyWork::Request(FeedRequest::Replies { .. }))
    ) {
        replies.request_done(true, read(vec![]), NOW + TYPING_HOLD / 2);
    }
    // No agent runs there now: it fails as its chat is no longer shown.
    replies.observe(&view("S", Lifecycle::None, None));
    let body = report_of(replies.next_work(NOW + TYPING_HOLD));
    assert_eq!(body["id"], "01K8A1");
    assert_eq!(body["failure"], "conversation_changed");
}

/// Type the next of `replies`'s pending replies (it must be `id`) at `at`
/// (`at_ms` on the wall clock); its agent takes it, and it's reported.
fn type_and_take(replies: &mut ReplySync, id: &str, at: Duration, at_ms: i64) {
    assert_eq!(submitted_reply(replies.next_work(at)).as_deref(), Some(id));
    replies.submitted(id.into(), Ok(()), at, at_ms);
    taken(replies);
    assert_eq!(report_of(replies.next_work(at))["id"], id);
    let mut delivered = reply(id, 9, Some("studio"));
    delivered.status = ReplyStatus::Delivered;
    replies.request_done(true, Ok(FeedResult::Reply(delivered)), at);
}

#[test]
fn back_to_back_replies_each_get_their_own_answer() {
    let mut replies = on(&[view("S", Lifecycle::Idle, Some(CHAT))]);
    let first = reply("01K8A1", 4, Some("studio"));
    let second = reply("01K8A2", 5, Some("studio"));
    assert!(matches!(
        after_reading(&mut replies, vec![first, second]),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"
    ));
    replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
    taken(&mut replies);
    report_of(replies.next_work(NOW));
    replies.request_done(true, read(vec![]), NOW);
    // Its turn ends at 2.9 s; the second goes in at once, before the
    // daemon captured the first's answer.
    let later = NOW + Duration::from_secs(1);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    type_and_take(&mut replies, "01K8A2", later, 3_100);
    // The first's answer (its turn finished at 2.9 s) is the first's; the
    // next one is the second's.
    assert_eq!(
        replies.answered(&answer(Some(CHAT), 2_900)),
        Some("01K8A1".into())
    );
    assert_eq!(
        replies.answered(&answer(Some(CHAT), 9_000)),
        Some("01K8A2".into())
    );
    assert_eq!(replies.answered(&answer(Some(CHAT), 12_000)), None);
}

#[test]
fn a_reply_whose_turn_ended_without_an_answer_never_takes_a_later_one() {
    let mut replies = on(&[view("S", Lifecycle::Idle, Some(CHAT))]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Ok(()), NOW, 2_000);
    taken(&mut replies);
    report_of(replies.next_work(NOW));
    replies.request_done(true, read(vec![]), NOW);
    // The user interrupts it: its turn ends at 3 s with no answer captured.
    replies.observe(&view("S", Lifecycle::Idle, Some(CHAT)));
    let ended = NOW + Duration::from_secs(1);
    replies.next_work(ended);
    // A prompt typed at the desk a minute later is answered: not the reply's.
    assert_eq!(replies.answered(&answer(Some(CHAT), 62_000)), None);
    // Forgotten once no answer can come.
    replies.next_work(ended + ANSWER_WAIT);
    assert!(replies.awaiting.is_empty());
}

/// A read answered with `poll` (rakumux.com's `poll_ms`), at `at`.
fn read_with_poll(replies: &mut ReplySync, poll: Duration, at: Duration) {
    read_after(replies.next_work(at));
    let mut page = read(vec![]);
    if let Ok(FeedResult::Replies(read)) = &mut page {
        read.poll = Some(poll);
    }
    replies.request_done(true, page, at);
}

#[test]
fn while_pokes_come_replies_are_read_when_poked_and_once_a_minute_in_case() {
    let mut replies = on(&[]);
    replies.set_pushed(true, NOW);
    read_with_poll(&mut replies, Duration::from_secs(15), NOW);
    // Nothing until a minute is up…
    assert_eq!(replies.next_work(NOW + Duration::from_secs(59)), None);
    // …unless poked.
    replies.poked(NOW + Duration::from_secs(2));
    read_with_poll(
        &mut replies,
        Duration::from_secs(15),
        NOW + Duration::from_secs(2),
    );
    assert_eq!(replies.next_work(NOW + Duration::from_secs(61)), None);
    assert_eq!(
        read_after(replies.next_work(NOW + Duration::from_secs(62))),
        0
    );
}

#[test]
fn someone_watching_keeps_reads_at_rakumux_coms_pace_while_pushed() {
    let mut replies = on(&[]);
    replies.set_pushed(true, NOW);
    read_with_poll(&mut replies, WATCHED_POLL, NOW);
    assert!(replies.watched());
    assert_eq!(read_after(replies.next_work(NOW + WATCHED_POLL)), 0);
}

#[test]
fn a_poke_during_a_read_reads_again_once_it_is_back() {
    let mut replies = on(&[]);
    replies.set_pushed(true, NOW);
    read_after(replies.next_work(NOW));
    // The change may have come after the read was answered.
    replies.poked(NOW);
    let mut page = read(vec![]);
    if let Ok(FeedResult::Replies(read)) = &mut page {
        read.poll = Some(Duration::from_secs(15));
    }
    replies.request_done(true, page, NOW);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
}

#[test]
fn a_closed_connection_brings_back_rakumux_coms_pace() {
    let mut replies = on(&[]);
    replies.set_pushed(true, NOW);
    read_with_poll(&mut replies, Duration::from_secs(15), NOW);
    replies.set_pushed(false, NOW + Duration::from_secs(1));
    assert_eq!(replies.next_work(NOW + Duration::from_secs(15)), None);
    assert_eq!(
        read_after(replies.next_work(NOW + Duration::from_secs(16))),
        0
    );
}

// Mid-turn: replies typed while an agent that takes input during a turn works.

use session_core::model::AgentInfo;

/// Session S running `agent` (as the daemon names it), `lifecycle`, in CHAT.
fn running(agent: &str, lifecycle: Lifecycle) -> SessionView {
    let mut view = view("S", lifecycle, Some(CHAT));
    view.agent = Some(AgentInfo {
        kind: agent.into(),
        ..AgentInfo::default()
    });
    view
}

/// S running `agent`, `lifecycle`, with a new prompt in its chat.
fn running_prompted(agent: &str, lifecycle: Lifecycle, prompt: &str) -> SessionView {
    let mut view = running(agent, lifecycle);
    view.agent_turn.as_mut().unwrap().id = new_turn();
    view.user_prompt = Some(prompt.into());
    view
}

/// `at` on the wall clock, the reply typed at [`NOW`] being 2 s in.
fn wall(at: Duration) -> i64 {
    2_000 + i64::try_from(at.saturating_sub(NOW).as_millis()).unwrap()
}

/// Rakumux.com took the report of `id`.
fn report_taken(replies: &mut ReplySync, id: &str, at: Duration) {
    let mut delivered = reply(id, 9, Some("studio"));
    delivered.status = ReplyStatus::Delivered;
    replies.request_done(true, Ok(FeedResult::Reply(delivered)), at);
}

#[test]
fn a_reply_goes_into_an_agent_that_takes_input_during_a_turn_while_it_works() {
    for agent in [
        "claude", "codex", "opencode", "pi", "copilot", "grok", "kimi",
    ] {
        let mut replies = on(&[running(agent, Lifecycle::Working)]);
        let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
        assert_eq!(
            work,
            Some(ReplyWork::Submit {
                reply: "01K8A1".into(),
                session: SessionId::from_string("S"),
                text: "Now add a test.".into(),
                foreground: Some("synthetic-agent".into()),
            }),
            "{agent}"
        );
        replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
        // Still at work on the same turn: nothing to say yet.
        replies.observe(&running(agent, Lifecycle::Working));
        assert_eq!(replies.next_work(NOW), None, "{agent}");
        // It takes the reply at its next step (or as the next turn's prompt):
        // its chat shows it as a prompt (Claude Code's `queued_command`,
        // Codex's `user_message`, Pi's and Copilot's steered user message,
        // Grok's and Kimi's next turn).
        replies.observe(&running_prompted(
            agent,
            Lifecycle::Working,
            "Now add a test.",
        ));
        let body = report_of(replies.next_work(NOW));
        assert_eq!(
            (&body["id"], &body["status"]),
            (&"01K8A1".into(), &"delivered".into()),
            "{agent}"
        );
    }
}

#[test]
fn a_blocked_agent_or_one_that_doesnt_take_input_mid_turn_is_waited_for() {
    let held = [
        // Asking something: the text would answer it.
        running("claude", Lifecycle::Blocked),
        running("codex", Lifecycle::Blocked),
        running("pi", Lifecycle::Blocked),
        running("grok", Lifecycle::Blocked),
        running("kimi", Lifecycle::Blocked),
        // What it does isn't known.
        running("claude", Lifecycle::Unknown),
        // Agents not known to take what's typed during a turn.
        running("gemini", Lifecycle::Working),
        running("droid", Lifecycle::Working),
        running("amp", Lifecycle::Working),
        running("cursor-agent", Lifecycle::Working),
        running("fakeagent", Lifecycle::Working),
        view("S", Lifecycle::Working, Some(CHAT)),
    ];
    for shown in held {
        let mut replies = on(std::slice::from_ref(&shown));
        let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
        assert_eq!(work, None, "{:?} {:?}", shown.agent, shown.lifecycle);
        // It goes in once the agent waits.
        let mut waiting = shown.clone();
        waiting.lifecycle = Lifecycle::Done;
        replies.observe(&waiting);
        assert_eq!(
            submitted_reply(replies.next_work(NOW)).as_deref(),
            Some("01K8A1")
        );
    }
    // Without its turns followed (an older daemon), a session message isn't
    // typed into Claude Code at work: its taking it couldn't be seen.
    let mut unfollowed = running("claude", Lifecycle::Working);
    unfollowed.agent_turn = None;
    let mut replies = on(&[unfollowed]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A2", 6, None)]), NOW);
    assert_eq!(replies.next_work(NOW), None);
}

#[test]
fn a_reply_typed_mid_turn_is_delivered_when_its_prompt_shows_minutes_later() {
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
    // A long tool call: the reply waits in Claude Code's queue, with no new
    // prompt long past the 30 s a reply typed to a waiting agent gets.
    let later = NOW + Duration::from_secs(600);
    replies.observe(&running("claude", Lifecycle::Working));
    assert_eq!(next_at(&mut replies, NOW + TAKE_WITHIN), None);
    assert_eq!(next_at(&mut replies, later), None);
    // It asks for a permission meanwhile: still waiting.
    replies.observe(&running("claude", Lifecycle::Blocked));
    assert_eq!(next_at(&mut replies, later), None);
    // The tool call ends and Claude Code takes the queued reply.
    replies.observe(&running_prompted(
        "claude",
        Lifecycle::Working,
        "Now add a test.",
    ));
    let body = report_of(next_at(&mut replies, later));
    assert_eq!(
        (&body["id"], &body["status"]),
        (&"01K8A1".into(), &"delivered".into())
    );
}

#[test]
fn a_reply_typed_mid_turn_fails_only_once_its_turn_ended_without_it() {
    let mut replies = on(&[running("codex", Lifecycle::Working)]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
    assert_eq!(next_at(&mut replies, NOW + 2 * TAKE_WITHIN), None);
    // The turn ends; the agent may take what was queued as its next prompt.
    let ended = NOW + Duration::from_secs(300);
    replies.observe(&running("codex", Lifecycle::Done));
    assert_eq!(next_at(&mut replies, ended), None);
    // It works again without that prompt (the screen lagged): the turn
    // wasn't over after all.
    let again = ended + Duration::from_secs(20);
    replies.observe(&running("codex", Lifecycle::Working));
    assert_eq!(next_at(&mut replies, again), None);
    let over = again + Duration::from_secs(5);
    replies.observe(&running("codex", Lifecycle::Idle));
    assert_eq!(next_at(&mut replies, over), None);
    assert_eq!(
        next_at(&mut replies, over + TAKE_WITHIN - Duration::from_secs(1)),
        None
    );
    // Waiting a while since, and no new prompt: its Enter was lost, or an
    // interrupt put it back in the input box.
    let body = report_of(next_at(&mut replies, over + TAKE_WITHIN));
    assert_eq!(
        (&body["id"], &body["status"], &body["failure"]),
        (&"01K8A1".into(), &"failed".into(), &"not_taken".into())
    );
    // The agent leaving counts as the turn ending too.
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    let mut message = message("01K8A2", 6, None);
    message.text = "Now add a test.".into();
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message]), NOW);
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A2")
    );
    replies.submitted("01K8A2".into(), Ok(()), NOW, wall(NOW));
    let mut shell = view("S", Lifecycle::None, None);
    shell.agent_turn = None;
    replies.observe(&shell);
    assert_eq!(next_at(&mut replies, ended), None);
    let body = report_of(next_at(&mut replies, ended + TAKE_WITHIN));
    assert_eq!(
        (&body["id"], &body["failure"]),
        (&"01K8A2".into(), &"not_taken".into())
    );
}

#[test]
fn a_reply_typed_mid_turn_is_answered_by_the_turn_that_took_it() {
    // Taken at the agent's next step: the run's last reply answers it.
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
    let took = NOW + Duration::from_secs(60);
    let mut shown = running_prompted("claude", Lifecycle::Working, "Now add a test.");
    replies.observe(&shown);
    report_of(next_at(&mut replies, took));
    report_taken(&mut replies, "01K8A1", took);
    let ended = took + Duration::from_secs(60);
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    assert_eq!(next_at(&mut replies, ended), None);
    // The daemon noted the turn's end just before this service heard of it.
    let finished = wall(ended) - 100;
    assert_eq!(replies.answered(&answer(Some("other"), finished)), None);
    assert_eq!(
        replies.answered(&answer(Some(CHAT), finished)),
        Some("01K8A1".into())
    );
    assert_eq!(
        replies.answered(&answer(Some(CHAT), finished + 5_000)),
        None
    );

    // The turn it was typed into ends before the agent takes it (it was
    // writing its last message): that turn's reply isn't its answer, before
    // or after the agent takes it; the turn it starts is.
    let mut replies = on(&[running("codex", Lifecycle::Working)]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
    let old_end = NOW + Duration::from_secs(10);
    replies.observe(&running("codex", Lifecycle::Done));
    assert_eq!(next_at(&mut replies, old_end), None);
    let old_answer = answer(Some(CHAT), wall(old_end) - 100);
    assert_eq!(replies.answered(&old_answer), None);
    let took = old_end + Duration::from_secs(1);
    let mut shown = running_prompted("codex", Lifecycle::Working, "Now add a test.");
    replies.observe(&shown);
    report_of(next_at(&mut replies, took));
    report_taken(&mut replies, "01K8A1", took);
    assert_eq!(replies.answered(&old_answer), None);
    let ended = took + Duration::from_secs(30);
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    next_at(&mut replies, ended);
    assert_eq!(
        replies.answered(&answer(Some(CHAT), wall(ended) - 100)),
        Some("01K8A1".into())
    );

    // Taken as the next prompt once the turn ended, its view still showing
    // the agent waiting (the lifecycle lags): its turn is the one it started.
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
    replies.observe(&running("claude", Lifecycle::Working));
    let old_end = NOW + Duration::from_secs(10);
    replies.observe(&running("claude", Lifecycle::Done));
    assert_eq!(next_at(&mut replies, old_end), None);
    let mut shown = running_prompted("claude", Lifecycle::Done, "Now add a test.");
    replies.observe(&shown);
    report_of(next_at(&mut replies, old_end));
    report_taken(&mut replies, "01K8A1", old_end);
    shown.lifecycle = Lifecycle::Working;
    replies.observe(&shown);
    next_at(&mut replies, old_end + Duration::from_secs(1));
    let ended = old_end + Duration::from_secs(40);
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    next_at(&mut replies, ended);
    assert_eq!(
        replies.answered(&answer(Some(CHAT), wall(ended) - 100)),
        Some("01K8A1".into())
    );

    // Taken, then interrupted: no answer, and a later one isn't its.
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
    let mut shown = running_prompted("claude", Lifecycle::Working, "Now add a test.");
    replies.observe(&shown);
    report_of(next_at(&mut replies, NOW));
    report_taken(&mut replies, "01K8A1", NOW);
    let interrupted = NOW + Duration::from_secs(5);
    shown.lifecycle = Lifecycle::Idle;
    replies.observe(&shown);
    next_at(&mut replies, interrupted);
    let later = interrupted + Duration::from_secs(60);
    assert_eq!(replies.answered(&answer(Some(CHAT), wall(later))), None);
}

#[test]
fn the_turn_a_reply_ended_at_once_keeps_its_own_reply_and_the_turn_it_began_answers_it() {
    // Queued (Grok, Kimi) or steered in (Pi, Copilot), the reply starts its
    // turn the moment the one it was typed into ends, never seen waiting:
    // the daemon captures that turn's own reply as the take is read, at
    // when it was said.
    for agent in ["grok", "kimi", "pi", "copilot"] {
        let mut replies = on(&[running(agent, Lifecycle::Working)]);
        after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
        replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
        let said = NOW + Duration::from_secs(8);
        let held = answer(Some(CHAT), wall(said));
        // Its event comes before the view showing the take: not taken yet,
        // nothing it answers.
        assert_eq!(replies.answered(&held), None, "{agent}");
        let took = said + Duration::from_millis(300);
        let mut shown = running_prompted(agent, Lifecycle::Working, "Now add a test.");
        replies.observe(&shown);
        report_of(next_at(&mut replies, took));
        report_taken(&mut replies, "01K8A1", took);
        // Nor after it: it was said before the take.
        assert_eq!(replies.answered(&held), None, "{agent}");
        // The turn the reply began, seen to end: its reply is the answer.
        let ended = took + Duration::from_secs(20);
        shown.lifecycle = Lifecycle::Done;
        replies.observe(&shown);
        next_at(&mut replies, ended);
        assert_eq!(
            replies.answered(&answer(Some(CHAT), wall(ended) - 100)),
            Some("01K8A1".into()),
            "{agent}"
        );
    }
}

#[test]
fn replies_written_one_after_another_mid_turn_each_get_their_turns_answer() {
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    let work = after_reading(
        &mut replies,
        vec![said("01K8A1", 4, "First."), said("01K8A2", 5, "Second.")],
    );
    assert_eq!(submitted_reply(work).as_deref(), Some("01K8A1"));
    replies.submitted("01K8A1".into(), Ok(()), NOW, wall(NOW));
    // Not taken yet: the second waits behind it.
    assert_eq!(next_at(&mut replies, NOW + Duration::from_secs(20)), None);
    // Claude Code takes the first at its next step and works on: the second
    // goes in, queued in the turn, without waiting for the turn to end.
    let took = NOW + Duration::from_secs(30);
    let mut shown = running_prompted("claude", Lifecycle::Working, "First.");
    replies.observe(&shown);
    assert_eq!(
        submitted_reply(next_at(&mut replies, took)).as_deref(),
        Some("01K8A2")
    );
    replies.submitted("01K8A2".into(), Ok(()), took, wall(took));
    assert_eq!(report_of(next_at(&mut replies, took))["id"], "01K8A1");
    report_taken(&mut replies, "01K8A1", took);
    // The turn ends with its own reply, and Claude Code takes the second as
    // its next prompt.
    let ended = NOW + Duration::from_secs(120);
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    assert_eq!(next_at(&mut replies, ended), None);
    let mut shown = running_prompted("claude", Lifecycle::Working, "Second.");
    replies.observe(&shown);
    assert_eq!(report_of(next_at(&mut replies, ended))["id"], "01K8A2");
    report_taken(&mut replies, "01K8A2", ended);
    // The first turn's answer, captured after the second was taken, is the
    // first's; the second's turn's is the second's.
    assert_eq!(
        replies.answered(&answer(Some(CHAT), wall(ended) - 100)),
        Some("01K8A1".into())
    );
    let second = ended + Duration::from_secs(40);
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    next_at(&mut replies, second);
    assert_eq!(
        replies.answered(&answer(Some(CHAT), wall(second) - 100)),
        Some("01K8A2".into())
    );
}

/// A reply to S's chat saying `text`.
fn said(id: &str, seq: u64, text: &str) -> FeedReply {
    let mut said = reply(id, seq, None);
    said.text = text.into();
    said
}

/// What `replies` does at `at` (reads answered with nothing new) until it
/// has nothing more to do: the replies it typed in, in order, the daemon
/// typing each, and those it reported delivered, rakumux.com taking each.
fn worked_at(replies: &mut ReplySync, at: Duration) -> (Vec<String>, Vec<String>) {
    let (mut typed, mut reported) = (Vec::new(), Vec::new());
    loop {
        match next_at(replies, at) {
            Some(ReplyWork::Submit { reply, .. }) => {
                assert!(replies.daemon_answered(reply.clone(), Ok(()), at, wall(at)));
                typed.push(reply);
            }
            Some(ReplyWork::Request(FeedRequest::ReplyStatus { body })) => {
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["status"], "delivered", "{body}");
                let id = body["id"].as_str().unwrap().to_owned();
                report_taken(replies, &id, at);
                reported.push(id);
            }
            None => return (typed, reported),
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// `ids` as owned strings, to compare with [`worked_at`]'s.
fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|&id| id.to_owned()).collect()
}

/// Read `pending` (rakumux.com's first read since the service started).
fn read_pending(replies: &mut ReplySync, pending: Vec<FeedReply>, at: Duration) {
    assert_eq!(read_after(replies.next_work(at)), 0);
    replies.request_done(true, read(pending), at);
}

#[test]
fn stuck_queue_replies_to_an_agent_that_never_stops_each_go_in_once_the_one_before_was_taken() {
    // Claude Code kept at work for good (background subagents, one turn
    // after another): never seen waiting, never asking anything.
    let mut shown = running("claude", Lifecycle::Working);
    let mut replies = on(std::slice::from_ref(&shown));
    let pending = vec![
        said("01K8A3", 6, "Third."),
        said("01K8A1", 4, "First."),
        said("01K8A2", 5, "Second."),
    ];
    read_pending(&mut replies, pending, NOW);
    assert_eq!(worked_at(&mut replies, NOW), (ids(&["01K8A1"]), vec![]));
    let mut at = NOW;
    for (id, text, next) in [
        ("01K8A1", "First.", Some("01K8A2")),
        ("01K8A2", "Second.", Some("01K8A3")),
        ("01K8A3", "Third.", None),
    ] {
        // Nothing more goes in until the agent takes it, however long that is.
        at += Duration::from_secs(300);
        replies.observe(&shown);
        assert_eq!(worked_at(&mut replies, at), (vec![], vec![]), "{id}");
        // Taken at its next step, the agent working on: delivered, and the
        // next goes in, in rakumux.com's order.
        shown = running_prompted("claude", Lifecycle::Working, text);
        replies.observe(&shown);
        at += Duration::from_secs(1);
        let typed: Vec<String> = next.into_iter().map(Into::into).collect();
        assert_eq!(worked_at(&mut replies, at), (typed, ids(&[id])), "{id}");
    }
    assert!(replies.pending.is_empty());
}

/// The bug behind replies with files never confirmed delivered: a reply of
/// several lines with two files, typed into Claude Code while it worked.
/// Claude Code logs it, once it takes it, as one paste block, and the
/// daemon now reads that as the new turn's prompt: `user_prompt` is the
/// reply's first line (`integrations/output.rs`'s
/// `pasted_prompt_bug_a_reply_with_files_queued_mid_turn_begins_a_turn_with_its_first_line`,
/// and `control_queued_prompt.rs`'s for the daemon's view). From that view
/// the reply is delivered and the session's next reply goes in; from the
/// view an older daemon gave (no new turn, the prompt before), it waited
/// for ever and held the next one behind it.
#[test]
fn pasted_prompt_bug_a_reply_with_files_typed_mid_turn_is_delivered_by_its_first_line_and_frees_the_next(
) {
    let (home, uploads) = uploads();
    let mut shown = running("claude", Lifecycle::Working);
    shown.user_prompt = Some("Fix the parser".into());
    let mut replies = on(std::slice::from_ref(&shown));
    let mut with_files = with_files(
        ATTACHED,
        vec![file('A', "image.png", 4), file('B', "image.png", 3)],
    );
    with_files.text = "Here's what I sent from the web app.\nBoth show the error.".into();
    let pending = vec![with_files, said("01K8A9", 5, "Then the docs.")];
    let first = fetch_of(after_reading(&mut replies, pending));
    let fetched = served(&first, &uploads, 200, b"\x89PNG");
    replies.fetched(true, ATTACHED.into(), fetched, NOW);
    let second = fetch_of(replies.next_work(NOW));
    let fetched = served(&second, &uploads, 200, b"PNG");
    replies.fetched(true, ATTACHED.into(), fetched, NOW);
    let dir = home.path().join("uploads").join(ATTACHED);
    assert_eq!(
        typed_text(replies.next_work(NOW)),
        format!(
            "Here's what I sent from the web app.\nBoth show the error.\n\n{}\n{}",
            dir.join("image.png").display(),
            dir.join("image (2).png").display()
        )
    );
    assert!(replies.daemon_answered(ATTACHED.into(), Ok(()), NOW, wall(NOW)));

    // As an older daemon showed it once Claude Code took it: the same turn,
    // the prompt before. Nothing is reported, and the next reply waits.
    let mut at = NOW;
    for _ in 0..3 {
        at += Duration::from_secs(60);
        replies.observe(&shown);
        assert_eq!(worked_at(&mut replies, at), (vec![], vec![]));
    }

    // As the daemon shows it now: a new turn, its prompt the reply's first
    // line, the agent at work. Delivered, and the next reply goes in.
    shown = running_prompted(
        "claude",
        Lifecycle::Working,
        "Here's what I sent from the web app.",
    );
    replies.observe(&shown);
    at += Duration::from_secs(1);
    assert_eq!(
        worked_at(&mut replies, at),
        (ids(&["01K8A9"]), ids(&[ATTACHED]))
    );
    shown = running_prompted("claude", Lifecycle::Working, "Then the docs.");
    replies.observe(&shown);
    at += Duration::from_secs(1);
    assert_eq!(worked_at(&mut replies, at), (vec![], ids(&["01K8A9"])));
    assert!(replies.pending.is_empty());
}

#[test]
fn of_replies_an_agent_takes_within_one_turn_the_last_taken_before_its_answer_has_it() {
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    let pending = vec![
        said("01K8A1", 4, "First."),
        said("01K8A2", 5, "Second."),
        said("01K8A3", 6, "Third."),
    ];
    read_pending(&mut replies, pending, NOW);
    assert_eq!(worked_at(&mut replies, NOW).0, ["01K8A1"]);
    // Steered into the turn it was typed into, between tool steps: the turn
    // goes on, and an answer said before the take isn't the first's.
    let first = NOW + Duration::from_secs(10);
    replies.observe(&running_prompted("claude", Lifecycle::Working, "First."));
    assert_eq!(
        worked_at(&mut replies, first),
        (ids(&["01K8A2"]), ids(&["01K8A1"]))
    );
    assert_eq!(
        replies.answered(&answer(Some(CHAT), wall(first) - 1_000)),
        None
    );
    // That turn ends with its own reply, and Claude Code takes the second as
    // its next prompt at once, never seen waiting.
    let said_at = NOW + Duration::from_secs(30);
    let second = said_at + Duration::from_millis(300);
    replies.observe(&running_prompted("claude", Lifecycle::Working, "Second."));
    assert_eq!(
        worked_at(&mut replies, second),
        (ids(&["01K8A3"]), ids(&["01K8A2"]))
    );
    // Its reply, heard of after the second was taken, was said before that:
    // the first's, never the second's.
    assert_eq!(
        replies.answered(&answer(Some(CHAT), wall(said_at))),
        Some("01K8A1".into())
    );
    // The third is steered into the second's turn between tool steps.
    let third = second + Duration::from_secs(20);
    let mut shown = running_prompted("claude", Lifecycle::Working, "Third.");
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, third), (vec![], ids(&["01K8A3"])));
    // That turn's one answer is the last reply taken in it, the third's; the
    // second has none, and nothing later is anyone's.
    let ended = third + Duration::from_secs(60);
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    worked_at(&mut replies, ended);
    assert_eq!(
        replies.answered(&answer(Some(CHAT), wall(ended) - 100)),
        Some("01K8A3".into())
    );
    assert_eq!(
        replies.answered(&answer(Some(CHAT), wall(ended) + 60_000)),
        None
    );
    assert!(replies.awaiting.is_empty());
}

#[test]
fn a_restart_while_replies_go_into_an_agent_that_never_stops_holds_nothing_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let pending = [
        said("01K8A1", 4, "First."),
        said("01K8A2", 5, "Second."),
        said("01K8A3", 6, "Third."),
    ];
    {
        let mut replies = kept_at(&path, &[running("claude", Lifecycle::Working)]);
        read_pending(&mut replies, pending.to_vec(), NOW);
        assert_eq!(worked_at(&mut replies, NOW).0, ["01K8A1"]);
        // The agent takes it, and the service stops (an update replaces it)
        // before it saw that.
    }
    // Restarted between replies, the agent at work on the first: once read
    // again, the first is seen taken and reported delivered, never typed
    // again, and the second goes in.
    let at = NOW + Duration::from_secs(5);
    let shown = running_prompted("claude", Lifecycle::Working, "First.");
    let mut replies = kept_at(&path, std::slice::from_ref(&shown));
    read_pending(&mut replies, pending.to_vec(), at);
    assert_eq!(
        worked_at(&mut replies, at),
        (ids(&["01K8A2"]), ids(&["01K8A1"]))
    );
    replies.observe(&running_prompted("claude", Lifecycle::Working, "Second."));
    assert_eq!(
        worked_at(&mut replies, at),
        (ids(&["01K8A3"]), ids(&["01K8A2"]))
    );
}

/// Restarted with a mid-turn reply going in: replies.json kept where the
/// first of `pending` went, typed while S's Claude Code worked on `turn`
/// (none: its first view's) and not taken yet. Returns the restarted
/// service, its agent at work as `shown`, with `pending` read again.
fn restarted_with_one_in_flight(
    path: &std::path::Path,
    pending: &[FeedReply],
    shown: &SessionView,
    at: Duration,
) -> ReplySync {
    {
        let mut replies = kept_at(path, &[running("claude", Lifecycle::Working)]);
        read_pending(&mut replies, pending.to_vec(), NOW);
        assert_eq!(worked_at(&mut replies, NOW).0, [pending[0].id.as_str()]);
    }
    let mut replies = kept_at(path, std::slice::from_ref(shown));
    read_pending(&mut replies, pending.to_vec(), at);
    replies
}

#[test]
fn restart_gap_a_reply_typed_mid_turn_and_not_yet_taken_holds_its_session_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let pending = [said("01K8A1", 4, "First."), said("01K8A2", 5, "Second.")];
    // Restarted while the first waits in Claude Code's queue (a long tool
    // call): it isn't reported, and the second doesn't go in beside it…
    let at = NOW + Duration::from_secs(5);
    let at_work = running("claude", Lifecycle::Working);
    let mut replies = restarted_with_one_in_flight(&path, &pending, &at_work, at);
    assert_eq!(worked_at(&mut replies, at), (vec![], vec![]));
    let later = at + Duration::from_secs(600);
    replies.observe(&at_work);
    assert_eq!(worked_at(&mut replies, later), (vec![], vec![]));
    // …until the agent takes the first: delivered, and the second goes in.
    replies.observe(&running_prompted("claude", Lifecycle::Working, "First."));
    assert_eq!(
        worked_at(&mut replies, later),
        (ids(&["01K8A2"]), ids(&["01K8A1"]))
    );
    drop(replies);

    // Taken while the service was down: delivered once read again, and the
    // second goes in.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let took = running_prompted("claude", Lifecycle::Working, "First.");
    let mut replies = restarted_with_one_in_flight(&path, &pending, &took, at);
    assert_eq!(
        worked_at(&mut replies, at),
        (ids(&["01K8A2"]), ids(&["01K8A1"]))
    );
}

#[test]
fn restart_gap_a_reply_in_flight_at_a_restart_is_decided_as_before_and_never_holds_for_ever() {
    let pending = [said("01K8A1", 4, "First."), said("01K8A2", 5, "Second.")];
    let at = NOW + Duration::from_secs(5);
    // Never taken: once its agent has waited 30 s without taking it, it
    // fails `not_taken`, and the second goes in.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let at_work = running("claude", Lifecycle::Working);
    let mut replies = restarted_with_one_in_flight(&path, &pending, &at_work, at);
    let waits = at + Duration::from_secs(60);
    replies.observe(&running("claude", Lifecycle::Idle));
    assert_eq!(next_at(&mut replies, waits), None);
    let over = waits + TAKE_WITHIN;
    assert_eq!(
        submitted_reply(next_at(&mut replies, over)).as_deref(),
        Some("01K8A2")
    );
    let body = report_of(next_at(&mut replies, over));
    assert_eq!(
        (&body["id"], &body["failure"]),
        (&"01K8A1".into(), &"not_taken".into())
    );

    // No longer pending on rakumux.com (expired meanwhile): its session no
    // longer waits for it, and it's never typed again.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    {
        let mut replies = kept_at(&path, &[at_work.clone()]);
        read_pending(&mut replies, pending.to_vec(), NOW);
        assert_eq!(worked_at(&mut replies, NOW).0, ["01K8A1"]);
    }
    let mut replies = kept_at(&path, std::slice::from_ref(&at_work));
    let mut expired = pending[0].clone();
    expired.status = ReplyStatus::Expired;
    expired.seq = 6;
    read_pending(&mut replies, vec![expired, pending[1].clone()], at);
    assert_eq!(worked_at(&mut replies, at), (ids(&["01K8A2"]), vec![]));
    drop(replies);
    let mut replies = kept_at(&path, std::slice::from_ref(&at_work));
    read_pending(&mut replies, vec![pending[0].clone()], at);
    assert_eq!(worked_at(&mut replies, at), (vec![], vec![]));
}

#[test]
fn restart_gap_an_older_file_without_where_a_reply_went_still_loads() {
    // An older release kept a reply handed to the daemon by its id alone:
    // reported delivered on load as before, never typed again, holding
    // nothing back.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    std::fs::write(
        &path,
        format!(
            r#"{{"version":1,"handled":{{"01K8A1":{at}}},"typing":{{"01K8A1":{at}}},"reports":[]}}"#
        ),
    )
    .unwrap();
    let mut replies = kept_at(&path, &[running("claude", Lifecycle::Working)]);
    let body = report_of(replies.next_work(NOW));
    assert_eq!(
        (&body["id"], &body["status"]),
        (&"01K8A1".into(), &"delivered".into())
    );
    report_taken(&mut replies, "01K8A1", NOW);
    let pending = vec![said("01K8A1", 4, "First."), said("01K8A2", 5, "Second.")];
    read_pending(&mut replies, pending, NOW);
    assert_eq!(worked_at(&mut replies, NOW), (ids(&["01K8A2"]), vec![]));
    // What this release keeps loads in the older one's terms too: the new
    // field is apart from the ones it reads.
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["version"], 1);
    assert!(saved["typing"].get("01K8A2").is_some(), "{saved}");
    assert_eq!(saved["typed_into"]["01K8A2"]["session"], "S", "{saved}");
    assert_eq!(saved["typed_into"]["01K8A2"]["mid_turn"], true, "{saved}");
}

#[test]
fn regression_replies_written_while_claude_code_works_for_40_minutes_across_a_restart_all_go_in() {
    // The production failure of 2026-10-30: a reply was handled at 09:51;
    // more were written while the session's Claude Code stayed `working`
    // for 40 minutes (background subagents, one turn after another), the
    // account service restarted once in between, and the later replies were
    // never tried: replies.json had nothing for them. Each must go in, in
    // order, as soon as the one before was taken, the restart holding the
    // one in flight until it's taken and nothing for ever.
    let nine = |minute: u64, second: u64| NOW + Duration::from_secs(minute * 60 + second - 51 * 60);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let first = said("01K8A1", 136, "Look at the parser.");
    let second = said("01K8A2", 138, "Also the lexer.");
    let third = said("01K8A3", 140, "And add tests.");
    let fourth = said("01K8A4", 142, "Then run them.");
    let fifth = said("01K8A5", 143, "Commit when green.");
    let delivered = |reply: &FeedReply, seq| {
        let mut reply = reply.clone();
        reply.seq = seq;
        reply.status = ReplyStatus::Delivered;
        reply
    };
    let read_in = |replies: &mut ReplySync, written: Vec<FeedReply>, at: Duration| {
        read_after(next_at_reading(replies, at));
        replies.request_done(true, read(written), at);
    };
    // Its agent at work on the second reply, the same turn before the
    // restart and after.
    let at_work = running_prompted("claude", Lifecycle::Working, "Also the lexer.");
    {
        let mut replies = kept_at(&path, &[running("claude", Lifecycle::Working)]);
        // 09:51:17: the first goes in mid-turn and is taken at once.
        read_pending(&mut replies, vec![first.clone()], nine(51, 17));
        assert_eq!(worked_at(&mut replies, nine(51, 17)).0, ["01K8A1"]);
        replies.observe(&running_prompted(
            "claude",
            Lifecycle::Working,
            "Look at the parser.",
        ));
        assert_eq!(
            worked_at(&mut replies, nine(51, 18)),
            (vec![], ids(&["01K8A1"]))
        );
        // 09:53:00: the second is written while the agent works on: it goes in.
        read_in(&mut replies, vec![second.clone()], nine(53, 0));
        assert_eq!(
            worked_at(&mut replies, nine(53, 0)),
            (ids(&["01K8A2"]), vec![]),
            "a reply written while the agent works goes in once the one before was taken"
        );
        replies.observe(&at_work);
        assert_eq!(
            worked_at(&mut replies, nine(53, 30)),
            (vec![], ids(&["01K8A2"]))
        );
        // 09:53:41: the third goes in, then waits in Claude Code's queue
        // behind a long tool call.
        read_in(&mut replies, vec![third.clone()], nine(53, 41));
        assert_eq!(
            worked_at(&mut replies, nine(53, 41)),
            (ids(&["01K8A3"]), vec![])
        );
        // 09:54:24: the account service restarts.
    }
    let mut replies = kept_at(&path, std::slice::from_ref(&at_work));
    // Its first work: no report of the third, which the agent hasn't taken,
    // then a read from the start.
    let mut reported = Vec::new();
    let read_first = loop {
        match replies.next_work(nine(54, 25)) {
            Some(ReplyWork::Request(FeedRequest::ReplyStatus { body })) => {
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let id = body["id"].as_str().unwrap().to_owned();
                report_taken(&mut replies, &id, nine(54, 25));
                reported.push(id);
            }
            work => break work,
        }
    };
    assert!(
        reported.is_empty(),
        "the reply in flight at the restart is followed, not reported: {reported:?}"
    );
    assert_eq!(read_after(read_first), 0);
    replies.request_done(
        true,
        read(vec![
            delivered(&first, 137),
            delivered(&second, 139),
            third.clone(),
        ]),
        nine(54, 25),
    );
    assert_eq!(
        worked_at(&mut replies, nine(54, 25)),
        (vec![], vec![]),
        "the reply in flight at the restart holds its session"
    );
    // 09:57:52: the fourth is written while the third still waits: it waits
    // behind the third, not beside it.
    read_in(&mut replies, vec![fourth.clone()], nine(57, 52));
    replies.observe(&at_work);
    assert_eq!(
        worked_at(&mut replies, nine(57, 52)),
        (vec![], vec![]),
        "the reply in flight at the restart still holds its session"
    );
    // 09:59: Claude Code takes the third; the fourth goes in and is taken.
    replies.observe(&running_prompted(
        "claude",
        Lifecycle::Working,
        "And add tests.",
    ));
    assert_eq!(
        worked_at(&mut replies, nine(59, 0)),
        (ids(&["01K8A4"]), ids(&["01K8A3"])),
        "the reply in flight at the restart, once taken, lets the next in"
    );
    replies.observe(&running_prompted(
        "claude",
        Lifecycle::Working,
        "Then run them.",
    ));
    assert_eq!(
        worked_at(&mut replies, nine(59, 30)),
        (vec![], ids(&["01K8A4"]))
    );
    // 10:10:11: the fifth, the agent still at work: in, and taken.
    read_in(&mut replies, vec![fifth.clone()], nine(70, 11));
    assert_eq!(
        worked_at(&mut replies, nine(70, 11)),
        (ids(&["01K8A5"]), vec![])
    );
    let mut shown = running_prompted("claude", Lifecycle::Working, "Commit when green.");
    replies.observe(&shown);
    assert_eq!(
        worked_at(&mut replies, nine(70, 40)),
        (vec![], ids(&["01K8A5"]))
    );
    // 10:31: still at work; nothing left, and every reply was typed in once.
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, nine(91, 0)), (vec![], vec![]));
    assert!(replies.pending.is_empty());
    for id in ["01K8A1", "01K8A2", "01K8A3", "01K8A4", "01K8A5"] {
        assert!(replies.kept.handled(id), "{id}");
    }
    // The agent finally waits: nothing is left to type.
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, nine(92, 0)), (vec![], vec![]));
}

#[test]
fn an_agent_asking_something_holds_the_next_reply_even_once_the_last_was_taken() {
    // Its chat shows the first taken, its agent asking for a permission.
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    let pending = vec![said("01K8A1", 4, "First."), said("01K8A2", 5, "Second.")];
    read_pending(&mut replies, pending, NOW);
    assert_eq!(worked_at(&mut replies, NOW).0, ["01K8A1"]);
    let mut shown = running_prompted("claude", Lifecycle::Blocked, "First.");
    replies.observe(&shown);
    let later = NOW + Duration::from_secs(600);
    assert_eq!(worked_at(&mut replies, later), (vec![], ids(&["01K8A1"])));
    // Answered, it works on: the second goes in.
    shown.lifecycle = Lifecycle::Working;
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, later).0, ["01K8A2"]);

    // Taken and working on when it starts asking, before the next was read.
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    read_pending(&mut replies, vec![said("01K8A1", 4, "First.")], NOW);
    assert_eq!(worked_at(&mut replies, NOW).0, ["01K8A1"]);
    let mut shown = running_prompted("claude", Lifecycle::Working, "First.");
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, NOW), (vec![], ids(&["01K8A1"])));
    shown.lifecycle = Lifecycle::Blocked;
    replies.observe(&shown);
    assert_eq!(read_after(next_at_reading(&mut replies, later)), 4);
    replies.request_done(true, read(vec![said("01K8A2", 5, "Second.")]), later);
    assert_eq!(worked_at(&mut replies, later), (vec![], vec![]));
    shown.lifecycle = Lifecycle::Working;
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, later).0, ["01K8A2"]);
}

/// Whether S's newest reply going in was typed mid-turn.
fn typed_mid_turn(replies: &ReplySync) -> bool {
    replies.deliveries[&SessionId::from_string("S")]
        .last()
        .unwrap()
        .mid_turn
}

#[test]
fn a_daemon_without_the_prompt_guard_gets_replies_to_an_agent_at_work_only_once_it_waits() {
    // Updated with `U`, the server not restarted: its daemon lacks
    // `session.submit.blocked`, so it would type a reply into a permission
    // prompt that opened as it went in, and its Enter would answer it.
    let mut shown = running("claude", Lifecycle::Working);
    let mut replies = ReplySync::new("studio".into(), None);
    replies.set_enabled(true, NOW);
    replies.connected(std::slice::from_ref(&shown), UNGUARDED);
    let pending = vec![said("01K8A1", 4, "First."), said("01K8A2", 5, "Second.")];
    read_pending(&mut replies, pending, NOW);
    // Claude Code at work: held, however long it works.
    assert_eq!(worked_at(&mut replies, NOW), (vec![], vec![]));
    let mut at = NOW + Duration::from_secs(600);
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, at), (vec![], vec![]));
    // It waits: the first goes in, typed to a waiting agent.
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    assert_eq!(
        next_at(&mut replies, at)
            .and_then(|work| submitted_reply(Some(work)))
            .as_deref(),
        Some("01K8A1")
    );
    assert!(!typed_mid_turn(&replies));
    assert!(replies.daemon_answered("01K8A1".into(), Ok(()), at, wall(at)));
    // It takes it and works on it: delivered, and the second waits for it
    // to wait again, not only for that take.
    shown = running_prompted("claude", Lifecycle::Working, "First.");
    replies.observe(&shown);
    at += Duration::from_secs(1);
    assert_eq!(worked_at(&mut replies, at), (vec![], ids(&["01K8A1"])));
    at += Duration::from_secs(600);
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, at), (vec![], vec![]));
    shown.lifecycle = Lifecycle::Idle;
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, at), (ids(&["01K8A2"]), vec![]));
    assert!(!typed_mid_turn(&replies));

    // A session message to it waits the same way.
    let mut replies = ReplySync::new("studio".into(), None);
    replies.set_enabled(true, NOW);
    replies.connected(&[running("claude", Lifecycle::Working)], UNGUARDED);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A3", 6, None)]), NOW);
    assert_eq!(replies.next_work(NOW), None);

    // With the guard, the same reply goes in while it works.
    let mut replies = on(&[running("claude", Lifecycle::Working)]);
    read_pending(&mut replies, vec![said("01K8A1", 4, "First.")], NOW);
    assert_eq!(worked_at(&mut replies, NOW), (ids(&["01K8A1"]), vec![]));
    assert!(typed_mid_turn(&replies));
}

#[test]
fn a_daemon_restarted_with_the_prompt_guard_gets_replies_mid_turn_again() {
    let shown = running("claude", Lifecycle::Working);
    let mut replies = ReplySync::new("studio".into(), None);
    replies.set_enabled(true, NOW);
    replies.connected(std::slice::from_ref(&shown), UNGUARDED);
    read_pending(&mut replies, vec![said("01K8A1", 4, "First.")], NOW);
    assert_eq!(worked_at(&mut replies, NOW), (vec![], vec![]));
    // `S` restarts the daemon in place on this build: the service connects
    // again, and the daemon now refuses to type into a prompt.
    let at = NOW + Duration::from_secs(5);
    replies.connected(std::slice::from_ref(&shown), GUARDED);
    assert_eq!(worked_at(&mut replies, at), (ids(&["01K8A1"]), vec![]));
    assert!(typed_mid_turn(&replies));
    // Taken at its next step, the agent at work: the next one goes in too.
    read_pending_more(&mut replies, vec![said("01K8A2", 5, "Second.")], at);
    replies.observe(&running_prompted("claude", Lifecycle::Working, "First."));
    let at = at + Duration::from_secs(1);
    assert_eq!(
        worked_at(&mut replies, at),
        (ids(&["01K8A2"]), ids(&["01K8A1"]))
    );
    assert!(typed_mid_turn(&replies));
    // Connected again to a daemon without it (replaced by an older one):
    // what's pending waits for the agent to wait.
    read_pending_more(&mut replies, vec![said("01K8A3", 6, "Third.")], at);
    replies.connected(
        &[running_prompted("claude", Lifecycle::Working, "Second.")],
        UNGUARDED,
    );
    let at = at + Duration::from_secs(1);
    assert_eq!(worked_at(&mut replies, at), (vec![], ids(&["01K8A2"])));
}

/// Read `pending` after earlier reads.
fn read_pending_more(replies: &mut ReplySync, pending: Vec<FeedReply>, at: Duration) {
    replies.read_now(at);
    assert!(matches!(
        replies.next_work(at),
        Some(ReplyWork::Request(FeedRequest::Replies { .. }))
    ));
    replies.request_done(true, read(pending), at);
}

/// The next work at `at`, a read expected.
fn next_at_reading(replies: &mut ReplySync, at: Duration) -> Option<ReplyWork> {
    replies.read_now(at);
    replies.next_work(at)
}

#[test]
fn an_agent_that_doesnt_take_input_mid_turn_gets_the_next_reply_only_once_it_waits() {
    // Gemini CLI, not known to take what's typed during a turn.
    let mut replies = on(&[running("gemini", Lifecycle::Done)]);
    let pending = vec![said("01K8A1", 4, "First."), said("01K8A2", 5, "Second.")];
    read_pending(&mut replies, pending, NOW);
    assert_eq!(worked_at(&mut replies, NOW).0, ["01K8A1"]);
    // Its chat shows the first taken, and it works on it a long while.
    let mut shown = running_prompted("gemini", Lifecycle::Working, "First.");
    replies.observe(&shown);
    let later = NOW + Duration::from_secs(600);
    assert_eq!(worked_at(&mut replies, later), (vec![], ids(&["01K8A1"])));
    // It waits: the second goes in.
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, later).0, ["01K8A2"]);

    // Claude Code without its turns followed (an older daemon, without the
    // guard): its take can't be seen, only its working, so its session
    // messages wait for it to wait as before.
    let mut shown = running("claude", Lifecycle::Done);
    shown.agent_turn = None;
    let mut replies = on(std::slice::from_ref(&shown));
    replies.connected(std::slice::from_ref(&shown), UNGUARDED);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(
        true,
        read_messages(vec![message("01K8A1", 4, None), message("01K8A2", 5, None)]),
        NOW,
    );
    assert_eq!(worked_at(&mut replies, NOW).0, ["01K8A1"]);
    shown.lifecycle = Lifecycle::Working;
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, later), (vec![], ids(&["01K8A1"])));
    shown.lifecycle = Lifecycle::Idle;
    replies.observe(&shown);
    assert_eq!(worked_at(&mut replies, later).0, ["01K8A2"]);
}

#[test]
fn a_session_message_goes_into_an_agent_at_work_in_whatever_chat_it_shows() {
    let mut at_work = running("claude", Lifecycle::Working);
    at_work.agent_turn.as_mut().unwrap().conversation_id = Some("another".into());
    let mut replies = on(&[at_work]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(
        true,
        read_messages(vec![message("01K8A2", 6, Some(CHAT))]),
        NOW,
    );
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A2")
    );
    replies.submitted("01K8A2".into(), Ok(()), NOW, wall(NOW));
    let mut shown = running_prompted("claude", Lifecycle::Working, "Start on the parser.");
    shown.agent_turn.as_mut().unwrap().conversation_id = Some("another".into());
    replies.observe(&shown);
    let body = report_of(next_at(&mut replies, NOW + Duration::from_secs(90)));
    assert_eq!(
        (&body["id"], &body["status"]),
        (&"01K8A2".into(), &"delivered".into())
    );
    let ended = NOW + Duration::from_secs(120);
    shown.lifecycle = Lifecycle::Done;
    replies.observe(&shown);
    replies.request_done(true, read(vec![]), ended);
    next_at(&mut replies, ended);
    assert_eq!(
        replies.answered(&answer(Some("another"), wall(ended) - 100)),
        Some("01K8A2".into())
    );
}

#[test]
fn a_reply_the_daemon_refuses_for_now_is_tried_again_shortly_and_never_reported() {
    let refusals = [
        // Someone typed there.
        ErrorCode::SessionInUse,
        // Its agent left the foreground.
        ErrorCode::ForegroundChanged,
        // Its agent opened a permission prompt (or a question, or a menu)
        // after its view said it worked: an Enter would answer it.
        ErrorCode::SessionBlocked,
    ];
    for refusal in refusals {
        for shown in [
            running("claude", Lifecycle::Working),
            running("codex", Lifecycle::Done),
        ] {
            let what = format!("{refusal:?} {:?}", shown.lifecycle);
            let mut replies = on(std::slice::from_ref(&shown));
            let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
            assert_eq!(submitted_reply(work).as_deref(), Some("01K8A1"), "{what}");
            assert!(
                !replies.daemon_answered("01K8A1".into(), Err(refusal), NOW, wall(NOW)),
                "{what}: not typed in"
            );
            // Still pending, not reported, and that session left alone a while.
            assert_eq!(next_at(&mut replies, NOW + TYPING_HOLD / 2), None, "{what}");
            // Then tried again, as its view still takes input.
            let again = NOW + TYPING_HOLD;
            assert_eq!(
                submitted_reply(next_at(&mut replies, again)).as_deref(),
                Some("01K8A1"),
                "{what}"
            );
            assert!(
                replies.daemon_answered("01K8A1".into(), Ok(()), again, wall(again)),
                "{what}: typed in"
            );
            taken(&mut replies);
            let body = report_of(next_at(&mut replies, again));
            assert_eq!(
                (&body["id"], &body["status"]),
                (&"01K8A1".into(), &"delivered".into()),
                "{what}"
            );
        }
    }
}

#[test]
fn a_reply_the_daemon_refuses_for_good_fails_refused() {
    let mut replies = on(&[running("claude", Lifecycle::Done)]);
    let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    assert_eq!(submitted_reply(work).as_deref(), Some("01K8A1"));
    assert!(!replies.daemon_answered(
        "01K8A1".into(),
        Err(ErrorCode::PermissionDenied),
        NOW,
        wall(NOW)
    ));
    let body = report_of(next_at(&mut replies, NOW));
    assert_eq!(
        (&body["id"], &body["failure"]),
        (&"01K8A1".into(), &"refused".into())
    );
}

/// Session S's agent's question, call `toolu_1`, answered: Blue.
fn answer_message(id: &str, seq: u64, conversation: &str) -> SessionMessage {
    SessionMessage {
        text: "Which colour do you prefer? Blue".into(),
        answer: Some(QuestionReply {
            call_id: "toolu_1".into(),
            answers: vec![session_core::api::QuestionAnswer {
                question: "Which colour do you prefer?".into(),
                choices: vec!["Blue".into()],
                text: None,
            }],
        }),
        ..message(id, seq, Some(conversation))
    }
}

/// Replies for studio with a daemon that answers questions, `views` its sessions.
fn answering(views: &[SessionView]) -> ReplySync {
    let mut replies = on(views);
    replies.set_answers(true);
    replies
}

/// Read `pending` messages, then return what comes next.
fn after_reading_messages(
    replies: &mut ReplySync,
    pending: Vec<SessionMessage>,
) -> Option<ReplyWork> {
    let after = match replies.next_work(NOW) {
        Some(ReplyWork::Request(FeedRequest::Replies { after, answers, .. })) => {
            assert!(answers, "the read asks for answers");
            after
        }
        other => panic!("expected a read, got {other:?}"),
    };
    assert_eq!(after, 0);
    replies.request_done(true, read_messages(pending), NOW);
    replies.next_work(NOW)
}

fn given(work: Option<ReplyWork>) -> Option<String> {
    match work {
        Some(ReplyWork::Answer {
            reply,
            session,
            call_id,
            answers,
        }) => {
            assert_eq!(session, SessionId::from_string("S"));
            assert_eq!(call_id, "toolu_1");
            assert_eq!(answers[0].choices, ["Blue"]);
            Some(reply)
        }
        _ => None,
    }
}

#[test]
fn an_answer_goes_to_the_daemon_while_its_agent_asks_and_is_reported_delivered() {
    let mut replies = answering(&[view("S", Lifecycle::Blocked, Some(CHAT))]);
    let work = after_reading_messages(&mut replies, vec![answer_message("01K8A3", 6, CHAT)]);
    assert_eq!(given(work).as_deref(), Some("01K8A3"));
    // Given once: nothing more for it while the daemon works the dialog.
    assert!(given(replies.next_work(NOW)).is_none());
    replies.answer_done("01K8A3".into(), Ok(()), NOW);
    let body = report_of(next_at(&mut replies, NOW));
    assert_eq!(
        (&body["id"], &body["status"]),
        (&"01K8A3".into(), &"delivered".into())
    );
    assert!(body.get("failure").is_none());
}

#[test]
fn without_a_daemon_that_answers_no_answer_is_asked_for_or_given() {
    let mut replies = on(&[view("S", Lifecycle::Blocked, Some(CHAT))]);
    match replies.next_work(NOW) {
        Some(ReplyWork::Request(FeedRequest::Replies { answers, .. })) => assert!(!answers),
        other => panic!("expected a read, got {other:?}"),
    }
    // One read all the same (a cloud off the contract) stays pending, never given.
    replies.request_done(
        true,
        read_messages(vec![answer_message("01K8A3", 6, CHAT)]),
        NOW,
    );
    for at in [NOW, NOW + ANSWER_GONE_AFTER * 2] {
        let work = replies.next_work(at);
        assert!(
            !matches!(
                work,
                Some(ReplyWork::Answer { .. } | ReplyWork::Submit { .. })
                    | Some(ReplyWork::Request(FeedRequest::ReplyStatus { .. }))
            ),
            "{work:?}"
        );
        replies.request_done(true, read_messages(vec![]), at);
    }
}

#[test]
fn an_answer_waits_while_its_agent_asks_nothing_then_the_daemon_decides() {
    let mut replies = answering(&[view("S", Lifecycle::Working, Some(CHAT))]);
    let work = after_reading_messages(&mut replies, vec![answer_message("01K8A3", 6, CHAT)]);
    assert!(given(work).is_none(), "its agent asks nothing yet");
    // Still asking nothing a while on (answered in the terminal, say): the
    // daemon says whether the question is open, and types nothing if not.
    replies.request_done(true, read_messages(vec![]), NOW);
    assert_eq!(
        given(replies.next_work(NOW + ANSWER_GONE_AFTER)).as_deref(),
        Some("01K8A3")
    );
    replies.answer_done(
        "01K8A3".into(),
        Err(ErrorCode::QuestionGone),
        NOW + ANSWER_GONE_AFTER,
    );
    let body = report_of(next_at(&mut replies, NOW + ANSWER_GONE_AFTER));
    assert_eq!(
        (&body["id"], &body["failure"]),
        (&"01K8A3".into(), &"question_gone".into())
    );
}

#[test]
fn an_answer_fails_when_its_session_ended_is_private_or_shows_another_chat() {
    let mut ended = view("S", Lifecycle::Blocked, Some(CHAT));
    ended.status = SessionStatus::Exited;
    let mut private = view("S", Lifecycle::Blocked, Some(CHAT));
    private.private = true;
    for (seen, failure) in [
        (ended, "session_gone"),
        (private, "private"),
        (
            view("S", Lifecycle::Blocked, Some("another")),
            "conversation_changed",
        ),
        (view("S", Lifecycle::None, None), "conversation_changed"),
    ] {
        let mut replies = answering(&[seen]);
        let work = after_reading_messages(&mut replies, vec![answer_message("01K8A3", 6, CHAT)]);
        let body = report_of(work);
        assert_eq!(body["failure"], failure);
    }
}

#[test]
fn the_daemons_refusals_are_reported_or_tried_again() {
    for (code, failure) in [
        (ErrorCode::QuestionGone, Some("question_gone")),
        (ErrorCode::NotTaken, Some("not_taken")),
        (ErrorCode::SessionNotLive, Some("session_gone")),
        (ErrorCode::InvalidArgument, Some("refused")),
        (ErrorCode::SessionInUse, None),
    ] {
        let mut replies = answering(&[view("S", Lifecycle::Blocked, Some(CHAT))]);
        let work = after_reading_messages(&mut replies, vec![answer_message("01K8A3", 6, CHAT)]);
        assert_eq!(given(work).as_deref(), Some("01K8A3"));
        replies.answer_done("01K8A3".into(), Err(code), NOW);
        match failure {
            Some(failure) => {
                let body = report_of(next_at(&mut replies, NOW));
                assert_eq!(body["failure"], failure, "{code:?}");
            }
            // Someone typed there: given again once they paused.
            None => {
                assert!(given(next_at(&mut replies, NOW)).is_none());
                assert_eq!(
                    given(next_at(&mut replies, NOW + TYPING_HOLD)).as_deref(),
                    Some("01K8A3")
                );
            }
        }
    }
}

#[test]
fn answers_dont_wait_behind_replies_and_replies_wait_while_one_is_given() {
    let mut replies = answering(&[view("S", Lifecycle::Blocked, Some(CHAT))]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    // A reply written before the answer waits: its agent asks something.
    let mut page = read(vec![reply("01K8A1", 4, Some("studio"))]);
    if let Ok(FeedResult::Replies(read)) = &mut page {
        read.messages.push(answer_message("01K8A3", 6, CHAT));
        read.page_seq = 6;
        read.latest_seq = 6;
    }
    replies.request_done(true, page, NOW);
    assert_eq!(given(replies.next_work(NOW)).as_deref(), Some("01K8A3"));
    // Answered on screen, its agent waits again: the reply still waits for
    // the daemon to finish the answer.
    replies.observe(&view("S", Lifecycle::Idle, Some(CHAT)));
    assert!(submitted_reply(next_at(&mut replies, NOW)).is_none());
    replies.answer_done("01K8A3".into(), Ok(()), NOW);
    assert_eq!(
        submitted_reply(next_at(&mut replies, NOW)).as_deref(),
        Some("01K8A1")
    );
    let body = report_of(next_at(&mut replies, NOW));
    assert_eq!(
        (&body["id"], &body["status"]),
        (&"01K8A3".into(), &"delivered".into())
    );
}

#[test]
fn an_answer_handed_to_the_daemon_is_never_given_again_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    let views = [view("S", Lifecycle::Blocked, Some(CHAT))];
    {
        let mut replies = kept_at(&path, &views);
        replies.set_answers(true);
        let work = after_reading_messages(&mut replies, vec![answer_message("01K8A3", 6, CHAT)]);
        assert_eq!(given(work).as_deref(), Some("01K8A3"));
        // The service stops before the daemon answered.
    }
    let mut replies = kept_at(&path, &views);
    replies.set_answers(true);
    // Whether the dialog took it isn't known: reported not taken, never given again.
    let body = report_of(replies.next_work(NOW));
    assert_eq!(
        (&body["id"], &body["failure"]),
        (&"01K8A3".into(), &"not_taken".into())
    );
    replies.request_done(true, Ok(FeedResult::Reply(reply("01K8A3", 7, None))), NOW);
    let work = after_reading_messages(&mut replies, vec![answer_message("01K8A3", 6, CHAT)]);
    assert!(given(work).is_none());
}
