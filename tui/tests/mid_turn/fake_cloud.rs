//! A local rakumux.com: the routes of `contracts/feed/` an enrolled machine's
//! account service uses for replies, over HTTPS on 127.0.0.1 (one request a
//! connection). It signs the machine in for its secret, says the feed is on,
//! keeps the machine's session list, serves the replies the test writes
//! (`poll_ms` 1000, attachments only once the session list says
//! `reply_attachments`, answers to agents' questions only to a reader that
//! asks for them and whose list says `question_answers`) and their files
//! while they're pending, and records every status report, uploaded entry,
//! chat message and request. With `summaries` on it says AI session titles
//! are on too and answers each title request with `title`. Once a device is
//! paired ([`FakeCloud::pair`]), it signs what it serves with that device's
//! key, as a paired browser or phone does: machines carry out nothing
//! unsigned.

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
    /// An answer to the agent's question (`questionAnswer`): a session
    /// message, without an entry.
    pub answer: Option<Value>,
    /// What a paired device signed for it (`signedCommand`), passed on as written.
    pub signed: Option<Value>,
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
        if let (Some(answer), Some(fields)) = (&self.answer, reply.as_object_mut()) {
            fields.remove("entry");
            fields.remove("thread");
            fields.insert("answer".into(), answer.clone());
        }
        if let Some(failure) = &self.failure {
            reply["failure"] = json!(failure);
        }
        if let Some(signed) = &self.signed {
            reply["signed"] = signed.clone();
        }
        reply
    }
}

/// A device's P-256 key, as the test holds it: signs commands itself, as the
/// phone app and a paired browser do.
pub struct Phone {
    key: ring::signature::EcdsaKeyPair,
    pub key_id: String,
}

impl Phone {
    pub fn new() -> Self {
        use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        Self {
            key,
            key_id: b64(ulid().as_bytes()),
        }
    }

    #[allow(dead_code)]
    pub fn proposed(&self, name: &str) -> rakumux_sensagent::signing::pairing::ProposedDevice {
        rakumux_sensagent::signing::pairing::ProposedDevice {
            key_id: self.key_id.clone(),
            format: rakumux_sensagent::signing::KeyFormat::P256,
            public_key: self.public_key(),
            name: name.into(),
        }
    }

    fn public_key(&self) -> String {
        use ring::signature::KeyPair;
        b64(self.key.public_key().as_ref())
    }

    /// `fields` as a command of `kind` for request `id` to machine
    /// `machine`, signed now (`signedCommand`).
    pub fn sign(&self, kind: &str, machine: &str, id: &str, fields: Value) -> Value {
        let now = now_ms();
        let mut command = json!({"v": 1, "kind": kind, "machine": machine, "id": id,
            "at": now, "expires": now + 10 * 60_000});
        if let (Some(command), Some(fields)) = (command.as_object_mut(), fields.as_object()) {
            command.extend(fields.clone());
        }
        let bytes = serde_json::to_vec(&command).unwrap();
        let signature = self
            .key
            .sign(&ring::rand::SystemRandom::new(), &bytes)
            .unwrap();
        json!({"device": self.key_id, "command": b64(&bytes), "signature": b64(signature.as_ref())})
    }
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Each row of a replies read that has no `signed`, signed by `phone` for
/// `machine` with the fields its kind's command says (contract, Signed
/// commands); a reply's files by name and SHA-256 of what `files` serves.
fn sign_read(phone: &Phone, machine: &str, files: &HashMap<String, Vec<u8>>, read: &mut Value) {
    let pick = |row: &Value, keys: &[&str]| {
        let mut fields = serde_json::Map::new();
        for key in keys {
            if let Some(value) = row.get(*key).filter(|value| !value.is_null()) {
                fields.insert((*key).into(), value.clone());
            }
        }
        fields
    };
    let mut sign = |list: &str, kind: &str, keys: &[&str]| {
        let Some(Value::Array(rows)) = read.get_mut(list) else {
            return;
        };
        for row in rows.iter_mut().filter(|row| row.get("signed").is_none()) {
            let mut fields = pick(row, keys);
            if list == "replies" {
                fields.insert("session".into(), row["session"]["id"].clone());
                if let Some(Value::Array(attached)) = row.get("attachments") {
                    let named: Vec<Value> = attached
                        .iter()
                        .map(|file| {
                            let bytes = file["id"]
                                .as_str()
                                .and_then(|id| files.get(id))
                                .cloned()
                                .unwrap_or_default();
                            let hash = ring::digest::digest(&ring::digest::SHA256, &bytes);
                            let hex: String =
                                hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
                            json!({"name": file["name"], "sha256": hex})
                        })
                        .collect();
                    fields.insert("attachments".into(), Value::Array(named));
                }
            }
            let id = row["id"].as_str().unwrap_or_default().to_owned();
            row["signed"] = phone.sign(kind, machine, &id, Value::Object(fields));
        }
    };
    sign("replies", "reply", &["entry", "text", "answer", "choice"]);
    sign("session_deletions", "delete_session", &["session"]);
    sign("session_renames", "rename_session", &["session", "name"]);
    sign("session_moves", "move_session", &["session", "workspace"]);
    sign(
        "workspace_changes",
        "workspace_change",
        &["workspace", "action", "name"],
    );
    sign(
        "heartbeat_actions",
        "heartbeat_action",
        &["heartbeat", "action"],
    );
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
    /// Chat messages uploaded (`POST /api/v1/feed/messages`), as sent.
    pub messages: Vec<Value>,
    /// The latest session list, as sent.
    pub sessions: Option<Value>,
    /// Replies reads, Unix ms each.
    pub reads: Vec<u64>,
    /// Attachments fetched, by id.
    pub fetches: Vec<String>,
    /// Every request: `METHOD path status`.
    pub log: Vec<String>,
    /// The account's AI session titles are on (`ai_session_summaries`).
    pub summaries: bool,
    /// The title each AI session-status request is answered with.
    pub title: String,
    /// The level the features read asks this machine's logs from (`logs`); none: not asked.
    pub logs_level: Option<&'static str>,
    /// Log uploads (`POST /api/v1/feed/logs`), as sent.
    pub logs: Vec<Value>,
    /// AI session-status requests (`POST /api/v1/ai/session-status`): when
    /// (Unix ms), and the body as sent.
    pub titles: Vec<(u64, Value)>,
    /// How many of the next status reports meet a security checkpoint.
    pub checkpoints: usize,
    /// Sessions asked to be deleted (`sessionDeletion`), as stored.
    pub deletions: Vec<Value>,
    /// Deletion status reports, as sent.
    pub deletion_reports: Vec<Value>,
    /// The machine's open pairing (`pairing-open`), as sent.
    pub pairing: Option<Value>,
    /// The proposal a device sent for it, while pending.
    pub proposal: Option<Value>,
    /// Pairing reports (`pairing-status`), as sent.
    pub pairing_reports: Vec<Value>,
    /// Sessions asked to be renamed (`sessionRename`), as stored.
    pub renames: Vec<Value>,
    /// Rename status reports, as sent.
    pub rename_reports: Vec<Value>,
    /// Sessions asked to move between workspaces (`sessionMove`), as stored.
    pub moves: Vec<Value>,
    /// Move status reports, as sent.
    pub move_reports: Vec<Value>,
    /// Workspaces asked to be renamed or deleted (`workspaceChange`), as stored.
    pub workspace_changes: Vec<Value>,
    /// Workspace change status reports, as sent.
    pub workspace_change_reports: Vec<Value>,
    /// What a session list's answer says the cloud takes (`accepts`).
    pub accepts: Vec<&'static str>,
    /// The latest heartbeats upload, as sent.
    pub heartbeats: Option<Value>,
    /// Heartbeat actions asked (`heartbeatAction`), as stored.
    pub heartbeat_actions: Vec<Value>,
    /// Heartbeat action status reports, as sent.
    pub heartbeat_action_reports: Vec<Value>,
    /// Updates asked of machines (`machineUpdate`), as stored.
    pub updates: Vec<Value>,
    /// Update status reports, as sent.
    pub update_reports: Vec<Value>,
    /// The device paired with the machine, which signs what's served.
    pub signer: Option<Arc<Phone>>,
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
        self.says("reply_attachments")
    }

    /// Whether the latest session list says `capability`.
    pub fn says(&self, capability: &str) -> bool {
        self.sessions.as_ref().is_some_and(|list| {
            list["capabilities"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == capability))
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

    /// Pair a device with the machine whose account service keeps its files
    /// in `sensagent` (`paired-devices.json`, as `rakumux devices pair`
    /// leaves it), and sign what's served with it from now on.
    #[allow(dead_code)]
    pub fn pair(&self, sensagent: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let phone = Phone::new();
        let file = sensagent.join("paired-devices.json");
        let devices = json!({"version": 1, "devices": [{
            "key_id": phone.key_id, "format": "p256", "public_key": phone.public_key(),
            "name": "Test browser", "paired_at": now_ms(),
        }]});
        std::fs::write(&file, devices.to_string()).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        self.state().signer = Some(Arc::new(phone));
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
            answer: None,
            signed: None,
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

impl FakeCloud {
    /// Ask this machine to delete `session`, as the website does; the request's id.
    #[allow(dead_code)]
    pub fn ask_deletion(&self, machine: &str, session: &str) -> String {
        let id = ulid();
        let at = now_ms();
        self.state()
            .deletions
            .push(json!({"id": id, "machine": machine, "session": session,
            "status": "pending", "created_at": at, "status_at": at}));
        id
    }

    /// Ask this machine to name `session` `name`, as the website does; the request's id.
    #[allow(dead_code)]
    pub fn ask_rename(&self, machine: &str, session: &str, name: &str) -> String {
        let id = ulid();
        let at = now_ms();
        self.state().renames.push(
            json!({"id": id, "machine": machine, "session": session, "name": name,
            "status": "pending", "created_at": at, "status_at": at}),
        );
        id
    }

    /// Ask this machine to move `session` into the workspace named
    /// `workspace` (out of any without one), as the website does; `signed`
    /// false leaves it unsigned (`"signed": null`, never signed as served).
    /// The request's id.
    #[allow(dead_code)]
    pub fn ask_move(
        &self,
        machine: &str,
        session: &str,
        workspace: Option<&str>,
        signed: bool,
    ) -> String {
        let id = ulid();
        let at = now_ms();
        let mut asked = json!({"id": id, "machine": machine, "session": session,
            "status": "pending", "created_at": at, "status_at": at});
        if let Some(workspace) = workspace {
            asked["workspace"] = json!(workspace);
        }
        if !signed {
            asked["signed"] = Value::Null;
        }
        self.state().moves.push(asked);
        id
    }

    /// Ask this machine to rename its workspace `workspace` to `name`, or to
    /// delete it without one, as the website does; `signed` false leaves it
    /// unsigned (`"signed": null`, never signed as served). The request's id.
    #[allow(dead_code)]
    pub fn ask_workspace_change(
        &self,
        machine: &str,
        workspace: &str,
        name: Option<&str>,
        signed: bool,
    ) -> String {
        let id = ulid();
        let at = now_ms();
        let action = if name.is_some() { "rename" } else { "delete" };
        let mut asked = json!({"id": id, "machine": machine, "workspace": workspace,
            "action": action, "status": "pending", "created_at": at, "status_at": at});
        if let Some(name) = name {
            asked["name"] = json!(name);
        }
        if !signed {
            asked["signed"] = Value::Null;
        }
        self.state().workspace_changes.push(asked);
        id
    }

    /// Ask this machine to `action` (`enable`, `pause`, `run`) its heartbeat
    /// `heartbeat`, as the website does (`enable` with the `updated_at` the
    /// user saw); the action's id.
    #[allow(dead_code)]
    pub fn ask_heartbeat_action(
        &self,
        machine: &str,
        heartbeat: &str,
        action: &str,
        updated_at: Option<i64>,
    ) -> String {
        let id = ulid();
        let at = now_ms();
        let mut asked = json!({"id": id, "machine": machine, "heartbeat": heartbeat,
            "action": action, "status": "pending", "created_at": at, "status_at": at});
        if let Some(updated_at) = updated_at {
            asked["updated_at"] = json!(updated_at);
        }
        self.state().heartbeat_actions.push(asked);
        id
    }

    /// Ask `machine` to update to `version`, as the website does; the update's
    /// id.
    pub fn ask_update(&self, machine: &str, version: &str) -> String {
        let id = ulid();
        let at = now_ms();
        self.state()
            .updates
            .push(json!({"id": id, "machine": machine,
            "version": version, "status": "pending", "created_at": at, "status_at": at}));
        id
    }

    /// Write an answer to the question `call_id` that `session`'s agent asks
    /// in chat `conversation`, as the user does on rakumux.com: `answers`
    /// as `questionAnswer`'s, `text` saying them. (`question_answers.rs`'s.)
    #[allow(dead_code)]
    pub fn write_answer(
        &self,
        session: (&str, &str),
        conversation: &str,
        call_id: &str,
        answers: Value,
        text: &str,
    ) -> String {
        let id = self.write_reply(session, conversation, text, &[]);
        let mut state = self.state();
        let reply = state
            .replies
            .iter_mut()
            .find(|reply| reply.id == id)
            .unwrap();
        reply.answer = Some(json!({"call_id": call_id, "answers": answers}));
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

/// What Vercel's Security Checkpoint answers in front of rakumux.com.
fn checkpoint() -> Answer {
    Answer(
        403,
        "text/html; charset=utf-8",
        b"<!DOCTYPE html><title>Vercel Security Checkpoint</title>".to_vec(),
    )
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
            403 => "Forbidden",
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
        if request.path == "/api/v1/feed/replies/status" && state.checkpoints > 0 {
            state.checkpoints -= 1;
            return checkpoint();
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/v1/features") => {
                let mut features = json!({"version": 1, "features":
                    {"ai_session_summaries": state.summaries, "feed": true}});
                if let Some(level) = state.logs_level {
                    features["logs"] = json!({"level": level});
                }
                answer(200, features)
            }
            ("POST", "/api/v1/feed/logs") => {
                let stored = body["events"].as_array().map_or(0, Vec::len);
                state.logs.push(body);
                answer(200, json!({"version": 1, "stored": stored, "wanted": true}))
            }
            ("POST", "/api/v1/ai/session-status") if state.summaries => {
                let request_id = body["request_id"].clone();
                let title = state.title.clone();
                state.titles.push((now_ms(), body));
                answer(
                    200,
                    json!({"version": 1, "request_id": request_id,
                        "status_text": "Watching the build", "title": title}),
                )
            }
            ("POST", "/api/v1/feed/sessions") => {
                let stored = body["sessions"].as_array().map_or(0, Vec::len);
                state.sessions = Some(body);
                let mut stored = json!({"version": 1, "stored": stored});
                if !state.accepts.is_empty() {
                    stored["accepts"] = json!(state.accepts);
                }
                answer(200, stored)
            }
            ("POST", "/api/v1/feed/heartbeats") if state.accepts.contains(&"heartbeats") => {
                let stored = body["heartbeats"].as_array().map_or(0, Vec::len);
                state.heartbeats = Some(body);
                answer(200, json!({"version": 1, "stored": stored}))
            }
            ("POST", "/api/v1/feed/heartbeat-actions/status") => {
                let Some(stored) = state
                    .heartbeat_actions
                    .iter_mut()
                    .find(|action| action["id"] == body["id"])
                else {
                    return error(404, "invalid_request");
                };
                stored["status"] = body["status"].clone();
                for field in ["failure", "session"] {
                    if !body[field].is_null() {
                        stored[field] = body[field].clone();
                    }
                }
                let stored = stored.clone();
                state.heartbeat_action_reports.push(body);
                answer(200, json!({"version": 1, "heartbeat_action": stored}))
            }
            ("POST", "/api/v1/feed/updates/status") => {
                let Some(stored) = state
                    .updates
                    .iter_mut()
                    .find(|update| update["id"] == body["id"])
                else {
                    return error(404, "invalid_request");
                };
                stored["status"] = body["status"].clone();
                if !body["failure"].is_null() {
                    stored["failure"] = body["failure"].clone();
                }
                let stored = stored.clone();
                state.update_reports.push(body);
                answer(200, json!({"version": 1, "update": stored}))
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
                let answers = state.says("question_answers")
                    && request.query.get("question_answers").map(String::as_str) == Some("1");
                let mut replies: Vec<&Reply> = state
                    .replies
                    .iter()
                    .filter(|reply| reply.seq > after)
                    .filter(|reply| attachments || reply.attachments.is_empty())
                    .filter(|reply| answers || reply.answer.is_none())
                    .collect();
                replies.sort_by_key(|reply| reply.seq);
                let replies: Vec<Value> = replies
                    .into_iter()
                    .map(|reply| reply.wire(&self.machine.name))
                    .collect();
                let mut read = json!({"version": 1, "replies": replies, "latest_seq": state.seq,
                    "more": false, "poll_ms": 1000});
                if state.says("delete_sessions") {
                    let pending: Vec<&Value> = state
                        .deletions
                        .iter()
                        .filter(|deletion| deletion["status"] == "pending")
                        .collect();
                    read["session_deletions"] = json!(pending);
                }
                if let (Some(open), Some(proposal)) = (&state.pairing, &state.proposal) {
                    if open["pairing"] == proposal["pairing"] {
                        read["pairings"] = json!([proposal]);
                    }
                }
                if state.says("workspaces") {
                    let pending: Vec<&Value> = state
                        .moves
                        .iter()
                        .filter(|asked| asked["status"] == "pending")
                        .collect();
                    read["session_moves"] = json!(pending);
                }
                if state.says("workspaces.edit") {
                    let pending: Vec<&Value> = state
                        .workspace_changes
                        .iter()
                        .filter(|asked| asked["status"] == "pending")
                        .collect();
                    read["workspace_changes"] = json!(pending);
                }
                if state.says("rename_sessions") {
                    let pending: Vec<&Value> = state
                        .renames
                        .iter()
                        .filter(|rename| rename["status"] == "pending")
                        .collect();
                    read["session_renames"] = json!(pending);
                }
                if state.says("heartbeats") {
                    let pending: Vec<&Value> = state
                        .heartbeat_actions
                        .iter()
                        .filter(|action| action["status"] == "pending")
                        .collect();
                    read["heartbeat_actions"] = json!(pending);
                }
                // Sent whatever the machine says, as a rakumux.com that
                // shouldn't (an older one, or one someone else changed) would.
                let pending: Vec<&Value> = state
                    .updates
                    .iter()
                    .filter(|update| update["status"] == "pending")
                    .take(1)
                    .collect();
                if !pending.is_empty() {
                    read["updates"] = json!(pending);
                }
                if let Some(phone) = &state.signer {
                    sign_read(phone, &self.machine.id, &state.files, &mut read);
                }
                answer(200, read)
            }
            ("POST", "/api/v1/feed/pairings") => {
                state.pairing = Some(body.clone());
                answer(
                    200,
                    json!({"version": 1, "pairing": body["pairing"], "expires_at": body["expires_at"]}),
                )
            }
            ("POST", "/api/v1/feed/pairings/status") => {
                let Some(proposal) = state.proposal.take() else {
                    return error(404, "invalid_request");
                };
                state.pairing = None;
                let status = body["status"].clone();
                state.pairing_reports.push(body);
                answer(
                    200,
                    json!({"version": 1, "id": proposal["id"], "pairing": proposal["pairing"],
                        "machine": self.machine.name, "status": status}),
                )
            }
            ("POST", "/api/v1/feed/session-deletions/status") => {
                let Some(stored) = state
                    .deletions
                    .iter_mut()
                    .find(|deletion| deletion["id"] == body["id"])
                else {
                    return error(404, "invalid_request");
                };
                stored["status"] = body["status"].clone();
                if !body["failure"].is_null() {
                    stored["failure"] = body["failure"].clone();
                }
                let stored = stored.clone();
                state.deletion_reports.push(body);
                answer(200, json!({"version": 1, "session_deletion": stored}))
            }
            ("POST", "/api/v1/feed/session-moves/status") => {
                let Some(stored) = state
                    .moves
                    .iter_mut()
                    .find(|asked| asked["id"] == body["id"])
                else {
                    return error(404, "invalid_request");
                };
                stored["status"] = body["status"].clone();
                if !body["failure"].is_null() {
                    stored["failure"] = body["failure"].clone();
                }
                let stored = stored.clone();
                state.move_reports.push(body);
                answer(200, json!({"version": 1, "session_move": stored}))
            }
            ("POST", "/api/v1/feed/workspace-changes/status") => {
                let Some(stored) = state
                    .workspace_changes
                    .iter_mut()
                    .find(|asked| asked["id"] == body["id"])
                else {
                    return error(404, "invalid_request");
                };
                stored["status"] = body["status"].clone();
                if !body["failure"].is_null() {
                    stored["failure"] = body["failure"].clone();
                }
                let stored = stored.clone();
                state.workspace_change_reports.push(body);
                answer(200, json!({"version": 1, "workspace_change": stored}))
            }
            ("POST", "/api/v1/feed/session-renames/status") => {
                let Some(stored) = state
                    .renames
                    .iter_mut()
                    .find(|rename| rename["id"] == body["id"])
                else {
                    return error(404, "invalid_request");
                };
                stored["status"] = body["status"].clone();
                if !body["failure"].is_null() {
                    stored["failure"] = body["failure"].clone();
                }
                let stored = stored.clone();
                state.rename_reports.push(body);
                answer(200, json!({"version": 1, "session_rename": stored}))
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
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let count = messages.len();
                state.messages.extend(messages);
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
