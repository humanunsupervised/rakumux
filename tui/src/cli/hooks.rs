//! Advisory hook transport. Policy and aggregation remain in the daemon.

use super::{args::HookAgent, output_error, CliError, Globals};
use session_core::api::{CallerInfo, ErrorCode, Report};
use session_core::client::SessionApi;
use std::io::{Read, Write};

#[path = "../../../integrations/hooks.rs"]
mod adapters;

const MAX_HOOK_BYTES: u64 = 1_048_576;

pub(super) fn configuration(
    agent: HookAgent,
    install: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let config = adapters::config(agent.name()).ok_or_else(|| {
        CliError::Usage(match agent {
            HookAgent::Opencode => {
                "OpenCode uses `rakumux-mcp opencode-plugin --install` instead".into()
            }
            HookAgent::Pi => "Pi uses `rakumux-mcp pi-plugin --install` instead".into(),
            HookAgent::Kimi => "Kimi Code uses `rakumux-mcp kimi-plugin --install` instead".into(),
            HookAgent::Amp => "Amp uses `rakumux-mcp amp-plugin --install` instead".into(),
            other => format!("{} has no hook configuration", other.name()),
        })
    })?;
    if !install {
        writeln!(out, "{config}").map_err(output_error)?;
        return Ok(0);
    }
    let path = install_file(agent, config, |name| std::env::var(name).ok())?;
    writeln!(out, "Wrote {}.", path.display()).map_err(output_error)?;
    writeln!(out, "Restart {} to load it.", agent_name(agent)).map_err(output_error)?;
    Ok(0)
}

/// Write `config` as the agent's file of Rakumux's own ([`adapters::install_path`]),
/// replacing an earlier copy; nothing else is touched. Returns where.
fn install_file(
    agent: HookAgent,
    config: &str,
    var: impl Fn(&str) -> Option<String>,
) -> Result<std::path::PathBuf, CliError> {
    let path = adapters::install_path(agent.name(), var).ok_or_else(|| {
        CliError::Usage(format!(
            "{}'s hooks go into its own settings: print them without --install and merge them there",
            agent.name()
        ))
    })?;
    let failed = |error: std::io::Error| {
        CliError::internal(format!("cannot write {}: {error}", path.display()))
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    std::fs::write(&path, format!("{}\n", config.trim_end())).map_err(failed)?;
    Ok(path)
}

/// The agent as its users know it.
fn agent_name(agent: HookAgent) -> &'static str {
    match agent {
        HookAgent::Copilot => "GitHub Copilot",
        HookAgent::Grok => "Grok",
        other => other.name(),
    }
}
pub(super) fn run(
    globals: &Globals,
    agent: HookAgent,
    event: Option<&str>,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let result = receive(globals, agent, event);
    if let Err(error) = result {
        // Do not include raw hook JSON: it may contain credentials or prompts.
        eprintln!("rakumux activity unavailable: {error}");
    }
    // Native hooks may interpret stdout or exit 2 as an instruction/denial.
    // Empty JSON has no effect on the agent's normal permission decisions
    // (Droid's answer, which it doesn't add to the chat, has none either).
    writeln!(out, "{}", adapters::stdout(agent.name())).map_err(output_error)?;
    Ok(0)
}

fn receive(globals: &Globals, agent: HookAgent, event: Option<&str>) -> Result<(), String> {
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
    let var = |name: &str| std::env::var(name).ok();
    let Some(report) = adapters::receive(agent.name(), event, input, var)? else {
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
    fn install_writes_a_file_of_rakumuxs_own_for_agents_that_load_one() {
        let home = tempfile::tempdir().unwrap();
        let homes = home.path().join("agent-homes");
        for (agent, home_var, default) in [
            (HookAgent::Copilot, "COPILOT_HOME", ".copilot"),
            (HookAgent::Grok, "GROK_HOME", ".grok"),
        ] {
            let own = homes.join(agent.name());
            std::fs::create_dir_all(own.join("hooks")).unwrap();
            // The user's own hooks are left alone.
            std::fs::write(own.join("hooks/mine.json"), "{}").unwrap();
            let config = adapters::config(agent.name()).unwrap();
            let var = |name: &str| match name {
                _ if name == home_var => Some(own.to_string_lossy().into_owned()),
                "HOME" => Some(home.path().to_string_lossy().into_owned()),
                _ => None,
            };
            let path = install_file(agent, config, var).unwrap();
            assert_eq!(path, own.join("hooks/rakumux.json"), "{}", agent.name());
            let written: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert!(written["hooks"].is_object(), "{}", agent.name());
            // Again: replaced, not added to.
            std::fs::write(&path, "old").unwrap();
            install_file(agent, config, var).unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!("{}\n", config.trim_end())
            );
            let mut names: Vec<_> = std::fs::read_dir(own.join("hooks"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            assert_eq!(names, ["mine.json", "rakumux.json"], "{}", agent.name());
            // Without its own home variable: under HOME, made when missing.
            let path = install_file(agent, config, |name| {
                (name == "HOME").then(|| home.path().to_string_lossy().into_owned())
            })
            .unwrap();
            assert_eq!(path, home.path().join(default).join("hooks/rakumux.json"));
            // Printed without --install.
            let mut out = Vec::new();
            configuration(agent, false, &mut out).unwrap();
            assert_eq!(String::from_utf8(out).unwrap(), format!("{config}\n"));
        }
    }

    #[test]
    fn other_agents_print_their_hooks_to_merge_or_name_their_plugin() {
        let home = tempfile::tempdir().unwrap();
        let var = |name: &str| (name == "HOME").then(|| home.path().to_string_lossy().into_owned());
        // Merged into the agent's own settings: printed, never installed.
        for agent in [
            HookAgent::Claude,
            HookAgent::Codex,
            HookAgent::CursorAgent,
            HookAgent::Droid,
        ] {
            let config = adapters::config(agent.name()).unwrap();
            let mut out = Vec::new();
            configuration(agent, false, &mut out).unwrap();
            assert!(String::from_utf8(out)
                .unwrap()
                .contains(&format!("--agent {}", agent.name())));
            assert!(matches!(
                install_file(agent, config, var),
                Err(CliError::Usage(_))
            ));
            let mut out = Vec::new();
            assert!(configuration(agent, true, &mut out).is_err());
            assert!(out.is_empty());
        }
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        // A plugin or extension of their own instead.
        for (agent, plugin) in [
            (HookAgent::Opencode, "opencode-plugin"),
            (HookAgent::Pi, "pi-plugin"),
            (HookAgent::Kimi, "kimi-plugin"),
            (HookAgent::Amp, "amp-plugin"),
        ] {
            for install in [false, true] {
                let Err(CliError::Usage(message)) = configuration(agent, install, &mut Vec::new())
                else {
                    panic!("{} has a hook configuration", agent.name());
                };
                assert!(message.contains(plugin), "{message}");
            }
        }
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
