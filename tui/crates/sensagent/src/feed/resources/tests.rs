use super::*;
use session_core::api::{ClientUsage, SessionUsage};

const NOW: Duration = Duration::from_secs(100);
const LISTED: &str = "01K89ZQ4D2F6H8J0K2M4N6P8R2";
const PRIVATE: &str = "01K89ZQ4D2F6H8J0K2M4N6P8R4";
const IDLE: &str = "01K89ZQ4D2F6H8J0K2M4N6P8R6";

fn process(pid: u32, name: &str, cpu_ms: u64, rss: u64, net: Option<u64>) -> ProcessUsage {
    ProcessUsage {
        pid,
        parent: None,
        name: name.into(),
        command: format!("{name} --resume s3cr3t-conversation /home/me/private"),
        cpu_ms,
        rss_bytes: rss,
        net_rx_bytes: net.map(|bytes| bytes * 10),
        net_tx_bytes: net,
    }
}

fn client(client: &str, process: ProcessUsage) -> ClientUsage {
    ClientUsage {
        client: client.into(),
        connections: 1,
        process,
    }
}

/// A reading `scale` along: CPU and network counters grow with it.
fn sample(at_ms: i64, scale: u64) -> ResourceSample {
    ResourceSample {
        at_ms,
        machine: MachineUsage {
            cpus: 8,
            cpu_busy_ms: Some(1000 * scale),
            cpu_total_ms: Some(8000 * scale),
            memory_total_bytes: Some(16_000),
            memory_available_bytes: Some(6_000),
            net_rx_bytes: Some(500 * scale),
            net_tx_bytes: Some(50 * scale),
        },
        daemon: process(424_210, "rakumux-ptyd", 100 * scale, 30, Some(0)),
        clients: vec![
            client("tui", process(424_220, "tui", 200 * scale, 40, None)),
            client("tui", process(424_221, "tui", 0, 41, None)),
            client("api", process(424_240, "rakumux-api", 0, 12, Some(scale))),
            client(
                "rakumux-status-service",
                process(424_250, "tui", 10 * scale, 20, Some(2 * scale)),
            ),
            client("cli", process(424_260, "tui", 0, 9, None)),
        ],
        sessions: vec![
            SessionUsage {
                id: SessionId::from_string(LISTED.to_owned()),
                processes: vec![
                    process(424_230, "bash", 0, 5, Some(0)),
                    process(424_231, "claude", 900 * scale, 800, Some(100 * scale)),
                    process(424_232, "node", 10 * scale, 25, None),
                ],
            },
            SessionUsage {
                id: SessionId::from_string(PRIVATE.to_owned()),
                processes: vec![process(424_270, "vim", 50 * scale, 900, None)],
            },
            SessionUsage {
                id: SessionId::from_string(IDLE.to_owned()),
                processes: vec![process(424_280, "zsh", 0, 7, None)],
            },
        ],
    }
}

fn listed(id: &SessionId) -> bool {
    id.as_str() != PRIVATE
}

/// Enabled and wanted at `NOW`.
fn wanted() -> ResourceSync {
    let mut resources = ResourceSync::new();
    resources.set_enabled(true, NOW);
    resources.wanted(NOW);
    resources
}

fn upload(work: Option<ResourceWork>) -> serde_json::Value {
    match work {
        Some(ResourceWork::Upload(FeedRequest::Resources { body })) => {
            serde_json::from_slice(&body).unwrap()
        }
        other => panic!("expected an upload, got {other:?}"),
    }
}

/// Two readings two seconds apart: the upload made from them.
fn two_readings() -> (ResourceSync, serde_json::Value) {
    let mut resources = wanted();
    assert_eq!(resources.next_work(NOW), Some(ResourceWork::Read));
    resources.read_done(Ok(sample(1_761_800_000_000, 1)), "studio", &listed, NOW);
    let second = NOW + FIRST_RATES_AFTER;
    assert_eq!(resources.next_work(second), Some(ResourceWork::Read));
    resources.read_done(Ok(sample(1_761_800_002_000, 2)), "studio", &listed, second);
    let body = upload(resources.next_work(second));
    (resources, body)
}

#[test]
fn nothing_is_read_until_rakumux_com_wants_it() {
    let mut resources = ResourceSync::new();
    resources.set_enabled(true, NOW);
    assert_eq!(resources.next_work(NOW), None);
    // Asked while the account is off: nothing.
    let mut off = ResourceSync::new();
    off.wanted(NOW);
    assert_eq!(off.next_work(NOW), None);
    resources.wanted(NOW);
    assert_eq!(resources.next_work(NOW), Some(ResourceWork::Read));
    // One at a time.
    assert_eq!(resources.next_work(NOW), None);
}

#[test]
fn the_first_upload_waits_for_a_second_reading() {
    let mut resources = wanted();
    resources.next_work(NOW);
    resources.read_done(Ok(sample(1_761_800_000_000, 1)), "studio", &listed, NOW);
    // No rates from one reading: the next is asked for a tick later.
    assert_eq!(resources.next_work(NOW), None);
    let second = NOW + FIRST_RATES_AFTER;
    assert_eq!(resources.next_work(second), Some(ResourceWork::Read));
    resources.read_done(Ok(sample(1_761_800_002_000, 2)), "studio", &listed, second);
    upload(resources.next_work(second));
    // The upload is out: nothing else until it's done.
    assert_eq!(resources.next_work(second + EVERY), None);
    resources.done(true, Ok(true), second);
    assert_eq!(
        resources.next_work(second + EVERY),
        Some(ResourceWork::Read)
    );
}

#[test]
fn the_upload_has_the_machine_rakumuxs_roles_and_listed_sessions_with_rates() {
    let (_, body) = two_readings();
    assert_eq!(body["version"], 1);
    assert_eq!(body["machine"], "studio");
    assert_eq!(body["at"], 1_761_800_002_000_i64);
    assert_eq!(
        body["host"],
        serde_json::json!({
            "cpus": 8,
            "cpu_percent": 12.5,
            "memory_used_bytes": 10_000,
            "memory_total_bytes": 16_000,
            "net_rx_bytes_per_s": 250,
            "net_tx_bytes_per_s": 25,
        })
    );
    assert_eq!(
        body["rakumux"],
        serde_json::json!([
            {"role": "server", "processes": 1,
             "usage": {"cpu_percent": 5.0, "memory_bytes": 30, "net_rx_bytes_per_s": 0, "net_tx_bytes_per_s": 0}},
            {"role": "window", "processes": 2,
             "usage": {"cpu_percent": 10.0, "memory_bytes": 81}},
            {"role": "api", "processes": 1,
             "usage": {"cpu_percent": 0.0, "memory_bytes": 12, "net_rx_bytes_per_s": 5, "net_tx_bytes_per_s": 1}},
            {"role": "account_service", "processes": 1,
             "usage": {"cpu_percent": 0.5, "memory_bytes": 20, "net_rx_bytes_per_s": 10, "net_tx_bytes_per_s": 1}},
            {"role": "other", "processes": 1,
             "usage": {"cpu_percent": 0.0, "memory_bytes": 9}},
        ])
    );
    // Only the sessions the session list has, the most memory first; 900 ms
    // of CPU over 2 s is 45% of one CPU.
    assert_eq!(
        body["sessions"],
        serde_json::json!([
            {"id": LISTED, "processes": 3,
             "usage": {"cpu_percent": 45.5, "memory_bytes": 830, "net_rx_bytes_per_s": 500, "net_tx_bytes_per_s": 50}},
            {"id": IDLE, "processes": 1,
             "usage": {"cpu_percent": 0.0, "memory_bytes": 7}},
        ])
    );
}

#[test]
fn the_upload_never_carries_names_commands_pids_or_paths() {
    let (_, body) = two_readings();
    let text = body.to_string();
    for secret in [
        "claude",
        "bash",
        "rakumux-ptyd",
        "rakumux-api",
        "s3cr3t",
        "--resume",
        "/home",
        "4242",
        "tui",
        "rakumux-status-service",
        "cli",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    // The contract's members only.
    let keys = |value: &serde_json::Value| -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(
        keys(&body),
        ["at", "host", "machine", "rakumux", "sessions", "version"]
    );
    for role in body["rakumux"].as_array().unwrap() {
        assert_eq!(keys(role), ["processes", "role", "usage"]);
    }
    for session in body["sessions"].as_array().unwrap() {
        assert_eq!(keys(session), ["id", "processes", "usage"]);
    }
}

#[test]
fn a_reused_pid_gets_no_rate() {
    let earlier = process(31, "claude", 900, 1, None);
    let now = process(31, "sleep", 1000, 1, None);
    assert_eq!(rates_of(&now, Some(&earlier), Some(1.0)).cpu, None);
    assert_eq!(rates_of(&now, None, Some(1.0)).cpu, None);
    let now = process(31, "claude", 1000, 1, None);
    assert_eq!(rates_of(&now, Some(&earlier), Some(1.0)).cpu, Some(10.0));
}

#[test]
fn an_answer_saying_wanted_keeps_it_going_and_one_saying_not_stops_it() {
    let (mut resources, _) = two_readings();
    let second = NOW + FIRST_RATES_AFTER;
    // Kept going past the replies read's window by each upload's answer.
    let later = NOW + WANTED_FOR - Duration::from_secs(1);
    resources.done(true, Ok(true), later);
    assert_eq!(
        resources.next_work(NOW + WANTED_FOR + Duration::from_secs(5)),
        Some(ResourceWork::Read)
    );
    resources.read_done(
        Ok(sample(1_761_800_004_000, 3)),
        "studio",
        &listed,
        NOW + WANTED_FOR + Duration::from_secs(5),
    );
    upload(resources.next_work(NOW + WANTED_FOR + Duration::from_secs(5)));
    // rtop closed: stops at once, and forgets the reading.
    resources.done(true, Ok(false), second + EVERY);
    assert_eq!(resources.next_work(second + EVERY * 2), None);
    assert!(resources.previous.is_none());
    // Asked again later, the first upload waits for two new readings.
    let again = second + Duration::from_secs(120);
    resources.wanted(again);
    assert_eq!(resources.next_work(again), Some(ResourceWork::Read));
    resources.read_done(Ok(sample(1_761_800_120_000, 60)), "studio", &listed, again);
    assert_eq!(resources.next_work(again), None);
}

#[test]
fn readings_stop_once_nobody_said_they_are_wanted_for_a_while() {
    let mut resources = wanted();
    resources.next_work(NOW);
    resources.read_done(Ok(sample(1_761_800_000_000, 1)), "studio", &listed, NOW);
    assert_eq!(resources.next_work(NOW + WANTED_FOR), None);
    assert!(resources.previous.is_none(), "rates never span a pause");
    // A replies read saying so again (within the window) extends it.
    let mut resources = wanted();
    resources.wanted(NOW + Duration::from_secs(20));
    assert_eq!(
        resources.next_work(NOW + Duration::from_secs(40)),
        Some(ResourceWork::Read)
    );
}

#[test]
fn failed_uploads_back_off() {
    let (mut resources, _) = two_readings();
    let at = NOW + FIRST_RATES_AFTER;
    resources.done(true, Err(FeedFailure::Unavailable(None)), at);
    assert_eq!(resources.next_work(at + Duration::from_secs(4)), None);
    assert_eq!(
        resources.next_work(at + RETRY_MIN),
        Some(ResourceWork::Read)
    );
    resources.read_done(
        Ok(sample(1_761_800_004_000, 3)),
        "studio",
        &listed,
        at + RETRY_MIN,
    );
    upload(resources.next_work(at + RETRY_MIN));
    // Rate limited with a delay: that delay, however short the backoff.
    let later = at + RETRY_MIN;
    resources.done(
        true,
        Err(FeedFailure::RateLimited(Some(Duration::from_secs(20)))),
        later,
    );
    assert_eq!(resources.next_work(later + Duration::from_secs(19)), None);
    // An upload for an account no longer signed in changes nothing.
    let (mut resources, _) = two_readings();
    resources.done(false, Err(FeedFailure::Unauthorized), at);
    assert_eq!(resources.next_work(at + EVERY), Some(ResourceWork::Read));
}

#[test]
fn an_older_daemon_isnt_asked_again_until_it_reconnects() {
    let mut resources = wanted();
    resources.next_work(NOW);
    resources.read_done(Err(ErrorCode::Unsupported), "studio", &listed, NOW);
    assert_eq!(resources.next_work(NOW + Duration::from_secs(10)), None);
    resources.connected();
    assert_eq!(
        resources.next_work(NOW + Duration::from_secs(10)),
        Some(ResourceWork::Read)
    );
    // Another failure: asked again in a while.
    resources.read_done(
        Err(ErrorCode::Internal),
        "studio",
        &listed,
        NOW + Duration::from_secs(10),
    );
    assert_eq!(resources.next_work(NOW + Duration::from_secs(11)), None);
    assert_eq!(
        resources.next_work(NOW + Duration::from_secs(10) + EVERY),
        Some(ResourceWork::Read)
    );
}

#[test]
fn turning_the_account_off_forgets_everything() {
    let (mut resources, _) = two_readings();
    resources.done(true, Ok(true), NOW + FIRST_RATES_AFTER);
    resources.set_enabled(false, NOW + EVERY);
    assert!(resources.previous.is_none());
    assert_eq!(resources.next_work(NOW + EVERY * 2), None);
    resources.set_enabled(true, NOW + EVERY * 2);
    assert_eq!(resources.next_work(NOW + EVERY * 2), None, "not wanted yet");
}
