//! Child environment, program and startup input for a session's terminal.
//!
//! The daemon's own environment goes stale, so a session inherits the
//! **requesting client's** environment snapshot, minus multiplexer and agent
//! markers that belong to the client's own context, plus Orchestra's session
//! identity.

use session_core::api::{ApiError, ErrorCode};
use session_core::env as orchestra_env;
use session_core::ids::{SessionId, Token};
use std::collections::BTreeMap;
use std::path::Path;

/// Variables removed from the inherited environment.
const STRIPPED: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CODEX_THREAD_ID",
    "OMPCODE",
    // Replaced below with this session's values.
    orchestra_env::SOCKET,
    "TERM",
    "COLORTERM",
];

/// Prefixes removed from the inherited environment (a parent session's
/// identity must never leak into a child).
const STRIPPED_PREFIXES: &[&str] = &["ORCHESTRA_SESSION_"];

/// Fallback when neither the request nor the daemon knows a shell.
pub(crate) const FALLBACK_SHELL: &str = "/bin/sh";

/// Values injected into every session.
pub(crate) struct Identity<'a> {
    pub session_id: &'a SessionId,
    pub token: &'a Token,
    pub socket: &'a Path,
    /// Daemon-configured `ORCHESTRA_BIN`; when None, the request's value (if
    /// any) is kept.
    pub orchestra_bin: Option<&'a Path>,
}

/// Validate a request environment: names must be non-empty and free of `=`
/// and NUL; values free of NUL.
pub(crate) fn validate(env: &[(String, String)]) -> Result<(), ApiError> {
    for (name, value) in env {
        if name.is_empty() || name.contains('=') || name.contains('\0') {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                format!("invalid environment variable name {name:?}"),
            ));
        }
        if value.contains('\0') {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                format!("environment variable {name} contains a NUL byte"),
            ));
        }
    }
    Ok(())
}

/// Terminal size used when a create request leaves it at 0 and the caller
/// has no terminal to copy it from.
pub(crate) const DEFAULT_COLS: u16 = 120;
pub(crate) const DEFAULT_ROWS: u16 = 40;

/// The environment a new session inherits, before identity is injected:
/// - the request's snapshot when it has one;
/// - for a session caller sending none, the caller session's own spawn
///   environment (held in daemon memory only), so agents creating sessions
///   needn't ship their environment;
/// - otherwise the daemon's own environment, so the program still gets
///   `PATH`, `HOME`, ….
pub(crate) fn inherited_env(
    request: &[(String, String)],
    caller_session_env: Option<&[(String, String)]>,
) -> Vec<(String, String)> {
    if !request.is_empty() {
        return request.to_vec();
    }
    if let Some(env) = caller_session_env {
        return env.to_vec();
    }
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// Requested size, where 0 means "the calling session's terminal size",
/// else [`DEFAULT_COLS`]×[`DEFAULT_ROWS`]. Each dimension is resolved
/// separately.
pub(crate) fn spawn_size(cols: u16, rows: u16, caller_size: Option<(u16, u16)>) -> (u16, u16) {
    let (caller_cols, caller_rows) = caller_size
        .filter(|(c, r)| *c > 0 && *r > 0)
        .unwrap_or((DEFAULT_COLS, DEFAULT_ROWS));
    (
        if cols == 0 { caller_cols } else { cols },
        if rows == 0 { caller_rows } else { rows },
    )
}

/// The environment a session's program starts with: `base` (see
/// [`inherited_env`]) minus stripped variables, plus this session's
/// identity, `TERM` and `COLORTERM`. Later duplicates in `base` win.
pub(crate) fn child_env(
    base: &[(String, String)],
    identity: &Identity<'_>,
) -> Vec<(String, String)> {
    let mut env: BTreeMap<String, String> = base.iter().cloned().collect();
    env.retain(|name, _| {
        !STRIPPED.contains(&name.as_str()) && !STRIPPED_PREFIXES.iter().any(|p| name.starts_with(p))
    });
    env.insert(
        orchestra_env::SESSION_ID.into(),
        identity.session_id.as_str().into(),
    );
    env.insert(
        orchestra_env::SESSION_TOKEN.into(),
        identity.token.as_str().into(),
    );
    env.insert(
        orchestra_env::SOCKET.into(),
        identity.socket.to_string_lossy().into_owned(),
    );
    if let Some(bin) = identity.orchestra_bin {
        env.insert(
            orchestra_env::BIN.into(),
            bin.to_string_lossy().into_owned(),
        );
    }
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.into_iter().collect()
}

/// `[$SHELL, "-l"]`: `SHELL` from the inherited environment, else the
/// daemon's, else `/bin/sh`.
pub(crate) fn login_shell(base: &[(String, String)]) -> Vec<String> {
    let from_request = base
        .iter()
        .rev()
        .find(|(name, value)| name == "SHELL" && !value.is_empty())
        .map(|(_, value)| value.clone());
    let shell = from_request
        .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| FALLBACK_SHELL.to_string());
    vec![shell, "-l".to_string()]
}

/// Bytes typed into the terminal after the program starts: each startup
/// command followed by `\r`, then the shell-quoted resume command, followed
/// by `\r` when it is to run (`(argv, run)`; a last command is only typed).
pub(crate) fn startup_input(startup_cmds: &[String], resume: Option<(&[String], bool)>) -> Vec<u8> {
    let resumed = resume
        .filter(|(_, run)| *run)
        .and_then(|(argv, _)| argv.first())
        .map(|p| program_name(p));
    let mut out = Vec::new();
    for cmd in startup_cmds {
        // The resume command replaces a startup command that starts the same
        // program (e.g. `claude`); setup commands (venv, cd) still run first.
        let first = cmd.split_whitespace().next().map(program_name);
        if resumed.is_some() && first == resumed {
            continue;
        }
        out.extend_from_slice(cmd.as_bytes());
        out.push(b'\r');
    }
    if let Some((argv, run)) = resume {
        let line: Vec<String> = argv.iter().map(|arg| shell_quote(arg)).collect();
        out.extend_from_slice(line.join(" ").as_bytes());
        if run {
            out.push(b'\r');
        }
    }
    out
}

/// Some startup command starts the program `argv` runs.
pub(crate) fn starts_program(startup_cmds: &[String], argv: &[String]) -> bool {
    let Some(program) = argv.first().map(|p| program_name(p)) else {
        return false;
    };
    startup_cmds
        .iter()
        .any(|cmd| cmd.split_whitespace().next().map(program_name) == Some(program))
}

/// Executable name without its directory: `/usr/bin/claude` → `claude`.
pub(crate) fn program_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// POSIX shell quoting: safe words stay as they are, everything else is
/// single-quoted.
pub(crate) fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c));
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn child_env_strips_markers_and_injects_identity() {
        let id = SessionId::from_string("S1");
        let token = Token::from_string("tok");
        let identity = Identity {
            session_id: &id,
            token: &token,
            socket: Path::new("/tmp/o.sock"),
            orchestra_bin: Some(Path::new("/opt/orchestra")),
        };
        let request = pairs(&[
            ("PATH", "/bin"),
            ("TMUX", "x"),
            ("TMUX_PANE", "%1"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "c"),
            ("CLAUDE_CODE_CHILD_SESSION", "1"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "m"),
            ("CODEX_THREAD_ID", "t"),
            ("OMPCODE", "1"),
            ("ORCHESTRA_SESSION_ID", "PARENT"),
            ("ORCHESTRA_SESSION_TOKEN", "parent-token"),
            ("ORCHESTRA_SESSION_FUTURE", "x"),
            ("ORCHESTRA_BIN", "/old"),
            ("ORCHESTRA_HOME", "/h"),
            ("TERM", "screen"),
        ]);
        let env: BTreeMap<_, _> = child_env(&request, &identity).into_iter().collect();
        let expected = pairs(&[
            ("COLORTERM", "truecolor"),
            ("ORCHESTRA_BIN", "/opt/orchestra"),
            ("ORCHESTRA_HOME", "/h"),
            ("ORCHESTRA_SESSION_ID", "S1"),
            ("ORCHESTRA_SESSION_TOKEN", "tok"),
            ("ORCHESTRA_SOCKET", "/tmp/o.sock"),
            ("PATH", "/bin"),
            ("TERM", "xterm-256color"),
        ]);
        assert_eq!(env.into_iter().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn request_orchestra_bin_kept_when_daemon_has_none() {
        let id = SessionId::from_string("S1");
        let token = Token::from_string("tok");
        let identity = Identity {
            session_id: &id,
            token: &token,
            socket: Path::new("/s"),
            orchestra_bin: None,
        };
        let env = child_env(&pairs(&[("ORCHESTRA_BIN", "/client/orchestra")]), &identity);
        assert!(env.contains(&("ORCHESTRA_BIN".into(), "/client/orchestra".into())));
    }

    #[test]
    fn login_shell_prefers_the_request() {
        assert_eq!(
            login_shell(&pairs(&[("SHELL", "/usr/bin/zsh")])),
            ["/usr/bin/zsh", "-l"]
        );
        assert_eq!(login_shell(&[]).len(), 2);
    }

    #[test]
    fn startup_input_types_commands_then_quoted_resume() {
        let cmds = [
            "cd src".to_string(),
            "/usr/local/bin/claude --model x".to_string(),
            "ls".to_string(),
        ];
        assert_eq!(
            startup_input(&cmds, None),
            b"cd src\r/usr/local/bin/claude --model x\rls\r".to_vec()
        );
        // The startup command that starts the resumed program is replaced.
        let resume: Vec<String> = vec!["claude".into(), "--resume".into(), "it's 1".into()];
        let input = startup_input(&cmds, Some((&resume, true)));
        assert_eq!(
            input,
            b"cd src\rls\rclaude --resume 'it'\\''s 1'\r".to_vec()
        );
        // A last command is typed but not run, and replaces nothing.
        let last: Vec<String> = vec!["npm".into(), "run".into(), "dev".into()];
        assert_eq!(
            startup_input(&cmds, Some((&last, false))),
            b"cd src\r/usr/local/bin/claude --model x\rls\rnpm run dev".to_vec()
        );
        assert!(starts_program(&cmds, &resume));
        assert!(!starts_program(&cmds, &last));
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a/b.c-d_e"), "a/b.c-d_e");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
    }

    #[test]
    fn empty_session_snapshot_inherits_the_caller_sessions_env() {
        let parent_spawn_env = pairs(&[
            ("PATH", "/parent/bin"),
            ("ORCHESTRA_SESSION_ID", "PARENT"),
            ("ORCHESTRA_SESSION_TOKEN", "parent-token"),
            ("CLAUDECODE", "1"),
        ]);
        // A session caller that sent no snapshot inherits its own spawn env…
        let base = inherited_env(&[], Some(&parent_spawn_env));
        let id = SessionId::from_string("CHILD");
        let token = Token::from_string("child-token");
        let identity = Identity {
            session_id: &id,
            token: &token,
            socket: Path::new("/s"),
            orchestra_bin: None,
        };
        let env: BTreeMap<_, _> = child_env(&base, &identity).into_iter().collect();
        assert_eq!(env["PATH"], "/parent/bin");
        assert_eq!(env["ORCHESTRA_SESSION_ID"], "CHILD");
        assert_eq!(env["ORCHESTRA_SESSION_TOKEN"], "child-token");
        assert!(!env.contains_key("CLAUDECODE"));
        // …an explicit snapshot wins…
        let explicit = pairs(&[("PATH", "/explicit")]);
        assert_eq!(inherited_env(&explicit, Some(&parent_spawn_env)), explicit);
        // …and user callers without one get the daemon's environment.
        let daemon = inherited_env(&[], None);
        assert_eq!(daemon.is_empty(), std::env::vars_os().next().is_none());
    }

    #[test]
    fn zero_size_means_caller_size_else_default() {
        assert_eq!(spawn_size(100, 30, Some((200, 50))), (100, 30));
        assert_eq!(spawn_size(0, 0, Some((200, 50))), (200, 50));
        assert_eq!(spawn_size(0, 30, None), (DEFAULT_COLS, 30));
        assert_eq!(spawn_size(0, 0, None), (DEFAULT_COLS, DEFAULT_ROWS));
        assert_eq!(spawn_size(0, 0, Some((0, 0))), (DEFAULT_COLS, DEFAULT_ROWS));
    }

    #[test]
    fn invalid_names_and_values_are_rejected() {
        assert!(validate(&pairs(&[("A", "1")])).is_ok());
        assert!(validate(&pairs(&[("", "1")])).is_err());
        assert!(validate(&pairs(&[("A=B", "1")])).is_err());
        assert!(validate(&pairs(&[("A", "x\0y")])).is_err());
    }
}
