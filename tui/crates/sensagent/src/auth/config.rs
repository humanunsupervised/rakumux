use reqwest::Url;

use super::AuthError;

/// The scope that authorizes AI session status unless another is configured.
pub const AI_STATUS_SCOPE: &str = "ai:session-status";

/// Public configuration only. A distributed terminal client never has a client secret.
#[derive(Clone, Debug)]
pub struct DeviceAuthConfig {
    pub(crate) issuer: Url,
    pub(crate) client_id: String,
    pub(crate) verification_uri: Url,
    pub(crate) scopes: Vec<String>,
    pub(crate) userinfo_endpoint: Option<Url>,
    /// The requested scope the AI status service authorizes.
    pub(crate) status_scope: String,
}

impl DeviceAuthConfig {
    /// Configure a trusted Clerk HTTPS issuer and its exact Account Portal device page.
    ///
    /// Issuers must be root origins; endpoint discovery is never guessed from an
    /// OAuth error or model output. Scopes must be configured on the public client.
    pub fn new(
        issuer: &str,
        client_id: &str,
        verification_uri: &str,
        scopes: &[&str],
    ) -> Result<Self, AuthError> {
        let issuer = trusted_url(issuer)?;
        if issuer.path() != "/" {
            return Err(AuthError::InvalidConfiguration(
                "issuer must be a root origin",
            ));
        }
        let verification_uri = trusted_url(verification_uri)?;
        if !valid_client_id(client_id) {
            return Err(AuthError::InvalidConfiguration("invalid public client ID"));
        }
        if scopes.is_empty() || scopes.len() > 16 {
            return Err(AuthError::InvalidConfiguration("provide 1 to 16 scopes"));
        }
        let mut configured = Vec::new();
        for scope in scopes {
            if !valid_scope(scope) || configured.iter().any(|value| value == scope) {
                return Err(AuthError::InvalidConfiguration(
                    "invalid or duplicate scope",
                ));
            }
            configured.push((*scope).to_owned());
        }
        Ok(Self {
            issuer,
            client_id: client_id.to_owned(),
            verification_uri,
            scopes: configured,
            userinfo_endpoint: None,
            status_scope: AI_STATUS_SCOPE.to_owned(),
        })
    }

    /// The pinned HTTPS issuer used for discovery and endpoint-origin validation.
    pub fn issuer(&self) -> &Url {
        &self.issuer
    }

    /// The non-secret public OAuth client identifier.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The exact trusted provider-hosted device verification page.
    pub fn verification_uri(&self) -> &Url {
        &self.verification_uri
    }

    /// The requested scopes; feature authorization is also enforced by the API.
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// The requested scope the AI status service authorizes: [`AI_STATUS_SCOPE`]
    /// unless [`with_status_scope`](Self::with_status_scope) chose another.
    pub fn status_scope(&self) -> &str {
        &self.status_scope
    }

    /// Name the requested scope the AI status service authorizes, for a provider
    /// without custom scopes. It must be one of the requested scopes.
    pub fn with_status_scope(mut self, scope: &str) -> Result<Self, AuthError> {
        if !self.scopes.iter().any(|requested| requested == scope) {
            return Err(AuthError::InvalidConfiguration(
                "the AI status scope must be requested",
            ));
        }
        self.status_scope = scope.to_owned();
        Ok(self)
    }

    /// Pin UserInfo from reviewed OAuth application settings when metadata omits it.
    /// The URL must share the trusted issuer origin. Conflicting advertised
    /// metadata is rejected rather than silently overriding this explicit choice.
    pub fn with_userinfo_endpoint(mut self, endpoint: &str) -> Result<Self, AuthError> {
        self.userinfo_endpoint = Some(self.provider_endpoint(endpoint)?);
        Ok(self)
    }

    pub(crate) fn provider_endpoint(&self, value: &str) -> Result<Url, AuthError> {
        let endpoint = trusted_url(value).map_err(|_| AuthError::UntrustedEndpoint)?;
        if endpoint.origin() != self.issuer.origin() {
            return Err(AuthError::UntrustedEndpoint);
        }
        Ok(endpoint)
    }
}

pub(crate) fn trusted_url(value: &str) -> Result<Url, AuthError> {
    let url =
        Url::parse(value).map_err(|_| AuthError::InvalidConfiguration("invalid HTTPS URL"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthError::InvalidConfiguration(
            "expected a clean HTTPS URL",
        ));
    }
    Ok(url)
}

pub(crate) fn valid_client_id(client_id: &str) -> bool {
    !client_id.is_empty()
        && client_id.len() <= 512
        && client_id.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(crate) fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= 128
        && scope.bytes().all(|byte| {
            byte == 0x21 || (0x23..=0x5b).contains(&byte) || (0x5d..=0x7e).contains(&byte)
        })
}
