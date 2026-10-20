//! Advisory hook transport. Policy and aggregation remain in the daemon.

use super::{args::HookAgent, output_error, CliError, Globals};
use session_core::api::{CallerInfo, ErrorCode, Report};
use session_core::client::SessionApi;
use std::io::{Read, Write};

#[path = "../../../Integrations/hooks.rs"]
mod adapters;

const MAX_HOOK_BYTES: u64 = 1_048_576;

pub(super) fn configuration(agent: HookAgent, out: &mut dyn Write) -> Result<i32, CliError> {
    let config = adapters::config(agent.name()).ok_or_else(|| {
        CliError::Usage("OpenCode uses `rakumux-mcp opencode-plugin --install` instead".into())
    })?;
    writeln!(out, "{config}").map_err(output_error)?;
    Ok(0)
}

pub(super) fn run(
    globals: &Globals,
    agent: HookAgent,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let result = receive(globals, agent);
    if let Err(error) = result {
        // Do not include raw hook JSON: it may contain credentials or prompts.
        eprintln!("rakumux activity unavailable: {error}");
    }
    // Native hooks may interpret stdout or exit 2 as an instruction/denial.
    // Empty JSON has no effect on the agent's normal permission decisions.
    writeln!(out, "{{}}").map_err(output_error)?;
    Ok(0)
}

fn receive(globals: &Globals, agent: HookAgent) -> Result<(), String> {
    if std::env::var_os(session_core::env::SESSION_ID).is_none()
        || std::env::var_os(session_core::env::SESSION_TOKEN).is_none()
    {
        return Ok(());
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_HOOK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read hook input")?;
    if bytes.len() as u64 > MAX_HOOK_BYTES {
        return Err("hook input exceeds 1 MiB".into());
    }
    let input = serde_json::from_slice(&bytes).map_err(|_| "invalid hook JSON")?;
    let Some(update) = adapters::normalize(agent.name(), &input)? else {
        return Ok(());
    };
    // Never create a daemon from a hook. Hooks are optional, not session launchers.
    let client = globals
        .connect(false)
        .map_err(|_| "cannot connect to the running daemon")?;
    submit(&client, update)
}

fn submit(
    api: &dyn SessionApi,
    update: session_core::activity::ActivityUpdate,
) -> Result<(), String> {
    if !matches!(api.caller(), CallerInfo::Session { .. }) {
        return Err("hook requires session authentication".into());
    }
    if !api.daemon_capabilities().is_some_and(|capabilities| {
        capabilities
            .iter()
            .any(|capability| capability == session_core::api::CAPABILITY_ACTIVITY)
    }) {
        return Err("daemon needs the session.activity capability".into());
    }
    api.report(Report {
        activity: Some(update),
        ..Report::default()
    })
    .map_err(|error| {
        if error.code == ErrorCode::Unsupported {
            "daemon does not support activity hooks".into()
        } else {
            "daemon rejected activity update".into()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::fake::FakeApi;
    use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};

    #[test]
    fn hook_transport_is_session_only_and_requires_daemon_support() {
        let update = ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "codex".into(),
            conversation_id: "thread".into(),
            event: ActivityEvent::Phase {
                phase: ActivityPhase::Working,
            },
        };
        let user = FakeApi::user(vec![]);
        assert!(submit(&user, update.clone()).is_err());
        let mut session = FakeApi::session_caller("A", "/repo", vec![]);
        session.capabilities = None;
        assert!(submit(&session, update.clone()).is_err());
        assert!(session.reports.lock().unwrap().is_empty());
        session.capabilities = Some(vec!["session.activity".into()]);
        submit(&session, update.clone()).unwrap();
        assert_eq!(
            session.reports.lock().unwrap()[0].activity.as_ref(),
            Some(&update)
        );
    }
}
