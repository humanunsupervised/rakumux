//! Orchestra's external API: a small HTTP/1.1 + JSON service on 127.0.0.1
//! that exposes a subset of session operations of the running daemon
//! (`orchestra-ptyd`), with WebSocket streams for a terminal and for session
//! events. It is a thin adapter over `session_core::client`: the daemon
//! still decides everything (scope confinement, permissions, audit). With
//! remote access on (`remote.json`) it also serves a remote listener, for
//! the user's phone over a tailnet.
//!
//! - `discovery`: `api.json`, how clients find the running API.
//! - `instance`: one API per Orchestra home (a `flock` on `api.lock`).
//! - `server`: bind, write `api.json`, serve until shutdown.
//! - `remote`: remote access: which addresses the remote listener binds.
//! - `routes`, `guard`, `query`: HTTP routing, request checks and parsing.
//! - `streams`: the WebSocket routes (terminal attach, session events).
//! - `sessions`, `folders`: the use cases, programmed against `SessionApi`.
//! - `daemon`: daemon clients per caller identity (user or session).
//! - `dto`: the JSON shapes of `openapi.yaml`, shared with `orchestra-mcp`.
//! - `error`: the error envelope and the status mapping.
//! - `detach`: `--detach`, start in the background and wait until healthy.
//! - `probe`: a minimal blocking `GET /v1/health`.

pub mod daemon;
pub mod detach;
pub mod discovery;
pub mod dto;
pub mod error;
pub mod folders;
pub mod guard;
pub mod instance;
pub mod log;
pub mod probe;
pub mod query;
pub mod remote;
pub mod routes;
pub mod server;
pub mod sessions;
pub mod streams;
pub mod time;

/// This build's version. Release builds set `ORCHESTRA_VERSION` at compile
/// time, like the daemon and the `orchestra` binary.
pub const VERSION: &str = match option_env!("ORCHESTRA_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Environment variables specific to the API and its clients.
pub mod env {
    /// Explicit path to the `orchestra-api` binary (used by `orchestra-mcp`).
    pub const API_BIN: &str = "ORCHESTRA_API_BIN";
    /// `debug` logs every request; anything else logs errors and lifecycle.
    pub const API_LOG: &str = "ORCHESTRA_API_LOG";
}

/// Identity headers: with both session headers present, the API talks to the
/// daemon as that session, so the daemon's agent policy and folder
/// confinement apply. With both agent headers instead, it acts for the
/// session that bound that agent conversation (`Auth::Delegated`).
pub mod headers {
    pub const SESSION_ID: &str = "x-orchestra-session-id";
    pub const SESSION_TOKEN: &str = "x-orchestra-session-token";
    pub const AGENT: &str = "x-orchestra-agent";
    pub const AGENT_CONVERSATION: &str = "x-orchestra-agent-conversation";
}
