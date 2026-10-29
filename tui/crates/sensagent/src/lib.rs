//! Local-first Sensagent services, independent of terminal ownership and UI.
//!
//! Explicit account authentication, opt-in status policy, native cloud adapters,
//! in-process dispatch, SessionApi source tracking/capture, scheduled refreshing and
//! a per-home owner with a local socket protocol. Nothing is installed or wired into
//! startup, session permissions or the TUI.

pub mod auth;
pub mod cloud;
pub mod dispatch;
pub mod enrolment;
pub mod feed;
pub mod owner;
pub mod refresh;
pub mod service;
pub mod source;
pub mod status;

/// Whether this machine's account service keeps a session to itself: a
/// private one, or one that runs on another machine (`SessionView::machine`),
/// which that machine's own service shares as its own.
pub(crate) fn shared_elsewhere(view: &session_core::model::SessionView) -> bool {
    view.private || view.machine.is_some()
}
