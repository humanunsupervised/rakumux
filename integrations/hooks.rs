//! Native hook adapters. Compiled into the CLI; no agent SDK dependency.

use serde_json::Value;
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};
use session_core::api::{AgentWorkingDirectory, Report};

#[path = "claude-code/hooks.rs"]
mod claude;
#[path = "codex/hooks.rs"]
mod codex;

pub fn normalize(agent: &str, input: &Value) -> Result<Option<ActivityUpdate>, String> {
    match agent {
        "claude" => claude::normalize(input),
        "codex" => codex::normalize(input),
        "opencode" => {
            let update: ActivityUpdate = serde_json::from_value(input.clone())
                .map_err(|_| "invalid OpenCode activity update")?;
            if update.agent != "opencode" {
                return Err("activity agent does not match hook adapter".into());
            }
            Ok(Some(update))
        }
        _ => Err("unknown hook adapter".into()),
    }
}

/// Reduce native input to activity and the root agent's logical directory.
/// Tool arguments, transcripts, prompts and child-agent directories are ignored.
pub fn report(agent: &str, input: &Value) -> Result<Option<Report>, String> {
    let activity = normalize(agent, input)?;
    let context_only = agent == "claude" && input["hook_event_name"] == "CwdChanged";
    if activity.is_none() && !context_only {
        return Ok(None);
    }
    let child = input.get("agent_id").and_then(Value::as_str).is_some();
    let working_directory = if child || input["hook_event_name"] == "SessionEnd" {
        None
    } else if agent == "opencode" {
        input
            .get("working_directory")
            .map(|value| {
                let context: AgentWorkingDirectory = serde_json::from_value(value.clone())
                    .map_err(|_| "invalid OpenCode working directory")?;
                if context.agent != agent {
                    return Err("working-directory agent does not match hook adapter");
                }
                Ok(context)
            })
            .transpose()?
    } else {
        input
            .get("cwd")
            .and_then(Value::as_str)
            .map(|path| AgentWorkingDirectory {
                agent: agent.into(),
                conversation_id: input
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                path: path.into(),
            })
    };
    if activity.is_none() && working_directory.is_none() {
        return Ok(None);
    }
    Ok(Some(Report {
        activity,
        working_directory,
        ..Report::default()
    }))
}

pub fn config(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some(include_str!("claude-code/hooks.json")),
        "codex" => Some(include_str!("codex/hooks.json")),
        _ => None,
    }
}

fn update(
    agent: &str,
    input: &Value,
    event: ActivityEvent,
) -> Result<Option<ActivityUpdate>, String> {
    let conversation = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("hook is missing session_id")?;
    Ok(Some(ActivityUpdate {
        version: ACTIVITY_VERSION,
        agent: agent.into(),
        conversation_id: conversation.into(),
        completion_id: None,
        event,
    }))
}

fn phase(phase: ActivityPhase) -> ActivityEvent {
    ActivityEvent::Phase { phase }
}

/// A hook fired inside a subagent (its input carries `agent_id`). It never replaces the
/// parent's phase. Start/stop change the count; a tool or permission hook only shows that
/// the subagent still runs, which keeps a long or background subagent counted and the
/// parent's activity fresh while the parent itself sends no hooks. A stopped subagent is
/// not resurrected (the daemon remembers stops). Other child events are ignored.
fn child(agent: &str, input: &Value, name: &str) -> Result<Option<ActivityUpdate>, String> {
    let event = match name {
        "SubagentStart" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
        | "PermissionRequest" => subagent(input, true)?,
        "SubagentStop" => subagent(input, false)?,
        _ => return Ok(None),
    };
    update(agent, input, event)
}

fn subagent(input: &Value, running: bool) -> Result<ActivityEvent, String> {
    let id = input
        .get("agent_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("subagent hook is missing agent_id")?;
    Ok(ActivityEvent::Subagent {
        id: id.into(),
        running,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_adapters_drop_prompts_outputs_and_track_subagents_by_id() {
        for agent in ["claude", "codex"] {
            let input = json!({"hook_event_name":"UserPromptSubmit", "session_id":"root", "prompt":"secret prompt", "tool_input":{"token":"secret"}});
            let update = normalize(agent, &input).unwrap().unwrap();
            assert_eq!(update.event, phase(ActivityPhase::Working));
            assert!(!serde_json::to_string(&update).unwrap().contains("secret"));
            let stopped = json!({"hook_event_name":"Stop", "session_id":"root", "last_assistant_message":"secret reply"});
            let done = normalize(agent, &stopped).unwrap().unwrap();
            assert_eq!(done.event, phase(ActivityPhase::Done));
            assert!(!serde_json::to_string(&done).unwrap().contains("secret"));
            for (name, running) in [("SubagentStart", true), ("SubagentStop", false)] {
                let input =
                    json!({"hook_event_name":name, "session_id":"root", "agent_id":"child"});
                assert_eq!(
                    normalize(agent, &input).unwrap().unwrap().event,
                    ActivityEvent::Subagent {
                        id: "child".into(),
                        running
                    }
                );
            }
            assert!(normalize(agent, &json!({"hook_event_name":"FutureEvent"}))
                .unwrap()
                .is_none());
            assert!(normalize(
                agent,
                &json!({"hook_event_name":"Stop", "session_id":"root", "agent_id":"child"})
            )
            .unwrap()
            .is_none());
            let config: Value = serde_json::from_str(config(agent).unwrap()).unwrap();
            assert!(config["hooks"]["PermissionRequest"].is_array());
        }
    }

    #[test]
    fn native_working_directories_come_from_root_metadata_not_child_tools_or_prompts() {
        for agent in ["claude", "codex"] {
            let input = json!({"hook_event_name":"PreToolUse", "session_id":"root", "cwd":"/repo/worktree",
                "prompt":"secret prompt", "tool_input":{"cwd":"/repo/wrong-child", "command":"secret command"}});
            let normalized = report(agent, &input).unwrap().unwrap();
            assert_eq!(
                normalized.working_directory.as_ref().unwrap().path,
                std::path::Path::new("/repo/worktree")
            );
            assert!(!serde_json::to_string(&normalized)
                .unwrap()
                .contains("secret"));
            let child = json!({"hook_event_name":"SubagentStart", "session_id":"root", "agent_id":"child", "cwd":"/repo/child"});
            assert!(report(agent, &child)
                .unwrap()
                .unwrap()
                .working_directory
                .is_none());
        }
        let changed = report("claude", &json!({"hook_event_name":"CwdChanged", "session_id":"root", "cwd":"/repo/new", "old_cwd":"/repo/old", "new_cwd":"/repo/new"})).unwrap().unwrap();
        assert!(
            changed.activity.is_none(),
            "a directory notification must not change the lifecycle"
        );
        assert_eq!(
            changed.working_directory.unwrap().path,
            std::path::Path::new("/repo/new")
        );
    }

    fn event_of(agent: &str, input: Value) -> Option<ActivityEvent> {
        normalize(agent, &input).unwrap().map(|update| update.event)
    }

    #[test]
    fn subagent_tool_hooks_keep_the_subagent_counted_without_replacing_the_parent_phase() {
        for agent in ["claude", "codex"] {
            for name in [
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "PermissionRequest",
            ] {
                let input = json!({"hook_event_name":name, "session_id":"root", "agent_id":"child",
                    "tool_name":"Bash", "tool_input":{"command":"secret command"}});
                let update = normalize(agent, &input).unwrap().unwrap();
                assert_eq!(update.conversation_id, "root");
                assert_eq!(
                    update.event,
                    ActivityEvent::Subagent {
                        id: "child".into(),
                        running: true
                    },
                    "{agent} {name}"
                );
                assert!(!serde_json::to_string(&update).unwrap().contains("secret"));
                assert!(report(agent, &input)
                    .unwrap()
                    .unwrap()
                    .working_directory
                    .is_none());
            }
            for name in ["Stop", "UserPromptSubmit", "Notification"] {
                let input =
                    json!({"hook_event_name":name, "session_id":"root", "agent_id":"child"});
                assert_eq!(event_of(agent, input), None, "{agent} {name}");
            }
        }
    }

    #[test]
    fn compaction_neither_starts_nor_ends_a_turn() {
        // Idle compaction and `/compact` have no Stop: reporting working left sessions
        // busy until the activity expired, then showed an unread reply.
        for agent in ["claude", "codex"] {
            let input =
                json!({"hook_event_name":"SessionStart", "session_id":"root", "source":"compact"});
            assert_eq!(event_of(agent, input.clone()), None, "{agent}");
            assert!(report(agent, &input).unwrap().is_none());
            let startup =
                json!({"hook_event_name":"SessionStart", "session_id":"root", "source":"startup"});
            assert_eq!(event_of(agent, startup), Some(phase(ActivityPhase::Idle)));
        }
    }

    #[test]
    fn claude_notifications_stop_failures_and_elicitations_map_to_what_waits() {
        let notification = |kind: &str| {
            event_of(
                "claude",
                json!({"hook_event_name":"Notification", "session_id":"root", "notification_type":kind, "message":"secret"}),
            )
        };
        assert_eq!(
            notification("permission_prompt"),
            Some(phase(ActivityPhase::NeedsInput))
        );
        assert_eq!(
            notification("idle_prompt"),
            Some(phase(ActivityPhase::Idle)),
            "idle_prompt follows a finished turn; nothing waits for an answer"
        );
        assert_eq!(notification("auth_success"), None);
        assert_eq!(
            event_of(
                "claude",
                json!({"hook_event_name":"StopFailure", "session_id":"root", "error":"rate_limit"})
            ),
            Some(phase(ActivityPhase::Idle)),
            "an API error ends the turn without a Stop"
        );
        assert_eq!(
            event_of(
                "claude",
                json!({"hook_event_name":"Elicitation", "session_id":"root", "mcp_server_name":"x"})
            ),
            Some(phase(ActivityPhase::NeedsInput))
        );
        assert_eq!(
            event_of(
                "claude",
                json!({"hook_event_name":"ElicitationResult", "session_id":"root", "action":"accept"})
            ),
            Some(phase(ActivityPhase::Working))
        );
        let config: Value = serde_json::from_str(config("claude").unwrap()).unwrap();
        for event in ["StopFailure", "Elicitation", "ElicitationResult"] {
            assert!(config["hooks"][event].is_array(), "{event} is configured");
        }
    }

    #[test]
    fn claude_stop_waiting_for_background_agents_is_still_working() {
        let stop = |tasks: Value| {
            event_of(
                "claude",
                json!({"hook_event_name":"Stop", "session_id":"root", "background_tasks":tasks}),
            )
        };
        assert_eq!(
            stop(
                json!([{"id":"a1", "type":"subagent", "status":"running", "agent_type":"Explore"}])
            ),
            Some(phase(ActivityPhase::WaitingForAgents))
        );
        assert_eq!(
            stop(json!([{"id":"w1", "type":"workflow", "status":"running"}])),
            Some(phase(ActivityPhase::WaitingForAgents))
        );
        for unrelated in [
            json!([]),
            json!([{"id":"s1", "type":"shell", "status":"running", "command":"npm run dev"}]),
            json!([{"id":"m1", "type":"monitor", "status":"running"}]),
            json!(null),
        ] {
            assert_eq!(
                stop(unrelated.clone()),
                Some(phase(ActivityPhase::Done)),
                "{unrelated}"
            );
        }
    }
}
