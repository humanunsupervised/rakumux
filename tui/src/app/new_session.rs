//! New session dialog (`sessions-refactor.md` §8, `folders.md`): just an
//! optional name. The session goes in the current folder
//! (`App::current_folder`), starts there and runs the login shell. Other folders and commands are for the command line
//! (`orchestra new --cwd DIR --cmd C -- ARGV`).

use session_core::api::CreateSession;
use session_core::model::ScopeRoot;

use super::text_input::TextInput;

#[derive(Debug, Clone)]
pub struct NewSessionDialog {
    /// The folder the session goes in; fixed when the dialog opens.
    pub folder: ScopeRoot,
    pub name: TextInput,
    pub error: Option<String>,
    /// The create request is in flight.
    pub busy: bool,
}

impl NewSessionDialog {
    /// A dialog creating in `folder`.
    pub fn new(folder: ScopeRoot) -> Self {
        Self {
            folder,
            name: TextInput::default(),
            error: None,
            busy: false,
        }
    }

    /// The create request: the folder, the login shell, no
    /// startup input, and the name unless it's blank (then the daemon
    /// picks one).
    pub fn build_request(
        &self,
        env: Vec<(String, String)>,
        (cols, rows): (u16, u16),
    ) -> CreateSession {
        let name = self.name.value().trim();
        CreateSession {
            scope: Some(self.folder.clone()),
            name: (!name.is_empty()).then(|| name.to_string()),
            cwd: Some(self.folder.path().to_path_buf()),
            argv: None,
            startup_cmds: Vec::new(),
            env,
            cols,
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_name_means_a_generated_one_in_the_folder() {
        let folder = ScopeRoot::from_canonical("/code/web".into());
        let dialog = NewSessionDialog::new(folder.clone());
        let request = dialog.build_request(vec![("A".into(), "1".into())], (100, 30));
        assert_eq!(request.scope.as_ref(), Some(&folder));
        assert_eq!(request.name, None);
        assert_eq!(request.cwd.as_deref(), Some(folder.path()));
        assert_eq!(request.argv, None);
        assert!(request.startup_cmds.is_empty());
        assert_eq!(request.env, vec![("A".to_string(), "1".to_string())]);
        assert_eq!((request.cols, request.rows), (100, 30));

        // Whitespace only is blank too.
        let dialog = NewSessionDialog {
            name: TextInput::new("   "),
            ..NewSessionDialog::new(folder)
        };
        assert_eq!(dialog.build_request(vec![], (80, 24)).name, None);
    }

    #[test]
    fn the_name_is_trimmed_and_nothing_else_is_asked() {
        let folder = ScopeRoot::from_canonical("/repo".into());
        let dialog = NewSessionDialog {
            name: TextInput::new("  api work "),
            ..NewSessionDialog::new(folder.clone())
        };
        let request = dialog.build_request(vec![], (80, 24));
        assert_eq!(request.name.as_deref(), Some("api work"));
        assert_eq!(request.cwd.as_deref(), Some(folder.path()));
        assert_eq!(request.argv, None);
        assert!(request.startup_cmds.is_empty());
    }
}
