//! Client for Rakumux's external API (`rakumux-api`).
//!
//! Discovery: `<RAKUMUX_HOME or ~/.rakumux>/api.json` names the API's URL,
//! token and pid. A file whose process is gone, or whose API doesn't answer
//! `/v1/health` with that pid, is ignored and `rakumux-api --detach` is
//! started (see `launch`). A request whose connection fails, or that is
//! refused because the API restarted with a new token, is retried once after
//! rediscovery.
//!
//! Identity: when `RAKUMUX_SESSION_ID` and `RAKUMUX_SESSION_TOKEN` are
//! both set, they are sent as `X-Rakumux-Session-Id`/`-Token`, so the
//! daemon treats calls as coming from that session. With only one of them
//! (an agent that strips `*TOKEN*` variables from MCP servers, or forwards
//! only those it's told), no call acts as the user: each is refused. A call
//! tagged with an agent conversation (OpenCode) is sent as
//! `X-Rakumux-Agent`/`-Conversation` instead: the daemon acts as the session
//! that bound that conversation.

use crate::launch::Launcher;
use rakumux_api::discovery::ApiInfo;
use rakumux_api::dto::Health;
use rakumux_api::error::ErrorEnvelope;
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use session_core::paths::Layout;
use std::time::Duration;

const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A tool-level error: shown to the model as an error result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    /// The API's error code (`not_found`, `invalid_argument`, …) or one of
    /// the MCP server's own (`api_unavailable`).
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl ToolError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            details: None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_argument", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    /// The API can't be reached or started.
    pub fn api_unavailable(message: impl Into<String>) -> Self {
        Self::new("api_unavailable", message)
    }

    /// The API couldn't act for an agent conversation: no running session
    /// bound it, or the daemon can't delegate (`details.agent_conversation`).
    pub fn conversation_unbound(&self) -> bool {
        self.details
            .as_ref()
            .and_then(|details| details.get("agent_conversation"))
            .is_some()
    }

    /// The API said a `folder` is needed (`details.missing == "folder"`, or
    /// `"project"` from builds that said project).
    pub fn needs_folder(&self) -> bool {
        matches!(
            self.details
                .as_ref()
                .and_then(|details| details.get("missing"))
                .and_then(Value::as_str),
            Some("folder" | "project")
        )
    }

    /// `{"error": {code, message, details?}}`.
    pub fn to_json(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(details) = &self.details {
            error["details"] = details.clone();
        }
        json!({ "error": error })
    }
}

/// A session identity from the environment.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub id: String,
    pub token: String,
}

impl std::fmt::Debug for SessionIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionIdentity({})", self.id)
    }
}

impl SessionIdentity {
    /// Both variables set and non-empty, else `None`.
    pub fn from_env() -> Option<Self> {
        match EnvIdentity::from_env() {
            EnvIdentity::Session(identity) => Some(identity),
            EnvIdentity::User | EnvIdentity::Incomplete => None,
        }
    }
}

/// Who the environment says the caller is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvIdentity {
    /// Neither variable: outside a Rakumux session, the user.
    User,
    /// Both variables: that session.
    Session(SessionIdentity),
    /// Only one of them: inside a session whose agent didn't pass both to
    /// this server. Never the user; every call that would act as the user
    /// is refused.
    Incomplete,
}

impl EnvIdentity {
    /// `RAKUMUX_SESSION_ID` and `RAKUMUX_SESSION_TOKEN`; an empty one counts
    /// as unset.
    pub fn from_env() -> Self {
        Self::from_vars(
            std::env::var(session_core::env::SESSION_ID).ok(),
            std::env::var(session_core::env::SESSION_TOKEN).ok(),
        )
    }

    fn from_vars(id: Option<String>, token: Option<String>) -> Self {
        let set = |value: Option<String>| value.filter(|value| !value.is_empty());
        match (set(id), set(token)) {
            (None, None) => Self::User,
            (Some(id), Some(token)) => Self::Session(SessionIdentity { id, token }),
            _ => Self::Incomplete,
        }
    }
}

/// The refusal of a call that would act as the user while the environment
/// gives half a session identity.
fn incomplete_identity() -> ToolError {
    ToolError::new(
        "unauthenticated",
        "rakumux-mcp got only one of RAKUMUX_SESSION_ID and RAKUMUX_SESSION_TOKEN, so it can't \
         act as this Rakumux session and won't act as the user: configure the agent to pass both \
         to rakumux-mcp",
    )
}

/// The connection to the API, shared by every tool call.
pub struct ApiClient {
    layout: Layout,
    http: reqwest::Client,
    identity: EnvIdentity,
    launcher: Launcher,
    current: tokio::sync::Mutex<Option<ApiInfo>>,
}

impl ApiClient {
    pub fn new(
        layout: Layout,
        identity: EnvIdentity,
        launcher: Launcher,
    ) -> Result<Self, ToolError> {
        let http = reqwest::Client::builder()
            // 127.0.0.1 must never go through an HTTP(S)_PROXY.
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("rakumux-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ToolError::new("internal", format!("cannot create HTTP client: {e}")))?;
        Ok(Self {
            layout,
            http,
            identity,
            launcher,
            current: tokio::sync::Mutex::new(None),
        })
    }

    /// The session identity forwarded with requests, if any.
    pub fn identity(&self) -> Option<&SessionIdentity> {
        match &self.identity {
            EnvIdentity::Session(identity) => Some(identity),
            EnvIdentity::User | EnvIdentity::Incomplete => None,
        }
    }

    /// A view that sends the session identity (when `forward_identity` and
    /// one is configured) or acts as the user. With an incomplete identity,
    /// every call it makes is refused.
    pub fn caller(&self, forward_identity: bool) -> Api<'_> {
        Api {
            client: self,
            acting: match &self.identity {
                EnvIdentity::Session(_) if forward_identity => Acting::Session,
                EnvIdentity::Incomplete => Acting::Refused,
                _ => Acting::User,
            },
        }
    }

    /// A view that acts as the session that bound `conversation` of `agent`.
    pub fn for_conversation(&self, agent: &str, conversation: &str) -> Api<'_> {
        Api {
            client: self,
            acting: Acting::Conversation {
                agent: agent.to_owned(),
                conversation: conversation.to_owned(),
            },
        }
    }

    /// A healthy API: the cached one, or one found (or started) anew.
    async fn connection(&self, refresh: bool) -> Result<ApiInfo, ToolError> {
        let mut current = self.current.lock().await;
        if !refresh {
            if let Some(info) = current.as_ref() {
                return Ok(info.clone());
            }
        }
        *current = None;
        let info = match self.healthy_from_file().await {
            Some(info) => info,
            None => {
                self.launcher.start().await?;
                self.healthy_from_file().await.ok_or_else(|| {
                    ToolError::api_unavailable(format!(
                        "rakumux-api started but {} names no healthy API",
                        rakumux_api::discovery::info_path(&self.layout).display()
                    ))
                })?
            }
        };
        *current = Some(info.clone());
        Ok(info)
    }

    /// `api.json`, if its process lives and answers `/v1/health` as itself.
    async fn healthy_from_file(&self) -> Option<ApiInfo> {
        let info = ApiInfo::load(&self.layout).ok()?;
        if !info.process_alive() {
            return None;
        }
        let health: Health = self
            .http
            .get(format!("{}/v1/health", info.url))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        (health.pid == info.pid).then_some(info)
    }
}

/// API calls as one caller (see [`ApiClient::caller`]).
pub struct Api<'a> {
    client: &'a ApiClient,
    acting: Acting,
}

/// Which identity headers a call sends.
enum Acting {
    User,
    /// The environment's session identity.
    Session,
    /// None: the environment names a session without its token.
    Refused,
    Conversation {
        agent: String,
        conversation: String,
    },
}

impl Api<'_> {
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ToolError> {
        self.send(Method::GET, path, query, None).await
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, ToolError> {
        self.send(Method::POST, path, &[], Some(body)).await
    }

    pub async fn patch<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, ToolError> {
        self.send(Method::PATCH, path, &[], Some(body)).await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<T, ToolError> {
        if matches!(self.acting, Acting::Refused) {
            return Err(incomplete_identity());
        }
        let mut refreshed = false;
        loop {
            let info = self.client.connection(refreshed).await?;
            let mut request = self
                .client
                .http
                .request(method.clone(), format!("{}{path}", info.url))
                .bearer_auth(&info.token)
                .query(query);
            match &self.acting {
                Acting::User | Acting::Refused => {}
                Acting::Session => {
                    if let Some(identity) = self.client.identity() {
                        request = request
                            .header(rakumux_api::headers::SESSION_ID, &identity.id)
                            .header(rakumux_api::headers::SESSION_TOKEN, &identity.token);
                    }
                }
                Acting::Conversation {
                    agent,
                    conversation,
                } => {
                    request = request
                        .header(rakumux_api::headers::AGENT, agent)
                        .header(rakumux_api::headers::AGENT_CONVERSATION, conversation);
                }
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = match request.send().await {
                Ok(response) => response,
                // Nothing was sent, so even a POST is safe to retry.
                Err(error) if !refreshed && error.is_connect() => {
                    refreshed = true;
                    continue;
                }
                Err(error) => {
                    return Err(ToolError::api_unavailable(format!(
                        "rakumux-api at {} did not answer: {error}",
                        info.url
                    )))
                }
            };
            let status = response.status();
            let bytes = response.bytes().await.map_err(|error| {
                ToolError::api_unavailable(format!("reading the answer of rakumux-api: {error}"))
            })?;
            if status.is_success() {
                return serde_json::from_slice(&bytes).map_err(|error| {
                    ToolError::new(
                        "internal",
                        format!("unexpected answer from rakumux-api: {error}"),
                    )
                });
            }
            // A restarted API has a new token: rediscover once.
            if status == StatusCode::UNAUTHORIZED && !refreshed && self.token_changed(&info) {
                refreshed = true;
                continue;
            }
            return Err(match serde_json::from_slice::<ErrorEnvelope>(&bytes) {
                Ok(envelope) => ToolError {
                    code: envelope.error.code,
                    message: envelope.error.message,
                    details: envelope.error.details,
                },
                Err(_) => ToolError::new(
                    "internal",
                    format!("rakumux-api answered {status} without an error body"),
                ),
            });
        }
    }

    fn token_changed(&self, info: &ApiInfo) -> bool {
        ApiInfo::load(&self.client.layout).map_or(true, |current| current.token != info.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_a_session_identity_is_never_the_user() {
        let some = |value: &str| Some(value.to_owned());
        assert_eq!(EnvIdentity::from_vars(None, None), EnvIdentity::User);
        assert_eq!(
            EnvIdentity::from_vars(some(""), some("")),
            EnvIdentity::User
        );
        assert_eq!(
            EnvIdentity::from_vars(some("01JSELF"), some("t")),
            EnvIdentity::Session(SessionIdentity {
                id: "01JSELF".into(),
                token: "t".into(),
            })
        );
        // Gemini CLI strips `*TOKEN*` variables from MCP servers' environment.
        for (id, token) in [
            (some("01JSELF"), None),
            (some("01JSELF"), some("")),
            (None, some("t")),
        ] {
            assert_eq!(EnvIdentity::from_vars(id, token), EnvIdentity::Incomplete);
        }
    }
}
