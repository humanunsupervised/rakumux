//! Background work for the TUI. `SessionApi` is blocking, so every call runs
//! off the UI thread and reports back as an [`AppEvent`] on one tokio
//! channel; attach and event streams are pumped from their blocking channels
//! into the same channel by small threads. The event loop `select!`s on it.
//!
//! [`ExecMode::Inline`] runs jobs immediately and polls streams on demand,
//! so tests are deterministic.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use session_core::api::{ApiError, EventEnvelope, ReadResult};
use session_core::client::{AttachEvent, AttachStream, EventStream, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{ProjectView, SessionView};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use super::folders::SyncResult;
use super::types::UpgradeMessage;

/// How long pump threads block before checking for cancellation.
const PUMP_POLL: Duration = Duration::from_millis(100);

/// Everything that happens outside the UI thread.
pub enum AppEvent {
    Connected(Arc<dyn SessionApi>),
    ConnectFailed(ApiError),
    /// The old daemon was stopped after a protocol mismatch.
    DaemonRestarted(Result<super::types::ServerRestart, String>),
    /// Folders loaded, subscription opened and the list loaded (subscribe
    /// first, so no change is missed).
    Synced {
        generation: u64,
        result: Result<SyncResult, ApiError>,
    },
    /// The daemon's folders, refreshed for the Folders modal and new
    /// folders' headers.
    FoldersLoaded {
        result: Result<Vec<ProjectView>, ApiError>,
    },
    /// A folder typed in the Folders modal was added.
    FolderAdded {
        result: Result<ProjectView, ApiError>,
    },
    Pinned {
        id: SessionId,
        pinned: bool,
        result: Result<SessionView, ApiError>,
    },
    SessionEvent {
        generation: u64,
        event: EventEnvelope,
    },
    SubscriptionLost {
        generation: u64,
        error: String,
    },
    Attached {
        generation: u64,
        result: Result<AttachStream, ApiError>,
    },
    Attach {
        generation: u64,
        event: AttachEvent,
    },
    SavedScreen {
        generation: u64,
        result: Result<ReadResult, ApiError>,
    },
    Created {
        result: Result<SessionView, ApiError>,
    },
    Renamed {
        id: SessionId,
        result: Result<SessionView, ApiError>,
        automatic: bool,
    },
    Deleted {
        id: SessionId,
        result: Result<(), ApiError>,
    },
    Killed {
        id: SessionId,
        result: Result<Vec<SessionId>, ApiError>,
    },
    Revived {
        id: SessionId,
        result: Result<SessionView, ApiError>,
    },
    /// Result of marking a session as used (failures become a status).
    Touched {
        result: Result<(), ApiError>,
    },
    NameSuggested {
        id: SessionId,
        /// The name when naming started; the suggestion is dropped if the
        /// session was renamed meanwhile.
        expected_name: String,
        result: Result<Option<String>, String>,
    },
    Upgrade(UpgradeMessage),
    /// Phone access checked at startup: whether it's on, and whether to
    /// offer it.
    PhoneChecked {
        enabled: bool,
        offer: bool,
    },
    /// Phone access turned on, or how to pair read (`None`: it's off).
    PhonePairing(Result<Option<crate::remote_access::Pairing>, String>),
    /// Phone access turned off.
    PhoneOff(Result<(), String>),
    /// A background job panicked or couldn't run.
    JobFailed(String),
}

/// What an attachment needs to stop its pump.
pub type CancelFlag = Arc<AtomicBool>;

pub fn cancel_flag() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    /// Production: `spawn_blocking` jobs, pump threads.
    Threaded,
    /// Tests: jobs run at once; streams are drained by `poll_inline`.
    #[cfg(test)]
    Inline,
}

#[cfg(test)]
struct InlineAttach {
    generation: u64,
    stream: AttachStream,
    cancel: CancelFlag,
}

#[cfg(test)]
struct InlineEvents {
    generation: u64,
    stream: EventStream,
    cancel: CancelFlag,
}

pub struct Exec {
    mode: ExecMode,
    tx: UnboundedSender<AppEvent>,
    #[cfg(test)]
    inline_attach: Vec<InlineAttach>,
    #[cfg(test)]
    inline_events: Vec<InlineEvents>,
}

impl Exec {
    pub fn new(mode: ExecMode) -> (Self, UnboundedReceiver<AppEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                mode,
                tx,
                #[cfg(test)]
                inline_attach: Vec::new(),
                #[cfg(test)]
                inline_events: Vec::new(),
            },
            rx,
        )
    }

    /// Run a blocking job; its result arrives as an event.
    pub fn job<F>(&self, job: F)
    where
        F: FnOnce() -> AppEvent + Send + 'static,
    {
        match self.mode {
            #[cfg(test)]
            ExecMode::Inline => {
                let _ = self.tx.send(job());
            }
            ExecMode::Threaded => {
                let tx = self.tx.clone();
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        handle.spawn(async move {
                            let event = match tokio::task::spawn_blocking(job).await {
                                Ok(event) => event,
                                Err(err) => AppEvent::JobFailed(err.to_string()),
                            };
                            let _ = tx.send(event);
                        });
                    }
                    Err(_) => {
                        let _ = tx.send(AppEvent::JobFailed("no async runtime".into()));
                    }
                }
            }
        }
    }

    /// Run an async job (network calls) on the runtime.
    pub fn task<F>(&self, task: F)
    where
        F: Future<Output = AppEvent> + Send + 'static,
    {
        let tx = self.tx.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = tx.send(task.await);
                });
            }
            Err(_) => {
                let _ = tx.send(AppEvent::JobFailed("no async runtime".into()));
            }
        }
    }

    /// Forward an attach stream's events until it closes or `cancel` is
    /// set. Dropping the stream detaches.
    pub fn pump_attach(&mut self, generation: u64, stream: AttachStream, cancel: CancelFlag) {
        match self.mode {
            #[cfg(test)]
            ExecMode::Inline => self.inline_attach.push(InlineAttach {
                generation,
                stream,
                cancel,
            }),
            ExecMode::Threaded => {
                let tx = self.tx.clone();
                let spawned = std::thread::Builder::new()
                    .name("rakumux-attach".into())
                    .spawn(move || {
                        while !cancel.load(Ordering::Relaxed) {
                            let Some(event) = stream.recv_timeout(PUMP_POLL) else {
                                continue;
                            };
                            let closed = matches!(event, AttachEvent::Closed(_));
                            if tx.send(AppEvent::Attach { generation, event }).is_err() || closed {
                                break;
                            }
                        }
                    });
                if let Err(err) = spawned {
                    let _ = self.tx.send(AppEvent::Attach {
                        generation,
                        event: AttachEvent::Closed(Some(format!("can't start reader: {err}"))),
                    });
                }
            }
        }
    }

    /// Forward subscription events until the stream fails or `cancel` is set.
    pub fn pump_events(&mut self, generation: u64, stream: EventStream, cancel: CancelFlag) {
        match self.mode {
            #[cfg(test)]
            ExecMode::Inline => self.inline_events.push(InlineEvents {
                generation,
                stream,
                cancel,
            }),
            ExecMode::Threaded => {
                let tx = self.tx.clone();
                let spawned = std::thread::Builder::new()
                    .name("rakumux-events".into())
                    .spawn(move || {
                        while !cancel.load(Ordering::Relaxed) {
                            match stream.recv_timeout(PUMP_POLL) {
                                None => continue,
                                Some(Ok(event)) => {
                                    if tx
                                        .send(AppEvent::SessionEvent { generation, event })
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Some(Err(error)) => {
                                    if !cancel.load(Ordering::Relaxed) {
                                        let _ = tx
                                            .send(AppEvent::SubscriptionLost { generation, error });
                                    }
                                    break;
                                }
                            }
                        }
                    });
                if let Err(err) = spawned {
                    let _ = self.tx.send(AppEvent::SubscriptionLost {
                        generation,
                        error: format!("can't start reader: {err}"),
                    });
                }
            }
        }
    }

    /// Inline mode: move whatever the streams hold into the event channel.
    #[cfg(test)]
    pub fn poll_inline(&mut self) {
        let tx = &self.tx;
        self.inline_attach.retain(|entry| {
            if entry.cancel.load(Ordering::Relaxed) {
                return false;
            }
            while let Some(event) = entry.stream.try_recv() {
                let closed = matches!(event, AttachEvent::Closed(_));
                let _ = tx.send(AppEvent::Attach {
                    generation: entry.generation,
                    event,
                });
                if closed {
                    return false;
                }
            }
            true
        });
        self.inline_events.retain(|entry| {
            if entry.cancel.load(Ordering::Relaxed) {
                return false;
            }
            while let Some(item) = entry.stream.try_recv() {
                match item {
                    Ok(event) => {
                        let _ = tx.send(AppEvent::SessionEvent {
                            generation: entry.generation,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = tx.send(AppEvent::SubscriptionLost {
                            generation: entry.generation,
                            error,
                        });
                        return false;
                    }
                }
            }
            true
        });
    }
}
