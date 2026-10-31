//! `session_answer`'s steps: working an agent's question dialog key by key
//! from its screen (`agent_output::questions`). Each step is planned from the
//! screen read just before it, sent only while the agent still asks
//! something and nobody typed in the session lately, and counts as taken
//! once the dialog shows it moved on and stays so; the first that doesn't
//! ends it, nothing more sent.

use crate::agent_output::questions::{Dialog, Next};
use crate::host::HostError;
use std::time::{Duration, Instant};

/// How long a step may take to show on screen.
const STEP_WAIT: Duration = Duration::from_secs(3);
/// How often the screen is read meanwhile.
const STEP_POLL: Duration = Duration::from_millis(40);
/// A step's screen counts as drawn once the dialog stays the same this long.
const SETTLE: Duration = Duration::from_millis(150);
/// The most steps one dialog takes: four questions of four options, each
/// box, free answer and `Next`, and the review, need well under this.
const MAX_STEPS: usize = 80;

/// The session's terminal, as answering sees it.
pub(super) trait Terminal {
    /// Its visible screen now.
    fn screen(&self) -> Option<String>;
    /// Its agent asks something now (a question, a prompt for permission),
    /// as the daemon has it or as detection reads the screen.
    fn asking(&self) -> bool;
    /// Send one step's keys or text, unless someone typed there lately
    /// ([`HostError::Typing`], nothing written).
    fn send(&self, step: &Next) -> Result<(), HostError>;
}

/// How working the dialog went.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Worked {
    /// The dialog took the answers and closed.
    Done,
    /// Nothing was sent: the screen doesn't show the dialog, or its agent
    /// asks nothing (why).
    NotShown(&'static str),
    /// Nothing was sent: someone typed in the session lately.
    Typing,
    /// Keys went in, then the screen showed something else (why): the
    /// dialog may be partly answered.
    Stopped(&'static str),
}

/// Answer `dialog` on `terminal`, a step at a time.
pub(super) fn work(dialog: &mut Dialog, terminal: &dyn Terminal) -> Worked {
    let Some(mut screen) = terminal.screen() else {
        return Worked::NotShown("the session's screen can't be read");
    };
    if !dialog.shown(&screen) {
        return Worked::NotShown("the screen doesn't show the question");
    }
    let mut sent = false;
    let stop = |sent: bool, why| {
        if sent {
            Worked::Stopped(why)
        } else {
            Worked::NotShown(why)
        }
    };
    for _ in 0..MAX_STEPS {
        let step = dialog.next(&screen);
        match step {
            Next::Done => return Worked::Done,
            Next::Stop(why) => return stop(sent, why),
            Next::Keys(_) | Next::Type(_) => {}
        }
        if !terminal.asking() {
            return stop(sent, "the agent asks nothing now");
        }
        match terminal.send(&step) {
            Ok(()) => sent = true,
            Err(HostError::Typing) if !sent => return Worked::Typing,
            Err(HostError::Typing) => return Worked::Stopped("someone typed in the session"),
            Err(_) => return stop(sent, "the session took no keys"),
        }
        match drawn(dialog, terminal, &screen) {
            Some(next) => screen = next,
            None => return Worked::Stopped("the dialog didn't take a step"),
        }
    }
    Worked::Stopped("the dialog took too many steps")
}

/// The screen once `dialog` moved on from `before` and stayed so for
/// [`SETTLE`]; none when it didn't within [`STEP_WAIT`].
fn drawn(dialog: &Dialog, terminal: &dyn Terminal, before: &str) -> Option<String> {
    let start = Instant::now();
    let mut moved: Option<(String, Instant)> = None;
    while start.elapsed() < STEP_WAIT {
        std::thread::sleep(STEP_POLL);
        let now = terminal.screen()?;
        if !dialog.moved(before, &now) {
            moved = None;
            continue;
        }
        match &moved {
            Some((seen, since)) if !dialog.moved(seen, &now) => {
                if since.elapsed() >= SETTLE {
                    return Some(now);
                }
            }
            _ => moved = Some((now, Instant::now())),
        }
    }
    None
}
