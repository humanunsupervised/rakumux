//! Native hook adapters. Compiled into the CLI; no agent SDK dependency.

use serde_json::Value;
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};

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
}
