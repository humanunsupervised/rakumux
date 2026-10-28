//! `rakumux sprite connect | status | disconnect`: your own Fly.io Sprite
//! for sessions (`crate::sprites`).

use super::args::SpriteCommand;
use super::{output_error, CliError};
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
        SpriteCommand::Connect { token, sprite } => {
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
            let machine = connect(api, layout, &token, sprite.as_deref(), &mut |label| {
                let _ = writeln!(out, "{label}…");
            })?;
            writeln!(
                out,
                "Ready: `rakumux new --sprite` runs a session on {} ({}).",
                machine.name, machine.url
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
            if let Some(machine) = connected(api, layout)? {
                api.remove_machine(&machine.id)?;
            }
            sprites::save(&layout.home, &sprites::Saved::default()).map_err(CliError::invalid)?;
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
    progress: &mut dyn FnMut(&str),
) -> Result<MachineView, CliError> {
    sprites::connect(api, &layout.home, token, sprite, &mut |step| {
        progress(step.label())
    })
    .map_err(CliError::invalid)
}

/// [`sprites::connected`], for the CLI.
pub(crate) fn connected(
    api: &dyn SessionApi,
    layout: &Layout,
) -> Result<Option<MachineView>, CliError> {
    Ok(sprites::connected(api, &layout.home)?)
}
