//! `rakumux-mcp`: an MCP server (stdio) that lets coding agents see and
//! drive Rakumux sessions and their folders through the external API
//! (`rakumux-api`, started on demand).
//!
//! - `server`: the MCP protocol adapter (the only module using `rmcp`).
//! - `tools`: the tool catalogue and what each tool does.
//! - `client`: the API client (discovery via `api.json`, identity headers).
//! - `launch`: finding and starting `rakumux-api --detach`.
//! - `render`: structured and text projections of API results.
//! - `untrusted`: output hygiene for terminal and session-written text.
//! - `instructions`: the server instructions sent at `initialize`.
//! - `opencode`: the plugin for OpenCode's TUI that links conversations to
//!   sessions, and its installer.
//! - `pi`: the extension for Pi that reports its activity and conversation to
//!   the session, and its installer.
//! - `kimi`: the plugin for Kimi Code (its hooks and this server), and its
//!   installer.
//! - `amp`: the plugin for Amp that links threads to sessions, reports their
//!   state and keeps their copy for the daemon, and its installer.

#[path = "../../integrations/amp/install.rs"]
pub mod amp;
pub mod client;
pub mod instructions;
#[path = "../../integrations/mcp.rs"]
pub mod integrations;
#[path = "../../integrations/kimi/install.rs"]
pub mod kimi;
pub mod launch;
#[path = "../../integrations/open-code/install.rs"]
pub mod opencode;
#[path = "../../integrations/pi/install.rs"]
pub mod pi;
pub mod render;
pub mod server;
pub mod tools;
pub mod untrusted;

/// This build's version (release builds set `RAKUMUX_VERSION`).
pub const VERSION: &str = match option_env!("RAKUMUX_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
