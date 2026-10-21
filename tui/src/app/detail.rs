//! The right pane's state: which session it shows, how (live attachment,
//! saved screen, placeholder), and the terminal view mirroring it.

use std::sync::atomic::Ordering;

use session_core::api::{Role, StreamCommand};
use session_core::client::AttachSender;
use session_core::ids::SessionId;
use session_core::model::SessionStatus;

use super::runtime::CancelFlag;
use crate::terminal_view::TerminalView;

/// What the detail pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DetailView {
    /// "+ New session" is selected, or there are no sessions.
    #[default]
    Empty,
    /// Attach or read in flight.
    Loading(SessionId),
    /// Live terminal through an attachment.
    Live(SessionId),
    /// A dead or exited session's saved/final screen.
    Saved {
        id: SessionId,
        status: SessionStatus,
        exit_code: Option<i32>,
    },
    /// The session this TUI runs inside (nesting guard).
    SelfSession(SessionId),
    Error {
        id: SessionId,
        message: String,
    },
}

impl DetailView {
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            DetailView::Empty => None,
            DetailView::Loading(id) | DetailView::Live(id) | DetailView::SelfSession(id) => {
                Some(id)
            }
            DetailView::Saved { id, .. } | DetailView::Error { id, .. } => Some(id),
        }
    }
}

/// A live attach stream. Dropping it detaches and stops its pump.
pub struct Attachment<Id = SessionId> {
    pub id: Id,
    pub generation: u64,
    pub role: Role,
    sender: AttachSender,
    cancel: CancelFlag,
    /// Offset of the next output byte expected.
    pub offset: u64,
    /// `(cols, rows)` last asked for (Resize as controller, Fit as
    /// observer).
    pub sent_size: Option<(u16, u16)>,
    /// When this window last claimed the terminal's size because its user
    /// did something (`App::claim_terminal_size`).
    pub last_claim: Option<std::time::Instant>,
    /// Roles asked for with Promote/Demote whose `RoleChanged` hasn't come
    /// back yet: those events are this client's own, not a takeover.
    awaited_roles: std::collections::VecDeque<Role>,
}

impl<Id> Attachment<Id> {
    pub fn new(
        id: Id,
        generation: u64,
        role: Role,
        sender: AttachSender,
        cancel: CancelFlag,
        offset: u64,
    ) -> Self {
        Self {
            id,
            generation,
            role,
            sender,
            cancel,
            offset,
            sent_size: None,
            last_claim: None,
            awaited_roles: std::collections::VecDeque::new(),
        }
    }

    /// Ask the daemon to make this attachment the controller (`Promote`)
    /// or an observer (`Demote`); the `RoleChanged` it answers with is
    /// then recognised by [`Attachment::own_role_change`].
    pub fn change_role(
        &mut self,
        command: StreamCommand,
    ) -> Result<(), session_core::api::ApiError> {
        let role = match &command {
            StreamCommand::Promote { .. } => Some(Role::Controller),
            StreamCommand::Demote => Some(Role::Observer),
            _ => None,
        };
        self.sender.command(command)?;
        if let Some(role) = role {
            self.awaited_roles.push_back(role);
        }
        Ok(())
    }

    /// Whether a `RoleChanged { role }` answers this client's own Promote or
    /// Demote. Anything else means another client took or gave up control,
    /// and earlier requests no longer matter.
    pub fn own_role_change(&mut self, role: Role) -> bool {
        if self.awaited_roles.front() == Some(&role) {
            self.awaited_roles.pop_front();
            true
        } else {
            self.awaited_roles.clear();
            false
        }
    }

    pub fn input(&self, bytes: Vec<u8>) -> Result<(), session_core::api::ApiError> {
        self.sender.input(bytes)
    }

    pub fn command(&self, command: StreamCommand) -> Result<(), session_core::api::ApiError> {
        self.sender.command(command)
    }
}

impl<Id> Drop for Attachment<Id> {
    fn drop(&mut self) {
        let _ = self.sender.command(StreamCommand::Detach);
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// How a chunk of output relates to what the view has already applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputAction {
    /// Feed the chunk from this byte index on.
    Feed { skip: usize },
    /// Already applied (e.g. replayed after a checkpoint).
    Duplicate,
    /// Bytes are missing before this chunk; the view must resync.
    Gap,
}

/// `expected` = next offset the view needs; `offset` = start of the chunk.
pub fn classify_output(expected: u64, offset: u64, len: usize) -> OutputAction {
    let end = offset + len as u64;
    if offset > expected {
        OutputAction::Gap
    } else if end <= expected {
        OutputAction::Duplicate
    } else {
        OutputAction::Feed {
            skip: (expected - offset) as usize,
        }
    }
}

#[derive(Default)]
pub struct Detail {
    pub view: DetailView,
    pub terminal: Option<TerminalView>,
    pub attachment: Option<Attachment>,
    /// Bumped whenever the pane is retargeted; stale results are dropped.
    pub generation: u64,
    /// Last title the program set (OSC 0/2).
    pub title: Option<String>,
}

impl Detail {
    /// Forget the current target (detaches) and return a new generation.
    pub fn retarget(&mut self, view: DetailView) -> u64 {
        self.attachment = None;
        self.terminal = None;
        self.title = None;
        self.view = view;
        self.generation += 1;
        self.generation
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.view.session_id()
    }

    /// The live attachment for `id`, if any.
    /// The attachment when it is to session `id`.
    pub fn attachment_for_mut(&mut self, id: &SessionId) -> Option<&mut Attachment> {
        self.attachment.as_mut().filter(|a| &a.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::api::Attached;
    use session_core::client::{AttachOutgoing, AttachStream};

    #[test]
    fn output_offsets_are_classified() {
        assert_eq!(classify_output(10, 10, 5), OutputAction::Feed { skip: 0 });
        assert_eq!(classify_output(10, 8, 5), OutputAction::Feed { skip: 2 });
        assert_eq!(classify_output(10, 5, 5), OutputAction::Duplicate);
        assert_eq!(classify_output(10, 11, 5), OutputAction::Gap);
    }

    #[test]
    fn dropping_an_attachment_detaches_and_cancels() {
        let attached = Attached {
            session: crate::app::testing::view("S", "s", 0),
            role: Role::Observer,
            cols: 80,
            rows: 24,
            checkpoint: String::new(),
            offset: 0,
        };
        let (stream, peer) = AttachStream::pair(attached);
        let cancel = super::super::runtime::cancel_flag();
        let mut detail = Detail {
            attachment: Some(Attachment::new(
                SessionId::from_string("S"),
                1,
                Role::Observer,
                stream.sender(),
                cancel.clone(),
                0,
            )),
            ..Default::default()
        };
        detail.retarget(DetailView::Empty);
        assert_eq!(
            peer.outgoing.try_recv().unwrap(),
            AttachOutgoing::Command(StreamCommand::Detach)
        );
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(detail.generation, 1);
    }
}
