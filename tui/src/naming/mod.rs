//! AI session naming: suggest a short name for a session from its recent
//! command history and screen, using the configured provider (Anthropic or
//! OpenAI) directly over HTTPS. Replaces the shell `ai-generate-name-*`
//! services; providers, models, prompt and name clean-up are the same.

mod history;
mod prompt;
use rakumux_ai::provider;

use crate::app::Config;

pub use history::load_history;

#[derive(Debug, Clone, Default)]
pub struct NamingInput {
    /// Recent commands typed in the session, oldest first.
    pub history: Vec<String>,
    /// Plain text of the visible screen.
    pub screen: String,
}

/// A short descriptive name (`a-z`, `A-Z`, `0-9`, `_`, `-`; words joined by
/// `_`; at most 100 characters), or `Ok(None)` when no provider is configured
/// or the input has nothing to go on.
///
/// API keys come from `config`, falling back to the `ANTHROPIC_API_KEY` /
/// `OPENAI_API_KEY` environment variables. Errors cover failed requests,
/// error responses and replies that leave no usable name.
pub async fn suggest_session_name(
    config: &Config,
    input: &NamingInput,
) -> anyhow::Result<Option<String>> {
    suggest_with_env(config, input, &|name| std::env::var(name).ok()).await
}

async fn suggest_with_env(
    config: &Config,
    input: &NamingInput,
    env: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<String>> {
    let Some(content) = prompt::build_content(input) else {
        return Ok(None);
    };
    let Some(provider) = provider::choose_provider(config, env) else {
        return Ok(None);
    };
    let reply = provider::request_text(&provider, &prompt::build_prompt(&content)).await?;
    prompt::clean_name(&reply)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("the AI reply contained no usable session name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[tokio::test]
    async fn naming_without_a_provider_is_none() {
        let input = NamingInput {
            history: vec!["cargo test".into()],
            screen: "running 3 tests".into(),
        };
        let name = suggest_with_env(&Config::default(), &input, &no_env)
            .await
            .unwrap();
        assert_eq!(name, None);
    }

    #[tokio::test]
    async fn naming_without_input_is_none_even_with_a_key() {
        let config = Config {
            anthropic_api_key: Some("sk-ant-test".into()),
            ..Config::default()
        };
        let input = NamingInput {
            history: vec!["  ".into()],
            screen: "\n \n".into(),
        };
        // Returns before any request is built.
        let name = suggest_with_env(&config, &input, &no_env).await.unwrap();
        assert_eq!(name, None);
    }
}
