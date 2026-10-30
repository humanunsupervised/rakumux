//! Agent markers that must not leak from a parent terminal into a new session.
#[path = "claude-code/environment.rs"]
mod claude;
#[path = "codex/environment.rs"]
mod codex;
#[path = "copilot/environment.rs"]
mod copilot;
#[path = "cursor-agent/environment.rs"]
mod cursor;
#[path = "grok/environment.rs"]
mod grok;
#[path = "kimi/environment.rs"]
mod kimi;
#[path = "pi/environment.rs"]
mod pi;

/// What `hooks::host` reads to tell that another agent runs Claude Code's
/// hooks (GitHub Copilot's `COPILOT_CLI`, Grok's `GROK_HOOK_EVENT`). A daemon
/// started from inside one (its MCP server starting `rakumux-api`, which
/// starts the daemon) would otherwise pass it to every session, and Claude
/// Code's hooks there would report as that agent.
const HOOK_HOSTS: &[&str] = &["COPILOT_CLI", "GROK_HOOK_EVENT"];

pub(crate) fn inherited_marker(name: &str) -> bool {
    claude::VARIABLES.contains(&name)
        || codex::VARIABLES.contains(&name)
        || pi::VARIABLES.contains(&name)
        || copilot::VARIABLES.contains(&name)
        || cursor::VARIABLES.contains(&name)
        || grok::VARIABLES.contains(&name)
        || kimi::VARIABLES.contains(&name)
        || HOOK_HOSTS.contains(&name)
}

/// Settings an agent gives its own tools that must not reach a new session
/// started from one of them, told by the rest of that environment (`var`):
/// GitHub Copilot's git settings, beside its `COPILOT_CLI=1`.
pub(crate) fn tool_setting(name: &str, var: impl Fn(&str) -> Option<String>) -> bool {
    copilot::git_setting(name, var)
}
