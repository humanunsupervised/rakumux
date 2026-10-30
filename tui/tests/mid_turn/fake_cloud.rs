//! A local rakumux.com: the routes of `contracts/feed/` an enrolled machine's
//! account service uses for replies, over HTTPS on 127.0.0.1 (one request a
//! connection). It signs the machine in for its secret, says the feed is on,
//! keeps the machine's session list, serves the replies the test writes
//! (`poll_ms` 1000, attachments only once the session list says
//! `reply_attachments`) and their files while they're pending, and records
//! every status report, uploaded entry and request.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::certs::TestCerts;

/// The machine the cloud knows: its name, id and secret.
pub struct Machine {
    pub name: String,
    pub id: String,
    pub secret: String,
}

/// A reply the test wrote, as the cloud keeps it.
#[derive(Clone, Debug)]
pub struct Reply {
    pub id: String,
    pub seq: u64,
    pub session_id: String,
    pub session_name: String,
    pub conversation_id: String,
    pub entry: String,
    pub text: String,
    /// `{id, name, media_type, size}` each.
    pub attachments: Vec<Value>,
    pub status: String,
    pub failure: Option<String>,
    pub created_at: u64,
    pub status_at: u64,
}

impl Reply {
    fn wire(&self, machine: &str) -> Value {
        let mut reply = json!({
            "seq": self.seq,
            "id": self.id,
            "entry": self.entry,
            "thread": self.entry,
            "machine": machine,
            "session": {"id": self.session_id, "name": self.session_name},
            "agent": "claude-code",
            "conversation_id": self.conversation_id,
            "text": self.text,
            "from": "rakumux.com",
            "status": self.status,
            "created_at": self.created_at,
            "status_at": self.status_at,
        });
        if !self.attachments.is_empty() {
            reply["attachments"] = Value::Array(self.attachments.clone());
        }
        if let Some(failure) = &self.failure {
            reply["failure"] = json!(failure);
        }
        reply
    }
}

/// A status report the machine sent.
#[derive(Clone, Debug)]
pub struct Report {
    pub id: String,
    pub status: String,
    pub failure: Option<String>,
    /// Unix ms.
    pub at: u64,
}

/// What the cloud holds and saw.
#[derive(Default)]
pub struct State {
    pub replies: Vec<Reply>,
    files: HashMap<String, Vec<u8>>,
    seq: u64,
    token: String,
    pub reports: Vec<Report>,
    /// Feed entries uploaded (`POST /api/v1/feed/entries`), as sent.
    pub entries: Vec<Value>,
    /// The latest session list, as sent.
    pub sessions: Option<Value>,
    /// Replies reads, Unix ms each.
    pub reads: Vec<u64>,
    /// Attachments fetched, by id.
    pub fetches: Vec<String>,
    /// Every request: `METHOD path status`.
    pub log: Vec<String>,
}

impl State {
    /// The status reports for `id`, oldest first.
    pub fn reports_for(&self, id: &str) -> Vec<Report> {
        self.reports
            .iter()
            .filter(|report| report.id == id)
            .cloned()
            .collect()
    }

    /// Whether the latest session list says the machine fetches attachments.
    fn takes_attachments(&self) -> bool {
        self.sessions.as_ref().is_some_and(|list| {
            list["capabilities"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == "reply_attachments"))
        })
    }
}

pub struct FakeCloud {
    pub origin: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

impl FakeCloud {
    pub fn start(certs: &TestCerts, machine: Machine) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        );
        let config = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(
                certs
                    .chain
                    .iter()
                    .cloned()
                    .map(rustls::Certificate)
                    .collect(),
                rustls::PrivateKey(certs.key.clone()),
            )
            .unwrap();
        let config = Arc::new(config);
        let state = Arc::new(Mutex::new(State {
            token: "rkma.v1.e2eAccessTokenForTheMachine.e2eSignatureOfTheToken".into(),
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let served = Served {
            machine,
            state: state.clone(),
        };
        let served = Arc::new(served);
        {
            let stop = stop.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let config = config.clone();
                    let served = served.clone();
                    std::thread::spawn(move || served.connection(stream, config));
                }
            });
        }
        Self {
            origin,
            state,
            stop,
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Write a reply to `session`'s chat `conversation`, as the user does on
    /// rakumux.com, with `files` (`(name, bytes)`) attached.
    pub fn write_reply(
        &self,
        session: (&str, &str),
        conversation: &str,
        text: &str,
        files: &[(&str, Vec<u8>)],
    ) -> String {
        let mut state = self.state();
        let now = now_ms();
        let mut attachments = Vec::new();
        for (name, bytes) in files {
            let id = ulid();
            attachments.push(json!({
                "id": id, "name": name, "media_type": "image/png", "size": bytes.len(),
            }));
            state.files.insert(id, bytes.clone());
        }
        state.seq += 1;
        let reply = Reply {
            id: ulid(),
            seq: state.seq,
            session_id: session.0.into(),
            session_name: session.1.into(),
            conversation_id: conversation.into(),
            entry: ulid(),
            text: text.into(),
            attachments,
            status: "pending".into(),
            failure: None,
            created_at: now,
            status_at: now,
        };
        let id = reply.id.clone();
        state.replies.push(reply);
        id
    }
}

impl Drop for FakeCloud {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the stop.
        let _ = TcpStream::connect(self.origin.trim_start_matches("https://"));
    }
}

struct Served {
    machine: Machine,
    state: Arc<Mutex<State>>,
}

/// A request as read.
struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// An answer: status, content type, body.
struct Answer(u16, &'static str, Vec<u8>);

fn answer(status: u16, body: Value) -> Answer {
    Answer(status, "application/json", body.to_string().into_bytes())
}

fn error(status: u16, code: &str) -> Answer {
    let message = match code {
        "unauthenticated" => "Sign in to use the feed.",
        _ => "Invalid feed request.",
    };
    answer(status, json!({"error": {"code": code, "message": message}}))
}

impl Served {
    fn connection(&self, stream: TcpStream, config: Arc<rustls::ServerConfig>) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let Ok(connection) = rustls::ServerConnection::new(config) else {
            return;
        };
        let mut tls = rustls::StreamOwned::new(connection, stream);
        let Some(request) = read_request(&mut tls) else {
            return;
        };
        let Answer(status, kind, body) = self.answer(&request);
        self.state()
            .log
            .push(format!("{} {} {status}", request.method, request.path));
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            _ => "Not Found",
        };
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = tls.write_all(head.as_bytes());
        let _ = tls.write_all(&body);
        tls.conn.send_close_notify();
        let _ = tls.flush();
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn answer(&self, request: &Request) -> Answer {
        let bearer = request
            .headers
            .get("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default();
        let machine_id = request
            .headers
            .get("x-rakumux-machine-id")
            .map(String::as_str);
        if request.path == "/api/v1/machines/token" {
            if request.method != "POST"
                || bearer != self.machine.secret
                || machine_id != Some(self.machine.id.as_str())
            {
                return error(401, "unauthenticated");
            }
            let token = self.state().token.clone();
            return answer(
                200,
                json!({"version": 1, "access_token": token, "expires_at": now_ms() + 15 * 60_000}),
            );
        }
        let mut state = self.state();
        if bearer != state.token || machine_id != Some(self.machine.id.as_str()) {
            return error(401, "unauthenticated");
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/v1/features") => answer(
                200,
                json!({"version": 1, "features": {"ai_session_summaries": false, "feed": true}}),
            ),
            ("POST", "/api/v1/feed/sessions") => {
                let stored = body["sessions"].as_array().map_or(0, Vec::len);
                state.sessions = Some(body);
                answer(200, json!({"version": 1, "stored": stored}))
            }
            ("GET", "/api/v1/feed/replies") => {
                if request.query.get("machine") != Some(&self.machine.name) {
                    return error(400, "invalid_request");
                }
                let after: u64 = request
                    .query
                    .get("after")
                    .and_then(|after| after.parse().ok())
                    .unwrap_or(0);
                state.reads.push(now_ms());
                let attachments = state.takes_attachments();
                let mut replies: Vec<&Reply> = state
                    .replies
                    .iter()
                    .filter(|reply| reply.seq > after)
                    .filter(|reply| attachments || reply.attachments.is_empty())
                    .collect();
                replies.sort_by_key(|reply| reply.seq);
                let replies: Vec<Value> = replies
                    .into_iter()
                    .map(|reply| reply.wire(&self.machine.name))
                    .collect();
                answer(
                    200,
                    json!({"version": 1, "replies": replies, "latest_seq": state.seq,
                        "more": false, "poll_ms": 1000}),
                )
            }
            ("POST", "/api/v1/feed/replies/status") => {
                let id = body["id"].as_str().unwrap_or_default().to_owned();
                let status = body["status"].as_str().unwrap_or_default().to_owned();
                let failure = body["failure"].as_str().map(String::from);
                if body["machine"] != json!(self.machine.name)
                    || !matches!(status.as_str(), "delivered" | "failed")
                {
                    return error(400, "invalid_request");
                }
                let now = now_ms();
                state.reports.push(Report {
                    id: id.clone(),
                    status: status.clone(),
                    failure: failure.clone(),
                    at: now,
                });
                state.seq += 1;
                let seq = state.seq;
                let Some(reply) = state.replies.iter_mut().find(|reply| reply.id == id) else {
                    return error(404, "invalid_request");
                };
                // Only a pending reply changes, taking a new seq.
                if reply.status == "pending" {
                    reply.status = status;
                    reply.failure = failure;
                    reply.seq = seq;
                    reply.status_at = now;
                }
                let wire = reply.wire(&self.machine.name);
                answer(200, json!({"version": 1, "reply": wire}))
            }
            ("POST", "/api/v1/feed/entries") => {
                let entries = body["entries"].as_array().cloned().unwrap_or_default();
                let mut accepted = Vec::new();
                for entry in entries {
                    state.seq += 1;
                    accepted.push(json!({"id": entry["id"], "seq": state.seq}));
                    state.entries.push(entry);
                }
                answer(200, json!({"version": 1, "accepted": accepted}))
            }
            ("POST", "/api/v1/feed/messages") => {
                let count = body["messages"].as_array().map_or(0, Vec::len);
                answer(200, json!({"version": 1, "accepted": count}))
            }
            ("GET", path) if path.starts_with("/api/v1/feed/replies/attachments/") => {
                let id = &path["/api/v1/feed/replies/attachments/".len()..];
                state.fetches.push(id.to_owned());
                let pending = state.replies.iter().any(|reply| {
                    reply.status == "pending"
                        && reply.attachments.iter().any(|file| file["id"] == id)
                });
                match state.files.get(id) {
                    Some(bytes) if pending => {
                        Answer(200, "application/octet-stream", bytes.clone())
                    }
                    _ => error(404, "invalid_request"),
                }
            }
            // No pokes: replies are read at `poll_ms`.
            _ => error(404, "invalid_request"),
        }
    }
}

/// One HTTP/1.1 request: its head, then a body of its `Content-Length`.
fn read_request(stream: &mut impl Read) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let head_end = loop {
        if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break at;
        }
        if bytes.len() > 64 * 1024 {
            return None;
        }
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|length| length.parse().ok())
        .unwrap_or(0);
    let mut body = bytes[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&buffer[..read]);
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    Some(Request {
        method,
        path: path.to_owned(),
        query,
        headers,
        body,
    })
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// A new ULID, as the cloud makes its ids.
pub fn ulid() -> String {
    session_core::ids::SessionId::generate().as_str().to_owned()
}
