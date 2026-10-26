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
    Ok(FeedResult::Replies(RepliesRead {
        replies,
        messages: vec![],
        latest_seq,
        more: false,
        poll: None,
    }))
}

fn on(accept_remote: Option<AcceptRemote>, views: &[SessionView]) -> ReplySync {
    let mut replies = ReplySync::new("studio".into(), accept_remote);
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

/// Read `pending`, then return what comes next.
fn after_reading(replies: &mut ReplySync, pending: Vec<FeedReply>) -> Option<ReplyWork> {
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    replies.request_done(true, read(pending), NOW);
    replies.next_work(NOW)
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
        })
    );
    // Typed once, whatever is read meanwhile.
    assert_eq!(replies.next_work(NOW), None);
    replies.submitted("01K8A1".into(), Ok(()), 2_000);
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
    replies.submitted("01K8A1".into(), Ok(()), 2_000);
    // Its report goes first; the second waits for the agent to wait again.
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
    replies.submitted("01K8A1".into(), Ok(()), 2_000);
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
    replies.submitted("01K8A1".into(), Err(ErrorCode::SessionNotLive), 2_000);
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

    // Typed, then read again still pending before the report landed.
    let pending = reply("01K8A3", 6, Some("studio"));
    replies.written(&pending);
    assert!(matches!(
        replies.next_work(NOW),
        Some(ReplyWork::Submit { .. })
    ));
    replies.submitted("01K8A3".into(), Ok(()), 2_000);
    report_of(replies.next_work(NOW));
    replies.request_done(true, Err(FeedFailure::Unavailable(None)), NOW);
    let later = NOW + Duration::from_secs(60);
    replies.observe(&view("S", Lifecycle::Done, Some(CHAT)));
    // The report is retried, and a stale read doesn't type the reply again.
    report_of(replies.next_work(later));
    let mut stored = pending.clone();
    stored.status = ReplyStatus::Delivered;
    replies.request_done(true, Ok(FeedResult::Reply(stored)), later);
    assert_eq!(read_after(replies.next_work(later)), 5);
    replies.request_done(true, read(vec![pending]), later);
    assert_eq!(replies.next_work(later), None);
}

#[test]
fn reads_follow_the_cursor_and_back_off_after_failures() {
    let mut replies = on(None, &[]);
    assert_eq!(read_after(replies.next_work(NOW)), 0);
    assert_eq!(replies.next_work(NOW), None);
    replies.request_done(
        true,
        Ok(FeedResult::Replies(RepliesRead {
            replies: vec![],
            messages: vec![],
            latest_seq: 12,
            more: true,
            poll: None,
        })),
        NOW,
    );
    // More waits: at once.
    assert_eq!(read_after(replies.next_work(NOW)), 12);
    replies.request_done(true, read(vec![]), NOW);
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
fn reads_come_as_soon_as_rakumux_com_says() {
    let paced = |poll: Option<Duration>| {
        Ok(FeedResult::Replies(RepliesRead {
            replies: vec![],
            messages: vec![],
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
        })
    );
    replies.submitted("01K8A2".into(), Ok(()), 2_000);
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
    replies.submitted("01K8A5".into(), Ok(()), 2_000);
    assert_eq!(
        replies.answered(&answer(Some(CHAT), 3_000)),
        Some("01K8A5".into())
    );
}
