//! `rakumux account login | logout | status [--json]`: the Rakumux account
//! that turns on AI session titles and the agent feed, and the hidden
//! `rakumux status-service` that holds it (see `crate::account`).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rakumux_sensagent::owner::protocol::{LineState, SignInView};
use serde_json::json;
use session_core::client::SessionApi;
use session_core::model::{SessionStatus, SessionView};

use super::args::AccountCommand;
use super::{layout, output_error, version, CliError};
use crate::account::{self, Account, AccountAccess, LocalAccount};
use crate::qr::Qr;

/// How often `login` asks whether the code was approved.
const POLL: Duration = Duration::from_secs(1);

pub(crate) fn run(
    api: &dyn SessionApi,
    command: AccountCommand,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let program = std::env::current_exe()
        .map_err(|error| CliError::internal(format!("can't find this program: {error}")))?;
    let access = LocalAccount::new(layout()?, program, "cli", version());
    match command {
        AccountCommand::Login => login(&access, atty::is(atty::Stream::Stdout), out),
        AccountCommand::Logout => logout(&access, out),
        AccountCommand::Status { json } => status(api, &access, json, out),
    }
}

/// `rakumux status-service`.
pub(crate) fn serve() -> Result<i32, CliError> {
    account::serve(layout()?, version())
        .map(|()| 0)
        .map_err(|error| CliError::internal(error.to_string()))
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn login(access: &dyn AccountAccess, qr: bool, out: &mut dyn Write) -> Result<i32, CliError> {
    if let Some(account) = access.account().map_err(CliError::internal)? {
        writeln!(out, "Signed in as {}.", account.name()).map_err(output_error)?;
        return Ok(0);
    }
    // Ctrl-C abandons the sign-in rather than leaving it waiting.
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    let mut view = access.sign_in().map_err(CliError::internal)?;
    let mut shown = false;
    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            let _ = access.cancel_sign_in();
            writeln!(out, "\nSign-in cancelled.").map_err(output_error)?;
            return Ok(130);
        }
        match &view {
            SignInView::Waiting { .. } if !shown => {
                writeln!(out, "{}", prompt_text(&view, qr)).map_err(output_error)?;
                out.flush().map_err(output_error)?;
                shown = true;
            }
            SignInView::SignedIn => {
                let account = access.account().map_err(CliError::internal)?;
                let name = account.as_ref().map_or("your account", Account::name);
                writeln!(out, "Signed in as {name}.").map_err(output_error)?;
                return Ok(0);
            }
            SignInView::Failed { reason } => {
                writeln!(out, "{}", account::failure_text(*reason)).map_err(output_error)?;
                return Ok(1);
            }
            SignInView::Idle => {
                writeln!(out, "The sign-in stopped.").map_err(output_error)?;
                return Ok(1);
            }
            _ => {}
        }
        std::thread::sleep(POLL);
        view = access.sign_in_status().map_err(CliError::internal)?;
    }
}

/// The code, the link to open and (on a terminal) its QR code.
fn prompt_text(view: &SignInView, qr: bool) -> String {
    let SignInView::Waiting { user_code, .. } = view else {
        return String::new();
    };
    let link = account::approval_link(view).unwrap_or_default();
    let mut lines = vec![
        "Open this link in any browser, sign in, and approve the code:".to_string(),
        String::new(),
        format!("  Link  {link}"),
        format!("  Code  {user_code}"),
        String::new(),
    ];
    if let Some(code) = qr.then_some(link).and_then(Qr::encode) {
        lines.push(code.ansi().trim_end().to_string());
        lines.push(String::new());
        lines.push("Or scan the code with your phone.".into());
    }
    lines.push("Waiting for approval… (Ctrl-C cancels)".into());
    lines.join("\n")
}

fn logout(access: &dyn AccountAccess, out: &mut dyn Write) -> Result<i32, CliError> {
    match access.account().map_err(CliError::internal)? {
        None => writeln!(out, "Not signed in.").map_err(output_error)?,
        Some(account) => {
            access.sign_out().map_err(CliError::internal)?;
            writeln!(
                out,
                "Signed out {}. AI session titles stopped.",
                account.name()
            )
            .map_err(output_error)?;
        }
    }
    Ok(0)
}

fn status(
    api: &dyn SessionApi,
    access: &dyn AccountAccess,
    as_json: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let account = access.account().map_err(CliError::internal)?;
    let described = match &account {
        Some(_) => live_descriptions(api, access)?,
        None => Vec::new(),
    };
    let text = if as_json {
        let sessions: Vec<_> = described
            .iter()
            .map(|(session, text)| {
                json!({"id": session.id, "name": session.name, "description": text})
            })
            .collect();
        json!({
            "signed_in": account.is_some(),
            "email": account.as_ref().and_then(|a| a.email.clone()),
            "ai_session_titles": account.as_ref().and_then(|a| a.titles),
            // The switch's name before titles, kept for scripts.
            "ai_session_summaries": account.as_ref().and_then(|a| a.titles),
            "feed": account.as_ref().and_then(|a| a.feed),
            "sessions": sessions,
        })
        .to_string()
    } else {
        let mut text = describe(account.as_ref());
        for (session, description) in &described {
            text.push_str(&format!("\n  {}  {description}", session.name));
        }
        text
    };
    writeln!(out, "{text}").map_err(output_error)?;
    Ok(0)
}

/// The live sessions of every folder that have an AI description.
fn live_descriptions(
    api: &dyn SessionApi,
    access: &dyn AccountAccess,
) -> Result<Vec<(SessionView, String)>, CliError> {
    let live: Vec<SessionView> = api
        .list_all(false)?
        .into_iter()
        .filter(|session| session.status == SessionStatus::Live)
        .take(account::MAX_SESSIONS)
        .collect();
    let ids: Vec<_> = live.iter().map(|session| session.id.clone()).collect();
    let lines = access.statuses(&ids).map_err(CliError::internal)?;
    Ok(live
        .into_iter()
        .zip(lines)
        .filter_map(|(session, line)| match (line.state, line.text) {
            (LineState::Ready, Some(text)) => Some((session, text)),
            _ => None,
        })
        .collect())
}

fn describe(account: Option<&Account>) -> String {
    match account {
        Some(account) => format!("Signed in as {}.", account.name()),
        None => "Not signed in. `rakumux account login` signs in for AI session titles and \
                 the agent feed."
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_describes_the_account() {
        assert!(describe(None).starts_with("Not signed in."));
        let account = Account {
            email: Some("person@example.com".into()),
            subject: "user_1".into(),
            titles: Some(false),
            feed: Some(true),
        };
        assert_eq!(describe(Some(&account)), "Signed in as person@example.com.");
    }

    #[test]
    fn the_prompt_has_a_qr_code_only_on_a_terminal() {
        let view = SignInView::Waiting {
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://accounts.example/device".into(),
            verification_uri_complete: None,
            connect_url: Some("https://site.example/connect?code=WDJB-MJHT".into()),
            expires_in_seconds: 600,
        };
        let plain = prompt_text(&view, false);
        assert!(plain.contains("  Link  https://site.example/connect?code=WDJB-MJHT"));
        assert!(plain.contains("  Code  WDJB-MJHT"));
        assert!(!plain.contains('▀'));
        assert!(prompt_text(&view, true).contains("Or scan the code"));
    }
}
