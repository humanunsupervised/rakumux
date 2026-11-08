//! A session's AI title once the program started at its prompt has run 1 s,
//! end to end: the real daemon (`rakumux-ptyd serve`), the real account
//! service (`rakumux status-service`, signed in as an enrolled machine, AI
//! session titles on) and a local rakumux.com (`mid_turn/fake_cloud.rs`,
//! HTTPS through `RAKUMUX_CLOUD_CA`). Temp dirs only; no network beyond
//! 127.0.0.1, no real account or `~/.rakumux`.
//!
//! A new session ("New session") starts at its login shell's prompt. A
//! program (`sleep 120`) gets the session's title a second after it started,
//! made of its screen, where the command that started it shows, and the
//! generated name becomes that title.
//!
//! Runs with `cargo test --workspace` (about 15 s), or alone with
//! `cargo test -p tui --test app_titles -- --nocapture`. Linux only, like the
//! other end-to-end tests with a local rakumux.com.
#![cfg(target_os = "linux")]

#[path = "mid_turn/certs.rs"]
mod certs;
// Its reply routes and reports serve the reply tests, not this one.
#[allow(dead_code)]
#[path = "mid_turn/fake_cloud.rs"]
mod fake_cloud;

use fake_cloud::{now_ms, ulid, FakeCloud, Machine};
use serde_json::{json, Value};
use session_core::api::CreateSession;
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionView};
use session_core::paths::{Layout, Registration};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TUI: &str = env!("CARGO_BIN_EXE_tui");
const MACHINE: &str = "e2e-machine";
const TITLE: &str = "Watching the build";
/// How long a program runs before its session is titled (`APP_RUNNING`).
const APP_RUNNING: Duration = Duration::from_secs(1);
/// The service ticks every second and reads views as the daemon publishes
/// them: a title asked for this much sooner than `APP_RUNNING` is too soon.
const SLACK: Duration = Duration::from_millis(500);
/// The longest any step may take.
const BOUND: Duration = Duration::from_secs(45);

#[test]
fn a_program_started_at_the_prompt_titles_its_session_after_a_second() {
    let world = World::new();
    let _daemon = world.start_daemon();
    let client = world.client();
    let session = world.start_shell(&client);
    assert!(session.auto_named, "{}", world.diagnostics());
    assert_eq!(session.name, "New session");
    let mut service = world.start_service();
    world.wait("the account service to list the session", || {
        world.cloud.state().sessions.as_ref().is_some_and(|list| {
            list["sessions"].as_array().is_some_and(|sessions| {
                sessions
                    .iter()
                    .any(|listed| listed["id"] == session.id.as_str())
            })
        })
    });

    // At the shell's prompt: no title.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        world.cloud.state().titles.is_empty(),
        "a title was asked for at the shell's prompt\n{}",
        world.diagnostics()
    );
    client.submit(&session.id, "sleep 120").unwrap();
    let started = world.foreground(&client, &session.id, "sleep");
    world.wait("the session's title asked for", || {
        !world.cloud.state().titles.is_empty()
    });
    let (asked, body) = world.cloud.state().titles[0].clone();
    let after = Duration::from_millis(asked.saturating_sub(started));
    assert!(
        after + SLACK >= APP_RUNNING,
        "asked {after:?} after `sleep 120` started\n{}",
        world.diagnostics()
    );
    assert!(
        after <= APP_RUNNING + Duration::from_secs(5),
        "asked only {after:?} after `sleep 120` started\n{}",
        world.diagnostics()
    );
    // Made of the screen, where the command shows.
    assert!(
        body["context"]["recent_output"]
            .as_str()
            .is_some_and(|output| output.contains("sleep 120")),
        "{body}\n{}",
        world.diagnostics()
    );
    world.wait("the session named by its title", || {
        client.get(&session.id).is_ok_and(|view| view.name == TITLE)
    });

    // Its one automatic title: nothing more while it runs on.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        world.cloud.state().titles.len(),
        1,
        "{}",
        world.diagnostics()
    );
    service.stop();
    let _ = client.shutdown();
}

/// Everything the test runs, in one temp dir.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    layout: Layout,
    /// `HOME` for everything started here.
    home: PathBuf,
    /// The session's folder.
    work: PathBuf,
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
        let work = root.join("work");
        for path in [&home, &work] {
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
        {
            let mut state = cloud.state();
            state.summaries = true;
            state.title = TITLE.into();
        }
        Self {
            _dir: dir,
            root,
            layout,
            home,
            work,
            ca,
            cloud,
        }
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
            .env("CLAUDE_CONFIG_DIR", self.home.join(".claude"))
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

    /// A new session at its login shell's prompt, with a generated name.
    fn start_shell(&self, client: &Client) -> SessionView {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let env = [
            ("PATH", path.as_str()),
            ("HOME", self.home.to_str().unwrap()),
            ("SHELL", "/bin/bash"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .to_vec();
        let created = client
            .create(CreateSession {
                scope: Some(ScopeRoot::resolve(&self.work).unwrap()),
                name: None,
                cwd: None,
                argv: None,
                startup_cmds: Vec::new(),
                env,
                cols: 80,
                rows: 24,
                machine: None,
                prefill: None,
            })
            .unwrap();
        self.wait("the login shell in the foreground", || {
            self.foreground_is(client, &created.id, "bash")
        });
        client.get(&created.id).unwrap()
    }

    /// `rakumux status-service` for this home, against the local
    /// rakumux.com; its log is `status-service.log`.
    fn start_service(&self) -> Running {
        let log = std::fs::File::create(self.root.join("status-service.log")).unwrap();
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

    fn foreground_is(&self, client: &Client, id: &SessionId, program: &str) -> bool {
        client.get(id).is_ok_and(|view| {
            view.terminal
                .as_ref()
                .and_then(|terminal| terminal.foreground_process.as_deref())
                == Some(program)
        })
    }

    /// Wait for `program` in the session's foreground; when it was seen
    /// there (Unix ms).
    fn foreground(&self, client: &Client, id: &SessionId, program: &str) -> u64 {
        self.wait(&format!("`{program}` in the foreground"), || {
            self.foreground_is(client, id, program)
        });
        now_ms()
    }

    /// Poll `check` until it holds, failing after [`BOUND`].
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
        let sessions: Vec<String> = self
            .client()
            .list_all(false)
            .unwrap_or_default()
            .iter()
            .map(|view| {
                format!(
                    "{} auto_named {} {:?} foreground {:?}",
                    view.name,
                    view.auto_named,
                    view.lifecycle,
                    view.terminal
                        .as_ref()
                        .and_then(|terminal| terminal.foreground_process.clone()),
                )
            })
            .collect();
        let state = self.cloud.state();
        let titles: Vec<String> = state
            .titles
            .iter()
            .map(|(at, body)| format!("{at} {body}"))
            .collect();
        format!(
            "--- sessions:\n{}\n--- title requests:\n{}\n--- requests:\n{}\n\
             --- status-service log:\n{}\n--- daemon log:\n{}",
            sessions.join("\n"),
            titles.join("\n"),
            tail(state.log.join("\n"), 30),
            tail(read(self.root.join("status-service.log")), 30),
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
