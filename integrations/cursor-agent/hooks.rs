//! Cursor's command hooks (`~/.cursor/hooks.json`, and Claude Code's hooks
//! it runs), reduced to activity metadata. Each input names Cursor's step
//! (`hook_event_name`: `beforeSubmitPrompt`, `preToolUse`, `stop`, …), the
//! chat (`conversation_id`, also `session_id`), its `cursor_version` and
//! `workspace_roots`. There is no step for an approval prompt or a question:
//! the screen says those (`approvals_on_screen`).
use super::*;

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let name = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    // A subagent's tool call (it names the parent's Task call): never the
    // parent's phase, and it doesn't say which subagent it is.
    if child(input) {
        return Ok(None);
    }
    let event = match name {
        "sessionStart" | "sessionEnd" => phase(ActivityPhase::Idle),
        "beforeSubmitPrompt" | "postToolUse" | "postToolUseFailure" => {
            phase(ActivityPhase::Working)
        }
        "preToolUse" if asks(input) => phase(ActivityPhase::NeedsInput),
        "preToolUse" => phase(ActivityPhase::RunningTool),
        "subagentStart" | "subagentStop" => {
            let id = input
                .get("subagent_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or("subagent hook is missing subagent_id")?;
            ActivityEvent::Subagent {
                id: id.into(),
                running: name == "subagentStart",
            }
        }
        "stop" if input["status"] == "completed" => phase(ActivityPhase::Done),
        // Interrupted, or an error: no reply finished.
        "stop" => phase(ActivityPhase::Idle),
        // Compaction (`preCompact`) neither starts nor ends a turn.
        _ => return Ok(None),
    };
    // A subagent counts under its parent's chat.
    let keys: &[&str] = match event {
        ActivityEvent::Subagent { .. } => {
            &["parent_conversation_id", "session_id", "conversation_id"]
        }
        _ => &["session_id", "conversation_id"],
    };
    let conversation = keys
        .iter()
        .find_map(|key| {
            input
                .get(*key)
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
        })
        .ok_or("hook is missing conversation_id")?;
    Ok(Some(ActivityUpdate {
        version: ACTIVITY_VERSION,
        agent: "cursor-agent".into(),
        conversation_id: conversation.into(),
        completion_id: None,
        event,
    }))
}

/// A hook fired for a subagent's tool call.
fn child(input: &Value) -> bool {
    input
        .get("parent_tool_call_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
}

/// Its question tool: the agent waits for the user's answers.
fn asks(input: &Value) -> bool {
    input["tool_name"]
        .as_str()
        .map(|name| name.to_ascii_lowercase().replace('_', ""))
        .is_some_and(|name| name == "askquestion")
}

/// The chat's directory: its workspace (a `-w` worktree's own), never a
/// tool's `cwd` or a subagent's.
pub(super) fn working_directory(input: &Value) -> Option<AgentWorkingDirectory> {
    let name = input["hook_event_name"].as_str().unwrap_or("");
    if child(input) || name.starts_with("subagent") || name == "sessionEnd" {
        return None;
    }
    let [root] = input.get("workspace_roots")?.as_array()?.as_slice() else {
        return None;
    };
    Some(AgentWorkingDirectory {
        agent: "cursor-agent".into(),
        conversation_id: ["conversation_id", "session_id"]
            .into_iter()
            .find_map(|key| input.get(key).and_then(Value::as_str))
            .filter(|id| !id.is_empty())
            .map(str::to_owned),
        path: root.as_str()?.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What Cursor sends every hook, as `4097.index.js` builds it.
    fn hook(name: &str, extra: Value) -> Value {
        let mut input = json!({"conversation_id": "c1", "generation_id": "g1", "model": "auto",
            "session_id": "c1", "hook_event_name": name, "cursor_version": "2026.10.22-27dcf5f",
            "workspace_roots": ["/repo/wt"], "user_email": "me@example.com",
            "transcript_path": "/home/me/.cursor/projects/repo/agent-transcripts/c1/c1.jsonl"});
        for (key, value) in extra.as_object().unwrap() {
            input[key] = value.clone();
        }
        input
    }

    fn event(input: Value) -> Option<ActivityEvent> {
        normalize(&input).unwrap().map(|update| update.event)
    }

    #[test]
    fn steps_map_to_phases() {
        for (name, extra, expected) in [
            (
                "sessionStart",
                json!({"composer_mode": "agent"}),
                Some(phase(ActivityPhase::Idle)),
            ),
            (
                "beforeSubmitPrompt",
                json!({"prompt": "secret prompt"}),
                Some(phase(ActivityPhase::Working)),
            ),
            (
                "preToolUse",
                json!({"tool_name": "Shell", "tool_input": {"command": "secret"}, "cwd": "/repo/wt/sub"}),
                Some(phase(ActivityPhase::RunningTool)),
            ),
            (
                "preToolUse",
                json!({"tool_name": "AskQuestion"}),
                Some(phase(ActivityPhase::NeedsInput)),
            ),
            (
                "postToolUse",
                json!({"tool_name": "Shell", "tool_output": "secret output"}),
                Some(phase(ActivityPhase::Working)),
            ),
            (
                "postToolUseFailure",
                json!({"tool_name": "Shell", "error_message": "x"}),
                Some(phase(ActivityPhase::Working)),
            ),
            (
                "stop",
                json!({"status": "completed", "loop_count": 0}),
                Some(phase(ActivityPhase::Done)),
            ),
            (
                "stop",
                json!({"status": "aborted"}),
                Some(phase(ActivityPhase::Idle)),
            ),
            (
                "stop",
                json!({"status": "error", "error": "rate limited"}),
                Some(phase(ActivityPhase::Idle)),
            ),
            (
                "sessionEnd",
                json!({"reason": "user_close"}),
                Some(phase(ActivityPhase::Idle)),
            ),
            ("preCompact", json!({}), None),
            ("afterAgentResponse", json!({"text": "secret reply"}), None),
            ("beforeShellExecution", json!({"command": "secret"}), None),
        ] {
            let input = hook(name, extra);
            assert_eq!(event(input.clone()), expected, "{name}");
            if let Some(update) = normalize(&input).unwrap() {
                assert_eq!(update.agent, "cursor-agent");
                assert_eq!(update.conversation_id, "c1");
                assert_eq!(update.completion_id, None);
                let report = report("cursor-agent", &input).unwrap().unwrap();
                let sent = serde_json::to_string(&report).unwrap();
                for private in [
                    "secret",
                    "me@example.com",
                    "agent-transcripts",
                    "/repo/wt/sub",
                ] {
                    assert!(!sent.contains(private), "{name} sends {private}");
                }
            }
        }
    }

    #[test]
    fn subagents_count_under_the_parent_and_their_tool_calls_never_replace_its_phase() {
        let start = hook(
            "subagentStart",
            json!({"subagent_id": "s1", "parent_conversation_id": "c1", "conversation_id": "c1-sub",
                "tool_call_id": "t1", "task": "secret task"}),
        );
        let update = normalize(&start).unwrap().unwrap();
        assert_eq!(update.conversation_id, "c1");
        assert_eq!(
            update.event,
            ActivityEvent::Subagent {
                id: "s1".into(),
                running: true
            }
        );
        assert!(report("cursor-agent", &start)
            .unwrap()
            .unwrap()
            .working_directory
            .is_none());
        let stop = hook(
            "subagentStop",
            json!({"subagent_id": "s1", "status": "completed"}),
        );
        assert_eq!(
            event(stop),
            Some(ActivityEvent::Subagent {
                id: "s1".into(),
                running: false
            })
        );
        assert!(normalize(&hook("subagentStop", json!({}))).is_err());
        for name in ["preToolUse", "postToolUse", "postToolUseFailure"] {
            let input = hook(
                name,
                json!({"parent_tool_call_id": "t1", "tool_name": "Read"}),
            );
            assert_eq!(event(input.clone()), None, "{name}");
            assert!(report("cursor-agent", &input).unwrap().is_none());
        }
    }

    #[test]
    fn the_working_directory_is_the_single_workspace_root() {
        let input = hook("beforeSubmitPrompt", json!({}));
        let context = report("cursor-agent", &input)
            .unwrap()
            .unwrap()
            .working_directory
            .unwrap();
        assert_eq!(context.agent, "cursor-agent");
        assert_eq!(context.conversation_id.as_deref(), Some("c1"));
        assert_eq!(context.path, std::path::Path::new("/repo/wt"));
        let mut two = input.clone();
        two["workspace_roots"] = json!(["/a", "/b"]);
        assert!(report("cursor-agent", &two)
            .unwrap()
            .unwrap()
            .working_directory
            .is_none());
        assert!(report("cursor-agent", &hook("sessionEnd", json!({})))
            .unwrap()
            .unwrap()
            .working_directory
            .is_none());
    }

    #[test]
    fn claude_code_hooks_run_by_cursor_report_as_cursor() {
        // Cursor runs `~/.claude/settings.json` hooks under its own step names.
        let input = hook("stop", json!({"status": "completed"}));
        let report = receive("claude", None, input, |_| None).unwrap().unwrap();
        let activity = report.activity.unwrap();
        assert_eq!(activity.agent, "cursor-agent");
        assert_eq!(activity.event, phase(ActivityPhase::Done));
        assert_eq!(report.working_directory.unwrap().agent, "cursor-agent");
    }

    #[test]
    fn the_configuration_is_cursors_format_and_runs_the_cursor_adapter() {
        let config: Value = serde_json::from_str(config("cursor-agent").unwrap()).unwrap();
        assert_eq!(config["version"], 1);
        let hooks = config["hooks"].as_object().unwrap();
        for step in [
            "sessionStart",
            "sessionEnd",
            "beforeSubmitPrompt",
            "preToolUse",
            "postToolUse",
            "postToolUseFailure",
            "subagentStart",
            "subagentStop",
            "stop",
        ] {
            let command = hooks[step][0]["command"].as_str().unwrap();
            assert!(
                command.ends_with("session hook --agent cursor-agent"),
                "{step}: {command}"
            );
            assert_eq!(hooks[step][0]["timeout"], 3);
        }
        // Every step it configures is one the adapter reads.
        for step in hooks.keys() {
            let input = hook(step, json!({"status": "completed", "subagent_id": "s1"}));
            assert!(normalize(&input).unwrap().is_some(), "{step}");
        }
    }
}
