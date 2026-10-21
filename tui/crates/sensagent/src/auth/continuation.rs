use reqwest::Url;
use thiserror::Error;

use super::config::trusted_url;
use super::{AuthError, VerificationPrompt};

const MAX_PROVIDER_URL_BYTES: usize = 2048;
const MAX_CONNECT_URL_BYTES: usize = 4096;

/// Reviewed public website origin. It never contains a credential or provider secret.
#[derive(Clone, Debug)]
pub struct AccountWebsite {
    origin: Url,
}

/// Branded continuation is optional; every failure retains the direct Clerk fallback.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum BrowserContinuationError {
    /// Never invent a prefilled provider link when the provider omitted it.
    #[error("the provider did not supply a prefilled verification link")]
    MissingProviderLink,
    /// A safe provider link can still exceed the website's bounded navigation contract.
    #[error("the provider link exceeds the website continuation limit")]
    LinkTooLong,
    /// A valid direct URL can use a query representation the branded website does not support.
    #[error("the provider link does not match the supported website continuation")]
    UnsupportedProviderLink,
}

impl AccountWebsite {
    /// Configure a trusted HTTPS root origin explicitly, not from terminal/model output.
    pub fn new(origin: &str) -> Result<Self, AuthError> {
        let url = trusted_url(origin)?;
        if origin.len() > MAX_PROVIDER_URL_BYTES
            || origin.chars().any(char::is_whitespace)
            || url.path() != "/"
        {
            return Err(AuthError::InvalidConfiguration(
                "expected a bounded website root origin",
            ));
        }
        Ok(Self { origin: url })
    }

    /// Build a fixed /connect URL containing only the provider's public code and link.
    ///
    /// This is navigation, not app approval or output-sharing consent. Display the
    /// canonical provider URL/code even when a branded URL is available. An error
    /// must leave that fallback usable; never truncate or guess a provider endpoint.
    pub fn connect_url(
        &self,
        prompt: &VerificationPrompt,
    ) -> Result<Url, BrowserContinuationError> {
        let complete = prompt
            .verification_uri_complete()
            .ok_or(BrowserContinuationError::MissingProviderLink)?;
        if complete.as_str().len() > MAX_PROVIDER_URL_BYTES {
            return Err(BrowserContinuationError::LinkTooLong);
        }
        if !supported_provider_link(prompt, complete) {
            return Err(BrowserContinuationError::UnsupportedProviderLink);
        }
        let mut link = self.origin.clone();
        link.set_path("/connect");
        link.query_pairs_mut()
            .append_pair("user_code", prompt.user_code())
            .append_pair("verification", complete.as_str());
        if link.as_str().len() > MAX_CONNECT_URL_BYTES {
            return Err(BrowserContinuationError::LinkTooLong);
        }
        Ok(link)
    }
}

fn supported_provider_link(prompt: &VerificationPrompt, complete: &Url) -> bool {
    let canonical = prompt.verification_uri();
    let code = prompt.user_code();
    canonical.scheme() == "https"
        && canonical.username().is_empty()
        && canonical.password().is_none()
        && canonical.query().is_none()
        && canonical.fragment().is_none()
        && !code.is_empty()
        && code.len() <= 128
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && complete.origin() == canonical.origin()
        && complete.path() == canonical.path()
        && complete.username().is_empty()
        && complete.password().is_none()
        && complete.fragment().is_none()
        && complete.query() == Some(format!("user_code={code}").as_str())
}

#[cfg(test)]
mod tests;
