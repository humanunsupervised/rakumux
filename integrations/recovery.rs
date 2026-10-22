//! How to bring back what a session ran when its terminal is lost (a
//! reboot, a crash, a restart that couldn't hand the terminals over).
//!
//! The daemon looks at each live session's foreground process now and then
//! ([`plan`]) and records a resume command in the registry:
//! - a coding agent it knows: the command that resumes the same
//!   conversation, run on revive (`ResumeSource::KnownAgent`);
//! - any other program: its command line, typed at the prompt on revive but
//!   not run (`ResumeSource::LastCommand`), since running an arbitrary
//!   command again may not be safe;
//! - a shell at its prompt: nothing (once it has stayed there a while).
//!
//! What an agent reports itself (`rakumux session report --resume`) wins
//! while that agent runs.
//!
//! Provider implementations live beside their hooks in `integrations/<agent>`.
//! Agents:
//! - **Claude Code** writes `<config dir>/sessions/<pid>.json` with the id
//!   of the conversation that process runs (`sessionId`): `claude <flags>
//!   --resume <id>`.
//! - **Codex** keeps the rollout file of its thread open
//!   (`$CODEX_HOME/sessions/…/rollout-…-<id>.jsonl`; Linux reads the
//!   process's descriptors); otherwise the newest interactive thread of the
//!   folder in `state_<n>.sqlite`: `codex resume <flags> <id>`.
//! - **OpenCode**: the conversation its Rakumux plugin bound to the
//!   session (`session_bind`); otherwise the newest top-level session of the
//!   folder in `opencode.db`: `opencode <flags> -s <id>`.
//!
//! Each keeps the flags of the running process that shape an interactive
//! session (permissions, model, …) and drops the rest (a prompt, `--print`,
//! `--continue`, an older resume).

use crate::host::ProcessInfo;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[path = "claude-code/recovery.rs"]
mod claude;
#[path = "codex/recovery.rs"]
mod codex;
#[path = "open-code/recovery.rs"]
mod opencode;
#[cfg(test)]
use claude::claude_flags;
use claude::{claude_config_dir, claude_resume};
use codex::codex_resume;
use opencode::opencode_resume;

/// What to record for a session's foreground process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Run this on revive: it resumes the agent's conversation.
    Resume(Vec<String>),
    /// An agent whose conversation isn't known (yet): keep what's recorded.
    Unresolved,
    /// Another program: type this command line on revive, without running
    /// it.
    Command(Vec<String>),
    /// A shell at its prompt: nothing to bring back.
    Shell,
}

/// What [`plan`] looks at.
pub struct Context<'a> {
    /// The PTY's foreground process (group leader).
    pub process: &'a ProcessInfo,
    /// The directory the agent runs in.
    pub cwd: Option<&'a Path>,
    /// The newest OpenCode conversation the session bound.
    pub opencode_conversation: Option<&'a str>,
}

/// The plan for a session whose foreground process is `ctx.process`.
pub fn plan(ctx: &Context<'_>) -> Plan {
    let process = ctx.process;
    let resumed = match crate::detect::agent_label(process) {
        Some("claude") => Some(claude_config_dir().and_then(|dir| claude_resume(process, &dir))),
        Some("codex") => Some(codex_resume(process, ctx.cwd)),
        Some("opencode") => Some(opencode_resume(process, ctx.cwd, ctx.opencode_conversation)),
        _ => None,
    };
    match resumed {
        Some(Some(argv)) => Plan::Resume(argv),
        Some(None) => Plan::Unresolved,
        None if crate::detect::is_shell(process) => Plan::Shell,
        None => match command_line(process) {
            Some(argv) => Plan::Command(argv),
            None => Plan::Unresolved,
        },
    }
}

/// The command line as typed: the arguments, or the words of a command line
/// a program rewrote into one string.
fn command_line(process: &ProcessInfo) -> Option<Vec<String>> {
    let argv: Vec<String> = match process.cmdline.as_slice() {
        [] => vec![process.name.clone()],
        [only] => only.split_whitespace().map(String::from).collect(),
        all => all.to_vec(),
    };
    (!argv.is_empty() && !argv[0].is_empty()).then_some(argv)
}

fn is_conversation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Another program's SQLite database, read-only, without waiting long for
/// its writers.
fn open_read_only(path: &Path) -> Option<Connection> {
    if !path.is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(Duration::from_millis(200)).ok()?;
    Some(conn)
}

/// `<dir>/<prefix><n><suffix>` with the highest `n`.
fn newest_numbered(dir: &Path, prefix: &str, suffix: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let n: u32 = name
                .strip_prefix(prefix)?
                .strip_suffix(suffix)?
                .parse()
                .ok()?;
            Some((n, dir.join(name)))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, path)| path)
}

/// The arguments after `program` in a command line: after the program
/// itself, after the script a runtime runs (`node …/bin/codex`), or the
/// words of a command line rewritten into one string.
fn program_args(cmdline: &[String], program: &str) -> Vec<String> {
    if let [only] = cmdline {
        return only.split_whitespace().skip(1).map(String::from).collect();
    }
    let is_program = |word: &str| {
        let lower = word.to_ascii_lowercase();
        let base = lower.rsplit('/').next().unwrap_or(&lower);
        let stem = base.split('.').next().unwrap_or(base);
        stem == program
            || lower.contains(&format!("{program}-code"))
            || lower.contains(&format!("/{program}/"))
    };
    let start = cmdline.iter().position(|w| is_program(w)).unwrap_or(0);
    cmdline.iter().skip(start + 1).cloned().collect()
}

/// The flags of `switches` (no value), `options` (one value) and `lists`
/// (every value that follows) in `args`, in order; everything else is
/// dropped.
fn keep_flags(args: &[String], switches: &[&str], options: &[&str], lists: &[&str]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut words = args.iter().peekable();
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (word.as_str(), None),
        };
        if switches.contains(&flag) && inline.is_none() {
            kept.push(word.clone());
        } else if options.contains(&flag) || lists.contains(&flag) {
            if inline.is_some() {
                kept.push(word.clone());
                continue;
            }
            let many = lists.contains(&flag);
            let mut values = Vec::new();
            while let Some(value) = words.next_if(|v| !v.starts_with('-')) {
                values.push(value.clone());
                if !many {
                    break;
                }
            }
            if !values.is_empty() {
                kept.push(word.clone());
                kept.extend(values);
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn claude(pid: u32, cmdline: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: "claude".into(),
            cmdline: args(cmdline),
        }
    }

    #[test]
    fn claude_launched_as_a_shell_script_keeps_its_conversation_and_flags() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("sessions")).unwrap();
        std::fs::write(
            directory.path().join("sessions/42.json"),
            r#"{"pid":42,"sessionId":"conv-123"}"#,
        )
        .unwrap();
        let process = ProcessInfo {
            pid: 42,
            name: "sh".into(),
            cmdline: args(&[
                "/bin/sh",
                "/var/folders/tmp/bin/claude",
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "fix the bug",
            ]),
        };
        assert_eq!(crate::detect::agent_label(&process), Some("claude"));
        assert_eq!(
            claude_resume(&process, directory.path()),
            Some(args(&[
                "claude",
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "--resume",
                "conv-123"
            ]))
        );
    }

    #[test]
    fn opencode_v2_started_as_opencode2_resumes_as_opencode2() {
        let plan_for = |name: &str, cmdline: &[&str]| {
            let process = ProcessInfo {
                pid: 7,
                name: name.into(),
                cmdline: args(cmdline),
            };
            plan(&Context {
                process: &process,
                cwd: None,
                opencode_conversation: Some("ses_abc123"),
            })
        };
        // `opencode` may be a different (1.x) install earlier on PATH.
        assert_eq!(
            plan_for(
                "opencode2",
                &["opencode2", "-m", "openai/gpt-5", "--print-logs"]
            ),
            Plan::Resume(args(&[
                "opencode2",
                "-m",
                "openai/gpt-5",
                "-s",
                "ses_abc123"
            ]))
        );
        assert_eq!(
            plan_for("opencode", &["/home/me/.opencode/bin/opencode"]),
            Plan::Resume(args(&["opencode", "-s", "ses_abc123"]))
        );
    }

    #[test]
    fn session_flags_are_kept_and_the_rest_dropped() {
        assert_eq!(
            claude_flags(&args(&[
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "--resume",
                "old-id",
                "--add-dir",
                "/a",
                "/b",
                "--permission-mode=plan",
                "-c",
                "--print",
                "fix the bug",
            ])),
            args(&[
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "--add-dir",
                "/a",
                "/b",
                "--permission-mode=plan",
            ])
        );
        // A prompt after a one-value option isn't taken along.
        assert_eq!(
            claude_flags(&args(&["--model", "opus", "hello"])),
            args(&["--model", "opus"])
        );
    }

    #[test]
    fn arguments_follow_the_claude_program() {
        assert_eq!(
            program_args(
                &args(&["/usr/bin/node", "/x/claude-code/cli.js", "--ide"]),
                "claude"
            ),
            args(&["--ide"])
        );
        assert_eq!(
            program_args(
                &args(&["/home/me/.local/bin/claude", "--verbose"]),
                "claude"
            ),
            args(&["--verbose"])
        );
        assert_eq!(
            program_args(&args(&["claude --dangerously-skip-permissions"]), "claude"),
            args(&["--dangerously-skip-permissions"])
        );
        assert_eq!(
            program_args(
                &args(&[
                    "node",
                    "/n/lib/node_modules/@openai/codex/bin/codex.js",
                    "--yolo"
                ]),
                "codex"
            ),
            args(&["--yolo"])
        );
    }

    #[test]
    fn the_conversation_comes_from_the_session_file_of_that_pid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        std::fs::write(
            dir.path().join("sessions/42.json"),
            r#"{"pid":42,"sessionId":"daea4b72-d49c-4568","cwd":"/w","status":"idle"}"#,
        )
        .unwrap();
        let process = claude(
            42,
            &["claude", "--dangerously-skip-permissions", "--continue"],
        );
        assert_eq!(
            claude_resume(&process, dir.path()),
            Some(args(&[
                "claude",
                "--dangerously-skip-permissions",
                "--resume",
                "daea4b72-d49c-4568",
            ]))
        );
        // Another process, or a file that names another pid or a strange id.
        assert_eq!(claude_resume(&claude(43, &["claude"]), dir.path()), None);
        std::fs::write(
            dir.path().join("sessions/42.json"),
            r#"{"pid":7,"sessionId":"abc"}"#,
        )
        .unwrap();
        assert_eq!(claude_resume(&process, dir.path()), None);
        std::fs::write(
            dir.path().join("sessions/42.json"),
            r#"{"sessionId":"abc; rm -rf ~"}"#,
        )
        .unwrap();
        assert_eq!(claude_resume(&process, dir.path()), None);
    }
}
