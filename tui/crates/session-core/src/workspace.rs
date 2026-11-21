//! Workspaces: named groups of a machine's sessions (capability
//! [`CAPABILITY_WORKSPACES`](crate::api::CAPABILITY_WORKSPACES)). A session
//! is in one workspace or in none; clients show one workspace at a time, or
//! the sessions in none. Workspaces belong to the daemon, so every window,
//! the CLI and the account service see the same ones.

use crate::ids::WorkspaceId;
use serde::{Deserialize, Serialize};

/// Workspaces a machine may have.
pub const MAX_WORKSPACES: usize = 100;
/// The longest workspace name, in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// A workspace, as every client sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Unique among the machine's workspaces: one line, trimmed, 1 to
    /// [`MAX_NAME_CHARS`] characters.
    pub name: String,
    /// Unix seconds.
    pub created_at: i64,
}

/// `name` as a workspace takes it: trimmed, one line, 1 to
/// [`MAX_NAME_CHARS`] characters, no control characters; `None` otherwise.
pub fn checked_name(name: &str) -> Option<String> {
    let name = name.trim();
    let count = name.chars().count();
    ((1..=MAX_NAME_CHARS).contains(&count) && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_trimmed_line_of_at_most_64_characters() {
        assert_eq!(checked_name("  Launch  ").as_deref(), Some("Launch"));
        assert_eq!(checked_name(""), None);
        assert_eq!(checked_name("   "), None);
        assert_eq!(checked_name("a\nb"), None);
        assert_eq!(checked_name("a\u{1b}[31m"), None);
        assert_eq!(checked_name(&"x".repeat(65)), None);
        assert!(checked_name(&"é".repeat(64)).is_some());
    }

    #[test]
    fn ids_read_back_with_or_without_their_prefix() {
        let id = WorkspaceId::generate();
        assert_eq!(WorkspaceId::from_string(format!("ws_{id}")), id);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<WorkspaceId>(&json).unwrap(), id);
    }
}
