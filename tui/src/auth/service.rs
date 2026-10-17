use anyhow::{anyhow, Result};

use super::{
    clerk_api::ClerkApi,
    store::{FileStore, SessionRecord},
};

#[derive(Debug, Clone, Default)]
pub struct AuthService {
    enabled: bool,
    clerk: Option<ClerkApi>,
    store: FileStore,
    // ephemeral state for sign-in flow
    pending_sign_in_id: Option<String>,
    pending_email: Option<String>,
}

impl AuthService {
    pub fn new_from_env() -> Self {
        let frontend = std::env::var("CLERK_FRONTEND_API").unwrap_or_default();
        let pub_key = std::env::var("CLERK_PUBLISHABLE_KEY").unwrap_or_default();
        let clerk = ClerkApi::new(frontend, pub_key).ok();
        let enabled = clerk.is_some();
        Self {
            enabled,
            clerk,
            store: FileStore::default(),
            pending_sign_in_id: None,
            pending_email: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn has_session(&self) -> bool {
        self.store.load().ok().flatten().is_some()
    }

    pub fn sign_out(&self) -> Result<()> {
        // Best-effort local clear; remote end is stubbed in ClerkApi
        self.store.clear()?;
        Ok(())
    }

    pub async fn start_email_sign_in(&mut self, email: &str) -> Result<()> {
        let api = self
            .clerk
            .as_ref()
            .ok_or_else(|| anyhow!("Clerk not configured"))?;
        let _client = api.create_client().await?;
        let start = api.sign_in_start_email_code(email).await?;
        let id = start.id.ok_or_else(|| anyhow!("Missing sign-in id"))?;
        self.pending_sign_in_id = Some(id);
        self.pending_email = Some(email.to_string());
        Ok(())
    }

    pub async fn complete_email_code(&mut self, code: &str) -> Result<()> {
        let api = self
            .clerk
            .as_ref()
            .ok_or_else(|| anyhow!("Clerk not configured"))?;
        let id = self
            .pending_sign_in_id
            .clone()
            .ok_or_else(|| anyhow!("No pending sign-in"))?;
        let token = api.sign_in_attempt_code(&id, code).await?;
        let rec = SessionRecord {
            session_id: token.session_id.unwrap_or_default(),
            token: token.token.unwrap_or_default(),
            email: self.pending_email.clone(),
        };
        self.store.save(&rec)?;
        Ok(())
    }
}
