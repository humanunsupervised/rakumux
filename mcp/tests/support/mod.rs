//! Helpers for the MCP tests: a stub external API (records every request),
//! the real `orchestra-mcp` binary driven over stdio JSON-RPC, and access to
//! the workspace's other binaries for the full-stack test.
#![allow(dead_code)]

use orchestra_api::discovery::{self, ApiInfo};
use serde_json::{json, Value};
use session_core::paths::Layout;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MCP_EXE: &str = env!("CARGO_BIN_EXE_orchestra-mcp");
pub const STUB_TOKEN: &str = "stub-token-0123456789";
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// One request the stub received.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: HashMap<String, String>,
    pub body: Value,
}

impl Recorded {
    pub fn query(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

type Handler = dyn Fn(&Recorded) -> (u16, Value) + Send + Sync;

/// A stub of `orchestra-api` on 127.0.0.1: answers `/v1/health` as the
/// current process and everything else with `handler`.
pub struct StubApi {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
    /// The bearer token the stub accepts (starts as [`STUB_TOKEN`]).
    pub token: Arc<Mutex<String>>,
}

impl StubApi {
    pub fn start(handler: impl Fn(&Recorded) -> (u16, Value) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let token = Arc::new(Mutex::new(STUB_TOKEN.to_owned()));
        let handler: Arc<Handler> = Arc::new(handler);
        let recorded = requests.clone();
        let accepted = token.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let handler = handler.clone();
                let recorded = recorded.clone();
                let accepted = accepted.clone();
                std::thread::spawn(move || serve(stream, handler, recorded, accepted));
            }
        });
        Self {
            url,
            requests,
            token,
        }
    }

    /// Accept only `token` from now on (an API restart with a new token).
    pub fn rotate_token(&self, token: &str) {
        *self.token.lock().unwrap() = token.to_owned();
    }

    /// Point `api.json` in `layout` at this stub (pid: this process).
    pub fn publish(&self, layout: &Layout) {
        discovery::write(
            layout,
            &ApiInfo {
                url: self.url.clone(),
                token: self.token.lock().unwrap().clone(),
                pid: std::process::id(),
                version: "stub".into(),
                remote_urls: Vec::new(),
            },
        )
        .expect("write api.json");
    }

    /// Requests other than health checks.
    pub fn calls(&self) -> Vec<Recorded> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path != "/v1/health")
            .cloned()
            .collect()
    }

    pub fn last(&self) -> Recorded {
        self.calls().pop().expect("a request")
    }
}

fn serve(
    stream: TcpStream,
    handler: Arc<Handler>,
    recorded: Arc<Mutex<Vec<Recorded>>>,
    token: Arc<Mutex<String>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("").to_owned();
        let target = parts.next().unwrap_or("").to_owned();
        let mut headers = HashMap::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
            }
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path.to_owned(), query.to_owned()),
            None => (target.clone(), String::new()),
        };
        let request = Recorded {
            method,
            path,
            query: form_urlencoded::parse(query.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
            headers,
            body: serde_json::from_slice(&body).unwrap_or(Value::Null),
        };
        recorded.lock().unwrap().push(request.clone());
        let (status, reply) = if request.path == "/v1/health" {
            (
                200,
                json!({"status": "ok", "version": "stub", "pid": std::process::id(),
                       "daemon": {"reachable": true, "capabilities": []}}),
            )
        } else if request.header("authorization")
            != Some(&format!("Bearer {}", token.lock().unwrap()))
        {
            (401, error("unauthenticated", "invalid API token"))
        } else {
            handler(&request)
        };
        let bytes = reply.to_string();
        let response = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{bytes}",
            bytes.len()
        );
        if writer.write_all(response.as_bytes()).is_err() {
            return;
        }
    }
}

/// An error envelope.
pub fn error(code: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message}})
}

/// A session as the API returns it.
pub fn session(id: &str, name: &str, folder: &str) -> Value {
    json!({
        "id": id, "name": name, "folder": folder, "project": folder, "cwd": folder,
        "status": "live", "lifecycle": "idle", "agent": {"kind": "claude"},
        "created_by": {"kind": "user"}, "depth": 0, "children": [],
        "command": ["claude"], "startup_commands": [], "pinned": false,
        "labels": {}, "private": false, "is_self": false, "auto_named": false,
        "created_at": "2026-10-13T10:00:00Z", "last_active_at": "2026-10-13T10:05:00Z",
        "terminal": {"pid": 7, "foreground_process": "claude", "title": "t", "cols": 80,
                     "rows": 24, "controlled": false, "alternate_screen": true}
    })
}

/// A temporary Orchestra home.
pub fn temp_home() -> (tempfile::TempDir, Layout) {
    let dir = tempfile::tempdir().expect("temp dir");
    let layout = Layout::with_home(std::fs::canonicalize(dir.path()).unwrap().join("orchestra"));
    std::fs::create_dir_all(&layout.home).unwrap();
    (dir, layout)
}

/// The `orchestra-mcp` binary speaking JSON-RPC over pipes.
pub struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    /// Start with `env` (plus an isolated `ORCHESTRA_HOME`) and no session
    /// identity unless `env` sets one.
    pub fn start(layout: &Layout, env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(MCP_EXE);
        command
            .env(session_core::env::HOME, &layout.home)
            .env_remove(session_core::env::SESSION_ID)
            .env_remove(session_core::env::SESSION_TOKEN)
            .env_remove(session_core::env::SOCKET)
            .env(orchestra_api::env::API_BIN, "/nonexistent/orchestra-api")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn orchestra-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout: ChildStdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            next_id: 1,
        }
    }

    /// Start and complete the MCP handshake; returns the initialize result.
    pub fn initialized(layout: &Layout, env: &[(&str, &str)]) -> (Self, Value) {
        let mut mcp = Self::start(layout, env);
        let result = mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "orchestra-test", "version": "1"}
            }),
        );
        let result = result.get("result").cloned().expect("initialize result");
        mcp.notify("notifications/initialized", json!({}));
        (mcp, result)
    }

    pub fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("write to orchestra-mcp");
        self.stdin.flush().unwrap();
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Send a request and return the whole response (result or error).
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self
                .lines
                .recv_timeout(TIMEOUT)
                .unwrap_or_else(|_| panic!("no answer to {method}"));
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout is not JSON-RPC ({e}): {line}"));
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
        }
    }

    /// `tools/call`; returns the result object.
    pub fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.call_with_meta(name, arguments, None)
    }

    pub fn call_with_meta(&mut self, name: &str, arguments: Value, meta: Option<Value>) -> Value {
        let mut params = json!({"name": name, "arguments": arguments});
        if let Some(meta) = meta {
            params["_meta"] = meta;
        }
        let response = self.request("tools/call", params);
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("tools/call {name} failed: {response}"))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text content of a tool result.
pub fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

/// A binary of another workspace package, next to this test's binaries
/// (built on demand when the test runs without the rest of the workspace).
pub fn workspace_binary(package: &str, name: &str) -> PathBuf {
    let profile_dir = Path::new(MCP_EXE).parent().expect("target profile dir");
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
    assert!(command.status().expect("cargo build").success());
    path
}
