//! `rakumux-ptyd`: the single per-user Rakumux server. It owns every
//! session's terminal (PTY + process + emulator) and the session registry,
//! and serves any number of clients (TUIs, CLI calls, agents) over a local
//! Unix socket.
//!
//! Layers:
//! - `server`: socket accept loop, framing, hello, attach/event streams.
//! - `control`: sessions, registry, identity, permissions, waits, audit
//!   (implements `control_api::Control`).
//! - `terminal`: PTYs, processes, emulators, attach fan-out
//!   (implements `host::TerminalHost`).
//! - `handoff`: restarting in place (`daemon_restart`) with running
//!   sessions carried over.
//! - `resume`: resume commands for known agents (Claude Code), so revived
//!   sessions come back in the same conversation.
//! - `persist`, `procinfo`, `detect`, `git`: support modules.

pub mod control;
pub mod control_api;
pub mod detect;
pub mod git;
pub mod handoff;
pub mod host;
pub mod log;
pub mod persist;
pub mod procinfo;
pub mod registration;
pub mod resume;
pub mod server;
pub mod terminal;
