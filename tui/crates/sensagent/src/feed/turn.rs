//! A feed entry's whole turn as a TUI shows it: read from rakumux.com
//! (`GET /api/v1/feed/turn`), each part cut for a terminal's detail pane and
//! the whole within one owner message; the rest comes with `next`.

use serde::{Deserialize, Serialize};
use session_core::api::{TranscriptMessage, TranscriptPart};

use super::wire::{FeedFailure, TurnRead};

/// Text per answer to a TUI: within the owner protocol's message bound, even
/// with every line break escaped.
const TEXT_BUDGET: usize = 96 * 1024;
/// The most of each kind of part a TUI is sent, in bytes.
const TEXT: usize = 16 * 1024;
const REASONING: usize = 8 * 1024;
const INPUT: usize = 4 * 1024;
const OUTPUT: usize = 8 * 1024;

/// Whether a turn could be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedTurnStatus {
    Ready,
    SignedOut,
    /// The account's feed is off.
    Off,
    /// rakumux.com can't be reached, or refused.
    Unavailable,
    #[serde(other)]
    Unknown,
}

/// The messages of a feed entry's turn, in order, each part cut to what a
/// TUI shows (`truncated` says so).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedTurn {
    pub status: FeedTurnStatus,
    #[serde(default)]
    pub messages: Vec<TranscriptMessage>,
    /// The turn goes on: ask again with `next` as `after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<u64>,
}

impl FeedTurn {
    /// A turn that couldn't be read.
    pub fn failed(failure: FeedFailure) -> Self {
        let status = match failure {
            FeedFailure::Unauthorized => FeedTurnStatus::SignedOut,
            FeedFailure::Off => FeedTurnStatus::Off,
            _ => FeedTurnStatus::Unavailable,
        };
        Self {
            status,
            messages: Vec::new(),
            next: None,
        }
    }

    /// `read` cut for a TUI: its messages while they fit [`TEXT_BUDGET`] (at
    /// least one), then `next` where to go on.
    pub(crate) fn shown(read: TurnRead) -> Self {
        let mut budget = TEXT_BUDGET;
        let mut messages = Vec::new();
        let total = read.messages.len();
        for message in read.messages {
            let message = TranscriptMessage {
                parts: message.parts.into_iter().map(shorter).collect(),
                ..message
            };
            let size: usize = message.parts.iter().map(text_len).sum::<usize>() + 64;
            if !messages.is_empty() && size > budget {
                break;
            }
            budget = budget.saturating_sub(size);
            messages.push(message);
        }
        let cut = messages.len() < total;
        let next = (cut || read.more)
            .then(|| messages.last().map(|message| message.position))
            .flatten();
        Self {
            status: FeedTurnStatus::Ready,
            messages,
            next,
        }
    }
}

fn text_len(part: &TranscriptPart) -> usize {
    match part {
        TranscriptPart::Text { text, .. } | TranscriptPart::Reasoning { text, .. } => text.len(),
        TranscriptPart::ToolCall { name, input, .. } => name.len() + input.len(),
        TranscriptPart::ToolResult { output, .. } => output.len(),
        TranscriptPart::Unknown => 0,
    }
}

/// `part` cut to what a TUI shows of its kind.
fn shorter(part: TranscriptPart) -> TranscriptPart {
    match part {
        TranscriptPart::Text { text, truncated } => {
            let (text, cut) = cut(text, TEXT);
            TranscriptPart::Text {
                text,
                truncated: truncated || cut,
            }
        }
        TranscriptPart::Reasoning { text, truncated } => {
            let (text, cut) = cut(text, REASONING);
            TranscriptPart::Reasoning {
                text,
                truncated: truncated || cut,
            }
        }
        TranscriptPart::ToolCall {
            call_id,
            name,
            input,
            truncated,
        } => {
            let (input, cut) = cut(input, INPUT);
            TranscriptPart::ToolCall {
                call_id,
                name,
                input,
                truncated: truncated || cut,
            }
        }
        TranscriptPart::ToolResult {
            call_id,
            output,
            error,
            truncated,
        } => {
            let (output, cut) = cut(output, OUTPUT);
            TranscriptPart::ToolResult {
                call_id,
                output,
                error,
                truncated: truncated || cut,
            }
        }
        TranscriptPart::Unknown => TranscriptPart::Unknown,
    }
}

fn cut(mut text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::api::TranscriptRole;

    fn message(position: u64, output: &str) -> TranscriptMessage {
        TranscriptMessage {
            id: position.to_string(),
            position,
            role: TranscriptRole::Tool,
            at: None,
            parts: vec![TranscriptPart::ToolResult {
                call_id: None,
                output: output.into(),
                error: false,
                truncated: false,
            }],
            subagent: None,
        }
    }

    #[test]
    fn parts_are_cut_and_the_turn_goes_on_where_it_stops_fitting() {
        let long = "x".repeat(OUTPUT * 2);
        let read = TurnRead {
            messages: (0..20).map(|n| message(n, &long)).collect(),
            more: false,
        };
        let shown = FeedTurn::shown(read);
        assert_eq!(shown.status, FeedTurnStatus::Ready);
        assert!(shown.messages.len() < 20);
        assert!(matches!(
            &shown.messages[0].parts[0],
            TranscriptPart::ToolResult { output, truncated: true, .. } if output.len() == OUTPUT
        ));
        let last = shown.messages.last().unwrap().position;
        assert_eq!(shown.next, Some(last));
        // All of it fits: nothing to go on with, unless the cloud has more.
        let read = TurnRead {
            messages: vec![message(1, "ok")],
            more: false,
        };
        assert_eq!(FeedTurn::shown(read).next, None);
        let read = TurnRead {
            messages: vec![message(1, "ok")],
            more: true,
        };
        assert_eq!(FeedTurn::shown(read).next, Some(1));
        assert_eq!(
            FeedTurn::failed(FeedFailure::Off).status,
            FeedTurnStatus::Off
        );
    }
}
