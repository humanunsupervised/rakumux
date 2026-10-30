//! GitHub Copilot CLI's command hooks, reduced to activity metadata.
//!
//! Rakumux's own hook file (`hooks.json`, installed as
//! `~/.copilot/hooks/rakumux.json`) uses Copilot's camelCase events, whose
//! input doesn't name its event: each command passes it with `--event`
//! (`hookEventName`). Copilot also runs a repository's Claude Code hooks
//! (`.claude/settings.json`), with snake_case input that names its event
//! (`hook_event_name`) and Claude's tool names; `hooks::host` hands those
//! here too. A subagent's hooks carry its `agentId`, and its own events its
//! own `sessionId` under the root's `parentAgentId`.
use super::*;

/// `input`'s first string among `keys` (camelCase, then snake_case).
fn field<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
}

/// The event's name, lowercase: `agentstop` (`--event agentStop`), `stop`
/// (Claude's format), `permissionrequest` (`hookName`).
fn event(input: &Value) -> String {
    field(input, &[EVENT, "hook_event_name", "hookName"])
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// The subagent a hook is about (`subagentStart`/`Stop`) or fired in.
fn subagent_id(input: &Value) -> Option<&str> {
    field(input, &["agentId", "agent_id"])
}

/// The root conversation: a subagent's `parentAgentId`, else the hook's own
/// `sessionId` (a subagent's own events carry the subagent's there).
fn root(input: &Value) -> Option<&str> {
    let agent = subagent_id(input);
    field(input, &["parentAgentId", "parent_agent_id"]).or_else(|| {
        field(input, &["sessionId", "session_id"]).filter(|session| Some(*session) != agent)
    })
}

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let name = event(input);
    let tool = field(input, &["toolName", "tool_name"]);
    let event = match (name.as_str(), subagent_id(input)) {
        ("subagentstart", Some(id)) => ActivityEvent::Subagent {
            id: id.into(),
            running: true,
        },
        ("subagentstop", Some(id)) => ActivityEvent::Subagent {
            id: id.into(),
            running: false,
        },
        // Inside a subagent: its tool hooks show it still runs; its prompt and
        // its stop are its own, never the parent's phase.
        ("pretooluse" | "posttooluse" | "posttoolusefailure", Some(id)) => {
            ActivityEvent::Subagent {
                id: id.into(),
                running: true,
            }
        }
        (_, Some(_)) => return Ok(None),
        ("sessionstart" | "sessionend", None) => phase(ActivityPhase::Idle),
        ("userpromptsubmitted" | "userpromptsubmit", None) => phase(ActivityPhase::Working),
        ("pretooluse", None) if matches!(tool, Some("ask_user" | "AskUserQuestion")) => {
            phase(ActivityPhase::NeedsInput)
        }
        ("pretooluse", None) => phase(ActivityPhase::RunningTool),
        ("posttooluse" | "posttoolusefailure", None) => phase(ActivityPhase::Working),
        // What waits for the user. `permissionRequest` fires before Copilot's
        // rules decide, for calls they then allow too, so it's not one.
        ("notification", None)
            if matches!(
                field(input, &["notification_type", "notificationType"]),
                Some("permission_prompt" | "elicitation_dialog")
            ) =>
        {
            phase(ActivityPhase::NeedsInput)
        }
        ("agentstop" | "stop", None) => phase(ActivityPhase::Done),
        // A model error ends the turn without `agentStop`; one Copilot retries
        // doesn't.
        ("erroroccurred", None)
            if field(input, &["errorContext", "error_context"]) == Some("model_call")
                && input["recoverable"] != Value::Bool(true) =>
        {
            phase(ActivityPhase::Idle)
        }
        ("stopfailure", None) => phase(ActivityPhase::Idle),
        _ => return Ok(None),
    };
    let conversation = root(input).ok_or("hook is missing sessionId")?;
    Ok(Some(ActivityUpdate {
        version: ACTIVITY_VERSION,
        agent: "copilot".into(),
        conversation_id: conversation.into(),
        completion_id: None,
        event,
    }))
}

/// Activity, and the root conversation's directory (`cwd`, which follows
/// `/cd`), never a subagent's or an ending session's.
pub(super) fn report(input: &Value) -> Result<Option<Report>, String> {
    let Some(activity) = normalize(input)? else {
        return Ok(None);
    };
    let name = event(input);
    let working_directory = (subagent_id(input).is_none() && name != "sessionend")
        .then(|| field(input, &["cwd"]))
        .flatten()
        .map(|path| AgentWorkingDirectory {
            agent: "copilot".into(),
            conversation_id: Some(activity.conversation_id.clone()),
            path: path.into(),
        });
    Ok(Some(Report {
        activity: Some(activity),
        working_directory,
        ..Report::default()
    }))
}

/// Where `hook-config --install` writes Rakumux's own hook file:
/// `$COPILOT_HOME/hooks/rakumux.json`, else `~/.copilot/hooks/rakumux.json`.
/// Copilot loads every file there beside the user's own.
pub(super) fn install_path(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    let home = match set("COPILOT_HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(set("HOME")?).join(".copilot"),
    };
    Some(home.join("hooks").join("rakumux.json"))
}

#[cfg(test)]
mod tests {
    use super::super::{config, receive, report as dispatch_report};
    use super::*;
    use serde_json::json;

    const ROOT: &str = "0471e2d2-e37e-4a26-ab61-7af3f61b8611";
    const CHILD: &str = "a8068685-6552-4d55-bfb7-7013dee37803";

    fn no_vars(_: &str) -> Option<String> {
        None
    }

    /// What `rakumux session hook --agent copilot --event <event>` reports.
    fn hook(event: &str, input: Value) -> Option<Report> {
        receive("copilot", Some(event), input, no_vars).unwrap()
    }

    fn event_of(event: &str, input: Value) -> Option<ActivityEvent> {
        hook(event, input).and_then(|report| report.activity.map(|update| update.event))
    }

    fn phase_of(event: &str, extra: Value) -> Option<ActivityEvent> {
        let mut input =
            json!({"sessionId": ROOT, "timestamp": 1793338028061u64, "cwd": "/home/me/project"});
        for (key, value) in extra.as_object().unwrap() {
            input[key] = value.clone();
        }
        event_of(event, input)
    }

    #[test]
    fn each_event_is_a_phase_of_the_root_conversation() {
        for (event, extra, expected) in [
            (
                "sessionStart",
                json!({"source": "new", "initialPrompt": "secret"}),
                Some(ActivityPhase::Idle),
            ),
            (
                "sessionEnd",
                json!({"reason": "user_exit"}),
                Some(ActivityPhase::Idle),
            ),
            (
                "userPromptSubmitted",
                json!({"prompt": "secret prompt"}),
                Some(ActivityPhase::Working),
            ),
            (
                "preToolUse",
                json!({"toolName": "bash", "toolArgs": "{'command': 'secret'}"}),
                Some(ActivityPhase::RunningTool),
            ),
            (
                "preToolUse",
                json!({"toolName": "ask_user", "toolArgs": "{}"}),
                Some(ActivityPhase::NeedsInput),
            ),
            (
                "postToolUse",
                json!({"toolName": "bash", "toolResult": "secret output"}),
                Some(ActivityPhase::Working),
            ),
            (
                "postToolUseFailure",
                json!({"toolName": "create", "error": "secret"}),
                Some(ActivityPhase::Working),
            ),
            (
                "notification",
                json!({"hook_event_name": "Notification", "notification_type": "permission_prompt", "message": "Run command: secret"}),
                Some(ActivityPhase::NeedsInput),
            ),
            (
                "notification",
                json!({"hook_event_name": "Notification", "notification_type": "elicitation_dialog"}),
                Some(ActivityPhase::NeedsInput),
            ),
            (
                "notification",
                json!({"hook_event_name": "Notification", "notification_type": "agent_completed"}),
                None,
            ),
            (
                "agentStop",
                json!({"transcriptPath": "/secret", "stopReason": "end_turn"}),
                Some(ActivityPhase::Done),
            ),
            (
                "errorOccurred",
                json!({"errorContext": "model_call", "recoverable": false, "error": "secret"}),
                Some(ActivityPhase::Idle),
            ),
            (
                "errorOccurred",
                json!({"errorContext": "model_call", "recoverable": true}),
                None,
            ),
            (
                "errorOccurred",
                json!({"errorContext": "tool_execution"}),
                None,
            ),
            // Fires before Copilot's rules decide, for calls they then allow.
            (
                "permissionRequest",
                json!({"hookName": "permissionRequest", "toolName": "bash"}),
                None,
            ),
            ("preCompact", json!({}), None),
            ("someFutureEvent", json!({}), None),
        ] {
            let got = phase_of(event, extra.clone());
            assert_eq!(got, expected.map(phase), "{event} {extra}");
        }
        let report = hook(
            "userPromptSubmitted",
            json!({"sessionId": ROOT, "cwd": "/home/me/project", "prompt": "secret prompt"}),
        )
        .unwrap();
        let update = report.activity.as_ref().unwrap();
        assert_eq!(
            (update.agent.as_str(), update.conversation_id.as_str()),
            ("copilot", ROOT)
        );
        assert_eq!(update.completion_id, None);
        assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
    }

    #[test]
    fn subagents_are_counted_by_their_id_and_their_own_hooks_keep_them_counted() {
        // The root's hooks about it.
        for (event, running) in [("subagentStart", true), ("subagentStop", false)] {
            let update = hook(
                event,
                json!({"sessionId": ROOT, "agentId": CHILD, "agentType": "explore",
                "parentAgentId": ROOT, "cwd": "/home/me/project"}),
            )
            .unwrap();
            let activity = update.activity.unwrap();
            assert_eq!(activity.conversation_id, ROOT);
            assert_eq!(
                activity.event,
                ActivityEvent::Subagent {
                    id: CHILD.into(),
                    running
                }
            );
            assert!(update.working_directory.is_none());
        }
        // Its own: tool hooks are its heartbeat, under the root.
        let child = |event: &str| {
            hook(
                event,
                json!({"sessionId": CHILD, "agentId": CHILD, "parentAgentId": ROOT,
                "cwd": "/home/me/project/sub", "toolName": "view", "prompt": "secret"}),
            )
        };
        for event in ["preToolUse", "postToolUse", "postToolUseFailure"] {
            let report = child(event).unwrap();
            let activity = report.activity.unwrap();
            assert_eq!(activity.conversation_id, ROOT, "{event}");
            assert_eq!(
                activity.event,
                ActivityEvent::Subagent {
                    id: CHILD.into(),
                    running: true
                }
            );
            assert!(report.working_directory.is_none());
        }
        // Its prompt and its stop are its own, never the parent's phase.
        for event in [
            "userPromptSubmitted",
            "agentStop",
            "notification",
            "sessionStart",
        ] {
            assert!(child(event).is_none(), "{event}");
        }
    }

    #[test]
    fn the_root_hooks_directory_is_where_copilot_works() {
        let report = hook(
            "preToolUse",
            json!({"sessionId": ROOT, "cwd": "/home/me/project/worktrees/fix",
            "toolName": "bash", "toolArgs": "{'command': 'cd /elsewhere'}"}),
        )
        .unwrap();
        let context = report.working_directory.unwrap();
        assert_eq!(context.agent, "copilot");
        assert_eq!(context.conversation_id.as_deref(), Some(ROOT));
        assert_eq!(
            context.path,
            std::path::Path::new("/home/me/project/worktrees/fix")
        );
        // Not from a session that's ending.
        let ended = hook(
            "sessionEnd",
            json!({"sessionId": ROOT, "cwd": "/home/me/project", "reason": "user_exit"}),
        )
        .unwrap();
        assert!(ended.working_directory.is_none());
        // Nothing reported without a session.
        assert!(receive("copilot", Some("agentStop"), json!({"cwd": "/x"}), no_vars).is_err());
        // The adapter reached through the shared report.
        assert!(dispatch_report(
            "copilot",
            &json!({"hookEventName": "agentStop", "sessionId": ROOT})
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn claude_code_hooks_copilot_runs_report_as_copilot() {
        // A repository's `.claude/settings.json` with Rakumux's Claude hooks.
        let copilot = |name: &str| (name == "COPILOT_CLI").then(|| "1".to_string());
        let stop = receive(
            "claude",
            None,
            json!({"hook_event_name": "Stop", "session_id": ROOT,
            "cwd": "/home/me/project", "stop_hook_active": false}),
            copilot,
        )
        .unwrap()
        .unwrap();
        let activity = stop.activity.unwrap();
        assert_eq!(
            (activity.agent.as_str(), activity.event.clone()),
            ("copilot", phase(ActivityPhase::Done))
        );
        assert_eq!(stop.working_directory.unwrap().agent, "copilot");
        let ask = receive(
            "claude",
            None,
            json!({"hook_event_name": "PreToolUse", "session_id": ROOT,
            "tool_name": "AskUserQuestion"}),
            copilot,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ask.activity.unwrap().event,
            phase(ActivityPhase::NeedsInput)
        );
        let prompt = receive(
            "claude",
            None,
            json!({"hook_event_name": "UserPromptSubmit", "session_id": ROOT}),
            copilot,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            prompt.activity.unwrap().event,
            phase(ActivityPhase::Working)
        );
    }

    #[test]
    fn the_hook_file_runs_every_event_and_never_fails() {
        let file: Value = serde_json::from_str(config("copilot").unwrap()).unwrap();
        assert_eq!(file["version"], 1);
        let hooks = file["hooks"].as_object().unwrap();
        for event in [
            "sessionStart",
            "sessionEnd",
            "userPromptSubmitted",
            "preToolUse",
            "postToolUse",
            "postToolUseFailure",
            "notification",
            "agentStop",
            "subagentStart",
            "subagentStop",
            "errorOccurred",
        ] {
            let entries = hooks[event].as_array().unwrap_or_else(|| panic!("{event}"));
            assert_eq!(entries.len(), 1);
            let command = entries[0]["bash"].as_str().unwrap();
            assert_eq!(entries[0]["type"], "command");
            assert!(entries[0]["timeoutSec"].as_u64().unwrap() <= 5);
            // A failing preToolUse hook denies the tool: a missing or older
            // `rakumux` must not.
            assert!(command.ends_with(" || true"), "{command}");
            assert!(
                command.contains(&format!("--agent copilot --event {event} ")),
                "{command}"
            );
            assert!(command.starts_with(r#""${RAKUMUX_BIN:-rakumux}" session hook"#));
        }
        assert_eq!(hooks.len(), 11);
        assert!(hooks.get("permissionRequest").is_none());
    }

    #[test]
    fn the_hook_file_goes_in_copilots_home() {
        let var = |home: Option<&'static str>, copilot: Option<&'static str>| {
            move |name: &str| match name {
                "HOME" => home.map(String::from),
                "COPILOT_HOME" => copilot.map(String::from),
                _ => None,
            }
        };
        assert_eq!(
            install_path(var(Some("/home/me"), None)),
            Some(PathBuf::from("/home/me/.copilot/hooks/rakumux.json"))
        );
        assert_eq!(
            install_path(var(Some("/home/me"), Some("/data/copilot"))),
            Some(PathBuf::from("/data/copilot/hooks/rakumux.json"))
        );
        assert_eq!(
            install_path(var(Some("/home/me"), Some(""))),
            Some(PathBuf::from("/home/me/.copilot/hooks/rakumux.json"))
        );
        assert_eq!(install_path(var(None, None)), None);
    }
}
