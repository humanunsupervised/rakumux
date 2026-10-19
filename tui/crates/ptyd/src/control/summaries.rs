//! One daemon-owned summary worker. No network or terminal reads run under
//! the registry lock; clients only receive cached session-view updates.

use super::service::{now_ms, Inner};
use crate::host::HostError;
use rakumux_ai::{provider, summary, Config};
use serde::Deserialize;
use session_core::ids::{SessionId, TerminalId};
use session_core::model::Lifecycle;
use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::path::Path;
use std::sync::{atomic::Ordering, Arc, Weak};
use std::time::Duration;

const POLL: Duration = Duration::from_secs(2);
const INTERVAL_MS: i64 = 60_000;
const QUIET_MS: i64 = 2_000;
const MAX_WAIT_MS: i64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SummaryRevision {
    pub terminal: TerminalId,
    pub output_offset: u64,
    pub report_seq: u64,
    pub lifecycle: Lifecycle,
    pub foreground_pid: Option<u32>,
}

pub(super) struct SummaryTarget {
    pub id: SessionId,
    pub revision: SummaryRevision,
    pub last_output_ms: Option<i64>,
    pub report: String,
    pub secrets: Vec<String>,
}

#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct Preferences {
    ai_activity_summaries: bool,
}

#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct SummarySettings {
    #[serde(flatten)]
    ai: Config,
    sessions: Preferences,
}

fn settings(path: &Path) -> io::Result<SummarySettings> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(SummarySettings::default())
        }
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "summary settings exceed their size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

type Model = Box<dyn Fn(&Config, &str) -> Result<String, String> + Send>;

fn ai_summary(config: &Config, prompt: &str) -> Result<String, String> {
    let provider = provider::choose_provider(config, &|name| std::env::var(name).ok())
        .ok_or_else(|| "No AI provider is configured; add an API key in Settings.".to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime
        .block_on(provider::request_text(&provider, prompt))
        .map_err(|error| error.to_string())
}

#[derive(Default)]
struct Attempt {
    first_seen_ms: i64,
    waiting_for_settle: bool,
    last_request_ms: Option<i64>,
    revision: Option<SummaryRevision>,
    input_hash: Option<u64>,
    failed: bool,
}

impl Attempt {
    fn due(&self, target: &SummaryTarget, now: i64) -> bool {
        let critical = matches!(
            target.revision.lifecycle,
            Lifecycle::Blocked | Lifecycle::Done
        ) && self.revision.as_ref().map(|revision| revision.lifecycle)
            != Some(target.revision.lifecycle);
        if !critical
            && self
                .last_request_ms
                .is_some_and(|last| now.saturating_sub(last) < INTERVAL_MS)
        {
            return false;
        }
        if !self.failed && self.revision.as_ref() == Some(&target.revision) {
            return false;
        }
        critical
            || target
                .last_output_ms
                .is_none_or(|last| now.saturating_sub(last) >= QUIET_MS)
            || now.saturating_sub(self.first_seen_ms) >= MAX_WAIT_MS
    }
}

struct SummaryService {
    attempts: HashMap<SessionId, Attempt>,
    model: Model,
}

impl SummaryService {
    fn tick(&mut self, inner: &Inner, config: &SummarySettings, now: i64) -> Result<(), String> {
        if !config.sessions.ai_activity_summaries {
            return Ok(());
        }
        let mut targets = inner.summary_targets().map_err(|error| error.to_string())?;
        let active: HashSet<_> = targets.iter().map(|target| target.id.clone()).collect();
        self.attempts.retain(|id, _| active.contains(id));
        // Newly blocked/done sessions lead; otherwise oldest attempts lead so
        // one noisy terminal cannot starve the others.
        targets.sort_by_key(|target| {
            let attempt = self.attempts.get(&target.id);
            let critical = matches!(
                target.revision.lifecycle,
                Lifecycle::Blocked | Lifecycle::Done
            ) && attempt
                .and_then(|attempt| attempt.revision.as_ref())
                .map(|revision| revision.lifecycle)
                != Some(target.revision.lifecycle);
            (
                !critical,
                attempt
                    .and_then(|attempt| attempt.last_request_ms)
                    .unwrap_or(0),
            )
        });
        for mut target in targets {
            let attempt = self
                .attempts
                .entry(target.id.clone())
                .or_insert_with(|| Attempt {
                    first_seen_ms: now,
                    waiting_for_settle: true,
                    ..Attempt::default()
                });
            if attempt
                .revision
                .as_ref()
                .is_some_and(|revision| revision.terminal != target.revision.terminal)
            {
                *attempt = Attempt {
                    first_seen_ms: now,
                    waiting_for_settle: true,
                    ..Attempt::default()
                };
            }
            if !attempt.waiting_for_settle && attempt.revision.as_ref() != Some(&target.revision) {
                attempt.first_seen_ms = now;
                attempt.waiting_for_settle = true;
            }
            if !attempt.due(&target, now) {
                continue;
            }
            let output = match inner.summary_output(&target) {
                Ok(output) => output,
                Err(HostError::NotFound | HostError::NotRunning) => continue,
                Err(error) => return Err(format!("reading activity for {}: {error}", target.id)),
            };
            target.secrets.extend(
                [
                    config.ai.anthropic_api_key.clone(),
                    config.ai.openai_api_key.clone(),
                    std::env::var("ANTHROPIC_API_KEY").ok(),
                    std::env::var("OPENAI_API_KEY").ok(),
                ]
                .into_iter()
                .flatten(),
            );
            let evidence = summary::activity(&target.report, &output, &target.secrets)
                .map_err(|error| error.to_string())?;
            let mut hasher = DefaultHasher::new();
            evidence.hash(&mut hasher);
            format!("{:?}", target.revision.lifecycle).hash(&mut hasher);
            let input_hash = hasher.finish();
            attempt.revision = Some(target.revision.clone());
            attempt.waiting_for_settle = false;
            if (attempt.input_hash == Some(input_hash) && !attempt.failed)
                || (target.report.trim().is_empty() && output.trim().chars().count() < 24)
            {
                continue;
            }
            attempt.input_hash = Some(input_hash);
            attempt.last_request_ms = Some(now);
            let prompt = summary::prompt(&format!(
                "Lifecycle: {:?}\n{evidence}",
                target.revision.lifecycle
            ));
            let result = (self.model)(&config.ai, &prompt).and_then(|reply| {
                summary::clean_reply(&reply, &target.secrets).map_err(|error| error.to_string())
            });
            // Opting out, changing credentials or switching providers while a
            // request runs invalidates its result as well as future requests.
            let current = settings(&inner.summary_config_path())
                .map_err(|error| format!("reloading summary settings: {error}"))?;
            if &current != config {
                inner.enable_summaries(current.sessions.ai_activity_summaries);
                return Ok(());
            }
            match result {
                Ok(text) => {
                    attempt.failed = !inner.apply_summary(&target, text, None, now);
                }
                Err(error) => {
                    attempt.failed = true;
                    let clean = summary::redact(&error, &target.secrets)
                        .map_err(|error| error.to_string())?;
                    crate::log_warn!("activity summary {}: {}", target.id, clean);
                    let message =
                        if provider::choose_provider(&config.ai, &|name| std::env::var(name).ok())
                            .is_none()
                        {
                            "No AI provider configured; add an API key in Settings."
                        } else {
                            "Could not refresh activity summary; see daemon logs."
                        };
                    inner.apply_summary(&target, None, Some(message.into()), now);
                }
            }
            // At most one provider request per worker pass, globally.
            return Ok(());
        }
        Ok(())
    }
}

pub(super) fn start(inner: &Arc<Inner>) -> io::Result<()> {
    let weak = Arc::downgrade(inner);
    std::thread::Builder::new()
        .name("ptyd-activity-summaries".into())
        .spawn(move || run(weak))?;
    Ok(())
}

fn run(weak: Weak<Inner>) {
    let mut worker = SummaryService {
        attempts: HashMap::new(),
        model: Box::new(ai_summary),
    };
    let mut previous = None;
    let mut last_error = None;
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if inner.stop.load(Ordering::SeqCst) {
            return;
        }
        match settings(&inner.summary_config_path()) {
            Ok(config) => {
                if previous.as_ref() != Some(&config) {
                    worker.attempts.clear();
                    previous = Some(config.clone());
                }
                inner.enable_summaries(config.sessions.ai_activity_summaries);
                if config.sessions.ai_activity_summaries {
                    if let Err(error) = worker.tick(&inner, &config, now_ms()) {
                        if last_error.as_ref() != Some(&error) {
                            crate::log_warn!("activity summaries: {error}");
                        }
                        last_error = Some(error);
                    } else {
                        last_error = None;
                    }
                }
            }
            Err(error) => {
                // A malformed or temporarily unreadable config never grants consent.
                inner.enable_summaries(false);
                let message = error.to_string();
                if last_error.as_ref() != Some(&message) {
                    crate::log_warn!("activity summary settings: {message}");
                }
                last_error = Some(message);
            }
        }
        drop(inner);
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn target(lifecycle: Lifecycle) -> SummaryTarget {
        SummaryTarget {
            id: SessionId::from_string("test"),
            revision: SummaryRevision {
                terminal: TerminalId(1),
                output_offset: 100,
                report_seq: 1,
                lifecycle,
                foreground_pid: Some(42),
            },
            last_output_ms: Some(0),
            report: String::new(),
            secrets: Vec::new(),
        }
    }

    #[test]
    fn summaries_debounce_rate_limit_deduplicate_and_refresh_on_blockers() {
        let mut target = target(Lifecycle::Working);
        target.last_output_ms = Some(1_000);
        let mut attempt = Attempt {
            first_seen_ms: 1_000,
            ..Attempt::default()
        };
        assert!(!attempt.due(&target, 1_500));
        assert!(attempt.due(&target, 3_000));
        attempt.last_request_ms = Some(3_000);
        attempt.revision = Some(target.revision.clone());
        target.revision.output_offset += 1;
        assert!(!attempt.due(&target, 10_000));
        assert!(attempt.due(&target, 63_000));
        attempt.revision = Some(target.revision.clone());
        assert!(!attempt.due(&target, 100_000));
        target.revision.lifecycle = Lifecycle::Blocked;
        assert!(attempt.due(&target, 5_000));
        target.last_output_ms = Some(200_000);
        target.revision.lifecycle = Lifecycle::Working;
        attempt.revision = None;
        attempt.last_request_ms = None;
        assert!(
            attempt.due(&target, 200_000),
            "continuous output cannot postpone the first summary forever"
        );
    }

    #[test]
    fn summary_settings_default_to_no_consent_and_share_ai_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert!(!settings(&path).unwrap().sessions.ai_activity_summaries);
        std::fs::write(&path, r#"{"openai_api_key":"sk-test","sessions":{"ai_activity_summaries":true,"layout":"tabs"}}"#).unwrap();
        let config = settings(&path).unwrap();
        assert!(config.sessions.ai_activity_summaries);
        assert_eq!(config.ai.openai_api_key.as_deref(), Some("sk-test"));
        std::fs::write(&path, "broken").unwrap();
        assert!(settings(&path).is_err());
    }
}
