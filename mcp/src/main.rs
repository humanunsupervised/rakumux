//! `orchestra-mcp`: Orchestra's MCP server for coding agents (stdio). See
//! `mcp/README.md` for installing it in Claude Code, Codex and OpenCode.

use orchestra_mcp::client::{ApiClient, SessionIdentity};
use orchestra_mcp::launch::Launcher;
use orchestra_mcp::opencode;
use orchestra_mcp::server::OrchestraServer;
use orchestra_mcp::VERSION;
use session_core::paths::Layout;
use std::process::ExitCode;

const USAGE: &str = "\
orchestra-mcp: Orchestra's MCP server (stdio) for coding agents.

Usage: orchestra-mcp [--version | --help]
       orchestra-mcp opencode-plugin [--install [--dir DIR]]

Speaks MCP on stdin/stdout. Register it with your agent, e.g.
  claude mcp add --scope user orchestra -- orchestra-mcp

OpenCode 2.x also needs the Orchestra plugin in its TUI, so tool calls act as
the Orchestra session the conversation runs in: `opencode-plugin` prints it,
`--install` writes it to DIR/plugins/orchestra-session/tui.js (DIR: OpenCode's
config directory, default $XDG_CONFIG_HOME/opencode or ~/.config/opencode),
where OpenCode loads it on start. No OpenCode config file is changed.

It drives orchestra-api (found through <ORCHESTRA_HOME or ~/.orchestra>/api.json,
started with `orchestra-api --detach` when needed; ORCHESTRA_API_BIN overrides the
binary). Inside an Orchestra session, ORCHESTRA_SESSION_ID and ORCHESTRA_SESSION_TOKEN
make it act as that session.";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("opencode-plugin") => return opencode_plugin(std::env::args().skip(2).collect()),
        Some("--version" | "-V") => {
            println!("orchestra-mcp {VERSION}");
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("orchestra-mcp: unknown argument {other:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    let layout = match Layout::from_env() {
        Ok(layout) => layout,
        Err(error) => {
            eprintln!("orchestra-mcp: {error}");
            return ExitCode::FAILURE;
        }
    };
    let api = match ApiClient::new(layout, SessionIdentity::from_env(), Launcher::from_env()) {
        Ok(api) => api,
        Err(error) => {
            eprintln!("orchestra-mcp: {}", error.message);
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("orchestra-mcp: cannot start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(OrchestraServer::new(api, VERSION).serve_stdio()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("orchestra-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `orchestra-mcp opencode-plugin [--install [--dir DIR]]`.
fn opencode_plugin(args: Vec<String>) -> ExitCode {
    let mut install = false;
    let mut dir = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--install" => install = true,
            "--dir" => match args.next() {
                Some(value) => dir = Some(std::path::PathBuf::from(value)),
                None => {
                    eprintln!("orchestra-mcp: --dir needs a directory");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("orchestra-mcp: unknown argument {other:?}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    if !install {
        if dir.is_some() {
            eprintln!("orchestra-mcp: --dir needs --install");
            return ExitCode::from(2);
        }
        print!("{}", opencode::PLUGIN);
        return ExitCode::SUCCESS;
    }
    let Some(dir) = dir.or_else(opencode::config_dir) else {
        eprintln!("orchestra-mcp: can't find OpenCode's config directory; pass --dir");
        return ExitCode::FAILURE;
    };
    match opencode::install(&dir) {
        Ok(path) => {
            println!("Wrote {}.", path.display());
            println!("Restart OpenCode (2.x) to load it.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("orchestra-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}
