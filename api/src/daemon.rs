//! Daemon clients per caller identity.
//!
//! `session_core::client::Client` is blocking and serialises requests on one
//! connection; handlers call it from `tokio::task::spawn_blocking`. The pool
//! keeps one user client (which may spawn the daemon, like the CLI) and one
//! client per session identity (which never spawns it). Delegated identities
//! connect per request, so a conversation that moves to another session is
//! followed at once. A client whose
//! identity the daemon rejects, or whose daemon went away, is dropped so the
//! next request starts fresh.

use crate::dto::DaemonHealth;
use session_core::api::{ApiError, ErrorCode};
use session_core::client::{AuthMode, Client, ClientConfig};
use session_core::ids::SessionId;
use session_core::paths::Layout;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Session clients kept at most; the oldest connections are dropped first.
const MAX_SESSION_CLIENTS: usize = 64;

/// Cached session clients by (session id, token), with their last use.
type SessionClients = HashMap<(String, String), (u64, Arc<Client>)>;

/// Who a request acts as.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Identity {
    /// The user: every folder, every operation the daemon allows users.
    User,
    /// A process inside a session (from the identity headers): the daemon's
    /// agent policy and folder confinement apply.
    Session { id: String, token: String },
    /// The session that bound this agent conversation (from the agent
    /// headers), as the daemon resolves it; then as `Session`.
    Delegated { agent: String, conversation: String },
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Identity::User => f.write_str("User"),
            Identity::Session { id, .. } => write!(f, "Session({id})"),
            Identity::Delegated {
                agent,
                conversation,
            } => write!(f, "Delegated({agent}:{conversation})"),
        }
    }
}

/// Configuration shared by every daemon client the API opens.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub layout: Layout,
    /// Explicit `rakumux-ptyd`; otherwise `paths::find_ptyd_binary`.
    pub ptyd_path: Option<PathBuf>,
    /// Version announced in the hello.
    pub version: String,
}

/// Lazily connected clients, one per identity.
pub struct DaemonPool {
    config: DaemonConfig,
    user: Mutex<Option<Arc<Client>>>,
    sessions: Mutex<SessionClients>,
    /// Monotonic counter for least-recently-used eviction.
    uses: Mutex<u64>,
}

impl DaemonPool {
    pub fn new(config: DaemonConfig) -> Self {
        Self {
            config,
            user: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            uses: Mutex::new(0),
        }
    }

    /// The client for `identity`, connecting (and, for the user, spawning
    /// the daemon) when needed. Blocking.
    pub fn client(&self, identity: &Identity) -> Result<Arc<Client>, ApiError> {
        match identity {
            Identity::User => {
                let mut user = lock(&self.user);
                if let Some(client) = user.as_ref() {
                    return Ok(client.clone());
                }
                let client = Arc::new(Client::connect(self.client_config(AuthMode::User, true))?);
                *user = Some(client.clone());
                Ok(client)
            }
            Identity::Session { id, token } => {
                let key = (id.clone(), token.clone());
                let tick = self.tick();
                if let Some((used, client)) = lock(&self.sessions).get_mut(&key) {
                    *used = tick;
                    return Ok(client.clone());
                }
                let auth = AuthMode::Session {
                    id: SessionId::from_string(id.clone()),
                    token: token.clone(),
                };
                // Connect without holding the map: the hello may take a while.
                let client = Arc::new(Client::connect(self.client_config(auth, false))?);
                let mut sessions = lock(&self.sessions);
                if sessions.len() >= MAX_SESSION_CLIENTS {
                    if let Some(oldest) = sessions
                        .iter()
                        .min_by_key(|(_, (used, _))| *used)
                        .map(|(key, _)| key.clone())
                    {
                        sessions.remove(&oldest);
                    }
                }
                sessions.insert(key, (tick, client.clone()));
                Ok(client)
            }
            Identity::Delegated {
                agent,
                conversation,
            } => {
                // `details.agent_conversation` tells callers (the MCP server)
                // that no session could be found for the conversation.
                let user = self.client(&Identity::User)?;
                if !user.capabilities().iter().any(|c| c == "auth.delegated") {
                    let mut error = ApiError::new(
                        ErrorCode::Unsupported,
                        "this Rakumux daemon can't act for agent conversations yet; \
                         restart it (`rakumux daemon restart`)",
                    );
                    error.details = Some(serde_json::json!({"agent_conversation": "unsupported"}));
                    return Err(error);
                }
                let auth = AuthMode::Delegated {
                    agent: agent.clone(),
                    conversation: conversation.clone(),
                };
                match Client::connect(self.client_config(auth, false)) {
                    Ok(client) => Ok(Arc::new(client)),
                    Err(mut error) if error.code == ErrorCode::Unauthenticated => {
                        error.details = Some(serde_json::json!({"agent_conversation": "unbound"}));
                        Err(error)
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    /// Forget the client for `identity` after an error with `code` when
    /// reusing it would fail again (rejected identity, daemon gone or
    /// replaced).
    pub fn forget_after(&self, identity: &Identity, code: ErrorCode) {
        if !matches!(
            code,
            ErrorCode::Unauthenticated | ErrorCode::DaemonUnavailable | ErrorCode::ProtocolMismatch
        ) {
            return;
        }
        match identity {
            Identity::User => *lock(&self.user) = None,
            Identity::Session { id, token } => {
                lock(&self.sessions).remove(&(id.clone(), token.clone()));
            }
            Identity::Delegated { .. } => {}
        }
    }

    /// Whether a daemon answers, without spawning one. Blocking.
    pub fn health(&self) -> DaemonHealth {
        match Client::connect(self.client_config(AuthMode::User, false)) {
            Ok(client) => DaemonHealth {
                reachable: true,
                version: Some(client.daemon_version().to_owned()),
                capabilities: client.capabilities().to_vec(),
                error: None,
            },
            Err(error) => DaemonHealth {
                reachable: false,
                version: None,
                capabilities: Vec::new(),
                error: Some(error.to_string()),
            },
        }
    }

    fn client_config(&self, auth: AuthMode, spawn: bool) -> ClientConfig {
        ClientConfig {
            layout: self.config.layout.clone(),
            client_name: "api".into(),
            version: self.config.version.clone(),
            auth,
            spawn,
            ptyd_path: self.config.ptyd_path.clone(),
        }
    }

    fn tick(&self) -> u64 {
        let mut uses = lock(&self.uses);
        *uses += 1;
        *uses
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(home: &std::path::Path) -> DaemonPool {
        DaemonPool::new(DaemonConfig {
            layout: Layout::with_home(home.to_path_buf()),
            ptyd_path: Some(home.join("no-such-ptyd")),
            version: "test".into(),
        })
    }

    #[test]
    fn health_reports_a_missing_daemon_without_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let health = pool(dir.path()).health();
        assert!(!health.reachable);
        assert!(health.error.unwrap().contains("not running"));
    }

    #[test]
    fn session_clients_never_spawn_and_fail_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let error = pool(dir.path())
            .client(&Identity::Session {
                id: "01J".into(),
                token: "t".into(),
            })
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::DaemonUnavailable);
    }

    #[test]
    fn user_client_reports_a_missing_daemon_binary() {
        let dir = tempfile::tempdir().unwrap();
        let error = pool(dir.path()).client(&Identity::User).err().unwrap();
        assert_eq!(error.code, ErrorCode::DaemonUnavailable);
        assert!(error.message.contains("not found"), "{}", error.message);
    }

    #[test]
    fn identity_debug_hides_the_token() {
        let identity = Identity::Session {
            id: "01J".into(),
            token: "secret".into(),
        };
        assert_eq!(format!("{identity:?}"), "Session(01J)");
    }
}
