//! Host event processing threads.
//!
//! - The **relay** drains the host's event receiver into an unbounded
//!   internal channel and never takes a lock, so a host that delivers events
//!   through a bounded channel can't block on the control layer (which may
//!   be calling into the host while holding its state lock).
//! - The **processor** applies events (lifecycle detection, exits, cwd,
//!   titles) and ticks every [`TICK`] for report expiry and the git context
//!   poll. It holds only a weak reference to the service and stops when the
//!   service is dropped or shut down.

use super::service::Inner;
use crate::host::HostEvent;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

pub(super) const TICK: Duration = Duration::from_millis(250);

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
    loop {
        let event = match rx.recv_timeout(TICK) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| match event {
            Some(event) => inner.on_host_event(event),
            None => inner.on_tick(),
        }));
        if outcome.is_err() {
            crate::log_error!("control: host event handler panicked; continuing");
        }
    }
}
