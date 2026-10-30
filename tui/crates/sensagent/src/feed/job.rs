use std::fmt;
use std::sync::Arc;

use crate::auth::{
    AccessTokenLease, AccountService, CredentialRevision, CredentialStore, SystemClock, WallClock,
};
use crate::cloud::{Cancellation, CloudAIClient, CloudAIError, FeedHttp};

use super::cache::FeedReply;
use super::wire::{self, Download, FeedFailure, FeedRequest, PageRead, RepliesRead, TurnRead};

/// One feed request, run off the owner thread. It borrows the bearer only for
/// the one request, like a features read.
pub struct FeedJob<A, C = SystemClock> {
    pub(crate) account: Arc<AccountService<A, C>>,
    pub(crate) cloud: Arc<CloudAIClient>,
    pub(crate) lease: AccessTokenLease,
    pub(crate) revision: CredentialRevision,
    pub(crate) request: FeedRequest,
}

/// What a feed request brought back.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FeedResult {
    /// The seq of each uploaded id.
    Accepted(Vec<(String, u64)>),
    /// A page read.
    Page(PageRead),
    /// A chat's messages were stored.
    Stored,
    /// A turn read.
    Turn(TurnRead),
    /// A reply as stored (after an upload or a status change).
    Reply(FeedReply),
    /// A session message as stored (after a status change).
    Message(wire::SessionMessage),
    /// The account's changed replies.
    Replies(RepliesRead),
    /// This machine's session list was stored: how many sessions it holds,
    /// and what more the cloud takes.
    SessionsStored(wire::SessionsStored),
    /// What this machine uses was stored; whether rakumux.com still wants it.
    ResourcesStored(bool),
    /// Where to connect to be poked.
    Realtime(wire::RealtimeToken),
    /// A file attached to a reply, or that it's gone.
    Download(Download),
    /// The enrolled machine's secret, given once.
    Enrolled(String),
    /// The enrolled machine's credential is gone.
    Revoked,
}

/// A finished feed request, tagged with the account revision it was made for.
pub struct FeedCompletion {
    pub(crate) revision: CredentialRevision,
    pub(crate) outcome: Result<FeedResult, FeedFailure>,
}

impl<A: CredentialStore, C: WallClock> FeedJob<A, C> {
    /// Make at most one request.
    pub fn run(self) -> FeedCompletion {
        let cancel = Cancellation::new();
        let query = match &self.request {
            FeedRequest::Read { after } => FeedRequest::query(*after),
            FeedRequest::Turn { entry, after } => FeedRequest::turn_query(entry, *after),
            FeedRequest::Replies { after, machine } => FeedRequest::replies_query(*after, machine),
            FeedRequest::Upload { .. }
            | FeedRequest::Messages { .. }
            | FeedRequest::ReplyUpload { .. }
            | FeedRequest::ReplyStatus { .. }
            | FeedRequest::Sessions { .. }
            | FeedRequest::Resources { .. }
            | FeedRequest::Attachment { .. }
            | FeedRequest::Realtime
            | FeedRequest::MachineEnrol { .. }
            | FeedRequest::MachineRevoke { .. } => String::new(),
        };
        let http = match &self.request {
            FeedRequest::Upload { body, .. } => FeedHttp::Upload(body),
            FeedRequest::Read { .. } => FeedHttp::Page(&query),
            FeedRequest::Messages { body } => FeedHttp::Messages(body),
            FeedRequest::Turn { .. } => FeedHttp::Turn(&query),
            FeedRequest::ReplyUpload { body } => FeedHttp::ReplyUpload(body),
            FeedRequest::Replies { .. } => FeedHttp::Replies(&query),
            FeedRequest::ReplyStatus { body } => FeedHttp::ReplyStatus(body),
            FeedRequest::Sessions { body } => FeedHttp::Sessions(body),
            FeedRequest::Resources { body } => FeedHttp::Resources(body),
            FeedRequest::Attachment { id, size } => FeedHttp::Attachment { id, size: *size },
            FeedRequest::Realtime => FeedHttp::Realtime,
            FeedRequest::MachineEnrol { body } => FeedHttp::MachineEnrol(body),
            FeedRequest::MachineRevoke { machine } => FeedHttp::MachineRevoke(machine),
        };
        let outcome = self
            .account
            .consume_access_token(self.lease, |token| self.cloud.feed(token, http, &cancel))
            .map_err(|_| FeedFailure::Unauthorized)
            .and_then(|result| match result {
                Some(Ok(reply)) => match &self.request {
                    FeedRequest::Upload { .. } => wire::accepted(&reply).map(FeedResult::Accepted),
                    FeedRequest::Read { .. } => wire::page(&reply).map(FeedResult::Page),
                    FeedRequest::Messages { .. } => {
                        wire::stored(&reply).map(|_| FeedResult::Stored)
                    }
                    FeedRequest::Turn { .. } => wire::turn(&reply).map(FeedResult::Turn),
                    FeedRequest::ReplyUpload { .. } | FeedRequest::ReplyStatus { .. } => {
                        wire::reply(&reply).map(|received| match received {
                            wire::Received::Reply(reply) => FeedResult::Reply(reply),
                            wire::Received::Message(message) => FeedResult::Message(message),
                        })
                    }
                    FeedRequest::Replies { .. } => wire::replies(&reply).map(FeedResult::Replies),
                    FeedRequest::Sessions { .. } => {
                        wire::sessions_stored(&reply).map(FeedResult::SessionsStored)
                    }
                    FeedRequest::Resources { .. } => {
                        wire::resources_stored(&reply).map(FeedResult::ResourcesStored)
                    }
                    FeedRequest::Attachment { size, .. } => {
                        wire::download(reply, *size).map(FeedResult::Download)
                    }
                    FeedRequest::Realtime => wire::realtime(&reply).map(FeedResult::Realtime),
                    FeedRequest::MachineEnrol { .. } => {
                        crate::enrolment::enrolled(&reply).map(FeedResult::Enrolled)
                    }
                    FeedRequest::MachineRevoke { .. } => {
                        crate::enrolment::revoked(&reply).map(|()| FeedResult::Revoked)
                    }
                },
                Some(Err(error)) => Err(transport(error)),
                // The sign-in changed meanwhile; the owner fences this completion.
                None => Err(FeedFailure::Unavailable(None)),
            });
        FeedCompletion {
            revision: self.revision,
            outcome,
        }
    }
}

fn transport(error: CloudAIError) -> FeedFailure {
    match error {
        CloudAIError::Network(_) | CloudAIError::HttpStatus(_) | CloudAIError::Canceled => {
            FeedFailure::Unavailable(None)
        }
        _ => FeedFailure::Invalid,
    }
}

impl<A, C> fmt::Debug for FeedJob<A, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FeedJob { bindings: [REDACTED] }")
    }
}

impl fmt::Debug for FeedCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeedCompletion")
            .field("ok", &self.outcome.is_ok())
            .finish()
    }
}
