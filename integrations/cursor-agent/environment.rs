/// What Cursor's agent sets for the commands its Shell tool runs (and its
/// launcher for itself): a session started from one isn't inside Cursor.
pub(super) const VARIABLES: &[&str] = &[
    "CURSOR_AGENT",
    "CURSOR_CONVERSATION_ID",
    "CURSOR_REQUEST_ID",
    "AGENT_TRANSCRIPTS",
    "CURSOR_INVOKED_AS",
];

#[cfg(test)]
mod tests {
    #[test]
    fn cursors_markers_stay_out_of_new_sessions_but_its_settings_dont() {
        for name in super::VARIABLES {
            assert!(super::super::inherited_marker(name), "{name}");
        }
        // The user's own settings for Cursor reach the agents they start.
        for name in ["CURSOR_API_KEY", "CURSOR_CONFIG_DIR", "CURSOR_DATA_DIR"] {
            assert!(!super::super::inherited_marker(name), "{name}");
        }
    }
}
