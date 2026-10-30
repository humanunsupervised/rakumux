//! Cursor's agent CLI through the real daemon, with a stand-in for Cursor:
//! see `integrations/cursor-agent/e2e.rs`.
#![cfg(target_os = "linux")]

mod runtime_support;

#[path = "../../../../integrations/cursor-agent/e2e.rs"]
mod e2e;
