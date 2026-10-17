//! Caller identity. User clients present the daemon's user token (from the
//! registration file); processes inside a session present that session's
//! token, issued at spawn/revive and held **in memory only**: tokens die with
//! the daemon, like the processes that received them. A restart in place
//! hands them to the next daemon image along with the processes.

use crate::control_api::Caller;
use session_core::api::{ApiError, Auth, CallerInfo, ErrorCode, Hello};
use session_core::ids::{SessionId, Token};
use session_core::model::ScopeRoot;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct SessionTokens {
    tokens: HashMap<SessionId, Token>,
}

impl SessionTokens {
    /// A fresh token for `id`, replacing any previous one.
    pub fn issue(&mut self, id: &SessionId) -> Token {
        let token = Token::generate();
        self.tokens.insert(id.clone(), token.clone());
        token
    }

    /// Restore a token handed over by the previous daemon image.
    pub fn restore(&mut self, id: &SessionId, token: Token) {
        self.tokens.insert(id.clone(), token);
    }

    /// Every token (for a restart in place).
    pub fn export(&self) -> Vec<(SessionId, String)> {
        self.tokens
            .iter()
            .map(|(id, token)| (id.clone(), token.as_str().to_string()))
            .collect()
    }

    pub fn revoke(&mut self, id: &SessionId) {
        self.tokens.remove(id);
    }

    pub fn verify(&self, id: &SessionId, presented: &str) -> bool {
        self.tokens
            .get(id)
            .is_some_and(|token| token.matches(presented))
    }
}

/// Check a hello's protocol and credentials. `scope_of` resolves a session's
/// scope from the registry; `bound` the running session that bound an
/// agent's conversation (`Auth::Delegated`).
pub(crate) fn authenticate(
    hello: &Hello,
    user_token: &Token,
    tokens: &SessionTokens,
    scope_of: impl FnOnce(&SessionId) -> Option<ScopeRoot>,
    bound: impl FnOnce(&str, &str) -> Option<SessionId>,
) -> Result<Caller, ApiError> {
    if hello.protocol != session_core::PROTOCOL_VERSION {
        let mut error = ApiError::new(
            ErrorCode::ProtocolMismatch,
            format!(
                "client speaks protocol {}, daemon speaks {}; restart the daemon",
                hello.protocol,
                session_core::PROTOCOL_VERSION
            ),
        );
        error.details = Some(serde_json::json!({
            "daemon_protocol": session_core::PROTOCOL_VERSION,
            "client_protocol": hello.protocol,
        }));
        return Err(error);
    }
    let info = match &hello.auth {
        Auth::User { token } => {
            if !user_token.matches(token) {
                return Err(unauthenticated());
            }
            CallerInfo::User
        }
        Auth::Session { id, token } => {
            if !tokens.verify(id, token) {
                return Err(unauthenticated());
            }
            let scope = scope_of(id).ok_or_else(unauthenticated)?;
            CallerInfo::Session {
                id: id.clone(),
                scope,
            }
        }
        // The user token, downgraded to the session: never an escalation.
        Auth::Delegated {
            token,
            agent,
            conversation,
        } => {
            if !user_token.matches(token) {
                return Err(unauthenticated());
            }
            let id = bound(agent, conversation).ok_or_else(|| {
                ApiError::new(
                    ErrorCode::Unauthenticated,
                    format!(
                        "no running Rakumux session has bound {agent} conversation \
                         {conversation} (`rakumux session bind`)"
                    ),
                )
            })?;
            let scope = scope_of(&id).ok_or_else(unauthenticated)?;
            CallerInfo::Session { id, scope }
        }
    };
    Ok(Caller {
        info,
        client: hello.client.clone(),
        max_blocking_ms: hello.max_blocking_ms,
    })
}

fn unauthenticated() -> ApiError {
    ApiError::new(ErrorCode::Unauthenticated, "invalid or expired credentials")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(auth: Auth) -> Hello {
        Hello {
            max_blocking_ms: None,
            protocol: session_core::PROTOCOL_VERSION,
            client: "cli".into(),
            version: "0".into(),
            auth,
        }
    }

    #[test]
    fn session_tokens_are_replaced_and_revoked() {
        let mut tokens = SessionTokens::default();
        let id = SessionId::from_string("S");
        let first = tokens.issue(&id);
        let second = tokens.issue(&id);
        assert!(!tokens.verify(&id, first.as_str()));
        assert!(tokens.verify(&id, second.as_str()));
        tokens.revoke(&id);
        assert!(!tokens.verify(&id, second.as_str()));
    }

    #[test]
    fn session_auth_requires_a_known_scope() {
        let mut tokens = SessionTokens::default();
        let id = SessionId::from_string("S");
        let token = tokens.issue(&id);
        let user = Token::generate();
        let auth = Auth::Session {
            id: id.clone(),
            token: token.as_str().into(),
        };
        let err =
            authenticate(&hello(auth.clone()), &user, &tokens, |_| None, |_, _| None).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthenticated);
        let scope = ScopeRoot::from_canonical("/r".into());
        let caller = authenticate(
            &hello(auth),
            &user,
            &tokens,
            |_| Some(scope.clone()),
            |_, _| None,
        )
        .unwrap();
        assert_eq!(caller.session_id(), Some(&id));
    }

    #[test]
    fn delegated_auth_needs_the_user_token_and_a_binding() {
        let tokens = SessionTokens::default();
        let user = Token::generate();
        let id = SessionId::from_string("S");
        let scope = ScopeRoot::from_canonical("/r".into());
        let delegated = |token: &str| Auth::Delegated {
            token: token.into(),
            agent: "opencode".into(),
            conversation: "ses_1".into(),
        };
        let bound = |agent: &str, conversation: &str| {
            (agent == "opencode" && conversation == "ses_1").then(|| SessionId::from_string("S"))
        };
        let err = authenticate(
            &hello(delegated("wrong")),
            &user,
            &tokens,
            |_| Some(scope.clone()),
            bound,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthenticated);
        let err = authenticate(
            &hello(delegated(user.as_str())),
            &user,
            &tokens,
            |_| Some(scope.clone()),
            |_, _| None,
        )
        .unwrap_err();
        assert!(err.message.contains("ses_1"), "{}", err.message);
        let caller = authenticate(
            &hello(delegated(user.as_str())),
            &user,
            &tokens,
            |_| Some(scope.clone()),
            bound,
        )
        .unwrap();
        assert_eq!(caller.session_id(), Some(&id));
        assert!(matches!(caller.info, CallerInfo::Session { scope: ref s, .. } if *s == scope));
    }
}
