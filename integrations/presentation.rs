//! Provider-owned presentation metadata; shared UI code consumes only a colour.
use ratatui::style::Color;
#[path = "amp/presentation.rs"]
mod amp;
#[path = "claude-code/presentation.rs"]
mod claude;
#[path = "codex/presentation.rs"]
mod codex;
#[path = "copilot/presentation.rs"]
mod copilot;
#[path = "cursor-agent/presentation.rs"]
mod cursor;
#[path = "droid/presentation.rs"]
mod droid;
#[path = "grok/presentation.rs"]
mod grok;
#[path = "kimi/presentation.rs"]
mod kimi;
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
        "droid" => Some(droid::color()),
        "opencode" => Some(opencode::color()),
        "pi" => Some(pi::color()),
        "grok" => Some(grok::color()),
        "kimi" => Some(kimi::color()),
        "amp" => Some(amp::color()),
        _ => None,
    }
}
