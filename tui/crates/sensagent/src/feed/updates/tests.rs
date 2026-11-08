use super::*;

const ID: &str = "01K8A2B3C4D5E6F7G8H9J0K1M2";

fn ask(id: &str, version: &str) -> MachineUpdateAsk {
    MachineUpdateAsk {
        id: id.into(),
        version: version.into(),
    }
}

/// The status body a report would send.
fn body(work: Option<UpdateWork>) -> serde_json::Value {
    match work {
        Some(UpdateWork::Report(FeedRequest::UpdateStatus { body })) => {
            serde_json::from_slice(&body).unwrap()
        }
        other => panic!("not a report: {other:?}"),
    }
}

fn sync(path: Option<PathBuf>) -> UpdateSync {
    let mut sync = UpdateSync::new("studio".into(), true, path);
    sync.set_enabled(true, Duration::ZERO);
    sync
}

#[test]
fn the_release_and_the_update_are_read_only_within_the_contract() {
    let value = |json: &str| Some(serde_json::from_str::<serde_json::Value>(json).unwrap());
    assert_eq!(
        release(value(r#"{"version":"0.1.49"}"#)),
        Some("0.1.49".into())
    );
    for bad in [
        r#"{"version":"0.1.49-rc.1"}"#,
        r#"{"version":"v0.1.49"}"#,
        r#"{"version":"01.1.4"}"#,
        "[]",
    ] {
        assert_eq!(release(value(bad)), None, "{bad}");
    }
    assert_eq!(release(None), None);
    assert_eq!(
        asks(value(&format!(
            r#"[{{"id":"{ID}","version":"0.1.49"}},{{"id":"{ID}","version":"0.1.50"}}]"#
        ))),
        [ask(ID, "0.1.49")],
        "at most one"
    );
    assert!(asks(value(r#"[{"id":"x","version":"0.1.49"}]"#)).is_empty());
    assert!(asks(value(&format!(r#"[{{"id":"{ID}","version":"latest"}}]"#))).is_empty());
}

/// An update asked of this machine: reported `installing`, installed, reported
/// `updated`, then the new account service takes over; never installed twice.
#[test]
fn an_update_is_installed_once_reported_then_handed_over() {
    let mut sync = sync(None);
    sync.read(true, vec![ask(ID, "0.1.49")]);
    assert_eq!(
        sync.next_work(Duration::ZERO),
        Some(UpdateWork::Install {
            id: ID.into(),
            version: "0.1.49".into()
        })
    );
    assert_eq!(
        body(sync.next_work(Duration::ZERO)),
        serde_json::json!({"version": 1, "id": ID, "machine": "studio", "status": "installing"})
    );
    // The next read still brings it while it installs: not installed again.
    sync.read(true, vec![ask(ID, "0.1.49")]);
    assert_eq!(sync.next_work(Duration::ZERO), None, "a report is out");
    sync.reported(true, Ok(()), Duration::ZERO);
    assert_eq!(sync.next_work(Duration::ZERO), None, "installing");
    sync.installed(ID, Ok(()));
    assert_eq!(
        body(sync.next_work(Duration::ZERO)),
        serde_json::json!({"version": 1, "id": ID, "machine": "studio", "status": "updated"})
    );
    sync.reported(true, Ok(()), Duration::ZERO);
    assert_eq!(sync.next_work(Duration::ZERO), Some(UpdateWork::HandOver));
    sync.read(true, vec![ask(ID, "0.1.49")]);
    assert_eq!(sync.next_work(Duration::ZERO), None);
}

#[test]
fn a_failed_update_says_why_and_isnt_handed_over() {
    let mut sync = sync(None);
    sync.read(true, vec![ask(ID, "0.1.49")]);
    assert!(matches!(
        sync.next_work(Duration::ZERO),
        Some(UpdateWork::Install { .. })
    ));
    let _ = sync.next_work(Duration::ZERO);
    sync.reported(true, Ok(()), Duration::ZERO);
    sync.installed(ID, Err(UpdateFailure::Unsupported));
    assert_eq!(
        body(sync.next_work(Duration::ZERO)),
        serde_json::json!({"version": 1, "id": ID, "machine": "studio", "status": "failed", "failure": "unsupported"})
    );
    sync.reported(true, Ok(()), Duration::ZERO);
    assert_eq!(sync.next_work(Duration::ZERO), None);
}

/// A machine without an updater (it never says `remote_update`) takes none;
/// one signed out installs nothing.
#[test]
fn nothing_is_installed_without_an_updater_or_while_off() {
    let mut unable = UpdateSync::new("studio".into(), false, None);
    unable.set_enabled(true, Duration::ZERO);
    unable.read(true, vec![ask(ID, "0.1.49")]);
    assert_eq!(unable.next_work(Duration::ZERO), None);
    let mut off = UpdateSync::new("studio".into(), true, None);
    off.read(true, vec![ask(ID, "0.1.49")]);
    assert_eq!(off.next_work(Duration::ZERO), None);
    let mut stale = sync(None);
    stale.read(false, vec![ask(ID, "0.1.49")]);
    assert_eq!(
        stale.next_work(Duration::ZERO),
        None,
        "a read for an earlier sign-in"
    );
}

/// A restart meanwhile never installs it again, and still reports what was decided.
#[test]
fn what_was_handled_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    let mut first = sync(Some(path.clone()));
    first.read(true, vec![ask(ID, "0.1.49")]);
    assert!(matches!(
        first.next_work(Duration::ZERO),
        Some(UpdateWork::Install { .. })
    ));
    drop(first);
    let mut again = sync(Some(path.clone()));
    again.read(true, vec![ask(ID, "0.1.49")]);
    assert_eq!(
        body(again.next_work(Duration::ZERO)),
        serde_json::json!({"version": 1, "id": ID, "machine": "studio", "status": "installing"}),
        "reported, never installed again"
    );
    again.reported(true, Ok(()), Duration::ZERO);
    assert_eq!(again.next_work(Duration::ZERO), None);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

/// A report that fails for now is tried again later; one the cloud refuses
/// is dropped.
#[test]
fn reports_back_off_and_refused_ones_are_dropped() {
    let mut sync = sync(None);
    sync.read(true, vec![ask(ID, "0.1.49")]);
    let _ = sync.next_work(Duration::ZERO);
    let _ = sync.next_work(Duration::ZERO);
    sync.reported(true, Err(FeedFailure::Unavailable(None)), Duration::ZERO);
    assert_eq!(sync.next_work(Duration::from_secs(1)), None);
    assert!(matches!(
        sync.next_work(RETRY_MIN),
        Some(UpdateWork::Report(_))
    ));
    sync.reported(true, Err(FeedFailure::Invalid), RETRY_MIN);
    assert_eq!(sync.next_work(RETRY_MIN * 10), None);
}
