//! Kimi Code's command hooks (the Rakumux plugin's, `install.rs`): which
//! conversation the main agent runs and where, never its lifecycle. Kimi
//! reports what it does through OSC 7501, subagents included; its hooks
//! can't say whose a `Stop` is (a subagent's carries the same `session_id`
//! and nothing else), so they never set a phase.
use super::*;

/// No activity, whatever the event: OSC 7501 says what Kimi does.
pub(super) fn normalize(_input: &Value) -> Result<Option<ActivityUpdate>, String> {
    Ok(None)
}

/// Whether the hook names the main agent's conversation (`session_id`) and
/// folder (`cwd`): a conversation starting or resuming, a prompt, a turn of
/// the main agent (not a subagent's own).
pub(super) fn names_context(input: &Value) -> bool {
    match input.get("hook_event_name").and_then(Value::as_str) {
        Some("SessionStart" | "UserPromptSubmit") => true,
        Some("TurnStarted") => input["origin_name"] != "subagent",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A Kimi hook's input: its base fields and `extra`.
    fn hook(event: &str, extra: Value) -> Value {
        let mut input = json!({"hook_event_name": event, "session_id": "session_9e55",
            "cwd": "/repo/worktree", "client_type": "kimi_code_cli",
            "session_title": "secret title"});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        input
    }

    #[test]
    fn the_main_agents_conversation_and_folder_and_nothing_else() {
        for input in [
            hook("SessionStart", json!({"source": "resume", "model": "kimi"})),
            hook(
                "UserPromptSubmit",
                json!({"prompt": [{"type": "text", "text": "secret prompt"}],
                "is_steer": false, "prompt_id": "msg_1"}),
            ),
            hook(
                "TurnStarted",
                json!({"turn_id": 3, "origin_kind": "user",
                "prompt": "secret prompt"}),
            ),
        ] {
            let report = receive("kimi", None, input.clone(), |_| None)
                .unwrap()
                .unwrap();
            assert!(report.activity.is_none(), "{input}");
            let context = report.working_directory.as_ref().unwrap();
            assert_eq!(context.agent, "kimi");
            assert_eq!(context.conversation_id.as_deref(), Some("session_9e55"));
            assert_eq!(context.path, std::path::Path::new("/repo/worktree"));
            assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
        }
    }

    #[test]
    fn no_phase_comes_from_kimis_hooks() {
        // A subagent's Stop is the main agent's, to its hooks: none sets a
        // phase, and a subagent's own turn names nothing.
        for input in [
            hook("Stop", json!({"stop_hook_active": false})),
            hook(
                "PreToolUse",
                json!({"tool_name": "Bash", "tool_input": {"command": "x"}}),
            ),
            hook(
                "PermissionRequest",
                json!({"agent_id": "main", "tool_name": "Bash"}),
            ),
            hook(
                "PermissionRequest",
                json!({"agent_id": "agent-0", "tool_name": "Bash"}),
            ),
            hook(
                "SubagentStart",
                json!({"agent_name": "coder", "prompt": "secret"}),
            ),
            hook(
                "SubagentStop",
                json!({"agent_name": "coder", "response": "secret"}),
            ),
            hook("Interrupt", json!({"turn_id": 3, "reason": "cancelled"})),
            hook("SessionEnd", json!({"reason": "exit"})),
            hook(
                "TurnStarted",
                json!({"origin_kind": "system_trigger",
                "origin_name": "subagent"}),
            ),
        ] {
            assert_eq!(report("kimi", &input).unwrap(), None, "{input}");
        }
        // Without a folder there's nothing to say.
        let mut no_cwd = hook("UserPromptSubmit", json!({}));
        no_cwd.as_object_mut().unwrap().remove("cwd");
        assert_eq!(report("kimi", &no_cwd).unwrap(), None);
        assert_eq!(config("kimi"), None, "installed as a plugin");
    }
}
