//! Writing back from the feed (`r`): only a session's newest card takes a
//! reply (`FeedStore::reply_to`). The account service stores it on
//! rakumux.com (`AccountAccess::feed_reply`), and the entry's machine types
//! it into the entry's session once its agent waits for input; this machine
//! at once, when the session runs here. The agent's next reply there answers
//! it, and the detail shows them as a thread. A prompt still waiting for its
//! answer replies to the session's newest entry in its chat, or with none
//! writes to the session itself (`AccountAccess::session_message`), typed in
//! the same way. Each reply has an id made here, sent again with the same
//! text after a failure, so one rakumux.com stored before the answer was lost
//! (a time-out) isn't stored, and typed in, twice.

use rakumux_sensagent::feed::{ReplyWrite, ReplyWriteStatus};
use session_core::ids::ReplyId;

use super::{Card, ReplyTo};
use crate::app::runtime::AppEvent;
use crate::app::types::{AccountStatus, AppMode, FeedReplyDialog};
use crate::app::{App, TextInput};

/// The longest reply rakumux.com takes, in UTF-8 bytes.
const MAX_REPLY_BYTES: usize = 8 * 1024;

impl App {
    /// Open the reply box for the selected card.
    pub fn open_feed_reply(&mut self) {
        if !matches!(self.account, AccountStatus::SignedIn(_)) {
            self.set_status("Sign in to reply: Settings → Account.");
            return;
        }
        let Some(card) = self.feed.selected_card() else {
            self.set_status("Select a reply to answer.");
            return;
        };
        if let Card::Entry(entry) = card {
            if entry.seq.is_none() {
                self.set_status("It's still being sent; answer it in a moment.");
                return;
            }
            if entry.conversation_id.is_none() {
                self.set_status("Its agent's chat isn't known, so it can't be answered.");
                return;
            }
        }
        let Some(to) = self.feed.reply_to(card) else {
            self.set_status("Only a session's newest message can be answered: reply to that one.");
            return;
        };
        // What the card shows: the answer, or the prompt still waiting for one.
        let shown = match card {
            Card::Entry(entry) => &entry.text,
            Card::Open(open) => &open.prompt.text,
        };
        let answering = super::plain(shown)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_owned();
        let machine = (card.machine() != self.feed.machine()).then(|| super::plain(card.machine()));
        self.mode = AppMode::FeedReply(FeedReplyDialog {
            to,
            session_name: super::plain(card.session_name()),
            machine,
            answering,
            input: TextInput::new(""),
            error_message: None,
            busy: false,
            sent: None,
        });
    }

    pub(in crate::app) fn submit_feed_reply(&mut self) {
        let AppMode::FeedReply(dialog) = &mut self.mode else {
            return;
        };
        if dialog.busy {
            return;
        }
        let text = dialog.input.value().trim().to_owned();
        if text.is_empty() {
            dialog.error_message = Some("Write something to send.".into());
            return;
        }
        if text.len() > MAX_REPLY_BYTES {
            dialog.error_message = Some("That's longer than rakumux.com takes (8 KB).".into());
            return;
        }
        dialog.busy = true;
        dialog.error_message = None;
        let id = match &dialog.sent {
            Some((id, sent)) if *sent == text => id.clone(),
            _ => ReplyId::generate().as_str().to_owned(),
        };
        dialog.sent = Some((id.clone(), text.clone()));
        let to = dialog.to.clone();
        let generation = self.feed.request_generation;
        let access = self.account_access.clone();
        self.exec.job(move || AppEvent::FeedReplied {
            generation,
            result: match &to {
                ReplyTo::Entry(entry) => access.feed_reply(entry, &text, &id),
                ReplyTo::Session { machine, session } => {
                    access.session_message(machine, session, &text, &id)
                }
            },
        });
    }

    pub(in crate::app) fn on_feed_replied(
        &mut self,
        generation: u64,
        result: Result<ReplyWrite, String>,
    ) {
        let AppMode::FeedReply(dialog) = &mut self.mode else {
            return;
        };
        if !dialog.busy {
            return;
        }
        dialog.busy = false;
        let written = match result {
            Ok(written) => written,
            Err(_) => {
                dialog.error_message = Some("The account service isn't running.".into());
                return;
            }
        };
        let failure = match written.status {
            ReplyWriteStatus::Sent => None,
            ReplyWriteStatus::SignedOut => Some("You're signed out: sign in again to reply."),
            ReplyWriteStatus::Off => Some("The Agent feed is off: turn it on in Settings."),
            ReplyWriteStatus::Invalid => Some("rakumux.com refused it."),
            ReplyWriteStatus::Unavailable | ReplyWriteStatus::Unknown => {
                Some("Couldn't reach rakumux.com. Try again.")
            }
        };
        if let Some(failure) = failure {
            dialog.error_message = Some(failure.into());
            return;
        }
        let place = match &dialog.machine {
            None => format!(
                "Sent: it goes into {} when its agent waits for you.",
                dialog.session_name
            ),
            Some(machine) => format!(
                "Sent to {machine}: it goes into {} there when its agent waits, if {machine} \
                 takes replies from rakumux.com.",
                dialog.session_name
            ),
        };
        self.mode = AppMode::Normal;
        if generation == self.feed.request_generation {
            if let Some(reply) = written.reply {
                self.feed.take_reply(reply);
            }
        }
        self.set_status(place);
    }
}
