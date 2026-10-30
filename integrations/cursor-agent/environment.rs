/// What Cursor's agent sets for the commands its Shell tool runs (and its
/// launcher for itself): a session started from one isn't inside Cursor.
pub(super) const VARIABLES: &[&str] = &[
    "CURSOR_AGENT",
    "CURSOR_CONVERSATION_ID",
    "CURSOR_REQUEST_ID",
    "AGENT_TRANSCRIPTS",
    "CURSOR_INVOKED_AS",
];
