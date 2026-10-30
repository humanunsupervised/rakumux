//! What GitHub Copilot CLI sets for its hooks, tools and MCP servers.

/// Its markers (`COPILOT_CLI` itself is a hook host's, beside).
pub(super) const VARIABLES: &[&str] = &[
    "COPILOT_AGENT_SESSION_ID",
    "COPILOT_LOADER_PID",
    "COPILOT_CLI_BINARY_VERSION",
    "COPILOT_CLI_RESOLVED_DIST_DIR",
    "COPILOT_HOOK_DIR",
    "COPILOT_PROJECT_DIR",
];

/// The git settings Copilot gives its shell tool: no editor (`GIT_EDITOR=true`),
/// no credential or terminal prompts, no optional locks, and its own config
/// entries (`GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>`, `GIT_CONFIG_VALUE_<n>`).
/// Only from an environment Copilot made (`COPILOT_CLI=1` in it): a session
/// started from its shell tool would otherwise have git that can't prompt
/// for a sign-in or open an editor.
pub(super) fn git_setting(name: &str, var: impl Fn(&str) -> Option<String>) -> bool {
    let git = matches!(
        name,
        "GIT_EDITOR"
            | "GIT_ASKPASS"
            | "SSH_ASKPASS"
            | "GIT_TERMINAL_PROMPT"
            | "GIT_OPTIONAL_LOCKS"
            | "GIT_CONFIG_COUNT"
    ) || ["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"]
        .iter()
        .any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
    git && var("COPILOT_CLI").as_deref() == Some("1")
}
