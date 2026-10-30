//! Native hook adapters. Compiled into the CLI; no agent SDK dependency.

use serde_json::Value;
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};
use session_core::api::{AgentWorkingDirectory, Report};
use std::path::PathBuf;

#[path = "claude-code/hooks.rs"]
mod claude;
#[path = "codex/hooks.rs"]
mod codex;
#[path = "copilot/hooks.rs"]
mod copilot;
#[path = "cursor-agent/hooks.rs"]
mod cursor;
#[path = "droid/hooks.rs"]
mod droid;
#[path = "grok/hooks.rs"]
mod grok;
#[path = "kimi/hooks.rs"]
mod kimi;
#[path = "pi/hooks.rs"]
mod pi;

pub fn normalize(agent: &str, input: &Value) -> Result<Option<ActivityUpdate>, String> {
    match agent {
        "claude" => claude::normalize(input),
        "codex" => codex::normalize(input),
        "copilot" => copilot::normalize(input),
        "cursor-agent" => cursor::normalize(input),
        "grok" => grok::normalize(input),
        "kimi" => kimi::normalize(input),
        "droid" => droid::normalize(input),
        "opencode" => {
            let update: ActivityUpdate = serde_json::from_value(input.clone())
                .map_err(|_| "invalid OpenCode activity update")?;
            if update.agent != "opencode" {
                return Err("activity agent does not match hook adapter".into());
            }
            Ok(Some(update))
        }
        "pi" => pi::normalize(input),
        _ => Err("unknown hook adapter".into()),
    }
}

/// Reduce native input to activity and the root agent's logical directory.
/// Tool arguments, transcripts, prompts and child-agent directories are ignored.
pub fn report(agent: &str, input: &Value) -> Result<Option<Report>, String> {
    if agent == "copilot" {
        return copilot::report(input);
    }
    let activity = normalize(agent, input)?;
    let context_only = (agent == "claude" && input["hook_event_name"] == "CwdChanged")
        || (agent == "kimi" && kimi::names_context(input));
    if activity.is_none() && !context_only {
        return Ok(None);
    }
    // Grok's subagents hook from sessions of their own (`subagentType`);
    // Kimi Code names its main agent `main`.
    let child = input
        .get("agent_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !(agent == "kimi" && id == "main"))
        || (agent == "grok" && input.get("subagentType").is_some())
        || (agent == "droid" && droid::is_child(input));
    let working_directory = if child || input["hook_event_name"] == "SessionEnd" {
        None
    } else if agent == "cursor-agent" {
        cursor::working_directory(input)
    } else if matches!(agent, "opencode" | "pi") {
        // Their plugin or extension names the folder itself.
        input
            .get("working_directory")
            .map(|value| {
                let context: AgentWorkingDirectory = serde_json::from_value(value.clone())
                    .map_err(|_| "invalid working directory")?;
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

/// What `rakumux session hook` received, reduced to a report: the input of a
/// hook fired by `agent` (`--agent`) or by an agent that runs its hooks (see
/// [`host`]), with `event` (`--event`) as its `hookEventName` when given,
/// for agents whose input doesn't name its event (GitHub Copilot's). `var`
/// reads the hook's environment.
pub fn receive(
    agent: &str,
    event: Option<&str>,
    mut input: Value,
    var: impl Fn(&str) -> Option<String>,
) -> Result<Option<Report>, String> {
    let agent = host(agent, &input, var);
    if let Some(event) = event {
        let valid = !event.is_empty()
            && event.len() <= 64
            && event
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'));
        if !valid {
            return Err("invalid hook event name".into());
        }
        let Some(object) = input.as_object_mut() else {
            return Err("hook input is not a JSON object".into());
        };
        object.insert(EVENT.into(), Value::String(event.into()));
    }
    report(agent, &input)
}

/// Where `--event` puts the event's name in the input.
pub const EVENT: &str = "hookEventName";

/// The agent whose hook this is. Cursor, GitHub Copilot and Grok also run
/// Claude Code's hooks (the user's or the repository's `.claude/settings.json`);
/// fired there, `--agent claude` hooks report as that agent, recognised by what
/// it adds: Cursor's input has `cursor_version`; Grok's has its own
/// `hookEventName` next to Claude's `hook_event_name` (or `GROK_HOOK_EVENT` is
/// set); GitHub Copilot sets `COPILOT_CLI=1`. Other agents' hooks are theirs.
pub fn host<'a>(agent: &'a str, input: &Value, var: impl Fn(&str) -> Option<String>) -> &'a str {
    if agent != "claude" {
        agent
    } else if input.get("cursor_version").is_some() {
        "cursor-agent"
    } else if input.get(EVENT).is_some() || var("GROK_HOOK_EVENT").is_some() {
        "grok"
    } else if var("COPILOT_CLI").as_deref() == Some("1") {
        "copilot"
    } else {
        agent
    }
}

pub fn config(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some(include_str!("claude-code/hooks.json")),
        "codex" => Some(include_str!("codex/hooks.json")),
        "copilot" => Some(include_str!("copilot/hooks.json")),
        "cursor-agent" => Some(include_str!("cursor-agent/hooks.json")),
        "droid" => Some(include_str!("droid/hooks.json")),
        "grok" => Some(include_str!("grok/hooks.json")),
        _ => None,
    }
}

/// Where `hook-config --install` writes `agent`'s configuration: a file of
/// Rakumux's own that the agent loads beside the user's (GitHub Copilot's
/// `~/.copilot/hooks/rakumux.json`, Grok's `~/.grok/hooks/rakumux.json`).
/// `None` for agents whose configuration is merged into the user's settings
/// by hand. `var` reads the environment.
pub fn install_path(agent: &str, var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    match agent {
        "copilot" => copilot::install_path(var),
        "grok" => grok::install_path(var),
        _ => None,
    }
}

/// What `rakumux session hook` prints for `agent`: `{}`, which changes
/// nothing, unless the agent reads that as more (Droid's).
pub fn stdout(agent: &str) -> &'static str {
    match agent {
        "droid" => droid::STDOUT,
        _ => "{}",
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

    fn no_vars(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn claude_code_hooks_run_by_another_agent_report_as_that_agent() {
        let claude = json!({"hook_event_name":"PreToolUse", "session_id":"root", "cwd":"/repo"});
        assert_eq!(host("claude", &claude, no_vars), "claude");
        let mut cursor = claude.clone();
        cursor["cursor_version"] = json!("2026.10.22");
        assert_eq!(host("claude", &cursor, no_vars), "cursor-agent");
        let mut grok = claude.clone();
        grok["hookEventName"] = json!("pre_tool_use");
        assert_eq!(host("claude", &grok, no_vars), "grok");
        let env = |set: &'static str, value: &'static str| {
            move |name: &str| (name == set).then(|| value.to_owned())
        };
        assert_eq!(
            host("claude", &claude, env("GROK_HOOK_EVENT", "pre_tool_use")),
            "grok"
        );
        assert_eq!(host("claude", &claude, env("COPILOT_CLI", "1")), "copilot");
        assert_eq!(host("claude", &claude, env("COPILOT_CLI", "0")), "claude");
        // GROK_AGENT is also Grok's own setting, and its tools' children inherit it.
        assert_eq!(host("claude", &claude, env("GROK_AGENT", "1")), "claude");
        // Only Claude Code's hooks are run elsewhere; other adapters stay theirs.
        assert_eq!(host("codex", &cursor, env("COPILOT_CLI", "1")), "codex");
        // Reported as that agent: an adapter of its own, never Claude Code's
        // (Cursor's reads Cursor's step names, `preToolUse`).
        assert!(receive("claude", None, cursor, no_vars).unwrap().is_none());
        let report = receive("claude", None, claude, no_vars).unwrap().unwrap();
        assert_eq!(report.activity.unwrap().agent, "claude");
    }

    #[test]
    fn an_event_named_on_the_command_line_reaches_the_adapter() {
        let input = json!({"sessionId":"root", "cwd":"/repo"});
        assert!(receive("claude", Some("bad event"), input.clone(), no_vars).is_err());
        assert!(receive("claude", Some(""), input.clone(), no_vars).is_err());
        assert!(receive("claude", Some("agentStop"), json!([1]), no_vars).is_err());
        // An adapter reads it as `hookEventName`; Claude Code's ignores it.
        let named = receive(
            "claude",
            Some("Stop"),
            json!({"hook_event_name":"Stop", "session_id":"root"}),
            no_vars,
        )
        .unwrap()
        .unwrap();
        assert_eq!(named.activity.unwrap().event, phase(ActivityPhase::Done));
        assert!(receive("claude", Some("agentStop"), input, no_vars)
            .unwrap()
            .is_none());
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
