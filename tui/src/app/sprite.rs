//! New sessions on your own Fly Sprite: the `☁ Sprite` toggle on
//! `+ New session` (`N`), per folder, and the dialog that sets the Sprite up
//! the first time (`AppMode::Sprite`). The work happens off the UI thread,
//! through `crate::sprites` and the daemon.

use std::sync::Arc;

use session_core::api::CreateSession;
use session_core::client::SessionApi;
use session_core::model::ScopeRoot;
use std::path::PathBuf;

use super::runtime::AppEvent;
use super::text_input::TextInput;
use super::types::{AccountStatus, AppMode, SpriteDialog, SpriteState};
use super::App;
use crate::sprites::{self, Step};

/// Where Sprites tokens are made (after signing up with Fly.io).
pub const TOKEN_PAGE: &str = "https://sprites.dev/account";

impl App {
    /// Whether new sessions in `folder` run on the Sprite.
    pub fn runs_on_sprite(&self, folder: &ScopeRoot) -> bool {
        self.settings
            .sessions
            .sprite_folders
            .iter()
            .any(|path| path == folder.path())
    }

    /// `N`, or the `☁ Sprite` toggle: new sessions in the current folder run
    /// on the Sprite, or here again.
    pub fn toggle_sprite(&mut self) {
        let folder = self.current_folder();
        let folders = &mut self.settings.sessions.sprite_folders;
        let on = match folders.iter().position(|path| path == folder.path()) {
            Some(at) => {
                folders.remove(at);
                false
            }
            None => {
                folders.push(folder.path().to_path_buf());
                true
            }
        };
        if let Err(error) = self.save_settings() {
            self.set_status(format!("Couldn't save the setting: {error}"));
            return;
        }
        let name = folder_name(&folder);
        self.set_status(if on {
            format!("New sessions in {name} run on your Sprite.")
        } else {
            format!("New sessions in {name} run here.")
        });
    }

    /// Create a session on the Sprite for `folder`, or open the dialog that
    /// sets the Sprite up first.
    /// Signed off once, a folder with `.env` files has them copied (they
    /// go after the clone the session types), or is asked about first.
    pub(super) fn open_new_sprite_session(&mut self, folder: ScopeRoot, api: Arc<dyn SessionApi>) {
        let origin = self.folder_origin(&folder);
        let copy_env = self
            .settings
            .sessions
            .sprite_env_copy
            .get(folder.path())
            .copied();
        let events = self.exec.events();
        let (cols, rows) = self.terminal_size();
        let Some(home) = self.rakumux_home() else {
            self.set_status("No Rakumux home to keep the Sprite's settings in.");
            return;
        };
        let cli = self.sprite_cli_dir();
        self.creating_session = true;
        self.grid.clear();
        self.mode = AppMode::Normal;
        self.set_status("Creating a session on your Sprite...");
        self.exec
            .job(move || match sprites::connected(api.as_ref(), &home) {
                Ok(Some(machine)) => {
                    let copied = match (&origin, copy_env) {
                        (None, _) | (_, Some(false)) => Ok(false),
                        (Some(_), decided) => {
                            let files = sprites::env_files(folder.path());
                            if files.is_empty() {
                                Ok(false)
                            } else if decided.is_none() {
                                return AppEvent::SpriteEnvAsk { folder, files };
                            } else {
                                copy_env_files(&home, &folder, &files).map(|()| true)
                            }
                        }
                    };
                    let env = copied.as_ref().is_ok_and(|copied| *copied);
                    let prefill =
                        origin.map(|origin| sprites::clone_command(&origin, folder.path(), env));
                    let created = AppEvent::Created {
                        result: api.create(CreateSession {
                            scope: Some(folder),
                            name: None,
                            cwd: None,
                            argv: None,
                            startup_cmds: Vec::new(),
                            env: Vec::new(),
                            cols,
                            rows,
                            machine: Some(machine.id),
                            prefill,
                        }),
                    };
                    // Said after the session's "Created".
                    match copied {
                        Err(error) => {
                            let _ = events.send(created);
                            AppEvent::SpriteEnvFailed(error)
                        }
                        Ok(_) => created,
                    }
                }
                Ok(None) => AppEvent::SpriteNeeded {
                    found: sprites::found_token(&home, cli.as_deref()),
                },
                Err(error) => AppEvent::Created { result: Err(error) },
            });
    }

    pub(super) fn on_sprite_needed(&mut self, found: Option<(String, &'static str)>) {
        self.creating_session = false;
        self.set_status("");
        let state = match found {
            Some((token, source)) => SpriteState::Found { token, source },
            None => SpriteState::Paste {
                input: TextInput::default(),
                error: None,
            },
        };
        self.mode = AppMode::Sprite(SpriteDialog { state });
    }

    /// The Sprites token to set up with: signed in, ask first whether
    /// replies from rakumux.com are typed into the Sprite's agents (its
    /// agents' replies reach the feed); signed out, set up at once.
    pub fn sprite_token_chosen(&mut self, token: String) {
        if matches!(self.account, AccountStatus::SignedIn(_)) {
            self.mode = AppMode::Sprite(SpriteDialog {
                state: SpriteState::Replies { token },
            });
        } else {
            self.start_sprite_setup(token, false);
        }
    }

    /// Set the Sprite up with `token`, then create the session. Signed in,
    /// the account enrols it for the feed, typing `replies` from elsewhere
    /// into its agents or not.
    pub fn start_sprite_setup(&mut self, token: String, replies: bool) {
        let (Some(api), Some(home)) = (self.connection.api(), self.rakumux_home()) else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        self.mode = AppMode::Sprite(SpriteDialog {
            state: SpriteState::Working { step: Step::Create },
        });
        let signed_in = matches!(self.account, AccountStatus::SignedIn(_));
        let account = self.account_access.clone();
        let events = self.exec.events();
        self.exec.job(move || {
            let feed = signed_in.then_some(sprites::Feed {
                account: account.as_ref(),
                replies,
            });
            let result = sprites::connect(api.as_ref(), &home, &token, None, feed, &mut |step| {
                let _ = events.send(AppEvent::SpriteStep(step));
            });
            AppEvent::SpriteSetUp(result)
        });
    }

    pub(super) fn on_sprite_step(&mut self, step: Step) {
        if let AppMode::Sprite(SpriteDialog {
            state: SpriteState::Working { step: at },
        }) = &mut self.mode
        {
            *at = step;
        }
    }

    pub(super) fn on_sprite_set_up(&mut self, result: Result<sprites::Connected, String>) {
        if !matches!(self.mode, AppMode::Sprite(_)) {
            return;
        }
        match result {
            Ok(connected) => {
                self.mode = AppMode::Sprite(SpriteDialog {
                    state: SpriteState::Ready {
                        sprite: connected.machine.name,
                        tools: connected.tools,
                        feed: connected.feed,
                    },
                });
            }
            Err(message) => {
                self.mode = AppMode::Sprite(SpriteDialog {
                    state: SpriteState::Failed { message },
                })
            }
        }
    }

    /// Set up and shown what's there: Enter goes on to the session it was set
    /// up for.
    pub fn sprite_ready_done(&mut self) {
        self.mode = AppMode::Normal;
        self.open_new_session();
    }

    /// A folder with `.env` files gets its first session on the Sprite: ask
    /// whether they're copied.
    pub(super) fn on_sprite_env_ask(&mut self, folder: ScopeRoot, files: Vec<PathBuf>) {
        self.creating_session = false;
        self.set_status("");
        self.mode = AppMode::Sprite(SpriteDialog {
            state: SpriteState::Env { folder, files },
        });
    }

    /// Whether the asked folder's `.env` files are copied, saved for it;
    /// then its session.
    pub fn sprite_env_chosen(&mut self, copy: bool) {
        let AppMode::Sprite(SpriteDialog {
            state: SpriteState::Env { folder, .. },
        }) = &self.mode
        else {
            return;
        };
        let folder = folder.clone();
        self.mode = AppMode::Normal;
        self.settings
            .sessions
            .sprite_env_copy
            .insert(folder.path().to_path_buf(), copy);
        if let Err(error) = self.save_settings() {
            self.set_status(format!("Couldn't save the setting: {error}"));
            return;
        }
        let Some(api) = self.connection.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        self.open_new_sprite_session(folder, api);
    }

    pub(super) fn on_sprite_env_failed(&mut self, error: String) {
        self.set_status(format!("The .env files weren't copied: {error}"));
    }

    /// Back to the token field after a failure, or to paste another one.
    pub fn sprite_paste_token(&mut self) {
        self.mode = AppMode::Sprite(SpriteDialog {
            state: SpriteState::Paste {
                input: TextInput::default(),
                error: None,
            },
        });
    }

    /// The Rakumux home: where `config.json` is.
    fn rakumux_home(&self) -> Option<PathBuf> {
        self.config_path.as_ref()?.parent().map(PathBuf::from)
    }

    /// The `sprite` CLI's folder; never read in tests.
    fn sprite_cli_dir(&self) -> Option<PathBuf> {
        if cfg!(test) {
            None
        } else {
            sprites::cli_dir()
        }
    }

    /// Copy where tokens are made (OSC 52).
    pub fn copy_sprite_link(&mut self) {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(TOKEN_PAGE);
        self.terminal_output
            .extend_from_slice(format!("\x1b]52;c;{data}\x07").as_bytes());
        self.set_status("Copied the link.");
    }

    /// The folder's repository's `origin`, which the Sprite session's new
    /// shell has the clone of typed for the user to run (Rakumux never runs
    /// git).
    fn folder_origin(&self, folder: &ScopeRoot) -> Option<String> {
        self.folders
            .known
            .iter()
            .find(|project| project.path == *folder)
            .and_then(|project| project.git.as_ref())
            .and_then(|git| git.origin.clone())
    }
}

/// Put the folder's `.env` files on the Sprite for its first session's
/// command to unpack.
fn copy_env_files(
    home: &std::path::Path,
    folder: &ScopeRoot,
    files: &[PathBuf],
) -> Result<(), String> {
    let archive = sprites::env_archive(folder.path(), files)?;
    sprites::stage_env(home, &sprites::folder_name(folder.path()), archive)
}

fn folder_name(folder: &ScopeRoot) -> String {
    folder
        .path()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.path().display().to_string())
}
