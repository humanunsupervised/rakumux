//! Provider selection and the HTTPS calls to Anthropic and OpenAI, ported
//! from `ai_choose_provider` and the `curl` calls of the shell
//! `ai-generate-name-from-base64` service.

use crate::app::Config;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::fmt;
use std::time::Duration;

pub(crate) const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Model used for Anthropic naming requests. The shell service used
/// `claude-3-5-haiku-latest`, which Anthropic retired on 2026-02-19.
pub(crate) const ANTHROPIC_MODEL: &str = "claude-haiku-5-5";
/// Claude Haiku 5.5 thinks by default and thinking counts toward
/// `max_tokens`; low effort keeps a one-line naming call fast and cheap, and
/// the larger cap leaves room for any thinking before the answer.
const ANTHROPIC_EFFORT: &str = "low";
const ANTHROPIC_MAX_TOKENS: u32 = 1024;

pub(crate) const OPENAI_CHAT_URL: &str = "https://api.openai.com/v1/chat/completions";
/// OpenAI model when `Config::openai_model` is unset.
pub(crate) const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
const OPENAI_TEMPERATURE: f64 = 0.2;

const OPENAI_MAX_TOKENS: u32 = 100;
/// Whole-request timeout (the shell service used `curl --max-time 20`).
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Environment variables consulted when the config has no key.
pub(crate) const ANTHROPIC_KEY_ENV: &str = "ANTHROPIC_API_KEY";
pub(crate) const OPENAI_KEY_ENV: &str = "OPENAI_API_KEY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderKind {
    Anthropic,
    OpenAi,
}

impl ProviderKind {
    fn label(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::OpenAi => "OpenAI",
        }
    }
}

/// A provider with a usable API key.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Provider {
    pub kind: ProviderKind,
    pub api_key: String,
    pub model: String,
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provider")
            .field("kind", &self.kind)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .finish()
    }
}

/// Pick the provider like `ai_choose_provider`: the configured primary
/// provider (`"openai"`, case-insensitive, else Anthropic) when it has a key,
/// otherwise the other one. Keys come from the config, then from
/// `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` via `env`; blank keys don't count.
/// `None` when neither provider has a key.
pub(crate) fn choose_provider(
    config: &Config,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<Provider> {
    let anthropic = || {
        api_key(config.anthropic_api_key.as_deref(), ANTHROPIC_KEY_ENV, env).map(|api_key| {
            Provider {
                kind: ProviderKind::Anthropic,
                api_key,
                model: ANTHROPIC_MODEL.to_string(),
            }
        })
    };
    let openai = || {
        api_key(config.openai_api_key.as_deref(), OPENAI_KEY_ENV, env).map(|api_key| Provider {
            kind: ProviderKind::OpenAi,
            api_key,
            model: config
                .openai_model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .unwrap_or(DEFAULT_OPENAI_MODEL)
                .to_string(),
        })
    };

    let openai_first = config
        .ai_primary_provider
        .as_deref()
        .is_some_and(|primary| primary.trim().eq_ignore_ascii_case("openai"));
    if openai_first {
        openai().or_else(anthropic)
    } else {
        anthropic().or_else(openai)
    }
}

fn api_key(
    configured: Option<&str>,
    env_var: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let non_blank = |key: &str| {
        let key = key.trim();
        (!key.is_empty()).then(|| key.to_string())
    };
    configured
        .and_then(non_blank)
        .or_else(|| env(env_var).as_deref().and_then(non_blank))
}

/// The naming request for `provider`, ready to send.
pub(crate) fn build_request(
    client: &reqwest::Client,
    provider: &Provider,
    prompt: &str,
) -> reqwest::RequestBuilder {
    let messages = json!([{ "role": "user", "content": prompt }]);
    match provider.kind {
        ProviderKind::Anthropic => client
            .post(ANTHROPIC_MESSAGES_URL)
            .timeout(REQUEST_TIMEOUT)
            .header("x-api-key", &provider.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&json!({
                "model": provider.model,
                "max_tokens": ANTHROPIC_MAX_TOKENS,
                "output_config": { "effort": ANTHROPIC_EFFORT },
                "messages": messages,
            })),
        ProviderKind::OpenAi => client
            .post(OPENAI_CHAT_URL)
            .timeout(REQUEST_TIMEOUT)
            .bearer_auth(&provider.api_key)
            .json(&json!({
                "model": provider.model,
                "max_tokens": OPENAI_MAX_TOKENS,
                "temperature": OPENAI_TEMPERATURE,
                "messages": messages,
            })),
    }
}

/// Send the naming request and return the model's raw reply text.
pub(crate) async fn request_name(provider: &Provider, prompt: &str) -> Result<String> {
    let label = provider.kind.label();
    let client = reqwest::Client::new();
    let response = build_request(&client, provider, prompt)
        .send()
        .await
        .with_context(|| format!("{label} naming request failed"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read the {label} response"))?;
    let parsed: Option<Value> = serde_json::from_str(&body).ok();

    if !status.is_success() {
        let detail = parsed
            .as_ref()
            .and_then(api_error_message)
            .unwrap_or("no error message");
        bail!("{label} API returned {status}: {detail}");
    }
    let parsed = parsed.ok_or_else(|| anyhow!("{label} returned a response that isn't JSON"))?;
    reply_text(provider.kind, &parsed)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("{label} response has no text content"))
}

/// The reply text of a successful response: the first `text` block of an
/// Anthropic message (it may follow `thinking` blocks), or the first choice's
/// message content from OpenAI.
pub(crate) fn reply_text(kind: ProviderKind, body: &Value) -> Option<&str> {
    match kind {
        ProviderKind::Anthropic => body
            .get("content")?
            .as_array()?
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .find_map(|block| block.get("text")?.as_str()),
        ProviderKind::OpenAi => body
            .get("choices")?
            .get(0)?
            .get("message")?
            .get("content")?
            .as_str(),
    }
}

/// `error.message` of an error response (both providers use this shape).
pub(crate) fn api_error_message(body: &Value) -> Option<&str> {
    body.get("error")?.get("message")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn config(anthropic: Option<&str>, openai: Option<&str>, primary: Option<&str>) -> Config {
        Config {
            anthropic_api_key: anthropic.map(str::to_string),
            openai_api_key: openai.map(str::to_string),
            ai_primary_provider: primary.map(str::to_string),
            openai_model: None,
        }
    }

    #[test]
    fn no_keys_means_no_provider() {
        assert_eq!(choose_provider(&Config::default(), &no_env), None);
        let blank = config(Some("  "), Some(""), None);
        assert_eq!(
            choose_provider(&blank, &env_of(&[(ANTHROPIC_KEY_ENV, " ")])),
            None
        );
    }

    #[test]
    fn anthropic_is_the_default_primary() {
        let provider = choose_provider(&config(Some("ant"), Some("oai"), None), &no_env).unwrap();
        assert_eq!(provider.kind, ProviderKind::Anthropic);
        assert_eq!(provider.api_key, "ant");
        assert_eq!(provider.model, ANTHROPIC_MODEL);

        let unknown = config(Some("ant"), Some("oai"), Some("gemini"));
        assert_eq!(
            choose_provider(&unknown, &no_env).unwrap().kind,
            ProviderKind::Anthropic
        );
    }

    #[test]
    fn openai_primary_wins_when_it_has_a_key() {
        let provider =
            choose_provider(&config(Some("ant"), Some("oai"), Some(" OpenAI ")), &no_env).unwrap();
        assert_eq!(provider.kind, ProviderKind::OpenAi);
        assert_eq!(provider.api_key, "oai");
        assert_eq!(provider.model, DEFAULT_OPENAI_MODEL);
    }

    #[test]
    fn falls_back_to_the_other_provider() {
        let provider =
            choose_provider(&config(Some("ant"), None, Some("openai")), &no_env).unwrap();
        assert_eq!(provider.kind, ProviderKind::Anthropic);

        let provider = choose_provider(&config(None, Some("oai"), None), &no_env).unwrap();
        assert_eq!(provider.kind, ProviderKind::OpenAi);
    }

    #[test]
    fn config_keys_win_over_env_and_env_fills_gaps() {
        let env = env_of(&[(ANTHROPIC_KEY_ENV, "env-ant"), (OPENAI_KEY_ENV, "env-oai")]);
        let provider = choose_provider(&config(Some(" cfg-ant\n"), None, None), &env).unwrap();
        assert_eq!(provider.api_key, "cfg-ant");

        let provider = choose_provider(&Config::default(), &env).unwrap();
        assert_eq!(provider.kind, ProviderKind::Anthropic);
        assert_eq!(provider.api_key, "env-ant");

        let provider = choose_provider(&config(None, None, Some("openai")), &env).unwrap();
        assert_eq!(provider.kind, ProviderKind::OpenAi);
        assert_eq!(provider.api_key, "env-oai");
    }

    #[test]
    fn openai_model_comes_from_config() {
        let mut cfg = config(None, Some("oai"), Some("openai"));
        cfg.openai_model = Some("gpt-4.1-mini".into());
        assert_eq!(
            choose_provider(&cfg, &no_env).unwrap().model,
            "gpt-4.1-mini"
        );
        cfg.openai_model = Some("  ".into());
        assert_eq!(
            choose_provider(&cfg, &no_env).unwrap().model,
            DEFAULT_OPENAI_MODEL
        );
    }

    #[test]
    fn debug_output_hides_the_key() {
        let provider =
            choose_provider(&config(Some("sk-ant-secret"), None, None), &no_env).unwrap();
        let debug = format!("{provider:?}");
        assert!(!debug.contains("sk-ant-secret"));
        assert!(debug.contains("redacted"));
    }

    fn body_of(request: &reqwest::Request) -> Value {
        let bytes = request.body().and_then(|b| b.as_bytes()).unwrap();
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn anthropic_request_uses_current_haiku_with_low_effort() {
        let provider = Provider {
            kind: ProviderKind::Anthropic,
            api_key: "sk-ant-test".into(),
            model: ANTHROPIC_MODEL.into(),
        };
        let request = build_request(&reqwest::Client::new(), &provider, "the prompt")
            .build()
            .unwrap();
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.url().as_str(), ANTHROPIC_MESSAGES_URL);
        assert_eq!(request.timeout(), Some(&REQUEST_TIMEOUT));
        let headers = request.headers();
        assert_eq!(headers["x-api-key"], "sk-ant-test");
        assert_eq!(headers["anthropic-version"], ANTHROPIC_VERSION);
        assert_eq!(headers["content-type"], "application/json");
        assert!(headers.get("authorization").is_none());
        assert_eq!(
            body_of(&request),
            json!({
                "model": "claude-haiku-5-5",
                "max_tokens": 1024,
                "output_config": { "effort": "low" },
                "messages": [{ "role": "user", "content": "the prompt" }],
            })
        );
    }

    #[test]
    fn openai_request_matches_the_shell_call() {
        let provider = Provider {
            kind: ProviderKind::OpenAi,
            api_key: "sk-test".into(),
            model: "gpt-4o-mini".into(),
        };
        let request = build_request(&reqwest::Client::new(), &provider, "the prompt")
            .build()
            .unwrap();
        assert_eq!(request.url().as_str(), OPENAI_CHAT_URL);
        assert_eq!(request.timeout(), Some(&REQUEST_TIMEOUT));
        let headers = request.headers();
        assert_eq!(headers["authorization"], "Bearer sk-test");
        assert_eq!(headers["content-type"], "application/json");
        assert!(headers.get("x-api-key").is_none());
        assert_eq!(
            body_of(&request),
            json!({
                "model": "gpt-4o-mini",
                "max_tokens": 100,
                "temperature": 0.2,
                "messages": [{ "role": "user", "content": "the prompt" }],
            })
        );
    }

    #[test]
    fn reply_text_reads_both_response_shapes() {
        let anthropic = json!({
            "content": [{ "type": "text", "text": "fixing_auth_bug" }],
            "stop_reason": "end_turn",
        });
        assert_eq!(
            reply_text(ProviderKind::Anthropic, &anthropic),
            Some("fixing_auth_bug")
        );

        // Claude Haiku 5.5 may think first; thinking text is omitted by default.
        let with_thinking = json!({
            "content": [
                { "type": "thinking", "thinking": "", "signature": "sig" },
                { "type": "text", "text": "renaming_sessions" },
            ],
            "stop_reason": "end_turn",
        });
        assert_eq!(
            reply_text(ProviderKind::Anthropic, &with_thinking),
            Some("renaming_sessions")
        );

        let openai = json!({
            "choices": [{ "message": { "role": "assistant", "content": "running_tests" } }],
        });
        assert_eq!(
            reply_text(ProviderKind::OpenAi, &openai),
            Some("running_tests")
        );

        assert_eq!(reply_text(ProviderKind::Anthropic, &openai), None);
        assert_eq!(reply_text(ProviderKind::OpenAi, &anthropic), None);
        assert_eq!(
            reply_text(ProviderKind::Anthropic, &json!({ "content": [] })),
            None
        );
    }

    #[test]
    fn api_error_message_reads_error_bodies() {
        let body = json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": "invalid x-api-key" },
        });
        assert_eq!(api_error_message(&body), Some("invalid x-api-key"));
        assert_eq!(api_error_message(&json!({ "ok": true })), None);
    }
}
