use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use session_core::api::{
    ApiError, CallerInfo, ErrorCode, ReadRequest, ReadSource, TextFormat, TurnContent,
};
use session_core::client::SessionApi;
use session_core::ids::SessionId;
use session_core::model::{Reported, SessionView};
use thiserror::Error;

use super::tracker::{Facts, Progress, TurnSettings, TurnState};
use crate::cloud::{CaptureInput, CaptureMetadata, MAX_INSPECTION_BYTES};
use crate::dispatch::StatusCaptureJob;
use crate::status::{InputSelection, SourceStamp};

/// Rows of history plus screen read for one output window, before byte bounds.
pub const OUTPUT_LINES: u32 = 200;

/// A turn's window: the prompt and the newest messages that fit, well inside the
/// context builder's output bound (8 KiB after redaction), whose tail selection would
/// otherwise cut the prompt.
const TURN_BYTES: usize = 6 * 1024;

/// Emoji joiners and presentation selectors, common in agent TUIs, which the context
/// builder rejects. Deleting them only joins neighbouring text, so redaction sees
/// at least as much of a credential as before; any other invisible still rejects.
const PRESENTATION: [char; 3] = ['\u{200d}', '\u{fe0e}', '\u{fe0f}'];

const MASK: &str = "[REDACTED]";

struct Patterns {
    key_begin: Regex,
    key_end: Regex,
    /// A whole line of at least 60 base64 characters, optionally after a line number
    /// (`cat -n`, editors, `grep -n`): PEM/OpenSSH key bodies whose markers are both
    /// off screen (a pager or `sed` in the middle of a key).
    encoded_line: Regex,
}

static PATTERNS: LazyLock<Result<Patterns, regex::Error>> = LazyLock::new(|| {
    Ok(Patterns {
        key_begin: Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")?,
        key_end: Regex::new(r"-----END [A-Z0-9 ]*PRIVATE KEY-----")?,
        encoded_line: Regex::new(r"(?m)^[ \t]*(?:\d+[ \t:│→|]+)?[A-Za-z0-9+/]{60,}={0,2}[ \t]*$")?,
    })
});

/// Static capture failures; never daemon messages, names, folders or terminal text.
/// After any failure, stop and call `StatusDispatchService::acknowledge_capture`.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CaptureError {
    /// The job was revoked; no further daemon requests were made.
    #[error("the AI-status capture was canceled")]
    Canceled,
    /// The session no longer matches the job's source. Refresh the coordinator from
    /// the daemon (`get`, then `apply_view`) or wait for its events.
    #[error("the session changed before the AI-status capture finished")]
    Changed,
    /// Private, not live, or not readable by a user client.
    #[error("the session is not eligible for AI-status capture")]
    NotPermitted,
    /// The daemon could not answer.
    #[error("the local session service is unavailable for AI-status capture")]
    Unavailable,
}

/// The source facts one capture job must find, from `StatusDispatchService::expectation`.
/// It holds no text and is safe to move to an I/O thread with its job.
pub struct CaptureExpectation {
    request: String,
    session: SessionId,
    stamp: SourceStamp,
    selection: InputSelection,
    facts: Facts,
    turns: TurnSettings,
    reports: bool,
    output: bool,
}

/// Bounded plain-text windows for one job's source stamp, ready for
/// `StatusDispatchService::build_capture`. Not cloneable or serializable; Debug never
/// prints the text.
pub struct SessionCapture {
    /// The capturing job's local correlation; another job's capture is refused.
    pub(crate) request: String,
    pub(crate) session: SessionId,
    pub(crate) stamp: SourceStamp,
    metadata: CaptureMetadata,
    /// Which sources were read, so a no-context result clears only those.
    pub(crate) read_report: bool,
    pub(crate) read_output: bool,
    reported_text: Option<String>,
    recent_output: Option<String>,
}

impl SessionCapture {
    /// The source stamp these windows were confirmed against.
    pub fn stamp(&self) -> SourceStamp {
        self.stamp
    }

    /// Native lifecycle and activity phase observed with the windows.
    pub fn metadata(&self) -> CaptureMetadata {
        self.metadata
    }

    /// The windows, borrowed for the context builder.
    pub fn input(&self) -> CaptureInput<'_> {
        CaptureInput {
            reported_text: self.reported_text.as_deref(),
            recent_output: self.recent_output.as_deref(),
        }
    }
}

#[cfg(test)]
impl SessionCapture {
    /// An empty capture of `stamp` that read the given sources, for tracker tests.
    pub(super) fn read(
        session: &SessionId,
        stamp: SourceStamp,
        read_report: bool,
        read_output: bool,
    ) -> Self {
        Self {
            request: "synthetic-request".into(),
            session: session.clone(),
            stamp,
            metadata: CaptureMetadata {
                lifecycle: session_core::model::Lifecycle::Working,
                activity_phase: None,
            },
            read_report,
            read_output,
            reported_text: None,
            recent_output: None,
        }
    }
}

impl CaptureExpectation {
    pub(super) fn new(
        job: &StatusCaptureJob,
        facts: Facts,
        turns: TurnSettings,
        reports: bool,
        output: bool,
    ) -> Self {
        Self {
            request: job.request_id().to_owned(),
            session: job.session_id().clone(),
            stamp: job.source_stamp(),
            selection: job.input_selection(),
            facts,
            turns,
            reports,
            output,
        }
    }

    /// Read the job's permitted windows through a user SessionApi client. Blocking:
    /// run it off the owner's control thread. The session must show the expected facts
    /// before and after the read, otherwise `Changed`. Reports-only jobs never read
    /// terminal output. Output written during the read stays under this stamp; the
    /// tracker marks the session dirty for a later revision. A followed agent turn is
    /// read instead of the terminal: the user's prompt and the agent's messages.
    pub fn capture<Api: SessionApi + ?Sized>(
        &self,
        api: &Api,
        job: &StatusCaptureJob,
    ) -> Result<SessionCapture, CaptureError> {
        if job.request_id() != self.request
            || job.session_id() != &self.session
            || job.source_stamp() != self.stamp
            || job.folder() != &self.facts.scope
            || job.input_selection() != self.selection
        {
            return Err(CaptureError::Changed);
        }
        if !matches!(api.caller(), CallerInfo::User) {
            return Err(CaptureError::NotPermitted);
        }
        self.confirm(api, job)?;
        let report = self
            .facts
            .reported
            .as_ref()
            .and_then(report_text)
            .filter(|_| self.reports);
        let read_report = report.is_some();
        let reported_text = match report {
            Some(text) => window(&text)?,
            None => None,
        };
        let read_output = self.selection == InputSelection::ReportAndOutput && self.output;
        let recent_output = if read_output {
            if job.is_canceled() {
                return Err(CaptureError::Canceled);
            }
            match self.facts.progress() {
                Progress::Turn { id, state } => {
                    let turn = api.turn(&self.session).map_err(failure)?;
                    if turn.id != id {
                        return Err(CaptureError::Changed);
                    }
                    let recent = match state {
                        TurnState::Finished => usize::MAX,
                        _ => self.turns.recent as usize,
                    };
                    window(&turn_text(&turn, recent))?
                }
                Progress::Lifecycle(_) => {
                    let read = api
                        .read(ReadRequest {
                            id: self.session.clone(),
                            source: ReadSource::RecentUnwrapped,
                            lines: Some(OUTPUT_LINES),
                            format: TextFormat::Text,
                        })
                        .map_err(failure)?;
                    window(&read.text)?
                }
            }
        } else {
            None
        };
        let after = self.confirm(api, job)?;
        Ok(SessionCapture {
            request: self.request.clone(),
            session: self.session.clone(),
            stamp: self.stamp,
            metadata: CaptureMetadata {
                lifecycle: after.lifecycle,
                activity_phase: after.activity.map(|activity| activity.phase),
            },
            read_report,
            read_output,
            reported_text,
            recent_output,
        })
    }

    fn confirm<Api: SessionApi + ?Sized>(
        &self,
        api: &Api,
        job: &StatusCaptureJob,
    ) -> Result<SessionView, CaptureError> {
        if job.is_canceled() {
            return Err(CaptureError::Canceled);
        }
        let view = api.get(&self.session).map_err(failure)?;
        if view.id != self.session || !Facts::of(&view).same(&self.facts) {
            return Err(CaptureError::Changed);
        }
        if job.is_canceled() {
            return Err(CaptureError::Canceled);
        }
        Ok(view)
    }
}

impl fmt::Debug for CaptureExpectation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CaptureExpectation { bindings: [REDACTED] }")
    }
}

impl fmt::Debug for SessionCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionCapture { text: [REDACTED] }")
    }
}

/// A session's own report: its text and result summary, never the result's file list.
pub(super) fn report_text(reported: &Reported) -> Option<String> {
    let parts: Vec<&str> = [
        reported.text.as_deref(),
        reported
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|part| !part.is_empty())
    .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// A turn as the context builder's output window: the user's prompt, then the newest
/// `recent` of the agent's messages that fit [`TURN_BYTES`], oldest first, one a line.
fn turn_text(turn: &TurnContent, recent: usize) -> String {
    let line = |text: &str| -> String { text.chars().filter(|c| !c.is_control()).collect() };
    let mut text = String::from("The user's last prompt:\n");
    let prompt = turn.prompt.as_deref().map(line).unwrap_or_default();
    text.push_str(if prompt.trim().is_empty() {
        "(not known)"
    } else {
        &prompt
    });
    let mut budget = TURN_BYTES.saturating_sub(text.len() + 120);
    let mut kept: Vec<String> = Vec::new();
    for message in turn.messages.iter().rev().take(recent) {
        let message = line(message);
        if message.trim().is_empty() {
            continue;
        }
        if message.len() + 1 > budget {
            break;
        }
        budget -= message.len() + 1;
        kept.push(message);
    }
    let total = turn.total as usize;
    if kept.is_empty() {
        text.push_str("\n\nThe agent hasn't written anything since.");
        return text;
    }
    let count = if kept.len() < total {
        format!("the last {} of {total}", kept.len())
    } else {
        format!("all {total}")
    };
    text.push_str(&format!(
        "\n\nThe agent's messages since (reasoning, tool calls and replies; {count}, oldest first):"
    ));
    for message in kept.iter().rev() {
        text.push('\n');
        text.push_str(message);
    }
    text
}

/// The text's tail within the builder's inspection bound, starting at a whole line
/// where one fits, without presentation selectors, without a private key whose BEGIN
/// line fell outside the window, and with long base64 lines masked. Blank windows are
/// omitted. The builder still redacts and bounds what remains.
pub(super) fn window(text: &str) -> Result<Option<String>, CaptureError> {
    let patterns = PATTERNS.as_ref().map_err(|_| CaptureError::Unavailable)?;
    let text: String = text
        .chars()
        .filter(|character| !PRESENTATION.contains(character))
        .collect();
    let mut start = text.len().saturating_sub(MAX_INSPECTION_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    if start > 0 && text.as_bytes()[start - 1] != b'\n' {
        if let Some(newline) = text[start..].find('\n') {
            start += newline + 1;
        }
    }
    let mut kept = &text[start..];
    // Every END before the first BEGIN closes a key whose BEGIN line was cut off.
    let first_begin = patterns.key_begin.find(kept).map(|found| found.start());
    if let Some(orphan) = patterns
        .key_end
        .find_iter(kept)
        .take_while(|found| first_begin.is_none_or(|begin| found.start() < begin))
        .last()
    {
        kept = &kept[orphan.end()..];
    }
    let kept = patterns.encoded_line.replace_all(kept, MASK);
    let kept = kept.trim();
    Ok((!kept.is_empty()).then(|| kept.to_owned()))
}

fn failure(error: ApiError) -> CaptureError {
    match error.code {
        ErrorCode::NotFound | ErrorCode::SessionNotLive => CaptureError::Changed,
        ErrorCode::Unauthenticated | ErrorCode::OutOfScope | ErrorCode::PermissionDenied => {
            CaptureError::NotPermitted
        }
        _ => CaptureError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
