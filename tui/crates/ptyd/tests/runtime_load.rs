//! A busy machine, end to end through the real binary: how late the daemon handles what happens
//! in its sessions, and how long requests take, while 30 sessions run, 10 stand-in agents write
//! output and grow conversation logs of several MB that the daemon follows, and 10 callers wait
//! on sessions.
//!
//! Ignored by default (it runs for about a minute and prints numbers rather than checking much):
//!
//! ```text
//! cargo test -p rakumux-ptyd --test runtime_load --locked -- --ignored --nocapture
//! ```
//!
//! `RAKUMUX_LOAD_PTYD=<path>` runs another build of `rakumux-ptyd` instead, to compare two.
//!
//! What it measures:
//! - **event lag**: a probe session sets its terminal title to the time every 200 ms; the
//!   `updated` event carrying each title reaches a subscriber this much later. The title goes
//!   through the daemon's control-events thread behind every other session's output, so this is
//!   how far behind that thread runs.
//! - **get / list**: how long `session_get` and `session_list` take, each needing the state lock.
//! - **waits**: how many `session_wait` calls the 10 waiters completed (each times out after 1 s).

mod runtime_support;

use runtime_support::*;
use session_core::api::{
    CreateSession, EventEnvelope, Request, Response, SessionEvent, SubscribeRequest, WaitRequest,
    WaitUntil,
};
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionView};
use session_core::paths::Registration;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EXE: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");

/// Sessions on the machine: agents, sessions printing steadily, and idle shells.
const AGENTS: usize = 10;
/// Stand-in Codex sessions with no conversation open yet: the daemon keeps looking for one.
const CODEX: usize = 4;
const CHATTY: usize = 6;
const IDLE: usize = 9;
/// Callers each waiting on a session, again and again.
const WAITERS: usize = 10;
/// Each agent's conversation log, written before it starts.
const LOG_BYTES: usize = 8 * 1024 * 1024;
/// How long the load is measured, after the sessions settle.
const MEASURE: Duration = Duration::from_secs(40);
const SETTLE: Duration = Duration::from_secs(5);

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A stand-in Claude Code: names its conversation as Claude Code does, prints a line every
/// 100 ms and adds an edit to its conversation every third line. Only builtins run in its loop,
/// so it stays the session's foreground.
const AGENT: &str = r#"#!/bin/bash
set -u
conversation=$1
log=$CLAUDE_CONFIG_DIR/projects/load/$conversation.jsonl
printf '{"pid":%d,"sessionId":"%s","cwd":"%s"}\n' $$ "$conversation" "$PWD" > "$CLAUDE_CONFIG_DIR/sessions/$$.json"
n=0
started=$SECONDS
while (( SECONDS - started < 600 )); do
  n=$((n + 1))
  printf 'step %d: editing src/file_%d.rs, running the tests, reading the output again\r\n' $n $((n % 40))
  if (( n % 3 == 0 )); then
    printf '{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"%s","sessionId":"%s","version":"2.1.296","type":"assistant","message":{"id":"msg_%d","type":"message","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_%d","name":"Edit","input":{"file_path":"%s/src/file_%d.rs","old_string":"a","new_string":"b"}}],"stop_reason":null},"uuid":"live-%d-%d","timestamp":"2026-11-23T00:00:00.000Z"}\n' \
      "$PWD" "$conversation" $n $n "$PWD" $((n % 40)) $$ $n >> "$log"
  fi
  read -rt 0.1 _ || true
done
"#;

/// A stand-in Codex before its first prompt: working, but no conversation file open yet.
const CODEX_STAND_IN: &str = r#"#!/bin/bash
set -u
n=0
started=$SECONDS
while (( SECONDS - started < 600 )); do
  n=$((n + 1))
  printf 'codex thinking %d\r\n' $n
  read -rt 0.2 _ || true
done
"#;

fn start_daemon(home: &TestHome, env: &[(&str, &Path)]) -> (Daemon, Registration) {
    // Another build of the daemon to compare with (the same test, the same machine).
    let exe = std::env::var("RAKUMUX_LOAD_PTYD").unwrap_or_else(|_| EXE.to_owned());
    let child = Command::new(exe)
        .arg("serve")
        .envs(env.iter().map(|(k, v)| (*k, *v)))
        .env(session_core::env::HOME, &home.layout.home)
        .env(session_core::env::BIN, "rakumux")
        .env("RAKUMUX_PTYD_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rakumux-ptyd");
    let mut registration = None;
    assert!(
        wait_until(TIMEOUT, || {
            registration = Registration::load(&home.layout).ok();
            registration
                .as_ref()
                .is_some_and(|r| r.pid == child.id() && r.socket.exists())
        }),
        "daemon never registered"
    );
    (Daemon(child), registration.expect("registration"))
}

fn client(registration: &Registration) -> TestClient {
    let mut client = TestClient::connect(&registration.socket);
    assert!(matches!(
        client.hello(&registration.token),
        Response::Hello(_)
    ));
    client
}

fn create(client: &mut TestClient, home: &TestHome, name: &str, path: &str) -> SessionView {
    let mut env = home.env();
    env.retain(|(k, _)| k != "PATH");
    env.push(("PATH".into(), path.into()));
    let claude = home.dir.path().join("claude-config");
    env.push(("CLAUDE_CONFIG_DIR".into(), claude.display().to_string()));
    match client.request(&Request::SessionCreate(CreateSession {
        scope: Some(ScopeRoot::resolve(&home.work).expect("scope")),
        name: Some(name.into()),
        cwd: None,
        argv: Some(bash_argv()),
        startup_cmds: Vec::new(),
        env,
        cols: 120,
        rows: 30,
        machine: None,
        prefill: None,
        first_input: None,
        workspace: None,
    })) {
        Response::Session { session } => session,
        other => panic!("create failed: {other:?}"),
    }
}

/// A conversation log of about `bytes`: a prompt, then the agent's text and edits.
fn write_log(path: &Path, conversation: &str, cwd: &Path, bytes: usize) {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).expect("log"));
    let cwd = cwd.display();
    writeln!(
        file,
        r#"{{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"{cwd}","sessionId":"{conversation}","version":"2.1.296","type":"user","message":{{"role":"user","content":"Refactor the parser and keep the tests green."}},"uuid":"p-0","timestamp":"2026-11-22T00:00:00.000Z"}}"#
    )
    .expect("write");
    let filler = "The parser now reads each token once and keeps its position; ".repeat(20);
    let (mut written, mut n) = (0, 0usize);
    while written < bytes {
        n += 1;
        let line = if n % 2 == 0 {
            format!(
                r#"{{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"{cwd}","sessionId":"{conversation}","version":"2.1.296","type":"assistant","message":{{"id":"msg_{n}","type":"message","role":"assistant","model":"claude-opus-5-5","content":[{{"type":"text","text":"{filler}"}}],"stop_reason":null}},"uuid":"h-{n}","timestamp":"2026-11-22T00:00:00.000Z"}}"#
            )
        } else {
            format!(
                r#"{{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"{cwd}","sessionId":"{conversation}","version":"2.1.296","type":"assistant","message":{{"id":"msg_{n}","type":"message","role":"assistant","model":"claude-opus-5-5","content":[{{"type":"tool_use","id":"toolu_h{n}","name":"Edit","input":{{"file_path":"{cwd}/src/old_{}.rs","old_string":"a","new_string":"b"}}}}],"stop_reason":null}},"uuid":"h-{n}","timestamp":"2026-11-22T00:00:00.000Z"}}"#,
                n % 40
            )
        };
        writeln!(file, "{line}").expect("write");
        written += line.len() + 1;
    }
    file.flush().expect("flush");
}

/// The daemon's CPU time so far (user and system), in milliseconds.
fn cpu_ms(pid: u32) -> u64 {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0;
    };
    // After the command name in parentheses: state is field 3, utime 14 and stime 15.
    let Some(rest) = stat.rsplit(')').next() else {
        return 0;
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks: u64 = [11, 12]
        .iter()
        .filter_map(|&i| fields.get(i)?.parse::<u64>().ok())
        .sum();
    // Linux counts in clock ticks of 10 ms (USER_HZ 100).
    ticks * 10
}

/// The daemon's log lines about events it was slow to handle.
fn slow_lines(home: &TestHome) -> Vec<String> {
    std::fs::read_to_string(home.layout.home.join("logs/ptyd.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("control: slow "))
        .map(String::from)
        .collect()
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// p50, p95 and the largest of `samples` (milliseconds), or dashes for none.
fn summary(samples: &mut [f64]) -> String {
    if samples.is_empty() {
        return "no samples".into();
    }
    samples.sort_by(f64::total_cmp);
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    format!(
        "n={} p50={:.1} ms p95={:.1} ms max={:.1} ms",
        samples.len(),
        at(0.5),
        at(0.95),
        samples[samples.len() - 1]
    )
}

#[test]
#[ignore = "a minute-long load measurement; run with --ignored --nocapture"]
fn a_busy_machine_stays_responsive() {
    let home = TestHome::new();
    // The folder is a git checkout, so the agents' edits are places.
    std::fs::create_dir_all(home.work.join(".git")).expect(".git");
    std::fs::write(home.work.join(".git/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::create_dir_all(home.work.join("src")).expect("src");
    let bin = home.dir.path().join("bin");
    let claude = home.dir.path().join("claude-config");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::create_dir_all(claude.join("sessions")).expect("sessions");
    std::fs::create_dir_all(claude.join("projects/load")).expect("projects");
    for (name, body) in [("claude", AGENT), ("codex", CODEX_STAND_IN)] {
        let script = bin.join(name);
        std::fs::write(&script, body).expect("stand-in");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let work = std::fs::canonicalize(&home.work).expect("work");
    let conversations: Vec<String> = (0..AGENTS)
        .map(|i| format!("0c0a{i:04x}-1oad-4000-8000-00000000{i:04x}"))
        .collect();
    let started = Instant::now();
    for conversation in &conversations {
        let path = claude.join(format!("projects/load/{conversation}.jsonl"));
        write_log(&path, conversation, &work, LOG_BYTES);
    }
    eprintln!(
        "wrote {AGENTS} logs of {} MB in {:.1} s",
        LOG_BYTES / (1024 * 1024),
        started.elapsed().as_secs_f64()
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let path_value = PathBuf::from(&path);
    let (daemon, registration) = start_daemon(
        &home,
        &[
            ("CLAUDE_CONFIG_DIR", claude.as_path()),
            ("PATH", path_value.as_path()),
        ],
    );
    let mut user = client(&registration);

    // The sessions.
    let probe = create(&mut user, &home, "probe", &path);
    let mut agents = Vec::new();
    for i in 0..AGENTS {
        agents.push(create(&mut user, &home, &format!("agent {i}"), &path));
    }
    let mut codex = Vec::new();
    for i in 0..CODEX {
        codex.push(create(&mut user, &home, &format!("codex {i}"), &path));
    }
    let mut chatty = Vec::new();
    for i in 0..CHATTY {
        chatty.push(create(&mut user, &home, &format!("chatty {i}"), &path));
    }
    let mut idle = Vec::new();
    for i in 0..IDLE {
        idle.push(create(&mut user, &home, &format!("idle {i}"), &path));
    }
    for session in std::iter::once(&probe)
        .chain(&agents)
        .chain(&codex)
        .chain(&chatty)
        .chain(&idle)
    {
        user.wait_for_text(&session.id, "$");
    }
    for (session, conversation) in agents.iter().zip(&conversations) {
        user.submit(&session.id, &format!("claude {conversation}"));
    }
    for session in &codex {
        user.submit(&session.id, "codex");
    }
    for session in &chatty {
        user.submit(
            &session.id,
            "n=0; while :; do n=$((n+1)); echo \"line $n $RANDOM $RANDOM $RANDOM\"; read -rt 0.05 _; done",
        );
    }
    user.submit(
        &probe.id,
        r#"while :; do printf '\e]0;probe %s\a' "$EPOCHREALTIME"; read -rt 0.2 _; done"#,
    );
    thread::sleep(SETTLE);
    for session in agents.iter().take(2) {
        let view = match user.request(&Request::SessionGet {
            id: session.id.clone(),
        }) {
            Response::Session { session } => session,
            other => panic!("get: {other:?}"),
        };
        eprintln!(
            "{}: foreground {:?}, agent {:?}, lifecycle {:?}, output {:?}, places {}",
            view.name,
            view.terminal
                .as_ref()
                .and_then(|t| t.foreground_process.clone()),
            view.agent.as_ref().map(|a| a.kind.clone()),
            view.lifecycle,
            view.agent_output,
            view.places.len()
        );
    }

    let stop = Arc::new(AtomicBool::new(false));
    let measuring = Arc::new(AtomicBool::new(false));

    // Event lag: the probe's title, stamped with the time it was set.
    let lags = Arc::new(Mutex::new(Vec::<f64>::new()));
    let subscriber = {
        let (stop, measuring, lags, probe) = (
            stop.clone(),
            measuring.clone(),
            lags.clone(),
            probe.id.clone(),
        );
        let mut events = client(&registration);
        thread::spawn(move || {
            match events.request(&Request::Subscribe(SubscribeRequest {
                all: true,
                ..SubscribeRequest::default()
            })) {
                Response::Subscribed => {}
                other => panic!("subscribe: {other:?}"),
            }
            let _ = events
                .stream
                .set_read_timeout(Some(Duration::from_millis(500)));
            while !stop.load(Ordering::SeqCst) {
                let Ok(envelope) =
                    session_core::frame::read_control::<_, EventEnvelope>(&mut events.stream)
                else {
                    continue;
                };
                if envelope.session_id != probe || !measuring.load(Ordering::SeqCst) {
                    continue;
                }
                if let SessionEvent::Updated { session } = envelope.event {
                    let stamp = session
                        .terminal
                        .as_ref()
                        .and_then(|t| t.title.strip_prefix("probe "))
                        .and_then(|s| s.trim().parse::<f64>().ok());
                    if let Some(stamp) = stamp {
                        lags.lock()
                            .expect("lags")
                            .push((now_secs() - stamp) * 1000.0);
                    }
                }
            }
        })
    };

    // Requests that take the state lock.
    let gets = Arc::new(Mutex::new(Vec::<f64>::new()));
    let lists = Arc::new(Mutex::new(Vec::<f64>::new()));
    let requester = {
        let (stop, measuring, gets, lists, probe) = (
            stop.clone(),
            measuring.clone(),
            gets.clone(),
            lists.clone(),
            probe.id.clone(),
        );
        let mut asker = client(&registration);
        thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let at = Instant::now();
                let got = asker.request(&Request::SessionGet { id: probe.clone() });
                let get_ms = at.elapsed().as_secs_f64() * 1000.0;
                assert!(matches!(got, Response::Session { .. }), "{got:?}");
                let at = Instant::now();
                let listed = asker.request(&Request::SessionList {
                    scope: None,
                    scopes: Vec::new(),
                    include_dead: true,
                    all: true,
                });
                let list_ms = at.elapsed().as_secs_f64() * 1000.0;
                assert!(matches!(listed, Response::Sessions { .. }), "{listed:?}");
                if measuring.load(Ordering::SeqCst) {
                    gets.lock().expect("gets").push(get_ms);
                    lists.lock().expect("lists").push(list_ms);
                }
                thread::sleep(Duration::from_millis(50));
            }
        })
    };

    // Callers waiting on sessions that never get there: half on output that never shows, half
    // for an exit that never comes, each for a second at a time.
    let waits = Arc::new(AtomicU64::new(0));
    let waiters: Vec<_> = (0..WAITERS)
        .map(|i| {
            let (stop, waits) = (stop.clone(), waits.clone());
            let target: SessionId = idle[i % IDLE].id.clone();
            let mut waiter = client(&registration);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let request = if i % 2 == 0 {
                        WaitRequest {
                            id: target.clone(),
                            until: WaitUntil::Output,
                            pattern: Some("this never shows".into()),
                            timeout_ms: 1000,
                        }
                    } else {
                        WaitRequest {
                            id: target.clone(),
                            until: WaitUntil::Exited,
                            pattern: None,
                            timeout_ms: 1000,
                        }
                    };
                    match waiter.request(&Request::SessionWait(request)) {
                        Response::Wait(_) => {
                            waits.fetch_add(1, Ordering::SeqCst);
                        }
                        other => panic!("wait: {other:?}"),
                    }
                }
            })
        })
        .collect();

    let cpu_before = cpu_ms(daemon.0.id());
    let slow_before = slow_lines(&home);
    measuring.store(true, Ordering::SeqCst);
    let measured = Instant::now();
    let waits_before = waits.load(Ordering::SeqCst);
    thread::sleep(MEASURE);
    measuring.store(false, Ordering::SeqCst);
    let cpu = cpu_ms(daemon.0.id()).saturating_sub(cpu_before);
    let slow: Vec<String> = slow_lines(&home)
        .into_iter()
        .skip(slow_before.len())
        .collect();
    let waits_done = waits.load(Ordering::SeqCst) - waits_before;
    let elapsed = measured.elapsed().as_secs_f64();
    stop.store(true, Ordering::SeqCst);
    for waiter in waiters {
        waiter.join().expect("waiter");
    }
    requester.join().expect("requester");
    subscriber.join().expect("subscriber");

    let mut lags = lags.lock().expect("lags").clone();
    let mut gets = gets.lock().expect("gets").clone();
    let mut lists = lists.lock().expect("lists").clone();
    eprintln!("--- a busy machine: {AGENTS} agents, {CODEX} codex, {CHATTY} chatty, {IDLE} idle, probe; {WAITERS} waiters; {elapsed:.0} s");
    eprintln!("event lag     {}", summary(&mut lags));
    eprintln!("session_get   {}", summary(&mut gets));
    eprintln!("session_list  {}", summary(&mut lists));
    eprintln!(
        "waits         {waits_done} done ({:.1}/s; {WAITERS} waiters at most {:.1}/s)",
        waits_done as f64 / elapsed,
        WAITERS as f64
    );
    eprintln!(
        "daemon CPU    {:.1}% of one core ({cpu} ms in {elapsed:.0} s)",
        cpu as f64 / (elapsed * 10.0)
    );
    let mut slow_ms: Vec<f64> = slow
        .iter()
        .filter_map(|line| line.rsplit(": ").next()?.strip_suffix(" ms")?.parse().ok())
        .collect();
    let mut kinds: Vec<&str> = slow
        .iter()
        .filter_map(|line| line.split("control: slow ").nth(1)?.split(':').next())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    eprintln!("slow handling {} ({kinds:?})", summary(&mut slow_ms));
    // The probe sets its title five times a second: most of those must have arrived.
    assert!(
        lags.len() as f64 > elapsed * 2.0,
        "only {} probe titles arrived in {elapsed:.0} s",
        lags.len()
    );
    drop(user);
    drop(daemon);
}
