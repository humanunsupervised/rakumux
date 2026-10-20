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

pub mod client;
pub mod instructions;
#[path = "../../Integrations/mcp.rs"]
pub mod integrations;
pub mod launch;
#[path = "../../Integrations/Open Code/install.rs"]
pub mod opencode;
pub mod render;
pub mod server;
pub mod tools;
pub mod untrusted;

/// This build's version (release builds set `RAKUMUX_VERSION`).
pub const VERSION: &str = match option_env!("RAKUMUX_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
