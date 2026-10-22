use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use session_core::api::{CallerInfo, EventEnvelope, SubscribeRequest};
use session_core::client::SessionApi;
use session_core::ids::SessionId;
use session_core::model::SessionView;
use thiserror::Error;

use super::engine::{AIStatus, CaptureWork, Captured, Completed, Dispatch, RefreshState};
use super::schedule::MAX_RUNNING;
use super::StatusRefresher;
use crate::auth::{AccountSession, CredentialStore, WallClock};
use crate::dispatch::{DispatchError, StatusConsentOffer};
use crate::status::{ConsentRevision, ConsentSnapshot, FolderConsent, StatusStore};

/// How often the owner rechecks jobs and plans refreshes.
const TICK: Duration = Duration::from_secs(1);
/// How often stream readers check for a stop.
const POLL: Duration = Duration::from_millis(100);
/// First and longest wait before resubscribing to the daemon.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// Static runner failures; never daemon, service or account text.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RefreshError {
    /// The refresher has stopped.
    #[error("AI-status refreshing has stopped")]
    Stopped,
    /// Only a user SessionApi client may capture sessions.
    #[error("AI-status refreshing requires a user client of the session daemon")]
    NotPermitted,
    /// A refresher thread could not be started.
    #[error("unable to start AI-status refreshing")]
    Thread,
    /// The coordinator refused the control.
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
}

type Reply<T> = Sender<Result<T, DispatchError>>;

enum Command {
    Statuses(Vec<SessionId>, Reply<Vec<AIStatus>>),
    State(Sender<RefreshState>),
    Account(Sender<Option<AccountSession>>),
    Consent(Reply<ConsentSnapshot>),
    ConsentOffer(Reply<StatusConsentOffer>),
    Enable(
        StatusConsentOffer,
        Vec<FolderConsent>,
        Reply<ConsentSnapshot>,
    ),
    Disable(ConsentRevision, Reply<ConsentSnapshot>),
    SetAccountSession(Option<AccountSession>, Reply<()>),
    SignOut(Reply<()>),
}

enum Message {
    Resync(Vec<SessionView>),
    Event(Box<EventEnvelope>),
    Disconnected,
    Captured(Box<Captured>),
    Completed(Completed),
    Command(Command),
    Stop,
}

enum Work<A, C> {
    Capture(CaptureWork),
    Dispatch(Dispatch<A, C>),
}

/// Runs a [`StatusRefresher`] for one Rakumux home: an owner thread holding the
/// coordinator, a thread following the daemon's sessions, and two workers for
/// captures and requests. Not an installed service or an IPC boundary: the embedding
/// process owns it and must keep controls user-only. Dropping it stops the owner at
/// once. The source thread and workers end on their own: a worker blocked in a request
/// at the request's deadline, submitting nothing more.
pub struct RefreshRunner {
    messages: Sender<Message>,
    stop: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
}

/// Account, consent and status controls for a running refresher. Cloneable; every
/// call is answered by the owner thread in order.
#[derive(Clone)]
pub struct RefreshClient {
    messages: Sender<Message>,
}

impl RefreshRunner {
    /// Start following `api`'s sessions (it must be a user client: session callers
    /// see only their own folder and can't capture). Refreshing starts once the
    /// first session list arrives, and only under the coordinator's confirmed consent.
    pub fn spawn<S, A, C>(
        refresher: StatusRefresher<S, A, C>,
        api: Arc<dyn SessionApi>,
    ) -> Result<Self, RefreshError>
    where
        S: StatusStore + 'static,
        A: CredentialStore + 'static,
        C: WallClock + 'static,
    {
        if !matches!(api.caller(), CallerInfo::User) {
            return Err(RefreshError::NotPermitted);
        }
        let (messages, inbox) = mpsc::channel();
        let (work, queue) = mpsc::channel::<Work<A, C>>();
        let queue = Arc::new(Mutex::new(queue));
        let stop = Arc::new(AtomicBool::new(false));
        let mut runner = Self {
            messages: messages.clone(),
            stop: stop.clone(),
            owner: None,
        };
        for _ in 0..MAX_RUNNING {
            let (queue, api, messages) = (queue.clone(), api.clone(), messages.clone());
            spawn("rakumux-status-work", move || {
                perform(&queue, &*api, &messages)
            })?;
        }
        runner.owner = Some(spawn("rakumux-status-owner", move || {
            own(refresher, &inbox, &work)
        })?);
        // Not joined: a daemon request can block for its whole timeout. The thread ends
        // at its next check of `stop`, or once the owner is gone.
        let source = api.clone();
        spawn("rakumux-status-source", move || {
            follow(&*source, &messages, &stop)
        })?;
        Ok(runner)
    }

    /// A control client for this refresher.
    pub fn client(&self) -> RefreshClient {
        RefreshClient {
            messages: self.messages.clone(),
        }
    }

    /// Stop and wait for the owner thread; outstanding jobs are canceled.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.messages.send(Message::Stop);
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

impl Drop for RefreshRunner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl RefreshClient {
    /// Status for each session, in order.
    pub fn statuses(&self, sessions: Vec<SessionId>) -> Result<Vec<AIStatus>, RefreshError> {
        self.call(|reply| Command::Statuses(sessions, reply))
    }

    pub fn state(&self) -> Result<RefreshState, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::State(reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// The adopted account view, for display.
    pub fn account(&self) -> Result<Option<AccountSession>, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Account(reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    pub fn consent(&self) -> Result<ConsentSnapshot, RefreshError> {
        self.call(Command::Consent)
    }

    /// The exact account, consent and destination a trusted user control must show.
    pub fn consent_offer(&self) -> Result<StatusConsentOffer, RefreshError> {
        self.call(Command::ConsentOffer)
    }

    /// Confirm a user's explicit choice for `offer`.
    pub fn enable(
        &self,
        offer: StatusConsentOffer,
        folders: Vec<FolderConsent>,
    ) -> Result<ConsentSnapshot, RefreshError> {
        self.call(|reply| Command::Enable(offer, folders, reply))
    }

    pub fn disable(&self, expected: ConsentRevision) -> Result<ConsentSnapshot, RefreshError> {
        self.call(|reply| Command::Disable(expected, reply))
    }

    pub fn set_account_session(&self, session: Option<AccountSession>) -> Result<(), RefreshError> {
        self.call(|reply| Command::SetAccountSession(session, reply))
    }

    pub fn sign_out(&self) -> Result<(), RefreshError> {
        self.call(Command::SignOut)
    }

    fn call<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(command(reply))?;
        Ok(answer.recv().map_err(|_| RefreshError::Stopped)??)
    }

    fn send(&self, command: Command) -> Result<(), RefreshError> {
        self.messages
            .send(Message::Command(command))
            .map_err(|_| RefreshError::Stopped)
    }
}

impl fmt::Debug for RefreshRunner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RefreshRunner")
    }
}

impl fmt::Debug for RefreshClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RefreshClient")
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, RefreshError> {
    thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .map_err(|_| RefreshError::Thread)
}

/// The owner thread: every coordinator call happens here.
fn own<S, A, C>(
    mut refresher: StatusRefresher<S, A, C>,
    inbox: &Receiver<Message>,
    work: &Sender<Work<A, C>>,
) where
    S: StatusStore,
    A: CredentialStore,
    C: WallClock,
{
    let origin = Instant::now();
    let mut next_tick = Duration::ZERO;
    loop {
        let now = origin.elapsed();
        if now >= next_tick {
            for capture in refresher.tick(now) {
                // Workers outlive the owner, so the queue only closes with it.
                let _ = work.send(Work::Capture(capture));
            }
            next_tick = now.saturating_add(TICK);
        }
        let message = match inbox.recv_timeout(next_tick.saturating_sub(origin.elapsed())) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let now = origin.elapsed();
        match message {
            Message::Stop => return,
            Message::Resync(views) => {
                let _ = refresher.resync(&views, now);
            }
            Message::Event(event) => {
                let _ = refresher.apply_event(&event, now);
            }
            Message::Disconnected => refresher.disconnected(),
            Message::Captured(captured) => {
                if let Some(dispatch) = refresher.captured(*captured, now) {
                    let _ = work.send(Work::Dispatch(dispatch));
                }
            }
            Message::Completed(completed) => refresher.completed(completed, now),
            Message::Command(command) => control(&mut refresher, command, now),
        }
    }
}

fn control<S, A, C>(refresher: &mut StatusRefresher<S, A, C>, command: Command, now: Duration)
where
    S: StatusStore,
    A: CredentialStore,
    C: WallClock,
{
    // A caller that stopped waiting is not an error.
    match command {
        Command::Statuses(sessions, reply) => {
            let statuses = sessions
                .iter()
                .map(|session| refresher.status(session))
                .collect();
            let _ = reply.send(statuses);
        }
        Command::State(reply) => {
            let _ = reply.send(refresher.state(now));
        }
        Command::Account(reply) => {
            let _ = reply.send(refresher.account().cloned());
        }
        Command::Consent(reply) => {
            let _ = reply.send(refresher.consent());
        }
        Command::ConsentOffer(reply) => {
            let _ = reply.send(refresher.consent_offer());
        }
        Command::Enable(offer, folders, reply) => {
            let _ = reply.send(refresher.enable(&offer, folders));
        }
        Command::Disable(expected, reply) => {
            let _ = reply.send(refresher.disable(expected));
        }
        Command::SetAccountSession(session, reply) => {
            let _ = reply.send(refresher.set_account_session(session));
        }
        Command::SignOut(reply) => {
            let _ = reply.send(refresher.sign_out());
        }
    }
}

/// A worker: captures and requests, one at a time, returned to the owner.
fn perform<A, C>(queue: &Mutex<Receiver<Work<A, C>>>, api: &dyn SessionApi, owner: &Sender<Message>)
where
    A: CredentialStore,
    C: WallClock,
{
    loop {
        let next = match queue.lock() {
            Ok(queue) => queue.recv(),
            Err(_) => return,
        };
        let message = match next {
            Err(_) => return,
            Ok(Work::Capture(work)) => Message::Captured(Box::new(work.capture(api))),
            // A panicking request loses its job; the coordinator keeps its slot fenced.
            Ok(Work::Dispatch(dispatch)) => {
                match panic::catch_unwind(AssertUnwindSafe(|| dispatch.run())) {
                    Ok(completed) => Message::Completed(completed),
                    Err(_) => continue,
                }
            }
        };
        if owner.send(message).is_err() {
            return;
        }
    }
}

/// Follow the daemon: subscribe to every folder, then list (so no change falls between
/// the two), forward events, and start again with backoff when the stream ends.
fn follow(api: &dyn SessionApi, owner: &Sender<Message>, stop: &AtomicBool) {
    let mut delay = RECONNECT_MIN;
    while !stop.load(Ordering::SeqCst) {
        let request = SubscribeRequest {
            all: true,
            ..Default::default()
        };
        if let Ok(stream) = api.subscribe(request) {
            if let Ok(views) = api.list_all(false) {
                if owner.send(Message::Resync(views)).is_err() {
                    return;
                }
                delay = RECONNECT_MIN;
                while !stop.load(Ordering::SeqCst) {
                    match stream.recv_timeout(POLL) {
                        None => {}
                        Some(Ok(event)) => {
                            if owner.send(Message::Event(Box::new(event))).is_err() {
                                return;
                            }
                        }
                        Some(Err(_)) => break,
                    }
                }
            }
        }
        if owner.send(Message::Disconnected).is_err() {
            return;
        }
        let resume = Instant::now() + delay;
        while !stop.load(Ordering::SeqCst) && Instant::now() < resume {
            thread::sleep(POLL.min(resume.saturating_duration_since(Instant::now())));
        }
        delay = delay.saturating_mul(2).min(RECONNECT_MAX);
    }
}
