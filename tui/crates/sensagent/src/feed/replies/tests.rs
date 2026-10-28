use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use session_core::ids::ReplyId;
use session_core::model::AgentTurn;

use crate::source::fake::live_view;

const NOW: Duration = Duration::from_secs(100);
const CHAT: &str = "5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23";

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
    }))
}

fn on(accept_remote: Option<AcceptRemote>, views: &[SessionView]) -> ReplySync {
    let mut replies = ReplySync::new("studio".into(), accept_remote, None);
    replies.set_enabled(true, NOW);
    replies.connected(views);
    replies
}

fn read_after(work: Option<ReplyWork>) -> u64 {
    match work {
        Some(ReplyWork::Request(FeedRequest::Replies { after, machine })) => {
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

/// Read `pending`, then return what comes next. Those from "studio" were
/// written here: this machine made their ids.
fn after_reading(replies: &mut ReplySync, pending: Vec<FeedReply>) -> Option<ReplyWork> {
    for reply in pending
        .iter()
        .filter(|reply| reply.from.as_deref() == Some("studio"))
    {
        replies.wrote(&reply.id);
    }
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
fn a_busy_or_blocked_agent_is_waited_for() {
    for lifecycle in [Lifecycle::Working, Lifecycle::Blocked, Lifecycle::Unknown] {
        let mut replies = on(None, &[view("S", lifecycle, Some(CHAT))]);
        let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
        assert_eq!(work, None, "{lifecycle:?}");
        replies.observe(&view("S", Lifecycle::Idle, Some(CHAT)));
        assert!(matches!(
            replies.next_work(NOW),
            Some(ReplyWork::Submit { .. })
        ));
    }
    // A chat not known yet is waited for too.
    let mut replies = on(None, &[view("S", Lifecycle::Done, None)]);
    let work = after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    assert_eq!(work, None);
}

#[test]
fn one_reply_at_a_time_goes_into_a_session() {
    let mut replies = on(None, &[view("S", Lifecycle::Idle, Some(CHAT))]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Idle, Some(CHAT))]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Idle, Some(CHAT))]);
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
fn replies_written_elsewhere_need_this_machines_setting() {
    let views = [view("S", Lifecycle::Done, Some(CHAT))];
    for from in [None, Some("laptop")] {
        let mut replies = on(None, &views);
        let body = report_of(after_reading(&mut replies, vec![reply("01K8A1", 4, from)]));
        assert_eq!(body["status"], "failed");
        assert_eq!(body["failure"], "not_accepted");
    }

    let accept = Arc::new(AtomicBool::new(true));
    let setting = accept.clone();
    let mut replies = on(
        Some(Arc::new(move || setting.load(Ordering::SeqCst))),
        &views,
    );
    let work = after_reading(&mut replies, vec![reply("01K8A1", 4, None)]);
    assert!(matches!(work, Some(ReplyWork::Submit { .. })));

    // Turned off, it's read again within seconds.
    accept.store(false, Ordering::SeqCst);
    assert!(replies.accepts_remote(NOW + Duration::from_secs(1)));
    assert!(!replies.accepts_remote(NOW + SETTING_EVERY));
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
        let mut replies = on(None, &views);
        let body = report_of(after_reading(
            &mut replies,
            vec![reply("01K8A1", 4, Some("studio"))],
        ));
        assert_eq!(body["status"], "failed");
        assert_eq!(body["failure"], failure);
    }

    // The daemon refused the text.
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
    after_reading(&mut replies, vec![reply("01K8A1", 4, Some("studio"))]);
    replies.submitted("01K8A1".into(), Err(ErrorCode::SessionNotLive), NOW, 2_000);
    assert_eq!(report_of(replies.next_work(NOW))["failure"], "refused");
    assert_eq!(replies.answered(&answer(Some(CHAT), 3_000)), None);
}

#[test]
fn only_this_machines_pending_replies_are_typed_and_never_twice() {
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
    replies.wrote("01K8A3");
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
    let mut replies = on(None, &[]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    // rakumux.com asks to wait an hour (until its quota's window ends): reads
    // hold back a minute at most, as a refused read isn't counted and
    // pending replies expire within the hour.
    replies.request_done(true, Err(FeedFailure::RateLimited(Some(hour))), NOW);
    assert_eq!(replies.next_work(NOW + READ_EVERY), None);

    // A reply written here meanwhile waits for a read only so long, and,
    // typed in, is reported at once.
    let later = NOW + Duration::from_secs(30);
    replies.wrote("01K8A1");
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
    replies.wrote("01K8A2");
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    // The first page: replies delivered long ago. The account's newest, the
    // pending one, is on a later page.
    let mut old = reply("01K8A0", 100, Some("studio"));
    old.status = ReplyStatus::Delivered;
    replies.request_done(true, page_of(vec![old], 100, 250, true), NOW);
    assert_eq!(read_after(replies.next_work(NOW)), 100);
    replies.wrote("01K8A1");
    let pending = reply("01K8A1", 250, Some("studio"));
    replies.request_done(true, page_of(vec![pending], 250, 250, false), NOW);
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"
    ));
}

#[test]
fn a_page_whose_rows_were_all_left_out_still_moves_reads_on() {
    let mut replies = on(None, &[]);
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
        }))
    };
    let mut replies = on(None, &[]);
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
    }))
}

#[test]
fn a_session_message_waits_for_an_agent_then_goes_into_whatever_chat_it_shows() {
    let remote: AcceptRemote = Arc::new(|| true);
    // A shell: nothing is typed into it.
    let mut replies = on(Some(remote), &[view("S", Lifecycle::None, None)]);
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
    // This machine takes nothing written elsewhere, but its own TUI's message
    // goes in, once a read reaches it.
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
    let mut written = message("01K8A4", 9, Some(CHAT));
    written.from = Some("studio".into());
    replies.wrote("01K8A4");
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
    // Written on rakumux.com: not typed here unless this machine takes those.
    let mut replies = on(None, &[view("S", Lifecycle::Idle, None)]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A3", 8, None)]), NOW);
    let body = report_of(replies.next_work(NOW));
    assert_eq!(body["status"], "failed");
    assert_eq!(body["failure"], "not_accepted");

    // A session that ended, or a private one, can't take it.
    let remote: AcceptRemote = Arc::new(|| true);
    let mut private = view("S", Lifecycle::Idle, None);
    private.private = true;
    let mut replies = on(Some(remote), &[private]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A4", 9, None)]), NOW);
    assert_eq!(report_of(replies.next_work(NOW))["failure"], "private");

    // With no chat to name, the agent's next reply answers it, whichever chat.
    let remote: AcceptRemote = Arc::new(|| true);
    let mut replies = on(Some(remote), &[view("S", Lifecycle::Done, None)]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Working, Some(CHAT))]);
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
        let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
        let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
        replies.connected(views);
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT)), other]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
        let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
    // An agent whose turns the daemon doesn't follow takes a session message
    // once seen busy, as before.
    let remote: AcceptRemote = Arc::new(|| true);
    let mut unfollowed = view("S", Lifecycle::Idle, None);
    unfollowed.agent_turn = None;
    let mut replies = on(Some(remote), &[unfollowed.clone()]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read_messages(vec![message("01K8A2", 6, None)]), NOW);
    assert_eq!(
        submitted_reply(replies.next_work(NOW)).as_deref(),
        Some("01K8A2")
    );
    replies.submitted("01K8A2".into(), Ok(()), NOW, 2_000);
    unfollowed.lifecycle = Lifecycle::Working;
    replies.observe(&unfollowed);
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
        replies.connected(&views);
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
    replies.connected(&[]);
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
    let remote: AcceptRemote = Arc::new(|| true);
    // Its chat isn't known yet: the reply to it waits, and the session
    // message written after it waits behind it.
    let mut replies = on(Some(remote), &[view("S", Lifecycle::Done, None)]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let read = Ok(FeedResult::Replies(RepliesRead {
        replies: vec![reply("01K8A9", 4, None)],
        messages: vec![message("01K8A1", 5, None)],
        page_seq: 5,
        latest_seq: 5,
        more: false,
        poll: None,
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
    let remote: AcceptRemote = Arc::new(|| true);
    let mut replies = on(Some(remote), &[view("S", Lifecycle::Done, Some(CHAT))]);
    // A read goes out; meanwhile a reply is written on rakumux.com, then one
    // in this machine's TUI, and the read comes back without either.
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let earlier = reply("01K8A1", 7, None);
    let mine = reply("01K8A2", 8, Some("studio"));
    replies.wrote("01K8A2");
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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

#[test]
fn a_reply_naming_this_machine_as_its_writer_counts_as_written_here_only_with_an_id_it_made() {
    // Another machine of the account (or a token taken from one) says the
    // reply was written on studio: without the setting, it isn't typed.
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read(vec![reply("01K8A1", 4, Some("studio"))]), NOW);
    let body = report_of(replies.next_work(NOW));
    assert_eq!(body["status"], "failed");
    assert_eq!(body["failure"], "not_accepted");
}

/// Replies for studio keeping what's typed in in `path`, with session S.
fn kept_at(path: &std::path::Path, views: &[SessionView]) -> ReplySync {
    let mut replies = ReplySync::new("studio".into(), None, Some(path.to_path_buf()));
    replies.set_enabled(true, NOW);
    replies.connected(views);
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
    // It's reported delivered…
    let body = report_of(replies.next_work(NOW));
    assert_eq!(body["id"], "01K8A1");
    assert_eq!(body["status"], "delivered");
    let mut delivered = pending.clone();
    delivered.status = ReplyStatus::Delivered;
    replies.request_done(true, Ok(FeedResult::Reply(delivered)), NOW);
    // …and a read from the start that still says pending types nothing.
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
fn replies_this_machine_wrote_are_still_its_own_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replies.json");
    kept_at(&path, &[]).wrote("01K8A1");
    let mut replies = kept_at(&path, &[view("S", Lifecycle::Done, Some(CHAT))]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read(vec![reply("01K8A1", 4, Some("studio"))]), NOW);
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { reply, .. }) if reply == "01K8A1"
    ));
}

#[test]
fn a_renamed_machine_reads_its_replies_from_the_start_under_its_new_name() {
    let mut replies = on(None, &[]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    let mut old = reply("01K8A1", 4, None);
    old.status = ReplyStatus::Delivered;
    replies.request_done(true, read(vec![old]), NOW);
    replies.rename("studio-6m8b".into(), NOW);
    match replies.next_work(NOW) {
        Some(ReplyWork::Request(FeedRequest::Replies { after, machine })) => {
            assert_eq!((after, machine.as_str()), (0, "studio-6m8b"));
        }
        other => panic!("expected a read, got {other:?}"),
    }
}

#[test]
fn a_reply_is_typed_only_while_the_program_seen_is_in_its_sessions_foreground() {
    let mut replies = on(None, &[view("S", Lifecycle::Done, Some(CHAT))]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Idle, Some(CHAT))]);
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
    let mut replies = on(None, &[view("S", Lifecycle::Idle, Some(CHAT))]);
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
    let mut replies = on(None, &[]);
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
    let mut replies = on(None, &[]);
    replies.set_pushed(true, NOW);
    read_with_poll(&mut replies, WATCHED_POLL, NOW);
    assert!(replies.watched());
    assert_eq!(read_after(replies.next_work(NOW + WATCHED_POLL)), 0);
}

#[test]
fn a_poke_during_a_read_reads_again_once_it_is_back() {
    let mut replies = on(None, &[]);
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
    let mut replies = on(None, &[]);
    replies.set_pushed(true, NOW);
    read_with_poll(&mut replies, Duration::from_secs(15), NOW);
    replies.set_pushed(false, NOW + Duration::from_secs(1));
    assert_eq!(replies.next_work(NOW + Duration::from_secs(15)), None);
    assert_eq!(
        read_after(replies.next_work(NOW + Duration::from_secs(16))),
        0
    );
}
