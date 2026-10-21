//! Advisory hook transport. Policy and aggregation remain in the daemon.

use super::{args::HookAgent, output_error, CliError, Globals};
use session_core::api::{CallerInfo, ErrorCode, Report};
use session_core::client::SessionApi;
use std::io::{Read, Write};

#[path = "../../../integrations/hooks.rs"]
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
    let Some(report) = adapters::report(agent.name(), &input)? else {
        return Ok(());
    };
    // Never create a daemon from a hook. Hooks are optional, not session launchers.
    let client = globals
        .connect(false)
        .map_err(|_| "cannot connect to the running daemon")?;
    submit_report(&client, report)
}

#[cfg(test)]
fn submit(
    api: &dyn SessionApi,
    update: session_core::activity::ActivityUpdate,
) -> Result<(), String> {
    submit_report(
        api,
        Report {
            activity: Some(update),
            ..Report::default()
        },
    )
}

fn submit_report(api: &dyn SessionApi, mut report: Report) -> Result<(), String> {
    if !matches!(api.caller(), CallerInfo::Session { .. }) {
        return Err("hook requires session authentication".into());
    }
    let capabilities = api.daemon_capabilities().unwrap_or_default();
    if report.activity.is_some()
        && !capabilities
            .iter()
            .any(|capability| capability == session_core::api::CAPABILITY_ACTIVITY)
    {
        return Err("daemon needs the session.activity capability".into());
    }
    if !capabilities
        .iter()
        .any(|capability| capability == session_core::api::CAPABILITY_WORKING_DIRECTORY)
    {
        report.working_directory = None;
    }
    if report.activity.is_none() && report.working_directory.is_none() {
        return Ok(());
    }
    api.report(report).map_err(|error| {
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
            completion_id: None,
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

    #[test]
    fn working_directory_reports_are_self_only_and_capability_gated() {
        let mut api = FakeApi::session_caller("A", "/repo", vec![]);
        let report = Report {
            working_directory: Some(session_core::api::AgentWorkingDirectory {
                agent: "codex".into(),
                conversation_id: Some("thread".into()),
                path: "/repo/worktree".into(),
            }),
            ..Report::default()
        };
        api.capabilities = Some(vec!["session.activity".into()]);
        submit_report(&api, report.clone()).unwrap();
        assert!(
            api.reports.lock().unwrap().is_empty(),
            "older daemons do not receive a meaning-changing optional field"
        );
        api.capabilities
            .as_mut()
            .unwrap()
            .push(session_core::api::CAPABILITY_WORKING_DIRECTORY.into());
        submit_report(&api, report.clone()).unwrap();
        assert_eq!(
            api.reports.lock().unwrap()[0].working_directory,
            report.working_directory
        );
        assert!(submit_report(&FakeApi::user(vec![]), report).is_err());
    }
}
