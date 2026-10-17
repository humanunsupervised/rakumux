//! The OpenCode plugin (`mcp/opencode/orchestra-session/tui.js`), run by Node
//! against a fake OpenCode TUI API and a fake `orchestra` binary. Skipped when
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
let listener;
const api = {
  ui: { router: { current: () => route } },
  data: {
    session: {
      root,
      family: (id) => Object.keys(parents).filter((k) => root(k) === id),
    },
    listen: (handler) => { listener = handler; return () => { listener = undefined; }; },
  },
};
const dispose = plugin.setup(api);
await sleep(400);
route = { type: "session", sessionID: "ses_child" };
await sleep(600);
if (listener) {
  parents.ses_new = "ses_root";
  listener({ details: { type: "session.created", data: { sessionID: "ses_new", parentID: "ses_root" } } });
  listener({ details: { type: "session.created", data: { sessionID: "ses_other" } } });
}
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

/// Run the plugin with `env`; the fake `orchestra` logs its arguments.
fn run(dir: &Path, env: &[(&str, &str)]) -> Vec<String> {
    let log = dir.join("calls.log");
    let bin = dir.join("orchestra");
    std::fs::write(&bin, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LOG\"\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let harness = dir.join("harness.mjs");
    std::fs::write(&harness, HARNESS).unwrap();
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("opencode/orchestra-session/tui.js");
    let mut command = Command::new("node");
    command
        .arg(&harness)
        .arg(&plugin)
        .env_remove("ORCHESTRA_SESSION_ID")
        .env_remove("ORCHESTRA_SESSION_TOKEN")
        .env("ORCHESTRA_BIN", &bin)
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
            ("ORCHESTRA_SESSION_ID", "01JSELF"),
            ("ORCHESTRA_SESSION_TOKEN", "tok"),
        ],
    );
    assert_eq!(
        calls,
        vec![
            "session bind --agent opencode ses_root ses_child ses_grand",
            "session bind --agent opencode ses_new",
        ]
    );
}

#[test]
fn outside_an_orchestra_session_the_plugin_does_nothing() {
    if node().is_none() {
        eprintln!("skipped: node is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    assert!(run(dir.path(), &[]).is_empty());
}
