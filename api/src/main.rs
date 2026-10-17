//! `orchestra-api`: Orchestra's external HTTP API (see `api/README.md` and
//! `api/openapi.yaml`).

use clap::Parser;
use orchestra_api::server::{self, ServeOptions};
use orchestra_api::{detach, log, VERSION};
use session_core::paths::Layout;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

/// Orchestra's external API: HTTP/1.1 + JSON on 127.0.0.1 over the session
/// daemon, plus a remote listener when remote access is on (remote.json,
/// `orchestra remote on`). Writes <ORCHESTRA_HOME or ~/.orchestra>/api.json
/// (url, token, pid, version, remote_urls) while it runs.
#[derive(Debug, Parser)]
#[command(name = "orchestra-api", version = VERSION)]
struct Args {
    /// Port on 127.0.0.1 (0 picks a free port).
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Run in the background; return once the API answers /v1/health.
    #[arg(long)]
    detach: bool,
    /// orchestra-ptyd binary to start when the daemon isn't running.
    #[arg(long, value_name = "PATH")]
    ptyd_path: Option<PathBuf>,
    /// Serve remote access on ADDR:PORT instead of this machine's Tailscale
    /// addresses (repeatable). Needs remote access on (`orchestra remote on`).
    #[arg(long, value_name = "ADDR:PORT")]
    remote_listen: Vec<SocketAddr>,
}

fn main() -> ExitCode {
    // The API acts for the caller named by each request, never for a
    // session it happens to run in. Done before any thread exists.
    for name in [
        session_core::env::SESSION_ID,
        session_core::env::SESSION_TOKEN,
        session_core::env::SOCKET,
    ] {
        std::env::remove_var(name);
    }
    let args = Args::parse();
    log::init();
    let layout = match Layout::from_env() {
        Ok(layout) => layout,
        Err(error) => {
            eprintln!("orchestra-api: {error}");
            return ExitCode::FAILURE;
        }
    };
    let ptyd_path = args.ptyd_path.or_else(sibling_ptyd);

    if args.detach {
        return match detach::start(
            &layout,
            args.port,
            ptyd_path.as_deref(),
            &args.remote_listen,
        ) {
            Ok(info) => {
                println!("orchestra-api listening on {} (pid {})", info.url, info.pid);
                for url in &info.remote_urls {
                    println!("remote access on {url}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("orchestra-api: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("orchestra-api: cannot start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let options = ServeOptions {
        layout,
        port: args.port,
        ptyd_path,
        remote_listen: args.remote_listen,
    };
    match runtime.block_on(server::serve(options, server::shutdown_signal())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("orchestra-api: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `orchestra-ptyd` next to the real (symlink-resolved) executable, so an
/// API reached through a symlink in a bin directory still finds the daemon
/// it was installed with. `None` (also when `ORCHESTRA_PTYD_BIN` is set)
/// falls back to `paths::find_ptyd_binary`.
fn sibling_ptyd() -> Option<PathBuf> {
    if std::env::var_os(session_core::env::PTYD_BIN).is_some() {
        return None;
    }
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    let sibling = exe.parent()?.join("orchestra-ptyd");
    sibling.is_file().then_some(sibling)
}
