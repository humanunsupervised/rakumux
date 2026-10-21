//! Explicit native client for the hosted status API. No automatic sharing or TUI wiring.
//! These low-level adapters are not an IPC/consent authorization boundary.

mod client;
mod context;
mod error;
mod wire;

pub use client::{Cancellation, CloudAIClient, CloudAIConfig};
pub use context::{CaptureInput, CaptureMetadata, ContextBuilder};
pub use error::CloudAIError;
pub use wire::{RemoteErrorCode, StatusResponse, ValidatedStatusRequest};
