use anyhow::{anyhow, Result};
use reqwest::Client as HttpClient;
use std::time::Duration;

use super::models::*;

#[derive(Debug, Clone)]
pub struct ClerkApi {
    base: String,
    pub_key: String,
    client: HttpClient,
}

impl ClerkApi {
    pub fn new(frontend_api: String, publishable_key: String) -> Result<Self> {
        if frontend_api.is_empty() || publishable_key.is_empty() {
            return Err(anyhow!("Clerk not configured"));
        }
        let base = format!("https://{}/v1", frontend_api.trim());
        let client = HttpClient::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            base,
            pub_key: publishable_key,
            client,
        })
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderValue, ORIGIN};
        let mut h = HeaderMap::new();
        // Minimal header set; Clerk uses publishable key in headers or body; keep minimal for now
        h.insert(ORIGIN, HeaderValue::from_static("https://rakumux.local"));
        h
    }

    pub async fn create_client(&self) -> Result<Client> {
        // Stub for now
        let _ = &self.base;
        Ok(Client {
            id: Some("stub_client".into()),
        })
    }

    pub async fn sign_in_start_email_code(&self, _email: &str) -> Result<SignInStart> {
        Ok(SignInStart {
            id: Some("stub_sign_in".into()),
        })
    }

    pub async fn sign_in_attempt_code(
        &self,
        _sign_in_id: &str,
        _code: &str,
    ) -> Result<SessionToken> {
        Ok(SessionToken {
            session_id: Some("stub_session".into()),
            token: Some("stub_token".into()),
        })
    }

    pub async fn sign_out(&self, _session_id: &str) -> Result<()> {
        Ok(())
    }
}
