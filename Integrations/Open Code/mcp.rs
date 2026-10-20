//! OpenCode's shared service tags calls; only the CLI plugin knows the terminal.
use super::*;

pub(super) fn conversation(metadata: &Map<String, Value>) -> Option<AgentConversation> {
    let id = metadata
        .get("ai.opencode/sessionID")?
        .as_str()
        .filter(|id| !id.is_empty())?;
    Some(AgentConversation {
        agent: "opencode",
        id: id.into(),
        unbound_message:
            "this OpenCode conversation isn't linked to a Rakumux session, and this MCP \
            server is shared by every OpenCode conversation, so it can't tell which \
            session is calling; tools that change sessions are disabled. Install the \
            Rakumux plugin for OpenCode (`rakumux-mcp opencode-plugin --install`, then \
            restart OpenCode), or run `rakumux new --detach` from the shell tool.",
    })
}
