//! Grok Build's command-hook input, reduced to activity metadata.
//!
//! Grok sends both spellings: its own (`hookEventName: "pre_tool_use"`,
//! `sessionId`) and Claude Code's (`hook_event_name: "PreToolUse"`,
//! `session_id`); this reads Claude Code's, which Grok also sends to Claude
//! Code's hooks it runs. A subagent runs in a session of its own, and its hooks
//! carry its own `session_id` (the `subagentId` its parent's `SubagentStart`
//! announced) and its `subagentType`, never the parent's id: they're passed as
//! plain phases under that id, which the daemon takes as that subagent's
//! heartbeat once the parent announced it.
use super::*;

/// `$GROK_HOME/hooks/rakumux.json` (`~/.grok` by default): Grok loads every
/// `*.json` in its hooks folder, so Rakumux's needs nothing merged.
pub(super) fn install_path(var: impl Fn(&str) -> Option<String>) -> Option<std::path::PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    let home = set("GROK_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| set("HOME").map(|home| std::path::Path::new(&home).join(".grok")))?;
    Some(home.join("hooks").join("rakumux.json"))
}

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let name = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    if name != "SubagentStart" && input.get("subagentType").is_some() {
        return subagent_session(input, name);
    }
    let notification = input.get("notification_type").and_then(Value::as_str);
    let event = match name {
        "SessionStart" | "SessionEnd" => phase(ActivityPhase::Idle),
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
            phase(ActivityPhase::Working)
        }
        "PreToolUse" if input["tool_name"] == "ask_user_question" => {
            phase(ActivityPhase::NeedsInput)
        }
        "PreToolUse" => phase(ActivityPhase::RunningTool),
        "Notification" => match notification {
            Some("permission_prompt" | "elicitation_dialog") => phase(ActivityPhase::NeedsInput),
            // About a minute after a turn ended (one Ctrl+C ended included): nothing
            // waits for an answer, and an unread reply stays unread.
            Some("idle_prompt") => phase(ActivityPhase::Idle),
            _ => return Ok(None),
        },
        // At exit Grok fires a Stop for each session it held (`reason: shutdown`).
        "Stop" if !ends_a_turn(input) => return Ok(None),
        "Stop" if waits_for_background_agents(input) => phase(ActivityPhase::WaitingForAgents),
        "Stop" => {
            let completion = input
                .get("promptId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned);
            let mut update = update("grok", input, phase(ActivityPhase::Done))?;
            if let Some(update) = update.as_mut() {
                update.completion_id = completion;
            }
            return Ok(update);
        }
        // An API error or an interrupt (Ctrl+C mid-turn, a declined prompt) ended
        // the turn without a reply.
        "StopFailure" | "StopCancelled" => phase(ActivityPhase::Idle),
        "SubagentStart" => {
            let id = input
                .get("subagentId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or("subagent hook is missing subagentId")?;
            ActivityEvent::Subagent {
                id: id.into(),
                running: true,
            }
        }
        _ => return Ok(None),
    };
    update("grok", input, event)
}

/// A turn's end: `end_turn`, never the Stop Grok fires for each session when it
/// exits (`shutdown`, `channel_closed`).
fn ends_a_turn(input: &Value) -> bool {
    input
        .get("reason")
        .and_then(Value::as_str)
        .is_none_or(|reason| reason == "end_turn")
}

/// Stop's `backgroundTasks` lists in-flight work that will wake the conversation
/// again. Only agent work counts; shells and monitors can outlive the turn.
fn waits_for_background_agents(input: &Value) -> bool {
    input
        .get("backgroundTasks")
        .and_then(Value::as_array)
        .is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| matches!(task["type"].as_str(), Some("subagent" | "workflow")))
        })
}

/// A hook from a subagent's own session: its work keeps it running, its end
/// stops it. Its first prompt is left out: its parent's `SubagentStart`,
/// fired just before, already counted it, and a hook that overtook that one
/// would read as a new conversation.
fn subagent_session(input: &Value, name: &str) -> Result<Option<ActivityUpdate>, String> {
    let event = match name {
        "PreToolUse" => phase(ActivityPhase::RunningTool),
        "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => phase(ActivityPhase::Working),
        "Notification" if input["notification_type"] == "permission_prompt" => {
            phase(ActivityPhase::NeedsInput)
        }
        "SubagentStop" | "Stop" => phase(ActivityPhase::Done),
        "SessionEnd" | "StopFailure" | "StopCancelled" => phase(ActivityPhase::Idle),
        _ => return Ok(None),
    };
    update("grok", input, event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROOT: &str = "01a18b49-8533-7252-8d65-46555f07d0c5";
    const CHILD: &str = "01a18b4b-bce9-74f0-9c4d-284e53688b80";

    /// A hook's input as Grok sends it: both spellings, with `fields`.
    fn hook(event: &str, grok_event: &str, session: &str, fields: Value) -> Value {
        let mut input = json!({
            "hookEventName": grok_event, "hook_event_name": event,
            "sessionId": session, "session_id": session,
            "cwd": "/home/me/proj", "workspaceRoot": "/home/me/proj",
            "timestamp": "2026-10-30T05:30:36.374820058+00:00",
            "permissionMode": "default", "permission_mode": "default",
            "transcriptPath": "/home/me/.grok/sessions/%2Fhome%2Fme%2Fproj/x/updates.jsonl",
        });
        for (key, value) in fields.as_object().unwrap() {
            input[key] = value.clone();
        }
        input
    }

    fn event_of(input: &Value) -> Option<ActivityEvent> {
        normalize(input).unwrap().map(|update| update.event)
    }

    #[test]
    fn root_events_map_to_what_the_agent_does() {
        let cases = [
            (
                hook(
                    "SessionStart",
                    "session_start",
                    ROOT,
                    json!({"source": "new"}),
                ),
                Some(phase(ActivityPhase::Idle)),
            ),
            (
                hook(
                    "UserPromptSubmit",
                    "user_prompt_submit",
                    ROOT,
                    json!({"prompt": "secret prompt", "promptId": "p1"}),
                ),
                Some(phase(ActivityPhase::Working)),
            ),
            (
                hook(
                    "PreToolUse",
                    "pre_tool_use",
                    ROOT,
                    json!({"tool_name": "run_terminal_command", "tool_input": {"command": "secret"}}),
                ),
                Some(phase(ActivityPhase::RunningTool)),
            ),
            (
                hook(
                    "PreToolUse",
                    "pre_tool_use",
                    ROOT,
                    json!({"tool_name": "ask_user_question"}),
                ),
                Some(phase(ActivityPhase::NeedsInput)),
            ),
            (
                hook(
                    "PostToolUse",
                    "post_tool_use",
                    ROOT,
                    json!({"tool_name": "write", "toolResult": {"type": "SearchReplace"}}),
                ),
                Some(phase(ActivityPhase::Working)),
            ),
            (
                hook(
                    "PostToolUseFailure",
                    "post_tool_use_failure",
                    ROOT,
                    json!({}),
                ),
                Some(phase(ActivityPhase::Working)),
            ),
            (
                hook("PermissionDenied", "permission_denied", ROOT, json!({})),
                Some(phase(ActivityPhase::Working)),
            ),
            (
                hook(
                    "Notification",
                    "notification",
                    ROOT,
                    json!({"notification_type": "permission_prompt", "notificationType": "permission_prompt"}),
                ),
                Some(phase(ActivityPhase::NeedsInput)),
            ),
            (
                hook(
                    "Notification",
                    "notification",
                    ROOT,
                    json!({"notification_type": "elicitation_dialog"}),
                ),
                Some(phase(ActivityPhase::NeedsInput)),
            ),
            (
                hook(
                    "Notification",
                    "notification",
                    ROOT,
                    json!({"notification_type": "idle_prompt"}),
                ),
                Some(phase(ActivityPhase::Idle)),
            ),
            (
                hook(
                    "Notification",
                    "notification",
                    ROOT,
                    json!({"notification_type": "task_complete"}),
                ),
                None,
            ),
            (
                hook(
                    "StopFailure",
                    "stop_failure",
                    ROOT,
                    json!({"error": "rate_limit"}),
                ),
                Some(phase(ActivityPhase::Idle)),
            ),
            (
                hook(
                    "StopCancelled",
                    "stop_cancelled",
                    ROOT,
                    json!({"reason": "user_interrupt"}),
                ),
                Some(phase(ActivityPhase::Idle)),
            ),
            (
                hook(
                    "SessionEnd",
                    "session_end",
                    ROOT,
                    json!({"reason": "shutdown"}),
                ),
                Some(phase(ActivityPhase::Idle)),
            ),
            (hook("PreCompact", "pre_compact", ROOT, json!({})), None),
            (hook("FutureEvent", "future_event", ROOT, json!({})), None),
        ];
        for (input, expected) in cases {
            assert_eq!(event_of(&input), expected, "{}", input["hook_event_name"]);
            if let Some(update) = normalize(&input).unwrap() {
                assert_eq!(
                    (update.agent.as_str(), update.conversation_id.as_str()),
                    ("grok", ROOT)
                );
                assert!(!serde_json::to_string(&update).unwrap().contains("secret"));
            }
        }
    }

    #[test]
    fn a_finished_turn_is_done_with_its_prompt_as_the_completion() {
        let stop = |fields: Value| normalize(&hook("Stop", "stop", ROOT, fields)).unwrap();
        let done = stop(
            json!({"reason": "end_turn", "promptId": "p1", "backgroundTasks": [],
            "lastAssistantMessage": "secret reply", "last_assistant_message": "secret reply"}),
        )
        .unwrap();
        assert_eq!(done.event, phase(ActivityPhase::Done));
        assert_eq!(done.completion_id.as_deref(), Some("p1"));
        assert!(!serde_json::to_string(&done).unwrap().contains("secret"));
        // The turn paused until its background subagents wake it.
        let waiting = stop(json!({"reason": "end_turn", "promptId": "p2",
            "backgroundTasks": [{"id": CHILD, "type": "subagent", "status": "running"}]}))
        .unwrap();
        assert_eq!(waiting.event, phase(ActivityPhase::WaitingForAgents));
        let shell = stop(json!({"reason": "end_turn", "promptId": "p3",
            "backgroundTasks": [{"id": "t1", "type": "shell", "status": "running"}]}))
        .unwrap();
        assert_eq!(shell.event, phase(ActivityPhase::Done));
        // At exit, one Stop per session: no turn ended.
        assert!(stop(json!({"reason": "shutdown"})).is_none());
        assert!(stop(json!({"reason": "channel_closed"})).is_none());
    }

    #[test]
    fn subagents_are_announced_under_the_root_then_their_own_hooks_are_heartbeats() {
        let start = normalize(&hook("SubagentStart", "subagent_start", ROOT,
            json!({"subagentId": CHILD, "subagentType": "general-purpose", "description": "Helper"})))
        .unwrap()
        .unwrap();
        assert_eq!(start.conversation_id, ROOT);
        assert_eq!(
            start.event,
            ActivityEvent::Subagent {
                id: CHILD.into(),
                running: true
            }
        );
        let child = |event: &str, grok_event: &str, fields: Value| {
            let mut fields = fields;
            fields["subagentType"] = json!("general-purpose");
            normalize(&hook(event, grok_event, CHILD, fields)).unwrap()
        };
        for (event, grok_event, expected) in [
            (
                "PreToolUse",
                "pre_tool_use",
                Some(phase(ActivityPhase::RunningTool)),
            ),
            (
                "PostToolUse",
                "post_tool_use",
                Some(phase(ActivityPhase::Working)),
            ),
            (
                "SubagentStop",
                "subagent_stop",
                Some(phase(ActivityPhase::Done)),
            ),
            (
                "SessionEnd",
                "session_end",
                Some(phase(ActivityPhase::Idle)),
            ),
            // Its first prompt follows its parent's SubagentStart; it adds nothing.
            ("UserPromptSubmit", "user_prompt_submit", None),
            ("Notification", "notification", None),
        ] {
            let update = child(
                event,
                grok_event,
                json!({"subagentId": CHILD, "prompt": "secret task"}),
            );
            assert_eq!(
                update.as_ref().map(|u| u.event.clone()),
                expected,
                "{event}"
            );
            if let Some(update) = update {
                assert_eq!(update.conversation_id, CHILD, "under its own session");
                assert_eq!(update.completion_id, None);
            }
        }
        assert_eq!(
            child(
                "Notification",
                "notification",
                json!({"notification_type": "permission_prompt"})
            )
            .map(|u| u.event),
            Some(phase(ActivityPhase::NeedsInput))
        );
        assert!(normalize(&hook("SubagentStart", "subagent_start", ROOT, json!({}))).is_err());
    }

    #[test]
    fn grok_payloads_at_claude_code_hooks_report_as_grok() {
        // Grok runs Claude Code's hooks too (`~/.claude/settings.json`), with its
        // own `hookEventName` beside Claude Code's spelling.
        let no_vars = |_: &str| None;
        let input = hook(
            "UserPromptSubmit",
            "user_prompt_submit",
            ROOT,
            json!({"prompt": "secret prompt", "promptId": "p1"}),
        );
        let report = receive("claude", None, input, no_vars).unwrap().unwrap();
        let activity = report.activity.unwrap();
        assert_eq!(
            (activity.agent.as_str(), activity.conversation_id.as_str()),
            ("grok", ROOT)
        );
        let directory = report.working_directory.unwrap();
        assert_eq!(directory.agent, "grok");
        assert_eq!(directory.conversation_id.as_deref(), Some(ROOT));
        assert_eq!(directory.path, std::path::Path::new("/home/me/proj"));
        // A subagent's own session says where it works, never where the session does.
        let child = hook(
            "PostToolUse",
            "post_tool_use",
            CHILD,
            json!({"subagentType": "explore", "cwd": "/home/me/proj/.grok/worktrees/x"}),
        );
        let report = receive("claude", None, child, no_vars).unwrap().unwrap();
        assert!(report.working_directory.is_none());
        assert_eq!(report.activity.unwrap().conversation_id, CHILD);
        // So does Grok's own adapter, and the session's end says nothing of a place.
        let end = hook(
            "SessionEnd",
            "session_end",
            ROOT,
            json!({"reason": "shutdown"}),
        );
        assert!(report_of(&end).working_directory.is_none());
    }

    fn report_of(input: &Value) -> Report {
        report("grok", input).unwrap().unwrap()
    }

    #[test]
    fn the_configuration_installs_as_a_file_of_its_own_in_groks_hooks_folder() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert_eq!(
            super::super::install_path("grok", env(&[("HOME", "/home/me")])),
            Some("/home/me/.grok/hooks/rakumux.json".into())
        );
        assert_eq!(
            super::super::install_path(
                "grok",
                env(&[("HOME", "/home/me"), ("GROK_HOME", "/srv/grok")])
            ),
            Some("/srv/grok/hooks/rakumux.json".into())
        );
        assert_eq!(
            super::super::install_path("grok", env(&[("GROK_HOME", "")])),
            None
        );
        assert_eq!(
            super::super::install_path("claude", env(&[("HOME", "/home/me")])),
            None
        );
    }

    #[test]
    fn the_configuration_names_every_event_the_adapter_reads() {
        let config: Value = serde_json::from_str(config("grok").unwrap()).unwrap();
        for event in [
            "SessionStart",
            "SessionEnd",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "PermissionDenied",
            "Notification",
            "Stop",
            "StopFailure",
            "StopCancelled",
            "SubagentStart",
            "SubagentStop",
        ] {
            let groups = config["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("{event}"));
            let command = groups[0]["hooks"][0]["command"].as_str().unwrap();
            assert!(command.ends_with("session hook --agent grok"), "{event}");
        }
        // Grok warns about a matcher on Stop or UserPromptSubmit.
        assert!(config["hooks"]["Stop"][0].get("matcher").is_none());
        assert!(config["hooks"]["UserPromptSubmit"][0]
            .get("matcher")
            .is_none());
    }
}
