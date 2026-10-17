//! AI naming: a session whose name Orchestra generated (`auto_named`, from
//! whichever client created it) gets an AI-suggested name the first time it
//! settles (`Idle`/`Done`), from its command history and visible screen
//! (`crate::naming`).

use std::path::Path;

use session_core::api::{ReadRequest, ReadSource, TextFormat};
use session_core::client::SessionApi;
use session_core::ids::SessionId;
use session_core::model::Lifecycle;

use super::runtime::AppEvent;
use super::{App, Config};
use crate::naming::NamingInput;

/// Suggests a name; runs on a blocking thread. Tests substitute a fake so
/// they never reach a provider.
pub type Namer = fn(&Config, &NamingInput) -> Result<Option<String>, String>;

/// The real namer: `crate::naming::suggest_session_name` driven to
/// completion on the current (blocking) thread. Its future isn't `Send`, so
/// it can't be spawned as a task.
pub fn ai_namer(config: &Config, input: &NamingInput) -> Result<Option<String>, String> {
    let handle =
        tokio::runtime::Handle::try_current().map_err(|_| "no async runtime".to_string())?;
    handle
        .block_on(crate::naming::suggest_session_name(config, input))
        .map_err(|e| e.to_string())
}

/// What naming looks at: the history file (missing = none) and the plain
/// text of the visible screen (unreadable = empty).
pub fn gather_input(
    api: &dyn SessionApi,
    history_file: Option<&Path>,
    id: &SessionId,
) -> NamingInput {
    let history = history_file
        .and_then(|path| crate::naming::load_history(path).ok())
        .unwrap_or_default();
    let screen = api
        .read(ReadRequest {
            id: id.clone(),
            source: ReadSource::Visible,
            lines: None,
            format: TextFormat::Text,
        })
        .map(|read| read.text)
        .unwrap_or_default();
    NamingInput { history, screen }
}

impl App {
    /// Start naming `id` if it is an auto-named session that just settled
    /// and naming hasn't been tried yet.
    pub(super) fn maybe_auto_name(&mut self, id: &SessionId) {
        if self.naming_started.contains(id) {
            return;
        }
        let Some(session) = self.list.get(id) else {
            return;
        };
        if !session.auto_named || !matches!(session.lifecycle, Lifecycle::Idle | Lifecycle::Done) {
            return;
        }
        let Some(api) = self.connection.api() else {
            return;
        };
        self.naming_started.insert(id.clone());
        let expected_name = session.name.clone();
        let config = self.settings.ai.clone();
        let history_file = self.paths.as_ref().map(|paths| paths.history_file(id));
        let namer = self.namer;
        let id = id.clone();
        self.exec.job(move || {
            let input = gather_input(api.as_ref(), history_file.as_deref(), &id);
            AppEvent::NameSuggested {
                result: namer(&config, &input),
                id,
                expected_name,
            }
        });
    }

    pub(super) fn on_name_suggested(
        &mut self,
        id: SessionId,
        expected_name: String,
        result: Result<Option<String>, String>,
    ) {
        match result {
            Ok(Some(name)) => {
                // Names the user chose (or changed meanwhile) are never replaced.
                let unchanged = self
                    .list
                    .get(&id)
                    .is_some_and(|s| s.auto_named && s.name == expected_name && s.name != name);
                if unchanged {
                    self.rename_automatically(id, name);
                }
            }
            Ok(None) => {}
            Err(error) => self.set_status(format!("AI naming failed: {error}")),
        }
    }
}
