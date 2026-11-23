//! Host event processing threads.
//!
//! - The **relay** drains the host's event receiver into an unbounded
//!   internal channel and never takes a lock, so a host that delivers events
//!   through a bounded channel can't block on the control layer (which may
//!   be calling into the host while holding its state lock).
//! - The **processor** applies events (lifecycle detection, exits, cwd,
//!   titles) and ticks every [`TICK`] for report expiry, the git context
//!   poll, due heartbeats and first inputs, however many events keep coming.
//!   Of the events waiting, an output event is passed over when a later one
//!   for the same terminal waits too: output only says "look again", so one
//!   look at the latest screen does for both. Every other event is applied in
//!   order. It holds only a weak reference to the service and stops when the
//!   service is dropped or shut down.

use super::service::Inner;
use crate::host::HostEvent;
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

pub(super) const TICK: Duration = Duration::from_millis(250);

/// Handling one event (or a tick) longer than this is logged: every other session's events
/// wait behind it.
const SLOW: Duration = Duration::from_millis(50);

pub(super) fn start(inner: &Arc<Inner>, host_events: Receiver<HostEvent>) -> std::io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let stop = inner.stop.clone();
    thread::Builder::new()
        .name("ptyd-control-relay".into())
        .spawn(move || relay(host_events, tx, stop))?;
    let weak = Arc::downgrade(inner);
    let stop = inner.stop.clone();
    thread::Builder::new()
        .name("ptyd-control-events".into())
        .spawn(move || process(weak, rx, stop))?;
    Ok(())
}

fn relay(host_events: Receiver<HostEvent>, tx: Sender<HostEvent>, stop: Arc<AtomicBool>) {
    loop {
        match host_events.recv_timeout(TICK) {
            Ok(event) => {
                if tx.send(event).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn process(inner: Weak<Inner>, rx: Receiver<HostEvent>, stop: Arc<AtomicBool>) {
    let mut pending = VecDeque::new();
    let mut next_tick = Instant::now() + TICK;
    loop {
        if pending.is_empty() {
            match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(event) => pending.push_back(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        // Everything already waiting, so a terminal's repeated output is looked at once.
        while let Ok(event) = rx.try_recv() {
            pending.push_back(event);
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let event = if Instant::now() >= next_tick {
            next_tick = Instant::now() + TICK;
            None
        } else {
            match next_event(&mut pending) {
                Some(event) => Some(event),
                None => continue,
            }
        };
        let kind = event.as_ref().map_or("tick", kind);
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| match event {
            Some(event) => inner.on_host_event(event),
            None => inner.on_tick(),
        }));
        if outcome.is_err() {
            crate::log_error!("control: host event handler panicked; continuing");
        }
        let took = started.elapsed();
        if took >= SLOW {
            crate::log_info!("control: slow {kind}: {} ms", took.as_millis());
        }
    }
}

/// The next event of `pending` to apply, passing over output a later output event of the same
/// terminal waiting there says again.
fn next_event(pending: &mut VecDeque<HostEvent>) -> Option<HostEvent> {
    while let Some(event) = pending.pop_front() {
        let superseded = match &event {
            HostEvent::Output { id, session_id } => pending.iter().any(|later| {
                matches!(later, HostEvent::Output { id: other, session_id: same }
                    if other == id && same == session_id)
            }),
            _ => false,
        };
        if !superseded {
            return Some(event);
        }
    }
    None
}

/// The event's name in the slow-handling log line.
fn kind(event: &HostEvent) -> &'static str {
    match event {
        HostEvent::Exited { .. } => "exit",
        HostEvent::Title { .. } => "title",
        HostEvent::Cwd { .. } => "cwd",
        HostEvent::Bell { .. } => "bell",
        HostEvent::ProgramStatus { .. } => "program status",
        HostEvent::Output { .. } => "output",
        HostEvent::ControllerChanged { .. } => "controller change",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::ids::{SessionId, TerminalId};

    fn output(terminal: u64, session: &str) -> HostEvent {
        HostEvent::Output {
            id: TerminalId(terminal),
            session_id: SessionId::from_string(session.to_owned()),
        }
    }

    #[test]
    fn a_terminals_repeated_output_is_looked_at_once_and_nothing_else_is_passed_over() {
        let title = HostEvent::Title {
            id: TerminalId(1),
            session_id: SessionId::from_string("a".to_owned()),
            title: "build".into(),
        };
        let exited = HostEvent::Exited {
            id: TerminalId(2),
            session_id: SessionId::from_string("b".to_owned()),
            code: Some(0),
        };
        let mut pending = VecDeque::from([
            output(1, "a"),
            output(2, "b"),
            title.clone(),
            output(1, "a"),
            output(2, "b"),
            exited.clone(),
            output(1, "a"),
        ]);
        let mut applied = Vec::new();
        while let Some(event) = next_event(&mut pending) {
            applied.push(event);
        }
        // b's output before its exit is looked at once; a's at its last place.
        assert_eq!(applied, vec![title, output(2, "b"), exited, output(1, "a")]);
    }
}
