//! Permission policy, stage B: agents drive sessions.
//!
//! - User callers may do everything.
//! - Session callers (an agent in a session, directly or through
//!   `rakumux-mcp`) reach every folder of this machine. They may
//!   list/get/read/wait/subscribe on any session (`all` too), list folders,
//!   create sessions in any folder (within `Limits`, checked by the service),
//!   bind their agent's conversations to themselves (for `Auth::Delegated`),
//!   **drive** any other session (send keys, submit text, kill, revive), and
//!   report/touch/rename/pin **themselves**. A session never drives itself
//!   (`PermissionDenied`): its own keys would go into the agent asking.
//!   Deleting sessions, attaching, answering an agent's question dialog,
//!   reading an agent's conversation, the session history and managing
//!   folders stay the user's. Other machines' sessions are the user's alone
//!   (`Caller::reaches_machines`).
//! - Heartbeats: session callers list, read, create and change them in any
//!   folder (what they make or change waits for the user's approval, paused:
//!   the service sees to that); enabling, running and deleting one are the
//!   user's.

use crate::control_api::Caller;
use session_core::api::{ApiError, CallerInfo, ErrorCode};
use session_core::ids::SessionId;

/// Operations subject to policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Get,
    Create,
    Import,
    Rename,
    Kill,
    Delete,
    /// List the session history.
    History,
    /// Bring a deleted session back.
    Restore,
    Revive,
    Report,
    Read,
    Turn,
    Transcript,
    Wait,
    Send,
    Submit,
    Answer,
    Touch,
    Pin,
    Bind,
    Attach,
    TerminalOpen,
    TerminalAttach,
    ProjectList,
    ProjectAdd,
    ProjectRemove,
    ProjectRelocate,
    ReplyCapture,
    ReplyList,
    HeartbeatList,
    HeartbeatGet,
    HeartbeatCreate,
    HeartbeatUpdate,
    HeartbeatEnable,
    HeartbeatDelete,
    HeartbeatRun,
}

impl Op {
    /// Method name used in the audit log and error messages.
    pub fn method(self) -> &'static str {
        match self {
            Op::Get => "session.get",
            Op::Create => "session.create",
            Op::Import => "session.import",
            Op::Rename => "session.rename",
            Op::Kill => "session.kill",
            Op::Delete => "session.delete",
            Op::History => "session.history",
            Op::Restore => "session.restore",
            Op::Revive => "session.revive",
            Op::Report => "session.report",
            Op::Read => "session.read",
            Op::Turn => "session.turn",
            Op::Transcript => "session.transcript",
            Op::Wait => "session.wait",
            Op::Send => "session.send",
            Op::Submit => "session.submit",
            Op::Answer => "session.answer",
            Op::Touch => "session.touch",
            Op::Pin => "session.pin",
            Op::Bind => "session.bind",
            Op::Attach => "attach",
            Op::TerminalOpen => "terminal.open",
            Op::TerminalAttach => "terminal.attach",
            Op::ProjectList => "project.list",
            Op::ProjectAdd => "project.add",
            Op::ProjectRemove => "project.remove",
            Op::ProjectRelocate => "project.relocate",
            Op::ReplyCapture => "reply.capture",
            Op::ReplyList => "reply.list",
            Op::HeartbeatList => "heartbeat.list",
            Op::HeartbeatGet => "heartbeat.get",
            Op::HeartbeatCreate => "heartbeat.create",
            Op::HeartbeatUpdate => "heartbeat.update",
            Op::HeartbeatEnable => "heartbeat.enable",
            Op::HeartbeatDelete => "heartbeat.delete",
            Op::HeartbeatRun => "heartbeat.run",
        }
    }

    /// Mutating ops are audited whether they succeed or fail.
    pub fn is_mutating(self) -> bool {
        matches!(
            self,
            Op::Create
                | Op::TerminalOpen
                | Op::TerminalAttach
                | Op::Import
                | Op::Rename
                | Op::Kill
                | Op::Delete
                | Op::Restore
                | Op::Revive
                | Op::Report
                | Op::Send
                | Op::Submit
                | Op::Answer
                | Op::Touch
                | Op::Pin
                | Op::Bind
                | Op::ProjectAdd
                | Op::ProjectRemove
                | Op::ProjectRelocate
                | Op::ReplyCapture
                | Op::HeartbeatCreate
                | Op::HeartbeatUpdate
                | Op::HeartbeatEnable
                | Op::HeartbeatDelete
                | Op::HeartbeatRun
        )
    }
}

/// The session an op acts on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target<'a> {
    pub id: &'a SessionId,
}

/// Decide whether `caller` may perform `op` on `target` (None for ops
/// without a target session, such as create, a scope-wide list or the
/// project ops).
pub(crate) fn authorize(
    caller: &Caller,
    op: Op,
    target: Option<Target<'_>>,
) -> Result<(), ApiError> {
    let self_id = match &caller.info {
        CallerInfo::User => return Ok(()),
        CallerInfo::Session { id, .. } => id,
    };
    let on_self = target.is_some_and(|target| target.id == self_id);
    match op {
        Op::Get | Op::Read | Op::Wait | Op::ProjectList => Ok(()),
        // What a session makes or changes waits for approval (the service).
        Op::HeartbeatList | Op::HeartbeatGet | Op::HeartbeatCreate | Op::HeartbeatUpdate => Ok(()),
        Op::HeartbeatEnable | Op::HeartbeatDelete | Op::HeartbeatRun => Err(denied(
            op,
            "heartbeats are enabled, run and deleted by the user",
        )),
        // In any folder: the service applies the limits.
        Op::Create => Ok(()),
        // Binds conversations to the caller itself (the service checks).
        Op::Bind => Ok(()),
        Op::Report | Op::Touch | Op::Rename | Op::Pin => match on_self {
            true => Ok(()),
            false => Err(denied(op, "a session may only do this to itself")),
        },
        Op::Send | Op::Submit | Op::Kill | Op::Revive => match on_self {
            true => Err(denied(op, "a session doesn't drive itself")),
            false => Ok(()),
        },
        Op::Import => Err(denied(op, "importing saved sessions is for user clients")),
        Op::History | Op::Restore => Err(denied(op, "the session history is for user clients")),
        Op::Turn | Op::Transcript => Err(denied(op, "an agent's conversation is for user clients")),
        Op::Answer => Err(denied(
            op,
            "answering an agent's question is for user clients",
        )),
        Op::TerminalOpen | Op::TerminalAttach => {
            Err(denied(op, "temporary terminals are for user clients"))
        }
        Op::ProjectAdd | Op::ProjectRemove | Op::ProjectRelocate => {
            Err(denied(op, "folders are managed by the user"))
        }
        Op::ReplyCapture | Op::ReplyList => Err(denied(op, "agent replies are for user clients")),
        Op::Delete => Err(denied(op, "deleting sessions is for user clients")),
        Op::Attach => Err(denied(op, "attaching to a terminal is for user clients")),
    }
}

fn denied(op: Op, why: &str) -> ApiError {
    ApiError::new(
        ErrorCode::PermissionDenied,
        format!(
            "{} is not allowed from inside a session: {why}",
            op.method()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::model::ScopeRoot;

    fn session_caller(id: &str, scope: &str) -> Caller {
        Caller {
            max_blocking_ms: None,
            info: CallerInfo::Session {
                id: SessionId::from_string(id),
                scope: ScopeRoot::from_canonical(scope.into()),
            },
            client: "cli".into(),
        }
    }

    const ALL: [Op; 36] = [
        Op::TerminalOpen,
        Op::TerminalAttach,
        Op::Get,
        Op::Create,
        Op::Import,
        Op::Rename,
        Op::Kill,
        Op::Delete,
        Op::History,
        Op::Restore,
        Op::Revive,
        Op::Report,
        Op::Read,
        Op::Turn,
        Op::Transcript,
        Op::Wait,
        Op::Send,
        Op::Submit,
        Op::Answer,
        Op::Touch,
        Op::Pin,
        Op::Bind,
        Op::Attach,
        Op::ProjectList,
        Op::ProjectAdd,
        Op::ProjectRemove,
        Op::ProjectRelocate,
        Op::ReplyCapture,
        Op::ReplyList,
        Op::HeartbeatList,
        Op::HeartbeatGet,
        Op::HeartbeatCreate,
        Op::HeartbeatUpdate,
        Op::HeartbeatEnable,
        Op::HeartbeatDelete,
        Op::HeartbeatRun,
    ];

    #[test]
    fn users_may_do_everything() {
        let user = Caller {
            max_blocking_ms: None,
            info: CallerInfo::User,
            client: "tui".into(),
        };
        let id = SessionId::from_string("X");
        for op in ALL {
            assert!(
                authorize(&user, op, Some(Target { id: &id })).is_ok(),
                "{op:?}"
            );
            assert!(authorize(&user, op, None).is_ok(), "{op:?}");
        }
    }

    #[test]
    fn session_matrix() {
        // Whatever folder the other session is in: the target names none.
        let me = session_caller("ME", "/s");
        let me_id = SessionId::from_string("ME");
        let other = SessionId::from_string("OTHER");
        for op in ALL {
            let on_self = authorize(&me, op, Some(Target { id: &me_id }));
            let on_other = authorize(&me, op, Some(Target { id: &other }));
            let anywhere = matches!(
                op,
                Op::Get
                    | Op::Read
                    | Op::Wait
                    | Op::ProjectList
                    | Op::Create
                    | Op::Bind
                    | Op::HeartbeatList
                    | Op::HeartbeatGet
                    | Op::HeartbeatCreate
                    | Op::HeartbeatUpdate
            );
            let self_only = matches!(op, Op::Report | Op::Touch | Op::Rename | Op::Pin);
            let drives = matches!(op, Op::Send | Op::Submit | Op::Kill | Op::Revive);
            assert_eq!(on_self.is_ok(), anywhere || self_only, "{op:?} on self");
            assert_eq!(on_other.is_ok(), anywhere || drives, "{op:?} on other");
            for result in [on_self, on_other] {
                if let Err(e) = result {
                    assert_eq!(e.code, ErrorCode::PermissionDenied, "{op:?}");
                }
            }
        }
        let error = authorize(&me, Op::Submit, Some(Target { id: &me_id })).unwrap_err();
        assert!(
            error.message.contains("doesn't drive itself"),
            "{}",
            error.message
        );
        for op in [
            Op::Pin,
            Op::ProjectAdd,
            Op::ProjectRemove,
            Op::Delete,
            Op::Attach,
        ] {
            assert_eq!(
                authorize(&me, op, None).unwrap_err().code,
                ErrorCode::PermissionDenied,
                "{op:?}"
            );
        }
        assert!(authorize(&me, Op::ProjectList, None).is_ok());
        assert!(authorize(&me, Op::Create, None).is_ok());
    }
}
