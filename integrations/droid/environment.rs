/// Droid gives the commands it runs (`Execute`) its session's id; a daemon
/// started from one would hand it to every session.
pub(super) const VARIABLES: &[&str] = &["FACTORY_SESSION_ID"];

#[cfg(test)]
mod tests {
    #[test]
    fn droids_session_id_never_reaches_a_new_session() {
        assert!(super::super::inherited_marker("FACTORY_SESSION_ID"));
        assert!(!super::super::inherited_marker("FACTORY_HOME_OVERRIDE"));
    }
}
