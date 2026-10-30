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
use crate::source::TurnSettings;
use crate::status::SqliteStatusStore;

/// Rakumux's production Clerk instance and its public "Rakumux terminal" client.
const ISSUER: &str = "https://clerk.rakumux.com";
const CLIENT_ID: &str = "PXOxFYwfkwKh2qjO";
const DEVICE_PAGE: &str = "https://accounts.rakumux.com/device";
const USERINFO: &str = "https://clerk.rakumux.com/oauth/userinfo";
const SCOPES: [&str; 4] = ["openid", "profile", "email", "offline_access"];
/// The requested scope rakumux.com's AI status API requires of this client.
const STATUS_SCOPE: &str = "profile";
/// rakumux.com: the AI status API and the sign-in-then-approve page.
const CLOUD_ORIGIN: &str = "https://rakumux.com";

/// Environment overrides, for development against another Clerk instance or origin.
pub const ISSUER_ENV: &str = "RAKUMUX_CLERK_ISSUER";
pub const CLIENT_ID_ENV: &str = "RAKUMUX_CLERK_CLIENT_ID";
pub const DEVICE_PAGE_ENV: &str = "RAKUMUX_CLERK_DEVICE_PAGE";
pub const CLOUD_ORIGIN_ENV: &str = "RAKUMUX_CLOUD_ORIGIN";
/// A PEM file of certificates trusted for [`CLOUD_ORIGIN_ENV`]'s origin besides
/// the public roots (a local rakumux.com, for development and tests); never for
/// rakumux.com itself, which only the public roots vouch for.
pub const CLOUD_CA_ENV: &str = "RAKUMUX_CLOUD_CA";

/// How long a client waits for a service it started to accept connections: it
/// restores a saved account online (provider discovery, a token refresh) first.
const START_WAIT: Duration = Duration::from_secs(20);
const START_POLL: Duration = Duration::from_millis(100);
/// The service's log, under the home's `logs/`.
pub const LOG_FILE: &str = "status-service.log";
/// How long a client waits for an older service it asked to stop.
const STOP_WAIT: Duration = Duration::from_secs(10);
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
    /// [`CLOUD_CA_ENV`]'s certificates, for an origin other than rakumux.com.
    cloud_roots: Option<Vec<u8>>,
    turn_settings: Option<TurnSettingsSource>,
}

/// Reads the turn settings (the TUI's `config.json`), again every minute.
#[derive(Clone)]
struct TurnSettingsSource(Arc<dyn Fn() -> TurnSettings + Send + Sync>);

impl std::fmt::Debug for TurnSettingsSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TurnSettingsSource")
    }
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
        .map_err(|_| ServiceError::Configuration)?
        // Clerk has no custom scopes: rakumux.com authorizes this client's `profile`.
        .with_status_scope(STATUS_SCOPE)
        .map_err(|_| ServiceError::Configuration)?;
        // The production userinfo endpoint belongs to the production issuer only.
        if issuer == ISSUER {
            auth = auth
                .with_userinfo_endpoint(USERINFO)
                .map_err(|_| ServiceError::Configuration)?;
        }
        let cloud_origin = value(CLOUD_ORIGIN_ENV, CLOUD_ORIGIN);
        let cloud_roots = match std::env::var_os(CLOUD_CA_ENV) {
            Some(path) if !is_rakumux_com(&cloud_origin) => {
                Some(std::fs::read(path).map_err(|_| ServiceError::Configuration)?)
            }
            _ => None,
        };
        Ok(Self {
            auth,
            cloud_origin,
            cloud_roots,
            turn_settings: None,
        })
    }

    /// When agents' turns get summaries: `read` gives the settings, at start and
    /// every minute after. Without it, [`TurnSettings::default`].
    pub fn with_turn_settings(
        mut self,
        read: impl Fn() -> TurnSettings + Send + Sync + 'static,
    ) -> Self {
        self.turn_settings = Some(TurnSettingsSource(Arc::new(read)));
        self
    }
}

/// Serve AI status for `home` until `stopping` returns true or a client asks it to
/// stop (both checked every second).
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
    // This machine's id and name for the feed, kept beside its other files.
    let machine = crate::feed::MachineIdentity::load(Some(layout.root().join("machine.json")));
    let cloud = CloudAIClient::new(cloud_config(&config, machine.id())?)
        .map_err(|_| ServiceError::Configuration)?;
    let website = AccountWebsite::new(&config.cloud_origin).ok();
    let store = SqliteStatusStore::open(layout.status_dir()).map_err(|_| ServiceError::Setup)?;
    let account = Arc::new(AccountService::new(
        config.auth.clone(),
        FileCredentialStore::new(layout.credentials_dir()),
    ));
    // An enrolled machine (the user's Sprite) signs in with its credential, never
    // Clerk: a new access token at once, else the saved one while it lasts.
    let tokens = enrolled_tokens(&layout, &config, machine.id())?;
    let enrolled = tokens.is_some();
    let session = match &tokens {
        Some(tokens) => crate::enrolment::sign_in(&account, tokens)
            .ok()
            .or_else(|| account.restore(tokens.as_ref()).ok().flatten()),
        None => ClerkDeviceClient::discover(config.auth.clone())
            .ok()
            .and_then(|client| account.restore(&client).ok().flatten()),
    };
    let mut service = StatusDispatchService::new(account.clone(), store, cloud);
    service
        .set_account_session(session)
        .map_err(|_| ServiceError::Setup)?;

    let api: Arc<dyn SessionApi> = Arc::new(connect_daemon(home, version)?);
    // Agents are given these files' paths: absolute, whatever the home says.
    let uploads = layout.uploads_dir();
    let uploads = std::path::absolute(&uploads).unwrap_or(uploads);
    let mut refresher = StatusRefresher::new(service)
        .following_account_switch()
        .with_chat_cursors(layout.status_dir().join("chats.json"))
        .with_reply_state(layout.status_dir().join("replies.json"))
        .with_new_session_state(layout.status_dir().join("new-sessions.json"))
        .with_titles(layout.status_dir().join("titles.json"))
        .with_titled_chats(layout.status_dir().join("titled-chats.json"))
        .with_uploads(uploads)
        .with_machine(machine);
    if let Some(tokens) = tokens {
        refresher = refresher.with_machine_tokens(tokens);
    }
    if let Some(TurnSettingsSource(read)) = config.turn_settings {
        refresher = refresher.with_turn_settings(move || read());
    }
    let runner = RefreshRunner::spawn_with_website(refresher, api, website)
        .map_err(|_| ServiceError::Setup)?;
    // A sign-out here also revokes the sign-in at Clerk; an enrolled machine's
    // credential is revoked from the machine that enrolled it, never here.
    let server = if enrolled {
        OwnerServer::start(&mut claim, runner.client(), version)
    } else {
        let revoking = account.clone();
        OwnerServer::start_revoking(
            &mut claim,
            runner.client(),
            version,
            Arc::new(move || revoking.revoke_signed_out_at_provider()),
        )
    }
    .map_err(|_| ServiceError::Setup)?;
    while !stopping() && !server.shutdown_requested() {
        thread::sleep(Duration::from_secs(1));
    }
    server.stop();
    runner.stop();
    drop(claim);
    Ok(())
}

/// The enrolled machine's tokens when this home has its credential
/// ([`crate::enrolment::CREDENTIAL_FILE`]); an unsafe or broken one stops the
/// service rather than leaving it signed out without saying why. Its requests
/// name the credential's machine id, the one it was enrolled with.
fn enrolled_tokens(
    layout: &OwnerLayout,
    config: &ServiceConfig,
    machine: &str,
) -> Result<Option<Arc<crate::enrolment::MachineTokens>>, ServiceError> {
    let path = layout.root().join(crate::enrolment::CREDENTIAL_FILE);
    let Some(credential) = crate::enrolment::MachineCredential::load(&path)
        .map_err(|_| ServiceError::Configuration)?
    else {
        return Ok(None);
    };
    if credential.machine_id() != machine {
        eprintln!("rakumux-status: this machine's credential is for another machine id");
    }
    let cloud = CloudAIClient::new(cloud_config(config, credential.machine_id())?)
        .map_err(|_| ServiceError::Configuration)?;
    Ok(Some(Arc::new(crate::enrolment::MachineTokens::new(
        cloud,
        config.auth.clone(),
        credential,
    ))))
}

/// Whether `origin` is rakumux.com's, or one of its subdomains' (an origin
/// that doesn't parse counts too: it's refused anyway).
fn is_rakumux_com(origin: &str) -> bool {
    reqwest::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_none_or(|host| {
            let host = host.trim_end_matches('.');
            host == "rakumux.com" || host.ends_with(".rakumux.com")
        })
}

/// rakumux.com's (or the overriding origin's) client settings, naming this
/// machine by `machine`.
fn cloud_config(config: &ServiceConfig, machine: &str) -> Result<CloudAIConfig, ServiceError> {
    let cloud = CloudAIConfig::new(&config.cloud_origin)
        .and_then(|cloud| cloud.with_machine_id(machine))
        .map_err(|_| ServiceError::Configuration)?;
    match &config.cloud_roots {
        Some(pem) => cloud
            .with_root_certificates(pem)
            .map_err(|_| ServiceError::Configuration),
        None => Ok(cloud),
    }
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
/// session, stdin and stdout on /dev/null, stderr appended to `logs/status-service.log`,
/// cwd `/`) when none answers, then waiting for it. A service older than `version`
/// (this build's) is asked to stop and replaced; a newer one is used as it is, so
/// clients of two builds never replace each other's.
pub fn connect_or_start(
    home: &Layout,
    program: &Path,
    client_name: &str,
    version: &str,
) -> Result<OwnerClient, ServiceError> {
    let layout = OwnerLayout::new(home);
    match OwnerClient::connect(&layout, client_name) {
        Ok(client) if !older(client.version(), version) => return Ok(client),
        Ok(client) => {
            client.shutdown().map_err(|_| ServiceError::Start)?;
            drop(client);
            wait_until_stopped(&layout, client_name)?;
        }
        Err(OwnerError::NotRunning) => {}
        Err(_) => return Err(ServiceError::Start),
    }
    start_and_connect(home, &layout, program, client_name)
}

/// Replace this home's running service with `program status-service`, whatever its
/// version (the server's restart in the TUI, `S`, so it runs that build): asked to stop,
/// waited for, and started as [`connect_or_start`] starts it. Without a running service
/// nothing starts. Returns whether one was replaced.
pub fn restart(home: &Layout, program: &Path, client_name: &str) -> Result<bool, ServiceError> {
    let layout = OwnerLayout::new(home);
    match OwnerClient::connect(&layout, client_name) {
        Ok(client) => {
            client.shutdown().map_err(|_| ServiceError::Start)?;
            drop(client);
            wait_until_stopped(&layout, client_name)?;
        }
        Err(OwnerError::NotRunning) => return Ok(false),
        Err(_) => return Err(ServiceError::Start),
    }
    start_and_connect(home, &layout, program, client_name).map(|_| true)
}

/// Start `program status-service` detached and wait for it to answer.
fn start_and_connect(
    home: &Layout,
    layout: &OwnerLayout,
    program: &Path,
    client_name: &str,
) -> Result<OwnerClient, ServiceError> {
    start_detached(home, program).map_err(|_| ServiceError::Start)?;
    let deadline = Instant::now() + START_WAIT;
    loop {
        match OwnerClient::connect(layout, client_name) {
            Ok(client) => return Ok(client),
            Err(OwnerError::NotRunning) if Instant::now() < deadline => thread::sleep(START_POLL),
            Err(_) => return Err(ServiceError::Start),
        }
    }
}

/// The service's log: static failure codes only (see `refresh`), private to the user.
fn log_file(home: &Layout) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let directory = home.home.join("logs");
    std::fs::create_dir_all(&directory)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(directory.join(LOG_FILE))
}

/// Wait for a service that was asked to stop to release this home.
fn wait_until_stopped(layout: &OwnerLayout, client_name: &str) -> Result<(), ServiceError> {
    let deadline = Instant::now() + STOP_WAIT;
    loop {
        match OwnerClient::connect(layout, client_name) {
            Err(OwnerError::NotRunning) => return Ok(()),
            _ if Instant::now() < deadline => thread::sleep(START_POLL),
            _ => return Err(ServiceError::Start),
        }
    }
}

/// Whether `running` is an earlier release than `ours` ("0.7.1" < "0.8.0"). Unreadable
/// versions are never older: nothing is replaced on a guess.
fn older(running: &str, ours: &str) -> bool {
    fn release(version: &str) -> Option<Vec<u64>> {
        let core = version.split(['-', '+']).next()?;
        core.split('.').map(|part| part.parse().ok()).collect()
    }
    match (release(running), release(ours)) {
        (Some(running), Some(ours)) => running < ours,
        _ => false,
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
        .stderr(log_file(home).map_or_else(|_| Stdio::null(), Stdio::from))
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
            .with_status_scope(STATUS_SCOPE)
            .unwrap()
            .with_userinfo_endpoint(USERINFO)
            .unwrap();
        assert_eq!(auth.status_scope(), "profile");
        assert!(CloudAIConfig::new(CLOUD_ORIGIN).is_ok());
        assert!(AccountWebsite::new(CLOUD_ORIGIN).is_ok());
    }

    #[test]
    fn extra_roots_are_never_trusted_for_rakumux_com() {
        for origin in [
            CLOUD_ORIGIN,
            "https://rakumux.com/",
            "https://RAKUMUX.com:443",
            "https://rakumux.com.",
            "https://www.rakumux.com",
            "not an origin",
        ] {
            assert!(is_rakumux_com(origin), "{origin}");
        }
        for origin in [
            "https://127.0.0.1:8443",
            "https://localhost",
            "https://rakumux.com.example",
            "https://notrakumux.com",
        ] {
            assert!(!is_rakumux_com(origin), "{origin}");
        }
    }

    #[test]
    fn only_an_earlier_release_is_older() {
        assert!(older("0.7.1", "0.8.0"));
        assert!(older("0.9.9", "0.10.0"));
        assert!(older("0.8.0-beta.1", "0.8.1"));
        assert!(!older("0.8.0", "0.8.0"));
        assert!(!older("0.8.1", "0.8.0"));
        assert!(!older("0.8.0", "0.8.0-beta.2"));
        assert!(!older("dev", "0.8.0"));
        assert!(!older("0.8.0", "dev"));
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
