//! What Kimi Code sets for its plugins' hooks and MCP servers that means
//! nothing outside them: a daemon started from inside one (`rakumux-mcp`
//! starting `rakumux-api`) would pass it to every session. Its
//! `KIMI_CODE_HOME` is the user's own Kimi home and is kept; its tools'
//! `NO_COLOR` may be the user's own too.
pub(super) const VARIABLES: &[&str] = &["KIMI_PLUGIN_ROOT"];

#[cfg(test)]
mod tests {
    #[test]
    fn only_the_plugin_root_is_kimis_own() {
        assert!(super::super::inherited_marker("KIMI_PLUGIN_ROOT"));
        for name in ["KIMI_CODE_HOME", "NO_COLOR", "KIMI_MODEL_NAME"] {
            assert!(!super::super::inherited_marker(name), "{name}");
        }
    }
}
