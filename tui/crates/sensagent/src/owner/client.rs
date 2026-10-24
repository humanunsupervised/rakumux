use std::fmt;
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::Duration;

use session_core::ids::SessionId;

use crate::feed::{FeedPage, FeedTurn, ReplyWrite};

use super::layout::{current_uid, peer_uid};
use super::protocol::{
    read_message, write_message, ConsentView, FolderChoice, Hello, OfferView, OwnerRequest,
    OwnerResponse, OwnerState, SignInView, StatusLine, MAX_SESSIONS, PROTOCOL,
};
use super::{OwnerError, OwnerLayout, OwnerRegistration};

/// How long the owner may take to answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A user client of a running status owner, found through its registration. One
/// connection; requests are answered in order. After any failed request the
/// connection is closed, since a late answer could otherwise be read as the next
/// one's: every later call fails with `Connection`, and the caller reconnects.
/// Never starts an owner.
pub struct OwnerClient {
    stream: Mutex<Option<UnixStream>>,
    version: String,
}

impl OwnerClient {
    /// Connect to this home's owner and complete the hello. The registration must be
    /// this user's private file and name this layout's socket.
    pub fn connect(layout: &OwnerLayout, client: &str) -> Result<Self, OwnerError> {
        let registration = OwnerRegistration::load(layout)?;
        if registration.socket != layout.socket() {
            return Err(OwnerError::Registration);
        }
        if registration.protocol != PROTOCOL {
            return Err(OwnerError::ProtocolMismatch);
        }
        let mut stream = UnixStream::connect(&registration.socket).map_err(|error| match error
            .kind()
        {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => OwnerError::NotRunning,
            _ => OwnerError::Connection,
        })?;
        // Another user could have bound a socket at this path while no owner ran.
        if peer_uid(&stream).ok() != Some(current_uid()) {
            return Err(OwnerError::Registration);
        }
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(REQUEST_TIMEOUT)))
            .map_err(|_| OwnerError::Connection)?;
        let hello = Hello {
            protocol: PROTOCOL,
            client: client.to_owned(),
            token: registration.token,
        };
        write_message(&mut stream, &hello).map_err(|_| OwnerError::Connection)?;
        match read_message(&mut stream).map_err(|_| OwnerError::Connection)? {
            Some(OwnerResponse::Welcome { version, .. }) => Ok(Self {
                stream: Mutex::new(Some(stream)),
                version,
            }),
            Some(OwnerResponse::Error { code }) => Err(OwnerError::Refused(code)),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// The owner's version.
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn state(&self) -> Result<OwnerState, OwnerError> {
        match self.request(&OwnerRequest::State)? {
            OwnerResponse::State { state } => Ok(state),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Status lines for `sessions` (at most `MAX_SESSIONS`), in order.
    pub fn statuses(&self, sessions: &[SessionId]) -> Result<Vec<StatusLine>, OwnerError> {
        if sessions.len() > MAX_SESSIONS {
            return Err(OwnerError::TooManySessions);
        }
        let request = OwnerRequest::Statuses {
            sessions: sessions.to_vec(),
        };
        match self.request(&request)? {
            OwnerResponse::Statuses { statuses }
                if statuses.len() == sessions.len()
                    && statuses
                        .iter()
                        .zip(sessions)
                        .all(|(line, id)| &line.session == id) =>
            {
                Ok(statuses)
            }
            _ => Err(OwnerError::Protocol),
        }
    }

    /// What enabling would bind. Show all of it before calling `enable`.
    pub fn consent_offer(&self) -> Result<OfferView, OwnerError> {
        match self.request(&OwnerRequest::ConsentOffer)? {
            OwnerResponse::Offer { offer } => Ok(offer),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Confirm the user's explicit choice for the offer with this id.
    pub fn enable(
        &self,
        offer: &str,
        folders: Vec<FolderChoice>,
    ) -> Result<ConsentView, OwnerError> {
        let request = OwnerRequest::Enable {
            offer: offer.to_owned(),
            folders,
        };
        match self.request(&request)? {
            OwnerResponse::Consent { consent } => Ok(consent),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Stop sharing, if consent is still at `revision`.
    pub fn disable(&self, revision: u64) -> Result<ConsentView, OwnerError> {
        match self.request(&OwnerRequest::Disable { revision })? {
            OwnerResponse::Consent { consent } => Ok(consent),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Stop sharing and remove local credentials.
    pub fn sign_out(&self) -> Result<(), OwnerError> {
        match self.request(&OwnerRequest::SignOut)? {
            OwnerResponse::Done => Ok(()),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Start a device sign-in, replacing any unfinished one. Poll `sign_in_status`
    /// until it is `SignedIn` or `Failed`.
    pub fn sign_in_start(&self) -> Result<SignInView, OwnerError> {
        match self.request(&OwnerRequest::SignInStart)? {
            OwnerResponse::SignIn { sign_in } => Ok(sign_in),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Where the current sign-in is up to.
    pub fn sign_in_status(&self) -> Result<SignInView, OwnerError> {
        match self.request(&OwnerRequest::SignInStatus)? {
            OwnerResponse::SignIn { sign_in } => Ok(sign_in),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Ask the service to stop, e.g. to replace it with a newer build.
    pub fn shutdown(&self) -> Result<(), OwnerError> {
        match self.request(&OwnerRequest::Shutdown)? {
            OwnerResponse::Done => Ok(()),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Renew the account's access token now rather than when it's due.
    pub fn renew(&self) -> Result<(), OwnerError> {
        match self.request(&OwnerRequest::Renew)? {
            OwnerResponse::Done => Ok(()),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Make a new AI summary of `session` now.
    pub fn summarize(&self, session: &SessionId) -> Result<(), OwnerError> {
        match self.request(&OwnerRequest::Summarize {
            session: session.clone(),
        })? {
            OwnerResponse::Done => Ok(()),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Turn the account's feature switches on or off; `None` leaves one as it is.
    pub fn set_features(
        &self,
        ai_session_summaries: Option<bool>,
        feed: Option<bool>,
    ) -> Result<(), OwnerError> {
        match self.request(&OwnerRequest::SetFeatures {
            ai_session_summaries,
            feed,
        })? {
            OwnerResponse::Done => Ok(()),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// The whole turn feed entry `entry` finished, past `after` when given.
    pub fn feed_turn(&self, entry: &str, after: Option<u64>) -> Result<FeedTurn, OwnerError> {
        match self.request(&OwnerRequest::FeedTurn {
            entry: entry.to_owned(),
            after,
        })? {
            OwnerResponse::FeedTurn { turn } => Ok(turn),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Write `text` back to feed entry `entry`.
    pub fn feed_reply(&self, entry: &str, text: &str) -> Result<ReplyWrite, OwnerError> {
        match self.request(&OwnerRequest::FeedReply {
            entry: entry.to_owned(),
            text: text.to_owned(),
        })? {
            OwnerResponse::FeedReply { reply } => Ok(reply),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// The feed entries that changed after `after` for a client holding
    /// `generation`; ask again at once while `more`.
    pub fn feed(
        &self,
        after: Option<u64>,
        generation: Option<u64>,
    ) -> Result<FeedPage, OwnerError> {
        match self.request(&OwnerRequest::Feed { after, generation })? {
            OwnerResponse::Feed { feed } => Ok(feed),
            _ => Err(OwnerError::Protocol),
        }
    }

    /// Abandon an unfinished sign-in.
    pub fn sign_in_cancel(&self) -> Result<(), OwnerError> {
        match self.request(&OwnerRequest::SignInCancel)? {
            OwnerResponse::Done => Ok(()),
            _ => Err(OwnerError::Protocol),
        }
    }

    fn request(&self, request: &OwnerRequest) -> Result<OwnerResponse, OwnerError> {
        let mut connection = self.stream.lock().map_err(|_| OwnerError::Connection)?;
        let stream = connection.as_mut().ok_or(OwnerError::Connection)?;
        let answer = write_message(stream, request).and_then(|()| read_message(stream));
        match answer {
            Ok(Some(OwnerResponse::Error { code })) => Err(OwnerError::Refused(code)),
            Ok(Some(response)) => Ok(response),
            Ok(None) | Err(_) => {
                *connection = None;
                Err(OwnerError::Connection)
            }
        }
    }
}

impl fmt::Debug for OwnerClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnerClient")
    }
}
