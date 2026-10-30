//! Agent markers that must not leak from a parent terminal into a new session.
#[path = "claude-code/environment.rs"]
mod claude;
#[path = "codex/environment.rs"]
mod codex;

/// What `hooks::host` reads to tell that another agent runs Claude Code's
/// hooks (GitHub Copilot's `COPILOT_CLI`, Grok's `GROK_HOOK_EVENT`). A daemon
/// started from inside one (its MCP server starting `rakumux-api`, which
/// starts the daemon) would otherwise pass it to every session, and Claude
/// Code's hooks there would report as that agent.
const HOOK_HOSTS: &[&str] = &["COPILOT_CLI", "GROK_HOOK_EVENT"];

pub(crate) fn inherited_marker(name: &str) -> bool {
    claude::VARIABLES.contains(&name)
        || codex::VARIABLES.contains(&name)
        || HOOK_HOSTS.contains(&name)
}
