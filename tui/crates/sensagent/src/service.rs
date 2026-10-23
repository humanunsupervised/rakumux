//! The per-home AI status service as one blocking call (what `rakumux status-service`
//! runs), its production configuration, and starting it from a client.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::paths::Layout;
use thiserror::Error;

use crate::auth::{
    AccountService, AccountWebsite, ClerkDeviceClient, CredentialStore, DeviceAuthConfig,
    FileCredentialStore,
};
use crate::cloud::{CloudAIClient, CloudAIConfig};
use crate::dispatch::StatusDispatchService;
use crate::owner::{OwnerClaim, OwnerClient, OwnerError, OwnerLayout, OwnerServer};
use crate::refresh::{RefreshRunner, StatusRefresher};
use crate::status::SqliteStatusStore;

/// Rakumux's production Clerk instance and its public "Rakumux terminal" client.
const ISSUER: &str = "https://clerk.rakumux.com";
const CLIENT_ID: &str = "PXOxFYwfkwKh2qjO";
const DEVICE_PAGE: &str = "https://accounts.rakumux.com/device";
const USERINFO: &str = "https://clerk.rakumux.com/oauth/userinfo";
const SCOPES: [&str; 4] = ["openid", "profile", "email", "offline_access"];
/// rakumux.com: the AI status API and the sign-in-then-approve page.
const CLOUD_ORIGIN: &str = "https://rakumux.com";

/// Environment overrides, for development against another Clerk instance or origin.
pub const ISSUER_ENV: &str = "RAKUMUX_CLERK_ISSUER";
pub const CLIENT_ID_ENV: &str = "RAKUMUX_CLERK_CLIENT_ID";
pub const DEVICE_PAGE_ENV: &str = "RAKUMUX_CLERK_DEVICE_PAGE";
pub const CLOUD_ORIGIN_ENV: &str = "RAKUMUX_CLOUD_ORIGIN";

/// How long a client waits for a service it started to accept connections: it
/// restores a saved account online (provider discovery, a token refresh) first.
const START_WAIT: Duration = Duration::from_secs(20);
const START_POLL: Duration = Duration::from_millis(100);
/// How long the service waits for the session daemon to accept it.
const DAEMON_WAIT: Duration = Duration::from_secs(3);

/// Static service failures; never provider, daemon or account text.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// Another process already serves this Rakumux home.
    #[error("the AI status service is already running for this Rakumux home")]
    AlreadyRunning,
    /// The built-in or overridden configuration is not valid.
    #[error("the AI status service configuration is invalid")]
    Configuration,
    /// The home's files, store or socket could not be prepared.
    #[error("the AI status service could not prepare this Rakumux home")]
    Setup,
    /// The session daemon could not be reached.
    #[error("the AI status service could not reach the session daemon")]
    Daemon,
    /// The service could not be started or reached.
    #[error("the AI status service could not be started")]
    Start,
}

/// Public sign-in and destination settings. Contains no credentials.
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    auth: DeviceAuthConfig,
    cloud_origin: String,
}

impl ServiceConfig {
    /// Production settings, with each value overridable by its environment variable.
    pub fn from_env() -> Result<Self, ServiceError> {
        let value =
            |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
        let issuer = value(ISSUER_ENV, ISSUER);
        let mut auth = DeviceAuthConfig::new(
            &issuer,
            &value(CLIENT_ID_ENV, CLIENT_ID),
            &value(DEVICE_PAGE_ENV, DEVICE_PAGE),
            &SCOPES,
        )
        .map_err(|_| ServiceError::Configuration)?;
        // The production userinfo endpoint belongs to the production issuer only.
        if issuer == ISSUER {
            auth = auth
                .with_userinfo_endpoint(USERINFO)
                .map_err(|_| ServiceError::Configuration)?;
        }
        Ok(Self {
            auth,
            cloud_origin: value(CLOUD_ORIGIN_ENV, CLOUD_ORIGIN),
        })
    }
}

/// Serve AI status for `home` until `stopping` returns true (checked every second).
/// Restores a saved account (online; offline it starts signed out), follows the
/// daemon's sessions and answers user clients on this home's private socket.
pub fn serve(
    home: &Layout,
    config: ServiceConfig,
    version: &str,
    stopping: &dyn Fn() -> bool,
) -> Result<(), ServiceError> {
    let layout = OwnerLayout::new(home);
    let mut claim = OwnerClaim::claim(&layout, version).map_err(|error| match error {
        OwnerError::AlreadyRunning => ServiceError::AlreadyRunning,
        _ => ServiceError::Setup,
    })?;
    let cloud = CloudAIClient::new(
        CloudAIConfig::new(&config.cloud_origin).map_err(|_| ServiceError::Configuration)?,
    )
    .map_err(|_| ServiceError::Configuration)?;
    let website = AccountWebsite::new(&config.cloud_origin).ok();
    let store = SqliteStatusStore::open(layout.status_dir()).map_err(|_| ServiceError::Setup)?;
    let account = Arc::new(AccountService::new(
        config.auth.clone(),
        FileCredentialStore::new(layout.credentials_dir()),
    ));
    let mut service = StatusDispatchService::new(account.clone(), store, cloud);
    let session = ClerkDeviceClient::discover(config.auth)
        .ok()
        .and_then(|client| account.restore(&client).ok().flatten());
    service
        .set_account_session(session)
        .map_err(|_| ServiceError::Setup)?;

    let api: Arc<dyn SessionApi> = Arc::new(connect_daemon(home, version)?);
    let refresher = StatusRefresher::new(service).following_account_switch();
    let runner = RefreshRunner::spawn_with_website(refresher, api, website)
        .map_err(|_| ServiceError::Setup)?;
    let server = OwnerServer::start(&mut claim, runner.client(), version)
        .map_err(|_| ServiceError::Setup)?;
    while !stopping() {
        thread::sleep(Duration::from_secs(1));
    }
    server.stop();
    runner.stop();
    drop(claim);
    Ok(())
}

/// The session daemon, waiting briefly for one that is still starting (a client
/// that started this service may have started the daemon at the same time).
fn connect_daemon(home: &Layout, version: &str) -> Result<Client, ServiceError> {
    let deadline = Instant::now() + DAEMON_WAIT;
    loop {
        let connected = Client::connect(ClientConfig {
            layout: home.clone(),
            client_name: "rakumux-status-service".into(),
            version: version.into(),
            auth: AuthMode::User,
            spawn: false,
            ptyd_path: None,
        });
        match connected {
            Ok(client) => return Ok(client),
            Err(_) if Instant::now() < deadline => thread::sleep(START_POLL),
            Err(_) => return Err(ServiceError::Daemon),
        }
    }
}

/// Whether this home has saved sign-in credentials (the store keeps its file
/// after a sign-out or an abandoned sign-in, so only its contents tell).
pub fn has_saved_account(home: &Layout) -> bool {
    let directory = OwnerLayout::new(home).credentials_dir();
    if !directory.exists() {
        return false;
    }
    FileCredentialStore::new(directory)
        .load()
        .is_ok_and(|snapshot| snapshot.credentials().is_some())
}

/// Connect to this home's service, starting `program status-service` detached (new
/// session, stdio on /dev/null, cwd `/`) when none answers, then waiting for it.
pub fn connect_or_start(
    home: &Layout,
    program: &Path,
    client_name: &str,
) -> Result<OwnerClient, ServiceError> {
    let layout = OwnerLayout::new(home);
    match OwnerClient::connect(&layout, client_name) {
        Ok(client) => return Ok(client),
        Err(OwnerError::NotRunning) => {}
        Err(_) => return Err(ServiceError::Start),
    }
    start_detached(home, program).map_err(|_| ServiceError::Start)?;
    let deadline = Instant::now() + START_WAIT;
    loop {
        match OwnerClient::connect(&layout, client_name) {
            Ok(client) => return Ok(client),
            Err(OwnerError::NotRunning) if Instant::now() < deadline => thread::sleep(START_POLL),
            Err(_) => return Err(ServiceError::Start),
        }
    }
}

fn start_detached(home: &Layout, program: &Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt;

    let program = std::path::absolute(program)?;
    let mut command = Command::new(program);
    command
        .arg("status-service")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir("/")
        .env(session_core::env::HOME, std::path::absolute(&home.home)?);
    // The service acts for the user, never as the session that started it.
    for name in [
        session_core::env::SESSION_ID,
        session_core::env::SESSION_TOKEN,
        session_core::env::SOCKET,
    ] {
        command.env_remove(name);
    }
    // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    thread::Builder::new()
        .name("rakumux-status-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn the_production_settings_are_valid() {
        let auth = DeviceAuthConfig::new(ISSUER, CLIENT_ID, DEVICE_PAGE, &SCOPES)
            .unwrap()
            .with_userinfo_endpoint(USERINFO);
        assert!(auth.is_ok());
        assert!(CloudAIConfig::new(CLOUD_ORIGIN).is_ok());
        assert!(AccountWebsite::new(CLOUD_ORIGIN).is_ok());
    }

    #[test]
    fn only_stored_credentials_are_a_saved_account() {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(home.path().to_path_buf());
        assert!(!has_saved_account(&layout));
        // A cleared store keeps its file.
        let directory = OwnerLayout::new(&layout).credentials_dir();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = FileCredentialStore::new(directory);
        let revision = store.load().unwrap().revision();
        store.compare_and_swap(revision, None).unwrap();
        assert!(!has_saved_account(&layout));
    }
}
