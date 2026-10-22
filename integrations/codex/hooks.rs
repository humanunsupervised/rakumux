//! Codex's released hooks protocol, not legacy turn-complete notifications.
use super::*;

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let name = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    // Child hooks never replace their parent's phase (see `child`).
    if input.get("agent_id").and_then(Value::as_str).is_some() {
        return child("codex", input, name);
    }
    let event = match name {
        // Compaction neither starts nor ends a turn.
        "SessionStart" if input["source"] == "compact" => return Ok(None),
        "SessionStart" | "SessionEnd" | "Interrupt" => phase(ActivityPhase::Idle),
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" => phase(ActivityPhase::Working),
        "PreToolUse" if input["tool_name"] == "request_user_input" => {
            phase(ActivityPhase::NeedsInput)
        }
        "PreToolUse" => phase(ActivityPhase::RunningTool),
        "PermissionRequest" => phase(ActivityPhase::NeedsInput),
        "Stop" => phase(ActivityPhase::Done),
        _ => return Ok(None),
    };
    update("codex", input, event)
}
