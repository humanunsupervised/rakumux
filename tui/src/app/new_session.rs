//! Immediate session creation in the current folder. The daemon chooses the
//! name and starts the login shell; custom names/commands remain CLI options.

use std::path::PathBuf;

use session_core::api::CreateSession;
use session_core::model::ScopeRoot;

/// Build a generated-name session without prompting or adding startup input,
/// starting in `cwd` (a worktree inside the folder) or the folder itself.
pub fn request(
    folder: ScopeRoot,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    (cols, rows): (u16, u16),
) -> CreateSession {
    CreateSession {
        cwd: Some(cwd.unwrap_or_else(|| folder.path().to_path_buf())),
        scope: Some(folder),
        name: None,
        argv: None,
        startup_cmds: Vec::new(),
        env,
        cols,
        rows,
        machine: None,
        prefill: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sessions_use_a_generated_name_and_the_current_folders_shell() {
        let folder = ScopeRoot::from_canonical("/code/web".into());
        let request = request(
            folder.clone(),
            None,
            vec![("A".into(), "1".into())],
            (100, 30),
        );
        assert_eq!(request.scope.as_ref(), Some(&folder));
        assert_eq!(request.name, None);
        assert_eq!(request.cwd.as_deref(), Some(folder.path()));
        assert_eq!(request.argv, None);
        assert!(request.startup_cmds.is_empty());
        assert_eq!(request.env, vec![("A".to_string(), "1".to_string())]);
        assert_eq!((request.cols, request.rows), (100, 30));
        // From a worktree's header: there, in the same folder.
        let worktree = PathBuf::from("/code/web/worktrees/feature");
        let there = super::request(folder.clone(), Some(worktree.clone()), vec![], (100, 30));
        assert_eq!(there.scope.as_ref(), Some(&folder));
        assert_eq!(there.cwd, Some(worktree));
    }
}
