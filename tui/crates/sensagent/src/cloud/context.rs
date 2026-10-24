use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};
use session_core::activity::ActivityPhase;
use session_core::model::Lifecycle;

use super::{CloudAIError, ValidatedStatusRequest};
use crate::auth::SecretValue;
use crate::status::{
    AISessionStatusService, AccountGate, GenerationTicket, InputSelection, StatusClock, StatusStore,
};

/// Largest source window the builder inspects; capture adapters bound their windows to it.
pub(crate) const MAX_INSPECTION_BYTES: usize = 64 * 1024;
const MAX_REPORT_BYTES: usize = 2048;
const MAX_OUTPUT_BYTES: usize = 8192;
const MAX_KNOWN_SECRETS: usize = 32;
const MASKS: [&str; 2] = ["[REDACTED]", "[PATH]"];
const SECRET_MASK: u8 = 1;
const PATH_MASK: u8 = 2;

struct Patterns {
    prohibited: Regex,
    meaningful: Regex,
    private_key: Regex,
    assignment: Regex,
    token: Regex,
    unix_path: Regex,
    windows_path: Regex,
}

static PATTERNS: LazyLock<Result<Patterns, regex::Error>> = LazyLock::new(|| {
    Ok(Patterns {
        // Rust strings have no surrogate scalars. TAB/LF/CR remain valid context.
        prohibited: Regex::new(
            r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f\p{Cf}\p{Default_Ignorable_Code_Point}]",
        )?,
        meaningful: Regex::new(r"[\p{L}\p{N}\p{P}\p{S}]")?,
        private_key: Regex::new(
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
        )?,
        assignment: Regex::new(
            r#"(?i)(?:authorization[ \t]*:[ \t]*bearer[ \t]+[^\s"'<>]+|["']?\b(?:[a-z0-9]+_)*(?:api[_-]?key|access[_-]?token|refresh[_-]?token|session[_-]?token|client[_-]?secret|secret[_-]?key|password|passwd|secret|token)["']?[ \t]*[:=][ \t]*(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s,;]+))"#,
        )?,
        token: Regex::new(
            r"\b(?:sk-(?:ant-)?[A-Za-z0-9_-]{20,}|(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+)\b",
        )?,
        unix_path: Regex::new(r#"(?m)(?P<before>^|[ \t\r\n"'=(])/(?:[^\s"'<>|]+)"#)?,
        windows_path: Regex::new(r#"(?im)(?P<before>^|[ \t\r\n"'=(])[a-z]:\\[^\s"'<>|]+"#)?,
    })
});

/// Agent-neutral metadata attested by the local owner for the ticket's source revision.
#[derive(Clone, Copy, Debug)]
pub struct CaptureMetadata {
    /// Authoritative native lifecycle; generated text never changes it.
    pub lifecycle: Lifecycle,
    /// Normalized native phase. Unknown is omitted, not invented or coerced.
    pub activity_phase: Option<ActivityPhase>,
}

/// Explicit plain-text windows, not environments, files, ANSI streams or complete history.
/// Obtain them through scoped SessionApi capture (`source`) and recheck the job.
#[derive(Clone, Copy)]
pub struct CaptureInput<'a> {
    /// Optional bounded report window. Empty/blank/redaction-only data is omitted.
    pub reported_text: Option<&'a str>,
    /// Optional recent plain terminal window; requires ReportAndOutput selection.
    pub recent_output: Option<&'a str>,
}

impl fmt::Debug for CaptureInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CaptureInput { text: [REDACTED] }")
    }
}

/// Bounded best-effort local hygiene, not secret detection or permission attestation.
pub struct ContextBuilder;

impl ContextBuilder {
    /// Check the exact live local job before and after bounded capture. No stale,
    /// foreign or revoked job may produce a request through this owner-bound path.
    /// Submission still requires the future owner's immediate dispatch recheck.
    pub fn build_current<S: StatusStore, A: AccountGate, C: StatusClock>(
        owner: &mut AISessionStatusService<S, A, C>,
        ticket: &GenerationTicket,
        metadata: CaptureMetadata,
        input: CaptureInput<'_>,
        known_secrets: &[&SecretValue],
    ) -> Result<ValidatedStatusRequest, CloudAIError> {
        if !owner
            .is_current(ticket)
            .map_err(|_| CloudAIError::PolicyUnavailable)?
        {
            return Err(CloudAIError::Canceled);
        }
        let request = Self::build(ticket, metadata, input, known_secrets)?;
        if !owner
            .is_current(ticket)
            .map_err(|_| CloudAIError::PolicyUnavailable)?
        {
            return Err(CloudAIError::Canceled);
        }
        Ok(request)
    }

    /// Select only job-approved data, redact before tail selection, then validate the
    /// complete encoded wire request. No network, home/environment reads or persistence.
    ///
    /// A ticket is not a dispatch lease. The embedding owner must recheck is_current
    /// before capture and immediately before submission, and serialize revocation.
    pub fn build(
        ticket: &GenerationTicket,
        metadata: CaptureMetadata,
        input: CaptureInput<'_>,
        known_secrets: &[&SecretValue],
    ) -> Result<ValidatedStatusRequest, CloudAIError> {
        if ticket.input_selection() == InputSelection::ReportOnly && input.recent_output.is_some() {
            return Err(CloudAIError::DataNotPermitted);
        }
        if known_secrets.len() > MAX_KNOWN_SECRETS {
            return Err(CloudAIError::CaptureTooLarge);
        }
        let patterns = PATTERNS.as_ref().map_err(|_| {
            CloudAIError::InvalidConfiguration("context Unicode/redaction profile unavailable")
        })?;
        let report = select(
            input.reported_text,
            MAX_REPORT_BYTES,
            known_secrets,
            patterns,
        )?;
        let output = select(
            input.recent_output,
            MAX_OUTPUT_BYTES,
            known_secrets,
            patterns,
        )?;
        if report.is_none() && output.is_none() {
            return Err(CloudAIError::NoContext);
        }
        let mut context = Map::new();
        context.insert(
            "lifecycle".into(),
            serde_json::to_value(metadata.lifecycle).map_err(|_| CloudAIError::InvalidRequest)?,
        );
        if let Some(phase) = metadata
            .activity_phase
            .filter(|phase| *phase != ActivityPhase::Unknown)
        {
            context.insert(
                "activity_phase".into(),
                serde_json::to_value(phase).map_err(|_| CloudAIError::InvalidRequest)?,
            );
        }
        if let Some(report) = report {
            context.insert("reported_text".into(), Value::String(report));
        }
        if let Some(output) = output {
            context.insert("recent_output".into(), Value::String(output));
        }
        ValidatedStatusRequest::from_value(&serde_json::json!({
            "version": 1, "request_id": ticket.request_id(), "context": context,
        }))
    }
}

fn select(
    input: Option<&str>,
    maximum: usize,
    secrets: &[&SecretValue],
    patterns: &Patterns,
) -> Result<Option<String>, CloudAIError> {
    let Some(input) = input else {
        return Ok(None);
    };
    if input.len() > MAX_INSPECTION_BYTES {
        return Err(CloudAIError::CaptureTooLarge);
    }
    // Reject, rather than silently removing obfuscating controls before secret matching.
    // SessionApi's future capture adapter must supply plain text, not terminal escape streams.
    if patterns.prohibited.is_match(input) {
        return Err(CloudAIError::InvalidRequest);
    }
    let text = redact(input, secrets, patterns);
    let kept = tail_without_partial_masks(&text, maximum);
    let mut signal = kept.to_owned();
    for mask in MASKS {
        signal = signal.replace(mask, "");
    }
    if !patterns.meaningful.is_match(&signal) {
        return Ok(None);
    }
    Ok(Some(kept.to_owned()))
}

/// An agent reply as the feed uploads it: without terminal and format
/// controls, with private keys, credential assignments and tokens masked, or
/// `None` when nothing meaningful is left. Paths stay: the feed shows the
/// user's own replies back to them.
pub(crate) fn feed_text(input: &str) -> Option<String> {
    let patterns = PATTERNS.as_ref().ok()?;
    let clean = patterns.prohibited.replace_all(input, "");
    let mut mask = vec![0u8; clean.len()];
    for pattern in [&patterns.private_key, &patterns.assignment, &patterns.token] {
        for found in pattern.find_iter(&clean) {
            mark(&mut mask, found.start(), found.end(), SECRET_MASK);
        }
    }
    let rendered = render_masks(&clean, &mask);
    let text = rendered.trim();
    patterns.meaningful.is_match(text).then(|| text.to_owned())
}

fn redact(input: &str, secrets: &[&SecretValue], patterns: &Patterns) -> String {
    // Mark original byte spans once. Rewriting text repeatedly can destroy key
    // delimiters or expand replacement labels when a short secret matches them.
    let mut mask = vec![0u8; input.len()];
    for pattern in [&patterns.private_key, &patterns.assignment, &patterns.token] {
        for found in pattern.find_iter(input) {
            mark(&mut mask, found.start(), found.end(), SECRET_MASK);
        }
    }
    for secret in secrets {
        mark_known_secret(
            input.as_bytes(),
            secret.expose_secret().as_bytes(),
            &mut mask,
        );
    }
    for pattern in [&patterns.unix_path, &patterns.windows_path] {
        for found in pattern.captures_iter(input) {
            if let (Some(whole), Some(prefix)) = (found.get(0), found.name("before")) {
                mark(
                    &mut mask,
                    whole.start() + prefix.len(),
                    whole.end(),
                    PATH_MASK,
                );
            }
        }
    }
    render_masks(input, &mask)
}

fn mark_known_secret(input: &[u8], needle: &[u8], mask: &mut [u8]) {
    if needle.is_empty() {
        return;
    }
    // Prefix matching finds overlapping occurrences in O(input + secret) time.
    // Merge their spans before marking: repeatedly filling a long overlapping
    // range would otherwise turn repetitive input into quadratic work.
    let mut prefix = vec![0usize; needle.len()];
    let mut matched = 0;
    for index in 1..needle.len() {
        while matched > 0 && needle[index] != needle[matched] {
            matched = prefix[matched - 1];
        }
        if needle[index] == needle[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }
    matched = 0;
    let mut span: Option<(usize, usize)> = None;
    for (index, byte) in input.iter().enumerate() {
        while matched > 0 && *byte != needle[matched] {
            matched = prefix[matched - 1];
        }
        if *byte == needle[matched] {
            matched += 1;
        }
        if matched != needle.len() {
            continue;
        }
        let start = index + 1 - needle.len();
        let end = index + 1;
        match span.as_mut() {
            Some((_, previous_end)) if start <= *previous_end => *previous_end = end,
            Some((previous_start, previous_end)) => {
                mark(mask, *previous_start, *previous_end, SECRET_MASK);
                *previous_start = start;
                *previous_end = end;
            }
            None => span = Some((start, end)),
        }
        matched = prefix[matched - 1];
    }
    if let Some((start, end)) = span {
        mark(mask, start, end, SECRET_MASK);
    }
}

fn mark(mask: &mut [u8], start: usize, end: usize, kind: u8) {
    for byte in &mut mask[start..end] {
        if *byte == 0 || kind == SECRET_MASK {
            *byte = kind;
        }
    }
}

fn render_masks(input: &str, mask: &[u8]) -> String {
    let mut rendered = String::with_capacity(input.len());
    let mut start = 0;
    while start < input.len() {
        let kind = mask[start];
        let mut end = start + 1;
        while end < input.len() && mask[end] == kind {
            end += 1;
        }
        // All span boundaries came from UTF-8 regex/string matches.
        match kind {
            SECRET_MASK => rendered.push_str(MASKS[0]),
            PATH_MASK => rendered.push_str(MASKS[1]),
            _ => rendered.push_str(&input[start..end]),
        }
        start = end;
    }
    rendered
}

fn tail_without_partial_masks(text: &str, maximum: usize) -> &str {
    let mut start = text.len().saturating_sub(maximum);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    for mask in MASKS {
        for (index, _) in text.match_indices(mask) {
            if index < start && start < index + mask.len() {
                start = index + mask.len();
                break;
            }
        }
    }
    &text[start..]
}

#[cfg(test)]
mod tests;
