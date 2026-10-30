//! `rakumux-mcp`: Rakumux's MCP server for coding agents (stdio). See
//! `mcp/README.md` for installing it in Claude Code, Codex and OpenCode.

use rakumux_mcp::client::{ApiClient, EnvIdentity};
use rakumux_mcp::launch::Launcher;
use rakumux_mcp::opencode;
use rakumux_mcp::server::RakumuxServer;
use rakumux_mcp::VERSION;
use session_core::paths::Layout;
use std::process::ExitCode;

const USAGE: &str = "\
rakumux-mcp: Rakumux's MCP server (stdio) for coding agents.

Usage: rakumux-mcp [--version | --help]
       rakumux-mcp opencode-plugin [--install [--dir DIR]]

Speaks MCP on stdin/stdout. Register it with your agent, e.g.
  claude mcp add --scope user rakumux -- rakumux-mcp

OpenCode 2.x also needs the Rakumux plugin in its TUI, so tool calls act as
the Rakumux session the conversation runs in: `opencode-plugin` prints it,
`--install` writes it to DIR/plugins/rakumux-session/tui.js (DIR: OpenCode's
config directory, default $XDG_CONFIG_HOME/opencode or ~/.config/opencode),
where OpenCode loads it on start. No OpenCode config file is changed.

It drives rakumux-api (found through <RAKUMUX_HOME or ~/.rakumux>/api.json,
started with `rakumux-api --detach` when needed; RAKUMUX_API_BIN overrides the
binary). Inside a Rakumux session, RAKUMUX_SESSION_ID and RAKUMUX_SESSION_TOKEN
make it act as that session; with only one of them, every call is refused.";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("opencode-plugin") => {
            return opencode::command(std::env::args().skip(2).collect(), USAGE)
        }
        Some("--version" | "-V") => {
            println!("rakumux-mcp {VERSION}");
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("rakumux-mcp: unknown argument {other:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    let layout = match Layout::from_env() {
        Ok(layout) => layout,
        Err(error) => {
            eprintln!("rakumux-mcp: {error}");
            return ExitCode::FAILURE;
        }
    };
    let identity = EnvIdentity::from_env();
    if identity == EnvIdentity::Incomplete {
        eprintln!(
            "rakumux-mcp: only one of RAKUMUX_SESSION_ID and RAKUMUX_SESSION_TOKEN is set; \
             every call will be refused"
        );
    }
    let api = match ApiClient::new(layout, identity, Launcher::from_env()) {
        Ok(api) => api,
        Err(error) => {
            eprintln!("rakumux-mcp: {}", error.message);
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("rakumux-mcp: cannot start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(RakumuxServer::new(api, VERSION).serve_stdio()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rakumux-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}
