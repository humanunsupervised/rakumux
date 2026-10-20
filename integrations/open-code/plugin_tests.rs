//! The OpenCode plugin, run by Node
//! against a fake OpenCode TUI API and a fake `rakumux` binary. Skipped when
//! `node` isn't installed.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const HARNESS: &str = r#"
import { pathToFileURL } from "node:url";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const plugin = (await import(pathToFileURL(process.argv[2]).href)).default;
let route = { type: "home" };
const parents = { ses_child: "ses_root", ses_grand: "ses_child" };
const root = (id) => { while (parents[id]) id = parents[id]; return id; };
const api = {
  ui: { router: { current: () => route } },
  data: {
    session: {
      get: (id) => ({ id }),
      root,
      family: (id) => Object.keys(parents).filter((k) => root(k) === id),
      status: () => ({ type: "running" }),
      permission: { list: () => [] },
      form: { list: () => [] },
      message: { list: () => [] },
    },
  },
};
const dispose = plugin.setup(api);
await sleep(400);
route = { type: "session", sessionID: "ses_child" };
await sleep(600);
parents.ses_new = "ses_root";
await sleep(600);
dispose?.();
"#;

fn node() -> Option<String> {
    Command::new("node")
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|_| "node".to_owned())
}

/// Run the plugin with `env`; the fake `rakumux` logs its arguments.
fn run(dir: &Path, env: &[(&str, &str)]) -> Vec<String> {
    let log = dir.join("calls.log");
    let bin = dir.join("rakumux");
    std::fs::write(&bin, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LOG\"\ncase \"$1 $2\" in 'session hook') cat >> \"$LOG.payload\";; esac\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let harness = dir.join("harness.mjs");
    std::fs::write(&harness, HARNESS).unwrap();
    let plugin = rakumux_mcp::opencode::install(dir).unwrap();
    // A minimal test double for the documented Plugin.define entry point.
    let package = dir.join("node_modules/@opencode/plugin");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(dir.join("package.json"), r#"{"type":"module"}"#).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"type":"module","exports":{"./tui":"./tui.js"}}"#,
    )
    .unwrap();
    std::fs::write(
        package.join("tui.js"),
        "export const Plugin = { define: (value) => value };",
    )
    .unwrap();
    let mut command = Command::new("node");
    command
        .arg(&harness)
        .arg(&plugin)
        .env_remove("RAKUMUX_SESSION_ID")
        .env_remove("RAKUMUX_SESSION_TOKEN")
        .env("RAKUMUX_BIN", &bin)
        .env("LOG", &log);
    for (name, value) in env {
        command.env(name, value);
    }
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "harness failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_plugin_binds_the_open_conversation_and_its_subagents() {
    if node().is_none() {
        eprintln!("skipped: node is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let calls = run(
        dir.path(),
        &[
            ("RAKUMUX_SESSION_ID", "01JSELF"),
            ("RAKUMUX_SESSION_TOKEN", "tok"),
        ],
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("session bind"))
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "session bind --agent opencode ses_root ses_child ses_grand",
            "session bind --agent opencode ses_new",
        ]
    );
    assert!(calls
        .iter()
        .any(|call| call == "session hook --agent opencode"));
    let payload = std::fs::read_to_string(dir.path().join("calls.log.payload")).unwrap();
    assert!(payload.contains("\"phase\":\"working\""));
    assert!(payload.contains("ses_grand"));
}

#[test]
fn outside_an_rakumux_session_the_plugin_does_nothing() {
    if node().is_none() {
        eprintln!("skipped: node is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    assert!(run(dir.path(), &[]).is_empty());
}
