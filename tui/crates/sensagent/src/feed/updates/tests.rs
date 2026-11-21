use super::*;

#[test]
fn the_release_is_read_only_within_the_contract() {
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
}
