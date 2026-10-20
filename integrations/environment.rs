//! Agent markers that must not leak from a parent terminal into a new session.
#[path = "claude-code/environment.rs"]
mod claude;
#[path = "codex/environment.rs"]
mod codex;

pub(crate) fn inherited_marker(name: &str) -> bool {
    claude::VARIABLES.contains(&name) || codex::VARIABLES.contains(&name)
}
