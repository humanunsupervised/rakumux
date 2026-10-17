//! `rakumux remote on [--port N] | off | pair | status [--json] | token
//! [--rotate]`: remote access for the Rakumux app on a phone
//! (`plans/mobile-client.md`). The work is in `crate::remote_access`, which
//! the TUI shares.

use super::args::RemoteCommand;
use super::{layout, output_error, CliError, Globals};
use crate::qr::Qr;
use crate::remote_access::{self, Pairing, RunningApi};
use serde_json::json;
use session_core::paths::Layout;
use session_core::remote::RemoteConfig;
use std::io::Write;

pub(crate) fn run(
    globals: &Globals,
    command: RemoteCommand,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let layout = layout()?;
    // The QR code only on a terminal (not in a pipe or a file).
    let qr = atty::is(atty::Stream::Stdout);
    match command {
        RemoteCommand::On { port } => on(globals, &layout, port, qr, out),
        RemoteCommand::Off => off(globals, &layout, out),
        RemoteCommand::Pair => pair(globals, &layout, qr, out),
        RemoteCommand::Status { json } => status(&layout, json, out),
        RemoteCommand::Token { rotate } => token(globals, &layout, rotate, out),
    }
}

fn internal(message: String) -> CliError {
    CliError::internal(message)
}

// ---------------------------------------------------------------- commands

fn on(
    globals: &Globals,
    layout: &Layout,
    port: Option<u16>,
    qr: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let pairing =
        remote_access::enable(layout, globals.ptyd_path.as_deref(), port).map_err(internal)?;
    let text = if pairing.urls.is_empty() {
        no_address(layout, &pairing)
    } else {
        format!("Remote access is on.\n\n{}", pairing_text(&pairing, qr))
    };
    writeln!(out, "{text}").map_err(output_error)?;
    Ok(0)
}

fn pair(
    globals: &Globals,
    layout: &Layout,
    qr: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let text =
        match remote_access::pairing(layout, globals.ptyd_path.as_deref()).map_err(internal)? {
            None => "Remote access is off (`rakumux remote on` turns it on).".to_string(),
            Some(pairing) if pairing.urls.is_empty() => no_address(layout, &pairing),
            Some(pairing) => pairing_text(&pairing, qr),
        };
    writeln!(out, "{text}").map_err(output_error)?;
    Ok(0)
}

/// The URLs, the token, the link and (on a terminal) the QR code, then how
/// to use them.
fn pairing_text(pairing: &Pairing, qr: bool) -> String {
    let mut lines = Vec::new();
    for (i, url) in pairing.urls.iter().enumerate() {
        let label = if i == 0 { "URL  " } else { "     " };
        lines.push(format!("  {label}  {url}"));
    }
    lines.push(format!("  Token  {}", pairing.token));
    let link = pairing.link();
    if let Some(link) = &link {
        lines.push(format!("  Link   {link}"));
    }
    lines.push(String::new());
    match link.as_deref().filter(|_| qr).and_then(Qr::encode) {
        Some(code) => {
            lines.push(code.ansi().trim_end().to_string());
            lines.push(String::new());
            lines.push(
                "Scan the code with the Rakumux app (Scan QR code) or the phone's camera, \
                 open the link on the phone, or enter the URL and token in the app."
                    .into(),
            );
        }
        None => lines.push(
            "Open the link on your phone to fill in the Rakumux app, or enter the URL and \
             token in it. `rakumux remote pair` in a terminal shows a QR code to scan."
                .into(),
        ),
    }
    lines.push(
        "Turn it off with `rakumux remote off`; `rakumux remote token --rotate` makes a \
         new token."
            .into(),
    );
    lines.join("\n")
}

/// Remote access is on but the API bound nothing.
fn no_address(layout: &Layout, pairing: &Pairing) -> String {
    [
        "Remote access is on, but rakumux-api found no address to listen on: this machine \
         has no Tailscale address (or remote.json's `listen` addresses can't be bound). It \
         serves 127.0.0.1 only for now."
            .to_string(),
        format!(
            "Start Tailscale (or set `listen` in {}) and run `rakumux remote pair`; the API \
             log is {}.",
            layout.remote_file().display(),
            remote_access::api_log(layout).display()
        ),
        format!("  Token  {}", pairing.token),
    ]
    .join("\n")
}

fn off(globals: &Globals, layout: &Layout, out: &mut dyn Write) -> Result<i32, CliError> {
    let restarted =
        remote_access::disable(layout, globals.ptyd_path.as_deref()).map_err(internal)?;
    let message = if restarted {
        "Remote access is off; rakumux-api restarted without the remote listener."
    } else {
        "Remote access is off."
    };
    writeln!(out, "{message}").map_err(output_error)?;
    Ok(0)
}

fn status(layout: &Layout, as_json: bool, out: &mut dyn Write) -> Result<i32, CliError> {
    let config = remote_access::load(layout).map_err(internal)?;
    let api = remote_access::running_api(layout);
    let enabled = config.as_ref().is_some_and(|c| c.enabled);
    let text = if as_json {
        json!({
            "enabled": enabled,
            "port": config.as_ref().map_or(session_core::remote::DEFAULT_PORT, |c| c.port),
            "listen": config.as_ref().map(|c| c.listen.clone()).unwrap_or_default(),
            "token_prefix": config.as_ref().map(|c| c.token_prefix().to_owned()),
            "api": match &api {
                Some(api) => json!({
                    "running": true,
                    "pid": api.pid,
                    "url": api.url,
                    "remote_urls": api.remote_urls,
                }),
                None => json!({ "running": false, "remote_urls": [] }),
            },
        })
        .to_string()
    } else {
        describe(config.as_ref(), api.as_ref())
    };
    writeln!(out, "{text}").map_err(output_error)?;
    Ok(0)
}

fn describe(config: Option<&RemoteConfig>, api: Option<&RunningApi>) -> String {
    let Some(config) = config.filter(|c| c.enabled) else {
        return "Remote access is off (`rakumux remote on` turns it on).".into();
    };
    let mut lines = vec![format!("Remote access is on (port {}).", config.port)];
    match api {
        Some(api) if api.remote_urls.is_empty() => lines.push(format!(
            "  rakumux-api (pid {}) has no remote listener: no Tailscale address, or it \
             started before remote access was turned on (`rakumux remote pair` restarts it).",
            api.pid
        )),
        Some(api) => {
            for (i, url) in api.remote_urls.iter().enumerate() {
                let label = if i == 0 { "URL  " } else { "     " };
                lines.push(format!("  {label}  {url}"));
            }
        }
        None => lines.push(
            "  rakumux-api is not running; Rakumux starts it (or run `rakumux remote pair`)."
                .into(),
        ),
    }
    lines.push(format!(
        "  Token  {}… (`rakumux remote pair` shows it and a QR code)",
        config.token_prefix()
    ));
    lines.join("\n")
}

fn token(
    globals: &Globals,
    layout: &Layout,
    rotate: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let token = if rotate {
        let config =
            remote_access::rotate(layout, globals.ptyd_path.as_deref()).map_err(internal)?;
        eprintln!("A new token: apps paired with the old one need this one.");
        config.token
    } else {
        let (config, created) = remote_access::load_or_new(layout).map_err(internal)?;
        if created {
            // Keep the token printed now for `rakumux remote on`.
            remote_access::save(&config, layout).map_err(internal)?;
        }
        config.token
    };
    writeln!(out, "{token}").map_err(output_error)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_describes_the_settings() {
        let mut config = RemoteConfig::new().unwrap();
        assert!(describe(Some(&config), None).starts_with("Remote access is off"));
        assert!(describe(None, None).starts_with("Remote access is off"));
        config.enabled = true;
        let api = RunningApi {
            url: "http://127.0.0.1:1".into(),
            pid: 42,
            remote_urls: vec!["http://100.64.0.1:7337".into()],
        };
        let text = describe(Some(&config), Some(&api));
        assert!(
            text.starts_with("Remote access is on (port 7337)."),
            "{text}"
        );
        assert!(text.contains("http://100.64.0.1:7337"), "{text}");
        assert!(text.contains(config.token_prefix()), "{text}");
        assert!(!text.contains(&config.token), "the full token is not shown");
        let idle = describe(Some(&config), None);
        assert!(idle.contains("not running"), "{idle}");
    }

    #[test]
    fn pairing_text_has_a_qr_code_only_on_a_terminal() {
        let pairing = Pairing {
            urls: vec![
                "http://box.tail1234.ts.net:7337".into(),
                "http://100.64.0.1:7337".into(),
            ],
            token: "abcdefghijklmnopqrstuvwx27".into(),
        };
        let plain = pairing_text(&pairing, false);
        assert!(
            plain.contains("  URL    http://box.tail1234.ts.net:7337"),
            "{plain}"
        );
        assert!(plain.contains("         http://100.64.0.1:7337"), "{plain}");
        assert!(
            plain.contains("  Token  abcdefghijklmnopqrstuvwx27"),
            "{plain}"
        );
        assert!(
            plain.contains("rakumux://connect?url=http%3A%2F%2Fbox"),
            "{plain}"
        );
        assert!(!plain.contains('▀'), "{plain}");
        let terminal = pairing_text(&pairing, true);
        assert!(terminal.contains('▀'));
        assert!(terminal.contains("Scan the code"), "{terminal}");
    }

    #[test]
    fn the_printed_token_is_the_saved_one() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        let globals = Globals {
            ptyd_path: None,
            debug: false,
        };
        let mut out = Vec::new();
        assert_eq!(token(&globals, &layout, false, &mut out).unwrap(), 0);
        let printed = String::from_utf8(out).unwrap();
        let saved = RemoteConfig::load(&layout).unwrap().unwrap();
        assert_eq!(printed.trim(), saved.token);
        assert!(
            !saved.enabled,
            "printing the token doesn't turn remote access on"
        );
        let mut out = Vec::new();
        token(&globals, &layout, false, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), printed, "stable");
    }

    #[test]
    fn pair_says_when_remote_access_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        let globals = Globals {
            ptyd_path: None,
            debug: false,
        };
        let mut out = Vec::new();
        assert_eq!(pair(&globals, &layout, true, &mut out).unwrap(), 0);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("Remote access is off"), "{text}");
        assert!(
            !layout.remote_file().exists(),
            "pair doesn't set anything up"
        );
    }
}
