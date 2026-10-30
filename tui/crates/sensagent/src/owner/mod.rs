//! One AI-status owner per Rakumux home: its file layout, single-instance claim and
//! local socket protocol, server and client. No process is installed or spawned.
//!
//! The owner runs a [refresher](crate::refresh) and serves user clients (the TUI)
//! over a private Unix socket. Starting it, and choosing its provider and service
//! configuration, belong to the embedding process; see the developer example.

mod claim;
mod client;
mod layout;
pub mod protocol;
mod server;

use thiserror::Error;

pub use claim::{OwnerClaim, OwnerRegistration};
pub use client::OwnerClient;
pub use layout::OwnerLayout;
pub use protocol::{EnrolledCredential, OwnerErrorCode};
pub use server::{OwnerServer, SignOutRevoker};

/// Static owner failures; never paths, tokens or account text.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum OwnerError {
    /// A directory, lock or file in the layout is unsafe or unavailable.
    #[error("the AI-status owner's files are unsafe or unavailable")]
    Layout,
    /// Another owner holds this home's lock.
    #[error("another AI-status owner is running for this Rakumux home")]
    AlreadyRunning,
    /// The socket could not be bound or served.
    #[error("unable to serve the AI-status owner socket")]
    Socket,
    /// The registration is missing a field, unsafe or for another socket.
    #[error("the AI-status owner registration is unreadable or unsafe")]
    Registration,
    /// No owner is running for this home.
    #[error("no AI-status owner is running")]
    NotRunning,
    /// The owner speaks another protocol version.
    #[error("the AI-status owner speaks another protocol version")]
    ProtocolMismatch,
    /// Connecting or talking to the owner failed.
    #[error("the connection to the AI-status owner failed")]
    Connection,
    /// The owner sent something unexpected.
    #[error("unexpected answer from the AI-status owner")]
    Protocol,
    /// More sessions than one request may name.
    #[error("too many sessions in one AI-status request")]
    TooManySessions,
    /// The owner refused the request.
    #[error("the AI-status owner refused the request: {0:?}")]
    Refused(OwnerErrorCode),
}

#[cfg(test)]
mod tests;
