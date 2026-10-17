//! Per-attachment delivery with an in-actor backlog.
//!
//! The actor never blocks on an attachment's channel: events that don't fit
//! wait in the sink's backlog and are pumped as the client drains. The actor
//! decides what to do when a backlog grows (controller backpressure,
//! observer resync) or stalls (close).

use session_core::api::{Role, StreamEvent};
use session_core::client::AttachEvent;
use std::collections::VecDeque;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::{Duration, Instant};

pub(crate) type AttachmentId = u64;

pub(crate) struct Sink {
    pub(crate) id: AttachmentId,
    pub(crate) role: Role,
    tx: SyncSender<AttachEvent>,
    backlog: VecDeque<AttachEvent>,
    backlog_bytes: usize,
    /// Backlog non-empty and no progress since this instant.
    stalled_since: Option<Instant>,
    /// The receiving side is gone.
    closed: bool,
}

/// Approximate memory held by a queued event.
fn weight(event: &AttachEvent) -> usize {
    match event {
        AttachEvent::Output { bytes, .. } => bytes.len(),
        AttachEvent::Event(StreamEvent::Resized { checkpoint, .. }) => checkpoint.len(),
        _ => 64,
    }
}

impl Sink {
    pub(crate) fn new(id: AttachmentId, role: Role, tx: SyncSender<AttachEvent>) -> Self {
        Self {
            id,
            role,
            tx,
            backlog: VecDeque::new(),
            backlog_bytes: 0,
            stalled_since: None,
            closed: false,
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn backlog_bytes(&self) -> usize {
        self.backlog_bytes
    }

    pub(crate) fn has_backlog(&self) -> bool {
        !self.backlog.is_empty()
    }

    /// How long the backlog has made no progress.
    pub(crate) fn stalled_for(&self, now: Instant) -> Option<Duration> {
        self.stalled_since
            .map(|since| now.saturating_duration_since(since))
    }

    /// Queue `event` behind anything backlogged and deliver what fits.
    pub(crate) fn push(&mut self, event: AttachEvent, now: Instant) {
        if self.closed {
            return;
        }
        if self.backlog.is_empty() {
            match self.tx.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    self.backlog_bytes += weight(&event);
                    self.backlog.push_back(event);
                    self.stalled_since = Some(now);
                }
                Err(TrySendError::Disconnected(_)) => self.close(),
            }
        } else {
            self.backlog_bytes += weight(&event);
            self.backlog.push_back(event);
            self.pump(now);
        }
    }

    /// Move backlogged events into the channel while it has room.
    pub(crate) fn pump(&mut self, now: Instant) {
        while let Some(event) = self.backlog.pop_front() {
            let size = weight(&event);
            match self.tx.try_send(event) {
                Ok(()) => {
                    self.backlog_bytes -= size;
                    self.stalled_since = Some(now);
                }
                Err(TrySendError::Full(event)) => {
                    self.backlog.push_front(event);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.close();
                    return;
                }
            }
        }
        self.stalled_since = None;
    }

    /// Replace backlogged output (and resizes it superseded) with `lagged`
    /// and a fresh `checkpoint`, keeping other events in order.
    pub(crate) fn resync(&mut self, lagged: AttachEvent, checkpoint: AttachEvent, now: Instant) {
        self.backlog.retain(|event| {
            !matches!(
                event,
                AttachEvent::Output { .. }
                    | AttachEvent::Event(StreamEvent::Resized { .. })
                    | AttachEvent::Event(StreamEvent::Lagged { .. })
            )
        });
        self.backlog_bytes = self.backlog.iter().map(weight).sum();
        self.backlog.push_back(lagged);
        self.backlog.push_back(checkpoint);
        self.backlog_bytes += self.backlog.iter().rev().take(2).map(weight).sum::<usize>();
        if self.stalled_since.is_none() {
            self.stalled_since = Some(now);
        }
        self.pump(now);
    }

    /// Drop everything and stop delivering; the receiver sees the channel
    /// close once it has drained what was already sent.
    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.backlog.clear();
        self.backlog_bytes = 0;
        self.stalled_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    fn output(offset: u64, len: usize) -> AttachEvent {
        AttachEvent::Output {
            offset,
            bytes: vec![b'x'; len],
        }
    }

    #[test]
    fn backlogs_when_full_and_pumps_in_order() {
        let (tx, rx) = sync_channel(1);
        let mut sink = Sink::new(1, Role::Observer, tx);
        let now = Instant::now();
        sink.push(output(0, 10), now);
        sink.push(output(10, 20), now);
        sink.push(AttachEvent::Event(StreamEvent::Bell), now);
        assert!(sink.has_backlog());
        assert_eq!(sink.backlog_bytes(), 20 + 64);
        assert!(sink.stalled_for(now).is_some());

        assert!(matches!(
            rx.recv().unwrap(),
            AttachEvent::Output { offset: 0, .. }
        ));
        sink.pump(now);
        assert!(matches!(
            rx.recv().unwrap(),
            AttachEvent::Output { offset: 10, .. }
        ));
        sink.pump(now);
        assert_eq!(rx.recv().unwrap(), AttachEvent::Event(StreamEvent::Bell));
        sink.pump(now);
        assert!(!sink.has_backlog());
        assert_eq!(sink.stalled_for(now), None);
    }

    #[test]
    fn resync_replaces_output_with_lag_marker_and_checkpoint() {
        let (tx, rx) = sync_channel(1);
        let mut sink = Sink::new(1, Role::Observer, tx);
        let now = Instant::now();
        sink.push(output(0, 5), now);
        sink.push(output(5, 5), now);
        sink.push(
            AttachEvent::Event(StreamEvent::Title { title: "t".into() }),
            now,
        );
        sink.push(output(10, 5), now);
        let checkpoint = AttachEvent::Event(StreamEvent::Resized {
            cols: 80,
            rows: 24,
            checkpoint: "Y2s=".into(),
            offset: 15,
        });
        sink.resync(
            AttachEvent::Event(StreamEvent::Lagged { offset: 15 }),
            checkpoint.clone(),
            now,
        );
        let mut got = vec![rx.recv().unwrap()];
        while sink.has_backlog() {
            sink.pump(now);
            got.push(rx.recv().unwrap());
        }
        assert!(matches!(got[0], AttachEvent::Output { offset: 0, .. }));
        assert_eq!(
            got[1..],
            [
                AttachEvent::Event(StreamEvent::Title { title: "t".into() }),
                AttachEvent::Event(StreamEvent::Lagged { offset: 15 }),
                checkpoint,
            ]
        );
    }

    #[test]
    fn closes_when_the_receiver_is_gone() {
        let (tx, rx) = sync_channel(1);
        drop(rx);
        let mut sink = Sink::new(1, Role::Controller, tx);
        sink.push(output(0, 1), Instant::now());
        assert!(sink.is_closed());
    }
}
