//! Claude Code's documented command-hook input, reduced to activity metadata.
use super::*;

pub(super) fn normalize(input: &Value) -> Result<Option<ActivityUpdate>, String> {
    let name = input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    // Child tool/stop hooks must not replace their parent's phase.
    if input.get("agent_id").and_then(Value::as_str).is_some()
        && !matches!(name, "SubagentStart" | "SubagentStop")
    {
        return Ok(None);
    }
    let event = match name {
        "SessionStart" if input["source"] == "compact" => phase(ActivityPhase::Working),
        "SessionStart" | "SessionEnd" => phase(ActivityPhase::Idle),
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" => phase(ActivityPhase::Working),
        "PreToolUse" if input["tool_name"] == "AskUserQuestion" => phase(ActivityPhase::NeedsInput),
        "PreToolUse" => phase(ActivityPhase::RunningTool),
        "PermissionRequest" => phase(ActivityPhase::NeedsInput),
        "Notification"
            if matches!(
                input["notification_type"].as_str(),
                Some("permission_prompt" | "idle_prompt")
            ) =>
        {
            phase(ActivityPhase::NeedsInput)
        }
        "Stop" => phase(ActivityPhase::Done),
        "SubagentStart" => subagent(input, true)?,
        "SubagentStop" => subagent(input, false)?,
        _ => return Ok(None),
    };
    update("claude", input, event)
}
