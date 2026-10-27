//! Keeps `openapi.yaml` in step with the code: every route, query parameter
//! and JSON field the API produces or accepts must be named in the document.

use rakumux_api::dto::*;
use session_core::model::{Creator, GitInfo, Lifecycle, SessionStatus};
use std::collections::BTreeMap;

const SPEC: &str = include_str!("../openapi.yaml");

fn keys(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                // Free-form maps (labels) have caller-chosen keys.
                if key != "labels" {
                    keys(child, out);
                }
                out.push(key.clone());
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
        _ => {}
    }
}

fn assert_documented(what: &str, value: impl serde::Serialize) {
    let mut names = Vec::new();
    keys(&serde_json::to_value(value).unwrap(), &mut names);
    for name in names {
        assert!(
            SPEC.contains(&format!(" {name}:")),
            "{what}.{name} is missing from openapi.yaml"
        );
    }
}

fn git() -> GitInfo {
    GitInfo {
        repo_root: "/r".into(),
        branch: Some("main".into()),
        detached_at: Some("abc1234".into()),
        worktree: Some("wt".into()),
        repo: Some("r".into()),
    }
}

#[test]
fn routes_and_parameters_are_documented() {
    for path in [
        "/v1/health:",
        "/v1/folders:",
        "/v1/projects:",
        "/v1/sessions:",
        "/v1/sessions/{id}:",
        "/v1/sessions/{id}/content:",
        "/v1/sessions/{id}/revive:",
        "/v1/sessions/{id}/kill:",
        "/v1/sessions/{id}/attach:",
        "/v1/events:",
    ] {
        assert!(SPEC.contains(path), "{path} missing");
    }
    for operation in [
        "operationId: deleteSession",
        "operationId: reviveSession",
        "operationId: killSession",
        "operationId: attachSession",
        "operationId: sessionEvents",
    ] {
        assert!(SPEC.contains(operation), "{operation} missing");
    }
    for parameter in [
        "folder",
        "project",
        "name",
        "q",
        "include_dead",
        "all",
        "source",
        "lines",
        "format",
        "role",
        "cols",
        "rows",
    ] {
        assert!(
            SPEC.contains(&format!("- name: {parameter}\n")),
            "query parameter {parameter} missing"
        );
    }
    for header in [
        "X-Rakumux-Session-Id",
        "X-Rakumux-Session-Token",
        "X-Rakumux-Agent",
        "X-Rakumux-Agent-Conversation",
    ] {
        assert!(
            SPEC.contains(&format!("name: {header}")),
            "{header} missing"
        );
    }
}

#[test]
fn every_json_field_is_documented() {
    let session = Session {
        id: "i".into(),
        name: "n".into(),
        folder: "/p".into(),
        project: "/p".into(),
        cwd: "/p".into(),
        status: SessionStatus::Live,
        exit_code: Some(0),
        lifecycle: Lifecycle::Idle,
        agent: Some(Agent {
            kind: "claude".into(),
            session_id: Some("c".into()),
        }),
        reported: Some(Reported {
            state: Some(Lifecycle::Done),
            text: Some("t".into()),
            summary: Some("s".into()),
            files: vec!["f".into()],
            at: "1970-01-01T00:00:00Z".into(),
        }),
        activity: Some(Activity {
            phase: session_core::activity::ActivityPhase::RunningTool,
            active_subagents: 2,
            updated_at: "1970-01-01T00:00:00Z".into(),
        }),
        created_by: Creator::Session {
            id: session_core::ids::SessionId::from_string("p"),
        },
        parent_id: Some("p".into()),
        depth: 1,
        children: vec!["c".into()],
        command: Some(vec!["claude".into()]),
        startup_commands: vec!["x".into()],
        resume_command: Some(vec!["claude".into()]),
        pinned: true,
        git: Some(git()),
        description: Some("d".into()),
        notes: Some("n".into()),
        labels: BTreeMap::from([("k".to_string(), "v".to_string())]),
        private: true,
        is_self: true,
        auto_named: true,
        created_at: "c".into(),
        last_active_at: "l".into(),
        terminal: Some(Terminal {
            pid: Some(1),
            foreground_process: Some("bash".into()),
            title: "t".into(),
            cols: 80,
            rows: 24,
            controlled: true,
            alternate_screen: true,
        }),
    };
    assert_documented(
        "Session",
        SessionEnvelope {
            session: session.clone(),
        },
    );
    assert_documented(
        "SessionList",
        SessionList {
            sessions: vec![session],
        },
    );
    assert_documented(
        "FolderList",
        FolderList::new(vec![Folder {
            path: "/p".into(),
            name: "p".into(),
            sessions: 1,
            live: 1,
            last_active_at: Some("l".into()),
            added_at: "a".into(),
            git: Some(git()),
        }]),
    );
    assert_documented(
        "Content",
        Content {
            session_id: "i".into(),
            source: ContentSource::Scrollback,
            format: ContentFormat::Ansi,
            text: "t".into(),
            rows: 1,
            cols: 1,
            cursor: Cursor { row: 0, col: 0 },
            truncated: true,
            untrusted: true,
        },
    );
    assert_documented(
        "Health",
        Health {
            status: "ok".into(),
            version: "v".into(),
            pid: 1,
            daemon: DaemonHealth {
                reachable: false,
                version: Some("v".into()),
                capabilities: vec!["projects".into()],
                error: Some("e".into()),
            },
        },
    );
    assert_documented(
        "CreateSession",
        CreateSessionBody {
            folder: Some("/p".into()),
            project: Some("/p".into()),
            name: Some("n".into()),
            cwd: Some("c".into()),
            command: Some(vec!["c".into()]),
            startup_commands: Some(vec!["s".into()]),
            cols: Some(1),
            rows: Some(1),
        },
    );
    assert_documented(
        "UpdateSession",
        UpdateSessionBody {
            name: Some("n".into()),
        },
    );
    assert_documented(
        "Error",
        rakumux_api::error::HttpError::invalid("m")
            .with_details(serde_json::json!(null))
            .envelope(),
    );
    assert_documented(
        "ReviveSession",
        ReviveSessionBody {
            cols: Some(1),
            rows: Some(1),
        },
    );
}

#[test]
fn every_stream_message_is_documented() {
    use session_core::api::{
        Attached, EventEnvelope, Role, SessionEvent, StreamCommand, StreamEvent,
    };
    use session_core::ids::SessionId;
    use session_core::model::{
        Creator, LaunchSpec, ReportResult, Reported as ModelReported, ScopeRoot, SessionView,
    };
    let view = SessionView {
        agent_output: None,
        user_prompt: None,
        agent_turn: None,
        places: vec![],
        id: SessionId::from_string("i"),
        name: "n".into(),
        scope: ScopeRoot::from_canonical("/p".into()),
        is_self: false,
        cwd: "/p".into(),
        status: SessionStatus::Live,
        exit_code: None,
        lifecycle: Lifecycle::Idle,
        agent: None,
        reported: None,
        activity: None,
        created_by: Creator::User,
        parent_id: None,
        depth: 0,
        children: vec![],
        launch: LaunchSpec::default(),
        resume: None,
        auto_named: false,
        description: None,
        notes: None,
        labels: BTreeMap::new(),
        private: false,
        created_at: 0,
        last_active_at: 0,
        terminal: None,
        pinned: false,
        git: None,
    };
    let reported = ModelReported {
        state: Some(Lifecycle::Done),
        text: Some("t".into()),
        result: Some(ReportResult {
            summary: "s".into(),
            files: vec!["f".into()],
        }),
        at: 0,
    };
    let events = [
        SessionEvent::Created {
            session: view.clone(),
        },
        SessionEvent::Updated {
            session: view.clone(),
        },
        SessionEvent::Removed,
        SessionEvent::Exited { code: Some(0) },
        SessionEvent::Lifecycle {
            lifecycle: Lifecycle::Working,
        },
        SessionEvent::Reported { reported },
        SessionEvent::Activity,
    ];
    assert_documented("EventMessage", EventMessage::Subscribed);
    for event in events {
        let kind = serde_json::to_value(&event).unwrap()["kind"].clone();
        let kind = kind.as_str().unwrap().to_owned();
        assert!(SPEC.contains(&kind), "event kind {kind} missing");
        let message = EventMessage::from_envelope(EventEnvelope {
            scope: ScopeRoot::from_canonical("/p".into()),
            session_id: SessionId::from_string("i"),
            at: 1,
            event,
        })
        .unwrap();
        assert_documented("EventMessage", message);
    }

    let attached = AttachMessage::attached(Attached {
        session: view,
        role: Role::Controller,
        cols: 1,
        rows: 1,
        checkpoint: "c".into(),
        offset: 0,
    });
    let stream_events = [
        StreamEvent::Resized {
            cols: 1,
            rows: 1,
            checkpoint: "c".into(),
            offset: 0,
        },
        StreamEvent::RoleChanged {
            role: Role::Observer,
        },
        StreamEvent::Title { title: "t".into() },
        StreamEvent::Bell,
        StreamEvent::Exited { code: Some(0) },
        StreamEvent::Lagged { offset: 0 },
    ];
    let messages = std::iter::once(attached).chain(
        stream_events
            .into_iter()
            .filter_map(AttachMessage::from_event),
    );
    for message in messages {
        let kind = serde_json::to_value(&message).unwrap()["type"].clone();
        let kind = kind.as_str().unwrap().to_owned();
        assert!(
            section("    AttachMessage:").contains(&kind),
            "{kind} missing"
        );
        assert_documented("AttachMessage", message);
    }

    for command in [
        AttachCommand::Resize { cols: 1, rows: 1 },
        AttachCommand::Promote { cols: 1, rows: 1 },
        AttachCommand::Demote,
        AttachCommand::Fit { cols: 1, rows: 1 },
    ] {
        let kind = serde_json::to_value(command).unwrap()["type"].clone();
        let kind = kind.as_str().unwrap().to_owned();
        assert!(
            section("    AttachCommand:").contains(&kind),
            "{kind} missing"
        );
        assert_documented("AttachCommand", command);
        let _: StreamCommand = command.into();
    }
}

/// The lines of the document from the line `start` up to the next line at
/// the same or a lower indentation.
fn section(start: &str) -> String {
    let indent = start.len() - start.trim_start().len();
    let mut lines = SPEC.lines().skip_while(|line| *line != start);
    let first = lines.next().unwrap_or_else(|| panic!("{start} missing"));
    let mut out = vec![first];
    out.extend(lines.take_while(|line| {
        line.trim().is_empty() || line.len() - line.trim_start().len() > indent
    }));
    out.join("\n")
}

#[test]
fn revive_answers_the_session_envelope() {
    let revive = section("  /v1/sessions/{id}/revive:");
    assert!(
        revive.contains(r##"schema: { $ref: "#/components/schemas/SessionEnvelope" }"##),
        "{revive}"
    );
    for route in ["  /v1/sessions/{id}/kill:", "  /v1/sessions/{id}:"] {
        assert!(
            section(route).contains(r#""204": { description:"#),
            "{route}"
        );
    }
}

#[test]
fn the_project_aliases_are_marked_deprecated() {
    assert!(section("  /v1/projects:").contains("deprecated: true"));
    assert!(!section("  /v1/folders:").contains("deprecated"));
    let sessions = section("  /v1/sessions:");
    let project = sessions
        .split("- name: ")
        .find(|parameter| parameter.starts_with("project\n"))
        .expect("the project parameter");
    assert!(project.contains("deprecated: true"), "{project}");
    for schema in ["    Session:", "    CreateSession:"] {
        let field: Vec<&str> = SPEC
            .lines()
            .skip_while(|line| *line != schema)
            .skip_while(|line| line.trim_start() != "project:")
            .take(4)
            .collect();
        assert!(
            field.contains(&"          deprecated: true"),
            "{schema} {field:?}"
        );
    }
    assert!(section("    FolderList:").contains("deprecated: true"));
}
