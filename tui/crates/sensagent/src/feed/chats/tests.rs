use super::*;
use crate::source::fake::live_view;

const NOW: Duration = Duration::from_secs(100);

fn view(id: &str, private: bool) -> SessionView {
    let mut view = live_view(&SessionId::from_string(id), "/home/me/rakumux", 1);
    view.private = private;
    view
}

fn text(text: &str) -> TranscriptPart {
    TranscriptPart::Text {
        text: text.into(),
        truncated: false,
    }
}

fn message(position: u64, parts: Vec<TranscriptPart>) -> TranscriptMessage {
    TranscriptMessage {
        id: format!("m{position}"),
        position,
        role: TranscriptRole::Assistant,
        at: Some(1_761_220_000_000),
        parts,
    }
}

fn page(messages: Vec<TranscriptMessage>, offset: u64, more: bool) -> TranscriptPage {
    TranscriptPage {
        agent: "claude-code".into(),
        conversation_id: "5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23".into(),
        messages,
        cursor: cursor(offset),
        more,
    }
}

fn cursor(offset: u64) -> TranscriptCursor {
    TranscriptCursor {
        conversation: "5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23".into(),
        offset,
        key: None,
    }
}

fn on(cursors: Option<PathBuf>) -> ChatSync {
    let mut chats = ChatSync::new("studio".into(), cursors);
    chats.set_enabled(true, NOW);
    chats.connected(&[view("S", false), view("P", true)]);
    chats
}

fn read_of(work: Option<ChatWork>) -> (SessionId, Option<TranscriptCursor>) {
    match work {
        Some(ChatWork::Read { session, after }) => (session, after),
        other => panic!("expected a read, got {other:?}"),
    }
}

fn upload_of(work: Option<ChatWork>) -> serde_json::Value {
    match work {
        Some(ChatWork::Upload(FeedRequest::Messages { body })) => {
            serde_json::from_slice(&body).unwrap()
        }
        other => panic!("expected an upload, got {other:?}"),
    }
}

#[test]
fn a_live_chat_is_read_uploaded_then_read_on_from_where_it_was() {
    let mut chats = on(None);
    // Only the non-private session, and nothing while one request is out.
    let (session, after) = read_of(chats.next_work(NOW));
    assert_eq!((session.as_str(), after), ("S", None));
    assert_eq!(chats.next_work(NOW), None);
    chats.read_done(
        session.clone(),
        Ok(page(vec![message(0, vec![text("Fixed it.")])], 120, false)),
        NOW,
    );
    let body = upload_of(chats.next_work(NOW));
    assert_eq!(body["version"], 1);
    assert_eq!(body["machine"], "studio");
    assert_eq!(body["chat"]["agent"], "claude-code");
    assert_eq!(body["chat"]["session"]["id"], "S");
    assert_eq!(body["chat"]["session"]["folder"], "rakumux");
    assert_eq!(body["messages"][0]["id"], "m0");
    assert_eq!(body["messages"][0]["role"], "assistant");
    assert_eq!(body["messages"][0]["parts"][0]["text"], "Fixed it.");
    chats.upload_done(true, Ok(()), NOW);
    // Read again once it changes, not sooner than every few seconds.
    assert_eq!(chats.next_work(NOW), None);
    chats.observe(&view("S", false));
    assert_eq!(chats.next_work(NOW), None);
    let (_, after) = read_of(chats.next_work(NOW + READ_EVERY));
    assert_eq!(after, Some(cursor(120)));
}

#[test]
fn while_rakumux_com_is_open_a_changing_chat_is_read_every_second() {
    let mut chats = on(None);
    let (session, _) = read_of(chats.next_work(NOW));
    chats.read_done(session, Ok(page(vec![], 120, false)), NOW);
    // At the usual pace it waits out READ_EVERY; once the site is open, a second at most.
    chats.observe(&view("S", false));
    assert_eq!(chats.next_work(NOW + READ_EVERY_WATCHED), None);
    chats.set_watched(true, NOW);
    let (session, _) = read_of(chats.next_work(NOW + READ_EVERY_WATCHED));
    chats.read_done(
        session,
        Ok(page(vec![], 120, false)),
        NOW + READ_EVERY_WATCHED,
    );
    chats.observe(&view("S", false));
    read_of(chats.next_work(NOW + 2 * READ_EVERY_WATCHED));
}

#[test]
fn nothing_is_read_while_the_feed_is_off_and_a_turned_off_feed_drops_what_wasnt_sent() {
    let mut chats = ChatSync::new("studio".into(), None);
    chats.connected(&[view("S", false)]);
    assert_eq!(chats.next_work(NOW), None);
    chats.set_enabled(true, NOW);
    let (session, _) = read_of(chats.next_work(NOW));
    chats.read_done(
        session,
        Ok(page(vec![message(0, vec![text("x")])], 10, false)),
        NOW,
    );
    chats.set_enabled(false, NOW);
    chats.set_enabled(true, NOW);
    // Read again from where it was uploaded (nowhere yet), not uploaded stale.
    let (_, after) = read_of(chats.next_work(NOW + READ_EVERY));
    assert_eq!(after, None);
}

#[test]
fn a_long_read_is_uploaded_in_pieces_and_its_place_kept_after_the_last() {
    let mut chats = on(None);
    let (session, _) = read_of(chats.next_work(NOW));
    let messages = (0..150).map(|n| message(n, vec![text("step")])).collect();
    chats.read_done(session.clone(), Ok(page(messages, 900, true)), NOW);
    assert_eq!(
        upload_of(chats.next_work(NOW))["messages"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
    chats.upload_done(true, Ok(()), NOW);
    assert_eq!(
        upload_of(chats.next_work(NOW))["messages"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
    chats.upload_done(true, Ok(()), NOW);
    // The daemon had more: read on at once.
    let (_, after) = read_of(chats.next_work(NOW));
    assert_eq!(after, Some(cursor(900)));
}

#[test]
fn failed_uploads_wait_and_refused_ones_are_passed_over() {
    let mut chats = on(None);
    let (session, _) = read_of(chats.next_work(NOW));
    chats.read_done(
        session.clone(),
        Ok(page(vec![message(0, vec![text("x")])], 10, false)),
        NOW,
    );
    upload_of(chats.next_work(NOW));
    chats.upload_done(true, Err(FeedFailure::Unavailable(None)), NOW);
    assert_eq!(chats.next_work(NOW), None);
    upload_of(chats.next_work(NOW + RETRY_MIN));
    chats.upload_done(true, Err(FeedFailure::Invalid), NOW + RETRY_MIN);
    chats.observe(&view("S", false));
    let (_, after) = read_of(chats.next_work(NOW + RETRY_MIN + READ_EVERY));
    assert_eq!(after, Some(cursor(10)));
}

#[test]
fn reads_the_daemon_refuses_say_whether_to_ask_again() {
    let mut chats = on(None);
    let (session, _) = read_of(chats.next_work(NOW));
    // No agent there: asked again only once the session changes.
    chats.read_done(session.clone(), Err(ErrorCode::NotFound), NOW);
    assert_eq!(chats.next_work(NOW + READ_EVERY), None);
    chats.observe(&view("S", false));
    let (session, _) = read_of(chats.next_work(NOW + READ_EVERY));
    // An older daemon: not asked again until it reconnects.
    chats.read_done(session, Err(ErrorCode::Unsupported), NOW);
    chats.observe(&view("S", false));
    assert_eq!(chats.next_work(NOW + 2 * READ_EVERY), None);
    chats.connected(&[view("S", false)]);
    read_of(chats.next_work(NOW + 2 * READ_EVERY));
}

#[test]
fn where_chats_are_up_to_is_kept_for_the_next_service() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("chats.json");
    let mut chats = on(Some(path.clone()));
    let (session, _) = read_of(chats.next_work(NOW));
    // Nothing worth uploading: its place is kept at once.
    chats.read_done(session, Ok(page(Vec::new(), 64, false)), NOW);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let mut next = on(Some(path));
    let (_, after) = read_of(next.next_work(NOW));
    assert_eq!(after, Some(cursor(64)));
}

#[test]
fn a_renamed_machine_uploads_its_chats_whole_under_its_new_name() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("chats.json");
    let mut chats = on(Some(path.clone()));
    let (session, _) = read_of(chats.next_work(NOW));
    chats.read_done(session, Ok(page(Vec::new(), 64, false)), NOW);
    // Another machine held "studio": the chat is read from its start, and
    // goes up under the new name.
    chats.rename("studio-6m8b".into(), NOW);
    let (session, after) = read_of(chats.next_work(NOW));
    assert_eq!(after, None);
    chats.read_done(
        session,
        Ok(page(vec![message(0, vec![text("Fixed it.")])], 120, false)),
        NOW,
    );
    assert_eq!(upload_of(chats.next_work(NOW))["machine"], "studio-6m8b");
    // Kept: a restart doesn't go back to where the old name was up to.
    chats.upload_done(true, Ok(()), NOW);
    let (_, after) = read_of(on(Some(path)).next_work(NOW));
    assert_eq!(after, Some(cursor(120)));
}

#[test]
fn messages_are_cleaned_before_they_leave_the_machine() {
    // Split so secret scanners don't take the fixture for a key.
    let key = concat!("sk-", "abcdefghijklmnopqrstuvwxyz");
    let cleaned = upload_message(message(
        7,
        vec![
            text("   "),
            TranscriptPart::ToolCall {
                call_id: Some("bad id".into()),
                name: "Bash".into(),
                input: format!("{{\"command\":\"export API_KEY={key}\"}}"),
                truncated: false,
            },
            TranscriptPart::ToolResult {
                call_id: Some("toolu_01".into()),
                output: "\u{1b}[31mred\u{1b}[0m and \u{202e}reversed".into(),
                error: true,
                truncated: false,
            },
            TranscriptPart::ToolResult {
                call_id: None,
                output: String::new(),
                error: false,
                truncated: false,
            },
            TranscriptPart::Unknown,
        ],
    ))
    .unwrap();
    assert_eq!(cleaned.parts.len(), 3, "{:?}", cleaned.parts);
    let TranscriptPart::ToolCall { call_id, input, .. } = &cleaned.parts[0] else {
        panic!("expected the call first");
    };
    assert_eq!(*call_id, None);
    assert!(!input.contains(key), "{input}");
    assert_eq!(
        cleaned.parts[1],
        TranscriptPart::ToolResult {
            call_id: Some("toolu_01".into()),
            output: "red and reversed".into(),
            error: true,
            truncated: false,
        }
    );
    // Nothing left, or no role: not uploaded.
    assert_eq!(upload_message(message(1, vec![text(" \u{200b} ")])), None);
    let mut unknown = message(1, vec![text("x")]);
    unknown.role = TranscriptRole::Unknown;
    assert_eq!(upload_message(unknown), None);
    // A message's text is bounded as a whole.
    let long = "y".repeat(MAX_PART_BYTES);
    let parts = (0..20).map(|_| text(&long)).collect();
    let bounded = upload_message(message(1, parts)).unwrap();
    let total: usize = bounded
        .parts
        .iter()
        .map(|part| match part {
            TranscriptPart::Text { text, .. } => text.len(),
            _ => 0,
        })
        .sum();
    assert!(total <= MESSAGE_TEXT);
}
