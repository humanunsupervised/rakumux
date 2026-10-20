//! Provider-owned presentation metadata; shared UI code consumes only a colour.
use ratatui::style::Color;
#[path = "claude-code/presentation.rs"]
mod claude;
#[path = "codex/presentation.rs"]
mod codex;
#[path = "open-code/presentation.rs"]
mod opencode;

pub(super) fn color(agent: &str) -> Option<Color> {
    match agent {
        "claude" => Some(claude::color()),
        "codex" => Some(codex::color()),
        "opencode" => Some(opencode::color()),
        _ => None,
    }
}
