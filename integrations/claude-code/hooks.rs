//! Claude Code's documented command-hook input, reduced to activity metadata.
use super::*;

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let name = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    // Child hooks never replace their parent's phase (see `child`).
    if input.get("agent_id").and_then(Value::as_str).is_some() {
        return child("claude", input, name);
    }
    let event = match name {
        // Compaction runs inside a turn (auto) or as a command (`/compact`, no Stop
        // follows): it neither starts nor ends a turn.
        "SessionStart" if input["source"] == "compact" => return Ok(None),
        "SessionStart" | "SessionEnd" => phase(ActivityPhase::Idle),
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" | "ElicitationResult" => {
            phase(ActivityPhase::Working)
        }
        "PreToolUse" if input["tool_name"] == "AskUserQuestion" => phase(ActivityPhase::NeedsInput),
        "PreToolUse" => phase(ActivityPhase::RunningTool),
        "PermissionRequest" | "Elicitation" => phase(ActivityPhase::NeedsInput),
        "Notification" if input["notification_type"] == "permission_prompt" => {
            phase(ActivityPhase::NeedsInput)
        }
        // About a minute after a finished (or an Esc-interrupted, which has no Stop) turn
        // with no typing: nothing is waiting for an answer, and an unread reply stays unread.
        "Notification" if input["notification_type"] == "idle_prompt" => phase(ActivityPhase::Idle),
        // The turn paused until background subagents wake it again ("Waiting for N
        // background agents to finish"): still working, not a finished reply.
        "Stop" if waits_for_background_agents(input) => phase(ActivityPhase::Working),
        "Stop" => phase(ActivityPhase::Done),
        // An API error ended the turn: no reply completed and no hook follows.
        "StopFailure" => phase(ActivityPhase::Idle),
        _ => return Ok(None),
    };
    update("claude", input, event)
}

/// Stop's `background_tasks` lists in-flight work that will wake the conversation again.
/// Only agent work counts; shells, monitors and teammates can outlive the conversation.
fn waits_for_background_agents(input: &Value) -> bool {
    input
        .get("background_tasks")
        .and_then(Value::as_array)
        .is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| matches!(task["type"].as_str(), Some("subagent" | "workflow")))
        })
}
