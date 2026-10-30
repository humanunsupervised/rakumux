//! Replies typed into Claude Code while it works, end to end: the real
//! daemon (`rakumux-ptyd serve`), the real account service (`rakumux
//! status-service`, signed in as an enrolled machine), a local rakumux.com
//! (`mid_turn/fake_cloud.rs`, HTTPS with a CA made for the run and trusted
//! through `RAKUMUX_CLOUD_CA`) and a stand-in for Claude Code
//! (`mid_turn/claude.sh`, a script named `claude` that logs its conversation
//! as Claude Code does and shows its captured screens). Temp dirs only; no
//! network beyond 127.0.0.1, no real Claude Code, account or `~/.rakumux`.
//!
//! It replays the incident of 2026-10-30. Three replies are written while
//! the agent works and never goes idle: the first goes in mid-turn and is
//! taken as a permission prompt comes up (nothing goes in while it shows);
//! the second has two files, fetched and typed as a paste of several lines,
//! which Claude Code logs as one `<pasted_content id="N">` block; the account
//! service restarts while that one waits in Claude Code's queue; the third
//! goes in once the second was taken; then the agent finishes. Each reply
//! must be typed once, in order, never beside one still queued, and reported
//! `delivered`; every step has a bound, so a stalled queue fails the test
//! instead of hanging it; the turn's answer names the last reply taken
//! (`reply_to`). It fails on each bug it guards: the queue held until the
//! agent waits (fixed in 3a3fbb7), a restart forgetting the reply in flight
//! (e40065b), and a paste block dropped as a prompt (360a1c6).
//!
//! Runs with `cargo test --workspace` (about 30 s), or alone with
//! `cargo test -p tui --test mid_turn_replies -- --nocapture` once
//! `rakumux-ptyd` is built (`cargo build --workspace`; it's built on demand
//! when missing, never rebuilt). Linux only, like the other stand-in tests.
#![cfg(target_os = "linux")]

#[path = "mid_turn/certs.rs"]
mod certs;
#[path = "mid_turn/fake_cloud.rs"]
mod fake_cloud;

use fake_cloud::{ulid, FakeCloud, Machine};
use serde_json::{json, Value};
use session_core::api::CreateSession;
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::model::{Lifecycle, ScopeRoot, SessionView};
use session_core::paths::{Layout, Registration};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TUI: &str = env!("CARGO_BIN_EXE_tui");
/// The stand-in's conversation.
const CONVERSATION: &str = "3b0c7a52-9e1d-4f6a-8c2b-5d7e9f1a2b3c";
const MACHINE: &str = "e2e-machine";
const FIRST: &str = "Look at the lexer too, it drops the last token.";
const SECOND: &str = "Here are the screenshots from the web app.";
const THIRD: &str = "Then add a test for each fix.";
const ANSWER: &str =
    "Done: the parser and the lexer are fixed, the screenshots match, and each fix has a test.";
/// The longest any step may take before the test calls the queue stalled.
const BOUND: Duration = Duration::from_secs(30);
/// How long the test watches for a reply typed where none may go.
const HOLD: Duration = Duration::from_secs(5);

#[test]
fn replies_written_while_claude_code_works_go_in_one_at_a_time_across_a_restart() {
    let started = Instant::now();
    let world = World::new();
    let mut daemon = world.start_daemon();
    let client = world.client();
    let session = world.start_claude(&client);
    let mut service = world.start_service(1);
    world.wait(
        "the account service to list the session and read replies",
        || {
            let state = world.cloud.state();
            let listed = state.sessions.as_ref().is_some_and(|list| {
                list["sessions"].as_array().is_some_and(|sessions| {
                    sessions.iter().any(|listed| {
                        listed["id"] == session.id.as_str()
                            && listed["agent"] == "claude-code"
                            && listed["conversation_id"] == CONVERSATION
                    })
                })
            });
            listed && !state.reads.is_empty()
        },
    );

    // Written on rakumux.com while Claude Code works, oldest first.
    world.allow(1);
    let shown = (session.id.as_str(), session.name.as_str());
    let first = world.cloud.write_reply(shown, CONVERSATION, FIRST, &[]);
    let files = vec![("image.png", png(1, 300)), ("image.png", png(2, 500))];
    let second = world.cloud.write_reply(shown, CONVERSATION, SECOND, &files);
    let third = world.cloud.write_reply(shown, CONVERSATION, THIRD, &[]);

    // The first goes in mid-turn. Claude Code takes it at its next step,
    // where its next tool call asks for permission.
    world.wait("the first reply typed in", || world.received().len() == 1);
    world.wait("the first reply reported delivered", || {
        world.delivered(&first)
    });
    world.wait("Claude Code's permission prompt", || {
        client
            .get(&session.id)
            .is_ok_and(|view| view.lifecycle == Lifecycle::Blocked)
    });
    // Nothing goes in while it asks, though the second waits and the first
    // was taken: the prompt would take it as its answer.
    std::thread::sleep(HOLD);
    assert_eq!(
        world.received().len(),
        1,
        "a reply went in while Claude Code asked for permission\n{}",
        world.diagnostics()
    );
    world.touch("approve");

    // It works on: the second goes in, files first, without waiting for
    // Claude Code to stop (it won't), and waits in its queue.
    world.wait(
        "the second reply typed in while Claude Code works on",
        || world.received().len() == 2,
    );
    let typed = world.received()[1]["text"].as_str().unwrap().to_owned();
    let paths = check_files(&world, &second, &typed, &files);

    // The account service restarts while the second waits in the queue.
    service.stop();
    let reads = world.cloud.state().reads.len();
    service = world.start_service(2);
    world.wait("the restarted service to read replies", || {
        world.cloud.state().reads.len() >= reads + 2
    });
    std::thread::sleep(HOLD);
    assert_eq!(
        world.received().len(),
        2,
        "after the restart a reply went in beside the one still in Claude Code's queue\n{}",
        world.diagnostics()
    );
    assert!(
        world.cloud.state().reports_for(&second).is_empty(),
        "after the restart the reply in Claude Code's queue was reported before Claude Code took it\n{}",
        world.diagnostics()
    );

    // Claude Code takes it, logged as one paste block: delivered, and the
    // third goes in and is taken in turn.
    world.allow(3);
    world.wait(
        "the reply with files, taken as a paste, reported delivered",
        || world.delivered(&second),
    );
    world.wait("the third reply typed in", || world.received().len() == 3);
    world.wait("the third reply reported delivered", || {
        world.delivered(&third)
    });

    // Claude Code finishes: its answer names the last reply it took.
    world.touch("finish");
    world.wait("the turn's answer uploaded", || {
        !world.cloud.state().entries.is_empty()
    });
    // Anything else in flight lands.
    std::thread::sleep(Duration::from_secs(2));
    service.stop();
    let _ = client.shutdown();
    daemon.stop();

    let diagnostics = world.diagnostics();
    // Each typed once, in order, each into a queue holding nothing else,
    // while Claude Code worked; nothing while it asked.
    let received = world.received();
    let texts: Vec<&str> = received
        .iter()
        .map(|typed| typed["text"].as_str().unwrap())
        .collect();
    let with_files = format!("{SECOND}\n\n{}", paths.join("\n"));
    assert_eq!(texts, [FIRST, with_files.as_str(), THIRD], "{diagnostics}");
    for typed in &received {
        assert_eq!(typed["state"], "working", "{typed}\n{diagnostics}");
        assert_eq!(typed["queued"], 0, "{typed}\n{diagnostics}");
    }
    assert_eq!(world.typed_while_asking(), "", "{diagnostics}");
    // Each next one went in only once the one before was taken.
    let events = world.events();
    let at = |event: &str| -> u64 {
        events
            .iter()
            .find(|(_, name)| name == event)
            .unwrap_or_else(|| panic!("no {event:?}\n{diagnostics}"))
            .0
    };
    assert!(at("took 1") < at("received 2"), "{diagnostics}");
    assert!(at("took 2") < at("received 3"), "{diagnostics}");

    // Each reported delivered once, in order, after Claude Code took it.
    let state = world.cloud.state();
    let reports: Vec<(&str, &str, Option<&str>)> = state
        .reports
        .iter()
        .map(|report| {
            (
                report.id.as_str(),
                report.status.as_str(),
                report.failure.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        reports,
        [
            (first.as_str(), "delivered", None),
            (second.as_str(), "delivered", None),
            (third.as_str(), "delivered", None),
        ],
        "{diagnostics}"
    );
    for (n, id) in [(1, &first), (2, &second), (3, &third)] {
        let report = &state.reports_for(id)[0];
        assert!(
            report.at >= at(&format!("took {n}")),
            "reply {n} reported before Claude Code took it\n{diagnostics}"
        );
    }
    // Its files fetched once each.
    let attachments: Vec<String> = state
        .replies
        .iter()
        .find(|reply| reply.id == second)
        .unwrap()
        .attachments
        .iter()
        .map(|file| file["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(state.fetches, attachments, "{diagnostics}");

    // The one answer, to the last reply taken before it was said (the
    // others, taken earlier in the same turn, get none).
    assert_eq!(state.entries.len(), 1, "{diagnostics}");
    let entry = &state.entries[0];
    assert_eq!(entry["text"], ANSWER, "{diagnostics}");
    assert_eq!(entry["conversation_id"], CONVERSATION, "{diagnostics}");
    assert_eq!(entry["session"]["id"], session.id.as_str(), "{diagnostics}");
    assert_eq!(entry["reply_to"], json!(third), "{diagnostics}");
    eprintln!("mid-turn replies end to end: {:?}", started.elapsed());
}

/// The second reply's files: saved once each under the reply's own folder
/// (the second `image.png` as `image (2).png`), as fetched, and typed as
/// their paths after its text. Returns the paths.
fn check_files(world: &World, reply: &str, typed: &str, files: &[(&str, Vec<u8>)]) -> Vec<String> {
    let folder = world.layout.home.join("uploads").join(reply);
    let paths: Vec<String> = ["image.png", "image (2).png"]
        .iter()
        .map(|name| folder.join(name).display().to_string())
        .collect();
    assert_eq!(
        typed,
        format!("{SECOND}\n\n{}", paths.join("\n")),
        "{}",
        world.diagnostics()
    );
    for (path, (_, bytes)) in paths.iter().zip(files) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "{path}");
    }
    paths
}

/// A small file that starts as a PNG does.
fn png(seed: u8, size: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend((0..size - bytes.len()).map(|at| seed.wrapping_add(at as u8)));
    bytes
}

/// Everything the test runs, in one temp dir.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    layout: Layout,
    /// `HOME` for everything started here.
    home: PathBuf,
    /// `CLAUDE_CONFIG_DIR`.
    claude: PathBuf,
    /// The session's folder.
    work: PathBuf,
    /// The stand-in's screens and control files.
    stand_in: PathBuf,
    /// The CA the account service trusts for the local rakumux.com.
    ca: PathBuf,
    cloud: FakeCloud,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let layout = Layout::with_home(root.join("rakumux"));
        let home = root.join("home");
        let claude = root.join("claude");
        let work = root.join("work");
        let stand_in = root.join("stand-in");
        for path in [
            &home,
            &work,
            &claude.join("sessions"),
            &claude.join("projects").join(project_slug(&work)),
            &stand_in.join("screens"),
            &stand_in.join("control"),
            &root.join("bin"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        let certs = certs::TestCerts::new();
        let ca = root.join("ca.pem");
        std::fs::write(&ca, &certs.ca_pem).unwrap();
        let id = ulid();
        let secret = format!("rkm_{}", format!("{}{}", ulid(), ulid()).get(..43).unwrap());
        // An enrolled machine: its credential and its feed identity, as the
        // machine that enrolled it writes them.
        let sensagent = layout.home.join("sensagent");
        std::fs::create_dir_all(&sensagent).unwrap();
        std::fs::set_permissions(&sensagent, std::fs::Permissions::from_mode(0o700)).unwrap();
        private(
            &sensagent.join("machine-credential.json"),
            &json!({"version": 1, "machine_id": id, "secret": secret,
                "account": {"subject": "user_e2e", "email": "e2e@example.com",
                    "email_verified": true}}),
        );
        private(
            &sensagent.join("machine.json"),
            &json!({"version": 1, "id": id, "base": MACHINE, "renames": 0}),
        );
        let cloud = FakeCloud::start(
            &certs,
            Machine {
                name: MACHINE.into(),
                id,
                secret,
            },
        );
        let world = Self {
            _dir: dir,
            root,
            layout,
            home,
            claude,
            work,
            stand_in,
            ca,
            cloud,
        };
        world.install_stand_in();
        world
    }

    /// `claude` and Claude Code's screens, from the detection fixtures: at
    /// work (its activity line drawn by the stand-in), asking for
    /// permission, and idle after a turn.
    fn install_stand_in(&self) {
        let script = self.root.join("bin").join("claude");
        std::fs::write(&script, include_str!("mid_turn/claude.sh")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let screens = self.stand_in.join("screens");
        let working = fixture_screen("claude_working_model_turn");
        let (top, bottom) = working
            .split_once("✶ Pondering… (34s · ↓ 1.8k tokens)")
            .unwrap();
        std::fs::write(screens.join("working_top.txt"), top).unwrap();
        std::fs::write(screens.join("working_bottom.txt"), bottom).unwrap();
        std::fs::write(
            screens.join("asking.txt"),
            fixture_screen("claude_blocked_bash_permission"),
        )
        .unwrap();
        std::fs::write(
            screens.join("idle.txt"),
            fixture_screen("claude_idle_after_turn"),
        )
        .unwrap();
    }

    /// A command with this world's environment: its homes, never the
    /// session identity of whatever runs the test.
    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        for name in [
            session_core::env::SESSION_ID,
            session_core::env::SESSION_TOKEN,
            session_core::env::SOCKET,
            session_core::env::BIN,
            "XDG_RUNTIME_DIR",
            "RAKUMUX_CLOUD_ORIGIN",
            "RAKUMUX_CLOUD_CA",
        ] {
            command.env_remove(name);
        }
        command
            .env(session_core::env::HOME, &self.layout.home)
            .env("HOME", &self.home)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("SHELL", "/bin/bash")
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        command
    }

    fn start_daemon(&self) -> Running {
        let child = self
            .command(&ptyd_binary())
            .arg("serve")
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let running = Running(child);
        self.wait("the daemon to register", || {
            Registration::load(&self.layout).is_ok_and(|r| r.pid == pid && r.socket.exists())
        });
        running
    }

    fn client(&self) -> Client {
        Client::connect(ClientConfig {
            layout: self.layout.clone(),
            client_name: "e2e".into(),
            version: "test".into(),
            auth: AuthMode::User,
            spawn: false,
            ptyd_path: None,
        })
        .unwrap()
    }

    /// A session running the stand-in, once the daemon sees Claude Code at
    /// work in its conversation.
    fn start_claude(&self, client: &Client) -> SessionView {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let env = [
            ("PATH", path.as_str()),
            ("HOME", self.home.to_str().unwrap()),
            ("SHELL", "/bin/bash"),
            ("CLAUDE_CONFIG_DIR", self.claude.to_str().unwrap()),
            ("STAND_IN_DIR", self.stand_in.to_str().unwrap()),
            ("STAND_IN_CONVERSATION", CONVERSATION),
            ("STAND_IN_PROJECT", &project_slug(&self.work)),
            ("STAND_IN_ASK_AFTER", "1"),
            ("STAND_IN_ANSWER", ANSWER),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .to_vec();
        let created = client
            .create(CreateSession {
                scope: Some(ScopeRoot::resolve(&self.work).unwrap()),
                name: Some("fix-parser".into()),
                cwd: None,
                argv: Some(vec![self.root.join("bin/claude").display().to_string()]),
                startup_cmds: Vec::new(),
                env,
                cols: 80,
                rows: 24,
                machine: None,
                prefill: None,
            })
            .unwrap();
        let mut view = None;
        self.wait("Claude Code at work in its conversation", || {
            view = client.get(&created.id).ok();
            view.as_ref().is_some_and(|view| {
                view.agent
                    .as_ref()
                    .is_some_and(|agent| agent.kind == "claude")
                    && view.lifecycle == Lifecycle::Working
                    && view
                        .agent_turn
                        .as_ref()
                        .and_then(|turn| turn.conversation_id.as_deref())
                        == Some(CONVERSATION)
            })
        });
        view.unwrap()
    }

    /// `rakumux status-service` for this home, against the local
    /// rakumux.com (Clerk's settings point there too: nothing leaves the
    /// machine); its log is `status-service-<n>.log`.
    fn start_service(&self, n: usize) -> Running {
        let log = std::fs::File::create(self.root.join(format!("status-service-{n}.log"))).unwrap();
        let origin = &self.cloud.origin;
        let child = self
            .command(Path::new(TUI))
            .arg("status-service")
            .env("RAKUMUX_CLOUD_ORIGIN", origin)
            .env("RAKUMUX_CLOUD_CA", &self.ca)
            .env("RAKUMUX_CLERK_ISSUER", origin)
            .env("RAKUMUX_CLERK_DEVICE_PAGE", format!("{origin}/device"))
            .stderr(log)
            .spawn()
            .unwrap();
        Running(child)
    }

    /// Let the stand-in have taken `n` of what was typed by now.
    fn allow(&self, n: usize) {
        let control = self.stand_in.join("control");
        std::fs::write(control.join("allow.new"), n.to_string()).unwrap();
        std::fs::rename(control.join("allow.new"), control.join("allow")).unwrap();
    }

    fn touch(&self, name: &str) {
        std::fs::write(self.stand_in.join("control").join(name), "").unwrap();
    }

    /// What was typed into the stand-in, in order.
    fn received(&self) -> Vec<Value> {
        std::fs::read_to_string(self.stand_in.join("control/received.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The stand-in's events: when (Unix ms), and what.
    fn events(&self) -> Vec<(u64, String)> {
        std::fs::read_to_string(self.stand_in.join("control/events"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (at, event) = line.split_once(' ')?;
                Some((at.parse().ok()?, event.to_owned()))
            })
            .collect()
    }

    fn typed_while_asking(&self) -> String {
        std::fs::read_to_string(self.stand_in.join("control/typed_while_asking"))
            .unwrap_or_default()
    }

    /// Whether `reply` was reported delivered (and nothing else).
    fn delivered(&self, reply: &str) -> bool {
        let reports = self.cloud.state().reports_for(reply);
        assert!(
            reports.iter().all(|report| report.status == "delivered"),
            "{reports:?}\n{}",
            self.diagnostics()
        );
        !reports.is_empty()
    }

    /// Poll `check` until it holds, failing after [`BOUND`]: the queue
    /// stalled, or a step never happened.
    fn wait(&self, what: &str, mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + BOUND;
        while !check() {
            assert!(
                Instant::now() < deadline,
                "waited {BOUND:?} for {what}\n{}",
                self.diagnostics()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// What each part did, for a failure's message.
    fn diagnostics(&self) -> String {
        let read = |path: PathBuf| std::fs::read_to_string(path).unwrap_or_default();
        let tail = |text: String, lines: usize| {
            let all: Vec<&str> = text.lines().collect();
            all[all.len().saturating_sub(lines)..].join("\n")
        };
        let sessions: Vec<String> = Client::connect(ClientConfig {
            layout: self.layout.clone(),
            client_name: "e2e".into(),
            version: "test".into(),
            auth: AuthMode::User,
            spawn: false,
            ptyd_path: None,
        })
        .and_then(|client| client.list_all(false))
        .unwrap_or_default()
        .iter()
        .map(|view| {
            format!(
                "{} {:?} agent {:?} prompt {:?} turn {:?} foreground {:?}",
                view.name,
                view.lifecycle,
                view.agent.as_ref().map(|agent| &agent.kind),
                view.user_prompt,
                view.agent_turn
                    .as_ref()
                    .map(|turn| (turn.id, turn.conversation_id.clone())),
                view.terminal
                    .as_ref()
                    .and_then(|terminal| terminal.foreground_process.clone()),
            )
        })
        .collect();
        let state = self.cloud.state();
        let replies: Vec<String> = state
            .replies
            .iter()
            .map(|reply| {
                format!(
                    "{} seq {} {} {:?}",
                    reply.id, reply.seq, reply.status, reply.failure
                )
            })
            .collect();
        let services = (1..=2)
            .map(|n| tail(read(self.root.join(format!("status-service-{n}.log"))), 20))
            .collect::<Vec<_>>()
            .join("\n--\n");
        format!(
            "--- sessions:\n{}\n--- stand-in events:\n{}\n--- typed in:\n{}\n--- bytes typed: {:?}\n\
             --- replies:\n{}\n--- reports: {:?}\n--- entries: {:?}\n--- reads: {}\n\
             --- requests:\n{}\n--- status-service logs:\n{}\n--- daemon log:\n{}",
            sessions.join("\n"),
            self.events()
                .iter()
                .map(|(at, event)| format!("{at} {event}"))
                .collect::<Vec<_>>()
                .join("\n"),
            read(self.stand_in.join("control/received.jsonl")),
            read(self.stand_in.join("control/input")),
            replies.join("\n"),
            state.reports,
            state.entries,
            state.reads.len(),
            tail(state.log.join("\n"), 30),
            services,
            tail(read(self.layout.home.join("logs/ptyd.log")), 20),
        )
    }
}

/// A child process the test started, stopped (SIGTERM, then SIGKILL) when
/// dropped.
struct Running(std::process::Child);

impl Running {
    fn stop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            // SAFETY: kill(2) on our own child's pid.
            unsafe {
                libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = self.0.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Write `value` to a file only this user can read.
fn private(path: &Path, value: &Value) {
    std::fs::write(path, value.to_string()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Claude Code's folder for a project's conversations: its path, every
/// other character than a letter or digit a dash.
fn project_slug(folder: &Path) -> String {
    folder
        .display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The screen of a detection fixture (`crates/ptyd/tests/fixtures/detect/`).
fn fixture_screen(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/ptyd/tests/fixtures/detect")
        .join(format!("{name}.txt"));
    let text = std::fs::read_to_string(path).unwrap();
    text.split_once("\n---\n").unwrap().1.to_string()
}

/// The `rakumux-ptyd` of this build, next to the `tui` under test (built
/// when missing, as when this test runs without the rest of the workspace).
fn ptyd_binary() -> PathBuf {
    let profile = Path::new(TUI).parent().unwrap();
    let path = profile.join("rakumux-ptyd");
    if path.is_file() {
        return path;
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args([
            "build",
            "-p",
            "rakumux-ptyd",
            "--bin",
            "rakumux-ptyd",
            "--manifest-path",
        ])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .arg("--target-dir")
        .arg(profile.parent().unwrap());
    if profile.file_name().is_some_and(|name| name == "release") {
        command.arg("--release");
    }
    assert!(
        command.status().unwrap().success(),
        "cargo build -p rakumux-ptyd failed"
    );
    path
}
