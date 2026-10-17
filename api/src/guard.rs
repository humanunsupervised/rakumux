//! Checks every request passes before it reaches a route.
//!
//! The loopback listener (127.0.0.1) is reachable by every local process
//! and by browsers. So:
//! - `Host` must name a loopback address (defeats DNS rebinding);
//! - requests that carry a browser `Origin` (or `Sec-Fetch-Site`) header are
//!   refused, and no CORS headers are ever sent;
//! - every route except `GET /v1/health` needs `Authorization: Bearer
//!   <token>` (from `api.json`), compared in constant time.
//!
//! The remote listener (remote access, see `remote`) admits a request by
//! its token alone: `Authorization: Bearer <remote token>` (from
//! `remote.json`), compared in constant time. It doesn't check `Host` and
//! allows `Origin` (React Native's WebSocket sends one); a remote caller is
//! always the user, so the identity headers are refused there.

use crate::daemon::Identity;
use crate::error::HttpError;
use crate::headers;
use hyper::header::{HeaderMap, AUTHORIZATION, HOST, ORIGIN};
use session_core::ids::Token;

/// Longest accepted session id / token header value.
const MAX_IDENTITY_LEN: usize = 256;

/// `Host` must be `127.0.0.1`, `localhost` or `[::1]`, with any port.
pub fn check_host(headers: &HeaderMap) -> Result<(), HttpError> {
    let host = headers
        .get(HOST)
        .ok_or_else(|| HttpError::forbidden("a Host header is required"))?
        .to_str()
        .map_err(|_| HttpError::forbidden("the Host header is not valid text"))?;
    if is_loopback_host(host) {
        Ok(())
    } else {
        Err(HttpError::forbidden(format!(
            "Host {host:?} is not a loopback address; use 127.0.0.1"
        )))
    }
}

/// Refuse requests made by a browser.
pub fn check_not_browser(headers: &HeaderMap) -> Result<(), HttpError> {
    if headers.contains_key(ORIGIN) || headers.contains_key("sec-fetch-site") {
        return Err(HttpError::forbidden(
            "requests from web pages are not accepted (Origin header present)",
        ));
    }
    Ok(())
}

/// `Authorization: Bearer <token>` with the API's token.
pub fn check_bearer(headers: &HeaderMap, token: &Token) -> Result<(), HttpError> {
    bearer(
        headers,
        token,
        "the token is in api.json",
        "invalid API token",
    )
}

/// `Authorization: Bearer <token>` with the remote pairing token.
pub fn check_remote_bearer(headers: &HeaderMap, token: &Token) -> Result<(), HttpError> {
    bearer(
        headers,
        token,
        "`rakumux remote token` prints the token",
        "invalid remote token (it changes with `rakumux remote token --rotate`)",
    )
}

/// Remote callers act as the user: any identity header is refused.
pub fn refuse_identity(headers: &HeaderMap) -> Result<(), HttpError> {
    for name in [
        headers::SESSION_ID,
        headers::SESSION_TOKEN,
        headers::AGENT,
        headers::AGENT_CONVERSATION,
    ] {
        if headers.contains_key(name) {
            return Err(HttpError::forbidden(format!(
                "{name} is not accepted from remote clients; they act as the user"
            )));
        }
    }
    Ok(())
}

fn bearer(headers: &HeaderMap, token: &Token, hint: &str, invalid: &str) -> Result<(), HttpError> {
    let value = headers.get(AUTHORIZATION).ok_or_else(|| {
        HttpError::unauthenticated(format!("missing `Authorization: Bearer <token>`; {hint}"))
    })?;
    let value = value
        .to_str()
        .map_err(|_| HttpError::unauthenticated("malformed Authorization header"))?;
    let (scheme, presented) = value
        .split_once(' ')
        .ok_or_else(|| HttpError::unauthenticated("Authorization must be `Bearer <token>`"))?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(HttpError::unauthenticated(
            "Authorization must be `Bearer <token>`",
        ));
    }
    if token.matches(presented.trim()) {
        Ok(())
    } else {
        Err(HttpError::unauthenticated(invalid))
    }
}

/// The caller's identity from `X-Rakumux-Session-Id` and
/// `X-Rakumux-Session-Token` (both → that session), or `X-Rakumux-Agent`
/// and `X-Rakumux-Agent-Conversation` (both → the session that bound that
/// conversation); none → the user.
pub fn identity(headers: &HeaderMap) -> Result<Identity, HttpError> {
    let id = header_text(headers, headers::SESSION_ID)?;
    let token = header_text(headers, headers::SESSION_TOKEN)?;
    let agent = header_text(headers, headers::AGENT)?;
    let conversation = header_text(headers, headers::AGENT_CONVERSATION)?;
    match (agent, conversation) {
        (None, None) => {}
        (Some(agent), Some(conversation)) => {
            if id.is_some() || token.is_some() {
                return Err(HttpError::invalid(
                    "send either the session headers or the agent headers, not both",
                ));
            }
            let valid_agent = !agent.is_empty()
                && agent
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
            if !valid_agent {
                return Err(HttpError::invalid(
                    "X-Rakumux-Agent must be an agent kind such as opencode",
                ));
            }
            if conversation.is_empty() {
                return Err(HttpError::invalid("X-Rakumux-Agent-Conversation is empty"));
            }
            return Ok(Identity::Delegated {
                agent,
                conversation,
            });
        }
        _ => {
            return Err(HttpError::invalid(
                "X-Rakumux-Agent and X-Rakumux-Agent-Conversation must be sent together",
            ))
        }
    }
    match (id, token) {
        (None, None) => Ok(Identity::User),
        (Some(id), Some(token)) => {
            let valid_id = !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !valid_id {
                return Err(HttpError::invalid(
                    "X-Rakumux-Session-Id is not a session id",
                ));
            }
            if token.is_empty() {
                return Err(HttpError::invalid("X-Rakumux-Session-Token is empty"));
            }
            Ok(Identity::Session { id, token })
        }
        _ => Err(HttpError::invalid(
            "X-Rakumux-Session-Id and X-Rakumux-Session-Token must be sent together",
        )),
    }
}

fn header_text(headers: &HeaderMap, name: &str) -> Result<Option<String>, HttpError> {
    let Some(value) = headers.get(name) else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| HttpError::invalid(format!("{name} is not valid text")))?
        .trim();
    if text.len() > MAX_IDENTITY_LEN {
        return Err(HttpError::invalid(format!("{name} is too long")));
    }
    Ok(Some(text.to_owned()))
}

fn is_loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((inner, port)) if valid_port_suffix(port) => inner,
            _ => return false,
        },
        None => match host.rsplit_once(':') {
            Some((name, port)) if valid_port_suffix(&format!(":{port}")) => name,
            Some(_) => return false,
            None => host,
        },
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

/// "" or ":<digits>".
fn valid_port_suffix(suffix: &str) -> bool {
    match suffix.strip_prefix(':') {
        None => suffix.is_empty(),
        Some(port) => {
            !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn only_loopback_hosts_pass() {
        for ok in [
            "127.0.0.1",
            "127.0.0.1:4000",
            "localhost:1",
            "LOCALHOST",
            "[::1]:80",
            "[::1]",
        ] {
            assert!(check_host(&headers(&[("host", ok)])).is_ok(), "{ok}");
        }
        for bad in [
            "evil.com",
            "evil.com:4000",
            "127.0.0.1.evil.com",
            "localhost:abc",
            "[::2]:80",
            "10.0.0.1",
            "",
        ] {
            let error = check_host(&headers(&[("host", bad)])).unwrap_err();
            assert_eq!(error.status.as_u16(), 403, "{bad}");
        }
        assert!(check_host(&HeaderMap::new()).is_err());
    }

    #[test]
    fn browser_requests_are_refused() {
        assert!(check_not_browser(&headers(&[("origin", "http://evil.com")])).is_err());
        assert!(check_not_browser(&headers(&[("origin", "null")])).is_err());
        assert!(check_not_browser(&headers(&[("sec-fetch-site", "cross-site")])).is_err());
        assert!(check_not_browser(&headers(&[("user-agent", "curl/8")])).is_ok());
    }

    #[test]
    fn bearer_token_must_match() {
        let token = Token::from_string("abc123");
        assert!(check_bearer(&headers(&[("authorization", "Bearer abc123")]), &token).is_ok());
        assert!(check_bearer(&headers(&[("authorization", "bearer abc123")]), &token).is_ok());
        for bad in ["Bearer abc124", "Basic abc123", "abc123", "Bearer "] {
            let error = check_bearer(&headers(&[("authorization", bad)]), &token).unwrap_err();
            assert_eq!(error.status.as_u16(), 401, "{bad}");
        }
        assert_eq!(
            check_bearer(&HeaderMap::new(), &token).unwrap_err().code(),
            "unauthenticated"
        );
    }

    #[test]
    fn the_remote_token_is_a_separate_bearer() {
        let api = Token::from_string("a".repeat(64));
        let remote = Token::from_string("abcdefghijklmnopqrstuvwxyz");
        let with = |token: &str| headers(&[("authorization", &format!("Bearer {token}"))]);
        assert!(check_remote_bearer(&with(remote.as_str()), &remote).is_ok());
        let error = check_remote_bearer(&with(api.as_str()), &remote).unwrap_err();
        assert_eq!(error.status.as_u16(), 401);
        assert!(error.body.message.contains("remote token"), "{error:?}");
        assert!(check_bearer(&with(remote.as_str()), &api).is_err());
        // One character off, or a prefix, never matches.
        assert!(check_remote_bearer(&with("abcdefghijklmnopqrstuvwxyy"), &remote).is_err());
        assert!(check_remote_bearer(&with("abcdefghijklm"), &remote).is_err());
        let error = check_remote_bearer(&HeaderMap::new(), &remote).unwrap_err();
        assert!(error.body.message.contains("rakumux remote token"));
    }

    #[test]
    fn remote_callers_may_not_send_identity_headers() {
        assert!(refuse_identity(&headers(&[("origin", "http://phone")])).is_ok());
        for name in [
            "x-rakumux-session-id",
            "x-rakumux-session-token",
            "x-rakumux-agent",
            "x-rakumux-agent-conversation",
        ] {
            let error = refuse_identity(&headers(&[(name, "x")])).unwrap_err();
            assert_eq!(error.status.as_u16(), 403, "{name}");
            assert_eq!(error.code(), "permission_denied");
        }
    }

    #[test]
    fn identity_needs_both_headers() {
        assert_eq!(identity(&HeaderMap::new()).unwrap(), Identity::User);
        let both = headers(&[
            ("x-rakumux-session-id", "01JABC"),
            ("x-rakumux-session-token", "tok"),
        ]);
        assert_eq!(
            identity(&both).unwrap(),
            Identity::Session {
                id: "01JABC".into(),
                token: "tok".into()
            }
        );
        let only_id = headers(&[("x-rakumux-session-id", "01JABC")]);
        assert_eq!(identity(&only_id).unwrap_err().status.as_u16(), 400);
        let bad_id = headers(&[
            ("x-rakumux-session-id", "../x"),
            ("x-rakumux-session-token", "tok"),
        ]);
        assert!(identity(&bad_id).is_err());
    }

    #[test]
    fn agent_headers_ask_for_a_delegated_identity() {
        let agent = headers(&[
            ("x-rakumux-agent", "opencode"),
            ("x-rakumux-agent-conversation", "ses_1"),
        ]);
        assert_eq!(
            identity(&agent).unwrap(),
            Identity::Delegated {
                agent: "opencode".into(),
                conversation: "ses_1".into()
            }
        );
        let only_agent = headers(&[("x-rakumux-agent", "opencode")]);
        assert_eq!(identity(&only_agent).unwrap_err().status.as_u16(), 400);
        let bad_agent = headers(&[
            ("x-rakumux-agent", "Open Code"),
            ("x-rakumux-agent-conversation", "ses_1"),
        ]);
        assert!(identity(&bad_agent).is_err());
        let mixed = headers(&[
            ("x-rakumux-agent", "opencode"),
            ("x-rakumux-agent-conversation", "ses_1"),
            ("x-rakumux-session-id", "01JABC"),
            ("x-rakumux-session-token", "tok"),
        ]);
        assert_eq!(identity(&mixed).unwrap_err().status.as_u16(), 400);
    }
}
