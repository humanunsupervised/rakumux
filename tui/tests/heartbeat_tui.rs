//! The real TUI in a real terminal (a session of one daemon), with a daemon
//! of its own in a temp home. A heartbeat is made in Beats' New heartbeat
//! form, renamed and deleted from the list, and the CLI sees each; the left
//! column's tabs switch what it shows, and Help is searched.
#![cfg(target_os = "linux")]

use serde_json::Value;
use session_core::api::{
    AttachRequest, CreateSession, ReadRequest, ReadSource, Role, SendRequest, TextFormat,
};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use session_core::paths::Layout;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TUI: &str = env!("CARGO_BIN_EXE_tui");

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        // The TUI's terminal first: it would start its daemon again.
        for home in ["outer", "inner"] {
            let _ = self
                .command(home)
                .args(["daemon", "stop", "--yes"])
                .output();
        }
    }
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for name in ["home", "work"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        std::fs::create_dir_all(root.join("inner")).unwrap();
        // No welcome tour or phone offer over the first screen.
        std::fs::write(
            root.join("inner/config.json"),
            r#"{"sessions":{"welcome_seen":true,"phone_offer_seen":true,"auto_update":false}}"#,
        )
        .unwrap();
        Self { _dir: dir, root }
    }

    fn command(&self, home: &str) -> Command {
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
            .current_dir(self.root.join("work"))
            .env(session_core::env::HOME, self.root.join(home))
            .env(session_core::env::BIN, TUI)
            .env("RAKUMUX_PTYD_BIN", ptyd_binary())
            .env("HOME", self.root.join("home"))
            .env("SHELL", "/bin/bash")
            .stdin(Stdio::null());
        command
    }

    fn ok(&self, home: &str, args: &[&str]) -> String {
        let output = self.command(home).args(args).output().unwrap();
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
            serde_json::from_str(&self.ok("inner", &["heartbeat", "ls", "--all", "--json"]))
                .unwrap();
        json["heartbeats"].as_array().unwrap().clone()
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

/// The TUI's terminal.
struct Screen {
    api: Arc<dyn SessionApi>,
    id: SessionId,
}

impl Screen {
    fn text(&self) -> String {
        self.api
            .read(ReadRequest {
                id: self.id.clone(),
                source: ReadSource::Visible,
                lines: None,
                format: TextFormat::default(),
            })
            .unwrap()
            .text
    }

    fn shows(&self, what: &str) -> String {
        self.until(what, |text| text.contains(what))
    }

    fn until(&self, what: &str, check: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let text = self.text();
            if check(&text) {
                return text;
            }
            assert!(Instant::now() < deadline, "no {what:?} on screen:\n{text}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Type `text`, then press `keys`.
    fn send(&self, text: &str, keys: &[&str]) {
        if !text.is_empty() {
            self.request(Some(text), &[]);
        }
        self.keys(keys);
    }

    /// Click at `column`, `row` (from 1), as a terminal reports it.
    fn click(&self, column: u16, row: u16) {
        let stream = self
            .api
            .attach(AttachRequest {
                id: self.id.clone(),
                role: Role::Controller,
                cols: 160,
                rows: 48,
            })
            .unwrap();
        let click = format!("\u{1b}[<0;{column};{row}M\u{1b}[<0;{column};{row}m");
        stream.sender().input(click.into_bytes()).unwrap();
        // Delivered before the stream closes.
        std::thread::sleep(Duration::from_millis(300));
    }

    fn request(&self, text: Option<&str>, keys: &[&str]) {
        self.api
            .send(SendRequest {
                id: self.id.clone(),
                keys: keys.iter().map(|key| key.to_string()).collect(),
                text: text.map(str::to_string),
            })
            .unwrap();
    }

    fn keys(&self, keys: &[&str]) {
        for key in keys {
            self.request(None, &[key]);
        }
    }
}

/// The TUI, started in a 160 × 48 terminal.
fn start_tui(world: &World) -> Screen {
    world.ok("outer", &["daemon", "start"]);
    let api: Arc<dyn SessionApi> = Arc::new(
        Client::connect(ClientConfig {
            layout: Layout::with_home(world.root.join("outer")),
            client_name: "e2e".into(),
            version: "test".into(),
            auth: AuthMode::User,
            spawn: false,
            ptyd_path: None,
        })
        .unwrap(),
    );
    let run = format!(
        "exec env -u RAKUMUX_SESSION_ID -u RAKUMUX_SESSION_TOKEN -u RAKUMUX_SOCKET \
         RAKUMUX_HOME='{}' HOME='{}' '{TUI}' --ptyd-path '{}'",
        world.root.join("inner").display(),
        world.root.join("home").display(),
        ptyd_binary().display()
    );
    let work = world.root.join("work");
    let session = api
        .create(CreateSession {
            scope: Some(ScopeRoot::resolve(&work).unwrap()),
            name: Some("ui".into()),
            cwd: Some(work),
            argv: Some(vec!["/bin/bash".into(), "-c".into(), run]),
            startup_cmds: vec![],
            env: vec![
                (
                    "PATH".into(),
                    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
                ),
                ("LANG".into(), "C.UTF-8".into()),
            ],
            cols: 160,
            rows: 48,
            machine: None,
            prefill: None,
            first_input: None,
            workspace: None,
        })
        .unwrap();
    Screen {
        api,
        id: session.id,
    }
}

#[test]
fn the_left_columns_tabs_switch_and_help_is_searched_in_the_real_tui() {
    let world = World::new();
    let screen = start_tui(&world);
    // A start shows the sessions, over the column's tabs.
    screen.shows("+ New session");
    let shown = screen.shows("❯ Sessions   ≡ Inbox   ♥ Beats");
    assert!(shown.contains("◔ Status"), "{shown}");
    // f: the Inbox in their place; Esc: the sessions.
    screen.send("f", &[]);
    let inbox = screen.shows("Sign in to see your inbox");
    assert!(!inbox.contains("◔ Status"), "{inbox}");
    screen.keys(&["esc"]);
    screen.until("the sessions", |text| {
        text.contains("◔ Status") && !text.contains("Sign in to see")
    });
    // A click on a tab at the bottom shows it (SGR mouse: press, release).
    for (column, what) in [(27, "+ New heartbeat"), (5, "+ New session")] {
        screen.click(column, 46);
        screen.shows(what);
    }
    // rtop is the drawer's, on the right, beside it.
    screen.send("t", &[]);
    let both = screen.shows("▥ rtop");
    assert!(both.contains("❯ Sessions   ≡ Inbox   ♥ Beats"), "{both}");
    screen.send("t", &[]);
    screen.until("rtop hidden", |text| !text.contains("▥ rtop"));
    screen.send("?", &[]);
    screen.shows("/ to search help");
    screen.send("/", &[]);
    screen.shows("Type to search help");
    screen.send("after", &[]);
    let found = screen.until("what help says", |text| {
        text.contains("⌕ after█") && text.contains("Esc end search")
    });
    assert!(found.to_lowercase().matches("after").count() > 2, "{found}");
    assert!(!found.contains("1 Overview"), "{found}");
    screen.keys(&["esc"]);
    screen.shows("1 Overview");
    screen.keys(&["esc"]);
    screen.until("help closed", |text| !text.contains("1 Overview"));
}

#[test]
fn a_heartbeat_is_made_renamed_and_deleted_in_the_real_tui() {
    let world = World::new();
    let screen = start_tui(&world);

    // Beats, empty: the row that makes one, and its form.
    screen.shows("+ New session");
    // The footer says what its tabs are.
    screen.shows("workspaces  ⊞   All 0   No workspace 0");
    screen.send("B", &[]);
    screen.shows("+ New heartbeat");
    screen.keys(&["enter"]);
    screen.shows("Ask agent");
    screen.send("nightly", &["enter"]);
    screen.send("Run the tests.", &["tab"]);
    // Daily, at 02:30.
    screen.keys(&["right", "tab"]);
    screen.keys(&[
        "backspace",
        "backspace",
        "backspace",
        "backspace",
        "backspace",
    ]);
    screen.send("02:30", &["tab"]);
    // Folder, agent, Create.
    screen.keys(&["tab", "tab", "enter"]);
    let shown = screen.shows("c rename");
    assert!(shown.contains("● nightly"), "{shown}");
    assert!(shown.contains("Run the tests."), "{shown}");
    let made = world.heartbeats();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0]["name"], "nightly");
    assert_eq!(made[0]["prompt"], "Run the tests.");
    assert_eq!(made[0]["enabled"], true);
    assert_eq!(made[0]["awaiting_approval"], false);
    assert_eq!(made[0]["schedule"]["kind"], "daily");
    assert_eq!(made[0]["schedule"]["at"], "02:30");
    assert_eq!(made[0]["command"], "claude");
    assert!(made[0]["next_run_at"].is_number(), "{made:?}");

    // A second of that name is refused in the form, which keeps its text.
    screen.send("n", &[]);
    screen.shows("Ask agent");
    screen.send("nightly", &["tab"]);
    screen.send("Again.", &["tab"]);
    screen.keys(&["tab", "tab", "tab", "tab", "enter"]);
    let refused = screen.shows("✕");
    assert!(refused.contains("Again."), "{refused}");
    assert_eq!(world.heartbeats().len(), 1);
    screen.keys(&["esc"]);
    screen.until("the form closed", |text| !text.contains("Ask agent"));

    // Back to the list; c renames it, on its schedule still.
    screen.keys(&["esc"]);
    screen.shows("+ New heartbeat");
    screen.send("c", &[]);
    screen.shows("Rename heartbeat");
    screen.send("-run", &["enter"]);
    screen.shows("● nightly-run");
    let renamed = world.heartbeats();
    assert_eq!(renamed[0]["name"], "nightly-run");
    assert_eq!(renamed[0]["enabled"], true);

    // d, then d again, deletes it.
    screen.send("d", &[]);
    screen.shows("d again deletes");
    assert_eq!(world.heartbeats().len(), 1);
    screen.send("d", &[]);
    screen.until("it gone", |text| !text.contains("● nightly-run"));
    assert!(world.heartbeats().is_empty());
    screen.shows("+ New heartbeat");
}
