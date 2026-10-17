//! Shared contract between the Orchestra daemon (`orchestra-ptyd`) and its
//! clients (TUI, CLI, later MCP): the session model, the wire protocol, the
//! filesystem layout and a blocking client.
//!
//! Architecture: exactly one daemon per user owns every session and the
//! session registry. Any number of clients (several TUIs, CLI calls from
//! inside sessions) connect to it over a local Unix socket.
//!
//! `remote` holds the remote access settings (`remote.json`) shared by
//! `orchestra-api` and the `orchestra remote` CLI; `tailscale` finds this
//! machine's Tailscale addresses and name.

pub mod api;
pub mod client;
pub mod frame;
pub mod ids;
pub mod model;
pub mod paths;
pub mod remote;
pub mod tailscale;

/// Wire protocol version. Bump only for incompatible changes; additive
/// changes keep the version and add optional fields or new ops.
pub const PROTOCOL_VERSION: u32 = 1;

/// Environment variables Orchestra injects into every session.
pub mod env {
    /// The session the process runs in.
    pub const SESSION_ID: &str = "ORCHESTRA_SESSION_ID";
    /// Secret that authenticates calls made from inside the session.
    pub const SESSION_TOKEN: &str = "ORCHESTRA_SESSION_TOKEN";
    /// Daemon socket path, so tools inside the session can find it.
    pub const SOCKET: &str = "ORCHESTRA_SOCKET";
    /// Path of the `orchestra` launcher, for agents and hooks.
    pub const BIN: &str = "ORCHESTRA_BIN";
    /// Overrides `~/.orchestra` (tests, isolated setups).
    pub const HOME: &str = "ORCHESTRA_HOME";
    /// Explicit path to the `orchestra-ptyd` binary.
    pub const PTYD_BIN: &str = "ORCHESTRA_PTYD_BIN";
}
