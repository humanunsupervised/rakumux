//! The status owner's local protocol: `session_core::frame` control frames (JSON),
//! a hello with the registration token, then alternating requests and responses.
//!
//! Compatibility follows the daemon's rules: new fields are optional, response enums
//! carry an `Unknown` fallback, an op never changes meaning, and `PROTOCOL` changes
//! only for incompatible changes.

use std::fmt;
use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;

use crate::dispatch::DispatchError;
use crate::refresh::{AIStatus, RefreshError, RefreshFailure, SignInFailure, SignInState};
use crate::status::{
    ConsentPolicy, ConsentSnapshot, FolderConsent, StatusError, StatusStoreError, StatusView,
    MAX_CACHE_ENTRIES,
};

/// Incompatible protocol changes bump this.
pub const PROTOCOL: u32 = 1;
/// Any one message, either way. Statuses for every tracked session fit easily.
pub const MAX_MESSAGE: usize = 256 * 1024;
/// Sessions per `statuses` request: the status policy's own bound.
pub const MAX_SESSIONS: usize = MAX_CACHE_ENTRIES;
/// Client names are labels, not identity.
pub const MAX_CLIENT_NAME: usize = 64;

const KIND_CONTROL: u8 = 0;

/// The first message a client sends.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// "tui", "cli", …; a label only.
    pub client: String,
    /// The registration's user token.
    pub token: String,
}

impl fmt::Debug for Hello {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Hello")
            .field("protocol", &self.protocol)
            .field("client", &self.client)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// What a user client may ask. Session callers have no access to this service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OwnerRequest {
    /// Account, consent and refreshing as a whole.
    State,
    /// Status lines for these sessions, in order.
    Statuses { sessions: Vec<SessionId> },
    /// The exact account, consent revision and destination to confirm; the returned
    /// id is used once by `enable`.
    ConsentOffer,
    /// Confirm the user's explicit choice for an offer.
    Enable {
        offer: String,
        folders: Vec<FolderChoice>,
    },
    /// Stop sharing, if consent is still at `revision`.
    Disable { revision: u64 },
    /// Stop sharing and remove local credentials.
    SignOut,
    /// Start a device sign-in, replacing any unfinished one; answered with its state.
    SignInStart,
    /// Where the current sign-in is up to.
    SignInStatus,
    /// Abandon an unfinished sign-in; an existing account stays signed in.
    SignInCancel,
    /// Renew the account's access token now rather than when it's due.
    Renew,
    /// Stop serving (a newer client replaces an older service); answered first.
    Shutdown,
}

/// The owner's answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OwnerResponse {
    Welcome {
        protocol: u32,
        version: String,
    },
    State {
        state: OwnerState,
    },
    Statuses {
        statuses: Vec<StatusLine>,
    },
    Offer {
        offer: OfferView,
    },
    Consent {
        consent: ConsentView,
    },
    SignIn {
        sign_in: SignInView,
    },
    Done,
    Error {
        code: OwnerErrorCode,
    },
    #[serde(other)]
    Unknown,
}

/// Account, consent and refreshing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerState {
    /// The signed-in account, if one was adopted.
    #[serde(default)]
    pub account: Option<AccountView>,
    pub consent: ConsentView,
    /// The daemon's sessions are being followed.
    #[serde(default)]
    pub connected: bool,
    /// The service asked for fewer requests.
    #[serde(default)]
    pub rate_limited: bool,
    /// The account was refused; sign in or confirm consent again.
    #[serde(default)]
    pub unauthorized: bool,
    /// The account's AI session summaries switch (on the website), once read;
    /// absent when signed out or not yet known.
    #[serde(default)]
    pub summaries: Option<bool>,
}

/// A device sign-in as the TUI shows it. Holds no device code or token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SignInView {
    Idle,
    Starting,
    /// Open one of the links in any browser and confirm `user_code`.
    Waiting {
        user_code: String,
        verification_uri: String,
        #[serde(default)]
        verification_uri_complete: Option<String>,
        /// rakumux.com's sign-in-then-approve page, when configured.
        #[serde(default)]
        connect_url: Option<String>,
        expires_in_seconds: u64,
    },
    SignedIn,
    Failed {
        reason: SignInReason,
    },
    #[serde(other)]
    Unknown,
}

/// Why a sign-in ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInReason {
    Denied,
    Expired,
    Unavailable,
    Canceled,
    Failed,
    #[serde(other)]
    Unknown,
}

impl From<&SignInState> for SignInView {
    fn from(state: &SignInState) -> Self {
        match state {
            SignInState::Idle => Self::Idle,
            SignInState::Starting => Self::Starting,
            SignInState::Waiting(prompt) => Self::Waiting {
                user_code: prompt.user_code.clone(),
                verification_uri: prompt.verification_uri.clone(),
                verification_uri_complete: prompt.verification_uri_complete.clone(),
                connect_url: prompt.connect_url.clone(),
                expires_in_seconds: prompt.expires_in_seconds,
            },
            SignInState::SignedIn => Self::SignedIn,
            SignInState::Failed(failure) => Self::Failed {
                reason: match failure {
                    SignInFailure::Denied => SignInReason::Denied,
                    SignInFailure::Expired => SignInReason::Expired,
                    SignInFailure::Unavailable => SignInReason::Unavailable,
                    SignInFailure::Canceled => SignInReason::Canceled,
                    SignInFailure::Failed => SignInReason::Failed,
                },
            },
        }
    }
}

/// An account as a trusted control shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountView {
    /// The provider's stable subject.
    pub subject: String,
    #[serde(default)]
    pub email: Option<String>,
    /// When the current access token expires, Unix seconds; it is renewed before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

/// The durable sharing decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentView {
    pub revision: u64,
    pub enabled: bool,
    /// Selected folders, each reports-only or with terminal output.
    #[serde(default)]
    pub folders: Vec<FolderChoice>,
}

/// One selected folder on the wire. Unlike the stored [`FolderConsent`], it accepts
/// fields a newer peer adds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderChoice {
    /// A canonical folder, as the daemon names it.
    pub folder: ScopeRoot,
    /// False shares the session's reports only, never terminal output.
    pub include_terminal_output: bool,
}

impl From<&FolderConsent> for FolderChoice {
    fn from(consent: &FolderConsent) -> Self {
        Self {
            folder: consent.folder.clone(),
            include_terminal_output: consent.include_terminal_output,
        }
    }
}

impl From<FolderChoice> for FolderConsent {
    fn from(choice: FolderChoice) -> Self {
        Self {
            folder: choice.folder,
            include_terminal_output: choice.include_terminal_output,
        }
    }
}

impl From<&ConsentSnapshot> for ConsentView {
    fn from(snapshot: &ConsentSnapshot) -> Self {
        let (enabled, folders) = match &snapshot.policy {
            ConsentPolicy::Disabled => (false, Vec::new()),
            ConsentPolicy::Enabled { folders, .. } => {
                (true, folders.iter().map(FolderChoice::from).collect())
            }
        };
        Self {
            revision: snapshot.revision.0,
            enabled,
            folders,
        }
    }
}

/// What enabling would bind: the user must see all of it before confirming.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferView {
    /// One-use handle for `enable`.
    pub id: String,
    /// The fixed AI service origin requests go to.
    pub origin: String,
    pub account: AccountView,
    pub consent_revision: u64,
}

/// One session's AI status line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusLine {
    pub session: SessionId,
    pub state: LineState,
    /// Advisory AI text, only when `state` is `ready`.
    #[serde(default)]
    pub text: Option<String>,
    /// When that text's input was captured, Unix seconds.
    #[serde(default)]
    pub captured_at: Option<u64>,
    /// Why the last refresh produced no text.
    #[serde(default)]
    pub failure: Option<FailureCode>,
}

impl StatusLine {
    pub(super) fn new(session: SessionId, status: AIStatus) -> Self {
        let (text, captured_at) = match &status.view {
            StatusView::Ready(entry) => (
                Some(entry.text.as_str().to_owned()),
                Some(entry.captured_at),
            ),
            _ => (None, None),
        };
        Self {
            session,
            state: LineState::from(&status.view),
            text,
            captured_at,
            failure: status.failure.map(FailureCode::from),
        }
    }
}

/// The status policy's view, without its data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineState {
    Disabled,
    SignedOut,
    ConsentRequired,
    Excluded,
    Private,
    NotLive,
    NoContext,
    Pending,
    Missing,
    Stale,
    Ready,
    #[serde(other)]
    Unknown,
}

impl From<&StatusView> for LineState {
    fn from(view: &StatusView) -> Self {
        match view {
            StatusView::Disabled => Self::Disabled,
            StatusView::SignedOut => Self::SignedOut,
            StatusView::ConsentRequired => Self::ConsentRequired,
            StatusView::Excluded => Self::Excluded,
            StatusView::Private => Self::Private,
            StatusView::NotLive => Self::NotLive,
            StatusView::NoContext => Self::NoContext,
            StatusView::Pending => Self::Pending,
            StatusView::Missing => Self::Missing,
            StatusView::Stale => Self::Stale,
            StatusView::Ready(_) => Self::Ready,
        }
    }
}

/// [`RefreshFailure`] on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    Unreadable,
    Rejected,
    Unavailable,
    RateLimited,
    Unauthorized,
    #[serde(other)]
    Unknown,
}

impl From<RefreshFailure> for FailureCode {
    fn from(failure: RefreshFailure) -> Self {
        match failure {
            RefreshFailure::Unreadable => Self::Unreadable,
            RefreshFailure::Rejected => Self::Rejected,
            RefreshFailure::Unavailable => Self::Unavailable,
            RefreshFailure::RateLimited => Self::RateLimited,
            RefreshFailure::Unauthorized => Self::Unauthorized,
        }
    }
}

/// Static refusals; never account, folder, session or service text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerErrorCode {
    /// Missing or wrong token, or the first message wasn't a hello.
    Unauthenticated,
    /// The client speaks another protocol version.
    ProtocolMismatch,
    /// Malformed, oversized or unsupported request, or invalid folders.
    InvalidRequest,
    /// The offer was used, replaced or never issued, or the account or consent
    /// changed since: ask for a new offer.
    StaleOffer,
    /// Consent changed since the revision the request named: read the state again.
    Conflict,
    /// Sign in first.
    SignedOut,
    /// Local storage or policy checks failed; sharing stopped.
    Unavailable,
    /// The owner is shutting down.
    Stopped,
    /// Too many clients are connected.
    Busy,
    #[serde(other)]
    Unknown,
}

impl From<&RefreshError> for OwnerErrorCode {
    fn from(error: &RefreshError) -> Self {
        match error {
            RefreshError::Dispatch(error) => Self::from(error),
            RefreshError::Stopped | RefreshError::Thread => Self::Stopped,
            RefreshError::NotPermitted => Self::Unavailable,
        }
    }
}

impl From<&DispatchError> for OwnerErrorCode {
    fn from(error: &DispatchError) -> Self {
        match error {
            DispatchError::Policy(StatusError::SignedOut) => Self::SignedOut,
            DispatchError::Policy(StatusError::Store(StatusStoreError::Conflict)) => Self::Conflict,
            DispatchError::Policy(StatusError::AccountChanged) | DispatchError::ForeignOwner => {
                Self::StaleOffer
            }
            _ => Self::Unavailable,
        }
    }
}

impl OwnerErrorCode {
    /// For `enable`, which acts on an offer: a consent conflict means the offer is
    /// out of date, and invalid data can only be the request's folders.
    pub(super) fn of_enable(error: &RefreshError) -> Self {
        match error {
            RefreshError::Dispatch(DispatchError::Policy(StatusError::Store(
                StatusStoreError::Conflict,
            ))) => Self::StaleOffer,
            RefreshError::Dispatch(DispatchError::Policy(StatusError::Store(
                StatusStoreError::InvalidData,
            ))) => Self::InvalidRequest,
            _ => Self::from(error),
        }
    }
}

/// Write one control frame.
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec(value).map_err(io::Error::other)?;
    if json.len() >= MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "message too large",
        ));
    }
    session_core::frame::write_control(writer, value)
}

/// Read one control frame of at most `MAX_MESSAGE` bytes and decode it.
/// `Ok(None)` on a clean end of stream.
pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<Option<T>> {
    let Some(payload) = read_payload(reader, MAX_MESSAGE)? else {
        return Ok(None);
    };
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "malformed message"))
}

/// Read one control frame's JSON payload of at most `max` bytes, checking the length
/// before reading the body. A malformed payload still leaves the stream at the next
/// frame.
pub(super) fn read_payload<R: Read>(reader: &mut R, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(header) as usize;
    if !(2..=max.min(MAX_MESSAGE)).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid message length",
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    if body[0] != KIND_CONTROL {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected frame kind",
        ));
    }
    body.remove(0);
    Ok(Some(body))
}
