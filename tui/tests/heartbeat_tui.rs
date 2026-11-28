//! The real TUI in a real terminal (a session of one daemon), with a daemon
//! of its own in a temp home. A heartbeat is made in Heartbeats' New heartbeat
//! form, edited and deleted from the list, and the CLI sees each; one the
//! form's `Ask agent` makes turns on once its agent saves its prompt and
//! schedule, nobody pressing `e`; a long
//! prompt is pasted, scrolled and edited in the form and read whole; the left
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

    /// Bytes as the terminal sends them, from a controller of the size the
    /// TUI's terminal is.
    fn input(&self, bytes: &[u8]) {
        let stream = self
            .api
            .attach(AttachRequest {
                id: self.id.clone(),
                role: Role::Controller,
                cols: 160,
                rows: 48,
            })
            .unwrap();
        stream.sender().input(bytes.to_vec()).unwrap();
        // Delivered before the stream closes.
        std::thread::sleep(Duration::from_millis(300));
    }

    /// Paste `text`, as a terminal does with bracketed paste on.
    fn paste(&self, text: &str) {
        self.input(format!("\u{1b}[200~{text}\u{1b}[201~").as_bytes());
    }

    /// One notch of the wheel at `column`, `row` (from 1): up or down.
    fn wheel(&self, column: u16, row: u16, up: bool) {
        let button = if up { 64 } else { 65 };
        self.input(format!("\u{1b}[<{button};{column};{row}M").as_bytes());
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
    start_tui_with_path(world, None)
}

/// The TUI, its `PATH` (and so its sessions') starting with `bin` when given.
fn start_tui_with_path(world: &World, bin: Option<&Path>) -> Screen {
    world.ok("outer", &["daemon", "start"]);
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let path = match bin {
        Some(bin) => format!("{}:{path}", bin.display()),
        None => path,
    };
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
            env: vec![("PATH".into(), path), ("LANG".into(), "C.UTF-8".into())],
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

/// Where `what` first shows on `text`'s screen: its column and row, from 1.
fn place(text: &str, what: &str) -> (u16, u16) {
    let (row, line) = text
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(what))
        .unwrap_or_else(|| panic!("no {what:?} on screen:\n{text}"));
    let column = line[..line.find(what).unwrap()].chars().count();
    (column as u16 + 1, row as u16 + 1)
}

/// The screen's row that shows `what`.
fn row_with<'a>(text: &'a str, what: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(what))
        .unwrap_or_else(|| panic!("no {what:?} on screen:\n{text}"))
}

/// Row `n` of the long prompt.
fn line(n: usize) -> String {
    format!("line {n:03} of the long prompt")
}

/// Row `n` of the long prompt shows as a row of its own (the command under
/// the form runs its first ones on in one line).
fn shown(text: &str, n: usize) -> bool {
    text.contains(&format!("{}  ", line(n)))
}

/// The screen's last row.
fn footer(text: &str) -> &str {
    text.trim_end().lines().last().unwrap_or_default()
}

#[test]
fn the_left_columns_tabs_switch_and_help_is_searched_in_the_real_tui() {
    let world = World::new();
    let screen = start_tui(&world);
    // A start shows the sessions, over the column's tabs.
    screen.shows("+ New session");
    let shown = screen.shows("▴ All workspaces   ≡ Inbox   ♥  Heartbeats");
    assert!(shown.contains("◔ Status"), "{shown}");
    // f: the Inbox in their place; Esc: the sessions.
    screen.send("f", &[]);
    let inbox = screen.shows("Sign in to see your inbox");
    assert!(!inbox.contains("◔ Status"), "{inbox}");
    screen.keys(&["esc"]);
    screen.until("the sessions", |text| {
        text.contains("◔ Status") && !text.contains("Sign in to see")
    });
    // A click on a tab at the bottom shows it (SGR mouse: press, release;
    // columns from 1).
    let tabs = shown
        .lines()
        .find(|line| line.contains("♥  Heartbeats"))
        .unwrap();
    let heartbeats = tabs[..tabs.find("♥  Heartbeats").unwrap()].chars().count() as u16;
    screen.click(heartbeats + 3, 46);
    screen.shows("+ New heartbeat");
    // The Workspaces tab's shows the sessions and the menu over it; Esc
    // closes the menu.
    screen.click(5, 46);
    let menu = screen.shows("+ Add new workspace");
    assert!(menu.contains("▾ All workspaces"), "{menu}");
    assert!(
        menu.contains("✓ All workspaces") && menu.contains("No workspace"),
        "{menu}"
    );
    screen.keys(&["esc"]);
    screen.until("the menu closed", |text| {
        !text.contains("+ Add new workspace") && text.contains("+ New session")
    });
    // So does w, and it moves nothing: W does.
    screen.send("w", &[]);
    screen.shows("+ Add new workspace");
    screen.keys(&["esc"]);
    screen.until("the menu closed", |text| {
        !text.contains("+ Add new workspace")
    });
    // A click on an item shows that workspace's, and the tab names it.
    screen.click(5, 46);
    let menu = screen.shows("No workspace");
    let row = menu
        .lines()
        .position(|line| line.contains("No workspace"))
        .unwrap();
    screen.click(8, row as u16 + 1);
    screen.shows("▴ No workspace   ≡ Inbox   ♥  Heartbeats");
    // The middle of the footer names it too, and no mode.
    screen.until("the footer's workspace", |text| {
        footer(text).contains(" No workspace ") && !footer(text).contains("MODE")
    });
    screen.click(5, 46);
    screen.shows("✓ No workspace");
    screen.keys(&["up", "enter"]);
    screen.shows("▴ All workspaces");
    // rtop is the drawer's, on the right, beside it.
    screen.send("t", &[]);
    let both = screen.shows("▥ rtop");
    assert!(both.contains("≡ Inbox   ♥  Heartbeats"), "{both}");
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
    // A session keeps the workspace there; a click on it is the way out.
    screen.send("n", &[]);
    let session = screen.until("a session", |text| {
        footer(text).contains("Ctrl-\\") && footer(text).contains(" All workspaces ")
    });
    let line = footer(&session);
    assert!(!line.contains("MODE"), "{line}");
    let before = &line[..line.find(" All workspaces ").unwrap()];
    screen.click(before.chars().count() as u16 + 2, 48);
    screen.until("Rakumux mode", |text| {
        footer(text).contains("? Help") && footer(text).contains(" All workspaces ")
    });
}

#[test]
fn a_heartbeat_is_made_edited_and_deleted_in_the_real_tui() {
    let world = World::new();
    let screen = start_tui(&world);

    // Heartbeats, empty: the row that makes one, and its form.
    screen.shows("+ New session");
    // The Workspaces tab names the one shown.
    screen.shows("▴ All workspaces   ≡ Inbox   ♥  Heartbeats");
    screen.send("B", &[]);
    screen.shows("+ New heartbeat");
    screen.keys(&["enter"]);
    screen.shows("Ask agent");
    screen.send("nightly", &["enter"]);
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
    // The folder; the agent, what it takes listed, and another's.
    screen.keys(&["tab"]);
    screen.shows("--permission-mode <mode>");
    screen.keys(&["right"]);
    screen.until("another agent's arguments", |text| {
        text.contains("--sandbox <mode>") && !text.contains("--permission-mode")
    });
    screen.keys(&["left", "tab"]);
    // Its arguments: one typed in part and taken, its value picked.
    screen.shows("--dangerously-skip-permissions");
    screen.send(" eff", &[]);
    screen.until("what's typed", |text| !text.contains("--add-dir"));
    screen.keys(&["tab"]);
    screen.shows("xhigh");
    screen.keys(&["down", "down", "enter", "tab"]);
    screen.until("the list closed", |text| !text.contains("xhigh"));
    screen.send("Run the tests.", &[]);
    screen.shows("$ claude --dangerously-skip-permissions --effort medium -- \"Run the tests.\"");
    screen.keys(&["tab", "enter"]);
    let shown = screen.shows("c edit");
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
    assert_eq!(made[0]["agent"], "claude");
    assert_eq!(
        made[0]["command"],
        "claude --dangerously-skip-permissions --effort medium"
    );
    assert!(made[0]["next_run_at"].is_number(), "{made:?}");

    // A second of that name is refused in the form, which keeps its text.
    screen.send("n", &[]);
    screen.shows("Ask agent");
    screen.send("nightly", &["tab"]);
    // Runs, when, folder, agent, arguments.
    screen.keys(&["tab", "tab", "tab", "tab", "tab"]);
    screen.send("Again.", &["tab"]);
    screen.keys(&["enter"]);
    let refused = screen.shows("✕");
    assert!(refused.contains("Again."), "{refused}");
    assert_eq!(world.heartbeats().len(), 1);
    screen.keys(&["esc"]);
    screen.until("the form closed", |text| !text.contains("Ask agent"));

    // Back to the list; c edits it: its name alone, on its schedule still.
    screen.keys(&["esc"]);
    screen.shows("+ New heartbeat");
    let next = made[0]["next_run_at"].clone();
    screen.send("c", &[]);
    screen.shows("Edit heartbeat");
    screen.send("-run", &["ctrl+s"]);
    screen.shows("● nightly-run");
    let renamed = world.heartbeats();
    assert_eq!(renamed[0]["name"], "nightly-run");
    assert_eq!(renamed[0]["enabled"], true);
    assert_eq!(renamed[0]["next_run_at"], next);
    assert_eq!(renamed[0]["command"], made[0]["command"]);
    // Its arguments, prompt and time; it runs with them, at the new one.
    screen.send("c", &["tab", "tab"]);
    screen.shows("Edit heartbeat");
    screen.keys(&["backspace"]);
    screen.send("5", &["tab", "tab"]);
    screen.send(" --verb", &["tab", "tab"]);
    screen.send(" Then stop.", &["tab", "enter"]);
    screen.until("it saved", |text| !text.contains("Edit heartbeat"));
    let edited = world.heartbeats();
    assert_eq!(edited[0]["schedule"]["at"], "02:35");
    assert_ne!(edited[0]["next_run_at"], next);
    assert_eq!(edited[0]["enabled"], true);
    assert_eq!(edited[0]["awaiting_approval"], false);
    assert_eq!(
        edited[0]["command"],
        "claude --dangerously-skip-permissions --effort medium --verbose"
    );
    assert_eq!(edited[0]["prompt"], "Run the tests. Then stop.");
    // One the CLI made, its command more than the form makes: kept as it is.
    world.ok(
        "inner",
        &[
            "heartbeat",
            "add",
            "wrapped",
            "--every",
            "1h",
            "--command",
            "cd . && A=1 claude --x",
            "--prompt",
            "Go.",
            "--paused",
        ],
    );
    screen.shows("wrapped");
    screen.send("", &["down"]);
    screen.send("c", &[]);
    screen.shows("&& A=1 claude --x");
    screen.send("-up", &["ctrl+s"]);
    screen.shows("wrapped-up");
    let kept = world.heartbeats();
    let kept = kept.iter().find(|h| h["name"] == "wrapped-up").unwrap();
    assert_eq!(kept["command"], "cd . && A=1 claude --x");
    assert_eq!(kept["enabled"], false);
    screen.send("d", &["d"]);
    screen.until("it gone", |text| !text.contains("wrapped-up"));
    screen.keys(&["up"]);

    // d, then d again, deletes it.
    screen.send("d", &[]);
    screen.shows("d again deletes");
    assert_eq!(world.heartbeats().len(), 1);
    screen.send("d", &[]);
    screen.until("it gone", |text| !text.contains("● nightly-run"));
    assert!(world.heartbeats().is_empty());
    screen.shows("+ New heartbeat");
}

/// `Ask agent` in the real TUI: the heartbeat is made for its agent, which
/// a session running the agent's command is asked to fill in; the agent (a
/// stand-in `claude` here) saving its prompt and schedule with `heartbeat
/// update` turns it on, nobody pressing `e`.
#[test]
fn a_heartbeat_ask_agent_makes_turns_on_once_its_agent_saves_it_in_the_real_tui() {
    let world = World::new();
    let bin = world.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let before = world.root.join("before.json");
    let after = world.root.join("after.out");
    let agent = bin.join("claude");
    std::fs::write(
        &agent,
        format!(
            "#!/bin/bash\n\
             '{TUI}' heartbeat show Heartbeat --json > '{before}' 2>&1\n\
             '{TUI}' heartbeat update Heartbeat --prompt 'Check the build.' --every 30m > '{after}' 2>&1\n\
             echo stand-in agent done\n\
             exec sleep 600\n",
            before = before.display(),
            after = after.display(),
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let screen = start_tui_with_path(&world, Some(&bin));

    screen.shows("+ New session");
    screen.send("B", &[]);
    screen.shows("+ New heartbeat");
    screen.keys(&["enter"]);
    screen.shows("Ask agent");
    // From the name, up to the buttons: Ask agent.
    screen.keys(&["up", "enter"]);
    screen.until("the form closed", |text| !text.contains("Ask agent"));
    screen.shows("stand-in agent done");

    // Made for its agent, it waited for it, then turned on by itself.
    let shown: Value = serde_json::from_str(&std::fs::read_to_string(&before).unwrap())
        .unwrap_or_else(|error| panic!("{error}: {}", std::fs::read_to_string(&before).unwrap()));
    assert_eq!(shown["heartbeat"]["enabled"], false, "{shown}");
    assert_eq!(shown["heartbeat"]["awaiting_approval"], true, "{shown}");
    let said = std::fs::read_to_string(&after).unwrap();
    assert!(said.contains("changed; next run in"), "{said}");
    let made = world.heartbeats();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0]["name"], "Heartbeat");
    assert_eq!(made[0]["prompt"], "Check the build.");
    assert_eq!(made[0]["schedule"]["minutes"], 30);
    assert_eq!(made[0]["enabled"], true);
    assert_eq!(made[0]["awaiting_approval"], false);
    assert!(made[0]["next_run_at"].is_number(), "{made:?}");
    assert_eq!(made[0]["created_by"]["kind"], "user");
}

/// A heartbeat's runs in the real TUI: read whole, it lists every run the
/// daemon recorded, and the session pane shows the selected one's output as
/// it runs; deleted, the run stays listed with its saved output.
#[test]
fn a_heartbeats_runs_are_listed_and_watched_in_the_real_tui() {
    let world = World::new();
    let screen = start_tui(&world);
    screen.shows("+ New session");
    world.ok(
        "inner",
        &[
            "heartbeat",
            "add",
            "ticker",
            "--every",
            "1h",
            "--command",
            "printf 'tick %s\\n' one; sleep 2; printf 'tick %s\\n' two; sleep 600",
            "--prompt",
            "Go.",
        ],
    );
    world.ok("inner", &["heartbeat", "run", "ticker"]);
    screen.send("B", &[]);
    // The tab's own row (the sessions list shows `ticker #1` too): read.
    screen.shows("every 1h · work");
    screen.keys(&["enter"]);
    // Its run, and the run's output beside it as it comes.
    let shown = screen.shows("#1 running");
    assert!(shown.contains("runs"), "{shown}");
    screen.shows("tick one");
    screen.shows("tick two");
    let runs: Value =
        serde_json::from_str(&world.ok("inner", &["heartbeat", "runs", "ticker", "--json"]))
            .unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);
    assert_eq!(runs["runs"][0]["run"], 1);
    assert_eq!(runs["runs"][0]["place"], "listed");

    // Deleted, it stays listed, its saved output shown.
    world.ok("inner", &["rm", "ticker #1"]);
    let deleted = screen.shows("#1 deleted");
    assert!(deleted.contains("tick two"), "{deleted}");
    let table = world.ok("inner", &["heartbeat", "runs", "ticker"]);
    assert!(table.contains("deleted (rakumux restore)"), "{table}");
}

/// A prompt of 200 lines in the real TUI: pasted into the New heartbeat form
/// with its newlines, it scrolls with the cursor (PgUp/PgDn, Home/End) and
/// the wheel, a scrollbar beside it; edited in the middle and saved, the
/// rest is as pasted. Read whole it scrolls the same way, and `c` edits it
/// again.
#[test]
fn a_long_prompt_scrolls_in_the_form_and_read_whole_in_the_real_tui() {
    let world = World::new();
    let screen = start_tui(&world);
    let pasted: Vec<String> = (1..=200).map(line).collect();
    screen.shows("+ New session");
    screen.send("B", &[]);
    screen.shows("+ New heartbeat");
    screen.keys(&["enter"]);
    screen.shows("Ask agent");
    screen.send("long", &["tab"]);
    // Runs, when, folder, agent, arguments: the prompt.
    screen.keys(&["tab", "tab", "tab", "tab", "tab"]);
    screen.paste(&pasted.join("\n"));

    // The cursor at its end, in view: the last rows, the scrollbar's thumb
    // beside them, more above.
    let end = screen.shows(&line(200));
    assert!(!shown(&end, 1), "{end}");
    assert!(shown(&end, 180), "several rows: {end}");
    assert!(
        row_with(&end, &format!("{}  ", line(200))).contains('┃'),
        "{end}"
    );
    // Home: the start of the text, the thumb at the top.
    screen.keys(&["home"]);
    let start = screen.until("the start", |text| shown(text, 1) && !shown(text, 200));
    assert!(
        row_with(&start, &format!("{}  ", line(1))).contains('┃'),
        "{start}"
    );
    // PgDn a page, PgUp back.
    screen.keys(&["pagedown"]);
    screen.until("a page down", |text| !shown(text, 1) && shown(text, 40));
    screen.keys(&["pageup"]);
    screen.until("a page up", |text| shown(text, 1));
    // End, then the wheel over it: up, the cursor left where it is; a key
    // brings it back in view.
    screen.keys(&["end"]);
    let end = screen.until("the end", |text| shown(text, 200));
    let (column, row) = place(&end, &line(190));
    screen.wheel(column, row, true);
    screen.wheel(column, row, true);
    screen.until("wheeled up", |text| !shown(text, 200) && shown(text, 194));
    screen.keys(&["end"]);
    screen.until("the end again", |text| shown(text, 200));

    // Row 100 edited where it starts; Ctrl-S creates it from the prompt.
    screen.keys(&["home"]);
    screen.request(None, &["down"; 99]);
    screen.send("EDITED ", &[]);
    screen.shows(&format!("EDITED {}", line(100)));
    screen.keys(&["ctrl+s"]);
    let mut expected = pasted.clone();
    expected[99] = format!("EDITED {}", line(100));
    screen.shows("c edit");
    let made = world.heartbeats();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0]["prompt"], expected.join("\n"));

    // Read whole, its start, a scrollbar; PgDn goes through it to its end.
    let whole = screen.shows(&line(1));
    assert!(!shown(&whole, 200), "{whole}");
    assert!(row_with(&whole, "schedule").contains('┃'), "{whole}");
    let deadline = Instant::now() + Duration::from_secs(30);
    let end = loop {
        screen.keys(&["pagedown"]);
        std::thread::sleep(Duration::from_millis(100));
        let text = screen.text();
        if shown(&text, 200) {
            break text;
        }
        assert!(Instant::now() < deadline, "never its end:\n{text}");
    };
    assert!(
        row_with(&end, &format!("{}  ", line(200))).contains('┃'),
        "{end}"
    );
    // The wheel over it, up.
    let (column, row) = place(&end, &line(198));
    screen.wheel(column, row, true);
    screen.until("wheeled up", |text| !shown(text, 200));
    screen.keys(&["pageup"]);
    screen.until("a page up", |text| !shown(text, 197));

    // c: the form on it, its end in view; row 50 edited, the rest kept.
    screen.send("c", &[]);
    screen.shows("Edit heartbeat");
    // Runs, when, agent, arguments: the prompt.
    screen.keys(&["tab", "tab", "tab", "tab", "tab"]);
    screen.shows(&line(200));
    screen.keys(&["home"]);
    screen.request(None, &["down"; 49]);
    screen.send("ALSO ", &[]);
    screen.shows(&format!("ALSO {}", line(50)));
    screen.keys(&["ctrl+s"]);
    screen.until("it saved", |text| !text.contains("Edit heartbeat"));
    expected[49] = format!("ALSO {}", line(50));
    let saved = world.heartbeats();
    assert_eq!(saved[0]["prompt"], expected.join("\n"));
    assert_eq!(saved[0]["name"], "long");
    assert_eq!(saved[0]["enabled"], true);
}
