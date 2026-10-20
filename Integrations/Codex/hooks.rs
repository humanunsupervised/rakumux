//! Codex's released hooks protocol, not legacy turn-complete notifications.
use super::*;

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let name = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    if input.get("agent_id").and_then(Value::as_str).is_some()
        && !matches!(name, "SubagentStart" | "SubagentStop")
    {
        return Ok(None);
    }
    let event = match name {
        "SessionStart" if input["source"] == "compact" => phase(ActivityPhase::Working),
        "SessionStart" | "SessionEnd" | "Interrupt" => phase(ActivityPhase::Idle),
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" => phase(ActivityPhase::Working),
        "PreToolUse" if input["tool_name"] == "request_user_input" => {
            phase(ActivityPhase::NeedsInput)
        }
        "PreToolUse" => phase(ActivityPhase::RunningTool),
        "PermissionRequest" => phase(ActivityPhase::NeedsInput),
        "Stop" => phase(ActivityPhase::Done),
        "SubagentStart" => subagent(input, true)?,
        "SubagentStop" => subagent(input, false)?,
        _ => return Ok(None),
    };
    update("codex", input, event)
}
