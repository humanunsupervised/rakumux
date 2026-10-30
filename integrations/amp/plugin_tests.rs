//! The Amp plugin, run by Node against a fake Amp plugin API (the documented
//! `PluginAPI`: `activeThread`, `threads.get().state/messages`, `on`) and a
//! fake `rakumux` binary. Skipped when `node` isn't installed.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const THREAD: &str = "T-5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f";

const HARNESS: &str = r#"
import { pathToFileURL } from "node:url";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const plugin = (await import(pathToFileURL(process.argv[2]).href)).default;
const T = process.env.THREAD;
const registered = [];
const handlers = {};
const activeSubscribers = [];
let state = "idle";
const stateSubscribers = [];
let dispose = () => {};
// The thread as Amp holds it: history from before the plugin saw it.
const thread = [
  { role: "user", id: "h1", content: [{ type: "text", text: "An earlier prompt" }] },
  { role: "assistant", id: "h2", content: [{ type: "text", text: "An earlier reply" }] },
];
const setState = (value) => { state = value; for (const s of stateSubscribers) s.next(value); };
const api = {
  logger: { log() {} },
  system: {
    cacheDirectory: process.env.CACHE,
    workspaceRoot: { fsPath: "/repo", toString: () => "file:///repo" },
    executor: { kind: process.env.EXECUTOR || "local" },
  },
  helpers: {
    filesModifiedByToolCall: (call) => call.tool === "edit_file" ? [{ fsPath: call.input.path }] : null,
    filePathFromURI: (uri) => uri.fsPath,
  },
  activeThread: {
    current: null,
    subscribe(next) { activeSubscribers.push(next); next(this.current); return { unsubscribe() {} }; },
  },
  threads: {
    get: (id) => ({
      state: { subscribe(observer) { stateSubscribers.push(observer); observer.next(state); return { unsubscribe() {} }; } },
      messages: async ({ offset = 0, limit = 10 } = {}) => {
        const end = Math.max(0, thread.length - offset);
        return thread.slice(Math.max(0, end - Math.min(limit, 20)), end).map((m) => structuredClone(m));
      },
    }),
  },
  on: (name, handler) => { registered.push(name); handlers[name] = handler; return { unsubscribe() {} }; },
  onDispose: (callback) => { dispose = callback; },
};
plugin(api);
console.log(JSON.stringify({ registered }));
if (!registered.length) process.exit(0);
api.activeThread.current = { id: T };
for (const next of activeSubscribers) next({ id: T });
await sleep(300);
const prompt = { role: "user", id: "m1", content: [{ type: "text", text: "Fix the parser" }] };
thread.push(prompt);
const started = await handlers["agent.start"]({ thread: { id: T }, id: "m1", message: "Fix the parser" });
console.log(JSON.stringify({ started }));
setState("running");
await sleep(1200);
thread.push({ role: "assistant", id: "m2", content: [
  { type: "thinking", thinking: "Check the parser" },
  { type: "tool_use", id: "toolu_1", name: "edit_file", input: { path: "/repo/src/a.rs", old_str: "a", new_str: "b" } },
] });
thread.push({ role: "user", id: "m3", content: [{ type: "tool_result", toolUseID: "toolu_1", status: "done", output: "ok" }] });
await sleep(2500);
thread.push({ role: "assistant", id: "m4", content: [{ type: "text", text: "Fixed it." }] });
const ended = handlers["agent.end"]({ thread: { id: T }, id: "m1", status: "done", message: "Fix the parser", messages: thread.slice(2) });
setState("idle");
console.log(JSON.stringify({ ended: ended ?? null }));
await sleep(1500);
await dispose();
process.exit(0);
"#;

fn node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

struct Run {
    stdout: String,
    calls: Vec<String>,
    payloads: Vec<serde_json::Value>,
}

/// Run the plugin with `env`; the fake `rakumux` logs its arguments and
/// what it was given.
fn run(dir: &Path, env: &[(&str, &str)]) -> Run {
    let log = dir.join("calls.log");
    let bin = dir.join("rakumux");
    std::fs::write(
        &bin,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LOG\"\ncase \"$1 $2\" in 'session hook') cat >> \"$LOG.payload\"; echo >> \"$LOG.payload\";; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let harness = dir.join("harness.mjs");
    std::fs::write(&harness, HARNESS).unwrap();
    std::fs::write(dir.join("package.json"), r#"{"type":"module"}"#).unwrap();
    let plugin = rakumux_mcp::amp::install(&dir.join("amp")).unwrap();
    let mut command = Command::new("node");
    command
        .arg(&harness)
        .arg(&plugin)
        .env_remove("RAKUMUX_SESSION_ID")
        .env_remove("RAKUMUX_SESSION_TOKEN")
        .env("RAKUMUX_BIN", &bin)
        .env("LOG", &log)
        .env("THREAD", THREAD)
        .env("CACHE", dir.join("cache/amp/plugin-cache/rakumux-session"));
    for (name, value) in env {
        command.env(name, value);
    }
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "harness failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        calls: read(&log).lines().map(str::to_owned).collect(),
        payloads: read(&dir.join("calls.log.payload"))
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
    }
}

const SESSION: &[(&str, &str)] = &[
    ("RAKUMUX_SESSION_ID", "01JSELF"),
    ("RAKUMUX_SESSION_TOKEN", "tok"),
];

#[test]
fn the_plugin_binds_reports_and_copies_the_thread_amp_shows() {
    if !node() {
        eprintln!("skipped: node is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let threads = dir.path().join("cache/amp/plugin-cache/rakumux-session/threads");
    // A copy unused for 40 days goes; a recent one stays.
    std::fs::create_dir_all(&threads).unwrap();
    let old = threads.join("T-old.jsonl");
    let recent = threads.join("T-recent.jsonl");
    std::fs::write(&old, "").unwrap();
    std::fs::write(&recent, "").unwrap();
    let forty_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(forty_days_ago)
        .unwrap();

    let run = run(dir.path(), SESSION);

    // It never handles `tool.call` (or any request it doesn't need).
    assert!(
        run.stdout
            .contains(r#"{"registered":["session.start","agent.start","agent.end"]}"#),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains(r#"{"started":{}}"#), "{}", run.stdout);
    assert!(run.stdout.contains(r#"{"ended":null}"#), "{}", run.stdout);
    // Bound once, reported as Amp.
    let binds: Vec<_> = run
        .calls
        .iter()
        .filter(|call| call.starts_with("session bind"))
        .collect();
    assert_eq!(binds, [&format!("session bind --agent amp {THREAD}")]);
    assert!(run
        .calls
        .iter()
        .any(|call| call == "session hook --agent amp"));
    let phases: Vec<&str> = run
        .payloads
        .iter()
        .map(|payload| payload["phase"].as_str().unwrap())
        .collect();
    assert!(phases.contains(&"waiting_for_response"), "{phases:?}");
    assert!(phases.contains(&"working"), "{phases:?}");
    let done = run
        .payloads
        .iter()
        .find(|payload| payload["phase"] == "done")
        .unwrap_or_else(|| panic!("no done in {phases:?}"));
    assert_eq!(done["completion_id"], "m4");
    assert_eq!(done["conversation_id"], THREAD);
    assert_eq!(done["working_directory"]["path"], "/repo");
    for payload in &run.payloads {
        assert_eq!(payload["agent"], "amp");
        let text = payload.to_string();
        assert!(!text.contains("Fix the parser") && !text.contains("Fixed it"));
    }

    // The copy: owner only, every message once, history without times.
    let copy = threads.join(format!("{THREAD}.jsonl"));
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&threads), 0o700);
    assert_eq!(mode(&copy), 0o600);
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&copy)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let ids: Vec<String> = lines
        .iter()
        .map(|line| format!("{}:{}", line["kind"].as_str().unwrap(), line["id"].as_str().unwrap()))
        .collect();
    assert_eq!(
        ids,
        [
            "message:h1",
            "message:h2",
            "message:m1",
            "message:m2",
            "message:m3",
            "message:m4",
            "turn_end:m1"
        ]
    );
    assert!(lines[0].get("timestamp").is_none() && lines[1].get("timestamp").is_none());
    assert!(lines[2..].iter().all(|line| line["timestamp"].is_string()));
    assert!(lines[..6].iter().all(|line| line["cwd"] == "/repo"));
    assert_eq!(lines[3]["content"][1]["files"], serde_json::json!(["/repo/src/a.rs"]));
    assert_eq!(lines[6]["status"], "done");
    assert!(!old.exists(), "a copy unused for 30 days is removed");
    assert!(recent.exists());
}

#[test]
fn outside_a_rakumux_session_or_a_local_executor_the_plugin_does_nothing() {
    if !node() {
        eprintln!("skipped: node is not installed");
        return;
    }
    for env in [&[][..], &[("RAKUMUX_SESSION_ID", "01JSELF")][..]] {
        let dir = tempfile::tempdir().unwrap();
        let run = run(dir.path(), env);
        assert!(run.stdout.contains(r#"{"registered":[]}"#), "{}", run.stdout);
        assert!(run.calls.is_empty());
        assert!(!dir.path().join("cache").exists());
    }
    let dir = tempfile::tempdir().unwrap();
    let mut env = SESSION.to_vec();
    env.push(("EXECUTOR", "remote"));
    let run = run(dir.path(), &env);
    assert!(run.stdout.contains(r#"{"registered":[]}"#), "{}", run.stdout);
    assert!(run.calls.is_empty());
}
