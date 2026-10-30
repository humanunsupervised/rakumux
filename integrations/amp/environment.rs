/// What Amp sets for the commands its tools run (its thread for `amp`
/// subcommands that default to it, and the agent-neutral markers it reads):
/// a session started from Amp's shell tool must not inherit them.
pub(super) const VARIABLES: &[&str] = &[
    "AMP_THREAD_ID",
    "AMP_CURRENT_THREAD_ID",
    "AGENT_THREAD_ID",
    "AI_AGENT",
];

#[cfg(test)]
mod tests {
    #[test]
    fn a_session_started_from_amp_doesnt_inherit_its_thread() {
        for name in super::VARIABLES {
            assert!(super::super::inherited_marker(name), "{name}");
        }
        assert!(!super::super::inherited_marker("AMP_API_KEY"));
        assert!(!super::super::inherited_marker("AMP_HOME"));
    }
}
