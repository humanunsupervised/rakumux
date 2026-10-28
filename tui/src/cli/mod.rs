//! Command-line subcommands of the `rakumux` binary (`tui`):
//! `new`, `ls`, `attach`, `rename`, `pin`, `unpin`, `kill`, `rm`,
//! `folders …` (alias `projects`), `session …` (JSON, for agents and
//! scripts), `daemon …` and `remote …` (remote access for the phone app).
//! Global options may precede the subcommand:
//! `--ptyd-path <PATH>`, `--services-path <PATH>` (ignored, legacy) and
//! `--debug`. With no subcommand the TUI starts.
//!
//! Every subcommand is a thin client of `session_core::client::SessionApi`.
//! Exit codes: 0 ok, 1 error (message on stderr; JSON for `session …`),
//! 2 usage.

mod account;
mod args;
mod attach;
mod commands;
mod daemon;
#[cfg(test)]
mod fake;
mod folders;
mod hooks;
mod remote;
mod session;
mod sprite;
mod table;

use args::{CliArgs, Command};
use clap::Parser;
use serde_json::json;
use session_core::api::{ApiError, CallerInfo, ErrorCode};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionView};
use session_core::paths::Layout;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Shown for `unsupported` errors: the daemon predates an operation.
const UNSUPPORTED_HINT: &str =
    "this Rakumux daemon is older; restart it with `rakumux daemon restart`";

/// Run a subcommand if `args` (including argv[0]) name one. Returns the
/// process exit code, or `None` when the TUI should start instead.
pub fn dispatch(args: &[String]) -> Option<i32> {
    args::subcommand_index(args)?;
    let cli = match CliArgs::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return Some(error.exit_code());
        }
    };
    Some(run(cli))
}

/// `rakumux --new-server`: replace the running daemon with a new one
/// (running sessions keep running when it can restart in place; an older
/// one is stopped after asking), or start one when none runs. Called before
/// the TUI takes over the terminal, so it may ask on stdin. Returns what
/// happened.
pub fn new_server(ptyd_path: Option<PathBuf>) -> Result<String, String> {
    let globals = Globals {
        ptyd_path,
        debug: false,
    };
    let result = layout().and_then(|layout| daemon::restart(&globals, &layout, false));
    result.map_err(|error| match error {
        CliError::Usage(message) => message,
        CliError::Api(error) => error.message,
        CliError::Exit(_) => "Kept the running server.".into(),
    })
}

fn run(cli: CliArgs) -> i32 {
    let json_errors = matches!(cli.command, Command::Session { .. });
    let debug = cli.debug;
    match execute(cli) {
        Ok(code) => code,
        Err(error) => report_error(error, json_errors, debug, &mut io::stderr()),
    }
}

fn execute(cli: CliArgs) -> Result<i32, CliError> {
    let globals = Globals {
        ptyd_path: cli.ptyd_path,
        debug: cli.debug,
    };
    let mut out = io::stdout();
    match cli.command {
        Command::Session {
            command: args::SessionCommand::Hook { agent },
        } => hooks::run(&globals, agent, &mut out),
        Command::Session {
            command: args::SessionCommand::HookConfig { agent },
        } => hooks::configuration(agent, &mut out),
        Command::Daemon { command } => daemon::run(&globals, command, &mut out),
        Command::Remote { command } => remote::run(&globals, command, &mut out),
        Command::Sprite { command } => {
            let client = globals.connect(true)?;
            sprite::run(&client, &layout()?, command, &mut out)
        }
        Command::Account { command } => {
            // The account service follows the daemon's sessions: start it first.
            let client = globals.connect(true)?;
            account::run(&client, command, &mut out)
        }
        Command::StatusService => account::serve(),
        command => {
            let invocation = Invocation::from_process()?;
            let client = globals.connect(true)?;
            if globals.debug {
                eprintln!(
                    "debug: connected to rakumux-ptyd {} as {:?}; cwd {}",
                    client.daemon_version(),
                    client.caller(),
                    invocation.cwd.display()
                );
            }
            commands::run(&client, &invocation, command, &mut out)
        }
    }
}

// ---------------------------------------------------------------- errors

/// Why a subcommand failed.
#[derive(Debug, PartialEq)]
pub(crate) enum CliError {
    /// Bad arguments: message on stderr, exit 2.
    Usage(String),
    /// The daemon (or a local step) failed: exit 1.
    Api(ApiError),
    /// Exit with this code without printing anything more.
    Exit(i32),
}

impl From<ApiError> for CliError {
    fn from(error: ApiError) -> Self {
        CliError::Api(error)
    }
}

impl CliError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        CliError::Api(ApiError::new(ErrorCode::InvalidArgument, message))
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        CliError::Api(ApiError::new(ErrorCode::Internal, message))
    }
}

/// Map an output failure: a closed pipe (`rakumux ls | head`) ends the
/// command quietly.
pub(crate) fn output_error(error: io::Error) -> CliError {
    if error.kind() == io::ErrorKind::BrokenPipe {
        CliError::Exit(1)
    } else {
        CliError::internal(format!("cannot write output: {error}"))
    }
}

/// Rewrite errors whose daemon message isn't actionable for a person.
fn present(mut error: ApiError) -> ApiError {
    if error.code == ErrorCode::Unsupported {
        let original = std::mem::replace(&mut error.message, UNSUPPORTED_HINT.into());
        let details = error.details.get_or_insert_with(|| json!({}));
        if let Some(map) = details.as_object_mut() {
            map.insert("daemon_message".into(), json!(original));
        }
    }
    error
}

/// Print `error` and return the exit code.
fn report_error(error: CliError, json_errors: bool, debug: bool, err: &mut dyn Write) -> i32 {
    match error {
        CliError::Exit(code) => code,
        CliError::Usage(message) => {
            let _ = writeln!(err, "rakumux: {message}");
            let _ = writeln!(err, "Run `rakumux help` for usage.");
            2
        }
        CliError::Api(error) => {
            let error = present(error);
            if json_errors {
                let _ = writeln!(err, "{}", json!({ "error": error }));
            } else {
                let _ = writeln!(err, "rakumux: {}", error.message);
                if debug {
                    let _ = writeln!(err, "debug: {error} {:?}", error.details);
                }
            }
            1
        }
    }
}

// ---------------------------------------------------------------- context

/// Options that apply to every subcommand.
pub(crate) struct Globals {
    pub ptyd_path: Option<PathBuf>,
    pub debug: bool,
}

impl Globals {
    /// Connect as "cli" (session auth inside a session, user auth
    /// otherwise), spawning the daemon when `spawn` and it isn't running.
    pub(crate) fn connect(&self, spawn: bool) -> Result<Client, CliError> {
        let layout = layout()?;
        Ok(Client::connect(ClientConfig {
            layout,
            client_name: "cli".into(),
            version: version().into(),
            auth: AuthMode::Auto,
            spawn,
            ptyd_path: self.ptyd_path.clone(),
        })?)
    }
}

pub(crate) fn layout() -> Result<Layout, CliError> {
    Layout::from_env().map_err(|error| CliError::internal(format!("no Rakumux home: {error}")))
}

fn version() -> &'static str {
    option_env!("RAKUMUX_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// The calling process: where it runs and what it would hand to a new
/// session.
#[derive(Debug, Clone, Default)]
pub(crate) struct Invocation {
    pub cwd: PathBuf,
    /// Environment snapshot for `CreateSession::env` (UTF-8 pairs only).
    pub env: Vec<(String, String)>,
    /// `RAKUMUX_SESSION_ID`: the session this process runs in.
    pub self_id: Option<SessionId>,
}

impl Invocation {
    fn from_process() -> Result<Self, CliError> {
        let cwd = std::env::current_dir().map_err(|error| {
            CliError::internal(format!("cannot read the current directory: {error}"))
        })?;
        let env = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        let self_id = std::env::var(session_core::env::SESSION_ID)
            .ok()
            .filter(|id| !id.is_empty())
            .map(SessionId::from_string);
        Ok(Self { cwd, env, self_id })
    }

    /// The scope to send with requests: the canonical cwd for user callers;
    /// `None` for session callers, whom the daemon confines to their own.
    pub(crate) fn request_scope(
        &self,
        api: &dyn SessionApi,
    ) -> Result<Option<ScopeRoot>, CliError> {
        match api.caller() {
            CallerInfo::User => ScopeRoot::resolve(&self.cwd).map(Some).map_err(|error| {
                CliError::internal(format!("cannot resolve {}: {error}", self.cwd.display()))
            }),
            CallerInfo::Session { .. } => Ok(None),
        }
    }

    /// The scope the caller sees (for display).
    pub(crate) fn display_scope(&self, api: &dyn SessionApi) -> Result<ScopeRoot, CliError> {
        match api.caller() {
            CallerInfo::Session { scope, .. } => Ok(scope.clone()),
            CallerInfo::User => self
                .request_scope(api)?
                .ok_or_else(|| CliError::internal("no scope")),
        }
    }

    /// `path` made absolute against the cwd.
    pub(crate) fn absolute(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    /// The folder a `<dir>` argument names: its canonical path, or, when
    /// the folder no longer exists, its absolute path with `.` and `..`
    /// resolved (its sessions can still be listed and the folder
    /// forgotten).
    pub(crate) fn folder_root(&self, dir: &Path) -> Result<ScopeRoot, CliError> {
        let path = self.absolute(dir);
        match ScopeRoot::resolve(&path) {
            Ok(scope) => Ok(scope),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(ScopeRoot::from_canonical(normalize(&path)))
            }
            Err(error) => Err(CliError::invalid(format!(
                "cannot resolve {}: {error}",
                path.display()
            ))),
        }
    }
}

/// Resolve `.` and `..` in an absolute path without the filesystem.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Find `<name|id>` among the caller's sessions (dead ones included): an
/// exact id first, then a unique name.
pub(crate) fn resolve_target(
    api: &dyn SessionApi,
    scope: Option<&ScopeRoot>,
    target: &str,
) -> Result<SessionView, CliError> {
    let sessions = api.list(scope, true)?;
    if let Some(session) = sessions.iter().find(|s| s.id.as_str() == target) {
        return Ok(session.clone());
    }
    let mut named: Vec<SessionView> = sessions.into_iter().filter(|s| s.name == target).collect();
    match named.len() {
        1 => Ok(named.remove(0)),
        0 => Err(CliError::Api(ApiError::new(
            ErrorCode::NotFound,
            match scope {
                Some(scope) => format!("no session `{target}` in {}", scope.path().display()),
                None => format!("no session `{target}`"),
            },
        ))),
        n => {
            let ids: Vec<&str> = named.iter().map(|s| s.id.as_str()).collect();
            Err(CliError::invalid(format!(
                "{n} sessions are named `{target}`; use the id ({})",
                ids.join(", ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{session, FakeApi};
    use super::*;

    #[test]
    fn dispatch_leaves_tui_invocations_alone() {
        let args = |line: &str| -> Vec<String> {
            std::iter::once("tui")
                .chain(line.split_whitespace())
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(dispatch(&args("")), None);
        assert_eq!(dispatch(&args("--debug --services-path /s")), None);
        assert_eq!(dispatch(&args("--help")), None);
        // Usage errors exit with 2 before anything connects.
        assert_eq!(dispatch(&args("kill")), Some(2));
        assert_eq!(dispatch(&args("session report --file x")), Some(2));
    }

    #[test]
    fn targets_resolve_by_id_then_unique_name() {
        let api = FakeApi::user(vec![
            session("01A", "api"),
            session("01B", "web"),
            session("01C", "web"),
            session("web", "other"),
        ]);
        assert_eq!(resolve_target(&api, None, "01A").unwrap().name, "api");
        assert_eq!(
            resolve_target(&api, None, "api").unwrap().id.as_str(),
            "01A"
        );
        // An exact id wins over names.
        assert_eq!(resolve_target(&api, None, "web").unwrap().name, "other");
        let api = FakeApi::user(vec![session("01B", "web"), session("01C", "web")]);
        match resolve_target(&api, None, "web") {
            Err(CliError::Api(error)) => {
                assert_eq!(error.code, ErrorCode::InvalidArgument);
                assert!(error.message.contains("01B, 01C"), "{}", error.message);
            }
            other => panic!("{other:?}"),
        }
        match resolve_target(&api, Some(&ScopeRoot::from_canonical("/p".into())), "nope") {
            Err(CliError::Api(error)) => {
                assert_eq!(error.code, ErrorCode::NotFound);
                assert_eq!(error.message, "no session `nope` in /p");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn errors_are_printed_as_text_or_json() {
        let mut err = Vec::new();
        let error = CliError::Api(ApiError::new(ErrorCode::NotFound, "no session `x`"));
        assert_eq!(report_error(error, false, false, &mut err), 1);
        assert_eq!(String::from_utf8(err).unwrap(), "rakumux: no session `x`\n");

        let mut err = Vec::new();
        let error = CliError::Api(ApiError::new(ErrorCode::OutOfScope, "elsewhere"));
        assert_eq!(report_error(error, true, false, &mut err), 1);
        let value: serde_json::Value = serde_json::from_slice(&err).unwrap();
        assert_eq!(value["error"]["code"], "out_of_scope");
        assert_eq!(value["error"]["message"], "elsewhere");

        let mut err = Vec::new();
        assert_eq!(
            report_error(CliError::Usage("bad".into()), true, false, &mut err),
            2
        );
        assert!(String::from_utf8(err)
            .unwrap()
            .starts_with("rakumux: bad\n"));
        assert_eq!(
            report_error(CliError::Exit(1), false, false, &mut Vec::new()),
            1
        );
    }

    #[test]
    fn unsupported_errors_suggest_a_daemon_restart() {
        let mut err = Vec::new();
        let error = CliError::Api(ApiError::new(ErrorCode::Unsupported, "unknown op foo"));
        assert_eq!(report_error(error, false, false, &mut err), 1);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            format!("rakumux: {UNSUPPORTED_HINT}\n")
        );

        let mut err = Vec::new();
        let error = CliError::Api(ApiError::new(ErrorCode::Unsupported, "unknown op foo"));
        report_error(error, true, false, &mut err);
        let value: serde_json::Value = serde_json::from_slice(&err).unwrap();
        assert_eq!(value["error"]["code"], "unsupported");
        assert_eq!(value["error"]["message"], UNSUPPORTED_HINT);
        assert_eq!(
            value["error"]["details"]["daemon_message"],
            "unknown op foo"
        );
    }

    #[test]
    fn user_callers_send_their_cwd_as_scope() {
        let dir = tempfile::tempdir().unwrap();
        let invocation = Invocation {
            cwd: dir.path().to_path_buf(),
            ..Invocation::default()
        };
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let user = FakeApi::user(vec![]);
        assert_eq!(
            invocation.request_scope(&user).unwrap(),
            Some(ScopeRoot::from_canonical(canonical))
        );
        let inside = FakeApi::session_caller("S1", "/elsewhere", vec![]);
        assert_eq!(invocation.request_scope(&inside).unwrap(), None);
        assert_eq!(
            invocation.display_scope(&inside).unwrap(),
            ScopeRoot::from_canonical("/elsewhere".into())
        );
        assert_eq!(
            invocation.absolute(Path::new("sub")),
            dir.path().join("sub")
        );
        assert_eq!(invocation.absolute(Path::new("/x")), PathBuf::from("/x"));
    }

    #[test]
    fn folder_arguments_resolve_to_canonical_or_normalized_paths() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(canonical.join("app")).unwrap();
        let invocation = Invocation {
            cwd: canonical.join("app"),
            ..Invocation::default()
        };
        let root = |dir: &str| {
            invocation
                .folder_root(Path::new(dir))
                .unwrap()
                .path()
                .to_path_buf()
        };
        assert_eq!(root("."), canonical.join("app"));
        assert_eq!(root("../app/"), canonical.join("app"));
        // A folder that is gone keeps its (normalized) absolute path.
        assert_eq!(root("../gone/./x/.."), canonical.join("gone"));
        assert_eq!(root("/gone/a/../b"), PathBuf::from("/gone/b"));
    }
}
