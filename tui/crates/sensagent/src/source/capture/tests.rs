use session_core::model::{Lifecycle, ReportResult};

use super::*;

fn kept(text: &str) -> Option<String> {
    window(text).unwrap()
}

#[test]
fn windows_keep_plain_text_without_surrounding_blank_rows() {
    assert_eq!(
        kept("\n\n  $ cargo test\n  ok: 3 passed\n\n\n   \n").as_deref(),
        Some("$ cargo test\n  ok: 3 passed")
    );
    for blank in ["", " \n\t\r\n ", "\u{fe0f}\n\u{200d}"] {
        assert_eq!(kept(blank), None);
    }
}

#[test]
fn windows_drop_only_emoji_presentation_characters() {
    assert_eq!(
        kept("\u{26a0}\u{fe0f} Build ok \u{1f469}\u{200d}\u{1f4bb} \u{2764}\u{fe0e}").as_deref(),
        Some("\u{26a0} Build ok \u{1f469}\u{1f4bb} \u{2764}")
    );
    // Other invisibles stay, so the context builder still rejects the window.
    assert_eq!(
        kept("pass\u{200b}word").as_deref(),
        Some("pass\u{200b}word")
    );
    // Deleting a joiner only lets redaction see the whole assignment.
    assert_eq!(
        kept("pass\u{200d}word=synthetic").as_deref(),
        Some("password=synthetic")
    );
}

#[test]
fn windows_keep_the_tail_within_the_inspection_bound_from_a_whole_line() {
    let lines: Vec<String> = (0..4000)
        .map(|number| format!("synthetic line {number:05}"))
        .collect();
    let text = lines.join("\n");
    assert!(text.len() > MAX_INSPECTION_BYTES);
    let kept = kept(&text).unwrap();
    assert!(kept.len() <= MAX_INSPECTION_BYTES);
    assert!(kept.starts_with("synthetic line "));
    assert!(lines.contains(&kept.lines().next().unwrap().to_owned()));
    assert!(kept.ends_with("synthetic line 03999"));

    let exact = format!("{}\n", ".".repeat(MAX_INSPECTION_BYTES - 1));
    let kept = window(&format!("discarded\n{exact}")).unwrap().unwrap();
    assert_eq!(kept.len(), MAX_INSPECTION_BYTES - 1);
}

#[test]
fn windows_of_one_long_line_end_on_a_character_boundary() {
    let text = "\u{e9}".repeat(MAX_INSPECTION_BYTES);
    let kept = kept(&text).unwrap();
    assert!(kept.len() <= MAX_INSPECTION_BYTES);
    assert!(kept.len() >= MAX_INSPECTION_BYTES - 1);
    assert!(kept.chars().all(|character| character == '\u{e9}'));
}

#[test]
fn windows_drop_a_private_key_whose_begin_line_was_cut_off() {
    let orphan =
        "MIIEsyntheticKEYbody\nMoreSyntheticKeyBody\n-----END RSA PRIVATE KEY-----\n$ next";
    assert_eq!(kept(orphan).as_deref(), Some("$ next"));
    let paired = "-----BEGIN PRIVATE KEY-----\nSyntheticBody\n-----END PRIVATE KEY-----\nafter";
    assert_eq!(kept(paired).as_deref(), Some(paired));
    let both = format!("TailOfAnotherKey\n-----END EC PRIVATE KEY-----\nbetween\n{paired}");
    assert_eq!(kept(&both), Some(format!("between\n{paired}")));
    assert_eq!(kept("body\n-----END PRIVATE KEY-----"), None);

    // Cutting at the inspection bound can orphan a key that the full text paired.
    let key = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\nvisible",
        "K".repeat(MAX_INSPECTION_BYTES)
    );
    assert_eq!(kept(&key).as_deref(), Some("visible"));
}

#[test]
fn windows_mask_long_base64_lines_whose_key_markers_are_off_screen() {
    let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7syntheticKEY+/abc";
    let tail = "c3ludGhldGljLWtleS10YWls==";
    let text = format!("$ sed -n 5,7p synthetic_key\n{body}\n  {body}  \n{tail}\n$ ");
    assert_eq!(
        kept(&text).as_deref(),
        Some("$ sed -n 5,7p synthetic_key\n[REDACTED]\n[REDACTED]\nc3ludGhldGljLWtleS10YWls==\n$")
    );
    let numbered = format!("     5\t{body}\n12: {body}\n7 \u{2502} {body}\n");
    assert_eq!(
        kept(&numbered).as_deref(),
        Some("[REDACTED]\n[REDACTED]\n[REDACTED]")
    );
    // Ordinary long lines with spaces or punctuation are kept.
    let prose = format!("{} done", "word ".repeat(20));
    assert_eq!(kept(&prose).as_deref(), Some(prose.as_str()));
    let url = format!("https://example.invalid/{}", "a".repeat(80));
    assert_eq!(kept(&url).as_deref(), Some(url.as_str()));
}

#[test]
fn reports_use_text_and_summary_but_never_file_lists() {
    let reported = Reported {
        state: Some(Lifecycle::Done),
        text: Some("  Synthetic report text \n".into()),
        result: Some(ReportResult {
            summary: "Synthetic summary".into(),
            files: vec!["/synthetic/private/file.rs".into()],
        }),
        at: 1,
    };
    assert_eq!(
        report_text(&reported).as_deref(),
        Some("Synthetic report text\nSynthetic summary")
    );
    let blank = Reported {
        state: None,
        text: Some(" ".into()),
        result: Some(ReportResult {
            summary: "\n".into(),
            files: vec!["/synthetic/private/file.rs".into()],
        }),
        at: 1,
    };
    assert_eq!(report_text(&blank), None);
}

#[test]
fn daemon_failures_map_to_static_categories() {
    for (code, expected) in [
        (ErrorCode::NotFound, CaptureError::Changed),
        (ErrorCode::SessionNotLive, CaptureError::Changed),
        (ErrorCode::Unauthenticated, CaptureError::NotPermitted),
        (ErrorCode::OutOfScope, CaptureError::NotPermitted),
        (ErrorCode::PermissionDenied, CaptureError::NotPermitted),
        (ErrorCode::Internal, CaptureError::Unavailable),
        (ErrorCode::ProtocolMismatch, CaptureError::Unavailable),
    ] {
        let error = failure(ApiError::new(code, "synthetic-private-daemon-diagnostic"));
        assert_eq!(error, expected);
        let diagnostic = format!("{error} {error:?}");
        assert!(!diagnostic.contains("synthetic-private"));
    }
}

#[test]
fn captured_text_is_redacted_from_debug() {
    let captured = SessionCapture {
        request: "synthetic-private-request".into(),
        session: SessionId::from_string("synthetic-private-session"),
        read_report: true,
        read_output: true,
        stamp: SourceStamp {
            terminal_generation: 1,
            source_revision: 1,
        },
        metadata: CaptureMetadata {
            lifecycle: Lifecycle::Working,
            activity_phase: None,
        },
        reported_text: Some("synthetic-private-report".into()),
        recent_output: Some("synthetic-private-output".into()),
    };
    assert_eq!(
        format!("{captured:?}"),
        "SessionCapture { text: [REDACTED] }"
    );
    let input = captured.input();
    assert_eq!(input.reported_text, Some("synthetic-private-report"));
    assert_eq!(input.recent_output, Some("synthetic-private-output"));
}
