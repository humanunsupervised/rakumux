//! Your own Fly.io Sprite for sessions: Rakumux sets it up with your Sprites
//! token, then reaches it as another machine (`machine_add`).
//!
//! The Sprites token never leaves this computer: it's kept in
//! `~/.rakumux/sprites.json` (0600) and used only here, for api.sprites.dev.
//! The Sprite gets a Rakumux remote token of its own, which is all the
//! daemon is given.

use crate::account::AccountAccess;
use serde_json::{json, Value};
use session_core::api::ApiError;
use session_core::client::SessionApi;
use session_core::ids::MachineId;
use session_core::model::MachineView;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const API: &str = "https://api.sprites.dev";
/// The Sprite Rakumux makes when none is named.
pub const DEFAULT_SPRITE: &str = "my-sprite";
/// What the `sprite` CLI keeps its tokens with on macOS.
const SECURITY: &str = "/usr/bin/security";
/// Where the Sprite's `rakumux-api` serves, behind its URL.
const PORT: u16 = 8080;
const TIMEOUT: Duration = Duration::from_secs(120);

/// What `sprites.json` keeps.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Saved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The Sprite sessions go to, once set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<String>,
    /// Its machine id in the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

fn saved_path(home: &Path) -> PathBuf {
    home.join("sprites.json")
}

/// What `sprites.json` in the Rakumux home `home` holds.
pub fn load(home: &Path) -> Saved {
    std::fs::read(saved_path(home))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(home: &Path, saved: &Saved) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(saved).map_err(|e| e.to_string())?;
    write_private(&saved_path(home), &bytes).map_err(|e| format!("saving the Sprites token: {e}"))
}

/// The `sprite` CLI's folder, `~/.sprites`.
pub fn cli_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".sprites"))
}

/// A Sprites token found on this computer, and where: `SPRITES_TOKEN`,
/// the one Rakumux saved in `home`, or the `sprite` CLI's in `cli` (in the
/// keychain on macOS, else when it keeps tokens in files: computers without
/// a system keyring).
pub fn found_token(home: &Path, cli: Option<&Path>) -> Option<(String, &'static str)> {
    if let Some(token) = std::env::var("SPRITES_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        return Some((token.trim().to_string(), "SPRITES_TOKEN"));
    }
    if let Some(token) = load(home).token {
        return Some((token, "Rakumux"));
    }
    cli_token(cli?).map(|token| (token, "the sprite CLI"))
}

/// The `sprite` CLI's token for its current organization: from the
/// keychain on macOS, else its files.
fn cli_token(home: &Path) -> Option<String> {
    if cfg!(target_os = "macos") {
        if let Some(token) = keychain_token(home, Path::new(SECURITY)) {
            return Some(token);
        }
    }
    file_token(home)
}

/// The `sprite` CLI's token for its current organization in the macOS
/// keychain, where it keeps them with `security` (zalando/go-keyring): the
/// generic password of service `sprites-cli:<user>` and account the
/// organization's `keyring_key` in that user's config. Read with the same
/// `security`, which the item lets in without asking.
fn keychain_token(home: &Path, security: &Path) -> Option<String> {
    let config = read_json(&home.join("sprites.json"))?;
    let user = config["current_user"].as_str()?;
    let url = config["current_selection"]["url"].as_str()?;
    let org = config["current_selection"]["org"].as_str()?;
    let key = config["users"]
        .as_array()
        .and_then(|users| users.iter().find(|u| u["id"].as_str() == Some(user)))
        .and_then(|u| u["config_path"].as_str())
        .and_then(|path| read_json(Path::new(path)))
        .and_then(|c| {
            c["urls"][url]["orgs"][org]["keyring_key"]
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| format!("sprites:org:{url}:{org}"));
    let output = std::process::Command::new(security)
        .args([
            "find-generic-password",
            "-s",
            &format!("sprites-cli:{user}"),
        ])
        .args(["-a", &key, "-w"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    keychain_password(String::from_utf8(output.stdout).ok()?.trim())
}

/// A password as go-keyring stores it: base64 (or, from older versions,
/// hex) after its prefix.
fn keychain_password(text: &str) -> Option<String> {
    use base64::Engine;
    let bytes = if let Some(rest) = text.strip_prefix("go-keyring-base64:") {
        base64::engine::general_purpose::STANDARD
            .decode(rest)
            .ok()?
    } else if let Some(rest) = text.strip_prefix("go-keyring-encoded:") {
        (0..rest.len())
            .step_by(2)
            .map(|i| {
                rest.get(i..i + 2)
                    .and_then(|b| u8::from_str_radix(b, 16).ok())
            })
            .collect::<Option<Vec<u8>>>()?
    } else {
        text.as_bytes().to_vec()
    };
    let token = String::from_utf8(bytes).ok()?.trim().to_string();
    Some(token).filter(|t| !t.is_empty())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The `sprite` CLI's token for its current organization, from
/// `<cli>/keyring/sprites-cli-*/sprites-org-https-/api.sprites.dev-<org>`.
fn file_token(home: &Path) -> Option<String> {
    let current = std::fs::read(home.join("sprites.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| v["current_selection"]["org"].as_str().map(str::to_owned));
    let keyring = std::fs::read_dir(home.join("keyring")).ok()?;
    let mut tokens = Vec::new();
    for dir in keyring.flatten() {
        let orgs = dir.path().join("sprites-org-https-");
        for file in std::fs::read_dir(orgs).into_iter().flatten().flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(org) = name.strip_prefix("api.sprites.dev-") else {
                continue;
            };
            if let Ok(token) = std::fs::read_to_string(file.path()) {
                tokens.push((org.to_string(), token.trim().to_string()));
            }
        }
    }
    let pick = tokens
        .iter()
        .find(|(org, _)| current.as_deref() == Some(org.as_str()))
        .or_else(|| tokens.first())?;
    Some(pick.1.clone()).filter(|t| !t.is_empty())
}

/// api.sprites.dev with one token.
pub struct Sprites {
    http: reqwest::blocking::Client,
    token: String,
}

/// A Sprite as the API describes it.
#[derive(Debug, Clone)]
pub struct Sprite {
    pub url: String,
}

/// One step of [`set_up`], for progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Create,
    Install,
    Start,
    Connect,
    /// Enrol it for the feed: its own sign-in, made by this machine's.
    Feed,
}

impl Step {
    pub fn label(self) -> &'static str {
        match self {
            Step::Create => "Create the Sprite",
            Step::Install => "Install Rakumux",
            Step::Start => "Start it",
            Step::Connect => "Connect",
            Step::Feed => "Connect the feed",
        }
    }
}

/// A Sprite ready for sessions: its URL and the Rakumux token it takes.
#[derive(Debug, Clone)]
pub struct Ready {
    pub sprite: String,
    pub url: String,
    pub token: String,
    /// Its home directory, and where Rakumux is installed there.
    pub home: String,
    pub libexec: String,
}

/// The feed from the Sprite: the Rakumux account here enrols it, so its
/// agents' replies reach the account's feed; `replies`: whether replies
/// written on rakumux.com or another machine are typed into its agents
/// (its `sessions.remote_replies`).
pub struct Feed<'a> {
    pub account: &'a dyn AccountAccess,
    pub replies: bool,
}

/// A connected Sprite, and what became of its feed.
#[derive(Debug, Clone)]
pub struct Connected {
    pub machine: MachineView,
    pub feed: FeedSetUp,
}

/// What connecting did for the Sprite's feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedSetUp {
    /// Enrolled: its account service runs there with a credential of its own.
    On,
    /// Signed out here: nothing to enrol it with.
    SignedOut,
    /// Not asked for.
    Skipped,
    /// Enrolling it failed (sessions there work all the same); connecting
    /// again tries again.
    Failed(String),
}

impl Sprites {
    pub fn new(token: &str) -> Result<Self, String> {
        let token = token.trim();
        if token.is_empty() || token.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err("that isn't a Sprites token".into());
        }
        let http = reqwest::blocking::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .user_agent(concat!("rakumux/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            token: token.to_string(),
        })
    }

    fn send(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response, String> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .map_err(|e| format!("api.sprites.dev: {}", without_url(&e)))?;
        match response.status().as_u16() {
            401 | 403 => {
                Err("Sprites refused the token: make a new one at sprites.dev/account".into())
            }
            402 => Err("Sprites needs a card on your Fly.io organization first".into()),
            _ => Ok(response),
        }
    }

    fn ok_json(response: reqwest::blocking::Response) -> Result<Value, String> {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "api.sprites.dev answered {status}: {}",
                first_line(&body)
            ));
        }
        serde_json::from_str(&body).map_err(|e| format!("api.sprites.dev: {e}"))
    }

    /// Check the token.
    pub fn check(&self) -> Result<(), String> {
        Self::ok_json(self.send(self.http.get(format!("{API}/v1/sprites")))?).map(|_| ())
    }

    pub fn sprite(&self, name: &str) -> Result<Option<Sprite>, String> {
        let response = self.send(self.http.get(format!("{API}/v1/sprites/{name}")))?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        sprite_of(&Self::ok_json(response)?).map(Some)
    }

    fn create(&self, name: &str) -> Result<Sprite, String> {
        let body = json!({"name": name, "url_settings": {"auth": "public"}});
        sprite_of(&Self::ok_json(
            self.send(self.http.post(format!("{API}/v1/sprites")).json(&body))?,
        )?)
    }

    fn make_public(&self, name: &str) -> Result<(), String> {
        let body = json!({"url_settings": {"auth": "public"}});
        Self::ok_json(
            self.send(
                self.http
                    .put(format!("{API}/v1/sprites/{name}"))
                    .json(&body),
            )?,
        )
        .map(|_| ())
    }

    /// Run a bash script there: its exit code and output.
    fn exec(&self, name: &str, script: &str) -> Result<(i32, String), String> {
        let url = format!("{API}/v1/sprites/{name}/exec");
        let request = self
            .http
            .post(url)
            .query(&[("cmd", "bash"), ("cmd", "-lc"), ("cmd", script)])
            .body(Vec::new());
        let response = self.send(request)?;
        let status = response.status();
        let bytes = response.bytes().map_err(|e| without_url(&e))?;
        if !status.is_success() {
            return Err(format!("running a command there: {status}"));
        }
        Ok(parse_exec(&bytes))
    }

    fn write_file(&self, name: &str, path: &str, bytes: Vec<u8>, mode: &str) -> Result<(), String> {
        let request = self
            .http
            .put(format!("{API}/v1/sprites/{name}/fs/write"))
            .query(&[
                ("path", path),
                ("workingDir", "/"),
                ("mode", mode),
                ("mkdir", "true"),
            ])
            .body(bytes);
        Self::ok_json(self.send(request)?).map(|_| ())
    }

    fn read_file(&self, name: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
        let request = self
            .http
            .get(format!("{API}/v1/sprites/{name}/fs/read"))
            .query(&[("path", path), ("workingDir", "/")]);
        let response = self.send(request)?;
        if !response.status().is_success() {
            return Ok(None);
        }
        response
            .bytes()
            .map(|b| Some(b.to_vec()))
            .map_err(|e| without_url(&e))
    }

    fn put_service(&self, name: &str, service: &str, body: Value) -> Result<(), String> {
        let request = self
            .http
            .put(format!("{API}/v1/sprites/{name}/services/{service}"))
            .query(&[("duration", "1s")])
            .json(&body);
        let response = self.send(request)?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(format!(
                "starting {service}: {status}: {}",
                first_line(&response.text().unwrap_or_default())
            ));
        }
        Ok(())
    }

    fn restart_service(&self, name: &str, service: &str) -> Result<(), String> {
        let request = self.http.post(format!(
            "{API}/v1/sprites/{name}/services/{service}/restart"
        ));
        let response = self.send(request)?;
        if !response.status().is_success() {
            return Err(format!("restarting {service}: {}", response.status()));
        }
        Ok(())
    }
}

/// Make `name` ready for sessions, creating it when missing: Rakumux
/// installed (this build's own binaries when it isn't a release), its daemon
/// and API running as Sprite services behind the Sprite's public URL, and a
/// Rakumux token for it. Safe to run again: each step checks first.
pub fn set_up(
    sprites: &Sprites,
    name: &str,
    progress: &mut dyn FnMut(Step),
) -> Result<Ready, String> {
    progress(Step::Create);
    let sprite = match sprites.sprite(name)? {
        Some(sprite) => {
            sprites.make_public(name)?;
            sprite
        }
        None => sprites.create(name)?,
    };

    progress(Step::Install);
    // `build.rs` always sets RAKUMUX_VERSION; a release's differs from the
    // package version every local build has.
    let version = option_env!("RAKUMUX_VERSION").filter(|v| *v != env!("CARGO_PKG_VERSION"));
    let pin = version
        .map(|v| format!("RAKUMUX_VERSION={v} "))
        .unwrap_or_default();
    let (code, output) = sprites.exec(
        name,
        &format!(
            "set -e; curl -fsSL https://rakumux.com/install.sh | {pin}PREFIX=\"$HOME/.local\" bash >/dev/null 2>&1; \
             pkill -x rakumux-api >/dev/null 2>&1 || true; echo \"HOME=$HOME\""
        ),
    )?;
    let home = output
        .lines()
        .find_map(|line| line.strip_prefix("HOME="))
        .map(str::trim)
        .filter(|h| h.starts_with('/'))
        .ok_or_else(|| {
            format!(
                "installing Rakumux there failed ({code}): {}",
                last_lines(&output)
            )
        })?
        .to_string();
    let libexec = format!("{home}/.local/libexec/rakumux");
    if version.is_none() {
        upload_own_binaries(sprites, name, &libexec)?;
    }
    let token = remote_token(sprites, name, &home)?;

    progress(Step::Start);
    sprites.put_service(
        name,
        "rakumux-ptyd",
        json!({"cmd": format!("{libexec}/rakumux-ptyd"), "args": ["serve"], "needs": [], "dir": home}),
    )?;
    sprites.put_service(
        name,
        "rakumux-api",
        json!({"cmd": format!("{libexec}/rakumux-api"), "args": [], "needs": ["rakumux-ptyd"],
               "http_port": PORT, "dir": home}),
    )?;
    // A daemon already running there takes the new binary in place, so its
    // sessions keep running; the API restarts for its own.
    let (code, output) = sprites.exec(
        name,
        "if pgrep -x rakumux-ptyd >/dev/null; then \"$HOME/.local/bin/rakumux\" daemon restart --yes; fi",
    )?;
    if code != 0 {
        return Err(format!("restarting Rakumux there: {}", last_lines(&output)));
    }
    sprites.restart_service(name, "rakumux-api")?;

    progress(Step::Connect);
    wait_healthy(&sprite.url)?;
    Ok(Ready {
        sprite: name.to_string(),
        url: sprite.url,
        token,
        home,
        libexec,
    })
}

/// Enrol the Sprite for the feed with the account here and start its own
/// account service: its credential (0600, in a 0700 directory, as the service
/// requires) and `machine.json` (the daemon's machine id for it, so the feed
/// and this daemon know it by one id, and its name), whether it types replies
/// from elsewhere, and a `rakumux-status` service (restarted, so a new
/// credential is taken). Enrolling again revokes the earlier credential.
fn connect_feed(
    sprites: &Sprites,
    ready: &Ready,
    machine: &MachineView,
    feed: &Feed<'_>,
) -> Result<FeedSetUp, String> {
    let name = &ready.sprite;
    let id = machine.id.to_string();
    let wake = ready.url.trim_end_matches('/');
    let Some(credential) = feed.account.enrol_machine(&id, name, wake)? else {
        return Ok(FeedSetUp::SignedOut);
    };
    let dir = format!("{}/.rakumux/sensagent", ready.home);
    let (code, output) = sprites.exec(
        name,
        &format!("set -e; mkdir -p '{dir}'; chmod 700 '{dir}'"),
    )?;
    if code != 0 {
        return Err(format!("preparing the feed there: {}", first_line(&output)));
    }
    sprites.write_file(
        name,
        &format!("{dir}/machine-credential.json"),
        credential.into_bytes(),
        "0600",
    )?;
    let identity = json!({"version": 1, "id": id, "base": name});
    sprites.write_file(
        name,
        &format!("{dir}/machine.json"),
        identity.to_string().into_bytes(),
        "0600",
    )?;
    set_remote_replies(sprites, name, &ready.home, feed.replies)?;
    sprites.put_service(
        name,
        "rakumux-status",
        json!({"cmd": format!("{}/rakumux", ready.libexec), "args": ["status-service"],
               "needs": ["rakumux-ptyd"], "dir": ready.home}),
    )?;
    sprites.restart_service(name, "rakumux-status")?;
    Ok(FeedSetUp::On)
}

/// Set the Sprite's `sessions.remote_replies` in its `config.json`, keeping
/// everything else there; a file that isn't a JSON object is left alone.
fn set_remote_replies(sprites: &Sprites, name: &str, home: &str, on: bool) -> Result<(), String> {
    let path = format!("{home}/.rakumux/config.json");
    let mut config = match sprites.read_file(name, &path)? {
        Some(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .filter(Value::is_object)
            .ok_or("the Sprite's Rakumux settings aren't readable")?,
        None => json!({}),
    };
    with_remote_replies(&mut config, on);
    sprites.write_file(name, &path, config.to_string().into_bytes(), "0600")
}

/// `config` (a JSON object) with `sessions.remote_replies` set to `on`.
fn with_remote_replies(config: &mut Value, on: bool) {
    let Some(object) = config.as_object_mut() else {
        return;
    };
    let sessions = object.entry("sessions").or_insert_with(|| json!({}));
    if !sessions.is_object() {
        *sessions = json!({});
    }
    if let Some(sessions) = sessions.as_object_mut() {
        sessions.insert("remote_replies".into(), Value::Bool(on));
    }
}

/// Set the Sprite up with `token` and have the daemon reach it, saving
/// both: `sprite` names it (else the one used before, else `rakumux`).
/// With `feed`, the account here enrols it for the feed. `progress` hears
/// each step.
pub fn connect(
    api: &dyn SessionApi,
    home: &Path,
    token: &str,
    sprite: Option<&str>,
    feed: Option<Feed<'_>>,
    progress: &mut dyn FnMut(Step),
) -> Result<Connected, String> {
    let client = Sprites::new(token)?;
    client.check()?;
    let mut saved = load(home);
    saved.token = Some(token.trim().to_string());
    save(home, &saved)?;
    let name = sprite
        .map(str::to_owned)
        .or_else(|| saved.sprite.clone())
        .unwrap_or_else(|| DEFAULT_SPRITE.to_string());
    let ready = set_up(&client, &name, progress)?;
    let known = api.machines().map_err(|e| e.message)?;
    let machine = match known.into_iter().find(|m| m.url == ready.url) {
        Some(machine) => machine,
        None => api
            .add_machine(&ready.sprite, &ready.url, &ready.token)
            .map_err(|e| e.message)?,
    };
    saved.sprite = Some(ready.sprite.clone());
    saved.machine = Some(machine.id.to_string());
    save(home, &saved)?;
    let feed = match feed {
        Some(feed) => {
            progress(Step::Feed);
            connect_feed(&client, &ready, &machine, &feed).unwrap_or_else(FeedSetUp::Failed)
        }
        None => FeedSetUp::Skipped,
    };
    Ok(Connected { machine, feed })
}

/// Forget the Sprite here: its credential for the feed revoked (when the
/// account here can), the daemon no longer reaching it, and `sprites.json`
/// emptied. The Sprite and its files stay on Fly.io. Whether a credential was
/// revoked.
pub fn disconnect(
    api: &dyn SessionApi,
    home: &Path,
    account: Option<&dyn AccountAccess>,
) -> Result<bool, String> {
    let saved = load(home);
    let mut revoked = false;
    if let (Some(account), Some(machine)) = (account, saved.machine.as_deref()) {
        account.revoke_machine(machine)?;
        revoked = true;
    }
    if let Some(machine) = connected(api, home).map_err(|e| e.message)? {
        api.remove_machine(&machine.id).map_err(|e| e.message)?;
    }
    save(home, &Saved::default())?;
    Ok(revoked)
}

/// The machine of the Sprite set up here, while the daemon still has it.
pub fn connected(api: &dyn SessionApi, home: &Path) -> Result<Option<MachineView>, ApiError> {
    let Some(id) = load(home).machine else {
        return Ok(None);
    };
    let id = MachineId::from_string(id);
    Ok(api.machines()?.into_iter().find(|m| m.id == id))
}

/// A development build: put its own daemon and API on the Sprite (they need
/// to match this one), next to the release `install.sh` put there.
fn upload_own_binaries(sprites: &Sprites, name: &str, libexec: &str) -> Result<(), String> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(
            "this is a development build of Rakumux: it can set up a Sprite from Linux x64 only, \
             or use a release"
                .into(),
        );
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    // The TUI binary is `rakumux` there, as a release names it; it runs the
    // account service.
    for (binary, installed) in [
        ("rakumux-ptyd", "rakumux-ptyd"),
        ("rakumux-api", "rakumux-api"),
        ("tui", "rakumux"),
    ] {
        let path = dir.join(binary);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Beside the running one, then renamed over it.
        let staged = format!("{libexec}/.{installed}.new");
        sprites.write_file(name, &staged, bytes, "0755")?;
        let (code, output) =
            sprites.exec(name, &format!("mv -f '{staged}' '{libexec}/{installed}'"))?;
        if code != 0 {
            return Err(format!("installing {binary}: {}", first_line(&output)));
        }
    }
    Ok(())
}

/// The Sprite's Rakumux remote token: the one its `remote.json` has when it
/// serves on the Sprite's URL already, else a new one written there.
fn remote_token(sprites: &Sprites, name: &str, home: &str) -> Result<String, String> {
    let path = format!("{home}/.rakumux/remote.json");
    if let Some(bytes) = sprites.read_file(name, &path)? {
        if let Ok(config) = serde_json::from_slice::<Value>(&bytes) {
            let serves = config["listen"].as_array().is_some_and(|l| {
                l.iter()
                    .any(|a| a.as_str() == Some(&format!("0.0.0.0:{PORT}")))
            });
            if let (true, true, Some(token)) = (
                config["enabled"].as_bool() == Some(true),
                serves,
                config["token"].as_str().filter(|t| t.len() >= 20),
            ) {
                return Ok(token.to_string());
            }
        }
    }
    let token = session_core::remote::generate_token().map_err(|e| e.to_string())?;
    let config = json!({"enabled": true, "port": 7337, "token": token, "listen": [format!("0.0.0.0:{PORT}")]});
    let bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
    sprites.write_file(name, &path, bytes, "0600")?;
    Ok(token)
}

/// Until the Sprite's API answers with its daemon reachable.
fn wait_healthy(url: &str) -> Result<(), String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = String::new();
    while Instant::now() < deadline {
        match http.get(format!("{url}/v1/health")).send() {
            Ok(response) if response.status().is_success() => {
                let body: Value = response.json().unwrap_or(Value::Null);
                if body["daemon"]["reachable"].as_bool() == Some(true) {
                    return Ok(());
                }
                last = "its daemon isn't running yet".into();
            }
            Ok(response) => last = format!("HTTP {}", response.status()),
            Err(e) => last = without_url(&e),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(format!("the Sprite's Rakumux didn't come up: {last}"))
}

fn sprite_of(value: &Value) -> Result<Sprite, String> {
    let url = value["url"]
        .as_str()
        .filter(|u| u.starts_with("https://"))
        .ok_or("api.sprites.dev gave the Sprite no URL")?
        .trim_end_matches('/')
        .to_string();
    Ok(Sprite { url })
}

/// The exec stream: chunks tagged `1` (stdout) or `2` (stderr), then `3` and
/// the exit code.
fn parse_exec(bytes: &[u8]) -> (i32, String) {
    let (body, code) = match bytes {
        [rest @ .., 3, code] => (rest, i32::from(*code)),
        _ => (bytes, -1),
    };
    let text: Vec<u8> = body
        .iter()
        .copied()
        .filter(|b| *b != 1 && *b != 2)
        .collect();
    (code, String::from_utf8_lossy(&text).into_owned())
}

/// The end of a command's output, without the login shell's noise.
fn last_lines(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.contains("line editing not enabled"))
        .collect();
    lines[lines.len().saturating_sub(3)..]
        .join(" | ")
        .chars()
        .take(400)
        .collect()
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

/// reqwest's errors name the URL, query and all: keep them out of messages.
fn without_url(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    if let Some(url) = error.url() {
        text = text.replace(url.as_str(), "api.sprites.dev");
    }
    text
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_streams_give_their_exit_code_and_output() {
        assert_eq!(
            parse_exec(b"\x02err\n\x01out\n\x03\x03"),
            (3, "err\nout\n".into())
        );
        assert_eq!(
            parse_exec(b"\x01HOME=/home/sprite\n\x03\x00"),
            (0, "HOME=/home/sprite\n".into())
        );
        assert_eq!(parse_exec(b""), (-1, String::new()));
    }

    #[test]
    fn replies_from_elsewhere_are_set_without_touching_other_settings() {
        let mut config = json!({"sessions": {"icons": "nerd"}, "openai_model": "x"});
        with_remote_replies(&mut config, true);
        assert_eq!(
            config,
            json!({"sessions": {"icons": "nerd", "remote_replies": true}, "openai_model": "x"})
        );
        with_remote_replies(&mut config, false);
        assert_eq!(config["sessions"]["remote_replies"], json!(false));
        let mut empty = json!({});
        with_remote_replies(&mut empty, true);
        assert_eq!(empty, json!({"sessions": {"remote_replies": true}}));
        let mut odd = json!({"sessions": 3});
        with_remote_replies(&mut odd, true);
        assert_eq!(odd, json!({"sessions": {"remote_replies": true}}));
    }

    #[test]
    fn the_sprite_clis_token_is_read_from_the_macos_keychain() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join(".sprites");
        std::fs::create_dir_all(cli.join("users")).unwrap();
        let user_config = cli.join("users/u1-abc.json");
        let config = json!({
            "current_user": "u1",
            "current_selection": {"url": "https://api.sprites.dev", "org": "me"},
            "users": [{"id": "u1", "config_path": user_config}],
        });
        std::fs::write(cli.join("sprites.json"), config.to_string()).unwrap();
        let key = "sprites:org:https://api.sprites.dev:me";
        let orgs =
            json!({"urls": {"https://api.sprites.dev": {"orgs": {"me": {"keyring_key": key}}}}});
        std::fs::write(&user_config, orgs.to_string()).unwrap();
        let asked = dir.path().join("asked");
        let security = dir.path().join("security");
        let stored = base64::engine::general_purpose::STANDARD.encode("me/1/k/secret");
        std::fs::write(
            &security,
            format!(
                "#!/bin/bash\nprintf '%s\\n' \"$@\" > '{}'\necho go-keyring-base64:{stored}\n",
                asked.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &security,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();

        assert_eq!(
            keychain_token(&cli, &security).as_deref(),
            Some("me/1/k/secret")
        );
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "find-generic-password",
                "-s",
                "sprites-cli:u1",
                "-a",
                key,
                "-w"
            ]
        );
        std::fs::write(&security, "#!/bin/bash\nexit 44\n").unwrap();
        assert_eq!(keychain_token(&cli, &security), None, "not in the keychain");
        assert_eq!(
            keychain_password("go-keyring-encoded:6d652f31").as_deref(),
            Some("me/1")
        );
        assert_eq!(keychain_password("plain").as_deref(), Some("plain"));
        assert_eq!(keychain_password("go-keyring-encoded:6d6"), None);
    }

    #[test]
    fn tokens_are_saved_privately() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let saved = Saved {
            token: Some("org/1/abc".into()),
            sprite: Some("rakumux".into()),
            machine: None,
        };
        save(home, &saved).unwrap();
        assert_eq!(load(home), saved);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(saved_path(home))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
