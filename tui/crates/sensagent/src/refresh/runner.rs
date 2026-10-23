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
use super::sign_in::{self, SignInEvent, SignInFailure, SignInState};
use super::StatusRefresher;
use crate::auth::{AccountSession, AccountWebsite, CredentialStore, WallClock};
use crate::dispatch::{DispatchError, FeaturesCompletion, FeaturesJob, StatusConsentOffer};
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
    SignInStart(Sender<()>),
    SignInStatus(Sender<SignInState>),
    SignInCancel(Sender<()>),
}

enum Message {
    Resync(Vec<SessionView>),
    Event(Box<EventEnvelope>),
    Disconnected,
    Captured(Box<Captured>),
    Completed(Completed),
    FeaturesRead(FeaturesCompletion),
    SignIn(u64, SignInEvent),
    Command(Command),
    Stop,
}

enum Work<A, C> {
    Capture(CaptureWork),
    Dispatch(Dispatch<A, C>),
    Features(FeaturesJob<A, C>),
}

/// The current device sign-in, held by the owner thread.
struct SignIn {
    attempt: u64,
    cancel: Arc<AtomicBool>,
    state: SignInState,
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
        Self::spawn_with_website(refresher, api, None)
    }

    /// Like `spawn`; sign-in prompts also carry `website`'s sign-in-then-approve link.
    pub fn spawn_with_website<S, A, C>(
        refresher: StatusRefresher<S, A, C>,
        api: Arc<dyn SessionApi>,
        website: Option<AccountWebsite>,
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
        let owner_messages = messages.clone();
        runner.owner = Some(spawn("rakumux-status-owner", move || {
            own(refresher, &inbox, &work, &owner_messages, website.as_ref())
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

    /// Start a device sign-in, replacing any unfinished one. Follow it with
    /// `sign_in_status`; on success the account is adopted and summaries follow the
    /// account's switch.
    pub fn sign_in_start(&self) -> Result<(), RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::SignInStart(reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// Where the current sign-in is up to.
    pub fn sign_in_status(&self) -> Result<SignInState, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::SignInStatus(reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// Abandon an unfinished sign-in; an existing account stays signed in.
    pub fn sign_in_cancel(&self) -> Result<(), RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::SignInCancel(reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
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
    messages: &Sender<Message>,
    website: Option<&AccountWebsite>,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let origin = Instant::now();
    let mut next_tick = Duration::ZERO;
    let mut sign_in = SignIn {
        attempt: 0,
        cancel: Arc::new(AtomicBool::new(false)),
        state: SignInState::Idle,
    };
    loop {
        let now = origin.elapsed();
        if now >= next_tick {
            for capture in refresher.tick(now) {
                // Workers outlive the owner, so the queue only closes with it.
                let _ = work.send(Work::Capture(capture));
            }
            if let Some(job) = refresher.features_job(now) {
                let _ = work.send(Work::Features(job));
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
            Message::FeaturesRead(completion) => refresher.features_read(completion, now),
            // A replaced attempt's progress is ignored; its login was fenced anyway.
            Message::SignIn(attempt, event) if attempt == sign_in.attempt => {
                sign_in.state = match event {
                    SignInEvent::Prompt(prompt) => SignInState::Waiting(prompt),
                    SignInEvent::SignedIn(session) => {
                        match refresher.set_account_session(Some(session)) {
                            Ok(()) => SignInState::SignedIn,
                            Err(_) => SignInState::Failed(SignInFailure::Failed),
                        }
                    }
                    SignInEvent::Failed(failure) => SignInState::Failed(failure),
                };
            }
            Message::SignIn(..) => {}
            Message::Command(command) => control(
                &mut refresher,
                command,
                now,
                &mut sign_in,
                messages,
                website,
            ),
        }
    }
}

fn control<S, A, C>(
    refresher: &mut StatusRefresher<S, A, C>,
    command: Command,
    now: Duration,
    sign_in: &mut SignIn,
    messages: &Sender<Message>,
    website: Option<&AccountWebsite>,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
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
            replace_sign_in(sign_in, SignInState::Idle);
            let _ = reply.send(refresher.sign_out());
        }
        Command::SignInStart(reply) => {
            replace_sign_in(sign_in, SignInState::Starting);
            let attempt = sign_in.attempt;
            let owner = messages.clone();
            let started = sign_in::spawn(
                refresher.account_service(),
                website.cloned(),
                sign_in.cancel.clone(),
                move |event| {
                    let _ = owner.send(Message::SignIn(attempt, event));
                },
            );
            if started.is_err() {
                sign_in.state = SignInState::Failed(SignInFailure::Failed);
            }
            let _ = reply.send(());
        }
        Command::SignInStatus(reply) => {
            let _ = reply.send(sign_in.state.clone());
        }
        Command::SignInCancel(reply) => {
            let state = match sign_in.state {
                SignInState::Starting | SignInState::Waiting(_) => {
                    SignInState::Failed(SignInFailure::Canceled)
                }
                ref other => other.clone(),
            };
            replace_sign_in(sign_in, state);
            let _ = reply.send(());
        }
    }
}

/// Cancel the current sign-in thread (if any) and start a new attempt generation.
fn replace_sign_in(sign_in: &mut SignIn, state: SignInState) {
    sign_in.cancel.store(true, Ordering::Release);
    sign_in.cancel = Arc::new(AtomicBool::new(false));
    sign_in.attempt += 1;
    sign_in.state = state;
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
            Ok(Work::Features(job)) => match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                Ok(completion) => Message::FeaturesRead(completion),
                Err(_) => continue,
            },
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
