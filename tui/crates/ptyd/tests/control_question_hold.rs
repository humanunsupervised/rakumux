//! A question Claude Code asks (AskUserQuestion: its `PreToolUse` hook says
//! `NeedsInput`) holds the session `blocked` until the conversation the
//! daemon follows moves on after it: answering writes the tool's result,
//! Esc the refused result and the interruption. Its hooks send nothing while
//! the dialog waits, and the screen check that took over once they went
//! quiet for two minutes once read a dialog as waiting for a prompt, so a
//! reply from rakumux.com was typed into it (2026-10-30).
//!
//! Its own test binary: it points `CLAUDE_CONFIG_DIR` at a folder of its own,
//! and nothing else in this process reads the environment.
#![cfg(target_os = "linux")]

mod control_common;

use control_common::*;
use ptyd::detect::{DetectInput, Detection};
use ptyd::host::ProcessInfo;
use serde_json::{json, Value};
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};
use session_core::api::{ErrorCode, Report, Request, Response};
use session_core::ids::SessionId;
use session_core::model::Lifecycle;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const CONVERSATION: &str = "8d3f6a10-2c4b-4e7f-9a1d-5b6c7d8e9f01";
/// Claude Code's pid as the stand-in shows it: only its `sessions/` file is read.
const PID: u32 = 4343;

/// Claude Code running, its screen read by markers: the incident's dialog
/// read as `[idle]`, a prompt waiting.
fn claude_detector(input: &DetectInput<'_>) -> Detection {
    Detection {
        agent: Some("claude".into()),
        ..marker_detector(input)
    }
}

/// The system's clock, moved on by `skip` (ms).
fn clock(skip: &Arc<AtomicI64>) -> ptyd::control::Clock {
    let skip = skip.clone();
    Arc::new(move || now_ms() + skip.load(Ordering::SeqCst))
}

/// `ms` (Unix) as Claude Code writes its entries' times.
fn rfc3339(ms: i64) -> String {
    let (secs, millis) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let (era, day_of_era) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Claude Code `pid`'s conversation's log, as Claude Code keeps it, in a
/// config folder of its own (`CLAUDE_CONFIG_DIR`, kept while the folder is).
fn conversation_log(pid: u32, conversation: &str) -> (tempfile::TempDir, File) {
    let folder = tempfile::tempdir().unwrap();
    let config = folder.path();
    std::env::set_var("CLAUDE_CONFIG_DIR", config);
    std::fs::create_dir_all(config.join("sessions")).unwrap();
    std::fs::write(
        config.join("sessions").join(format!("{pid}.json")),
        json!({"sessionId": conversation, "pid": pid}).to_string(),
    )
    .unwrap();
    let project = config.join("projects").join("-home-me-project");
    std::fs::create_dir_all(&project).unwrap();
    let log = File::create(project.join(format!("{conversation}.jsonl"))).unwrap();
    (folder, log)
}

fn entry(log: &mut File, entry: Value) {
    writeln!(log, "{entry}").unwrap();
}

fn user(uuid: &str, at: i64, content: Value) -> Value {
    json!({"type": "user", "uuid": uuid, "timestamp": rfc3339(at), "sessionId": CONVERSATION,
        "message": {"role": "user", "content": content}})
}

fn needs_input() -> Report {
    Report {
        activity: Some(ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: CONVERSATION.into(),
            completion_id: None,
            event: ActivityEvent::Phase {
                phase: ActivityPhase::NeedsInput,
            },
        }),
        ..Report::default()
    }
}

/// As the account service types a reply in: quiet, expecting Claude Code,
/// refused while its agent asks something.
fn reply(id: &SessionId, text: &str) -> Request {
    Request::SessionSubmit {
        id: id.clone(),
        text: text.into(),
        quiet_ms: Some(10_000),
        expect_foreground: Some("claude".into()),
        refuse_if_blocked: true,
    }
}

#[test]
fn rfc3339_writes_claude_codes_times() {
    assert_eq!(rfc3339(1_793_367_870_177), "2026-10-30T13:44:30.177Z");
    assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00.000Z");
}

#[test]
fn a_question_holds_until_its_conversation_moves_on_however_long_its_hooks_stay_quiet() {
    let (_config, mut log) = conversation_log(PID, CONVERSATION);
    let skip = Arc::new(AtomicI64::new(0));
    let h = Harness::with_clock(claude_detector, clock(&skip));
    // The daemon's log, in the test's home (this binary's only logger).
    ptyd::log::init(&h.layout);
    let logged = || std::fs::read_to_string(h.layout.log_file()).unwrap_or_default();
    let now = || now_ms() + skip.load(Ordering::SeqCst);
    let id = h.create(Some("claude")).id;
    let terminal = h.host.terminal_of(&id);
    h.host.set_foreground(
        &id,
        ProcessInfo {
            pid: PID,
            name: "claude".into(),
            cmdline: vec!["claude".into()],
            exe: None,
        },
    );
    // The turn so far, followed by the daemon (read on output, at most once
    // a second).
    entry(
        &mut log,
        user("u1", now() - 60_000, json!("Plan the release notes")),
    );
    let mut frame = 0;
    let mut idle_screen = |h: &Harness| {
        frame += 1;
        h.host.show(&id, &format!("> [idle] {frame}"));
    };
    eventually("the conversation followed", || {
        idle_screen(&h);
        (h.get(&id).user_prompt.as_deref() == Some("Plan the release notes")).then_some(())
    });

    // 13:44:30: AskUserQuestion. Its PreToolUse hook says NeedsInput; the
    // screen, as detection reads it, waits for a prompt.
    let asked_at = now();
    let inside = h.session_caller(&id);
    assert_eq!(
        h.handle(&inside, Request::SessionReport(needs_input())),
        Response::Ok
    );
    assert_eq!(h.get(&id).lifecycle, Lifecycle::Blocked);
    // Claude Code logs the call (dated before its hook), then its hook's
    // success and its own bookkeeping while the dialog waits: none of it is
    // the conversation moving on.
    entry(
        &mut log,
        json!({"type": "assistant", "uuid": "a1", "timestamp": rfc3339(asked_at - 40),
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1",
                "name": "AskUserQuestion", "input": {"questions": []}}]}}),
    );
    entry(
        &mut log,
        json!({"type": "attachment", "uuid": "h1", "timestamp": rfc3339(asked_at + 20),
            "attachment": {"type": "hook_success", "hookName": "PreToolUse:AskUserQuestion",
                "hookEvent": "PreToolUse"}}),
    );
    entry(
        &mut log,
        json!({"type": "cost-state", "sessionId": CONVERSATION}),
    );
    entry(
        &mut log,
        json!({"type": "last-prompt", "sessionId": CONVERSATION}),
    );

    // Five minutes without a hook, the screen still read as a prompt.
    for minute in 1..=5 {
        skip.fetch_add(60_000, Ordering::SeqCst);
        idle_screen(&h);
        // Ticks every 250 ms re-evaluate the lifecycle on the moved clock.
        std::thread::sleep(Duration::from_millis(1_100));
        assert_eq!(
            h.get(&id).lifecycle,
            Lifecycle::Blocked,
            "{minute} min: the question still waits"
        );
    }
    // 13:48:35: the reply from rakumux.com isn't typed into the dialog.
    let refused = error(h.handle(&h.user, reply(&id, "Ship it on Monday")));
    assert_eq!(refused.code, ErrorCode::SessionBlocked);
    assert!(h.host.terminal(terminal).submits.is_empty());

    // 13:49:08: Esc. Claude Code writes the refused result and the
    // interruption; the agent waits for a prompt.
    let esc_at = now();
    entry(
        &mut log,
        user(
            "u2",
            esc_at,
            json!([{"type": "tool_result", "tool_use_id": "toolu_1", "is_error": true,
                "content": "The user doesn't want to proceed with this tool use."}]),
        ),
    );
    entry(
        &mut log,
        user(
            "u3",
            esc_at + 2,
            json!([{"type": "text", "text": "[Request interrupted by user for tool use]"}]),
        ),
    );
    idle_screen(&h);
    eventually("the screen decides again", || {
        (h.get(&id).lifecycle == Lifecycle::Idle).then_some(())
    });
    assert!(h.get(&id).activity.is_none(), "the question long past");
    assert_eq!(
        h.handle(&h.user, reply(&id, "Ship it on Monday")),
        Response::Ok
    );
    h.host.enter_due(&id);
    let typed = h.host.terminal(terminal);
    assert_eq!(typed.submits, ["Ship it on Monday"]);
    assert_eq!(typed.enters, [true]);

    // One line for the submit that went in, never its text: what the daemon
    // and the screen said, to tell a reply typed into a question.
    let logged = logged();
    let lines: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains("guarded submit"))
        .collect();
    assert_eq!(lines.len(), 1, "{logged}");
    for part in [
        format!("typed into {id}:"),
        "lifecycle Idle".into(),
        "hooks none".into(),
        "screen Idle (claude)".into(),
        "foreground claude".into(),
        "size 80x24".into(),
    ] {
        assert!(lines[0].contains(&part), "{part}: {}", lines[0]);
    }
    assert!(!logged.contains("Ship it"), "{logged}");
}
