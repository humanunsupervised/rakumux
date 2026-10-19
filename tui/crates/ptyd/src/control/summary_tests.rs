//! Summary use cases against the existing in-memory terminal host. No
//! daemon thread, real terminal, API provider or user configuration is used.

use super::*;
use crate::control::registry::Registry;
use crate::control::{ControlConfig, Limits};
use crate::control_api::Caller;
use crate::control_test_support::FakeHost;
use session_core::api::{CallerInfo, CreateSession, Request, Response, SubscribeRequest};
use session_core::ids::Token;
use session_core::model::{ScopeRoot, SessionView};
use session_core::paths::Layout;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Context {
    _directory: tempfile::TempDir,
    layout: Layout,
    inner: Arc<Inner>,
    host: Arc<FakeHost>,
    caller: Caller,
    id: SessionId,
}

impl Context {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let layout = Layout::with_home(root.join("home"));
        let registry = Registry::open(&layout.registry_db(), now_ms()).unwrap();
        let host = FakeHost::new();
        let inner = Arc::new(Inner::new(
            ControlConfig {
                layout: layout.clone(),
                user_token: Token::from_string("private-user-token"),
                socket: root.join("socket"),
                rakumux_bin: None,
                version: "test".into(),
                limits: Limits::default(),
            },
            host.clone(),
            registry,
            crate::detect::detect,
        ));
        let caller = Caller {
            info: CallerInfo::User,
            client: "summary-test".into(),
            max_blocking_ms: None,
        };
        let request = CreateSession {
            scope: Some(ScopeRoot::from_canonical(root.clone())),
            name: Some("my-session".into()),
            cwd: None,
            argv: Some(vec!["fake-agent".into()]),
            startup_cmds: vec![],
            env: vec![("CUSTOM_TOKEN".into(), "private-env-token".into())],
            cols: 80,
            rows: 24,
        };
        let id = match inner.handle(&caller, Request::SessionCreate(request)) {
            Response::Session { session } => session.id,
            other => panic!("{other:?}"),
        };
        Self {
            _directory: directory,
            layout,
            inner,
            host,
            caller,
            id,
        }
    }

    fn configure(&self, enabled: bool) -> SummarySettings {
        std::fs::write(
            self.inner.summary_config_path(),
            serde_json::json!({
                "openai_api_key": "sk-private-provider-key",
                "ai_primary_provider": "openai",
                "sessions": {"ai_activity_summaries": enabled},
            })
            .to_string(),
        )
        .unwrap();
        self.inner.enable_summaries(enabled);
        settings(&self.inner.summary_config_path()).unwrap()
    }

    fn view(&self) -> SessionView {
        match self.inner.handle(
            &self.caller,
            Request::SessionGet {
                id: self.id.clone(),
            },
        ) {
            Response::Session { session } => session,
            other => panic!("{other:?}"),
        }
    }
}

fn settled_tick(worker: &mut SummaryService, inner: &Inner, config: &SummarySettings, now: i64) {
    worker.tick(inner, config, now - QUIET_MS).unwrap();
    worker.tick(inner, config, now).unwrap();
}

#[test]
fn activity_summary_is_shared_cached_persisted_and_deduplicated() {
    let context = Context::new();
    assert!(context.view().activity_summary.is_none());
    let config = context.configure(true);
    let first_client = context
        .inner
        .subscribe(
            &context.caller,
            &SubscribeRequest {
                all: true,
                ..Default::default()
            },
        )
        .unwrap();
    let second_client = context
        .inner
        .subscribe(
            &context.caller,
            &SubscribeRequest {
                all: true,
                ..Default::default()
            },
        )
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(move |_, prompt| {
            count.fetch_add(1, Ordering::SeqCst);
            for secret in [
                "private-user-token",
                "private-env-token",
                "sk-private-provider-key",
            ] {
                assert!(!prompt.contains(secret));
            }
            Ok("Fixed login; verifying tests.".into())
        }),
    };
    context.host.show(&context.id, "Fixed the login flow; running the integration suite. CUSTOM_TOKEN=private-env-token private-user-token sk-private-provider-key");
    let now = now_ms() + 10_000;
    settled_tick(&mut worker, &context.inner, &config, now);
    let summary = context.view().activity_summary.unwrap();
    assert_eq!(summary.text, "Fixed login; verifying tests.");
    assert_eq!(context.view().name, "my-session");
    for client in [first_client, second_client] {
        assert!(client.try_iter().any(|envelope| matches!(envelope.event,
            session_core::api::SessionEvent::Updated { session } if session.activity_summary.as_ref() == Some(&summary))));
    }
    worker.tick(&context.inner, &config, now + 120_000).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "clients and unchanged output do not duplicate requests"
    );
    let cache = crate::persist::ScrollbackStore::new(context.layout.clone());
    assert_eq!(
        cache.load_activity_summary(&context.id).unwrap(),
        Some(summary)
    );
    let disabled = context.configure(false);
    worker
        .tick(&context.inner, &disabled, now + 240_000)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(context.view().activity_summary.is_none());
}

#[test]
fn private_sessions_never_reach_the_summary_provider() {
    let context = Context::new();
    let config = context.configure(true);
    let connection = rusqlite::Connection::open(context.layout.registry_db()).unwrap();
    connection
        .execute(
            "UPDATE sessions_v2 SET private = 1 WHERE id = ?1",
            [context.id.as_str()],
        )
        .unwrap();
    context.host.show(
        &context.id,
        "Private task with enough output that a normal session would be summarized.",
    );
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(|_, _| panic!("private session reached AI provider")),
    };
    settled_tick(&mut worker, &context.inner, &config, now_ms() + 10_000);
    assert!(context.view().activity_summary.is_none());
}

#[test]
fn a_summary_result_after_opt_out_is_discarded() {
    let context = Context::new();
    let config = context.configure(true);
    context.host.show(
        &context.id,
        "Investigated the parser failure; now checking the regression test.",
    );
    let inner = context.inner.clone();
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(move |_, _| {
            inner.enable_summaries(false);
            Ok("This late result must not be stored.".into())
        }),
    };
    settled_tick(&mut worker, &context.inner, &config, now_ms() + 10_000);
    assert!(context.view().activity_summary.is_none());
    assert!(crate::persist::ScrollbackStore::new(context.layout.clone())
        .load_activity_summary(&context.id)
        .unwrap()
        .is_none());
}

#[test]
fn a_summary_result_after_a_new_agent_report_is_discarded() {
    let context = Context::new();
    let config = context.configure(true);
    context.host.show(
        &context.id,
        "Investigating the login flow and preparing the next check.",
    );
    let inner = context.inner.clone();
    let caller = Caller {
        info: CallerInfo::Session {
            id: context.id.clone(),
            scope: context.view().scope,
        },
        client: "agent".into(),
        max_blocking_ms: None,
    };
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(move |_, _| {
            assert!(matches!(
                inner.handle(
                    &caller,
                    Request::SessionReport(session_core::api::Report {
                        state: Some(Lifecycle::Blocked),
                        text: Some("Awaiting a user decision.".into()),
                        ..Default::default()
                    })
                ),
                Response::Ok
            ));
            Ok("This predates the new blocker and must be dropped.".into())
        }),
    };
    settled_tick(&mut worker, &context.inner, &config, now_ms() + 10_000);
    assert!(context.view().activity_summary.unwrap().text.is_empty());
    assert!(crate::persist::ScrollbackStore::new(context.layout.clone())
        .load_activity_summary(&context.id)
        .unwrap()
        .is_none());
}

#[test]
fn summary_refresh_errors_keep_the_last_successful_text() {
    let context = Context::new();
    let config = context.configure(true);
    context.host.show(
        &context.id,
        "Fixed login; checking integration tests and final checks.",
    );
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(|_, _| Ok("Fixed login; verifying tests.".into())),
    };
    let now = now_ms() + 10_000;
    settled_tick(&mut worker, &context.inner, &config, now);
    worker.model = Box::new(|_, _| Err("provider unavailable".into()));
    context.host.show(
        &context.id,
        "Now investigating a newly exposed test failure in the login flow.",
    );
    settled_tick(&mut worker, &context.inner, &config, now + 60_000);
    let summary = context.view().activity_summary.unwrap();
    assert_eq!(summary.text, "Fixed login; verifying tests.");
    assert!(summary.error.is_some());
    assert_eq!(summary.updated_at, now / 1000);
}

#[test]
fn every_session_update_refreshes_after_ten_idle_seconds_without_a_minute_delay() {
    let context = Context::new();
    let config = context.configure(true);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(move |_, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok("Investigated login; verifying the next step.".into())
        }),
    };
    context.host.show(
        &context.id,
        "Investigating the initial login failure in the authentication code.",
    );
    let start = now_ms() + 100;
    worker.tick(&context.inner, &config, start).unwrap();
    worker.tick(&context.inner, &config, start + 9_999).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    worker
        .tick(&context.inner, &config, start + 10_000)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    context.host.show(
        &context.id,
        "Fixed the login failure; now checking integration coverage and final tests.",
    );
    worker
        .tick(&context.inner, &config, start + 12_000)
        .unwrap();
    worker
        .tick(&context.inner, &config, start + 21_999)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    worker
        .tick(&context.inner, &config, start + 22_000)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn existing_dead_sessions_receive_a_summary_from_saved_output_without_being_recreated() {
    let context = Context::new();
    let config = context.configure(true);
    crate::persist::ScrollbackStore::new(context.layout.clone())
        .save(
            &context.id,
            b"Fixed the migration; verifying the final integration checks.\r\n",
            &crate::persist::SessionMeta::default(),
        )
        .unwrap();
    let host = FakeHost::new();
    let restored = Inner::new(
        ControlConfig {
            layout: context.layout.clone(),
            user_token: Token::from_string("restored-user-token"),
            socket: context.layout.home.join("socket"),
            rakumux_bin: None,
            version: "test".into(),
            limits: Limits::default(),
        },
        host.clone(),
        Registry::open(&context.layout.registry_db(), now_ms()).unwrap(),
        crate::detect::detect,
    );
    restored.enable_summaries(true);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(move |_, prompt| {
            assert!(prompt.contains("Session status: Dead"));
            assert!(prompt.contains("Fixed the migration"));
            count.fetch_add(1, Ordering::SeqCst);
            Ok("Fixed the migration; last checked integration coverage.".into())
        }),
    };
    let now = now_ms() + QUIET_MS;
    settled_tick(&mut worker, &restored, &config, now);
    let response = restored.handle(
        &context.caller,
        Request::SessionGet {
            id: context.id.clone(),
        },
    );
    let Response::Session { session } = response else {
        panic!("{response:?}");
    };
    assert_eq!(session.id, context.id);
    assert_eq!(session.status, SessionStatus::Dead);
    assert!(session.activity_summary.unwrap().text.contains("migration"));
    assert!(
        host.spawns().is_empty(),
        "backfilling never starts or recreates a terminal"
    );
    worker.tick(&restored, &config, now + 60_000).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "unchanged archived output is not regenerated"
    );
}

#[test]
fn output_arriving_during_a_summary_request_invalidates_that_reply() {
    let context = Context::new();
    let config = context.configure(true);
    context.host.show(
        &context.id,
        "Investigating the parser failure before the next update.",
    );
    let host = context.host.clone();
    let id = context.id.clone();
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(move |_, _| {
            host.show(
                &id,
                "New output: the parser is fixed and integration checks have started.",
            );
            Ok("An obsolete description which must not overwrite newer activity.".into())
        }),
    };
    settled_tick(&mut worker, &context.inner, &config, now_ms() + QUIET_MS);
    assert!(context.view().activity_summary.unwrap().text.is_empty());
    assert!(
        !worker.attempts[&context.id].failed,
        "obsolete replies do not trigger a network-error backoff"
    );
}
