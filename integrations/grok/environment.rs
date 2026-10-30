/// What Grok Build sets in the processes it starts (its tools' shells, its MCP
/// servers): its session, and `GROK_AGENT=1`, which is also Grok's own setting
/// for which agent to run, so a Grok started in a session that inherited it
/// would look for an agent named `1`.
pub(super) const VARIABLES: &[&str] = &["GROK_SESSION_ID", "GROK_AGENT"];
