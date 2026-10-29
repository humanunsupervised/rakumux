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
    pub(super) fn open_new_sprite_session(&mut self, folder: ScopeRoot, api: Arc<dyn SessionApi>) {
        let prefill = self.clone_command(&folder);
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
                Ok(Some(machine)) => AppEvent::Created {
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
                },
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
            })
            .map(|connected| match connected.feed {
                sprites::FeedSetUp::Failed(error) => Some(error),
                _ => None,
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

    pub(super) fn on_sprite_set_up(&mut self, result: Result<Option<String>, String>) {
        if !matches!(self.mode, AppMode::Sprite(_)) {
            return;
        }
        match result {
            Ok(feed_failed) => {
                self.mode = AppMode::Normal;
                self.set_status(match feed_failed {
                    None => "Your Sprite is ready.".to_owned(),
                    Some(error) => {
                        format!("Your Sprite is ready; its feed couldn't be connected: {error}")
                    }
                });
                self.open_new_session();
            }
            Err(message) => {
                self.mode = AppMode::Sprite(SpriteDialog {
                    state: SpriteState::Failed { message },
                })
            }
        }
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

    /// `git clone <origin> .` for the folder's repository, typed into the
    /// Sprite session's new shell for the user to run (Rakumux never runs
    /// git).
    fn clone_command(&self, folder: &ScopeRoot) -> Option<String> {
        let origin = self
            .folders
            .known
            .iter()
            .find(|project| project.path == *folder)
            .and_then(|project| project.git.as_ref())
            .and_then(|git| git.origin.clone())?;
        let safe = origin
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@:/._-~+%".contains(c));
        let origin = if safe {
            origin
        } else {
            format!("'{}'", origin.replace('\'', r"'\''"))
        };
        Some(format!("git clone {origin} ."))
    }
}

fn folder_name(folder: &ScopeRoot) -> String {
    folder
        .path()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.path().display().to_string())
}
