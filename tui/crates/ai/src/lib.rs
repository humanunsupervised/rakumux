//! Provider adapters shared by AI naming and daemon-owned activity summaries.

pub mod provider;
pub mod summary;

use serde::{Deserialize, Serialize};
use std::fmt;

/// AI credentials and model selection, stored at the top of `config.json`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Config {
    pub anthropic_api_key: Option<String>,
    pub openai_api_key: Option<String>,
    pub ai_primary_provider: Option<String>,
    pub openai_model: Option<String>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field(
                "anthropic_api_key",
                &self.anthropic_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "openai_api_key",
                &self.openai_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("ai_primary_provider", &self.ai_primary_provider)
            .field("openai_model", &self.openai_model)
            .finish()
    }
}
