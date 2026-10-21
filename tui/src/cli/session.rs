//! `rakumux session …`: JSON-first commands for agents and scripts
//! (agent control stage A: list, get, read, wait, report, bind). Results go to
//! stdout as one JSON object; errors to stderr as `{"error":{…}}`.

use super::args::{ReportArgs, SessionCommand, UntilArg};
use super::{output_error, resolve_target, CliError, Invocation};
use serde::Serialize;
use serde_json::json;
use session_core::api::{ReadRequest, Report, WaitRequest};
use session_core::client::SessionApi;
use session_core::model::{AgentInfo, ReportResult};
use std::io::Write;

pub(crate) fn run(
    api: &dyn SessionApi,
    invocation: &Invocation,
    command: SessionCommand,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let scope = || invocation.request_scope(api);
    match command {
        SessionCommand::Hook { .. } | SessionCommand::HookConfig { .. } => Err(CliError::internal(
            "hook command requires the hook transport",
        )),
        SessionCommand::List { all } => {
            let sessions = api.list(scope()?.as_ref(), all)?;
            emit(out, &json!({ "sessions": sessions }))
        }
        SessionCommand::Get { target } => {
            let session = resolve_target(api, scope()?.as_ref(), &target)?;
            // Fresh view of the one session (the list may be abbreviated).
            let session = api.get(&session.id)?;
            emit(out, &json!({ "session": session }))
        }
        SessionCommand::Read {
            target,
            source,
            lines,
            format,
        } => {
            let id = resolve_target(api, scope()?.as_ref(), &target)?.id;
            let result = api.read(ReadRequest {
                id,
                source: source.into(),
                lines,
                format: format.into(),
            })?;
            emit(out, &result)
        }
        SessionCommand::Wait {
            target,
            until,
            pattern,
            timeout_ms,
        } => {
            match (until, &pattern) {
                (UntilArg::Output, None) => {
                    return Err(CliError::Usage("--until output needs --pattern".into()))
                }
                (UntilArg::Output, Some(_)) | (_, None) => {}
                (_, Some(_)) => {
                    return Err(CliError::Usage(
                        "--pattern only applies to --until output".into(),
                    ))
                }
            }
            let id = resolve_target(api, scope()?.as_ref(), &target)?.id;
            let result = api.wait(WaitRequest {
                id,
                until: until.into(),
                pattern,
                timeout_ms,
            })?;
            emit(out, &result)
        }
        SessionCommand::Report(args) => {
            api.report(build_report(args)?)?;
            emit(out, &json!({}))
        }
        SessionCommand::Bind {
            agent,
            conversations,
        } => {
            api.bind(&agent, &conversations)?;
            emit(out, &json!({}))
        }
    }
}

/// Turn `report` flags into a `Report`, refusing an empty one.
fn build_report(args: ReportArgs) -> Result<Report, CliError> {
    if args.resume != !args.resume_argv.is_empty() {
        return Err(CliError::Usage(if args.resume {
            "--resume needs the command after `--`, e.g. `--resume -- claude --resume ID`".into()
        } else {
            "a command after `--` needs --resume".into()
        }));
    }
    let report = Report {
        working_directory: None,
        activity: None,
        state: args.state.map(Into::into),
        text: args.text,
        result: args.summary.map(|summary| ReportResult {
            summary,
            files: args.files,
        }),
        agent: args.agent_kind.map(|kind| AgentInfo {
            kind,
            agent_session_id: args.agent_session_id,
            agent_session_path: args.agent_session_path,
        }),
        resume: args.resume.then_some(args.resume_argv),
    };
    if report == Report::default() {
        return Err(CliError::Usage(
            "nothing to report: give --state, --text, --summary, --agent-kind or --resume".into(),
        ));
    }
    Ok(report)
}

fn emit(out: &mut dyn Write, value: &impl Serialize) -> Result<i32, CliError> {
    let text = serde_json::to_string(value)
        .map_err(|error| CliError::internal(format!("cannot encode JSON: {error}")))?;
    writeln!(out, "{text}").map_err(output_error)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::super::args::{FormatArg, SourceArg, StateArg};
    use super::super::fake::{session, FakeApi};
    use super::*;
    use session_core::api::{ReadSource, TextFormat, WaitUntil};
    use session_core::ids::SessionId;
    use session_core::model::Lifecycle;

    fn inside() -> (FakeApi, Invocation) {
        let api = FakeApi::session_caller(
            "S1",
            "/scope",
            vec![session("S1", "me"), session("S2", "reviewer")],
        );
        let invocation = Invocation {
            cwd: "/scope".into(),
            env: vec![],
            self_id: Some(SessionId::from_string("S1")),
        };
        (api, invocation)
    }

    fn json_of(
        api: &FakeApi,
        invocation: &Invocation,
        command: SessionCommand,
    ) -> serde_json::Value {
        let mut out = Vec::new();
        assert_eq!(run(api, invocation, command, &mut out).unwrap(), 0);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "one JSON line: {text}");
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn list_and_get_print_sessions() {
        let (api, inv) = inside();
        let value = json_of(&api, &inv, SessionCommand::List { all: true });
        assert_eq!(value["sessions"].as_array().unwrap().len(), 2);
        // Session callers send no scope: the daemon uses their own.
        assert_eq!(api.calls()[0], "list None dead=true");

        let value = json_of(
            &api,
            &inv,
            SessionCommand::Get {
                target: "reviewer".into(),
            },
        );
        assert_eq!(value["session"]["id"], "S2");
        assert_eq!(api.calls().last().unwrap(), "get S2");
    }

    #[test]
    fn read_and_wait_map_their_options() {
        let (api, inv) = inside();
        let value = json_of(
            &api,
            &inv,
            SessionCommand::Read {
                target: "reviewer".into(),
                source: SourceArg::RecentUnwrapped,
                lines: Some(120),
                format: FormatArg::Ansi,
            },
        );
        assert_eq!(value["untrusted"], true);
        assert_eq!(
            api.reads.lock().unwrap()[0],
            ReadRequest {
                id: SessionId::from_string("S2"),
                source: ReadSource::RecentUnwrapped,
                lines: Some(120),
                format: TextFormat::Ansi,
            }
        );

        let value = json_of(
            &api,
            &inv,
            SessionCommand::Wait {
                target: "S2".into(),
                until: UntilArg::Output,
                pattern: Some("PASS".into()),
                timeout_ms: 5000,
            },
        );
        assert_eq!(value["matched"], false);
        assert_eq!(value["reason"], "timeout");
        assert_eq!(
            api.waits.lock().unwrap()[0],
            WaitRequest {
                id: SessionId::from_string("S2"),
                until: WaitUntil::Output,
                pattern: Some("PASS".into()),
                timeout_ms: 5000,
            }
        );
    }

    #[test]
    fn wait_patterns_are_validated_before_calling() {
        let (api, inv) = inside();
        for (until, pattern) in [(UntilArg::Output, None), (UntilArg::Idle, Some("x".into()))] {
            let command = SessionCommand::Wait {
                target: "S2".into(),
                until,
                pattern,
                timeout_ms: 1,
            };
            let error = run(&api, &inv, command, &mut Vec::new()).unwrap_err();
            assert!(matches!(error, CliError::Usage(_)));
        }
        assert!(api.calls().is_empty());
    }

    #[test]
    fn bind_sends_the_agent_and_conversations() {
        let (api, inv) = inside();
        let command = SessionCommand::Bind {
            agent: "opencode".into(),
            conversations: vec!["ses_a".into(), "ses_b".into()],
        };
        assert_eq!(json_of(&api, &inv, command), json!({}));
        assert_eq!(
            api.binds.lock().unwrap()[0],
            (
                "opencode".to_string(),
                vec!["ses_a".to_string(), "ses_b".to_string()]
            )
        );
    }

    #[test]
    fn report_builds_the_full_payload() {
        let (api, inv) = inside();
        let args = ReportArgs {
            state: Some(StateArg::Done),
            text: Some("3 findings".into()),
            summary: Some("see files".into()),
            files: vec!["a.rs".into()],
            agent_kind: Some("claude".into()),
            agent_session_id: Some("abc".into()),
            agent_session_path: None,
            resume: true,
            resume_argv: vec!["claude".into(), "--resume".into(), "abc".into()],
        };
        let value = json_of(&api, &inv, SessionCommand::Report(args));
        assert_eq!(value, json!({}));
        assert_eq!(
            api.reports.lock().unwrap()[0],
            Report {
                working_directory: None,
                activity: None,
                state: Some(Lifecycle::Done),
                text: Some("3 findings".into()),
                result: Some(ReportResult {
                    summary: "see files".into(),
                    files: vec!["a.rs".into()],
                }),
                agent: Some(AgentInfo {
                    kind: "claude".into(),
                    agent_session_id: Some("abc".into()),
                    agent_session_path: None,
                }),
                resume: Some(vec!["claude".into(), "--resume".into(), "abc".into()]),
            }
        );
    }

    #[test]
    fn report_refuses_empty_or_inconsistent_flags() {
        for args in [
            ReportArgs::default(),
            ReportArgs {
                resume: true,
                ..ReportArgs::default()
            },
            ReportArgs {
                text: Some("x".into()),
                resume_argv: vec!["claude".into()],
                ..ReportArgs::default()
            },
        ] {
            assert!(matches!(build_report(args), Err(CliError::Usage(_))));
        }
        let only_state = ReportArgs {
            state: Some(StateArg::Working),
            ..ReportArgs::default()
        };
        assert_eq!(
            build_report(only_state).unwrap(),
            Report {
                state: Some(Lifecycle::Working),
                ..Report::default()
            }
        );
    }
}
