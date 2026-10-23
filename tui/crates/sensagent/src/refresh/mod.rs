//! Scheduled AI-status refreshing over one dispatch coordinator. No startup/IPC/TUI wiring.
//!
//! The scheduler decides when each session is refreshed; the refresher applies daemon
//! views and events, starts captures and applies requests; the runner gives it an
//! owner thread, a daemon subscription and two workers. Nothing here chooses a
//! Rakumux home, opens a store, holds a per-home lock or accepts connections.

mod engine;
mod renewal;
mod runner;
mod schedule;
mod sign_in;

pub use engine::{
    AIStatus, CaptureWork, Captured, Completed, Dispatch, RefreshState, StatusRefresher,
};
pub use runner::{RefreshClient, RefreshError, RefreshRunner};
pub use schedule::{
    RefreshFailure, BUDGET_STARTS, BUDGET_WINDOW, ERROR_BACKOFF, MAX_ERROR_BACKOFF, MAX_RUNNING,
    QUIET_PERIOD, SESSION_INTERVAL,
};
pub use sign_in::{SignInFailure, SignInPrompt, SignInState};
