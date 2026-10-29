//! When the status service uploads, reads and captures: one feed request at
//! a time, this machine's replies first, then the account's new entries every
//! 10 s while a TUI shows the feed and every 2 minutes otherwise. Driven by
//! the owner thread with its own clock, so it holds no threads or timers.

use std::collections::HashMap;
use std::time::Duration;

use session_core::api::{AgentReply, MAX_REPLY_BYTES};
use session_core::ids::{ReplyId, SessionId};
use session_core::model::{ScopeRoot, SessionView};

use super::cache::{FeedCache, FeedEntry, FeedPage, FeedStatus};
use super::job::FeedResult;
use super::wire::{self, FeedFailure, FeedRequest};

/// How often the account's feed is read while a TUI shows it.
pub(crate) const WATCHED_READ: Duration = Duration::from_secs(10);
/// How often otherwise.
pub(crate) const IDLE_READ: Duration = Duration::from_secs(120);
/// A TUI that asked for the feed this recently is showing it.
pub(crate) const WATCH_WINDOW: Duration = Duration::from_secs(30);
/// The first and longest waits after a failed request.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// After the cloud says the switch is off, until the next features read.
const OFF_WAIT: Duration = Duration::from_secs(120);
/// The longest wait the cloud can ask for (a day's quota).
const LONGEST_WAIT: Duration = Duration::from_secs(86_400);

/// What the feed shows of a session: its name, folder name and branch, and
/// whether it's private (never uploaded).
struct SessionMeta {
    name: String,
    folder: Option<String>,
    branch: Option<String>,
    private: bool,
}

impl SessionMeta {
    fn of(view: &SessionView) -> Self {
        Self {
            name: view.name.clone(),
            folder: folder_name(&view.scope),
            branch: branch_of(view),
            private: crate::shared_elsewhere(view),
        }
    }
}

/// A branch as the feed shows it: one line of at most 128 characters, else
/// none (a cut branch name would name another).
fn branch_name(branch: &str) -> Option<String> {
    let fits = !branch.trim().is_empty()
        && branch.chars().count() <= 128
        && !branch.chars().any(char::is_control);
    fits.then(|| branch.to_owned())
}

/// `view`'s folder name, as the feed shows it.
pub(super) fn folder_of(view: &SessionView) -> Option<String> {
    folder_name(&view.scope)
}

/// The branch `view`'s agent works on, as the feed shows it.
pub(super) fn branch_of(view: &SessionView) -> Option<String> {
    view.git
        .as_ref()
        .and_then(|git| git.branch.as_deref())
        .and_then(branch_name)
}

fn folder_name(scope: &ScopeRoot) -> Option<String> {
    let name = scope.path().file_name()?.to_str()?;
    let name: String = name.chars().filter(|c| !c.is_control()).take(128).collect();
    (!name.trim().is_empty()).then_some(name)
}

/// The feed's state on the owner thread.
pub(crate) struct FeedSync {
    cache: FeedCache,
    machine: String,
    sessions: HashMap<SessionId, SessionMeta>,
    signed_in: bool,
    /// Signed out, or the switch was read: whether to capture is known.
    known: bool,
    /// Signed in, with the account's Feed switch on.
    enabled: bool,
    /// What the daemon was last told about capture; `None` since it (re)connected.
    capture: Option<bool>,
    capture_busy: bool,
    /// The newest reply the daemon sent, to catch up after reconnecting.
    last_reply: Option<ReplyId>,
    busy: bool,
    /// The account's feed was read since the switch turned on.
    read: bool,
    next_read: Duration,
    retry_at: Duration,
    retry: Duration,
    watched_until: Option<Duration>,
    failure: Option<FeedFailure>,
    /// Whether the account's feed is read back, for TUIs: not on an enrolled
    /// machine, where none runs.
    reads: bool,
}

impl FeedSync {
    pub fn new(machine: String) -> Self {
        Self {
            cache: FeedCache::default(),
            machine,
            sessions: HashMap::new(),
            signed_in: false,
            known: false,
            enabled: false,
            capture: None,
            capture_busy: false,
            last_reply: None,
            busy: false,
            read: false,
            next_read: Duration::ZERO,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
            watched_until: None,
            failure: None,
            reads: true,
        }
    }

    /// Upload only: never read the account's feed back.
    pub fn without_reads(mut self) -> Self {
        self.reads = false;
        self
    }

    /// Follow the account: signed in, and its Feed switch (`None` until read).
    /// Signing out or turning the switch off forgets the feed. True when the
    /// feed just turned on.
    pub fn set_account(&mut self, signed_in: bool, switch: Option<bool>, now: Duration) -> bool {
        let enabled = signed_in && switch == Some(true);
        self.signed_in = signed_in;
        self.known = !signed_in || switch.is_some();
        if enabled == self.enabled {
            return false;
        }
        self.enabled = enabled;
        self.read = false;
        self.failure = None;
        self.retry = RETRY_MIN;
        self.retry_at = now;
        self.next_read = now;
        if !enabled {
            self.cache.clear();
        }
        enabled
    }

    /// The daemon (re)connected: tell it about capture again, and catch up on
    /// replies after this one.
    pub fn connected(&mut self, views: &[SessionView]) -> Option<ReplyId> {
        self.capture = None;
        self.sessions = views
            .iter()
            .map(|view| (view.id.clone(), SessionMeta::of(view)))
            .collect();
        self.last_reply.clone()
    }

    /// A session's view changed or appeared.
    pub fn observe(&mut self, view: &SessionView) {
        self.sessions.insert(view.id.clone(), SessionMeta::of(view));
    }

    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
    }

    /// Whether the daemon should be told to capture or not, now. Not while the
    /// switch is unknown: turning capture off would drop the replies an earlier
    /// service had the daemon keep, before they're caught up.
    pub fn capture_change(&mut self) -> Option<bool> {
        if !self.known || self.capture_busy || self.capture == Some(self.enabled) {
            return None;
        }
        self.capture_busy = true;
        Some(self.enabled)
    }

    /// The daemon was told (or refused, an older daemon: not asked again until
    /// it reconnects).
    pub fn capture_changed(&mut self, enabled: bool) {
        self.capture_busy = false;
        self.capture = Some(enabled);
    }

    /// Replies read back or written here, for TUIs' threads.
    pub fn merge_replies(&mut self, replies: Vec<super::cache::FeedReply>) {
        if self.enabled && !replies.is_empty() {
            self.cache.merge_replies(replies);
        }
    }

    /// A reply the daemon captured: queued for upload unless its session is
    /// private, unknown, or the feed is off. `reply_to` is the feed reply it
    /// answers, when one was typed into its session.
    pub fn reply(&mut self, reply: AgentReply, reply_to: Option<String>) {
        if self.last_reply.as_ref().is_none_or(|last| reply.id > *last) {
            self.last_reply = Some(reply.id.clone());
        }
        if !self.enabled {
            return;
        }
        let Some(meta) = self.sessions.get(&reply.session_id) else {
            return;
        };
        if meta.private {
            return;
        }
        let Some(text) = crate::cloud::feed_text(&reply.text) else {
            return;
        };
        let (text, cut) = bounded(text);
        self.cache.insert_local(FeedEntry {
            id: reply.id.as_str().to_owned(),
            seq: None,
            machine: self.machine.clone(),
            session_id: reply.session_id.as_str().to_owned(),
            session_name: meta.name.clone(),
            folder: meta.folder.clone(),
            branch: meta.branch.clone(),
            agent: reply.agent,
            conversation_id: reply.conversation_id.filter(|id| wire::conversation_id(id)),
            text,
            truncated: reply.truncated || cut,
            finished_at: reply.finished_at,
            position: reply.position,
            reply_to,
            thread: None,
            prompt: None,
        });
    }

    /// The next request to make, if one is due: uploads first.
    pub fn next_request(&mut self, now: Duration) -> Option<FeedRequest> {
        if !self.enabled || self.busy || now < self.retry_at {
            return None;
        }
        let request = match wire::upload(&self.machine, &self.cache.pending()) {
            Some(upload) => upload,
            None if !self.reads => return None,
            None if !self.read => FeedRequest::Read { after: None },
            None if now >= self.next_read => FeedRequest::Read {
                after: Some(self.cache.latest_seq()),
            },
            None => return None,
        };
        self.busy = true;
        Some(request)
    }

    /// The request couldn't start (no usable sign-in): try again later.
    pub fn not_started(&mut self, now: Duration) {
        self.busy = false;
        self.fail(FeedFailure::Unauthorized, now);
    }

    /// A request finished; `current` says whether it was made for the account
    /// still signed in (otherwise its answer is dropped).
    pub fn completed(
        &mut self,
        current: bool,
        outcome: Result<FeedResult, FeedFailure>,
        now: Duration,
    ) {
        self.busy = false;
        if !current || !self.enabled {
            return;
        }
        match outcome {
            Ok(FeedResult::Accepted(accepted)) => {
                self.succeeded();
                self.cache.confirm(&accepted);
            }
            Ok(FeedResult::Page(page)) => {
                self.succeeded();
                self.read = true;
                let more = page.more;
                let read_to = page.read_to();
                self.cache.merge(page.entries, read_to);
                if let Some(open) = page.open {
                    self.cache.set_open(open);
                }
                self.next_read = if more {
                    now
                } else {
                    now + self.read_interval(now)
                };
            }
            // Chats', replies' and enrolled machines' requests, not the feed's.
            Ok(
                FeedResult::Stored
                | FeedResult::Turn(_)
                | FeedResult::Reply(_)
                | FeedResult::Message(_)
                | FeedResult::Replies(_)
                | FeedResult::SessionsStored(_)
                | FeedResult::Download(_)
                | FeedResult::Realtime(_)
                | FeedResult::Enrolled(_)
                | FeedResult::Revoked,
            ) => {}
            Err(failure) => self.fail(failure, now),
        }
    }

    /// The entries changed since a TUI's revision. Asking while `shown` means
    /// a TUI shows the feed: reads come every [`WATCHED_READ`], the first one
    /// now. A TUI that only keeps it (to count what's new) changes nothing.
    pub fn page(
        &mut self,
        after: Option<u64>,
        generation: Option<u64>,
        shown: bool,
        now: Duration,
    ) -> FeedPage {
        if shown {
            let was_watched = self.watched(now);
            self.watched_until = Some(now + WATCH_WINDOW);
            if !was_watched && self.read {
                self.next_read = self.next_read.min(now);
            }
        }
        FeedPage {
            machine: Some(self.machine.clone()),
            ..self.cache.page(self.status(), after, generation)
        }
    }

    /// Another machine held this one's name: it goes by `machine` now, and
    /// uploads what waits at once.
    pub fn rename(&mut self, machine: String, now: Duration) {
        self.machine = machine;
        self.retry = RETRY_MIN;
        self.retry_at = now;
    }

    pub fn status(&self) -> FeedStatus {
        match (self.signed_in, self.enabled, self.failure) {
            (false, _, _) => FeedStatus::SignedOut,
            (true, false, _) => FeedStatus::Off,
            (true, true, Some(_)) => FeedStatus::Unavailable,
            (true, true, None) if self.read => FeedStatus::Ready,
            (true, true, None) => FeedStatus::Loading,
        }
    }

    fn watched(&self, now: Duration) -> bool {
        self.watched_until.is_some_and(|until| now < until)
    }

    fn read_interval(&self, now: Duration) -> Duration {
        if self.watched(now) {
            WATCHED_READ
        } else {
            IDLE_READ
        }
    }

    fn succeeded(&mut self) {
        self.failure = None;
        self.retry = RETRY_MIN;
    }

    fn fail(&mut self, failure: FeedFailure, now: Duration) {
        self.failure = Some(failure);
        let wait = match failure {
            FeedFailure::RateLimited(Some(delay)) | FeedFailure::Unavailable(Some(delay)) => delay,
            FeedFailure::Off => OFF_WAIT,
            _ => self.retry,
        };
        self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
        self.retry_at = now + wait.min(LONGEST_WAIT);
    }
}

/// At most the contract's [`MAX_REPLY_BYTES`] (redaction can lengthen a reply),
/// and whether it was cut.
fn bounded(mut text: String) -> (String, bool) {
    if text.len() <= MAX_REPLY_BYTES {
        return (text, false);
    }
    let mut end = MAX_REPLY_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

/// This machine's name for the feed: its host name, or "this machine".
pub fn machine_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname writes at most
    // that many bytes.
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    let name = if result == 0 {
        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..end]).into_owned()
    } else {
        String::new()
    };
    let name: String = name
        .chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .take(64)
        .collect();
    if name.is_empty() {
        "this machine".to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::wire::PageRead;
    use crate::source::fake::live_view;

    const NOW: Duration = Duration::from_secs(100);

    fn view(id: &str, private: bool) -> SessionView {
        let mut view = live_view(&SessionId::from_string(id), "/home/me/rakumux", 1);
        view.private = private;
        view
    }

    fn reply(session: &str, text: &str) -> AgentReply {
        AgentReply {
            id: ReplyId::generate(),
            session_id: SessionId::from_string(session),
            agent: "claude-code".into(),
            conversation_id: Some("5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23".into()),
            text: text.into(),
            truncated: false,
            finished_at: 1_761_220_000_000,
            position: None,
        }
    }

    fn on() -> FeedSync {
        let mut feed = FeedSync::new("studio".into());
        feed.set_account(true, Some(true), NOW);
        feed.connected(&[view("S", false), view("P", true)]);
        feed
    }

    fn page(entries: Vec<FeedEntry>, latest: u64, more: bool) -> Result<FeedResult, FeedFailure> {
        let page_seq = entries.iter().filter_map(|entry| entry.seq).max();
        rows(entries, page_seq.unwrap_or(0), latest, more)
    }

    /// A page of a read: `entries` kept, its rows up to `page_seq`.
    fn rows(
        entries: Vec<FeedEntry>,
        page_seq: u64,
        latest: u64,
        more: bool,
    ) -> Result<FeedResult, FeedFailure> {
        Ok(FeedResult::Page(PageRead {
            entries,
            page_seq,
            latest_seq: latest,
            more,
            open: None,
        }))
    }

    #[test]
    fn capture_follows_the_switch_and_is_resent_after_a_reconnect() {
        let mut feed = FeedSync::new("studio".into());
        feed.set_account(true, None, NOW);
        assert_eq!(feed.capture_change(), None, "not before the switch is read");
        feed.set_account(false, None, NOW);
        assert_eq!(feed.capture_change(), Some(false));
        assert_eq!(feed.capture_change(), None, "one change at a time");
        feed.capture_changed(false);
        assert_eq!(feed.capture_change(), None);
        feed.set_account(true, Some(true), NOW);
        assert_eq!(feed.capture_change(), Some(true));
        feed.capture_changed(true);
        feed.connected(&[]);
        assert_eq!(feed.capture_change(), Some(true));
    }

    #[test]
    fn replies_of_known_non_private_sessions_upload_first_redacted() {
        let mut feed = on();
        // Split so secret scanners don't take the fixture for a key.
        let key = concat!("sk-", "abcdefghijklmnopqrstuvwxyz");
        feed.reply(
            reply("S", &format!("Done. The key was api_key={key}")),
            None,
        );
        feed.reply(reply("P", "private"), None);
        feed.reply(reply("unknown", "who?"), None);
        let Some(FeedRequest::Upload { ids, body }) = feed.next_request(NOW) else {
            panic!("expected an upload");
        };
        assert_eq!(ids.len(), 1);
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("[REDACTED]") && !body.contains("sk-abc"));
        assert!(body.contains(r#""folder":"rakumux""#));
        assert!(feed.next_request(NOW).is_none(), "one request at a time");
        feed.completed(
            true,
            Ok(FeedResult::Accepted(vec![(ids[0].clone(), 7)])),
            NOW,
        );
        // Then the first read: the newest page.
        assert_eq!(
            feed.next_request(NOW),
            Some(FeedRequest::Read { after: None })
        );
    }

    #[test]
    fn an_enrolled_machine_uploads_but_never_reads_the_feed() {
        let mut feed = FeedSync::new("my-first-sprite".into()).without_reads();
        feed.set_account(true, Some(true), NOW);
        feed.connected(&[view("S", false)]);
        assert_eq!(feed.next_request(NOW), None, "no first read");
        feed.reply(reply("S", "Done."), None);
        let Some(FeedRequest::Upload { ids, .. }) = feed.next_request(NOW) else {
            panic!("expected an upload");
        };
        feed.completed(
            true,
            Ok(FeedResult::Accepted(vec![(ids[0].clone(), 7)])),
            NOW,
        );
        assert_eq!(feed.next_request(NOW + Duration::from_secs(600)), None);
    }

    #[test]
    fn an_entry_carries_the_branch_its_session_was_on() {
        let mut feed = on();
        let mut branched = view("S", false);
        branched.git = Some(session_core::model::GitInfo {
            branch: Some("feature/parser-fix".into()),
            ..Default::default()
        });
        feed.observe(&branched);
        feed.reply(reply("S", "Fixed."), None);
        let Some(FeedRequest::Upload { body, .. }) = feed.next_request(NOW) else {
            panic!("expected an upload");
        };
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["entries"][0]["session"]["branch"],
            "feature/parser-fix"
        );
        // A detached HEAD, or a name that doesn't fit, gives none.
        assert_eq!(branch_name(&"b".repeat(129)), None);
        assert_eq!(branch_name("a\nb"), None);
    }

    #[test]
    fn an_answer_to_a_reply_uploads_naming_it() {
        let mut feed = on();
        feed.reply(
            reply("S", "Added the test."),
            Some("01K8A1B2C3D4E5F6G7H8J9K0MN".into()),
        );
        let Some(FeedRequest::Upload { body, .. }) = feed.next_request(NOW) else {
            panic!("expected an upload");
        };
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["entries"][0]["reply_to"], "01K8A1B2C3D4E5F6G7H8J9K0MN");
    }

    #[test]
    fn reads_continue_after_the_newest_seq_faster_while_watched() {
        let mut feed = on();
        assert_eq!(feed.status(), FeedStatus::Loading);
        assert_eq!(
            feed.next_request(NOW),
            Some(FeedRequest::Read { after: None })
        );
        feed.completed(true, page(Vec::new(), 12, false), NOW);
        assert_eq!(feed.status(), FeedStatus::Ready);
        assert_eq!(
            feed.next_request(NOW + WATCHED_READ),
            None,
            "idle: every 2 minutes"
        );
        // A TUI that only keeps the feed changes nothing.
        feed.page(None, None, false, NOW + Duration::from_secs(1));
        assert_eq!(feed.next_request(NOW + Duration::from_secs(1)), None);
        // A TUI showing it brings the next read forward and keeps it at 10 s.
        feed.page(None, None, true, NOW + Duration::from_secs(1));
        let at = NOW + Duration::from_secs(1);
        assert_eq!(
            feed.next_request(at),
            Some(FeedRequest::Read { after: Some(12) })
        );
        feed.completed(true, page(Vec::new(), 12, false), at);
        assert_eq!(
            feed.next_request(at + WATCHED_READ - Duration::from_millis(1)),
            None
        );
        assert!(feed.next_request(at + WATCHED_READ).is_some());
    }

    #[test]
    fn reads_go_on_page_by_page_past_rows_left_out() {
        let mut feed = on();
        assert!(feed.next_request(NOW).is_some());
        feed.completed(true, page(Vec::new(), 12, false), NOW);
        let later = NOW + Duration::from_secs(600);
        assert_eq!(
            feed.next_request(later),
            Some(FeedRequest::Read { after: Some(12) })
        );
        // More than a page arrived meanwhile: after this page, not after the
        // account's newest, so the entries between aren't skipped.
        let entries = vec![crate::feed::cache::tests::entry("E1", Some(13), 1)];
        feed.completed(true, page(entries, 300, true), later);
        assert_eq!(
            feed.next_request(later),
            Some(FeedRequest::Read { after: Some(13) })
        );
        // A page whose rows were all left out still moves reads on.
        feed.completed(true, rows(Vec::new(), 113, 300, true), later);
        assert_eq!(
            feed.next_request(later),
            Some(FeedRequest::Read { after: Some(113) })
        );
        feed.completed(true, rows(Vec::new(), 213, 300, false), later);
        assert_eq!(feed.next_request(later), None);
        assert_eq!(
            feed.next_request(later + RETRY_MAX),
            Some(FeedRequest::Read { after: Some(300) })
        );
    }

    #[test]
    fn failures_back_off_and_the_switch_turning_off_forgets_everything() {
        let mut feed = on();
        feed.reply(reply("S", "hello"), None);
        assert!(feed.next_request(NOW).is_some());
        feed.completed(true, Err(FeedFailure::Unavailable(None)), NOW);
        assert_eq!(feed.status(), FeedStatus::Unavailable);
        assert!(feed.next_request(NOW + Duration::from_secs(4)).is_none());
        assert!(feed.next_request(NOW + Duration::from_secs(5)).is_some());
        feed.completed(
            true,
            Err(FeedFailure::RateLimited(Some(Duration::from_secs(60)))),
            NOW + Duration::from_secs(5),
        );
        assert!(feed.next_request(NOW + Duration::from_secs(64)).is_none());
        // An answer for an older sign-in changes nothing.
        let before = feed.page(None, None, true, NOW).entries.len();
        feed.set_account(true, Some(false), NOW);
        assert_eq!(feed.status(), FeedStatus::Off);
        let page = feed.page(None, None, true, NOW);
        assert!(before == 1 && page.entries.is_empty());
        feed.set_account(false, None, NOW);
        assert_eq!(feed.status(), FeedStatus::SignedOut);
        assert!(feed.next_request(NOW + RETRY_MAX).is_none());
    }

    #[test]
    fn the_machine_has_a_short_visible_name() {
        let name = machine_name();
        assert!(!name.is_empty() && name.chars().count() <= 64);
        assert!(!name.chars().any(char::is_control));
    }
}
