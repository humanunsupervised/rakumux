//! `rakumux workspace` end to end: the real `rakumux` (this crate's `tui`)
//! and the real daemon in a temp home. The user makes workspaces, moves
//! sessions into them (making one by name when it's missing) and out,
//! lists them with their counts and forgets one, whose sessions are then in
//! none.
#![cfg(target_os = "linux")]

use serde_json::Value;
use session_core::paths::Layout;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const TUI: &str = env!("CARGO_BIN_EXE_tui");

struct World {
    _dir: tempfile::TempDir,
    home: PathBuf,
    layout: Layout,
    work: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.rakumux(&["daemon", "stop"]);
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

    fn workspaces(&self) -> Value {
        serde_json::from_str(&self.ok(&["workspace", "ls", "--json"])).unwrap()
    }

    /// The workspace `count` of each named, and the sessions in none.
    fn counts(&self) -> (Vec<(String, u64)>, u64) {
        let json = self.workspaces();
        let named = json["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                (
                    w["name"].as_str().unwrap().to_owned(),
                    w["sessions"].as_u64().unwrap(),
                )
            })
            .collect();
        (named, json["unassigned"]["sessions"].as_u64().unwrap())
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
fn sessions_move_between_workspaces_from_the_cli() {
    let world = World::new();
    world.ok(&["new", "--detach", "--name", "web"]);
    world.ok(&["new", "--detach", "--name", "api"]);
    assert_eq!(world.counts(), (vec![], 2));

    assert!(world
        .ok(&["workspace", "add", "Launch"])
        .contains("Made workspace Launch"));
    let taken = world.rakumux(&["workspace", "add", "launch"]);
    assert!(!taken.status.success());
    assert!(world
        .ok(&["workspace", "move", "web", "launch"])
        .contains("Moved web to Launch"));
    // A name with no workspace makes one.
    assert!(world
        .ok(&["workspace", "move", "api", "Research"])
        .contains("Moved api to Research"));
    assert_eq!(
        world.counts(),
        (vec![("Launch".into(), 1), ("Research".into(), 1)], 0)
    );
    let table = world.ok(&["workspace", "ls"]);
    assert!(table.contains("Unassigned"), "{table}");
    assert!(table.contains("Research"), "{table}");
    let ls: Value = serde_json::from_str(&world.ok(&["ls", "--json"])).unwrap();
    let launch = world.workspaces()["workspaces"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let web = ls["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "web")
        .unwrap()
        .clone();
    assert_eq!(web["workspace"].as_str(), Some(launch.as_str()));

    assert!(world
        .ok(&["workspace", "move", "web", "--none"])
        .contains("Moved web to Unassigned"));
    assert!(world
        .ok(&["workspace", "rename", "research", "Deep research"])
        .contains("Renamed workspace Research to Deep research"));
    assert!(!world
        .rakumux(&["workspace", "rename", "Launch", "deep research"])
        .status
        .success());
    assert_eq!(
        world.counts(),
        (vec![("Launch".into(), 0), ("Deep research".into(), 1)], 1)
    );
    assert!(world
        .ok(&["workspace", "rm", "Deep research"])
        .contains("Deleted workspace Deep research"));
    assert_eq!(world.counts(), (vec![("Launch".into(), 0)], 2));
    assert!(!world
        .rakumux(&["workspace", "rm", "Deep research"])
        .status
        .success());
}
