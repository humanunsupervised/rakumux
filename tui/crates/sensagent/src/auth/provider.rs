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
