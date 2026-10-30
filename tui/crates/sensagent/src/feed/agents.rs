//! The agents rakumux.com takes (`contracts/feed/` `agent`). What any other
//! agent said stays on this machine: its replies and chats are never
//! queued, so a refused one can't hold back the rest of a batch, and the
//! session list names its session's `program` instead.

/// The contract's agent list, in its order.
pub(crate) const AGENTS: &[&str] = &[
    "claude-code",
    "codex",
    "opencode",
    "copilot",
    "cursor-agent",
    "pi",
    "grok",
    "kimi",
    "droid",
    "amp",
];

/// The feed's name for an agent the daemon names (a detected agent, a
/// hook's, or a follower's: `claude`, `opencode2`, `codex`, …), when
/// rakumux.com takes it.
pub(crate) fn feed_agent(kind: &str) -> Option<&'static str> {
    match kind {
        "claude" | "claude-code" => Some("claude-code"),
        "opencode" | "opencode2" => Some("opencode"),
        other => AGENTS.iter().find(|agent| **agent == other).copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn the_list_is_the_contracts() {
        let common: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/feed/common.schema.json"
        )))
        .unwrap();
        assert_eq!(common["$defs"]["agent"]["enum"], serde_json::json!(AGENTS));
    }

    #[test]
    fn daemon_names_map_to_the_feeds_and_others_stay_out() {
        for (kind, feed) in [
            ("claude", Some("claude-code")),
            ("claude-code", Some("claude-code")),
            ("opencode2", Some("opencode")),
            ("cursor-agent", Some("cursor-agent")),
            ("kimi", Some("kimi")),
            ("gemini", None),
            ("cursor", None),
            ("Codex", None),
            ("", None),
        ] {
            assert_eq!(feed_agent(kind), feed, "{kind}");
        }
        for agent in AGENTS {
            assert_eq!(feed_agent(agent), Some(*agent));
        }
    }
}
