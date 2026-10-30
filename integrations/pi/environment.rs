//! Pi marks its own process (`PI_CODING_AGENT`, `AI_AGENT`) and gives its
//! `bash` tool's commands the session's state (`PI_SESSION_*`, `PI_PROVIDER`,
//! `PI_MODEL`, `PI_REASONING_LEVEL`). A daemon started from inside Pi would
//! otherwise pass them to every session.
pub(super) const VARIABLES: &[&str] = &[
    "PI_CODING_AGENT",
    "AI_AGENT",
    "PI_SESSION_ID",
    "PI_SESSION_FILE",
    "PI_PROVIDER",
    "PI_MODEL",
    "PI_REASONING_LEVEL",
];

#[cfg(test)]
mod tests {
    #[test]
    fn pis_markers_never_reach_a_new_session() {
        for name in super::VARIABLES {
            assert!(super::super::inherited_marker(name), "{name}");
        }
        // Pi's own configuration is the user's, not a marker.
        assert!(!super::super::inherited_marker("PI_CODING_AGENT_DIR"));
        assert!(!super::super::inherited_marker(
            "PI_CODING_AGENT_SESSION_DIR"
        ));
    }
}
