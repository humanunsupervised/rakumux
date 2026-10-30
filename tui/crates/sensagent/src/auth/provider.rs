use super::{AuthError, AuthorizationGrant, ClerkDeviceClient, VerifiedAuthorization};

/// Where an account's access tokens come from: Clerk, with the sign-in's refresh
/// token, or rakumux.com for an enrolled machine, with its secret
/// (`crate::enrolment`).
pub trait TokenProvider: Send + Sync {
    /// A new grant for `grant`'s account.
    fn refresh(&self, grant: &AuthorizationGrant) -> Result<AuthorizationGrant, AuthError>;

    /// The account `grant` is for, as its provider vouches.
    fn verify_account(&self, grant: AuthorizationGrant)
        -> Result<VerifiedAuthorization, AuthError>;
}

/// Ends a sign-in at its provider: Clerk's revocation endpoint for a terminal's
/// own sign-in. An enrolled machine's credential is revoked on its own, from
/// the machine that enrolled it (`crate::enrolment`), never here.
pub trait GrantRevoker: Send + Sync {
    /// Revoke `grant`'s refresh token (and its access token where possible).
    fn revoke(&self, grant: &AuthorizationGrant) -> Result<(), AuthError>;
}

impl GrantRevoker for ClerkDeviceClient {
    fn revoke(&self, grant: &AuthorizationGrant) -> Result<(), AuthError> {
        ClerkDeviceClient::revoke(self, grant)
    }
}

impl TokenProvider for ClerkDeviceClient {
    fn refresh(&self, grant: &AuthorizationGrant) -> Result<AuthorizationGrant, AuthError> {
        ClerkDeviceClient::refresh(self, grant)
    }

    fn verify_account(
        &self,
        grant: AuthorizationGrant,
    ) -> Result<VerifiedAuthorization, AuthError> {
        ClerkDeviceClient::verify_account(self, grant)
    }
}
