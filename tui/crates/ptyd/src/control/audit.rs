//! Audit entries: every mutating op and every denial is recorded in the
//! registry's `audit_log` table (pruned to 30 days when the daemon starts).
//! Details are short; terminal output is never logged.

use crate::control_api::Caller;
use session_core::api::CallerInfo;

/// Longest detail kept (characters), e.g. the start of submitted text.
pub(crate) const DETAIL_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Permitted and carried out.
    Allowed,
    /// Refused by scope or permission policy.
    Denied,
    /// Permitted but failed (not found, invalid argument, host error, …).
    Error,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allowed => "allowed",
            Decision::Denied => "denied",
            Decision::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditEntry {
    pub ts_ms: i64,
    pub scope_root: Option<String>,
    /// `user:<client>` or `session:<id>`.
    pub caller: String,
    /// Wire-level method name, e.g. `session.rename`.
    pub method: &'static str,
    pub target_session_id: Option<String>,
    pub decision: Decision,
    pub detail: Option<String>,
}

pub(crate) fn caller_label(caller: &Caller) -> String {
    match &caller.info {
        CallerInfo::User => format!("user:{}", caller.client),
        CallerInfo::Session { id, .. } => format!("session:{id}"),
    }
}

/// The first [`DETAIL_CHARS`] characters of `text`, with an ellipsis when cut.
pub(crate) fn clip(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(DETAIL_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::ids::SessionId;
    use session_core::model::ScopeRoot;

    #[test]
    fn clip_keeps_short_text_and_cuts_long_text() {
        assert_eq!(clip("hello"), "hello");
        let long = "é".repeat(500);
        let clipped = clip(&long);
        assert_eq!(clipped.chars().count(), DETAIL_CHARS + 1);
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn caller_labels_name_the_client_or_session() {
        let user = Caller {
            max_blocking_ms: None,
            info: CallerInfo::User,
            client: "tui".into(),
        };
        assert_eq!(caller_label(&user), "user:tui");
        let session = Caller {
            max_blocking_ms: None,
            info: CallerInfo::Session {
                id: SessionId::from_string("S1"),
                scope: ScopeRoot::from_canonical("/r".into()),
            },
            client: "cli".into(),
        };
        assert_eq!(caller_label(&session), "session:S1");
    }
}
