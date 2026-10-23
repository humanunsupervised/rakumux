//! Ordered SessionApi observations and scoped capture for AI status. No startup/IPC/TUI wiring.
//!
//! The dispatch coordinator's tracker turns daemon views and events into source stamps
//! for the status policy; capture reads only the bounded plain-text windows a dispatch
//! job permits. Neither opens the daemon registry or a PTY, schedules work or makes
//! network requests.

mod capture;
#[cfg(test)]
pub(crate) mod fake;
mod tracker;

pub use capture::{CaptureError, CaptureExpectation, SessionCapture, OUTPUT_LINES};
pub(crate) use tracker::{Candidate, SourceTracker, SourceUpdate};
pub use tracker::{SourceError, TurnSettings};
