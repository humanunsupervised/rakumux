//! Local-first Sensagent services, independent of terminal ownership and UI.
//!
//! Explicit account authentication, opt-in status policy, native cloud adapters,
//! in-process dispatch, SessionApi source tracking/capture, scheduled refreshing and
//! a per-home owner with a local socket protocol. Nothing is installed or wired into
//! startup, session permissions or the TUI.

pub mod auth;
pub mod cloud;
pub mod dispatch;
pub mod owner;
pub mod refresh;
pub mod service;
pub mod source;
pub mod status;
