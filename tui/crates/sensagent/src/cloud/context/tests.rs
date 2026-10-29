use super::*;
use crate::status::{
    AISessionStatusService, AccountBinding, AccountGate, AccountKey, CachedStatus, ConsentPolicy,
    ConsentRevision, ConsentSnapshot, ContextAvailability, FolderConsent, Preparation, SourceStamp,
    StatusError, StatusSource, StatusStore, StatusStoreError,
};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Default)]
struct Store(ConsentSnapshot);

impl StatusStore for Store {
    fn load_consent(&mut self) -> Result<ConsentSnapshot, StatusStoreError> {
        Ok(self.0.clone())
    }
    fn compare_and_swap_consent(
        &mut self,
        expected: ConsentRevision,
        policy: &ConsentPolicy,
    ) -> Result<ConsentSnapshot, StatusStoreError> {
        if expected != self.0.revision {
            return Err(StatusStoreError::Conflict);
        }
        self.0 = ConsentSnapshot {
            revision: ConsentRevision(expected.0 + 1),
            policy: policy.clone(),
        };
        Ok(self.0.clone())
    }
    fn load_cache(&mut self, _: &SessionId) -> Result<Option<CachedStatus>, StatusStoreError> {
        Ok(None)
    }
    fn save_cache(&mut self, _: ConsentRevision, _: &CachedStatus) -> Result<(), StatusStoreError> {
        Err(StatusStoreError::InvalidData)
    }
    fn remove_cache(&mut self, _: &SessionId) -> Result<(), StatusStoreError> {
        Ok(())
    }
}

#[derive(Clone)]
struct Gate {
    checks: Arc<AtomicUsize>,
    deny_at: Arc<AtomicUsize>,
    fail_at: Arc<AtomicUsize>,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            checks: Arc::new(AtomicUsize::new(0)),
            deny_at: Arc::new(AtomicUsize::new(usize::MAX)),
            fail_at: Arc::new(AtomicUsize::new(usize::MAX)),
        }
    }
}
impl AccountGate for Gate {
    fn current(&self) -> Result<Option<AccountBinding>, StatusError> {
        let count = self.checks.fetch_add(1, Ordering::SeqCst);
        if count >= self.fail_at.load(Ordering::SeqCst) {
            return Err(StatusError::AccountUnavailable);
        }
        if count >= self.deny_at.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(Some(binding()))
    }
}

fn binding() -> AccountBinding {
    AccountBinding {
        account: AccountKey {
            issuer: "https://issuer.example/".into(),
            client_id: "native-client".into(),
            subject: "synthetic-subject".into(),
        },
        auth_revision: 2,
    }
}

fn ticket(output: bool) -> Box<GenerationTicket> {
    owner_and_ticket(output).1
}

fn owner_and_ticket(
    output: bool,
) -> (
    AISessionStatusService<Store, Gate>,
    Box<GenerationTicket>,
    Gate,
) {
    let folder = ScopeRoot::from_canonical(PathBuf::from("/synthetic/folder"));
    let session = SessionId::generate();
    let gate = Gate::default();
    let mut owner = AISessionStatusService::new(Store::default(), gate.clone());
    owner
        .observe(StatusSource {
            session_id: session.clone(),
            folder: folder.clone(),
            stamp: SourceStamp {
                terminal_generation: 1,
                source_revision: 1,
            },
            private: false,
            live: true,
            context: ContextAvailability {
                reported_text: true,
                recent_output: true,
            },
        })
        .unwrap();
    owner
        .enable(
            ConsentRevision(0),
            &binding(),
            vec![FolderConsent {
                folder,
                include_terminal_output: output,
            }],
        )
        .unwrap();
    let Preparation::Ready(ticket) = owner.prepare(&session).unwrap() else {
        panic!("missing ticket");
    };
    (owner, ticket, gate)
}

fn metadata() -> CaptureMetadata {
    CaptureMetadata {
        lifecycle: Lifecycle::Working,
        activity_phase: Some(ActivityPhase::RunningTool),
    }
}

fn build(
    output: bool,
    report: Option<&str>,
    terminal: Option<&str>,
) -> Result<ValidatedStatusRequest, CloudAIError> {
    ContextBuilder::build(
        &ticket(output),
        metadata(),
        CaptureInput {
            reported_text: report,
            recent_output: terminal,
        },
        &[],
    )
}

fn value(request: &ValidatedStatusRequest) -> Value {
    serde_json::from_slice(request.body()).unwrap()
}

#[test]
fn capture_profile_compiles_and_only_canonical_fields_leave_the_builder() {
    assert!(PATTERNS.as_ref().is_ok());
    let ticket = ticket(false);
    let request = ContextBuilder::build(
        &ticket,
        metadata(),
        CaptureInput {
            reported_text: Some("Checking synthetic changes"),
            recent_output: None,
        },
        &[],
    )
    .unwrap();
    assert_eq!(request.request_id(), ticket.request_id());
    assert_eq!(
        value(&request),
        serde_json::json!({ "version": 1, "request_id": ticket.request_id(), "context": {
            "lifecycle": "working", "activity_phase": "running_tool", "reported_text": "Checking synthetic changes",
        }})
    );
    let encoded = std::str::from_utf8(request.body()).unwrap();
    for private in [
        "synthetic-subject",
        "/synthetic/folder",
        "session_id",
        "auth_revision",
        "source_revision",
        "credential",
    ] {
        assert!(!encoded.contains(private));
    }
}

#[test]
fn reports_only_jobs_reject_supplied_terminal_output_even_when_empty() {
    for terminal in ["", "secret terminal content", " \t\r\n"] {
        assert_eq!(
            build(false, Some("Checking"), Some(terminal)).unwrap_err(),
            CloudAIError::DataNotPermitted
        );
    }
}

#[test]
fn output_requires_its_own_selection_and_unknown_activity_is_omitted() {
    let request = ContextBuilder::build(
        &ticket(true),
        CaptureMetadata {
            lifecycle: Lifecycle::Unknown,
            activity_phase: Some(ActivityPhase::Unknown),
        },
        CaptureInput {
            reported_text: Some("Checking"),
            recent_output: Some("Synthetic checks passed\n"),
        },
        &[],
    )
    .unwrap();
    let body = value(&request);
    assert_eq!(body["context"]["lifecycle"], "unknown");
    assert!(body["context"].get("activity_phase").is_none());
    assert_eq!(
        body["context"]["recent_output"],
        "Synthetic checks passed\n"
    );
}

#[test]
fn all_known_lifecycle_and_activity_values_preserve_the_native_vocabulary() {
    for lifecycle in [
        Lifecycle::Working,
        Lifecycle::Blocked,
        Lifecycle::Done,
        Lifecycle::Idle,
        Lifecycle::Unknown,
        Lifecycle::None,
    ] {
        for phase in [
            ActivityPhase::Working,
            ActivityPhase::WaitingForResponse,
            ActivityPhase::RunningTool,
            ActivityPhase::NeedsInput,
            ActivityPhase::Idle,
            ActivityPhase::Done,
        ] {
            let request = ContextBuilder::build(
                &ticket(false),
                CaptureMetadata {
                    lifecycle,
                    activity_phase: Some(phase),
                },
                CaptureInput {
                    reported_text: Some("Checking"),
                    recent_output: None,
                },
                &[],
            )
            .unwrap();
            assert_eq!(
                value(&request)["context"]["lifecycle"],
                serde_json::to_value(lifecycle).unwrap()
            );
            assert_eq!(
                value(&request)["context"]["activity_phase"],
                serde_json::to_value(phase).unwrap()
            );
        }
    }
}

#[test]
fn selection_uses_utf8_byte_bounded_recent_tails_without_normalization() {
    let report = format!("old{} café", "é".repeat(2_000));
    let output = format!("old{} 検証 e\u{301}", "界".repeat(4_000));
    let request = build(true, Some(&report), Some(&output)).unwrap();
    let body = value(&request);
    let kept_report = body["context"]["reported_text"].as_str().unwrap();
    let kept_output = body["context"]["recent_output"].as_str().unwrap();
    assert!(kept_report.len() <= MAX_REPORT_BYTES && kept_report.ends_with(" café"));
    assert!(kept_output.len() <= MAX_OUTPUT_BYTES && kept_output.ends_with(" e\u{301}"));
    assert!(!kept_report.starts_with("old") && !kept_output.starts_with("old"));
    assert!(request.body().len() <= 16_384);
}

#[test]
fn inspection_limit_applies_before_redaction_or_tail_selection() {
    let maximum = "a".repeat(MAX_INSPECTION_BYTES);
    assert!(build(false, Some(&maximum), None).is_ok());
    assert_eq!(
        build(false, Some(&(maximum + "a")), None).unwrap_err(),
        CloudAIError::CaptureTooLarge
    );
    assert_eq!(
        build(
            true,
            Some("Checking"),
            Some(&"a".repeat(MAX_INSPECTION_BYTES + 1))
        )
        .unwrap_err(),
        CloudAIError::CaptureTooLarge
    );
}

#[test]
fn known_secrets_are_redacted_longest_first_before_any_truncation() {
    let secret = SecretValue::try_from("X".repeat(3_000)).unwrap();
    let prefix = SecretValue::try_from("X".repeat(100)).unwrap();
    let raw = format!("{}; checking synthetic updates", secret.expose_secret());
    let request = ContextBuilder::build(
        &ticket(false),
        metadata(),
        CaptureInput {
            reported_text: Some(&raw),
            recent_output: None,
        },
        &[&prefix, &secret],
    )
    .unwrap();
    let text = value(&request)["context"]["reported_text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(text, "[REDACTED]; checking synthetic updates");
    assert!(!text.contains("XXXX"));
}

#[test]
fn environment_assignments_bearers_and_known_token_formats_are_masked() {
    for secret_line in [
        "OPENAI_API_KEY=synthetic-env-value",
        "RAKUMUX_SESSION_TOKEN='synthetic-local-token'",
        "CLERK_SECRET_KEY=synthetic-key",
        "Authorization: Bearer synthetic-bearer-value",
        "\"access_token\": \"synthetic-json-token\"",
        "sk-abcdefghijklmnopqrstuvwxyz123456",
        // Split so secret scanners don't take these synthetic keys for real ones.
        concat!("sk_live_", "abcdefghijklmnopqrstuvwxyz"),
        concat!("ghp_", "abcdefghijklmnopqrstuvwxyz1234"),
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.syntheticSignature",
        // An enrolled machine's secret (ending in a `-`) and access token.
        concat!("rkm_", "3q2-7wF6m0ZxQh9b1y4Ku8LpN5sTvR2cJdE0aBfGhI-"),
        concat!("rkma.v1.", "eyJzdWIiOiJ1c2VyXzEyMyJ9.c2lnbmF0dXJlLXNpZ25hdHVyZQ"),
    ] {
        let input = format!("Checking synthetic updates\n{secret_line}");
        let request = build(false, Some(&input), None).unwrap();
        let text = value(&request)["context"]["reported_text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("[REDACTED]"), "{secret_line}");
        assert!(!text.contains(secret_line));
        assert!(!text.contains("synthetic-json-token"));
    }
}

#[test]
fn private_key_blocks_are_masked_including_a_truncated_block_to_eof() {
    for block in [
        "-----BEGIN PRIVATE KEY-----\nsynthetic-base64-key\n-----END PRIVATE KEY-----",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nsynthetic-truncated-key",
    ] {
        assert_eq!(
            build(false, Some(block), None).unwrap_err(),
            CloudAIError::NoContext
        );
        let input = format!("Checking updates\n{block}");
        let request = build(false, Some(&input), None).unwrap();
        assert_eq!(
            value(&request)["context"]["reported_text"],
            "Checking updates\n[REDACTED]"
        );
    }
}

#[test]
fn common_absolute_paths_are_masked_but_urls_are_not_fetched_or_rewritten() {
    let input = "Editing /home/synthetic/repo/file.rs and C:\\Users\\synthetic\\repo\\file.rs; see https://docs.example/usage";
    let request = build(false, Some(input), None).unwrap();
    let text = value(&request)["context"]["reported_text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!text.contains("/home/synthetic") && !text.contains("C:\\Users"));
    assert!(text.contains("[PATH]") && text.contains("https://docs.example/usage"));
    assert_eq!(
        build(false, Some("/home/synthetic/private-file"), None).unwrap_err(),
        CloudAIError::NoContext
    );
}

#[test]
fn blank_mark_only_and_redaction_only_sources_produce_no_request() {
    for input in [
        None,
        Some(""),
        Some(" \t\r\n"),
        Some("\u{301}\u{308}"),
        Some("[REDACTED] [PATH]"),
        Some("password=synthetic-password"),
    ] {
        assert_eq!(
            build(false, input, None).unwrap_err(),
            CloudAIError::NoContext
        );
    }
}

#[test]
fn prohibited_controls_and_invisibles_are_rejected_not_silently_stripped() {
    for character in [
        '\u{1b}', '\0', '\u{7f}', '\u{9b}', '\u{202e}', '\u{200b}', '\u{fe0f}',
    ] {
        assert_eq!(
            build(false, Some(&format!("Checking{character}updates")), None).unwrap_err(),
            CloudAIError::InvalidRequest
        );
        assert_eq!(
            build(
                true,
                Some("Safe report"),
                Some(&format!("Output{character}"))
            )
            .unwrap_err(),
            CloudAIError::InvalidRequest
        );
    }
}

#[test]
fn multiline_context_preserves_canonical_tabs_newlines_and_unicode_line_separators() {
    let input = "Checking\tupdates\r\nNext line\u{2028}paragraph\u{2029}done";
    let request = build(false, Some(input), None).unwrap();
    assert_eq!(value(&request)["context"]["reported_text"], input);
}

#[test]
fn tail_selection_never_turns_a_partial_mask_into_false_context() {
    for mask in MASKS {
        let input = format!("prefix {mask} suffix");
        let marker = input.find(mask).unwrap();
        for start in marker + 1..marker + mask.len() {
            assert_eq!(
                tail_without_partial_masks(&input, input.len() - start),
                " suffix"
            );
        }
        assert_eq!(tail_without_partial_masks(mask, mask.len() - 1), "");
    }
}

#[test]
fn json_escape_overhead_cannot_bypass_the_complete_request_body_cap() {
    let report = "\"".repeat(MAX_REPORT_BYTES);
    let output = "\\".repeat(MAX_OUTPUT_BYTES);
    assert_eq!(
        build(true, Some(&report), Some(&output)).unwrap_err(),
        CloudAIError::InvalidRequest
    );
}

#[test]
fn capture_debug_never_discloses_text_and_secret_lists_are_bounded() {
    let input = CaptureInput {
        reported_text: Some("sensitive synthetic report"),
        recent_output: None,
    };
    assert!(!format!("{input:?} {input:#?}").contains("sensitive synthetic"));
    let secret = SecretValue::try_from("synthetic-known-secret".to_owned()).unwrap();
    let secrets = vec![&secret; MAX_KNOWN_SECRETS + 1];
    assert_eq!(
        ContextBuilder::build(&ticket(false), metadata(), input, &secrets).unwrap_err(),
        CloudAIError::CaptureTooLarge
    );
}

#[test]
fn known_secrets_cannot_destroy_private_key_delimiters_before_block_redaction() {
    let secret = SecretValue::try_from("PRIVATE".to_owned()).unwrap();
    let key = SecretValue::try_from("KEY".to_owned()).unwrap();
    let input = "Checking updates\n-----BEGIN PRIVATE KEY-----\nsynthetic-key-material\n-----END PRIVATE KEY-----";
    let request = ContextBuilder::build(
        &ticket(false),
        metadata(),
        CaptureInput {
            reported_text: Some(input),
            recent_output: None,
        },
        &[&secret, &key],
    )
    .unwrap();
    assert_eq!(
        value(&request)["context"]["reported_text"],
        "Checking updates\n[REDACTED]"
    );
}

#[test]
fn short_known_secrets_do_not_rewrite_or_expand_replacement_markers() {
    let first = SecretValue::try_from("R".to_owned()).unwrap();
    let second = SecretValue::try_from("E".to_owned()).unwrap();
    let secrets: Vec<_> = [&first, &second]
        .into_iter()
        .cycle()
        .take(MAX_KNOWN_SECRETS)
        .collect();
    let input = format!("{} checking updates", "RE".repeat(30_000));
    let request = ContextBuilder::build(
        &ticket(false),
        metadata(),
        CaptureInput {
            reported_text: Some(&input),
            recent_output: None,
        },
        &secrets,
    )
    .unwrap();
    assert_eq!(
        value(&request)["context"]["reported_text"],
        "[REDACTED] checking updates"
    );
}

#[test]
fn checked_capture_accepts_only_a_current_owned_job() {
    let (mut owner, ticket, _) = owner_and_ticket(false);
    let input = CaptureInput {
        reported_text: Some("Checking updates"),
        recent_output: None,
    };
    let request =
        ContextBuilder::build_current(&mut owner, &ticket, metadata(), input, &[]).unwrap();
    assert_eq!(request.request_id(), ticket.request_id());
    let (_, foreign, _) = owner_and_ticket(false);
    assert_eq!(
        ContextBuilder::build_current(&mut owner, &foreign, metadata(), input, &[]).unwrap_err(),
        CloudAIError::Canceled
    );
    owner.disable(ConsentRevision(1)).unwrap();
    assert_eq!(
        ContextBuilder::build_current(&mut owner, &ticket, metadata(), input, &[]).unwrap_err(),
        CloudAIError::Canceled
    );
}

#[test]
fn checked_capture_discards_data_when_authentication_changes_during_preparation() {
    let (mut owner, ticket, gate) = owner_and_ticket(false);
    gate.deny_at
        .store(gate.checks.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
    let input = CaptureInput {
        reported_text: Some("Checking updates"),
        recent_output: None,
    };
    assert_eq!(
        ContextBuilder::build_current(&mut owner, &ticket, metadata(), input, &[]).unwrap_err(),
        CloudAIError::Canceled
    );
}

#[test]
fn overlapping_known_secrets_leave_no_partial_fragment_as_false_context() {
    for (secret, input) in [
        ("abcabc".to_owned(), "abcabcabc".to_owned()),
        ("X".repeat(3000), "X".repeat(5999)),
    ] {
        let secret = SecretValue::try_from(secret).unwrap();
        let input = CaptureInput {
            reported_text: Some(&input),
            recent_output: None,
        };
        assert_eq!(
            ContextBuilder::build(&ticket(false), metadata(), input, &[&secret]).unwrap_err(),
            CloudAIError::NoContext
        );
    }
}

#[test]
fn overlapping_redaction_is_bounded_for_maximum_window_and_secret_list() {
    let secret = SecretValue::try_from("X".repeat(16_384)).unwrap();
    let secrets = vec![&secret; MAX_KNOWN_SECRETS];
    let input = format!("{} checking updates", "X".repeat(64_000));
    let request = ContextBuilder::build(
        &ticket(false),
        metadata(),
        CaptureInput {
            reported_text: Some(&input),
            recent_output: None,
        },
        &secrets,
    )
    .unwrap();
    assert_eq!(
        value(&request)["context"]["reported_text"],
        "[REDACTED] checking updates"
    );
}

#[test]
fn checked_capture_fails_closed_when_policy_cannot_be_rechecked() {
    for delay in [0, 1] {
        let (mut owner, ticket, gate) = owner_and_ticket(false);
        gate.fail_at
            .store(gate.checks.load(Ordering::SeqCst) + delay, Ordering::SeqCst);
        let input = CaptureInput {
            reported_text: Some("Checking updates"),
            recent_output: None,
        };
        assert_eq!(
            ContextBuilder::build_current(&mut owner, &ticket, metadata(), input, &[]).unwrap_err(),
            CloudAIError::PolicyUnavailable
        );
    }
}
