//! Shared contract between the Rakumux daemon (`rakumux-ptyd`) and its
//! clients (TUI, CLI, later MCP): the session model, the wire protocol, the
//! filesystem layout and a blocking client.
//!
//! Architecture: exactly one daemon per user owns every session and the
//! session registry. Any number of clients (several TUIs, CLI calls from
//! inside sessions) connect to it over a local Unix socket.
//!
//! `remote` holds the remote access settings (`remote.json`) shared by
//! `rakumux-api` and the `rakumux remote` CLI; `tailscale` finds this
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

/// Environment variables Rakumux injects into every session.
pub mod env {
    /// The session the process runs in.
    pub const SESSION_ID: &str = "RAKUMUX_SESSION_ID";
    /// Secret that authenticates calls made from inside the session.
    pub const SESSION_TOKEN: &str = "RAKUMUX_SESSION_TOKEN";
    /// Daemon socket path, so tools inside the session can find it.
    pub const SOCKET: &str = "RAKUMUX_SOCKET";
    /// Path of the `rakumux` launcher, for agents and hooks.
    pub const BIN: &str = "RAKUMUX_BIN";
    /// Overrides `~/.rakumux` (tests, isolated setups).
    pub const HOME: &str = "RAKUMUX_HOME";
    /// Explicit path to the `rakumux-ptyd` binary.
    pub const PTYD_BIN: &str = "RAKUMUX_PTYD_BIN";
}
