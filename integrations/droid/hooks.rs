//! Droid's command hooks (9 events, Claude Code's names), reduced to activity
//! metadata. A Task subagent is a Droid session of its own whose hooks carry
//! its own `session_id`: its `SessionStart` names its caller
//! (`calling_session_id`), so it's announced there, and the daemon then takes
//! its other hooks as its heartbeat. Before its session exists Droid sends
//! `session_id: "unknown"`, which names no conversation: nothing is reported.
use super::*;

/// What `rakumux session hook --agent droid` prints: Droid adds a hook's
/// stdout to the conversation as context unless it's one of its hook-control
/// objects (`{}` isn't one), and `suppressOutput` also keeps the hook's row
/// out of its chat.
pub(super) const STDOUT: &str = r#"{"suppressOutput":true}"#;

/// Tools whose call shows a question or a plan for the user to answer.
const ASKS: &[&str] = &[
    "AskUser",
    "ExitSpecMode",
    "ProposeWorkstream",
    "ProposeMission",
];

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let name = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let session = input
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if session.is_empty() || session == "unknown" {
        return Ok(None);
    }
    let event = match name {
        // Compaction moves to a new session with a summary inside a turn or as
        // `/compress`: it neither starts nor ends a turn.
        "SessionStart" if input["source"] == "compact" => return Ok(None),
        "SessionStart" => {
            if let Some(caller) = caller(input) {
                return Ok(Some(ActivityUpdate {
                    version: ACTIVITY_VERSION,
                    agent: "droid".into(),
                    conversation_id: caller.into(),
                    completion_id: None,
                    event: ActivityEvent::Subagent {
                        id: session.into(),
                        running: true,
                    },
                }));
            }
            phase(ActivityPhase::Idle)
        }
        "SessionEnd" => phase(ActivityPhase::Idle),
        "UserPromptSubmit" | "PostToolUse" => phase(ActivityPhase::Working),
        "PreToolUse"
            if input["tool_name"]
                .as_str()
                .is_some_and(|tool| ASKS.contains(&tool)) =>
        {
            phase(ActivityPhase::NeedsInput)
        }
        "PreToolUse" => phase(ActivityPhase::RunningTool),
        "Notification"
            if matches!(
                input["notification_type"].as_str(),
                Some("permission_prompt" | "elicitation_dialog")
            ) =>
        {
            phase(ActivityPhase::NeedsInput)
        }
        // Droid fires this only when the user stopped a turn (Esc), in place of
        // Stop: nothing waits for an answer and no reply completed.
        "Notification" if input["notification_type"] == "idle_prompt" => phase(ActivityPhase::Idle),
        "Stop" => {
            let mut done = update("droid", input, phase(ActivityPhase::Done))?;
            if let Some(done) = done.as_mut() {
                done.completion_id = input
                    .get("message_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                    .map(str::to_owned);
            }
            return Ok(done);
        }
        // SubagentStop names no subagent (its own hooks stop it); PreCompact
        // changes nothing.
        _ => return Ok(None),
    };
    update("droid", input, event)
}

/// The conversation that started this session as its subagent, if any.
fn caller(input: &Value) -> Option<&str> {
    input
        .get("calling_session_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && *id != "unknown")
}

/// Whether the hook comes from a subagent's session announcing itself: its
/// directory isn't the root's.
pub(super) fn is_child(input: &Value) -> bool {
    caller(input).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(input: Value) -> Option<ActivityEvent> {
        normalize(&input).unwrap().map(|update| update.event)
    }

    fn hook(name: &str) -> Value {
        json!({"hook_event_name": name, "session_id": "11111111-2222-4333-8444-555555555555",
            "transcript_path": "/home/me/.factory/sessions/-repo/11111111-2222-4333-8444-555555555555.jsonl",
            "cwd": "/repo", "permission_mode": "off"})
    }

    fn with(mut input: Value, key: &str, value: Value) -> Value {
        input[key] = value;
        input
    }

    #[test]
    fn droids_events_map_to_activity() {
        assert_eq!(
            event(hook("UserPromptSubmit")),
            Some(phase(ActivityPhase::Working))
        );
        assert_eq!(
            event(with(hook("PreToolUse"), "tool_name", json!("Execute"))),
            Some(phase(ActivityPhase::RunningTool))
        );
        for tool in ["AskUser", "ExitSpecMode", "ProposeWorkstream"] {
            assert_eq!(
                event(with(hook("PreToolUse"), "tool_name", json!(tool))),
                Some(phase(ActivityPhase::NeedsInput)),
                "{tool}"
            );
        }
        assert_eq!(
            event(hook("PostToolUse")),
            Some(phase(ActivityPhase::Working))
        );
        let notification =
            |kind: &str| event(with(hook("Notification"), "notification_type", json!(kind)));
        assert_eq!(
            notification("permission_prompt"),
            Some(phase(ActivityPhase::NeedsInput))
        );
        assert_eq!(
            notification("elicitation_dialog"),
            Some(phase(ActivityPhase::NeedsInput))
        );
        assert_eq!(
            notification("idle_prompt"),
            Some(phase(ActivityPhase::Idle)),
            "a stopped turn: Droid sends no Stop"
        );
        assert_eq!(notification("auth_success"), None);
        assert_eq!(
            event(with(hook("SessionStart"), "source", json!("startup"))),
            Some(phase(ActivityPhase::Idle))
        );
        assert_eq!(event(hook("SessionEnd")), Some(phase(ActivityPhase::Idle)));
        for name in ["SubagentStop", "PreCompact", "FutureEvent"] {
            assert_eq!(event(hook(name)), None, "{name}");
        }
    }

    #[test]
    fn stop_completes_the_reply_its_message_names() {
        let done = normalize(&with(hook("Stop"), "message_id", json!("msg-7")))
            .unwrap()
            .unwrap();
        assert_eq!(done.event, phase(ActivityPhase::Done));
        assert_eq!(done.completion_id.as_deref(), Some("msg-7"));
        let bare = normalize(&hook("Stop")).unwrap().unwrap();
        assert_eq!(bare.completion_id, None);
    }

    #[test]
    fn compaction_neither_starts_nor_ends_a_turn() {
        let compact = with(hook("SessionStart"), "source", json!("compact"));
        assert_eq!(event(compact.clone()), None);
        let child = with(compact, "calling_session_id", json!("parent"));
        assert_eq!(event(child), None);
    }

    #[test]
    fn a_subagents_session_is_announced_under_its_caller() {
        let start = with(
            with(hook("SessionStart"), "source", json!("startup")),
            "calling_session_id",
            json!("99999999-2222-4333-8444-555555555555"),
        );
        let update = normalize(&start).unwrap().unwrap();
        assert_eq!(
            update.conversation_id,
            "99999999-2222-4333-8444-555555555555"
        );
        assert_eq!(
            update.event,
            ActivityEvent::Subagent {
                id: "11111111-2222-4333-8444-555555555555".into(),
                running: true
            }
        );
        assert!(is_child(&start));
        assert!(!is_child(&hook("PreToolUse")));
    }

    #[test]
    fn a_session_droid_hasnt_made_yet_reports_nothing() {
        for name in ["SessionStart", "SessionEnd", "UserPromptSubmit", "Stop"] {
            let unknown = with(hook(name), "session_id", json!("unknown"));
            assert_eq!(normalize(&unknown).unwrap(), None, "{name}");
            let empty = with(hook(name), "session_id", json!(""));
            assert_eq!(normalize(&empty).unwrap(), None, "{name}");
        }
        // A subagent whose caller isn't known yet is the session's own.
        let start = with(hook("SessionStart"), "calling_session_id", json!("unknown"));
        assert_eq!(event(start), Some(phase(ActivityPhase::Idle)));
        // Nor does it name a working directory.
        let unknown = with(hook("UserPromptSubmit"), "session_id", json!("unknown"));
        assert_eq!(super::super::report("droid", &unknown).unwrap(), None);
    }

    #[test]
    fn the_root_sessions_directory_is_reported_never_a_subagents_or_a_prompt() {
        let prompt = with(hook("UserPromptSubmit"), "prompt", json!("secret prompt"));
        let report = super::super::report("droid", &prompt).unwrap().unwrap();
        let context = report.working_directory.as_ref().unwrap();
        assert_eq!(context.agent, "droid");
        assert_eq!(
            context.conversation_id.as_deref(),
            Some("11111111-2222-4333-8444-555555555555")
        );
        assert_eq!(context.path, std::path::Path::new("/repo"));
        assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
        let tool = with(
            with(hook("PreToolUse"), "tool_name", json!("Execute")),
            "tool_input",
            json!({"command": "secret command", "summary": "secret"}),
        );
        let report = super::super::report("droid", &tool).unwrap().unwrap();
        assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
        // A subagent announcing itself works elsewhere: not the root's directory.
        let child = with(
            with(hook("SessionStart"), "cwd", json!("/repo/child")),
            "calling_session_id",
            json!("parent"),
        );
        let report = super::super::report("droid", &child).unwrap().unwrap();
        assert!(report.working_directory.is_none());
        assert_eq!(report.activity.unwrap().conversation_id, "parent");
        // SessionEnd sends none, as for the others.
        let end = super::super::report("droid", &hook("SessionEnd"))
            .unwrap()
            .unwrap();
        assert!(end.working_directory.is_none());
    }

    #[test]
    fn its_configuration_is_droids_own_shape_and_its_answer_is_inert() {
        let config: Value = serde_json::from_str(super::super::config("droid").unwrap()).unwrap();
        let events = config.as_object().unwrap();
        assert!(
            !events.contains_key("hooks"),
            "keyed by event, as hooks.json is"
        );
        for event in [
            "SessionStart",
            "SessionEnd",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Notification",
            "Stop",
        ] {
            let command = &config[event][0]["hooks"][0];
            assert_eq!(command["type"], "command", "{event}");
            assert_eq!(
                command["command"], "\"${RAKUMUX_BIN:-rakumux}\" session hook --agent droid",
                "{event}"
            );
        }
        // Events that report nothing aren't run.
        assert!(!events.contains_key("SubagentStop") && !events.contains_key("PreCompact"));
        // One of Droid's hook-control keys, so it never becomes context.
        let answer: Value = serde_json::from_str(STDOUT).unwrap();
        assert_eq!(answer, json!({"suppressOutput": true}));
        assert_eq!(super::super::stdout("droid"), STDOUT);
        assert_eq!(super::super::stdout("claude"), "{}");
    }
}
