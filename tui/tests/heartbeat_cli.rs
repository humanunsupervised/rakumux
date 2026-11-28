//! `rakumux heartbeat` end to end: the real `rakumux` (this crate's `tui`)
//! and the real daemon in a temp home. The user makes, lists, shows, runs,
//! pauses and deletes heartbeats; an agent inside a session makes one with
//! the same command (`$RAKUMUX_BIN heartbeat add`, no MCP needed), which is
//! on at once without the user's approval, or, made without its prompt and
//! schedule, turns on once its agent's update saves them. An agent runs any
//! heartbeat now; enabling, pausing and deleting stay the user's.
#![cfg(target_os = "linux")]

use serde_json::Value;
use session_core::paths::{Layout, Registration};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const TUI: &str = env!("CARGO_BIN_EXE_tui");

struct World {
    _dir: tempfile::TempDir,
    home: PathBuf,
    layout: Layout,
    work: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.rakumux(&["daemon", "stop", "--yes"]);
    }
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let home = root.join("home");
        let work = root.join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let layout = Layout::with_home(root.join("rakumux"));
        Self {
            _dir: dir,
            home,
            layout,
            work,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(TUI);
        for name in [
            session_core::env::SESSION_ID,
            session_core::env::SESSION_TOKEN,
            session_core::env::SOCKET,
            "XDG_RUNTIME_DIR",
        ] {
            command.env_remove(name);
        }
        command
            .current_dir(&self.work)
            .env(session_core::env::HOME, &self.layout.home)
            .env(session_core::env::BIN, TUI)
            .env("RAKUMUX_PTYD_BIN", ptyd_binary())
            .env("HOME", &self.home)
            .env("SHELL", "/bin/bash")
            .stdin(Stdio::null());
        command
    }

    fn rakumux(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    /// Run `args`, which must succeed; its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.rakumux(args);
        assert!(
            output.status.success(),
            "rakumux {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn heartbeats(&self) -> Vec<Value> {
        let json: Value =
            serde_json::from_str(&self.ok(&["heartbeat", "ls", "--all", "--json"])).unwrap();
        json["heartbeats"].as_array().unwrap().clone()
    }

    /// Run `script` in a new session named `name` (its `$RAKUMUX_BIN` calls
    /// are that session's, an agent's); what it printed, once it ended.
    fn in_session(&self, name: &str, script: &str) -> String {
        let out = self.work.join(format!("{name}.out"));
        let line = format!(
            "{{ {script}; }} > {out} 2>&1; echo done=$? >> {out}",
            out = out.display()
        );
        self.ok(&["new", "--detach", "--name", name, "--cmd", &line]);
        self.wait(&format!("{name}'s script"), || {
            std::fs::read_to_string(&out).is_ok_and(|text| text.contains("done="))
        });
        std::fs::read_to_string(&out).unwrap()
    }

    fn heartbeat(&self, name: &str) -> Value {
        let shown: Value =
            serde_json::from_str(&self.ok(&["heartbeat", "show", name, "--json"])).unwrap();
        shown["heartbeat"].clone()
    }

    /// The run record of `name`, newest first.
    fn runs(&self, name: &str) -> Vec<Value> {
        let json: Value =
            serde_json::from_str(&self.ok(&["heartbeat", "runs", name, "--json"])).unwrap();
        json["runs"].as_array().unwrap().clone()
    }

    /// The id of this folder's session `name`.
    fn session_id(&self, name: &str) -> String {
        let json: Value = serde_json::from_str(&self.ok(&["ls", "--json"])).unwrap();
        let sessions = json["sessions"].as_array().unwrap();
        let session = sessions.iter().find(|s| s["name"] == name);
        session.unwrap_or_else(|| panic!("no session {name}: {json}"))["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn wait(&self, what: &str, mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !check() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The `rakumux-ptyd` of this build, next to the `tui` under test.
fn ptyd_binary() -> PathBuf {
    let path = Path::new(TUI).with_file_name("rakumux-ptyd");
    assert!(
        path.is_file(),
        "build the workspace first (cargo build --workspace): {} is missing",
        path.display()
    );
    path
}

#[test]
fn heartbeats_are_made_and_run_from_the_cli_and_an_agents_are_on_at_once() {
    let world = World::new();
    let made = world.ok(&[
        "heartbeat",
        "add",
        "deps",
        "--every",
        "30m",
        "--command",
        "echo started-deps",
        "--",
        "update",
        "the",
        "dependencies",
    ]);
    assert!(made.contains("Heartbeat deps (hb_"), "{made}");
    assert!(
        made.contains("next run in 29m") || made.contains("next run in 30m"),
        "{made}"
    );
    assert!(Registration::load(&world.layout).is_ok());

    let listed = world.ok(&["heartbeat", "ls"]);
    assert!(
        listed.contains("deps") && listed.contains("every 30m") && listed.contains("on"),
        "{listed}"
    );
    let shown = world.ok(&["heartbeat", "show", "deps"]);
    assert!(shown.contains("command:  echo started-deps"), "{shown}");
    assert!(shown.contains("  update the dependencies"), "{shown}");

    // A run now: `deps #1`, the command typed into its shell.
    let run = world.ok(&["heartbeat", "run", "deps"]);
    assert!(run.starts_with("Started deps #1 ("), "{run}");
    world.wait("the run's session", || {
        world.ok(&["ls", "--json"]).contains("\"deps #1\"")
    });

    // An agent in a session makes one with the same command: on at once;
    // enabling is still the user's.
    let out = world.work.join("agent.out");
    let script = format!(
        "\"$RAKUMUX_BIN\" heartbeat add nightly --daily 02:00 --command 'echo nightly' --prompt 'run the tests' > {out} 2>&1; \
         \"$RAKUMUX_BIN\" heartbeat enable nightly >> {out} 2>&1; echo exit=$? >> {out}",
        out = out.display()
    );
    world.ok(&["new", "--detach", "--name", "agent", "--cmd", &script]);
    world.wait("the agent's heartbeat", || {
        std::fs::read_to_string(&out).is_ok_and(|text| text.contains("exit="))
    });
    let said = std::fs::read_to_string(&out).unwrap();
    assert!(said.contains("Heartbeat nightly (hb_"), "{said}");
    assert!(said.contains("next run in"), "{said}");
    assert!(!said.contains("approv"), "{said}");
    assert!(said.contains("not allowed from inside a session"), "{said}");
    assert!(said.contains("exit=1"), "{said}");
    let nightly = world
        .heartbeats()
        .into_iter()
        .find(|h| h["name"] == "nightly")
        .unwrap();
    assert_eq!(nightly["enabled"], true);
    assert_eq!(nightly["awaiting_approval"], false);
    assert_eq!(nightly["created_by"]["kind"], "session");
    assert_eq!(nightly["prompt"], "run the tests");

    // The user pauses deps, and deletes it.
    world.ok(&["heartbeat", "pause", "deps"]);
    let all = world.heartbeats();
    let deps = all.iter().find(|h| h["name"] == "deps").unwrap();
    assert_eq!(deps["enabled"], false);
    assert_eq!(deps["runs"], 1);
    let nightly = all.iter().find(|h| h["name"] == "nightly").unwrap();
    assert_eq!(
        (
            nightly["enabled"].clone(),
            nightly["awaiting_approval"].clone()
        ),
        (Value::Bool(true), Value::Bool(false))
    );
    // A schedule faster than five minutes is refused before it's sent.
    let fast = world.rakumux(&["heartbeat", "update", "deps", "--every", "1m"]);
    assert!(!fast.status.success());
    assert!(String::from_utf8_lossy(&fast.stderr).contains("every 5 minutes"));
    world.ok(&["heartbeat", "rm", "deps"]);
    assert_eq!(world.heartbeats().len(), 1);
    // Its run stays, as a plain session.
    assert!(world.ok(&["ls", "--json"]).contains("\"deps #1\""));

    // An empty heartbeat an agent made, filled in by the agent of a session
    // with the line the TUI's setup prompt gives (no MCP): it turns on.
    let said = world.in_session("asker", "\"$RAKUMUX_BIN\" heartbeat add");
    assert!(
        said.contains("it turns on once its prompt and schedule are saved"),
        "{said}"
    );
    let empty = world.heartbeat("Heartbeat");
    let handle = format!("hb_{}", empty["id"].as_str().unwrap());
    assert_eq!(empty["enabled"], false);
    assert_eq!(empty["awaiting_approval"], true);
    let listed = world.ok(&["heartbeat", "ls"]);
    assert!(listed.contains("setting up"), "{listed}");
    let said = world.in_session(
        "setup",
        &format!(
            "\"$RAKUMUX_BIN\" heartbeat update {handle} --prompt 'Check the build, fix it if red.' --every 30m"
        ),
    );
    assert!(said.contains("changed; next run in"), "{said}");
    let shown = world.heartbeat(&handle);
    assert_eq!(shown["prompt"], "Check the build, fix it if red.");
    assert_eq!(shown["schedule"]["minutes"], 30);
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["awaiting_approval"], false);
    assert!(shown["next_run_at"].is_number(), "{shown}");

    // One the user made empty is a plain pause: the agent filling it in
    // leaves it paused.
    let mine: Value =
        serde_json::from_str(&world.ok(&["heartbeat", "add", "mine", "--json"])).unwrap();
    assert_eq!(mine["heartbeat"]["awaiting_approval"], false);
    let said = world.in_session(
        "filler",
        "\"$RAKUMUX_BIN\" heartbeat update mine --prompt 'Tidy up.' --every 1h",
    );
    assert!(said.contains("changed; paused"), "{said}");
    let mine = world.heartbeat("mine");
    assert_eq!(
        (mine["enabled"].clone(), mine["awaiting_approval"].clone()),
        (Value::Bool(false), Value::Bool(false))
    );
}

#[test]
fn an_agent_in_a_session_runs_any_heartbeat_and_its_changes_never_pause_one() {
    let world = World::new();
    // The user's own, paused: an agent runs it now, as the user would.
    world.ok(&[
        "heartbeat",
        "add",
        "deps",
        "--every",
        "30m",
        "--paused",
        "--command",
        "echo started-deps",
        "--",
        "update",
        "the",
        "dependencies",
    ]);
    let deps = world.heartbeat("deps");
    assert_eq!(deps["enabled"], false);
    assert_eq!(deps["awaiting_approval"], false);
    let said = world.in_session("runner", "\"$RAKUMUX_BIN\" heartbeat run deps");
    assert!(said.starts_with("Started deps #1 ("), "{said}");
    assert!(said.contains("done=0"), "{said}");
    // Recorded with its session, which runs like the user's; still paused.
    world.wait("the run's session", || {
        world.ok(&["ls", "--json"]).contains("\"deps #1\"")
    });
    let run = world.session_id("deps #1");
    let runs = world.runs("deps");
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["run"], 1);
    assert_eq!(runs[0]["session_id"], run.as_str());
    assert_eq!(runs[0]["place"], "listed");
    let deps = world.heartbeat("deps");
    assert_eq!(
        (deps["runs"].clone(), deps["enabled"].clone()),
        (1.into(), false.into())
    );

    // Changed by an agent, it stays as the user left it (paused), and the
    // agent runs it with the change.
    let said = world.in_session(
        "changer",
        "\"$RAKUMUX_BIN\" heartbeat update deps --prompt 'update them and push' > /dev/null; \
         \"$RAKUMUX_BIN\" heartbeat run deps",
    );
    assert!(said.starts_with("Started deps #2 ("), "{said}");
    assert!(said.contains("done=0"), "{said}");
    let deps = world.heartbeat("deps");
    assert_eq!(deps["prompt"], "update them and push");
    assert_eq!(
        (deps["enabled"].clone(), deps["awaiting_approval"].clone()),
        (false.into(), false.into())
    );
    assert_eq!(world.runs("deps").len(), 2);
    // The user runs it as before.
    let run = world.ok(&["heartbeat", "run", "deps"]);
    assert!(run.starts_with("Started deps #3 ("), "{run}");

    // Made by an agent: on at once, and the agent runs it; enabling,
    // pausing and deleting stay the user's.
    let said = world.in_session(
        "maker",
        "\"$RAKUMUX_BIN\" heartbeat add nightly --daily 02:00 --command 'echo nightly' \
         --prompt 'run the tests' > /dev/null; \
         \"$RAKUMUX_BIN\" heartbeat run nightly; echo run=$?; \
         \"$RAKUMUX_BIN\" heartbeat enable nightly; echo enable=$?; \
         \"$RAKUMUX_BIN\" heartbeat pause nightly; echo pause=$?; \
         \"$RAKUMUX_BIN\" heartbeat rm nightly; echo rm=$?",
    );
    assert!(said.starts_with("Started nightly #1 ("), "{said}");
    for outcome in ["run=0", "enable=1", "pause=1", "rm=1"] {
        assert!(said.contains(outcome), "{outcome}: {said}");
    }
    assert!(
        said.contains("heartbeat.enable is not allowed from inside a session"),
        "{said}"
    );
    assert!(
        said.contains("heartbeat.delete is not allowed from inside a session"),
        "{said}"
    );
    let nightly = world.heartbeat("nightly");
    assert_eq!(nightly["enabled"], true);
    assert_eq!(nightly["awaiting_approval"], false);
    world.wait("the agent's run's session", || {
        world.ok(&["ls", "--json"]).contains("\"nightly #1\"")
    });
    let runs = world.runs("nightly");
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(
        runs[0]["session_id"],
        world.session_id("nightly #1").as_str()
    );
    assert_eq!(world.heartbeats().len(), 2);
}
