//! `rakumux devices pair | ls [--json] | rm DEVICE | log [--json]`: the devices that sign
//! what rakumux.com asks of this machine (a browser's passkey for the signer
//! site, or the phone app's key). Pairing happens here, at this machine: it
//! shows a one-time code, a link and its QR code, and pairs the device sent
//! with that code only once you say yes to its name and fingerprint. `log`
//! shows what became of each signed command: which device signed it, or why
//! it was refused.

use std::io::{BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rakumux_sensagent::feed::{pairing_failure_text, DeviceView, PairingAction, PairingView};
use rakumux_sensagent::owner::OwnerLayout;
use rakumux_sensagent::signing::log::{self as signed_log, LogEntry, Outcome};
use rakumux_sensagent::signing::pairing::PairingState;
use rakumux_sensagent::signing::KeyFormat;
use serde_json::json;
use session_core::client::SessionApi;
use session_core::ids::SessionId;

use super::args::DevicesCommand;
use super::{layout, output_error, version, CliError};
use crate::account::{AccountAccess, LocalAccount};
use crate::printable::printable;
use crate::qr::Qr;

/// How often `pair` asks where the pairing is.
const POLL: Duration = Duration::from_secs(1);

pub(crate) fn run(
    api: &dyn SessionApi,
    command: DevicesCommand,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let program = std::env::current_exe()
        .map_err(|error| CliError::internal(format!("can't find this program: {error}")))?;
    let access = LocalAccount::new(layout()?, program, "cli", version());
    match command {
        DevicesCommand::Pair => pair(&access, std::io::stdout().is_terminal(), out),
        DevicesCommand::Ls { json } => list(&access, json, out),
        DevicesCommand::Rm { device } => remove(&access, &device, out),
        DevicesCommand::Log { json } => {
            let path = OwnerLayout::new(&layout()?)
                .root()
                .join(signed_log::FILE_NAME);
            log(api, &signed_log::read(&path), json, out)
        }
    }
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn pair(access: &dyn AccountAccess, qr: bool, out: &mut dyn Write) -> Result<i32, CliError> {
    if access.account().map_err(CliError::internal)?.is_none() {
        writeln!(out, "Sign in first: rakumux account login").map_err(output_error)?;
        return Ok(1);
    }
    // Ctrl-C closes the pairing rather than leaving its code working.
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    let Some(view) = access
        .pairing(PairingAction::Start)
        .map_err(CliError::internal)?
    else {
        writeln!(out, "The account service isn't running.").map_err(output_error)?;
        return Ok(1);
    };
    let mut shown_code = String::new();
    let mut view = view;
    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            let _ = access.pairing(PairingAction::Cancel);
            writeln!(out, "\nPairing cancelled.").map_err(output_error)?;
            return Ok(130);
        }
        if view.code != shown_code && view.state == PairingState::Open {
            writeln!(out, "{}", prompt_text(&view, qr)).map_err(output_error)?;
            out.flush().map_err(output_error)?;
            shown_code = view.code.clone();
        }
        match &view.state {
            PairingState::Open => {}
            PairingState::Proposed { name, fingerprint } => {
                write!(
                    out,
                    "\nA device asks to pair: {}\n  Fingerprint  {}  (the device shows the same)\nPair it? [y/N] ",
                    printable(name),
                    printable(fingerprint)
                )
                .map_err(output_error)?;
                out.flush().map_err(output_error)?;
                let mut line = String::new();
                std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .map_err(output_error)?;
                let yes = matches!(line.trim(), "y" | "Y" | "yes" | "Yes");
                if let Some(next) = access
                    .pairing(PairingAction::Answer { yes })
                    .map_err(CliError::internal)?
                {
                    view = next;
                    continue;
                }
            }
            PairingState::Paired { name } => {
                writeln!(
                    out,
                    "Paired {}. It can now reply and act from rakumux.com: this machine carries out only what a paired device signed.",
                    printable(name)
                )
                .map_err(output_error)?;
                return Ok(0);
            }
            PairingState::Failed { failure } => {
                writeln!(out, "Not paired: {}.", pairing_failure_text(*failure))
                    .map_err(output_error)?;
                return Ok(1);
            }
        }
        std::thread::sleep(POLL);
        match access
            .pairing(PairingAction::Show)
            .map_err(CliError::internal)?
        {
            Some(next) => view = next,
            None => {
                writeln!(out, "The pairing stopped.").map_err(output_error)?;
                return Ok(1);
            }
        }
    }
}

/// The code, the link to open and (on a terminal) its QR code.
fn prompt_text(view: &PairingView, qr: bool) -> String {
    let mut lines = vec![
        "On the device to pair, open this link (a browser), or scan the QR code with the Rakumux app:"
            .to_string(),
        String::new(),
        format!("  Link  {}", printable(&view.link)),
        format!("  Code  {}", printable(&view.code)),
        String::new(),
    ];
    if let Some(code) = qr.then_some(view.link.as_str()).and_then(Qr::encode) {
        lines.push(code.ansi().trim_end().to_string());
        lines.push(String::new());
    }
    lines.push("The code works once, for 5 minutes. Waiting… (Ctrl-C cancels)".into());
    lines.join("\n")
}

fn kind(format: KeyFormat) -> &'static str {
    match format {
        KeyFormat::Webauthn => "passkey",
        KeyFormat::P256 => "key",
    }
}

fn list(access: &dyn AccountAccess, json: bool, out: &mut dyn Write) -> Result<i32, CliError> {
    let devices = access.devices(None).map_err(CliError::internal)?;
    if json {
        let devices: Vec<serde_json::Value> = devices
            .iter()
            .map(|device| {
                json!({
                    "key_id": device.key_id,
                    "kind": kind(device.format),
                    "name": device.name,
                    "fingerprint": device.fingerprint,
                    "paired_at": device.paired_at,
                    "last_used_at": device.last_used_at,
                })
            })
            .collect();
        writeln!(out, "{}", json!({ "devices": devices })).map_err(output_error)?;
        return Ok(0);
    }
    if devices.is_empty() {
        writeln!(
            out,
            "No paired devices: nothing asked on rakumux.com is carried out here. Pair one: rakumux devices pair"
        )
        .map_err(output_error)?;
        return Ok(0);
    }
    for device in &devices {
        writeln!(
            out,
            "{}  {}  {}",
            printable(&device.name),
            kind(device.format),
            printable(&device.fingerprint)
        )
        .map_err(output_error)?;
    }
    Ok(0)
}

fn remove(access: &dyn AccountAccess, device: &str, out: &mut dyn Write) -> Result<i32, CliError> {
    let devices = access.devices(None).map_err(CliError::internal)?;
    let named: Vec<&DeviceView> = devices
        .iter()
        .filter(|kept| kept.key_id == device || kept.name.eq_ignore_ascii_case(device))
        .collect();
    let [found] = named.as_slice() else {
        let why = if named.is_empty() {
            "No paired device has that name."
        } else {
            "Several paired devices have that name: give its key id (rakumux devices ls --json)."
        };
        writeln!(out, "{why}").map_err(output_error)?;
        return Ok(1);
    };
    let left = access
        .devices(Some(&found.key_id))
        .map_err(CliError::internal)?;
    writeln!(out, "Forgot {}.", printable(&found.name)).map_err(output_error)?;
    if left.is_empty() {
        writeln!(
            out,
            "No device is paired now: nothing asked on rakumux.com is carried out here until one is."
        )
        .map_err(output_error)?;
    }
    Ok(0)
}

/// How many of the newest entries `log` prints as text.
const LOG_SHOWN: usize = 50;

fn outcome_text(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Accepted => "accepted",
        Outcome::Unsigned => "refused: unsigned",
        Outcome::UnknownDevice => "refused: a device not paired here",
        Outcome::BadSignature => "refused: the signature doesn't hold",
        Outcome::Expired => "refused: expired",
    }
}

fn log(
    api: &dyn SessionApi,
    entries: &[LogEntry],
    json: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    if json {
        writeln!(out, "{}", json!({ "entries": entries })).map_err(output_error)?;
        return Ok(0);
    }
    if entries.is_empty() {
        writeln!(out, "No signed commands yet.").map_err(output_error)?;
        return Ok(0);
    }
    let mut names = std::collections::HashMap::new();
    for entry in &entries[entries.len().saturating_sub(LOG_SHOWN)..] {
        let when = chrono::DateTime::from_timestamp_millis(entry.at)
            .map(|at| {
                at.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        let session = entry.session.as_ref().map(|id| {
            names
                .entry(id.clone())
                .or_insert_with(|| {
                    api.get(&SessionId::from_string(id.clone()))
                        .map(|view| view.name)
                        .unwrap_or_else(|_| id.clone())
                })
                .clone()
        });
        let device = match (&entry.name, &entry.device) {
            (Some(name), _) => name.clone(),
            (None, Some(key)) => format!("key {}", key.chars().take(12).collect::<String>()),
            (None, None) => "no device".into(),
        };
        let mut line = format!(
            "{when}  {}  {}  {}",
            entry.kind,
            outcome_text(entry.outcome),
            printable(&device)
        );
        if let Some(session) = session {
            line.push_str(&format!("  {}", printable(&session)));
        }
        writeln!(out, "{line}").map_err(output_error)?;
    }
    Ok(0)
}
