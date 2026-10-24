//! Permission policy, stage A (0.6.0).
//!
//! - User callers may do everything.
//! - Session callers never leave their own scope (`OutOfScope`, also for a
//!   list or subscription of every scope, `all`). Within it
//!   they may list/get/read/wait/subscribe on any session, list projects
//!   (they see their own), create sessions (within `Limits`, checked by the
//!   service), bind their agent's conversations to themselves (for
//!   `Auth::Delegated`), and report/touch/rename/pin **themselves**. Adding and
//!   removing projects is the user's. Everything else (send, submit, kill,
//!   delete, revive, attach, acting on others) is `PermissionDenied` until
//!   agent control of other sessions ships (stage B).

use crate::control_api::Caller;
use session_core::api::{ApiError, CallerInfo, ErrorCode};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;

/// Operations subject to policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    List,
    Get,
    Create,
    Import,
    Rename,
    Kill,
    Delete,
    Revive,
    Report,
    Read,
    Turn,
    Wait,
    Send,
    Submit,
    Touch,
    Pin,
    Bind,
    Attach,
    TerminalOpen,
    TerminalAttach,
    Subscribe,
    ProjectList,
    ProjectAdd,
    ProjectRemove,
    ProjectRelocate,
    ReplyCapture,
    ReplyList,
}

impl Op {
    /// Method name used in the audit log and error messages.
    pub fn method(self) -> &'static str {
        match self {
            Op::List => "session.list",
            Op::Get => "session.get",
            Op::Create => "session.create",
            Op::Import => "session.import",
            Op::Rename => "session.rename",
            Op::Kill => "session.kill",
            Op::Delete => "session.delete",
            Op::Revive => "session.revive",
            Op::Report => "session.report",
            Op::Read => "session.read",
            Op::Turn => "session.turn",
            Op::Wait => "session.wait",
            Op::Send => "session.send",
            Op::Submit => "session.submit",
            Op::Touch => "session.touch",
            Op::Pin => "session.pin",
            Op::Bind => "session.bind",
            Op::Attach => "attach",
            Op::TerminalOpen => "terminal.open",
            Op::TerminalAttach => "terminal.attach",
            Op::Subscribe => "subscribe",
            Op::ProjectList => "project.list",
            Op::ProjectAdd => "project.add",
            Op::ProjectRemove => "project.remove",
            Op::ProjectRelocate => "project.relocate",
            Op::ReplyCapture => "reply.capture",
            Op::ReplyList => "reply.list",
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
                | Op::Revive
                | Op::Report
                | Op::Send
                | Op::Submit
                | Op::Touch
                | Op::Pin
                | Op::Bind
                | Op::ProjectAdd
                | Op::ProjectRemove
                | Op::ProjectRelocate
                | Op::ReplyCapture
        )
    }
}

/// The session an op acts on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target<'a> {
    pub id: &'a SessionId,
    pub scope: &'a ScopeRoot,
}

/// Decide whether `caller` may perform `op` on `target` (None for ops
/// without a target session, such as create, a scope-wide list or the
/// project ops). Which projects a session caller sees is the service's
/// filter ([`check_scope`]).
pub(crate) fn authorize(
    caller: &Caller,
    op: Op,
    target: Option<Target<'_>>,
) -> Result<(), ApiError> {
    let (self_id, own_scope) = match &caller.info {
        CallerInfo::User => return Ok(()),
        CallerInfo::Session { id, scope } => (id, scope),
    };
    if let Some(target) = target {
        if target.scope != own_scope {
            return Err(out_of_scope());
        }
    }
    match op {
        Op::List | Op::Get | Op::Read | Op::Wait | Op::Subscribe | Op::ProjectList => Ok(()),
        // In the caller's own project: the service creates it there and
        // applies the limits.
        Op::Create => Ok(()),
        // Binds conversations to the caller itself (the service checks).
        Op::Bind => Ok(()),
        Op::Report | Op::Touch | Op::Rename | Op::Pin => match target {
            Some(target) if target.id == self_id => Ok(()),
            _ => Err(denied(op, "a session may only do this to itself")),
        },
        Op::Import => Err(denied(op, "importing saved sessions is for user clients")),
        Op::Turn => Err(denied(op, "an agent's conversation is for user clients")),
        Op::TerminalOpen | Op::TerminalAttach => {
            Err(denied(op, "temporary terminals are for user clients"))
        }
        Op::ProjectAdd | Op::ProjectRemove | Op::ProjectRelocate => {
            Err(denied(op, "folders are managed by the user"))
        }
        Op::ReplyCapture | Op::ReplyList => Err(denied(op, "agent replies are for user clients")),
        Op::Kill | Op::Delete | Op::Revive | Op::Send | Op::Submit | Op::Attach => Err(denied(
            op,
            "agent control of other sessions arrives in a later release",
        )),
    }
}

/// Check a scope named by a session caller (list/subscribe/create).
pub(crate) fn check_scope(caller: &Caller, scope: &ScopeRoot) -> Result<(), ApiError> {
    match &caller.info {
        CallerInfo::User => Ok(()),
        CallerInfo::Session { scope: own, .. } if own == scope => Ok(()),
        CallerInfo::Session { .. } => Err(out_of_scope()),
    }
}

/// Check a list/subscribe of every scope (`all`): user callers only, since a
/// session caller never sees past its own scope.
pub(crate) fn check_all_scopes(caller: &Caller) -> Result<(), ApiError> {
    match &caller.info {
        CallerInfo::User => Ok(()),
        CallerInfo::Session { .. } => Err(ApiError::new(
            ErrorCode::OutOfScope,
            "a session sees only its own folder; `all` (every folder) is for user clients",
        )),
    }
}

fn out_of_scope() -> ApiError {
    ApiError::new(
        ErrorCode::OutOfScope,
        "that session belongs to another folder than the calling session",
    )
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

    const ALL: [Op; 24] = [
        Op::TerminalOpen,
        Op::TerminalAttach,
        Op::List,
        Op::Get,
        Op::Create,
        Op::Import,
        Op::Rename,
        Op::Kill,
        Op::Delete,
        Op::Revive,
        Op::Report,
        Op::Read,
        Op::Wait,
        Op::Send,
        Op::Submit,
        Op::Touch,
        Op::Pin,
        Op::Bind,
        Op::Attach,
        Op::Subscribe,
        Op::ProjectList,
        Op::ProjectAdd,
        Op::ProjectRemove,
        Op::ProjectRelocate,
    ];

    #[test]
    fn users_may_do_everything() {
        let user = Caller {
            max_blocking_ms: None,
            info: CallerInfo::User,
            client: "tui".into(),
        };
        let id = SessionId::from_string("X");
        let scope = ScopeRoot::from_canonical("/anywhere".into());
        for op in ALL {
            let target = Target {
                id: &id,
                scope: &scope,
            };
            assert!(authorize(&user, op, Some(target)).is_ok(), "{op:?}");
        }
    }

    #[test]
    fn session_matrix() {
        let me = session_caller("ME", "/s");
        let me_id = SessionId::from_string("ME");
        let other = SessionId::from_string("OTHER");
        let scope = ScopeRoot::from_canonical("/s".into());
        let elsewhere = ScopeRoot::from_canonical("/t".into());
        for op in ALL {
            let on_self = authorize(
                &me,
                op,
                Some(Target {
                    id: &me_id,
                    scope: &scope,
                }),
            );
            let on_other = authorize(
                &me,
                op,
                Some(Target {
                    id: &other,
                    scope: &scope,
                }),
            );
            let outside = authorize(
                &me,
                op,
                Some(Target {
                    id: &other,
                    scope: &elsewhere,
                }),
            );
            assert_eq!(outside.unwrap_err().code, ErrorCode::OutOfScope, "{op:?}");
            let read_only = matches!(
                op,
                Op::List | Op::Get | Op::Read | Op::Wait | Op::Subscribe | Op::ProjectList
            );
            // Create and bind have no target session; allowed either way.
            let anywhere_in_scope = read_only || op == Op::Create || op == Op::Bind;
            let self_only = matches!(op, Op::Report | Op::Touch | Op::Rename | Op::Pin);
            assert_eq!(
                on_self.is_ok(),
                anywhere_in_scope || self_only,
                "{op:?} on self"
            );
            assert_eq!(on_other.is_ok(), anywhere_in_scope, "{op:?} on other");
            if let Err(e) = on_other {
                assert_eq!(e.code, ErrorCode::PermissionDenied);
            }
        }
        for op in [Op::Pin, Op::ProjectAdd, Op::ProjectRemove] {
            assert_eq!(
                authorize(&me, op, None).unwrap_err().code,
                ErrorCode::PermissionDenied,
                "{op:?}"
            );
        }
        assert!(authorize(&me, Op::ProjectList, None).is_ok());
        assert!(authorize(&me, Op::Create, None).is_ok());
        assert!(check_scope(&me, &scope).is_ok());
        assert_eq!(
            check_scope(&me, &elsewhere).unwrap_err().code,
            ErrorCode::OutOfScope
        );
    }

    #[test]
    fn only_users_watch_every_scope() {
        let user = Caller {
            max_blocking_ms: None,
            info: CallerInfo::User,
            client: "tui".into(),
        };
        assert!(check_all_scopes(&user).is_ok());
        let err = check_all_scopes(&session_caller("ME", "/s")).unwrap_err();
        assert_eq!(err.code, ErrorCode::OutOfScope);
        assert!(err.message.contains("own folder"), "{}", err.message);
    }
}
