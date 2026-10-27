use super::*;
use session_core::activity::AgentActivity;
use session_core::model::{AgentInfo, AgentTurn, GitInfo};

use crate::source::fake::live_view;

const NOW: Duration = Duration::from_secs(100);
const WALL: i64 = 1_761_220_000_000;
const CHAT: &str = "5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23";

fn view(id: &str) -> SessionView {
    let mut view = live_view(&SessionId::from_string(id), "/home/me/rakumux", 1);
    view.name = format!("session {id}");
    view.last_active_at = 1_761_219_990;
    view
}

fn agent(mut view: SessionView) -> SessionView {
    view.agent = Some(AgentInfo {
        kind: "claude".into(),
        agent_session_id: None,
        agent_session_path: None,
    });
    view.agent_turn = Some(AgentTurn {
        id: 1,
        messages: 2,
        conversation_id: Some(CHAT.into()),
    });
    view.git = Some(GitInfo {
        branch: Some("web-session".into()),
        ..Default::default()
    });
    view
}

fn on(views: &[SessionView]) -> SessionSync {
    let mut sessions = SessionSync::new("studio".into(), true);
    sessions.set_enabled(true, NOW);
    sessions.connected(views);
    sessions
}

fn body(request: Option<FeedRequest>) -> serde_json::Value {
    match request {
        Some(FeedRequest::Sessions { body }) => serde_json::from_slice(&body).unwrap(),
        other => panic!("expected a session list, got {other:?}"),
    }
}

#[test]
fn the_list_says_what_the_tuis_status_and_folders_tabs_group_by() {
    let mut working = agent(view("01K89ZQ4D2F6H8J0K2M4N6P8R0"));
    working.pinned = true;
    working.activity = Some(AgentActivity {
        phase: ActivityPhase::WaitingForResponse,
        active_subagents: 0,
        updated_at: 1,
    });
    let mut private = view("01K89ZQ4D2F6H8J0K2M4N6P8R1");
    private.private = true;
    let mut dead = view("01K89ZQ4D2F6H8J0K2M4N6P8R2");
    dead.status = SessionStatus::Dead;
    let mut exited = view("01K89ZQ4D2F6H8J0K2M4N6P8R3");
    exited.status = SessionStatus::Exited;
    exited.lifecycle = Lifecycle::None;
    exited.last_active_at = 1_761_210_000;
    let mut sessions = on(&[working, private, dead, exited]);
    let upload = body(sessions.next_request(NOW, WALL));
    assert_eq!(upload["machine"], "studio");
    assert_eq!(upload["at"], WALL);
    let listed = upload["sessions"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "private and dead sessions aren't listed");
    assert_eq!(
        listed[0],
        serde_json::json!({
            "id": "01K89ZQ4D2F6H8J0K2M4N6P8R0", "name": "session 01K89ZQ4D2F6H8J0K2M4N6P8R0",
            "folder": "rakumux", "branch": "web-session", "agent": "claude-code",
            "status": "live", "lifecycle": "working", "phase": "waiting_for_response",
            "conversation_id": CHAT, "pinned": true, "last_active_at": 1_761_219_990_000_i64
        })
    );
    assert_eq!(listed[1]["status"], "exited");
    assert_eq!(listed[1]["lifecycle"], "none");
    assert!(listed[1].get("agent").is_none());
}

#[test]
fn a_changed_list_goes_once_settled_and_an_unchanged_one_every_minute() {
    let mut sessions = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0")]);
    assert!(sessions.next_request(NOW, WALL).is_some());
    assert!(sessions.next_request(NOW, WALL).is_none(), "one at a time");
    sessions.done(true, Ok(1), NOW);
    assert!(sessions
        .next_request(NOW + Duration::from_secs(1), WALL)
        .is_none());

    // A change waits for the list to settle.
    let mut done = view("01K89ZQ4D2F6H8J0K2M4N6P8R0");
    done.lifecycle = Lifecycle::Done;
    sessions.observe(&done);
    assert!(sessions
        .next_request(NOW + Duration::from_secs(2), WALL)
        .is_none());
    let changed = body(sessions.next_request(NOW + SETTLE, WALL));
    assert_eq!(changed["sessions"][0]["lifecycle"], "done");
    sessions.done(true, Ok(1), NOW + SETTLE);

    // Unchanged: nothing until the heartbeat.
    let later = NOW + SETTLE;
    assert!(sessions
        .next_request(later + Duration::from_secs(30), WALL)
        .is_none());
    assert!(sessions.next_request(later + HEARTBEAT, WALL).is_some());
    sessions.done(true, Ok(1), later + HEARTBEAT);

    // A session that goes is left out of the next list.
    sessions.forget(&SessionId::from_string("01K89ZQ4D2F6H8J0K2M4N6P8R0"));
    let gone = body(sessions.next_request(later + HEARTBEAT + SETTLE, WALL));
    assert_eq!(gone["sessions"], serde_json::json!([]));
}

#[test]
fn failures_back_off_and_the_switch_turning_on_again_sends_at_once() {
    let mut sessions = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0")]);
    assert!(sessions.next_request(NOW, WALL).is_some());
    sessions.done(true, Err(FeedFailure::Unavailable(None)), NOW);
    assert!(sessions
        .next_request(NOW + Duration::from_secs(4), WALL)
        .is_none());
    assert!(sessions.next_request(NOW + RETRY_MIN, WALL).is_some());
    // An answer for an account signed out meanwhile changes nothing.
    sessions.done(false, Ok(1), NOW + RETRY_MIN);
    sessions.set_enabled(false, NOW + RETRY_MIN);
    assert!(sessions.next_request(NOW + HEARTBEAT * 2, WALL).is_none());
    sessions.set_enabled(true, NOW + HEARTBEAT * 2);
    assert!(sessions.next_request(NOW + HEARTBEAT * 2, WALL).is_some());
}

#[test]
fn rakumux_com_says_how_many_it_holds() {
    let reply = |status: u16, body: &str| crate::cloud::FeedHttpReply {
        status,
        retry_after: None,
        body: body.as_bytes().to_vec(),
    };
    assert_eq!(
        wire::sessions_stored(&reply(200, r#"{"version":1,"stored":2}"#)),
        Ok(2)
    );
    assert_eq!(
        wire::sessions_stored(&reply(200, r#"{"version":1,"stored":201}"#)),
        Err(FeedFailure::Invalid)
    );
    assert_eq!(
        wire::sessions_stored(&reply(
            403,
            r#"{"error":{"code":"forbidden","message":"x"}}"#
        )),
        Err(FeedFailure::Off)
    );
}

#[test]
fn sessions_without_a_named_agent_say_the_program_the_app_tab_files_them_under() {
    let running = |id: &str, program: &str| {
        let mut view = view(id);
        if let Some(terminal) = view.terminal.as_mut() {
            terminal.foreground_process = Some(program.into());
        }
        view
    };
    let node = running("01K89ZQ4D2F6H8J0K2M4N6P8R0", "node");
    let shell = running("01K89ZQ4D2F6H8J0K2M4N6P8R1", "zsh");
    let mut launched = running("01K89ZQ4D2F6H8J0K2M4N6P8R2", "node");
    launched.status = SessionStatus::Exited;
    launched.launch.argv = Some(vec!["/usr/local/bin/HTOP".into(), "-d".into(), "10".into()]);
    let mut other_agent = running("01K89ZQ4D2F6H8J0K2M4N6P8R3", "node");
    other_agent.agent = Some(AgentInfo {
        kind: "pi".into(),
        agent_session_id: None,
        agent_session_path: None,
    });
    let named = agent(running("01K89ZQ4D2F6H8J0K2M4N6P8R4", "node"));
    let mut sessions = on(&[node, shell, launched, other_agent, named]);
    let upload = body(sessions.next_request(NOW, WALL));
    let program = |id: &str| {
        upload["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["id"] == id)
            .unwrap()
            .get("program")
            .cloned()
    };
    // Its foreground program while it runs; a shell is no program.
    assert_eq!(program("01K89ZQ4D2F6H8J0K2M4N6P8R0"), Some("node".into()));
    assert_eq!(program("01K89ZQ4D2F6H8J0K2M4N6P8R1"), None);
    // Ended: the program it started with, its name only, in lowercase.
    assert_eq!(program("01K89ZQ4D2F6H8J0K2M4N6P8R2"), Some("htop".into()));
    // An agent the contract doesn't name is its program; a named one is the agent.
    assert_eq!(program("01K89ZQ4D2F6H8J0K2M4N6P8R3"), Some("pi".into()));
    assert_eq!(program("01K89ZQ4D2F6H8J0K2M4N6P8R4"), None);
}

#[test]
fn the_list_says_this_machine_takes_replies_with_files_when_it_saves_them() {
    let mut sessions = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0")]);
    let upload = body(sessions.next_request(NOW, WALL));
    assert_eq!(
        upload["capabilities"],
        serde_json::json!(["reply_attachments"])
    );

    let mut sessions = SessionSync::new("studio".into(), false);
    sessions.set_enabled(true, NOW);
    let upload = body(sessions.next_request(NOW, WALL));
    assert!(upload.get("capabilities").is_none(), "{upload}");
}
