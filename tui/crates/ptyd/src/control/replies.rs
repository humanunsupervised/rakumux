//! Agent replies captured while reply capture is on (`reply_capture`), for
//! user clients that ask for them (the status service's feed). Memory only:
//! never the registry, never the audit log's text, never another caller, and
//! gone after a daemon start or restart.

use session_core::api::AgentReply;
use session_core::ids::ReplyId;
use std::collections::VecDeque;

/// Replies kept, oldest dropped first.
pub(crate) const MAX_REPLIES: usize = 200;
/// Their text, at most: a `reply_list` of every reply stays well within one
/// protocol frame (8 MiB), even with its text escaped.
pub(crate) const MAX_REPLY_TEXT: usize = 2 * 1024 * 1024;
/// How long a reply is kept (ms): a status service that started late or
/// reconnected catches up within it.
pub(crate) const REPLY_TTL_MS: i64 = 60 * 60 * 1000;
/// How long after a turn finishes its reply may still reach the log (ms).
pub(crate) const REPLY_WAIT_MS: i64 = 10_000;

/// How long after an agent's turn ended (the next one began) its session
/// may still be seen finishing it (`Done`, from its screen) (ms).
pub(crate) const DONE_LAG_MS: i64 = 500;
/// How many of a session's captured replies are remembered, so none is
/// captured twice.
const CAPTURED_KEYS: usize = 16;

/// The keys of a session's replies captured last (`agent_output::Reply::key`).
#[derive(Debug, Default)]
pub(crate) struct CapturedKeys(VecDeque<String>);

impl CapturedKeys {
    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|captured| captured == key)
    }

    pub fn insert(&mut self, key: String) {
        if self.contains(&key) {
            return;
        }
        if self.0.len() == CAPTURED_KEYS {
            self.0.pop_front();
        }
        self.0.push_back(key);
    }
}

/// A finished turn whose reply hasn't been captured yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplyDue {
    /// When the turn finished: the log must be read after this.
    pub since: i64,
    /// When to stop waiting for the reply.
    pub until: i64,
}

impl ReplyDue {
    pub fn new(now: i64) -> Self {
        Self {
            since: now,
            until: now.saturating_add(REPLY_WAIT_MS),
        }
    }
}

/// Whether capture is on, and what it captured.
#[derive(Debug, Default)]
pub(crate) struct ReplyRing {
    enabled: bool,
    replies: VecDeque<AgentReply>,
}

impl ReplyRing {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turning capture off forgets every captured reply.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.replies.clear();
        }
    }

    /// Keep `reply` (only while capture is on).
    pub fn push(&mut self, reply: AgentReply, now: i64) {
        if !self.enabled {
            return;
        }
        self.expire(now);
        self.replies.push_back(reply);
        let mut text: usize = self.replies.iter().map(|reply| reply.text.len()).sum();
        while self.replies.len() > MAX_REPLIES || text > MAX_REPLY_TEXT {
            match self.replies.pop_front() {
                Some(oldest) => text -= oldest.text.len(),
                None => break,
            }
        }
    }

    /// The replies captured after `after`, oldest first; every reply when
    /// `after` is absent or was already forgotten.
    pub fn after(&mut self, after: Option<&ReplyId>, now: i64) -> Vec<AgentReply> {
        self.expire(now);
        let start = after
            .and_then(|after| self.replies.iter().position(|reply| &reply.id == after))
            .map_or(0, |position| position + 1);
        self.replies.iter().skip(start).cloned().collect()
    }

    fn expire(&mut self, now: i64) {
        while self
            .replies
            .front()
            .is_some_and(|reply| now.saturating_sub(reply.finished_at) > REPLY_TTL_MS)
        {
            self.replies.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::ids::SessionId;

    fn reply(text: &str, at: i64) -> AgentReply {
        AgentReply {
            id: ReplyId::generate(),
            session_id: SessionId::from_string("S"),
            agent: "claude-code".into(),
            conversation_id: None,
            text: text.into(),
            truncated: false,
            finished_at: at,
            position: None,
        }
    }

    #[test]
    fn nothing_is_kept_while_capture_is_off() {
        let mut ring = ReplyRing::default();
        ring.push(reply("a", 1), 1);
        assert!(ring.after(None, 1).is_empty());
        ring.set_enabled(true);
        ring.push(reply("b", 2), 2);
        assert_eq!(ring.after(None, 2).len(), 1);
        ring.set_enabled(false);
        ring.set_enabled(true);
        assert!(ring.after(None, 3).is_empty(), "turning off forgets");
    }

    #[test]
    fn replies_are_listed_after_a_known_one_and_bounded() {
        let mut ring = ReplyRing::default();
        ring.set_enabled(true);
        let first = reply("first", 10);
        let id = first.id.clone();
        ring.push(first, 10);
        ring.push(reply("second", 20), 20);
        let after: Vec<_> = ring
            .after(Some(&id), 20)
            .into_iter()
            .map(|r| r.text)
            .collect();
        assert_eq!(after, ["second"]);
        let forgotten = ReplyId::generate();
        assert_eq!(ring.after(Some(&forgotten), 20).len(), 2);
        for i in 0..MAX_REPLIES as i64 {
            ring.push(reply("more", 30 + i), 30 + i);
        }
        assert_eq!(ring.after(None, 30).len(), MAX_REPLIES);
        let mut long = reply(&"x".repeat(32 * 1024), 1000);
        for i in 0..100 {
            long.id = ReplyId::generate();
            ring.push(long.clone(), 1000 + i);
        }
        let text: usize = ring.after(None, 1100).iter().map(|r| r.text.len()).sum();
        assert!(text <= MAX_REPLY_TEXT);
    }

    #[test]
    fn old_replies_expire() {
        let mut ring = ReplyRing::default();
        ring.set_enabled(true);
        ring.push(reply("old", 0), 0);
        ring.push(reply("new", REPLY_TTL_MS), REPLY_TTL_MS);
        let left: Vec<_> = ring
            .after(None, REPLY_TTL_MS + 1)
            .into_iter()
            .map(|r| r.text)
            .collect();
        assert_eq!(left, ["new"]);
    }
}
