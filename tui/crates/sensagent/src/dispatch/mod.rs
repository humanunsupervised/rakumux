//! In-process, explicitly driven status dispatch coordinator. No startup/IPC/TUI wiring.
//!
//! Trusted local integration must authorize user-only controls and attest scoped
//! source/capture data. This module is not a running per-home service or an IPC
//! permission boundary. Blocking worker I/O runs separately from its control state.

mod job;
mod service;

pub use job::{
    DispatchCompletion, DispatchError, DispatchPreparation, PreparedStatusDispatch,
    StatusCaptureJob, StatusConsentOffer, StatusDispatchJob,
};
pub use service::StatusDispatchService;
