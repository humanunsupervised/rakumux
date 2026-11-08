//! A machine's warnings and errors reaching the owner's log viewer, end to end: the real daemon, the
//! real account service (signed in as an enrolled machine) and a local rakumux.com
//! (`mid_turn/fake_cloud.rs`). Asked for logs by its features read, the service follows the files in
//! `logs/`, masks each line and uploads it; it starts at each file's end, never reads the input log,
//! and sends nothing while it isn't asked.
#![cfg(target_os = "linux")]

#[path = "mid_turn/certs.rs"]
mod certs;
#[path = "mid_turn/fake_cloud.rs"]
mod fake_cloud;

use fake_cloud::{ulid, FakeCloud, Machine};
use serde_json::{json, Value};
use session_core::paths::{Layout, Registration};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TUI: &str = env!("CARGO_BIN_EXE_tui");
const MACHINE: &str = "e2e-logs";
const BOUND: Duration = Duration::from_secs(90);

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
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
        let _ = self.0.wait();
    }
}

fn private(path: &Path, value: &Value) {
    std::fs::write(path, value.to_string()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn ptyd_binary() -> PathBuf {
    Path::new(TUI).with_file_name("rakumux-ptyd")
}

fn command(program: &Path, layout: &Layout) -> Command {
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
        .env(session_core::env::HOME, &layout.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    command
}

fn append(path: &Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

fn uploaded(cloud: &FakeCloud) -> Vec<Value> {
    cloud
        .state()
        .logs
        .iter()
        .flat_map(|upload| upload["events"].as_array().cloned().unwrap_or_default())
        .collect()
}

#[test]
fn a_machines_warnings_and_errors_reach_the_log_viewer_masked() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let layout = Layout::with_home(root.join("rakumux"));
    let logs = layout.home.join("logs");
    let certs = certs::TestCerts::new();
    let ca = root.join("ca.pem");
    std::fs::write(&ca, &certs.ca_pem).unwrap();
    let id = ulid();
    let secret = format!("rkm_{}", format!("{}{}", ulid(), ulid()).get(..43).unwrap());
    let sensagent = layout.home.join("sensagent");
    std::fs::create_dir_all(&sensagent).unwrap();
    std::fs::set_permissions(&sensagent, std::fs::Permissions::from_mode(0o700)).unwrap();
    private(
        &sensagent.join("machine-credential.json"),
        &json!({"version": 1, "machine_id": id, "secret": secret,
            "account": {"subject": "user_e2e", "email": "e2e@example.com", "email_verified": true}}),
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
    let wait = |what: &str, check: &mut dyn FnMut() -> bool| {
        let deadline = Instant::now() + BOUND;
        while !check() {
            assert!(
                Instant::now() < deadline,
                "waited {BOUND:?} for {what}\n--- requests:\n{}\n--- service log:\n{}",
                cloud.state().log.join("\n"),
                std::fs::read_to_string(logs.join("status-service.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    };

    let daemon = Running(
        command(&ptyd_binary(), &layout)
            .arg("serve")
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = daemon.0.id();
    wait("the daemon to register", &mut || {
        Registration::load(&layout).is_ok_and(|r| r.pid == pid && r.socket.exists())
    });
    let ptyd_log = logs.join("ptyd.log");
    append(
        &ptyd_log,
        "2026-10-13T08:15:01.000Z ERROR [old] from before it was asked\n",
    );
    std::fs::create_dir_all(&logs).unwrap();
    let _service = Running(
        command(Path::new(TUI), &layout)
            .arg("status-service")
            .env("RAKUMUX_CLOUD_ORIGIN", &cloud.origin)
            .env("RAKUMUX_CLOUD_CA", &ca)
            .env("RAKUMUX_CLERK_ISSUER", &cloud.origin)
            .env(
                "RAKUMUX_CLERK_DEVICE_PAGE",
                format!("{}/device", cloud.origin),
            )
            .stderr(std::fs::File::create(logs.join("status-service.log")).unwrap())
            .spawn()
            .unwrap(),
    );

    // Not asked: its session list goes up, and no logs do.
    wait("the session list", &mut || cloud.state().sessions.is_some());
    append(
        &ptyd_log,
        "2026-10-13T08:15:02.000Z ERROR [old] still not asked\n",
    );
    std::thread::sleep(Duration::from_secs(2));
    assert!(uploaded(&cloud).is_empty());

    cloud.state().logs_level = Some("warn");
    // The features read comes every 2 minutes; restarting the service reads it at once.
    drop(_service);
    let _service = Running(
        command(Path::new(TUI), &layout)
            .arg("status-service")
            .env("RAKUMUX_CLOUD_ORIGIN", &cloud.origin)
            .env("RAKUMUX_CLOUD_CA", &ca)
            .env("RAKUMUX_CLERK_ISSUER", &cloud.origin)
            .env(
                "RAKUMUX_CLERK_DEVICE_PAGE",
                format!("{}/device", cloud.origin),
            )
            .stderr(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(logs.join("status-service.log"))
                    .unwrap(),
            )
            .spawn()
            .unwrap(),
    );
    let state = layout.home.join("sensagent/status/logs.json");
    wait("the log files followed", &mut || {
        std::fs::read_to_string(&state).is_ok_and(|text| text.contains("ptyd.log"))
    });

    let home = std::env::var("HOME").unwrap();
    append(
        &ptyd_log,
        &format!(
            "2026-10-13T08:15:03.000Z ERROR [control] cannot save {home}/x: token=abc123def456ghi\n\
             2026-10-13T08:15:04.000Z INFO  [control] chatter\n"
        ),
    );
    append(
        &logs.join("api.log"),
        "2026-10-13T08:15:05Z WARN listener closed\n",
    );
    append(
        &logs.join("tui.log"),
        "2026-10-13T08:15:06.000Z ERROR [panic] window panicked\n",
    );
    append(
        &logs.join("mcp.log"),
        "2026-10-13T08:15:07.000Z ERROR [server] stdio closed\n",
    );
    append(
        &logs.join("status-service.log"),
        "rakumux-status: saving session titles failed\n",
    );
    append(
        &logs.join("tui-input.log"),
        "2026-10-13T08:15:08.000Z ERROR [keys] pressed s3cret\n",
    );

    wait("every program's line", &mut || uploaded(&cloud).len() >= 5);
    let events = uploaded(&cloud);
    let find = |source: &str| {
        events
            .iter()
            .find(|event| event["source"] == source)
            .unwrap_or_else(|| panic!("no {source} event in {events:?}"))
    };
    let daemon_event = find("daemon");
    assert_eq!(daemon_event["level"], "error");
    assert_eq!(daemon_event["component"], "control");
    let message = daemon_event["message"].as_str().unwrap();
    assert!(message.starts_with("cannot save ~/x"), "{message}");
    assert!(!message.contains("abc123def456ghi"), "{message}");
    assert_eq!(find("api")["message"], "listener closed");
    assert_eq!(find("tui")["message"], "window panicked");
    assert_eq!(find("mcp")["message"], "stdio closed");
    assert_eq!(find("account_service")["level"], "warn");

    let text = serde_json::to_string(&events).unwrap();
    for absent in ["from before", "still not asked", "chatter", "s3cret"] {
        assert!(!text.contains(absent), "{absent} in {text}");
    }
    let upload = cloud.state().logs[0].clone();
    assert_eq!(upload["machine"], MACHINE);
    assert!(upload["platform"].as_str().unwrap().starts_with("linux-"));
}
