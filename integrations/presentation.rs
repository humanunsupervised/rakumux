//! Provider-owned presentation metadata; shared UI code consumes only a colour.
use ratatui::style::Color;
#[path = "claude-code/presentation.rs"]
mod claude;
#[path = "codex/presentation.rs"]
mod codex;
#[path = "copilot/presentation.rs"]
mod copilot;
#[path = "cursor-agent/presentation.rs"]
mod cursor;
#[path = "open-code/presentation.rs"]
mod opencode;
#[path = "pi/presentation.rs"]
mod pi;

pub(super) fn color(agent: &str) -> Option<Color> {
    match agent {
        "claude" => Some(claude::color()),
        "codex" => Some(codex::color()),
        "copilot" => Some(copilot::color()),
        "cursor-agent" => Some(cursor::color()),
        "opencode" => Some(opencode::color()),
        "pi" => Some(pi::color()),
        _ => None,
    }
}
