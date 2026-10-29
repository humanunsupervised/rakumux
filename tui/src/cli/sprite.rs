//! `rakumux sprite connect | status | disconnect`: your own Fly.io Sprite
//! for sessions (`crate::sprites`).

use super::args::SpriteCommand;
use super::{output_error, version, CliError};
use crate::account::{AccountAccess, LocalAccount};
use crate::sprites;
use session_core::client::SessionApi;
use session_core::model::MachineView;
use session_core::paths::Layout;
use std::io::Write;

pub(crate) fn run(
    api: &dyn SessionApi,
    layout: &Layout,
    command: SpriteCommand,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    match command {
        SpriteCommand::Connect {
            token,
            sprite,
            replies,
            no_replies,
        } => {
            let token = match token.as_deref() {
                Some("-") => {
                    let mut line = String::new();
                    std::io::stdin()
                        .read_line(&mut line)
                        .map_err(|e| CliError::invalid(format!("reading the token: {e}")))?;
                    line.trim().to_string()
                }
                Some(token) => token.to_string(),
                None => {
                    match sprites::found_token(&layout.home, sprites::cli_dir().as_deref()) {
                        Some((token, source)) => {
                            writeln!(out, "Using the Sprites token from {source}.")
                                .map_err(output_error)?;
                            token
                        }
                        None => return Err(CliError::invalid(
                            "no Sprites token here: make one at https://sprites.dev/account and \
                             pass it with --token (sign up there with your Fly.io account first)",
                        )),
                    }
                }
            };
            let account = local_account(layout)?;
            let signed_in = matches!(account.account(), Ok(Some(_)));
            let replies = if replies || no_replies || !signed_in {
                !no_replies
            } else {
                ask_replies(out)?
            };
            let feed = signed_in.then_some(sprites::Feed {
                account: &account,
                replies,
            });
            let connected = connect(api, layout, &token, sprite.as_deref(), feed, &mut |label| {
                let _ = writeln!(out, "{label}…");
            })?;
            let machine = &connected.machine;
            writeln!(
                out,
                "Ready: `rakumux new --sprite` runs a session on {} ({}).",
                machine.name, machine.url
            )
            .map_err(output_error)?;
            let feed = match &connected.feed {
                sprites::FeedSetUp::On if replies => "Its agents' replies reach your feed, and \
                    replies from rakumux.com are typed into them."
                    .to_owned(),
                sprites::FeedSetUp::On => "Its agents' replies reach your feed; replies from \
                    rakumux.com aren't typed into them."
                    .to_owned(),
                sprites::FeedSetUp::SignedOut | sprites::FeedSetUp::Skipped => "Sign in \
                    (`rakumux account login`) and connect again for its replies in your feed."
                    .to_owned(),
                sprites::FeedSetUp::Failed(error) => {
                    format!("Its feed couldn't be connected ({error}); connect again to retry.")
                }
            };
            writeln!(out, "{feed}").map_err(output_error)?;
            if !connected.tools.is_empty() {
                writeln!(out, "Installed there: {}.", connected.tools.join(", "))
                    .map_err(output_error)?;
            }
            writeln!(
                out,
                "A folder's first session there starts in its home with the clone typed: \
                 Enter signs in to GitHub and clones it."
            )
            .map_err(output_error)?;
            Ok(0)
        }
        SpriteCommand::Status => {
            let saved = sprites::load(&layout.home);
            match connected(api, layout)? {
                Some(machine) => {
                    let state = serde_json::to_value(machine.state)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    writeln!(out, "{}  {}  {state}", machine.name, machine.url)
                        .map_err(output_error)?;
                    if let Some(error) = machine.error {
                        writeln!(out, "  {error}").map_err(output_error)?;
                    }
                }
                None if saved.token.is_some() => {
                    writeln!(
                        out,
                        "A Sprites token is saved; `rakumux sprite connect` sets up the Sprite."
                    )
                    .map_err(output_error)?;
                }
                None => {
                    writeln!(
                        out,
                        "No Sprite; `rakumux sprite connect --token …` sets one up."
                    )
                    .map_err(output_error)?;
                }
            }
            Ok(0)
        }
        SpriteCommand::Disconnect => {
            let account = local_account(layout)?;
            sprites::disconnect(api, &layout.home, Some(&account)).map_err(CliError::invalid)?;
            writeln!(
                out,
                "Disconnected; the Sprite and its files stay on Fly.io."
            )
            .map_err(output_error)?;
            Ok(0)
        }
    }
}

/// [`sprites::connect`], for the CLI.
pub(crate) fn connect(
    api: &dyn SessionApi,
    layout: &Layout,
    token: &str,
    sprite: Option<&str>,
    feed: Option<sprites::Feed<'_>>,
    progress: &mut dyn FnMut(&str),
) -> Result<sprites::Connected, CliError> {
    sprites::connect(api, &layout.home, token, sprite, feed, &mut |step| {
        progress(step.label())
    })
    .map_err(CliError::invalid)
}

/// The Rakumux account here, which enrols the Sprite for the feed.
fn local_account(layout: &Layout) -> Result<LocalAccount, CliError> {
    let program = std::env::current_exe()
        .map_err(|error| CliError::internal(format!("can't find this program: {error}")))?;
    Ok(LocalAccount::new(layout.clone(), program, "cli", version()))
}

/// Ask once whether replies from rakumux.com are typed into the Sprite's
/// agents: yes unless the answer starts with `n`; yes without a terminal.
fn ask_replies(out: &mut dyn Write) -> Result<bool, CliError> {
    if !atty::is(atty::Stream::Stdin) {
        return Ok(true);
    }
    write!(
        out,
        "Type replies you write on rakumux.com into the Sprite's agents? [Y/n] "
    )
    .map_err(output_error)?;
    out.flush().map_err(output_error)?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| CliError::invalid(format!("reading the answer: {e}")))?;
    Ok(!answer.trim().to_ascii_lowercase().starts_with('n'))
}

/// [`sprites::connected`], for the CLI.
pub(crate) fn connected(
    api: &dyn SessionApi,
    layout: &Layout,
) -> Result<Option<MachineView>, CliError> {
    Ok(sprites::connected(api, &layout.home)?)
}
