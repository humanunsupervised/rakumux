//! Enrolled machines: machines of the account with no sign-in of their own,
//! such as the user's Fly Sprite (`contracts/feed/`, "Enrolled machines"). A
//! signed-in machine enrols one with its Clerk token and writes the secret it
//! gets beside that machine's account service, in [`CREDENTIAL_FILE`] (0600).
//! There the service never signs in with Clerk: it trades the secret for
//! rakumux.com's access tokens ([`MachineTokens`], 15 minutes each), renewed as
//! a Clerk sign-in's are, and uses them for what a machine does.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::auth::{
    AccountError, AccountIdentity, AccountService, AccountSession, AuthError, AuthorizationGrant,
    CredentialStore, DeviceAuthConfig, ProviderIdentity, SecretValue, TokenProvider, TokenReceipt,
    VerifiedAuthorization, WallClock,
};
use crate::cloud::{Cancellation, CloudAIClient, CloudAIError, FeedHttp, FeedHttpReply};
use crate::feed::FeedFailure;

/// The enrolled machine's credential, in the account service's directory.
pub const CREDENTIAL_FILE: &str = "machine-credential.json";
const VERSION: u8 = 1;
const MAX_FILE_BYTES: u64 = 4096;
/// `rkm_` and 32 random bytes, base64url without padding.
const SECRET_PREFIX: &str = "rkm_";
const SECRET_CHARS: usize = 43;
const ACCESS_PREFIX: &str = "rkma.v1.";
const MAX_ACCESS_BYTES: usize = 1024;
/// The longest an access token is taken to last, whatever rakumux.com says.
const MAX_LIFETIME: Duration = Duration::from_secs(60 * 60);

/// What [`CREDENTIAL_FILE`] holds: the secret, the machine id it was enrolled
/// with, and the account, as the enrolling machine's sign-in verified it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    version: u8,
    machine_id: String,
    secret: String,
    account: AccountFile,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountFile {
    subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

/// Why a credential couldn't be read.
#[derive(Debug, PartialEq, Eq)]
pub enum CredentialError {
    /// Not a regular file of this user, private to them, or too large.
    Unsafe,
    /// Not a credential of this version.
    Invalid,
    Io(io::ErrorKind),
}

/// An enrolled machine's credential.
pub struct MachineCredential {
    machine_id: String,
    secret: SecretValue,
    account: AccountIdentity,
}

impl fmt::Debug for MachineCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachineCredential")
            .field("machine_id", &self.machine_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl MachineCredential {
    /// The credential `secret` gives machine `machine_id` (a ULID) of `account`.
    pub fn new(
        machine_id: &str,
        secret: &str,
        account: &AccountIdentity,
    ) -> Result<Self, CredentialError> {
        if !crate::feed::is_entry_id(machine_id) || !is_secret(secret) {
            return Err(CredentialError::Invalid);
        }
        Ok(Self {
            machine_id: machine_id.to_owned(),
            secret: SecretValue::try_from(secret.to_owned())
                .map_err(|_| CredentialError::Invalid)?,
            account: account.clone(),
        })
    }

    /// The machine id it was enrolled with.
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    /// The file's contents, for the enrolling machine to write (0600).
    pub fn to_json(&self) -> String {
        let file = CredentialFile {
            version: VERSION,
            machine_id: self.machine_id.clone(),
            secret: self.secret.expose_secret().to_owned(),
            account: AccountFile {
                subject: self.account.subject.clone(),
                email: self.account.email.clone(),
                email_verified: self.account.email_verified,
                name: self.account.name.clone(),
            },
        };
        // A struct of strings and a bool always serializes.
        serde_json::to_string(&file).unwrap_or_default()
    }

    /// The credential in `path`; `None` when there's none. The file must be a
    /// regular file of this user that only they can read.
    pub fn load(path: &Path) -> Result<Option<Self>, CredentialError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(CredentialError::Io(error.kind())),
        };
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        if !metadata.file_type().is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
            || metadata.len() > MAX_FILE_BYTES
        {
            return Err(CredentialError::Unsafe);
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|error| CredentialError::Io(error.kind()))?;
        Self::parse(&bytes).map(Some)
    }

    fn parse(bytes: &[u8]) -> Result<Self, CredentialError> {
        let file: CredentialFile =
            serde_json::from_slice(bytes).map_err(|_| CredentialError::Invalid)?;
        if file.version != VERSION {
            return Err(CredentialError::Invalid);
        }
        let account = AccountIdentity {
            subject: file.account.subject,
            email: file.account.email,
            email_verified: file.account.email_verified,
            name: file.account.name,
        }
        .validate()
        .map_err(|_| CredentialError::Invalid)?;
        Self::new(&file.machine_id, &file.secret, &account)
    }
}

fn is_secret(secret: &str) -> bool {
    secret.strip_prefix(SECRET_PREFIX).is_some_and(|rest| {
        rest.len() == SECRET_CHARS
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    })
}

/// An enrolled machine's access tokens, from rakumux.com for its secret: the
/// account service's [`TokenProvider`] in place of Clerk. Its grants carry the
/// service's own provider and scopes, so they're kept and leased as a Clerk
/// sign-in's are; only this provider renews them, never Clerk.
pub struct MachineTokens {
    cloud: CloudAIClient,
    config: DeviceAuthConfig,
    credential: MachineCredential,
}

impl fmt::Debug for MachineTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MachineTokens([REDACTED])")
    }
}

impl MachineTokens {
    /// `cloud` must name the credential's machine id (`with_machine_id`).
    pub fn new(
        cloud: CloudAIClient,
        config: DeviceAuthConfig,
        credential: MachineCredential,
    ) -> Self {
        Self {
            cloud,
            config,
            credential,
        }
    }

    /// A new access token for the secret (`POST /api/v1/machines/token`). A
    /// secret rakumux.com refuses (revoked, or enrolled again elsewhere) is
    /// `InvalidGrant`: enrolling the machine again is what helps.
    pub fn exchange(&self) -> Result<AuthorizationGrant, AuthError> {
        let receipt = TokenReceipt::now();
        let reply = self
            .cloud
            .feed(
                &self.credential.secret,
                FeedHttp::MachineToken,
                &Cancellation::new(),
            )
            .map_err(auth_error)?;
        let (token, expires_at) = access_token(&reply)?;
        let expires_in = UNIX_EPOCH
            .checked_add(Duration::from_millis(expires_at))
            .and_then(|expires| expires.duration_since(receipt.wall).ok())
            .filter(|lifetime| lifetime.as_secs() > 0)
            .ok_or(AuthError::InvalidResponse("expired machine token"))?
            .min(MAX_LIFETIME);
        Ok(AuthorizationGrant {
            provider: Box::new(ProviderIdentity::from(&self.config)),
            access_token: SecretValue::try_from(token)
                .map_err(|_| AuthError::InvalidResponse("invalid machine token"))?,
            // The store keeps a sign-in's refresh token beside its access token;
            // renewing always takes the credential's own secret.
            refresh_token: Some(self.credential.secret.clone()),
            expires_in,
            scopes: self.config.scopes().to_vec(),
            receipt: Some(receipt),
        })
    }

    /// The account the credential is for.
    pub fn account(&self) -> &AccountIdentity {
        &self.credential.account
    }
}

impl TokenProvider for MachineTokens {
    fn refresh(&self, _grant: &AuthorizationGrant) -> Result<AuthorizationGrant, AuthError> {
        self.exchange()
    }

    /// The credential's account: rakumux.com took the secret it was enrolled
    /// with, and signs each token for that account and this machine.
    fn verify_account(
        &self,
        grant: AuthorizationGrant,
    ) -> Result<VerifiedAuthorization, AuthError> {
        if *grant.provider != ProviderIdentity::from(&self.config) {
            return Err(AuthError::UntrustedEndpoint);
        }
        Ok(VerifiedAuthorization {
            identity: self.credential.account.clone(),
            grant,
        })
    }
}

fn auth_error(error: CloudAIError) -> AuthError {
    match error {
        CloudAIError::Network(failure) => AuthError::Network(failure),
        CloudAIError::HttpStatus(status) => AuthError::HttpStatus(status),
        _ => AuthError::InvalidResponse("invalid machine token answer"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireToken {
    version: u8,
    access_token: String,
    expires_at: u64,
}

/// A token answer's token and expiry (Unix milliseconds).
fn access_token(reply: &FeedHttpReply) -> Result<(String, u64), AuthError> {
    match reply.status {
        200 => {}
        401 => return Err(AuthError::InvalidGrant),
        status => return Err(AuthError::HttpStatus(status)),
    }
    let answer: WireToken = serde_json::from_slice(&reply.body)
        .map_err(|_| AuthError::InvalidResponse("invalid machine token answer"))?;
    let fits = answer.version == 1
        && answer.access_token.len() <= MAX_ACCESS_BYTES
        && answer.access_token.starts_with(ACCESS_PREFIX)
        && answer
            .access_token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !fits {
        return Err(AuthError::InvalidResponse("invalid machine token answer"));
    }
    Ok((answer.access_token, answer.expires_at))
}

/// Sign `account` in with the enrolled machine's credential: an access token
/// for its secret, saved as a sign-in is.
pub fn sign_in<S: CredentialStore, C: WallClock>(
    account: &AccountService<S, C>,
    tokens: &MachineTokens,
) -> Result<AccountSession, AccountError> {
    let ticket = account.begin_login()?;
    let verified = tokens.verify_account(tokens.exchange()?)?;
    account.commit_login(&ticket, verified)
}

/// The body enrolling machine `machine_id` as `name`, woken at `wake_url`.
pub(crate) fn enrol_body(machine_id: &str, name: &str, wake_url: Option<&str>) -> Option<Vec<u8>> {
    #[derive(Serialize)]
    struct Body<'a> {
        version: u8,
        machine_id: &'a str,
        name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        wake_url: Option<&'a str>,
    }
    let fits = crate::feed::is_entry_id(machine_id)
        && is_machine_name(name)
        && wake_url.is_none_or(is_wake_url);
    fits.then(|| {
        serde_json::to_vec(&Body {
            version: 1,
            machine_id,
            name,
            wake_url,
        })
        .ok()
    })
    .flatten()
}

/// One visible line of at most 64 characters, as the feed names machines.
fn is_machine_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= 64
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

/// A Fly Sprite's public URL, the one wake URL rakumux.com takes.
pub fn is_wake_url(url: &str) -> bool {
    url.strip_prefix("https://")
        .and_then(|host| host.strip_suffix(".sprites.app"))
        .is_some_and(|label| {
            (1..=64).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// The secret an enrol answer gave.
pub(crate) fn enrolled(reply: &FeedHttpReply) -> Result<String, FeedFailure> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        version: u8,
        #[allow(dead_code)]
        machine_id: String,
        secret: String,
        #[allow(dead_code)]
        created_at: u64,
    }
    if !matches!(reply.status, 200 | 201) {
        return Err(crate::feed::refused(reply));
    }
    let answer: Wire = serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if answer.version != 1 || !is_secret(&answer.secret) {
        return Err(FeedFailure::Invalid);
    }
    Ok(answer.secret)
}

/// Whether a revoke answer says the machine's credential is gone: revoked
/// now (204), or there was none (404).
pub(crate) fn revoked(reply: &FeedHttpReply) -> Result<(), FeedFailure> {
    match reply.status {
        204 | 404 => Ok(()),
        _ => Err(crate::feed::refused(reply)),
    }
}

#[cfg(test)]
mod tests;
