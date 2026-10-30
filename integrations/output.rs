//! The latest thing a coding agent said or did, and the user's latest
//! prompt to it, each as one line for the session list: the agent's
//! reasoning, a tool call or its reply; what the user last typed. Read from
//! the agent's own conversation on this machine as it grows; tool output
//! and subagents' messages are never taken, and nothing leaves the daemon
//! but the lines.
//!
//! Provider implementations live beside their hooks in `integrations/<agent>`:
//! - **Claude Code**: `<config dir>/projects/*/<conversation>.jsonl`, the
//!   conversation being the one in `<config dir>/sessions/<pid>.json`.
//! - **Codex**: the `rollout-…-<thread>.jsonl` file the process keeps open.
//! - **OpenCode**: its database (`opencode.db`), for the conversation its
//!   Rakumux plugin bound to the session, else the folder's newest one.
//! - **Pi**: `<agent dir>/sessions/--<folder>--/<time>_<id>.jsonl`, for the
//!   conversation its Rakumux extension bound to the session, else the
//!   folder's newest one.
//! - **GitHub Copilot**: `$COPILOT_HOME/session-state/<id>/events.jsonl`, the
//!   conversation being the one its process log says is on screen.
//! - **Cursor**: its chat's transcript
//!   (`<data>/projects/<folder>/agent-transcripts/<chat>/<chat>.jsonl`), for
//!   the chat whose store the process has open, else the one its hooks name.
//! - **Grok Build**: `$GROK_HOME/sessions/<folder>/<session>/updates.jsonl`,
//!   for the session the process shows (`active_sessions.json`; the one its
//!   hooks named, else the one written last).
//! - **Kimi Code**: `$KIMI_CODE_HOME/sessions/…/<session>/agents/main/wire.jsonl`,
//!   the conversation being the one its plugin's hooks named, else the one
//!   the process holds a lock on, else the one followed (or the folder's
//!   newest); checked on every read.
//! - **Droid**: `~/.factory/sessions/<folder>/<conversation>.jsonl`, the
//!   conversation its hooks report, else the folder's newest one; positions
//!   count from the end of its first line, which Droid rewrites.
//! - **Amp**: the copy of the thread its Rakumux plugin bound to the
//!   session, which the plugin keeps under Amp's cache (Amp keeps none).
//!
//! For the agent feed, the follower also keeps the current turn's reply
//! whole ([`Reply`]): what the agent said after its last tool call, or the
//! turn's last message. The control layer captures it when the turn
//! finishes, and only while reply capture is on. A turn can end with the
//! next one's prompt before its session is ever seen waiting (an agent that
//! queues or steers in what was typed during the turn starts on it at once):
//! the reply it had then is kept as [`Finished`], for the control layer to
//! capture too. The whole conversation is read on demand ([`transcript`]),
//! and where the agent works from it ([`places`]).

use crate::host::ProcessInfo;
use serde_json::Value;
use session_core::api::MAX_REPLY_BYTES;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[path = "amp/output.rs"]
mod amp;
#[path = "claude-code/output.rs"]
mod claude;
#[path = "codex/output.rs"]
mod codex;
#[path = "copilot/output.rs"]
mod copilot;
#[path = "cursor-agent/output.rs"]
mod cursor;
#[path = "droid/output.rs"]
mod droid;
#[path = "grok/output.rs"]
mod grok;
#[path = "kimi/output.rs"]
mod kimi;
#[path = "open-code/output.rs"]
mod opencode;
#[path = "pi/output.rs"]
mod pi;
#[path = "places.rs"]
pub mod places;
#[path = "transcript.rs"]
pub mod transcript;

/// The most of a log read at once: a log that grew more is read from its tail.
const MAX_READ: u64 = 256 * 1024;
/// How far before its tail a new follower looks for the prompt of the turn the
/// tail is in, and reads the turn from it.
const MAX_PROMPT_SEARCH: u64 = 8 * 1024 * 1024;
/// The longest line kept, in characters.
pub const MAX_LINE: usize = 240;
/// The longest prompt or message a turn keeps, in characters.
pub const MAX_TURN_TEXT: usize = 600;
/// The most messages a turn keeps: its newest.
pub const MAX_TURN_MESSAGES: usize = 100;

/// A conversation's current turn, for an AI summary: the user's last
/// prompt and the agent's messages since (its reasoning, replies and tool
/// calls), each flattened to one line of at most [`MAX_TURN_TEXT`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Turn {
    /// Tells this turn from the next one: the prompts a log showed so far,
    /// or OpenCode's prompt row.
    pub id: u64,
    pub prompt: Option<String>,
    /// The newest [`MAX_TURN_MESSAGES`] messages, oldest first.
    pub messages: VecDeque<String>,
    /// Every message of the turn, kept or not.
    pub total: u32,
}

impl Turn {
    fn begin(&mut self, prompt: Option<String>) {
        self.id = self.id.wrapping_add(1);
        self.prompt = prompt;
        self.messages.clear();
        self.total = 0;
    }

    fn push(&mut self, message: &str) {
        let Some(message) = flat(message) else {
            return;
        };
        if self.messages.len() == MAX_TURN_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(message);
        self.total = self.total.saturating_add(1);
    }
}

/// The current turn's reply, as far as the log has shown it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The id of the log entry or turn it came from: a reply already
    /// captured is recognised by it.
    pub key: String,
    /// At most [`MAX_REPLY_BYTES`].
    pub text: String,
    /// The reply was longer than `text`.
    pub truncated: bool,
    /// The [`transcript`] position of the reply's last message.
    pub position: Option<u64>,
}

/// A turn's reply as it stood when the next turn began (its prompt, or for
/// Codex its next task), read as the log grew: the turn ended there, whether
/// or not its session was seen waiting in between.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub reply: Reply,
    /// The conversation it's in.
    pub conversation: Option<String>,
    /// When its turn began (its prompt), as the log says (Unix ms).
    pub began: Option<i64>,
    /// When its last message was written, as the log says (Unix ms).
    pub at: Option<i64>,
    /// When the turn after it began, as the log says (Unix ms).
    pub ended: Option<i64>,
}

/// The most finished replies kept for the control layer between two reads.
const MAX_FINISHED: usize = 16;
/// How many reads an OpenCode turn that ended is looked at for its reply.
const OPENCODE_PREVIOUS_READS: u8 = 5;

/// How the reply being built came to be, to tell a finished one.
#[derive(Debug, Default)]
struct ReplyState {
    /// When its last message was written (Unix ms), if the log says.
    at: Option<i64>,
    /// When the turn under way began (its prompt), if the log says.
    began: Option<i64>,
    /// Its last message was read as the log grew, not from the log's past
    /// (a follower's first read, a conversation it switched to, a log read
    /// again from its start or past a gap): only such a reply is finished.
    live: bool,
    /// The log followed was read once: what a read finds after is new.
    primed: bool,
    /// It's a finished turn's whole reply (Codex's `task_complete`): it ends
    /// with whatever resets it, not only with a prompt.
    complete: bool,
    /// Replies of turns that ended since the control layer last took them.
    finished: VecDeque<Finished>,
    /// OpenCode: a turn that ended when the next prompt came, whose reply
    /// is looked at again until it reads the same twice.
    opencode_previous: Option<OpenCodePrevious>,
}

/// An OpenCode turn that ended with the next prompt.
#[derive(Debug)]
struct OpenCodePrevious {
    /// The turn (as [`Turn::id`]) and its conversation.
    turn: u64,
    conversation: String,
    /// What the last read found its reply to be.
    seen: Option<opencode::TurnReply>,
    /// Reads left before it's given up on.
    reads_left: u8,
}

/// How one log entry changes the reply being built.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Nothing to do with the reply (thinking, metadata, …).
    Keep,
    /// A prompt or a tool call: what was said before it isn't the reply.
    Reset,
    /// More of the reply (Claude Code: one text block per entry).
    Text { key: String, text: String },
    /// The whole reply of a finished turn (Codex's `task_complete`).
    Complete { key: String, text: String },
    /// The turn ended (Kimi Code's `turn.ended`, completed): what it said
    /// is its whole reply, finished by whatever resets it.
    Finish,
}

/// An agent whose log is an append-only file of JSON lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogAgent {
    Claude,
    Codex,
    Pi,
    Copilot,
    Cursor,
    Grok,
    Kimi,
    /// Droid's session file, whose first line is rewritten in place: its
    /// offsets and positions count from the end of that line.
    Droid,
    /// The copy of a thread the Rakumux plugin for Amp keeps.
    Amp,
}

/// Where an agent's output is read from.
#[derive(Debug)]
enum Source {
    /// An append-only log, read from where it was last read.
    Log {
        agent: LogAgent,
        /// Claude Code's, Grok's or Kimi Code's conversation, checked on
        /// every read: the process can clear it, resume another or park it
        /// in a background job (Claude Code, Kimi Code), or hold several
        /// (Grok). Pi's, found again whenever the conversation bound to the
        /// session changes (or, unbound, its folder's newest does); none
        /// until its file exists.
        conversation: Option<String>,
        path: PathBuf,
        /// How far the log has been read.
        offset: u64,
        /// The start of a line not finished yet.
        partial: Vec<u8>,
    },
    /// OpenCode's database. The conversation is looked up on every read,
    /// since one OpenCode can switch between them.
    OpenCode {
        database: PathBuf,
        cwd: Option<PathBuf>,
        /// The one the last read found.
        conversation: Option<String>,
    },
}

/// Follows one agent process's conversation.
#[derive(Debug)]
pub struct Follower {
    pid: u32,
    source: Source,
    latest: Option<String>,
    prompt: Option<String>,
    turn: Turn,
    reply: Option<Reply>,
    /// What tells the reply being built finished, and those that did.
    reply_state: ReplyState,
    /// When the conversation last had a prompt or an agent message, by its
    /// own clock (Unix milliseconds): the newest seen, never moving back.
    last_at: Option<i64>,
    /// GitHub Copilot: which of the conversations its process holds open is
    /// on screen.
    foreground: Option<copilot::Foreground>,
}

impl Follower {
    /// Where `process`'s output can be read, when it runs `agent` (as
    /// detected, `claude`, `codex`, `opencode`, `copilot`, `grok` or `amp`,
    /// or as its hooks name it).
    pub fn locate(process: &ProcessInfo, agent: &str) -> Option<Self> {
        let mut foreground = None;
        let log = |agent, conversation, path| Source::Log {
            agent,
            conversation,
            path,
            offset: 0,
            partial: Vec::new(),
        };
        let source = match agent {
            "claude" | "claude-code" => {
                let config = crate::resume::claude_config_dir()?;
                let id = claude::conversation(&config, process.pid)?;
                let path = claude::transcript(&config, &id)?;
                log(LogAgent::Claude, Some(id), path)
            }
            "codex" => log(LogAgent::Codex, None, codex::log(process)?),
            // Its file is found on the first read.
            "pi" => log(LogAgent::Pi, None, PathBuf::new()),
            // Its chat is found on each read (`cursor::switch`).
            "cursor-agent" => log(LogAgent::Cursor, None, PathBuf::new()),
            "grok" => {
                let (id, path) = grok::current(process.pid, None)?;
                log(LogAgent::Grok, Some(id), path)
            }
            // Its conversation is found on every read (`kimi::switch`).
            "kimi" => log(LogAgent::Kimi, None, PathBuf::new()),
            // Found on the first read, from its hooks or its folder.
            "droid" => log(LogAgent::Droid, None, PathBuf::new()),
            // The thread is the one bound: `follow` opens its copy.
            "amp" => log(LogAgent::Amp, None, amp::threads_dir()?),
            "opencode" | "opencode2" => Source::OpenCode {
                database: crate::resume::opencode_database()?,
                cwd: crate::procinfo::cwd_of(process.pid),
                conversation: None,
            },
            // Its conversation is found on every read.
            "copilot" => {
                foreground = Some(copilot::Foreground::new(process)?);
                log(LogAgent::Copilot, None, PathBuf::new())
            }
            _ => return None,
        };
        Some(Self {
            pid: process.pid,
            source,
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            reply_state: ReplyState::default(),
            last_at: None,
            foreground,
        })
    }

    /// The agent as replies and turns name it: `claude-code`, `codex`,
    /// `opencode`, `pi`, `copilot`, `cursor-agent`, `grok`, `kimi`, `droid`
    /// or `amp`.
    pub fn agent(&self) -> &'static str {
        match &self.source {
            Source::Log {
                agent: LogAgent::Kimi,
                ..
            } => "kimi",
            Source::Log {
                agent: LogAgent::Claude,
                ..
            } => "claude-code",
            Source::Log {
                agent: LogAgent::Codex,
                ..
            } => "codex",
            Source::Log {
                agent: LogAgent::Pi,
                ..
            } => "pi",
            Source::Log {
                agent: LogAgent::Copilot,
                ..
            } => "copilot",
            Source::Log {
                agent: LogAgent::Cursor,
                ..
            } => "cursor-agent",
            Source::Log {
                agent: LogAgent::Grok,
                ..
            } => "grok",
            Source::Log {
                agent: LogAgent::Droid,
                ..
            } => "droid",
            Source::Log {
                agent: LogAgent::Amp,
                ..
            } => "amp",
            Source::OpenCode { .. } => "opencode",
        }
    }

    /// The conversation followed: the one its log was found for (Claude
    /// Code's, Pi's, …), Codex's thread from its
    /// `rollout-<time>-<thread>.jsonl`, the OpenCode session last read, or
    /// the Amp thread bound.
    pub fn conversation(&self) -> Option<String> {
        let id = match &self.source {
            Source::Log {
                agent:
                    LogAgent::Claude
                    | LogAgent::Pi
                    | LogAgent::Copilot
                    | LogAgent::Cursor
                    | LogAgent::Kimi
                    | LogAgent::Droid
                    | LogAgent::Amp,
                conversation,
                ..
            } => conversation.clone()?,
            Source::Log {
                agent: LogAgent::Codex,
                path,
                ..
            } => {
                let stem = path.file_stem()?.to_str()?;
                stem.get(stem.len().checked_sub(36)?..)?.to_owned()
            }
            Source::Log {
                agent: LogAgent::Grok,
                conversation,
                ..
            } => conversation.clone()?,
            Source::OpenCode { conversation, .. } => conversation.clone()?,
        };
        let valid = !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        valid.then_some(id)
    }

    /// Where the whole conversation is read from, once it's known.
    pub fn transcript_source(&self) -> Option<transcript::TranscriptSource> {
        let conversation = self.conversation()?;
        Some(match &self.source {
            Source::Log { agent, path, .. } => transcript::TranscriptSource::Log {
                agent: *agent,
                conversation,
                path: path.clone(),
            },
            Source::OpenCode { database, .. } => transcript::TranscriptSource::OpenCode {
                database: database.clone(),
                conversation,
            },
        })
    }

    /// The current turn's reply, once the log has shown one.
    pub fn reply(&self) -> Option<&Reply> {
        self.reply.as_ref()
    }

    /// The replies of the turns that ended (with the next turn's start)
    /// since the last call, oldest first ([`Finished`]).
    pub fn take_finished(&mut self) -> Vec<Finished> {
        self.reply_state.finished.drain(..).collect()
    }

    /// The process whose conversation this is.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether `conversation` is one this agent's process showed and
    /// doesn't now: GitHub Copilot's sessions sidebar keeps several open in
    /// one process, and only the one on screen is the session's.
    pub fn in_background(&self, conversation: &str) -> bool {
        self.foreground
            .as_ref()
            .is_some_and(|foreground| foreground.in_background(conversation))
    }

    /// The agent's latest line, once one was found.
    pub fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }

    /// The user's latest prompt, once one was found.
    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    /// The conversation's current turn.
    pub fn turn(&self) -> &Turn {
        &self.turn
    }

    /// When the conversation last had a prompt or an agent message (Unix
    /// milliseconds), once one was found.
    pub fn last_at(&self) -> Option<i64> {
        self.last_at
    }

    /// Read what the conversation gained since the last time (a log from its
    /// tail the first time). `bound` is the conversation the session named
    /// for this agent (bound with `session_bind`, else its hooks'), if any;
    /// agents whose process names its own conversation (Claude Code, Codex)
    /// don't need it. Returns whether either line changed.
    pub fn follow(&mut self, bound: Option<&str>) -> bool {
        let before = (self.turn.id, self.turn.total);
        // Replies of turns that ended in this read, their conversation named
        // once it's read.
        let mut ended: Vec<Finished> = Vec::new();
        let (line, prompt) = match &mut self.source {
            Source::Log {
                agent,
                conversation,
                path,
                offset,
                partial,
            } => {
                let agent = *agent;
                let switched = match agent {
                    LogAgent::Claude => {
                        switch_conversation(self.pid, conversation, path, offset, partial)
                    }
                    LogAgent::Pi => {
                        pi::switch(self.pid, bound, conversation, path, offset, partial)
                    }
                    LogAgent::Copilot => self.foreground.as_mut().is_some_and(|foreground| {
                        foreground.switch(bound, conversation, path, offset, partial)
                    }),
                    // Its chat is found again on each read; its transcript is
                    // written whole now and then.
                    LogAgent::Cursor => {
                        cursor::switch(self.pid, bound, conversation, path, offset, partial)
                            || cursor::realign(path, offset, partial)
                    }
                    LogAgent::Grok => {
                        grok::switch(self.pid, bound, conversation, path, offset, partial)
                    }
                    LogAgent::Kimi => {
                        kimi::switch(self.pid, bound, conversation, path, offset, partial)
                    }
                    LogAgent::Droid => {
                        droid::switch(self.pid, bound, conversation, path, offset, partial)
                    }
                    LogAgent::Amp => amp::switch(bound, conversation, path, offset, partial),
                    LogAgent::Codex => false,
                };
                if switched {
                    self.turn.begin(None);
                    self.reply = None;
                    self.reply_state.live = false;
                    self.reply_state.complete = false;
                    self.reply_state.primed = false;
                }
                // Amp's copy is read only once a thread is bound.
                if agent == LogAgent::Amp && conversation.is_none() {
                    return false;
                }
                // What the user typed: a turn's start.
                let typed = |line: &[u8]| {
                    let candidate = match agent {
                        LogAgent::Claude => claude::may_be_typed(line),
                        LogAgent::Codex => codex::may_be_typed(line),
                        LogAgent::Pi => pi::may_be_typed(line),
                        LogAgent::Copilot => copilot::may_be_typed(line),
                        LogAgent::Cursor => cursor::may_be_typed(line),
                        LogAgent::Grok => grok::may_be_typed(line),
                        LogAgent::Kimi => kimi::may_be_typed(line),
                        LogAgent::Droid => droid::may_be_typed(line),
                        LogAgent::Amp => amp::may_be_typed(line),
                    };
                    candidate
                        && serde_json::from_slice::<Value>(line)
                            .ok()
                            .and_then(|entry| match agent {
                                LogAgent::Claude => claude::typed(&entry),
                                LogAgent::Codex => codex::typed(&entry),
                                LogAgent::Pi => pi::typed(&entry),
                                LogAgent::Copilot => copilot::typed(&entry),
                                LogAgent::Cursor => cursor::typed(&entry),
                                LogAgent::Grok => grok::typed(&entry),
                                LogAgent::Kimi => kimi::typed(&entry),
                                LogAgent::Droid => droid::typed(&entry),
                                LogAgent::Amp => amp::typed(&entry),
                            })
                            .is_some_and(|typed| prompt_line(&typed).is_some())
                };
                // Where the next line was to start: a read that starts
                // elsewhere (from the start again, or past a gap) isn't new.
                let from = offset.saturating_sub(partial.len() as u64);
                let lines = match agent {
                    LogAgent::Droid => droid::read(path, offset, partial, &typed),
                    _ => read(path, offset, partial, &typed),
                };
                let Some(lines) = lines else {
                    return false;
                };
                let live =
                    self.reply_state.primed && lines.first().is_none_or(|(at, _)| *at == from);
                self.reply_state.primed = true;
                // Cursor's lines have no times: what a read finds is as new
                // as the file.
                let written = match agent {
                    LogAgent::Cursor => {
                        cursor::hold_turn_end(&lines, offset, partial);
                        cursor::modified_ms(path)
                    }
                    _ => None,
                };
                let entries: Vec<(u64, Value)> = lines
                    .iter()
                    .filter_map(|(at, line)| Some((*at, serde_json::from_slice(line).ok()?)))
                    .collect();
                // In order: a prompt starts a turn, the agent's messages join it.
                for (position, entry) in &entries {
                    let (typed, messages) = match agent {
                        LogAgent::Claude => (claude::typed(entry), claude::messages(entry)),
                        LogAgent::Codex => (codex::typed(entry), codex::messages(entry)),
                        LogAgent::Pi => (pi::typed(entry), pi::messages(entry)),
                        LogAgent::Copilot => (copilot::typed(entry), copilot::messages(entry)),
                        LogAgent::Cursor => (cursor::typed(entry), cursor::messages(entry)),
                        LogAgent::Grok => (grok::typed(entry), grok::messages(entry)),
                        LogAgent::Kimi => (kimi::typed(entry), kimi::messages(entry)),
                        LogAgent::Droid => (droid::typed(entry), droid::messages(entry)),
                        LogAgent::Amp => (amp::typed(entry), amp::messages(entry)),
                    };
                    let typed = typed.filter(|typed| prompt_line(typed).is_some());
                    let at = entry_time(agent, entry).or(written);
                    if typed.is_some() || !messages.is_empty() {
                        self.last_at = self.last_at.max(at);
                    }
                    let step = match agent {
                        LogAgent::Claude => claude::reply_step(entry),
                        LogAgent::Codex => codex::reply_step(entry),
                        LogAgent::Pi => pi::reply_step(entry),
                        LogAgent::Copilot => copilot::reply_step(entry),
                        LogAgent::Cursor => {
                            cursor::reply_step(entry, *position, conversation.as_deref())
                        }
                        LogAgent::Grok => grok::reply_step(entry),
                        LogAgent::Kimi => kimi::reply_step(entry),
                        LogAgent::Droid => droid::reply_step(entry),
                        LogAgent::Amp => amp::reply_step(entry),
                    };
                    // The next turn begins (a prompt; after a whole reply,
                    // whatever resets it): the reply this turn had, read as
                    // the log grew, is its finished reply.
                    let next_turn =
                        typed.is_some() || (step == Step::Reset && self.reply_state.complete);
                    if next_turn && live && self.reply_state.live {
                        if let Some(reply) = &self.reply {
                            ended.push(Finished {
                                reply: reply.clone(),
                                conversation: None,
                                began: self.reply_state.began,
                                at: self.reply_state.at,
                                ended: at,
                            });
                        }
                    }
                    if let Some(typed) = typed {
                        self.turn.begin(prompt_text(&typed));
                        self.reply_state.began = at;
                    }
                    for message in &messages {
                        self.turn.push(message);
                    }
                    if step == Step::Finish {
                        self.reply_state.complete = self.reply.is_some();
                    } else if step != Step::Keep {
                        let complete = matches!(step, Step::Complete { .. });
                        self.reply = next_reply(self.reply.take(), step, *position);
                        let state = &mut self.reply_state;
                        state.at = self.reply.as_ref().and(at);
                        state.live = live && self.reply.is_some();
                        state.complete = complete;
                    }
                }
                let newest = |line: fn(&Value) -> Option<String>| {
                    entries.iter().rev().find_map(|(_, entry)| line(entry))
                };
                match agent {
                    LogAgent::Claude => (newest(claude::line), newest(claude::prompt)),
                    LogAgent::Codex => (newest(codex::line), newest(codex::prompt)),
                    LogAgent::Pi => (newest(pi::line), newest(pi::prompt)),
                    LogAgent::Copilot => (newest(copilot::line), newest(copilot::prompt)),
                    LogAgent::Cursor => (newest(cursor::line), newest(cursor::prompt)),
                    LogAgent::Grok => (newest(grok::line), newest(grok::prompt)),
                    LogAgent::Kimi => (newest(kimi::line), newest(kimi::prompt)),
                    LogAgent::Droid => (newest(droid::line), newest(droid::prompt)),
                    LogAgent::Amp => (newest(amp::line), newest(amp::prompt)),
                }
            }
            Source::OpenCode {
                database,
                cwd,
                conversation: followed,
            } => {
                let conversation = match bound {
                    Some(bound) => Some(bound.to_string()),
                    None => cwd
                        .as_deref()
                        .and_then(crate::resume::opencode_recent_session),
                };
                let conversation = conversation.filter(|id| crate::resume::is_conversation_id(id));
                let switched = *followed != conversation;
                if switched {
                    *followed = conversation.clone();
                    self.reply = None;
                    self.reply_state.primed = false;
                    self.reply_state.opencode_previous = None;
                }
                let live = self.reply_state.primed;
                self.reply_state.primed = true;
                match conversation {
                    Some(id) => {
                        self.last_at = self.last_at.max(opencode::last_at(database, &id));
                        self.reply =
                            opencode::reply(database, &id).map(|(key, text, position)| Reply {
                                position: Some(position),
                                ..bounded(key, text)
                            });
                        if let Some((turn_id, prompt, messages)) = opencode::turn(database, &id) {
                            if turn_id != self.turn.id {
                                // The turn before ended with this prompt: its
                                // reply is read once it stays put.
                                if live && self.turn.id != 0 {
                                    self.reply_state.opencode_previous = Some(OpenCodePrevious {
                                        turn: self.turn.id,
                                        conversation: id.clone(),
                                        seen: None,
                                        reads_left: OPENCODE_PREVIOUS_READS,
                                    });
                                }
                                self.turn = Turn {
                                    id: turn_id,
                                    ..Turn::default()
                                };
                            }
                            self.turn.prompt = prompt.as_deref().and_then(prompt_text);
                            self.turn.messages.clear();
                            self.turn.total = 0;
                            for message in &messages {
                                self.turn.push(message);
                            }
                        }
                        if let Some(previous) = self.reply_state.opencode_previous.as_mut() {
                            let now = (previous.conversation == id)
                                .then(|| opencode::turn_reply(database, &id, previous.turn))
                                .flatten();
                            if now.is_some() && now == previous.seen {
                                if let Some(found) = now {
                                    ended.push(Finished {
                                        reply: Reply {
                                            position: Some(found.position),
                                            ..bounded(found.key, found.text)
                                        },
                                        conversation: None,
                                        began: found.began,
                                        at: found.at,
                                        ended: found.ended,
                                    });
                                }
                                self.reply_state.opencode_previous = None;
                            } else {
                                previous.seen = now;
                                previous.reads_left = previous.reads_left.saturating_sub(1);
                                if previous.reads_left == 0 {
                                    self.reply_state.opencode_previous = None;
                                }
                            }
                        }
                        (
                            opencode::latest(database, &id),
                            opencode::latest_prompt(database, &id),
                        )
                    }
                    None => (None, None),
                }
            }
        };
        if !ended.is_empty() {
            let conversation = self.conversation();
            let finished = &mut self.reply_state.finished;
            for mut reply in ended {
                reply.conversation = conversation.clone();
                if finished.len() == MAX_FINISHED {
                    finished.pop_front();
                }
                finished.push_back(reply);
            }
        }
        // A read with nothing new keeps what was there.
        let mut changed = before != (self.turn.id, self.turn.total);
        for (found, kept) in [(line, &mut self.latest), (prompt, &mut self.prompt)] {
            if let Some(found) = found.filter(|found| kept.as_deref() != Some(found.as_str())) {
                *kept = Some(found);
                changed = true;
            }
        }
        changed
    }
}

/// When a log entry was written (Unix milliseconds), as its agent records it:
/// most in an RFC 3339 `timestamp`. Cursor's have no time (the file's stands
/// in, where it's read).
fn entry_time(agent: LogAgent, entry: &Value) -> Option<i64> {
    match agent {
        LogAgent::Grok => grok::timestamp(entry),
        LogAgent::Kimi => kimi::at(entry),
        _ => entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(transcript::timestamp),
    }
}

/// Follow Claude Code's conversation to wherever the process works now: a
/// new transcript is read from its tail. Returns whether it switched.
fn switch_conversation(
    pid: u32,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    let Some(config) = crate::resume::claude_config_dir() else {
        return false;
    };
    let Some(current) = claude::conversation(&config, pid) else {
        return false;
    };
    if conversation.as_deref() == Some(current.as_str()) {
        return false;
    }
    let Some(transcript) = claude::transcript(&config, &current) else {
        return false;
    };
    *path = transcript;
    *offset = 0;
    partial.clear();
    *conversation = Some(current);
    true
}

/// The complete lines `path` gained since `offset` (moved past them), each
/// with where it starts in the file; the start of an unfinished one waits in
/// `partial`. More than [`MAX_READ`] is read from the tail; the first time,
/// from the newest line before it that `prompt` picks (the user's prompt of
/// the turn the tail is in), within [`MAX_PROMPT_SEARCH`].
fn read(
    path: &Path,
    offset: &mut u64,
    partial: &mut Vec<u8>,
    prompt: &dyn Fn(&[u8]) -> bool,
) -> Option<Vec<(u64, Vec<u8>)>> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < *offset {
        // Rewritten: start again.
        *offset = 0;
        partial.clear();
    }
    if len == *offset {
        return Some(Vec::new());
    }
    let skipped = len - *offset > MAX_READ;
    let first = *offset == 0;
    let tail = len - MAX_READ.min(len);
    let start = match (skipped, first) {
        (false, _) => *offset,
        (true, true) => tail.saturating_sub(MAX_PROMPT_SEARCH),
        (true, false) => tail,
    };
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes).ok()?;
    *offset = start + bytes.len() as u64;
    let (mut data, base) = if skipped {
        partial.clear();
        // Mid-line: from the next one, or the prompt before it when the
        // tail has none.
        let window = usize::try_from(tail - start).ok()?;
        let from = match bytes[window..].iter().position(|b| *b == b'\n') {
            Some(at) => {
                let from = window + at + 1;
                let prompted = bytes[from..].split(|b| *b == b'\n').any(prompt);
                (!prompted)
                    .then(|| newest_line(&bytes[..from], start == 0, prompt))
                    .flatten()
                    .unwrap_or(from)
            }
            None => bytes.len(),
        };
        (bytes.split_off(from), start + from as u64)
    } else {
        let base = start.saturating_sub(partial.len() as u64);
        let mut data = std::mem::take(partial);
        data.extend_from_slice(&bytes);
        (data, base)
    };
    let complete = data
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    let rest = data.split_off(complete);
    if (rest.len() as u64) <= MAX_READ {
        *partial = rest;
    }
    let mut lines = Vec::new();
    let mut at = base;
    for line in data.split(|b| *b == b'\n') {
        if !line.is_empty() {
            lines.push((at, line.to_vec()));
        }
        at += line.len() as u64 + 1;
    }
    Some(lines)
}

/// Where the newest line of `head` that `wanted` picks starts. `head` ends
/// a line; its first is whole only when `whole` (the file's start).
fn newest_line(head: &[u8], whole: bool, wanted: &dyn Fn(&[u8]) -> bool) -> Option<usize> {
    let mut end = head.len().checked_sub(1)?;
    loop {
        let begin = match head[..end].iter().rposition(|b| *b == b'\n') {
            Some(at) => at + 1,
            None if whole => 0,
            None => return None,
        };
        if wanted(&head[begin..end]) {
            return Some(begin);
        }
        end = begin.checked_sub(1)?;
    }
}

/// Whether `haystack` holds `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The reply after one more log entry, the one at `position`.
fn next_reply(reply: Option<Reply>, step: Step, position: u64) -> Option<Reply> {
    let at = |reply: Reply| Reply {
        position: Some(position),
        ..reply
    };
    match step {
        Step::Keep | Step::Finish => reply,
        Step::Reset => None,
        Step::Text { key, text } => {
            let text = match reply {
                Some(reply) if !reply.truncated => format!("{}\n\n{text}", reply.text),
                Some(reply) => return Some(at(Reply { key, ..reply })),
                None => text,
            };
            Some(at(bounded(key, text)))
        }
        Step::Complete { key, text } => Some(at(bounded(key, text))),
    }
}

/// A reply of at most [`MAX_REPLY_BYTES`], cut at a character boundary.
fn bounded(key: String, mut text: String) -> Reply {
    let truncated = text.len() > MAX_REPLY_BYTES;
    if truncated {
        let mut end = MAX_REPLY_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Reply {
        key,
        text,
        truncated,
        position: None,
    }
}

/// `text`'s first meaningful line, for the list: Markdown marks and control
/// characters dropped, whitespace collapsed, at most [`MAX_LINE`] characters.
fn first_line(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.chars().any(char::is_alphanumeric))?;
    let line = line
        .trim_start_matches(['#', '>', '-', '*', ' '])
        .replace("**", "")
        .replace('`', "");
    let words: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    let line: String = words.join(" ").chars().take(MAX_LINE).collect();
    (!line.is_empty()).then_some(line)
}

/// What the user typed, as a line: the agents' own wrapped insertions
/// (reminders, notifications, command output) dropped, a slash command as
/// `/name args`, and a paste's first line only when nothing was typed
/// around it.
fn prompt_line(text: &str) -> Option<String> {
    let pieces = sent(text)?;
    let line = |pasted: bool| {
        pieces
            .iter()
            .filter(|piece| piece.pasted == pasted)
            .find_map(|piece| first_line(&piece.text))
    };
    line(false).or_else(|| line(true))
}

/// What the user sent, whole, flattened for a summary ([`flat`]).
fn prompt_text(text: &str) -> Option<String> {
    flat(&typed_text(text)?)
}

/// `text` flattened to one line for a summary: whitespace and control
/// characters collapsed, at most [`MAX_TURN_TEXT`] characters.
fn flat(text: &str) -> Option<String> {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    let text: String = words.join(" ").chars().take(MAX_TURN_TEXT).collect();
    (!text.is_empty()).then_some(text)
}

/// What the user sent in `text`, whole: what they typed, the agents' own
/// insertions taken out, and what they pasted, in order (see [`sent`]).
fn typed_text(text: &str) -> Option<String> {
    let pieces = sent(text)?;
    let texts: Vec<&str> = pieces
        .iter()
        .map(|piece| piece.text.trim_matches('\n'))
        .collect();
    Some(texts.join("\n\n"))
}

/// A piece of what the user sent: typed, or pasted.
struct Piece {
    text: String,
    pasted: bool,
}

/// What the user sent in `text`, in order, each piece with a line in it:
/// what they typed, the agents' own insertions taken out, and what they
/// pasted that Claude Code logs as a block of its own (a paste of several
/// lines: a reply typed in with its files' paths among them), as pasted
/// ([`pastes`]). A slash command is one piece, `/name args`. `None` when
/// nothing is left.
fn sent(text: &str) -> Option<Vec<Piece>> {
    if let Some(command) = tagged(text, "command-name") {
        let args = tagged(text, "command-args").unwrap_or_default();
        return Some(vec![Piece {
            text: format!("{} {}", command.trim(), args.trim()),
            pasted: false,
        }]);
    }
    let pieces: Vec<Piece> = pastes(text)
        .into_iter()
        .map(|(text, pasted)| Piece {
            text: if pasted {
                text.to_owned()
            } else {
                without_insertions(text)
            },
            pasted,
        })
        .filter(|piece| first_line(&piece.text).is_some())
        .collect();
    (!pieces.is_empty()).then_some(pieces)
}

/// `text` without the agents' own wrapped insertions (reminders,
/// notifications, command output).
fn without_insertions(text: &str) -> String {
    let mut typed = text.to_string();
    for tag in [
        "system-reminder",
        "task-notification",
        "local-command-stdout",
        "local-command-stderr",
        "local-command-caveat",
        "user_instructions",
        "environment_context",
    ] {
        while let Some(stripped) = without(&typed, tag) {
            typed = stripped;
        }
    }
    typed
}

/// The opening tag of Claude Code's block around a paste.
const PASTE_OPEN: &str = "<pasted_content";

/// `text` split at Claude Code's paste blocks: the text around them
/// (`false`) and each paste's own (`true`), in order. Claude Code logs a
/// paste of several lines (in a prompt, or one queued while a turn ran) as
/// `<pasted_content id="…">\n…\n</pasted_content id="…">`, its closing tag
/// naming the same id, with the user's text around it; a block never closed
/// is text, as Claude Code reads it.
fn pastes(text: &str) -> Vec<(&str, bool)> {
    let mut pieces = Vec::new();
    let (mut typed_from, mut from) = (0, 0);
    while let Some(found) = text[from..].find(PASTE_OPEN) {
        let open = from + found;
        from = open + PASTE_OPEN.len();
        let Some(head) = text[from..].find('>') else {
            break;
        };
        // ` id="…"`, or nothing.
        let attributes = &text[from..from + head];
        if attributes.contains(['\n', '<'])
            || !(attributes.is_empty() || attributes.starts_with(' '))
        {
            continue;
        }
        let body = from + head + 1;
        let close = format!("</pasted_content{attributes}>");
        let Some(length) = text[body..].find(&close) else {
            continue;
        };
        let end = body + length;
        let inner = &text[body..end];
        let inner = inner.strip_prefix('\n').unwrap_or(inner);
        let inner = inner.strip_suffix('\n').unwrap_or(inner);
        pieces.push((&text[typed_from..open], false));
        pieces.push((inner, true));
        typed_from = end + close.len();
        from = typed_from;
    }
    pieces.push((&text[typed_from..], false));
    pieces
}

/// The text inside `text`'s first `<tag …>…</tag>`.
fn tagged(text: &str, tag: &str) -> Option<String> {
    let open = text.find(&format!("<{tag}"))?;
    let start = open + text[open..].find('>')? + 1;
    let end = start + text[start..].find(&format!("</{tag}>"))?;
    Some(text[start..end].to_string())
}

/// `text` without its first `<tag …>…</tag>` (to its end when unclosed).
fn without(text: &str, tag: &str) -> Option<String> {
    let open = text.find(&format!("<{tag}"))?;
    let close = format!("</{tag}>");
    let end = text[open..]
        .find(&close)
        .map_or(text.len(), |at| open + at + close.len());
    Some(format!("{}{}", &text[..open], &text[end..]))
}

/// A tool call as a line: `name: what it works on`, or the name alone.
fn tool_line(name: &str, subject: Option<&str>) -> Option<String> {
    match subject.and_then(first_line) {
        Some(subject) => first_line(&format!("{name}: {subject}")),
        None => first_line(name),
    }
}

/// A file's last two path components: `app/account.rs`.
fn short_path(path: &str) -> String {
    let parts: Vec<_> = Path::new(path)
        .components()
        .rev()
        .take(2)
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn codex_log(path: PathBuf) -> Follower {
        Follower {
            pid: 1,
            source: Source::Log {
                agent: LogAgent::Codex,
                conversation: None,
                path,
                offset: 0,
                partial: Vec::new(),
            },
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            reply_state: Default::default(),
            last_at: None,
            foreground: None,
        }
    }

    #[test]
    fn the_last_message_is_the_newest_prompt_or_agent_message() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        let event = |at: &str, payload: serde_json::Value| {
            format!(
                "{}\n",
                serde_json::json!({"timestamp": at, "type": "event_msg", "payload": payload})
            )
        };
        let mut follower = codex_log(path);
        file.write_all(
            event(
                "2026-10-22T00:54:42.000Z",
                serde_json::json!({"type": "user_message", "message": "go"}),
            )
            .as_bytes(),
        )
        .unwrap();
        follower.follow(None);
        assert_eq!(follower.last_at(), Some(1_792_630_482_000));
        file.write_all(
            event(
                "2026-10-22T00:55:01.000Z",
                serde_json::json!({"type": "agent_message", "message": "Done."}),
            )
            .as_bytes(),
        )
        .unwrap();
        follower.follow(None);
        assert_eq!(follower.last_at(), Some(1_792_630_501_000));
        // Bookkeeping later on, or an entry that says it's older, isn't a message.
        file.write_all(
            event(
                "2026-10-27T05:35:18.000Z",
                serde_json::json!({"type": "token_count"}),
            )
            .as_bytes(),
        )
        .unwrap();
        file.write_all(
            event(
                "2026-10-21T00:00:00.000Z",
                serde_json::json!({"type": "agent_message", "message": "Old"}),
            )
            .as_bytes(),
        )
        .unwrap();
        follower.follow(None);
        assert_eq!(follower.last_at(), Some(1_792_630_501_000));
    }

    /// A Codex rollout of conversation `THREAD` in `directory`, and its
    /// file, written with [`timed_event`].
    fn codex_rollout(directory: &Path) -> (PathBuf, File) {
        let path = directory.join(format!("rollout-2026-10-23T10-00-00-{THREAD}.jsonl"));
        let file = File::create(&path).unwrap();
        (path, file)
    }

    const THREAD: &str = "019a1b2c-3d4e-7f60-8a9b-0c1d2e3f4a5b";

    /// An event of a Codex rollout, stamped `at` (RFC 3339).
    fn timed_event(file: &mut File, at: &str, payload: serde_json::Value) {
        let entry = serde_json::json!({"timestamp": at, "type": "event_msg", "payload": payload});
        writeln!(file, "{entry}").unwrap();
    }

    #[test]
    fn a_turn_that_ends_with_the_next_ones_start_keeps_its_own_reply() {
        use serde_json::json;
        let directory = tempfile::tempdir().unwrap();
        let (path, mut file) = codex_rollout(directory.path());
        // Already in the log: a whole turn and the start of the next. The
        // log's past: nothing finished in it.
        timed_event(
            &mut file,
            "2026-10-30T10:00:00.000Z",
            json!({"type": "task_started"}),
        );
        timed_event(
            &mut file,
            "2026-10-30T10:00:00.000Z",
            json!({"type": "user_message", "message": "Fix the parser"}),
        );
        timed_event(
            &mut file,
            "2026-10-30T10:00:09.000Z",
            json!({"type": "task_complete",
            "turn_id": "t1", "last_agent_message": "Fixed the parser."}),
        );
        timed_event(
            &mut file,
            "2026-10-30T10:00:09.040Z",
            json!({"type": "task_started"}),
        );
        let mut follower = codex_log(path);
        follower.follow(None);
        assert_eq!(follower.take_finished(), []);

        // As the log grows: a turn finished, and the next started at once.
        timed_event(
            &mut file,
            "2026-10-30T10:00:10.000Z",
            json!({"type": "user_message", "message": "And the docs"}),
        );
        timed_event(
            &mut file,
            "2026-10-30T10:00:20.000Z",
            json!({"type": "task_complete",
            "turn_id": "t2", "last_agent_message": "Updated the docs."}),
        );
        timed_event(
            &mut file,
            "2026-10-30T10:00:20.030Z",
            json!({"type": "task_started"}),
        );
        timed_event(
            &mut file,
            "2026-10-30T10:00:20.030Z",
            json!({"type": "user_message", "message": "Now the tests"}),
        );
        follower.follow(None);
        let finished = follower.take_finished();
        assert_eq!(finished.len(), 1, "{finished:?}");
        assert_eq!(
            (
                finished[0].reply.key.as_str(),
                finished[0].reply.text.as_str()
            ),
            ("t2", "Updated the docs.")
        );
        assert_eq!(finished[0].conversation.as_deref(), Some(THREAD));
        assert_eq!(finished[0].at, Some(1_793_354_420_000));
        assert_eq!(finished[0].ended, Some(1_793_354_420_030));
        assert_eq!(follower.take_finished(), [], "taken once");

        // Steered in mid-task (no task_complete before it): the turn under
        // way has no reply of its own yet, so none is finished.
        timed_event(
            &mut file,
            "2026-10-30T10:00:30.000Z",
            json!({"type": "user_message", "message": "Skip the slow ones"}),
        );
        timed_event(
            &mut file,
            "2026-10-30T10:00:40.000Z",
            json!({"type": "task_complete",
            "turn_id": "t3", "last_agent_message": "Ran the fast tests."}),
        );
        follower.follow(None);
        assert_eq!(follower.take_finished(), []);
        assert_eq!(follower.reply().map(|reply| reply.key.as_str()), Some("t3"));
    }

    #[test]
    fn an_opencode_turn_that_ended_with_the_next_prompt_gives_its_reply_once_it_stays_put() {
        use serde_json::json;
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT,
                   seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER,
                   time_updated INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
                   time_created INTEGER, time_updated INTEGER, data TEXT);",
            )
            .unwrap();
        let row = |id: &str, kind: &str, seq: i64, at: i64, data: serde_json::Value| {
            connection
                .execute(
                    "INSERT INTO session_message VALUES (?1, 'ses_a', ?2, ?3, ?4, ?4, ?5)",
                    rusqlite::params![id, kind, seq, at, data.to_string()],
                )
                .unwrap();
        };
        let mut follower = Follower {
            pid: 1,
            source: Source::OpenCode {
                database: database.clone(),
                cwd: None,
                conversation: None,
            },
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            reply_state: Default::default(),
            last_at: None,
            foreground: None,
        };
        row("u1", "user", 1, 100, json!({"text": "Fix the parser"}));
        follower.follow(Some("ses_a"));
        row(
            "a1",
            "assistant",
            2,
            110,
            json!({"content": [
            {"type": "tool", "name": "edit", "state": {"input": {"filePath": "src/a.rs"}}},
            {"type": "text", "text": "Fixed the parser."}]}),
        );
        follower.follow(Some("ses_a"));
        // The next prompt, at once: the turn before ended with it.
        row("u2", "user", 3, 120, json!({"text": "And the docs"}));
        follower.follow(Some("ses_a"));
        assert_eq!(follower.take_finished(), [], "looked at again first");
        follower.follow(Some("ses_a"));
        let finished = follower.take_finished();
        assert_eq!(finished.len(), 1, "{finished:?}");
        assert_eq!(
            (
                finished[0].reply.key.as_str(),
                finished[0].reply.text.as_str()
            ),
            ("a1:1", "Fixed the parser.")
        );
        assert_eq!(finished[0].reply.position, Some(2 * 1024 + 1));
        assert_eq!((finished[0].at, finished[0].ended), (Some(110), Some(120)));
        assert_eq!(finished[0].conversation.as_deref(), Some("ses_a"));
        follower.follow(Some("ses_a"));
        assert_eq!(follower.take_finished(), [], "once");

        // A turn whose last text came before a tool call it went on with,
        // and the next prompt: it said nothing after, so nothing's finished.
        row(
            "a2",
            "assistant",
            4,
            130,
            json!({"content": [
            {"type": "text", "text": "Let me look at the docs."},
            {"type": "tool", "name": "read", "state": {"input": {"filePath": "README.md"}}}]}),
        );
        row("u3", "user", 5, 140, json!({"text": "Stop, do the tests"}));
        for _ in 0..3 {
            follower.follow(Some("ses_a"));
        }
        assert_eq!(follower.take_finished(), []);
    }

    #[test]
    fn the_follower_keeps_the_turns_reply_and_names_its_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let thread = "019a1b2c-3d4e-7f60-8a9b-0c1d2e3f4a5b";
        let path = directory
            .path()
            .join(format!("rollout-2026-10-23T10-00-00-{thread}.jsonl"));
        let mut file = File::create(&path).unwrap();
        let event = |payload: serde_json::Value| {
            format!(
                "{}\n",
                serde_json::json!({"type": "event_msg", "payload": payload})
            )
        };
        file.write_all(
            event(serde_json::json!({"type": "user_message", "message": "go"})).as_bytes(),
        )
        .unwrap();
        let mut follower = codex_log(path);
        follower.follow(None);
        assert_eq!(follower.reply(), None);
        file.write_all(
            event(serde_json::json!({"type": "task_complete", "turn_id": "t1",
                "last_agent_message": "Done: the tests pass."}))
            .as_bytes(),
        )
        .unwrap();
        follower.follow(None);
        let reply = follower.reply().unwrap();
        assert_eq!(
            (reply.key.as_str(), reply.text.as_str()),
            ("t1", "Done: the tests pass.")
        );
        assert_eq!(follower.conversation().as_deref(), Some(thread));
        assert_eq!(follower.agent(), "codex");
    }

    #[test]
    fn a_long_reply_is_cut_at_a_character_boundary() {
        let reply = bounded("k".into(), "é".repeat(MAX_REPLY_BYTES));
        assert!(reply.truncated);
        assert!(reply.text.len() <= MAX_REPLY_BYTES);
        assert!(reply.text.chars().all(|c| c == 'é'));
        assert!(!bounded("k".into(), "short".into()).truncated);
    }

    #[test]
    fn a_first_line_is_plain_and_bounded() {
        assert_eq!(
            first_line("\n\n## **Checking** the `parser`\nmore"),
            Some("Checking the parser".into())
        );
        assert_eq!(first_line("a\u{1b}[31mb\tc"), Some("a [31mb c".into()));
        assert_eq!(first_line("  \n---\n"), None);
        assert_eq!(first_line(&"x".repeat(500)).unwrap().len(), MAX_LINE);
        assert_eq!(short_path("/repo/tui/src/app.rs"), "src/app.rs");
    }

    #[test]
    fn a_prompt_is_what_was_typed() {
        assert_eq!(
            prompt_line("Fix the parser\nthanks"),
            Some("Fix the parser".into())
        );
        assert_eq!(
            prompt_line("<command-name>/review</command-name>\n<command-args>src</command-args>"),
            Some("/review src".into())
        );
        assert_eq!(
            prompt_line("<task-notification>done</task-notification>"),
            None
        );
        assert_eq!(
            prompt_line("<system-reminder>x</system-reminder>\nShip it"),
            Some("Ship it".into())
        );
        assert_eq!(
            prompt_line(&(paste_block("1f0a", "error: boom") + "\nWhy?")),
            Some("Why?".into())
        );
        assert_eq!(
            prompt_line(&paste_block("1f0a", "error: boom")),
            Some("error: boom".into())
        );
    }

    /// A paste as Claude Code logs it in a prompt: a blank line, then
    /// `<pasted_content id="…">`, the paste on lines of its own, and the
    /// closing tag naming the same id.
    fn paste_block(id: &str, text: &str) -> String {
        format!("\n\n<pasted_content id=\"{id}\">\n{text}\n</pasted_content id=\"{id}\">\n")
    }

    /// The bug behind replies with files never confirmed: a reply of
    /// several lines (its text, a blank line, its files' paths) typed into
    /// Claude Code is logged as one paste block, `</pasted_content id="…">`
    /// closing it; read as an unclosed `<pasted_content>`, the whole prompt
    /// was dropped, so no new turn and no `user_prompt` showed it.
    #[test]
    fn pasted_prompt_bug_a_prompt_that_is_one_paste_is_what_was_pasted() {
        let reply = "Here's what I sent from the web app.\nTwo screenshots:\n\n\
            /home/me/.rakumux/uploads/01JREPLY/image.png\n\
            /home/me/.rakumux/uploads/01JREPLY/image (2).png";
        // Queued while a turn ran: the block alone, no blank line before it.
        let queued =
            format!("<pasted_content id=\"6524\">\n{reply}\n</pasted_content id=\"6524\">");
        assert_eq!(
            prompt_line(&queued),
            Some("Here's what I sent from the web app.".into())
        );
        assert_eq!(typed_text(&queued).as_deref(), Some(reply));
        assert_eq!(
            prompt_text(&queued).as_deref(),
            Some(
                "Here's what I sent from the web app. Two screenshots: \
                 /home/me/.rakumux/uploads/01JREPLY/image.png \
                 /home/me/.rakumux/uploads/01JREPLY/image (2).png"
            )
        );
        // Typed at the prompt: the same, after a blank line.
        let typed = paste_block("a3f9", reply);
        assert_eq!(
            prompt_line(&typed),
            Some("Here's what I sent from the web app.".into())
        );
        assert_eq!(typed_text(&typed).as_deref(), Some(reply));
    }

    #[test]
    fn pasted_prompt_bug_pastes_and_what_was_typed_around_them_are_all_the_users() {
        // Typed, then a paste, then typed again: the line is what was
        // typed; the whole prompt keeps everything, in order.
        let text = format!(
            "Look at this log{}Why does it fail?",
            paste_block("0b1c", "error: boom\n  at parser.rs:12")
        );
        assert_eq!(prompt_line(&text), Some("Look at this log".into()));
        assert_eq!(
            typed_text(&text).as_deref(),
            Some("Look at this log\n\nerror: boom\n  at parser.rs:12\n\nWhy does it fail?")
        );
        // Two pastes, nothing typed: the first paste's first line, and both.
        let two = paste_block("aaaa", "first paste\nline two") + &paste_block("bbbb", "second");
        assert_eq!(prompt_line(&two), Some("first paste".into()));
        assert_eq!(
            typed_text(&two).as_deref(),
            Some("first paste\nline two\n\nsecond")
        );
        // Text between pastes, and a paste's last line ending in a newline.
        let between = paste_block("c0de", "one\n") + "and" + &paste_block("d00d", "two");
        assert_eq!(prompt_line(&between), Some("and".into()));
        assert_eq!(typed_text(&between).as_deref(), Some("one\n\nand\n\ntwo"));
        // Claude Code's own insertions around a paste are still left out.
        let reminded =
            paste_block("e1e1", "the paste") + "<system-reminder>be careful</system-reminder>";
        assert_eq!(prompt_line(&reminded), Some("the paste".into()));
        assert_eq!(typed_text(&reminded).as_deref(), Some("the paste"));
        // Without an id, closed the same way.
        assert_eq!(
            prompt_line("<pasted_content>\nplain\n</pasted_content>"),
            Some("plain".into())
        );
    }

    #[test]
    fn pasted_prompt_bug_a_block_not_closed_as_it_opened_is_text() {
        // Never closed, or closed with another id: text, as Claude Code
        // reads it, not a prompt left empty.
        let open = "<pasted_content id=\"1234\">\nhalf a paste";
        assert_eq!(
            prompt_line(open),
            Some("<pasted_content id=\"1234\">".into())
        );
        let other = "<pasted_content id=\"1234\">\nx\n</pasted_content id=\"9999\">";
        assert_eq!(typed_text(other).as_deref(), Some(other));
        // A tag that only begins the same way.
        assert_eq!(
            prompt_line("<pasted_contents>\nx\n</pasted_contents>"),
            Some("<pasted_contents>".into())
        );
        // An empty paste and nothing else: nothing was sent.
        assert_eq!(prompt_line(&paste_block("ffff", "")), None);
    }

    fn claude_log(path: PathBuf) -> Follower {
        Follower {
            pid: 1,
            source: Source::Log {
                agent: LogAgent::Claude,
                conversation: Some("5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23".into()),
                path,
                offset: 0,
                partial: Vec::new(),
            },
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            reply_state: Default::default(),
            last_at: None,
            foreground: None,
        }
    }

    /// The failure as it happened: a reply with two files, typed into
    /// Claude Code while it worked, logged once it took it as a queued
    /// prompt that is one paste block. The follower must show it as a new
    /// turn whose prompt is the reply's first line (what the account
    /// service matches it by).
    #[test]
    fn pasted_prompt_bug_a_reply_with_files_queued_mid_turn_begins_a_turn_with_its_first_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("conversation.jsonl");
        let mut file = File::create(&path).unwrap();
        let mut write = |entry: serde_json::Value| writeln!(file, "{entry}").unwrap();
        write(serde_json::json!({"type": "user", "uuid": "u1",
            "timestamp": "2026-10-30T10:00:00.000Z",
            "message": {"role": "user", "content": "Fix the parser"}}));
        write(serde_json::json!({"type": "assistant", "uuid": "a1",
            "timestamp": "2026-10-30T10:00:05.000Z",
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t1",
                "name": "Bash", "input": {"command": "cargo test"}}]}}));
        let mut follower = claude_log(path);
        assert!(follower.follow(None));
        assert_eq!(follower.prompt(), Some("Fix the parser"));
        let first = follower.turn().id;

        let reply = "Here's what I sent from the web app.\n\n\
            /home/me/.rakumux/uploads/01JREPLY/image.png\n\
            /home/me/.rakumux/uploads/01JREPLY/image (2).png";
        write(serde_json::json!({"type": "user", "uuid": "u2",
            "timestamp": "2026-10-30T10:00:20.000Z",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}}));
        write(serde_json::json!({"type": "attachment", "uuid": "q1",
            "timestamp": "2026-10-30T10:00:21.000Z",
            "attachment": {"type": "queued_command", "commandMode": "prompt",
                "origin": {"kind": "human"}, "humanTurn": true,
                "prompt": format!("<pasted_content id=\"6524\">\n{reply}\n</pasted_content id=\"6524\">")}}));
        assert!(follower.follow(None));
        assert_eq!(
            follower.prompt(),
            Some("Here's what I sent from the web app.")
        );
        let turn = follower.turn();
        assert_eq!(turn.id, first + 1, "a new turn");
        assert_eq!(
            turn.prompt.as_deref(),
            Some(
                "Here's what I sent from the web app. \
                 /home/me/.rakumux/uploads/01JREPLY/image.png \
                 /home/me/.rakumux/uploads/01JREPLY/image (2).png"
            )
        );
        assert_eq!(turn.total, 0, "nothing said since");

        // The same reply typed while Claude Code waited: a `user` entry.
        write(serde_json::json!({"type": "user", "uuid": "u3",
            "timestamp": "2026-10-30T10:01:00.000Z",
            "message": {"role": "user", "content": paste_block("77ab", "Second reply\nwith two lines")}}));
        assert!(follower.follow(None));
        assert_eq!(follower.prompt(), Some("Second reply"));
        assert_eq!(follower.turn().id, first + 2);
    }

    #[test]
    fn the_follower_reads_new_complete_lines_from_where_it_left_off() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        let entry = |text: &str| {
            format!(
                "{}\n",
                serde_json::json!({"type":"event_msg","payload":{"type":"agent_message","message":text}})
            )
        };
        file.write_all(entry("First reply").as_bytes()).unwrap();
        let mut follower = codex_log(path.clone());
        assert!(follower.follow(None));
        assert_eq!(follower.latest(), Some("First reply"));
        // Nothing new: unchanged.
        assert!(!follower.follow(None));
        // Half a line waits for the rest.
        let next = entry("Second reply");
        let (head, tail) = next.split_at(10);
        file.write_all(head.as_bytes()).unwrap();
        assert!(!follower.follow(None));
        file.write_all(tail.as_bytes()).unwrap();
        assert!(follower.follow(None));
        assert_eq!(follower.latest(), Some("Second reply"));
        // Lines without anything to show keep the last one.
        file.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\"}}\n")
            .unwrap();
        assert!(!follower.follow(None));
        assert_eq!(follower.latest(), Some("Second reply"));
    }

    #[test]
    fn a_turn_starts_at_each_prompt_and_gathers_the_agents_messages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        let event = |kind: &str, key: &str, text: &str| {
            format!(
                "{}\n",
                serde_json::json!({"type":"event_msg","payload":{"type":kind, key:text}})
            )
        };
        let mut follower = codex_log(path.clone());
        file.write_all(event("user_message", "message", "Fix the parser").as_bytes())
            .unwrap();
        file.write_all(event("agent_message", "message", "Looking at it").as_bytes())
            .unwrap();
        assert!(follower.follow(None));
        let first = follower.turn().clone();
        assert_eq!(first.prompt.as_deref(), Some("Fix the parser"));
        assert_eq!(first.messages, ["Looking at it"]);
        assert_eq!(first.total, 1);
        file.write_all(event("agent_message", "message", "Fixed:\n  two lines").as_bytes())
            .unwrap();
        assert!(follower.follow(None));
        assert_eq!(
            follower.turn().messages,
            ["Looking at it", "Fixed: two lines"]
        );
        assert_eq!(follower.turn().total, 2);
        // The next prompt starts the next turn.
        file.write_all(event("user_message", "message", "Now the docs").as_bytes())
            .unwrap();
        assert!(follower.follow(None));
        assert_ne!(follower.turn().id, first.id);
        assert_eq!(follower.turn().prompt.as_deref(), Some("Now the docs"));
        assert_eq!(follower.turn().total, 0);
    }

    #[test]
    fn a_long_log_is_read_from_its_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        let filler = format!(
            "{}\n",
            serde_json::json!({"type":"x","pad":"y".repeat(1000)})
        );
        for _ in 0..400 {
            file.write_all(filler.as_bytes()).unwrap();
        }
        let last = serde_json::json!({"type":"event_msg","payload":{"type":"agent_message","message":"At the end"}});
        writeln!(file, "{last}").unwrap();
        let mut follower = codex_log(path);
        assert!(follower.follow(None));
        assert_eq!(follower.latest(), Some("At the end"));
    }

    /// A Codex rollout line: an event of `kind` with `message`.
    fn codex_event(kind: &str, message: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type":"event_msg","payload":{"type":kind,"message":message}})
        )
    }

    #[test]
    fn a_long_logs_turn_keeps_its_prompt_from_before_the_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(codex_event("user_message", "An earlier prompt").as_bytes())
            .unwrap();
        file.write_all(codex_event("agent_message", "An earlier reply").as_bytes())
            .unwrap();
        file.write_all(codex_event("user_message", "Fix the flaky login test").as_bytes())
            .unwrap();
        file.write_all(codex_event("agent_message", "Reading the test").as_bytes())
            .unwrap();
        // Tool output the agent read: more than the tail.
        let output = format!(
            "{}\n",
            serde_json::json!({"type":"response_item",
                "payload":{"type":"function_call_output","output":"y".repeat(1000)}})
        );
        for _ in 0..400 {
            file.write_all(output.as_bytes()).unwrap();
        }
        file.write_all(codex_event("agent_message", "Fixed: it waits for the form").as_bytes())
            .unwrap();
        // A new follower (the daemon restarted in place, say) reads the tail,
        // and the turn it's in from its prompt.
        let mut follower = codex_log(path);
        assert!(follower.follow(None));
        let turn = follower.turn().clone();
        assert_eq!(turn.prompt.as_deref(), Some("Fix the flaky login test"));
        assert_eq!(
            turn.messages,
            ["Reading the test", "Fixed: it waits for the form"]
        );
        assert_eq!(turn.total, 2);
        assert_eq!(follower.prompt(), Some("Fix the flaky login test"));
        // Read once: nothing new is nothing more.
        assert!(!follower.follow(None));
        assert_eq!(follower.turn(), &turn);
    }

    #[test]
    fn a_tail_with_its_prompt_is_read_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(codex_event("user_message", "An earlier prompt").as_bytes())
            .unwrap();
        for _ in 0..400 {
            file.write_all(codex_event("agent_message", &"y".repeat(1000)).as_bytes())
                .unwrap();
        }
        file.write_all(codex_event("user_message", "Now the docs").as_bytes())
            .unwrap();
        file.write_all(codex_event("agent_message", "Writing them").as_bytes())
            .unwrap();
        let mut follower = codex_log(path);
        assert!(follower.follow(None));
        assert_eq!(follower.turn().prompt.as_deref(), Some("Now the docs"));
        assert_eq!(follower.turn().messages, ["Writing them"]);
        // Only the tail was read: the earlier turn's messages weren't seen.
        assert_eq!(follower.turn().id, 1);
    }

    #[test]
    fn a_tail_without_its_prompt_nearby_is_a_turn_without_one() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(codex_event("user_message", "Too far back").as_bytes())
            .unwrap();
        let output = format!(
            "{}\n",
            serde_json::json!({"type":"response_item",
                "payload":{"type":"function_call_output","output":"y".repeat(64 * 1024)}})
        );
        let lines = (MAX_PROMPT_SEARCH + MAX_READ) / output.len() as u64 + 2;
        for _ in 0..lines {
            file.write_all(output.as_bytes()).unwrap();
        }
        file.write_all(codex_event("agent_message", "Done").as_bytes())
            .unwrap();
        let mut follower = codex_log(path);
        assert!(follower.follow(None));
        assert_eq!(follower.turn().prompt, None);
        assert_eq!(follower.turn().messages, ["Done"]);
        assert!(!follower.follow(None));
        assert_eq!(follower.turn().total, 1);
    }
}
