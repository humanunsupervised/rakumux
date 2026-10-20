//! Normalize agent-owned MCP metadata; the transport applies common identity policy.
use serde_json::{Map, Value};
#[path = "Open Code/mcp.rs"]
mod opencode;

/// Agent conversation identified by a tool call, independent of its MCP transport.
pub struct AgentConversation {
    pub agent: &'static str,
    pub id: String,
    pub unbound_message: &'static str,
}

/// Interpret known integration metadata. Unknown callers use normal environment identity.
pub fn conversation(metadata: &Map<String, Value>) -> Option<AgentConversation> {
    opencode::conversation(metadata)
}
