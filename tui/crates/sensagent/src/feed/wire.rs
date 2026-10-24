//! The feed's wire messages (`contracts/feed/`): upload bodies built here,
//! pages and acceptances read back within the contract's bounds.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::cache::FeedEntry;
use crate::cloud::FeedHttpReply;

/// Entries per upload (contract).
pub(crate) const MAX_UPLOAD_ENTRIES: usize = 50;
/// Upload bodies stay under this, well within the contract's 1 MiB.
pub(crate) const UPLOAD_BUDGET: usize = 768 * 1024;
/// Entries asked for per read (the contract allows 200).
pub(crate) const PAGE_LIMIT: u32 = 100;
/// The longest reply text, in UTF-8 bytes (contract).
pub(crate) const MAX_TEXT_BYTES: usize = 32 * 1024;

/// Why a feed request failed. Static: never reply text, URLs or tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedFailure {
    /// The sign-in was refused; renewing it or signing in again helps.
    Unauthorized,
    /// The account's Feed switch is off.
    Off,
    /// Too many requests; try again after the delay, when given.
    RateLimited(Option<Duration>),
    /// The feed can't be reached or is down.
    Unavailable(Option<Duration>),
    /// The request or the answer broke the contract.
    Invalid,
}

/// One request to the feed, ready to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FeedRequest {
    /// Upload these entries (their ids, in order), as `body`.
    Upload { ids: Vec<String>, body: Vec<u8> },
    /// Read a page: the newest (`after` absent) or the entries after a seq.
    Read { after: Option<u64> },
}

impl FeedRequest {
    /// `GET /api/v1/feed`'s query for a read.
    pub fn query(after: Option<u64>) -> String {
        match after {
            Some(after) => format!("after={after}&limit={PAGE_LIMIT}"),
            None => format!("limit={PAGE_LIMIT}"),
        }
    }
}

#[derive(Serialize)]
struct Upload<'a> {
    version: u8,
    machine: &'a str,
    entries: Vec<UploadEntry<'a>>,
}

#[derive(Serialize)]
struct UploadEntry<'a> {
    id: &'a str,
    session: UploadSession<'a>,
    agent: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<&'a str>,
    text: &'a str,
    truncated: bool,
    finished_at: i64,
}

#[derive(Serialize)]
struct UploadSession<'a> {
    id: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<&'a str>,
}

fn upload_entry(entry: &FeedEntry) -> UploadEntry<'_> {
    UploadEntry {
        id: &entry.id,
        session: UploadSession {
            id: &entry.session_id,
            name: &entry.session_name,
            folder: entry.folder.as_deref(),
        },
        agent: &entry.agent,
        conversation_id: entry.conversation_id.as_deref(),
        text: &entry.text,
        truncated: entry.truncated,
        finished_at: entry.finished_at,
    }
}

/// An upload of the first of `entries` that fit one request, oldest first.
pub(crate) fn upload(machine: &str, entries: &[&FeedEntry]) -> Option<FeedRequest> {
    let mut chosen = Vec::new();
    let mut size = 256 + machine.len();
    for entry in entries.iter().take(MAX_UPLOAD_ENTRIES) {
        let one = serde_json::to_vec(&upload_entry(entry)).ok()?.len() + 1;
        if !chosen.is_empty() && size + one > UPLOAD_BUDGET {
            break;
        }
        size += one;
        chosen.push(*entry);
    }
    if chosen.is_empty() {
        return None;
    }
    let body = serde_json::to_vec(&Upload {
        version: 1,
        machine,
        entries: chosen.iter().map(|entry| upload_entry(entry)).collect(),
    })
    .ok()?;
    Some(FeedRequest::Upload {
        ids: chosen.iter().map(|entry| entry.id.clone()).collect(),
        body,
    })
}

#[derive(Deserialize)]
struct Accepted {
    version: u8,
    accepted: Vec<AcceptedEntry>,
}

#[derive(Deserialize)]
struct AcceptedEntry {
    id: String,
    seq: u64,
}

#[derive(Deserialize)]
struct Page {
    version: u8,
    entries: Vec<PageEntry>,
    latest_seq: u64,
    more: bool,
}

#[derive(Deserialize)]
struct PageEntry {
    seq: u64,
    id: String,
    machine: String,
    session: PageSession,
    agent: String,
    #[serde(default)]
    conversation_id: Option<String>,
    text: String,
    truncated: bool,
    finished_at: i64,
}

#[derive(Deserialize)]
struct PageSession {
    id: String,
    name: String,
    #[serde(default)]
    folder: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    code: String,
    #[serde(default)]
    retry_after_seconds: Option<u64>,
}

/// The longest wait a failure can ask for (the contract's: a day).
const MAX_RETRY_AFTER: Duration = Duration::from_secs(86_400);

/// A page as read: its entries, the account's newest seq and whether more follow.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PageRead {
    pub entries: Vec<FeedEntry>,
    pub latest_seq: u64,
    pub more: bool,
}

/// The seq of each uploaded id.
pub(crate) fn accepted(reply: &FeedHttpReply) -> Result<Vec<(String, u64)>, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let accepted: Accepted =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if accepted.version != 1 {
        return Err(FeedFailure::Invalid);
    }
    Ok(accepted
        .accepted
        .into_iter()
        .filter(|entry| entry.seq > 0)
        .map(|entry| (entry.id, entry.seq))
        .collect())
}

/// A page's entries, each checked against the contract's bounds and stripped of
/// anything a terminal would act on; entries that don't fit are left out.
pub(crate) fn page(reply: &FeedHttpReply) -> Result<PageRead, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let page: Page = serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if page.version != 1 || page.entries.len() > 200 {
        return Err(FeedFailure::Invalid);
    }
    let entries = page
        .entries
        .into_iter()
        .filter_map(|entry| {
            let fits = entry.seq > 0
                && within(&entry.id, 64)
                && within(&entry.machine, 256)
                && within(&entry.session.id, 64)
                && within(&entry.session.name, 256)
                && entry
                    .session
                    .folder
                    .as_deref()
                    .is_none_or(|f| within(f, 512))
                && within(&entry.agent, 32)
                && entry
                    .conversation_id
                    .as_deref()
                    .is_none_or(|c| within(c, 128))
                && entry.text.len() <= MAX_TEXT_BYTES;
            fits.then(|| FeedEntry {
                id: entry.id,
                seq: Some(entry.seq),
                machine: line(&entry.machine),
                session_id: entry.session.id,
                session_name: line(&entry.session.name),
                folder: entry.session.folder.map(|folder| line(&folder)),
                agent: line(&entry.agent),
                conversation_id: entry.conversation_id,
                text: plain(&entry.text),
                truncated: entry.truncated,
                finished_at: entry.finished_at,
            })
        })
        .collect();
    Ok(PageRead {
        entries,
        latest_seq: page.latest_seq,
        more: page.more,
    })
}

fn failure(reply: &FeedHttpReply) -> FeedFailure {
    // The code must agree with the status; anything else is a broken answer.
    let error = serde_json::from_slice::<ErrorBody>(&reply.body)
        .ok()
        .map(|body| body.error);
    let code = error.as_ref().map(|error| error.code.as_str());
    // The body's delay may be longer than a Retry-After header carries (a day's quota).
    let delay = error
        .as_ref()
        .and_then(|error| error.retry_after_seconds)
        .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
        .max(reply.retry_after);
    match (reply.status, code) {
        (401, Some("unauthenticated")) => FeedFailure::Unauthorized,
        (403, Some("forbidden")) => FeedFailure::Off,
        (429, Some("rate_limited")) => FeedFailure::RateLimited(delay),
        (503, Some("unavailable")) => FeedFailure::Unavailable(delay),
        (500..=599, _) => FeedFailure::Unavailable(None),
        _ => FeedFailure::Invalid,
    }
}

fn within(text: &str, bytes: usize) -> bool {
    !text.is_empty() && text.len() <= bytes
}

/// Whether a terminal would act on `character` rather than show it: controls
/// (but tab and line feed), and bidi and zero-width format characters.
fn hidden(character: char) -> bool {
    (character.is_control() && character != '\n' && character != '\t')
        || matches!(character, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

/// Text to show, without anything a terminal would act on.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|character| !hidden(*character))
        .collect()
}

/// One line to show: `plain`, with line breaks and tabs as spaces.
fn line(text: &str) -> String {
    plain(text)
        .chars()
        .map(|character| {
            if character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> FeedHttpReply {
        FeedHttpReply {
            status,
            retry_after: Some(Duration::from_secs(30)),
            body: body.as_bytes().to_vec(),
        }
    }

    fn entry(id: &str, text: &str) -> FeedEntry {
        FeedEntry {
            id: id.into(),
            seq: None,
            machine: "studio".into(),
            session_id: "01K89ZQ4D2F6H8J0K2M4N6P8R0".into(),
            session_name: "fix-tests".into(),
            folder: Some("rakumux".into()),
            agent: "claude-code".into(),
            conversation_id: None,
            text: text.into(),
            truncated: false,
            finished_at: 1_761_220_000_000,
        }
    }

    #[test]
    fn an_upload_takes_what_fits_one_request() {
        let small = entry("A", "short");
        let Some(FeedRequest::Upload { ids, body }) = upload("studio", &[&small]) else {
            panic!("expected an upload");
        };
        assert_eq!(ids, ["A"]);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["machine"], "studio");
        assert_eq!(json["entries"][0]["session"]["folder"], "rakumux");
        assert!(json["entries"][0].get("conversation_id").is_none());

        let big: Vec<FeedEntry> = (0..40)
            .map(|i| entry(&format!("E{i}"), &"x".repeat(30 * 1024)))
            .collect();
        let refs: Vec<&FeedEntry> = big.iter().collect();
        let Some(FeedRequest::Upload { ids, body }) = upload("studio", &refs) else {
            panic!("expected an upload");
        };
        assert!(ids.len() < 40 && !ids.is_empty());
        assert!(body.len() <= UPLOAD_BUDGET);
        assert!(upload("studio", &[]).is_none());
    }

    #[test]
    fn a_page_is_read_and_its_text_made_safe_to_show() {
        let body = r#"{"version":1,"latest_seq":9,"more":false,"entries":[
            {"seq":9,"id":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","machine":"lab\nbox","session":{"id":"01K89ZQ4D2F6H8J0K2M4N6P8R0","name":"tests"},
             "agent":"codex","text":"done \u001b[31mred\u202e","truncated":false,"finished_at":1,"received_at":2},
            {"seq":0,"id":"bad","machine":"m","session":{"id":"s","name":"n"},"agent":"codex","text":"t","truncated":false,"finished_at":1,"received_at":2}
        ]}"#;
        let read = page(&reply(200, body)).unwrap();
        assert_eq!(read.latest_seq, 9);
        assert_eq!(read.entries.len(), 1, "a zero seq is left out");
        let entry = &read.entries[0];
        assert_eq!(entry.seq, Some(9));
        assert_eq!(entry.machine, "lab box");
        assert_eq!(entry.text, "done [31mred");
        assert_eq!(entry.folder, None);
    }

    #[test]
    fn errors_follow_the_contracts_codes() {
        let error = |status, code: &str| {
            page(&reply(
                status,
                &format!(r#"{{"error":{{"code":"{code}","message":"x"}}}}"#),
            ))
            .unwrap_err()
        };
        assert_eq!(error(401, "unauthenticated"), FeedFailure::Unauthorized);
        assert_eq!(error(403, "forbidden"), FeedFailure::Off);
        assert_eq!(
            error(429, "rate_limited"),
            FeedFailure::RateLimited(Some(Duration::from_secs(30)))
        );
        assert_eq!(
            error(503, "unavailable"),
            FeedFailure::Unavailable(Some(Duration::from_secs(30)))
        );
        assert_eq!(error(403, "rate_limited"), FeedFailure::Invalid);
        let daily = page(&reply(
            429,
            r#"{"error":{"code":"rate_limited","message":"x","retry_after_seconds":7200}}"#,
        ));
        assert_eq!(
            daily.unwrap_err(),
            FeedFailure::RateLimited(Some(Duration::from_secs(7200)))
        );
        assert_eq!(
            page(&reply(502, "<html>")).unwrap_err(),
            FeedFailure::Unavailable(None)
        );
        assert_eq!(
            accepted(&reply(
                200,
                r#"{"version":1,"accepted":[{"id":"A","seq":3}]}"#
            ))
            .unwrap(),
            [("A".to_owned(), 3)]
        );
    }
}
