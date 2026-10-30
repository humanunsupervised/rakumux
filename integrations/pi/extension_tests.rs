//! The Pi extension, run by Node against a fake Pi extension API and a fake
//! `rakumux` binary. Skipped when `node` isn't installed.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const HARNESS: &str = r#"
import { pathToFileURL } from "node:url";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const factory = (await import(pathToFileURL(process.argv[2]).href)).default;
const mode = process.argv[3];
const handlers = {};
const pi = { on: (name, handler) => { (handlers[name] ??= []).push(handler); } };
factory(pi);
let id = "01a18a95-6f2e-7c3b-9a1d-2cd72115b9d0";
const ctx = () => ({
  mode, cwd: "/launch", isIdle: () => true,
  sessionManager: { getSessionId: () => id, getCwd: () => "/repo" },
});
const emit = async (name, event = {}) => {
  for (const handler of handlers[name] ?? []) await handler({ type: name, ...event }, ctx());
};
await emit("session_start", { reason: "startup" });
await sleep(400);
await emit("agent_start");
await emit("turn_start", { turnIndex: 0 });
await emit("message_start", { message: { role: "user", content: "secret prompt" } });
await emit("message_start", { message: { role: "assistant" } });
await emit("tool_execution_start", { toolCallId: "c1", toolName: "bash", args: { command: "secret" } });
await sleep(400);
await emit("tool_execution_end", { toolCallId: "c1", toolName: "bash", result: "secret output" });
await emit("turn_end", { message: { role: "assistant", stopReason: "stop", content: [] }, messageEntryId: "a2" });
await emit("agent_before_settle", { outcome: "completed" });
await emit("agent_settled", { aborted: false });
await sleep(400);
await emit("session_shutdown", { reason: "new" });
id = "second-conversation";
await emit("session_start", { reason: "new" });
await sleep(400);
await emit("session_shutdown", { reason: "quit" });
"#;

fn node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Run the extension in Pi's `mode` with `env`; the fake `rakumux` logs its
/// arguments, and each hook's input.
fn run(dir: &Path, mode: &str, env: &[(&str, &str)]) -> (Vec<String>, Vec<serde_json::Value>) {
    let log = dir.join("calls.log");
    let bin = dir.join("rakumux");
    std::fs::write(
        &bin,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LOG\"\n\
         case \"$1 $2\" in 'session hook') cat >> \"$LOG.payload\"; echo >> \"$LOG.payload\";; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let harness = dir.join("harness.mjs");
    std::fs::write(&harness, HARNESS).unwrap();
    // Pi loads the installed `.ts` with jiti; Node runs the same module as `.mjs`.
    let installed = rakumux_mcp::pi::install(&dir.join("agent")).unwrap();
    let module = dir.join("rakumux-session.mjs");
    std::fs::copy(&installed, &module).unwrap();
    let mut command = Command::new("node");
    command
        .arg(&harness)
        .arg(&module)
        .arg(mode)
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
    let calls = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    let payloads = std::fs::read_to_string(dir.join("calls.log.payload"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (calls, payloads)
}

const SESSION: &[(&str, &str)] = &[
    ("RAKUMUX_SESSION_ID", "01JSELF"),
    ("RAKUMUX_SESSION_TOKEN", "tok"),
];

#[test]
fn the_extension_binds_each_conversation_and_reports_its_turns() {
    if !node() {
        eprintln!("skipped: node is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (calls, payloads) = run(dir.path(), "tui", SESSION);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("session bind"))
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "session bind --agent pi 01a18a95-6f2e-7c3b-9a1d-2cd72115b9d0",
            "session bind --agent pi second-conversation",
        ]
    );
    assert!(calls
        .iter()
        .all(|call| call == "session hook --agent pi"
            || call.starts_with("session bind --agent pi ")));
    let phases: Vec<&str> = payloads
        .iter()
        .map(|payload| payload["phase"].as_str().unwrap())
        .collect();
    assert_eq!(phases.first(), Some(&"idle"));
    assert!(phases.contains(&"running_tool"), "{phases:?}");
    let done = payloads
        .iter()
        .find(|payload| payload["phase"] == "done")
        .expect("a done snapshot");
    assert_eq!(done["completion_id"], "a2");
    assert_eq!(done["agent"], "pi");
    assert_eq!(
        done["working_directory"],
        serde_json::json!({"agent": "pi",
            "conversation_id": "01a18a95-6f2e-7c3b-9a1d-2cd72115b9d0", "path": "/repo"})
    );
    let last = payloads.last().unwrap();
    assert_eq!(last["conversation_id"], "second-conversation");
    assert_eq!(last["phase"], "idle");
    let all = serde_json::to_string(&payloads).unwrap();
    assert!(
        !all.contains("secret"),
        "no prompt, argument or output leaves Pi"
    );
}

#[test]
fn only_pis_own_tui_in_a_rakumux_session_reports() {
    if !node() {
        eprintln!("skipped: node is not installed");
        return;
    }
    // `pi -p`, `--mode json` or `rpc` (a subagent extension's children).
    for mode in ["print", "json", "rpc"] {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            run(dir.path(), mode, SESSION).0,
            Vec::<String>::new(),
            "{mode}"
        );
    }
    // Outside Rakumux, or with half an identity.
    let dir = tempfile::tempdir().unwrap();
    assert!(run(dir.path(), "tui", &[]).0.is_empty());
    let dir = tempfile::tempdir().unwrap();
    assert!(run(dir.path(), "tui", &SESSION[..1]).0.is_empty());
}
