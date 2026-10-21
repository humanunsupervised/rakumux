use serde::Deserialize;
use std::fmt;

use super::{AuthError, AuthorizationGrant};

/// Bounded account data. Cached identity alone never proves current authentication.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountIdentity {
    pub(crate) subject: String,
    pub(crate) email: Option<String>,
    pub(crate) email_verified: bool,
    pub(crate) name: Option<String>,
}

impl AccountIdentity {
    /// Stable provider subject used for account ownership, never an email match.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Optional provider email; use email_verified before claiming it is verified.
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    /// Whether the provider explicitly marked its email as verified.
    pub fn email_verified(&self) -> bool {
        self.email_verified
    }

    /// Optional bounded display name, not authorization information.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn validate(self) -> Result<Self, AuthError> {
        if self.subject.is_empty()
            || self.subject.len() > 256
            || !self.subject.bytes().all(|byte| byte.is_ascii_graphic())
            || self.email.as_deref().is_some_and(|email| {
                !valid_text(email, 320)
                    || email.chars().any(char::is_whitespace)
                    || email.matches('@').count() != 1
            })
            || self
                .name
                .as_deref()
                .is_some_and(|name| !valid_text(name, 256))
            || (self.email_verified && self.email.is_none())
        {
            return Err(AuthError::InvalidResponse("invalid account identity"));
        }
        Ok(self)
    }

    pub(crate) fn reject_credentials(&self, grant: &AuthorizationGrant) -> Result<(), AuthError> {
        let fields = [Some(self.subject.as_str()), self.email(), self.name()];
        let secrets = [Some(grant.access_token()), grant.refresh_token()];
        if fields.into_iter().flatten().any(|field| {
            secrets
                .into_iter()
                .flatten()
                .any(|secret| field.contains(secret.expose_secret()))
        }) {
            return Err(AuthError::InvalidResponse(
                "credential appeared in account identity",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for AccountIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountIdentity { personal_data: [REDACTED] }")
    }
}

/// Account identity and the exact grant verified through the trusted UserInfo endpoint.
///
/// Only the provider client constructs this proof. Persistence / current-generation
/// checks are still required before an application reports successful sign-in.
#[derive(Debug)]
pub struct VerifiedAuthorization {
    pub(crate) identity: AccountIdentity,
    pub(crate) grant: AuthorizationGrant,
}

impl VerifiedAuthorization {
    /// Account returned by the trusted provider for this exact access token.
    pub fn account(&self) -> &AccountIdentity {
        &self.identity
    }
}

#[derive(Deserialize)]
pub(crate) struct IdentityResponse {
    sub: String,
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
    name: Option<String>,
}

impl IdentityResponse {
    pub(crate) fn validate(self) -> Result<AccountIdentity, AuthError> {
        AccountIdentity {
            subject: self.sub,
            email: self.email,
            email_verified: self.email_verified,
            name: self.name,
        }
        .validate()
    }
}

fn valid_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && !value.chars().any(|character| {
            character.is_control()
                || matches!(character,
                    '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}' | '\u{feff}'
                )
        })
}
