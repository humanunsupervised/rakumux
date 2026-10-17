// Runtime feature flags for gating UI/flows
// Configure these toggles in code only (no env vars).

#[derive(Debug, Clone, Copy)]
pub struct FeatureFlags {
    pub auth: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        // Toggle defaults here
        Self { auth: false }
    }
}

impl FeatureFlags {
    // Back-compat: construct flags using code defaults (no environment variables)
    pub fn from_env() -> Self {
        Self::default()
    }
}
