//! Provider-owned presentation metadata; shared UI code consumes only a colour.
use ratatui::style::Color;
#[path = "Claude Code/presentation.rs"]
mod claude;
#[path = "Codex/presentation.rs"]
mod codex;
#[path = "Open Code/presentation.rs"]
mod opencode;

pub(super) fn color(agent: &str) -> Option<Color> {
    match agent {
        "claude" => Some(claude::color()),
        "codex" => Some(codex::color()),
        "opencode" => Some(opencode::color()),
        _ => None,
    }
}
