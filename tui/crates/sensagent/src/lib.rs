//! Local-first Sensagent services, independent of terminal ownership and UI.
//!
//! Explicit account authentication, opt-in status policy, native cloud adapters
//! and in-process dispatch. Nothing is wired into startup, session permissions
//! or the TUI; running shared ownership and scoped capture remain separate gates.

pub mod auth;
pub mod cloud;
pub mod dispatch;
pub mod status;
