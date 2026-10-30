//! The Program Status Protocol (OSC 7501,
//! <https://www.superlogical.com/rex/docs/build/program-status>): a program
//! tells its terminal whether it is idle, working, blocked on the user, done
//! or failed, for itself (the root record) and for its children (records with
//! an `id`, such as subagents). Pi and Kimi Code speak it, but only after the
//! terminal answers its query, `OSC 7501;? ST`, with the same body.
//!
//! A report is `OSC 7501;state=<state>[:app=<name>][:kind=<kind>][:id=<id>]
//! [:title=<base64>][:msg=<base64>] ST`. Only what says what the program is
//! doing is kept: `title` and `msg` (a session or dialog title) are dropped.

/// What a record says its program is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    Idle,
    Working,
    /// Waiting for the user (`kind`: `permission`, `question`, `auth`, …).
    Blocked,
    /// Finished what it was asked.
    Done,
    /// Failed.
    Error,
    /// The record is gone (the root's: the program is leaving).
    Clear,
}

/// One OSC 7501 report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    /// Why it's blocked, when it says.
    pub kind: Option<String>,
    /// A child record's id (a subagent); none for the program itself.
    pub id: Option<String>,
    /// The program's name for itself (`pi`, `kimi-code`).
    pub app: Option<String>,
}

/// The longest `id` kept; a report with a longer one is ignored.
const MAX_ID: usize = 128;
/// The longest `app` and `kind`; longer ones are left out.
const MAX_NAME: usize = 32;

/// The body after `7501;` is a feature query (`?`, perhaps with pairs a
/// later revision adds).
pub(crate) fn is_query(body: &[u8]) -> bool {
    body.first() == Some(&b'?')
}

/// The query's answer: the same body.
pub(crate) const ANSWER: &str = "\x1b]7501;?\x1b\\";

/// A report's body (`state=…:app=…`), or `None` when it isn't one this
/// protocol revision describes: no known `state`, or an `id` that isn't one.
pub(crate) fn parse(body: &[u8]) -> Option<ProgramStatus> {
    let body = std::str::from_utf8(body).ok()?;
    let mut state = None;
    let mut kind = None;
    let mut id = None;
    let mut app = None;
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "state" if state.is_none() => {
                state = Some(match value {
                    "idle" => ProgramState::Idle,
                    "working" => ProgramState::Working,
                    "blocked" => ProgramState::Blocked,
                    "done" => ProgramState::Done,
                    "error" => ProgramState::Error,
                    "clear" => ProgramState::Clear,
                    _ => return None,
                })
            }
            "id" if id.is_none() => {
                if !word(value, MAX_ID) {
                    return None;
                }
                id = Some(value.to_owned());
            }
            "app" if app.is_none() => app = word(value, MAX_NAME).then(|| value.to_owned()),
            "kind" if kind.is_none() => kind = word(value, MAX_NAME).then(|| value.to_owned()),
            _ => {}
        }
    }
    Some(ProgramStatus {
        state: state?,
        kind,
        id,
        app,
    })
}

/// A short printable token: letters, digits and `_ . + - /`.
fn word(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: ProgramState) -> ProgramStatus {
        ProgramStatus {
            state,
            kind: None,
            id: None,
            app: None,
        }
    }

    #[test]
    fn reports_keep_what_the_program_is_doing_and_drop_its_texts() {
        assert_eq!(
            parse(b"state=working:app=pi:msg=c2Vzc2lvbg=="),
            Some(ProgramStatus {
                app: Some("pi".into()),
                ..status(ProgramState::Working)
            })
        );
        assert_eq!(
            parse(b"state=blocked:app=kimi-code:kind=permission:msg=QXBwcm92YWw="),
            Some(ProgramStatus {
                kind: Some("permission".into()),
                app: Some("kimi-code".into()),
                ..status(ProgramState::Blocked)
            })
        );
        assert_eq!(
            parse(b"state=done:app=kimi-code:id=agent-0123abcd:title=R3JlZXQ="),
            Some(ProgramStatus {
                id: Some("agent-0123abcd".into()),
                app: Some("kimi-code".into()),
                ..status(ProgramState::Done)
            })
        );
        for (body, state) in [
            ("state=idle", ProgramState::Idle),
            ("state=error:msg=Zg==", ProgramState::Error),
            ("state=clear", ProgramState::Clear),
            // Pairs a later revision adds, and repeats, change nothing.
            ("future=1:state=working:state=idle", ProgramState::Working),
        ] {
            assert_eq!(parse(body.as_bytes()), Some(status(state)), "{body}");
        }
    }

    #[test]
    fn reports_this_revision_does_not_describe_are_ignored() {
        for body in [
            &b"state=thinking"[..],
            b"app=pi",
            b"",
            b"state=working:id=",
            b"state=working:id=a b",
            b"state=working:id=\x1b[2J",
            b"state=working\xff",
        ] {
            assert_eq!(parse(body), None, "{body:?}");
        }
        let long = format!("state=working:id={}", "a".repeat(MAX_ID + 1));
        assert_eq!(parse(long.as_bytes()), None);
        // An app or kind off the grammar is left out, the report kept.
        assert_eq!(
            parse(b"state=blocked:app=a b:kind=\x07"),
            Some(status(ProgramState::Blocked))
        );
    }
}
