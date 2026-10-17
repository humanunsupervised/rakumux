//! Helpers for the API integration tests: a temporary `ORCHESTRA_HOME`, the
//! real `orchestra-api` and `orchestra-ptyd` binaries, and a raw HTTP/1.1
//! client (so tests control every header, including `Host`).
#![allow(dead_code)]

use orchestra_api::discovery::ApiInfo;
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::paths::Layout;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(15);
pub const API_EXE: &str = env!("CARGO_BIN_EXE_orchestra-api");

/// Poll `check` until it returns true or `timeout` passes.
pub fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The `orchestra-ptyd` binary of this build (built on demand when the
/// test runs without the rest of the workspace).
pub fn ptyd_binary() -> PathBuf {
    workspace_binary("orchestra-ptyd", "orchestra-ptyd")
}

/// A binary of another workspace package, next to this test's binaries.
pub fn workspace_binary(package: &str, name: &str) -> PathBuf {
    let api = Path::new(API_EXE);
    let profile_dir = api.parent().expect("target profile dir");
    let path = profile_dir.join(name);
    if path.is_file() {
        return path;
    }
    let target_dir = profile_dir.parent().expect("target dir");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tui/Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["build", "-p", package, "--bin", name, "--manifest-path"])
        .arg(&manifest)
        .arg("--target-dir")
        .arg(target_dir);
    if profile_dir.file_name().is_some_and(|n| n == "release") {
        command.arg("--release");
    }
    let status = command.status().expect("run cargo build");
    assert!(status.success(), "cargo build -p {package} failed");
    assert!(path.is_file(), "{} was not built", path.display());
    path
}

/// A temporary Orchestra home and project directories.
pub struct TestEnv {
    pub dir: tempfile::TempDir,
    pub layout: Layout,
    /// Canonical project directory.
    pub project: PathBuf,
    /// A second canonical project directory.
    pub other_project: PathBuf,
}

impl TestEnv {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical temp dir");
        let layout = Layout::with_home(root.join("orchestra"));
        let project = root.join("project");
        let other_project = root.join("other");
        std::fs::create_dir_all(project.join("sub")).expect("project dir");
        std::fs::create_dir_all(&other_project).expect("other project dir");
        Self {
            dir,
            layout,
            project,
            other_project,
        }
    }

    /// Environment for the API (and the daemon it spawns): isolated home,
    /// no session identity, a predictable shell.
    pub fn command(&self, exe: &Path) -> Command {
        let mut command = Command::new(exe);
        command
            .env(session_core::env::HOME, &self.layout.home)
            .env(session_core::env::PTYD_BIN, ptyd_binary())
            .env("HOME", self.dir.path())
            .env("SHELL", "/bin/bash")
            .env("PS1", "$ ")
            .env("HISTFILE", self.dir.path().join("history"))
            .env("ORCHESTRA_API_LOG", "debug")
            .env_remove(session_core::env::SESSION_ID)
            .env_remove(session_core::env::SESSION_TOKEN)
            .env_remove(session_core::env::SOCKET)
            .env_remove("XDG_RUNTIME_DIR");
        command
    }

    /// Turn remote access on in this home's `remote.json` and return the
    /// token. Tests always pass `--remote-listen 127.0.0.1:0` (or a loopback
    /// `listen`) so nothing listens on a real network address.
    pub fn enable_remote(&self, listen: &[&str]) -> String {
        let config = session_core::remote::RemoteConfig {
            enabled: true,
            listen: listen.iter().map(|s| s.to_string()).collect(),
            ..session_core::remote::RemoteConfig::new().expect("token")
        };
        config.save(&self.layout).expect("write remote.json");
        config.token
    }

    /// Stop the daemon of this home, if one runs.
    pub fn stop_daemon(&self) {
        let config = ClientConfig {
            layout: self.layout.clone(),
            client_name: "api-test".into(),
            version: "test".into(),
            auth: AuthMode::User,
            spawn: false,
            ptyd_path: None,
        };
        if let Ok(client) = Client::connect(config) {
            let _ = client.shutdown();
        }
    }
}

/// A running `orchestra-api` (and, once a user request was made, its
/// daemon). Dropping it stops both.
pub struct Api {
    pub env: TestEnv,
    pub child: Child,
    pub info: ApiInfo,
}

impl Api {
    pub fn start() -> Self {
        Self::start_in(TestEnv::new())
    }

    pub fn start_in(env: TestEnv) -> Self {
        Self::start_with(env, &[])
    }

    /// Start with remote access on and the remote listener on a free
    /// loopback port (`--remote-listen 127.0.0.1:0`). Returns the API and
    /// the remote token.
    pub fn start_remote() -> (Self, String) {
        let env = TestEnv::new();
        let token = env.enable_remote(&[]);
        let api = Self::start_with(env, &["--remote-listen", "127.0.0.1:0"]);
        assert_eq!(api.info.remote_urls.len(), 1, "{:?}", api.info);
        (api, token)
    }

    pub fn start_with(env: TestEnv, args: &[&str]) -> Self {
        let log = std::fs::File::create(env.dir.path().join("api-test.log")).expect("log file");
        let child = env
            .command(Path::new(API_EXE))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("spawn orchestra-api");
        let pid = child.id();
        let mut info = None;
        let ready = wait_until(TIMEOUT, || {
            info = ApiInfo::load(&env.layout)
                .ok()
                .filter(|info| info.pid == pid);
            info.is_some()
        });
        assert!(ready, "orchestra-api never wrote api.json");
        Self {
            env,
            child,
            info: info.unwrap(),
        }
    }

    pub fn addr(&self) -> String {
        self.info.url.trim_start_matches("http://").to_owned()
    }

    /// `host:port` of the (first) remote listener.
    pub fn remote_addr(&self) -> String {
        self.info
            .remote_urls
            .first()
            .expect("a remote listener")
            .trim_start_matches("http://")
            .to_owned()
    }

    /// A request to the remote listener with `token` as the bearer.
    pub fn call_remote(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> HttpReply {
        let auth = format!("Bearer {token}");
        let body = body.map(|b| b.to_string());
        let mut headers = vec![("Authorization", auth.as_str())];
        if body.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        http(&self.remote_addr(), method, path, &headers, body.as_deref())
    }

    /// A request with the API token.
    pub fn call(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> HttpReply {
        let auth = format!("Bearer {}", self.info.token);
        let body = body.map(|b| b.to_string());
        let mut headers = vec![("Authorization", auth.as_str())];
        if body.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        self.raw(method, path, &headers, body.as_deref())
    }

    /// A request as session `id` with `token`.
    pub fn call_as(
        &self,
        id: &str,
        token: &str,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> HttpReply {
        let auth = format!("Bearer {}", self.info.token);
        let body = body.map(|b| b.to_string());
        let mut headers = vec![
            ("Authorization", auth.as_str()),
            ("X-Orchestra-Session-Id", id),
            ("X-Orchestra-Session-Token", token),
        ];
        if body.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        self.raw(method, path, &headers, body.as_deref())
    }

    /// A request with exactly these headers (plus `Host` unless given).
    pub fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> HttpReply {
        http(&self.addr(), method, path, headers, body)
    }

    /// Stop with SIGTERM and wait for the exit.
    pub fn terminate(&mut self) -> Option<i32> {
        // SAFETY: plain kill(2) on our own child.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let mut status = None;
        wait_until(TIMEOUT, || {
            status = self.child.try_wait().ok().flatten();
            status.is_some()
        });
        status.and_then(|s| s.code())
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.env.stop_daemon();
        if let Ok(None) = self.child.try_wait() {
            self.terminate();
        }
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A parsed HTTP answer.
#[derive(Debug)]
pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: serde_json::Value,
}

impl HttpReply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The error code of an error envelope.
    pub fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }

    pub fn message(&self) -> &str {
        self.body["error"]["message"].as_str().unwrap_or("")
    }
}

/// One HTTP/1.1 request over a fresh connection.
pub fn http(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> HttpReply {
    let mut stream = TcpStream::connect(addr).expect("connect to the API");
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        request.push_str(&format!("Host: {addr}\r\n"));
    }
    for (key, value) in headers {
        request.push_str(&format!("{key}: {value}\r\n"));
    }
    let body = body.unwrap_or("");
    if !body.is_empty() || matches!(method, "POST" | "PATCH") {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).expect("send request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let body_bytes = &raw[split + 4..];
    let body = if body_bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(body_bytes).unwrap_or_else(|e| {
            panic!(
                "non-JSON body ({e}): {}",
                String::from_utf8_lossy(body_bytes)
            )
        })
    };
    HttpReply {
        status,
        headers,
        body,
    }
}

/// A WebSocket client connection (tokio-tungstenite over plain TCP).
pub type WebSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// Open a WebSocket to `ws://<addr><path>` with these extra headers. An
/// error answer before the upgrade comes back as `Err((status, body))`.
pub async fn ws_connect(
    addr: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<WebSocket, (u16, serde_json::Value)> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    let mut request = format!("ws://{addr}{path}")
        .into_client_request()
        .expect("a WebSocket request");
    for (name, value) in headers {
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_bytes())
                .expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to the API");
    match tokio_tungstenite::client_async(request, stream).await {
        Ok((socket, _response)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            let status = response.status().as_u16();
            let body = response
                .body()
                .as_deref()
                .and_then(|body| serde_json::from_slice(body).ok())
                .unwrap_or(serde_json::Value::Null);
            Err((status, body))
        }
        Err(error) => panic!("WebSocket handshake failed: {error}"),
    }
}

/// The next text message as JSON, skipping pings; `None` when the socket
/// closed. Panics after [`TIMEOUT`].
pub async fn next_json(socket: &mut WebSocket) -> Option<serde_json::Value> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        let message = tokio::time::timeout(TIMEOUT, socket.next())
            .await
            .expect("a WebSocket message in time")?;
        match message {
            Ok(Message::Text(text)) => {
                return Some(serde_json::from_str(&text).expect("JSON text message"))
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// Read until `check` accepts a text message (JSON) or binary output
/// (collected); returns what it accepted. Panics after [`TIMEOUT`].
pub async fn read_until(
    socket: &mut WebSocket,
    output: &mut Vec<u8>,
    mut check: impl FnMut(Option<&serde_json::Value>, &[u8]) -> bool,
) -> Option<serde_json::Value> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out; output so far:\n{}",
                    String::from_utf8_lossy(output)
                )
            });
        match message {
            Some(Ok(Message::Text(text))) => {
                let value: serde_json::Value =
                    serde_json::from_str(&text).expect("JSON text message");
                if check(Some(&value), output) {
                    return Some(value);
                }
            }
            Some(Ok(Message::Binary(bytes))) => {
                output.extend_from_slice(&bytes);
                if check(None, output) {
                    return None;
                }
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                panic!(
                    "the socket closed; output so far:\n{}",
                    String::from_utf8_lossy(output)
                )
            }
            Some(Ok(_)) => {}
        }
    }
}

/// A Tokio runtime for tests that speak WebSocket.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// `bash --norc --noprofile`, so tests never read the user's rc files.
pub fn bash() -> serde_json::Value {
    serde_json::json!(["bash", "--norc", "--noprofile"])
}

/// Percent-encode a query value.
pub fn enc(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
