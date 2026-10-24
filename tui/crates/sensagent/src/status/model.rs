use std::fmt;
use std::path::Component;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;

use super::{StatusStoreError, StatusText};

/// Stable account identity for local consent/cache ownership, never a bearer proof.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountKey {
    /// Pinned HTTPS issuer, including its canonical root slash.
    pub issuer: String,
    /// Public native OAuth client identifier.
    pub client_id: String,
    /// Verified provider subject; email is not ownership.
    pub subject: String,
}

impl AccountKey {
    /// Validate local disk data without trusting it as current authentication.
    pub fn validate(&self) -> Result<(), StatusStoreError> {
        let issuer = Url::parse(&self.issuer).map_err(|_| StatusStoreError::InvalidData)?;
        if issuer.scheme() != "https"
            || issuer.host_str().is_none()
            || issuer.path() != "/"
            || !issuer.username().is_empty()
            || issuer.password().is_some()
            || issuer.query().is_some()
            || issuer.fragment().is_some()
            || issuer.as_str() != self.issuer
            || self.issuer.len() > 2048
            || !ascii_identifier(&self.client_id, 512)
            || !ascii_identifier(&self.subject, 256)
        {
            return Err(StatusStoreError::InvalidData);
        }
        Ok(())
    }
}

impl fmt::Debug for AccountKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountKey { identity: [REDACTED] }")
    }
}

/// A trusted local account gate's current observation. No credential is included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountBinding {
    /// Stable account ownership.
    pub account: AccountKey,
    /// Current persistent credential revision; zero is never authenticated.
    pub auth_revision: u64,
}

impl AccountBinding {
    /// Reject malformed adapter observations before using them as policy inputs.
    pub fn validate(&self) -> Result<(), StatusStoreError> {
        self.account.validate()?;
        if self.auth_revision == 0 {
            return Err(StatusStoreError::InvalidData);
        }
        Ok(())
    }
}

/// Persistent generation fences stale consent writes and cache completions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConsentRevision(pub u64);

/// Explicit per-folder data selection; there is no implicit all-folders grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderConsent {
    /// Already-canonical folder supplied by the local session API/owner.
    pub folder: ScopeRoot,
    /// False permits reports only, never recent terminal output.
    pub include_terminal_output: bool,
}

/// Disabled by default. Enabling always binds the exact current account generation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsentPolicy {
    /// No AI sharing, regardless of whether an account is signed in.
    #[default]
    Disabled,
    /// An explicit local-owner action selected these folders and data kinds.
    Enabled {
        /// Exact account and credential revision approved by the owner.
        account: AccountKey,
        /// Fresh sign-in/restore revisions require explicit reconfirmation in this slice.
        auth_revision: u64,
        /// Nonempty, unique, bounded canonical folder selections.
        folders: Vec<FolderConsent>,
    },
}

impl ConsentPolicy {
    /// Validate bounded private-store records, not cloud authorization.
    pub fn validate(&self) -> Result<(), StatusStoreError> {
        if let Self::Enabled {
            account,
            auth_revision,
            folders,
        } = self
        {
            account.validate()?;
            if *auth_revision == 0 || folders.is_empty() || folders.len() > 64 {
                return Err(StatusStoreError::InvalidData);
            }
            for (index, consent) in folders.iter().enumerate() {
                validate_folder(&consent.folder)?;
                if folders[..index]
                    .iter()
                    .any(|previous| previous.folder == consent.folder)
                {
                    return Err(StatusStoreError::InvalidData);
                }
            }
        }
        Ok(())
    }

    /// Whether this policy only adds to `earlier`: the same account (a renewed
    /// sign-in has a new credential revision), every earlier folder still shared,
    /// none with less data. Cached text survives such a change (a renewal, a
    /// session started in a new folder), since summaries are made once per chat
    /// and on request, not on every change.
    pub fn extends(&self, earlier: &ConsentPolicy) -> bool {
        match (self, earlier) {
            (
                Self::Enabled {
                    account, folders, ..
                },
                Self::Enabled {
                    account: earlier_account,
                    folders: earlier_folders,
                    ..
                },
            ) => {
                account == earlier_account
                    && earlier_folders.iter().all(|before| {
                        folders.iter().any(|now| {
                            now.folder == before.folder
                                && (now.include_terminal_output || !before.include_terminal_output)
                        })
                    })
            }
            _ => false,
        }
    }

    /// The credential revision an enabled policy binds.
    pub fn auth_revision(&self) -> Option<u64> {
        match self {
            Self::Enabled { auth_revision, .. } => Some(*auth_revision),
            Self::Disabled => None,
        }
    }

    /// Whether a cache record belongs to the exact currently approved account/folder.
    pub fn permits_cache(&self, entry: &CachedStatus) -> bool {
        match self {
            Self::Enabled {
                account,
                auth_revision,
                folders,
            } => {
                account == &entry.account
                    && *auth_revision == entry.auth_revision
                    && folders.iter().any(|consent| consent.folder == entry.folder)
            }
            Self::Disabled => false,
        }
    }
}

/// A bounded persisted policy snapshot. Loading it does not authenticate its account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentSnapshot {
    /// Persistent compare-and-swap revision.
    pub revision: ConsentRevision,
    /// Current explicit sharing decision.
    pub policy: ConsentPolicy,
}

impl ConsentSnapshot {
    /// Validate a loaded revision/policy pair; revision zero is the missing default only.
    pub fn validate(&self) -> Result<(), StatusStoreError> {
        self.policy.validate()?;
        if self.revision.0 == 0 && self.policy != ConsentPolicy::Disabled {
            return Err(StatusStoreError::InvalidData);
        }
        Ok(())
    }
}

/// Ordered observations from one future local session owner, not a PID or model label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceStamp {
    /// Increases when the daemon terminal/session incarnation changes.
    pub terminal_generation: u64,
    /// Increases on relevant output, lifecycle, activity or privacy changes.
    pub source_revision: u64,
}

/// Whether selected/redacted bounded sources contain meaningful context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContextAvailability {
    /// A useful bounded report is available.
    pub reported_text: bool,
    /// Useful recent output is available; consent is still independently required.
    pub recent_output: bool,
}

/// Local-only observation. It is never serialized into a cloud request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSource {
    /// Daemon-owned session identifier.
    pub session_id: SessionId,
    /// Its canonical folder.
    pub folder: ScopeRoot,
    /// Exact observed source incarnation/revision.
    pub stamp: SourceStamp,
    /// Private sessions are excluded even with folder consent.
    pub private: bool,
    /// Exited/dead sessions do not generate status in this first policy slice.
    pub live: bool,
    /// Context eligibility, not raw text or proof of redaction.
    pub context: ContextAvailability,
}

/// Allowed input fields for a prepared job; never infer sharing from sign-in alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSelection {
    /// Only an explicitly allowed bounded report.
    ReportOnly,
    /// Reports and selected terminal text, after separate output-sharing consent.
    ReportAndOutput,
}

/// Local advisory cache. Contains no access tokens or raw terminal context.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedStatus {
    /// Stable account owner.
    pub account: AccountKey,
    /// Credential revision used for this generation.
    pub auth_revision: u64,
    /// Explicit consent revision used for this generation.
    pub consent_revision: ConsentRevision,
    /// Local daemon session identifier, never a cloud correlation value.
    pub session_id: SessionId,
    /// Canonical folder used for local confinement.
    pub folder: ScopeRoot,
    /// Exact source that was summarized.
    pub stamp: SourceStamp,
    /// Fresh public correlation only; not a local session/account/cache identifier.
    pub request_id: String,
    /// Validated advisory text, not lifecycle or executable output.
    pub text: StatusText,
    /// Local capture/completion time in Unix seconds, not an authorization proof.
    pub captured_at: u64,
}

impl CachedStatus {
    /// Reject malformed private disk rows before returning any cached text.
    pub fn validate(&self) -> Result<(), StatusStoreError> {
        self.account.validate()?;
        validate_folder(&self.folder)?;
        validate_session(&self.session_id)?;
        if self.auth_revision == 0
            || self.consent_revision.0 == 0
            || self.stamp.terminal_generation == 0
            || self.stamp.source_revision == 0
            || !valid_request_id(&self.request_id)
        {
            return Err(StatusStoreError::InvalidData);
        }
        Ok(())
    }
}

impl fmt::Debug for CachedStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CachedStatus { identity_and_content: [REDACTED] }")
    }
}

pub(crate) fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub(crate) fn validate_folder(folder: &ScopeRoot) -> Result<(), StatusStoreError> {
    if !folder.path().is_absolute()
        || folder.path().as_os_str().len() > 4096
        || folder
            .path()
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(StatusStoreError::InvalidData);
    }
    Ok(())
}

pub(crate) fn validate_session(session: &SessionId) -> Result<(), StatusStoreError> {
    if session.as_str().is_empty()
        || session.as_str().len() > 256
        || session.as_str().chars().any(char::is_control)
    {
        return Err(StatusStoreError::InvalidData);
    }
    Ok(())
}

fn ascii_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && value.bytes().all(|byte| byte.is_ascii_graphic())
}
