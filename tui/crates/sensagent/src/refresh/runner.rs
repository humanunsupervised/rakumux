use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SendError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use session_core::api::{
    AgentReply, CallerInfo, CreateSession, ErrorCode, EventEnvelope, QuestionAnswer,
    ResourceSample, SessionEvent, SubscribeRequest, TranscriptCursor, TranscriptPage,
    CAPABILITY_ANSWER, CAPABILITY_RESOURCES, CAPABILITY_SUBMIT_BLOCKED,
};
use session_core::client::SessionApi;
use session_core::ids::{ReplyId, SessionId};
use session_core::model::{SessionStatus, SessionView};
use thiserror::Error;

use super::awake::Awake;
use super::engine::{AIStatus, CaptureWork, Captured, Completed, Dispatch, RefreshState};
use super::renewal::{self, Renewal, RenewalEvent};
use super::schedule::MAX_RUNNING;
use super::sign_in::{self, SignInEvent, SignInFailure, SignInState};
use super::titles::{Retitle, Retitled};
use super::StatusRefresher;
use crate::auth::{
    AccountError, AccountIdentity, AccountSession, AccountWebsite, AuthError, CredentialRevision,
    CredentialStore, WallClock,
};
use crate::cloud::FeatureChange;
use crate::dispatch::{DispatchError, FeaturesCompletion, FeaturesJob, StatusConsentOffer};
use crate::feed::{
    ChatSync, ChatWork, FeedCompletion, FeedFailure, FeedJob, FeedPage, FeedReply, FeedRequest,
    FeedResult, FeedSync, Fetch, Fetched, NewSessionSync, NewSessionWork, PokeEvent, PokeSync,
    RealtimeToken, ReplySync, ReplyWork, ResourceSync, ResourceWork, SessionMessage, SessionSync,
    TurnRead, Uploads,
};
use crate::status::{ConsentRevision, ConsentSnapshot, FolderConsent, StatusStore};

/// How often the owner rechecks jobs and plans refreshes.
const TICK: Duration = Duration::from_secs(1);
/// Requests already out under a name another machine holds are refused too:
/// the name changes at most this often, so they don't change it again.
const RENAME_EVERY: Duration = Duration::from_secs(20);
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
    Renew(Sender<()>),
    /// After, generation, and whether a TUI shows the feed.
    Feed(Option<u64>, Option<u64>, bool, Sender<FeedPage>),
    Summarize(SessionId, Sender<()>),
    SetFeatures(FeatureChange, Reply<()>),
    FeedTurn(String, Option<u64>, Sender<Result<TurnRead, FeedFailure>>),
    /// A reply: entry, text, and its id when the TUI made one.
    FeedReply(
        String,
        String,
        Option<String>,
        Sender<Result<FeedReply, FeedFailure>>,
    ),
    /// A session message: machine, session, text, and its id likewise.
    SessionMessage(
        String,
        String,
        String,
        Option<String>,
        Sender<Result<(), FeedFailure>>,
    ),
    /// Enrol another machine: its id, name and wake URL; answered with its
    /// credential file's contents.
    MachineEnrol(
        String,
        String,
        Option<String>,
        Sender<Result<String, FeedFailure>>,
    ),
    /// Revoke an enrolled machine's credential (its machine id).
    MachineRevoke(String, Sender<Result<(), FeedFailure>>),
}

/// What the daemon announces of what this service uses, read at each
/// (re)subscribe: a daemon restarted since (in place, or replaced) may
/// announce more, or less.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DaemonFeatures {
    /// It answers `resource_usage` (capability `resources`), for rtop.
    resources: bool,
    /// It refuses to type into an agent that asks something (capability
    /// `session.submit.blocked`): replies go in mid-turn only then.
    guards_prompts: bool,
    /// It answers agents' questions in their own dialogs (capability
    /// `session.answer`): answers from rakumux.com are taken only then.
    answers: bool,
}

impl DaemonFeatures {
    /// From the capabilities the daemon announced; none when they aren't known.
    fn of(capabilities: Option<&[String]>) -> Self {
        let has = |wanted: &str| {
            capabilities.is_some_and(|names| names.iter().any(|name| name == wanted))
        };
        Self {
            resources: has(CAPABILITY_RESOURCES),
            guards_prompts: has(CAPABILITY_SUBMIT_BLOCKED),
            answers: has(CAPABILITY_ANSWER),
        }
    }
}

enum Message {
    /// A fresh list: every session the daemon has, dead ones too, and what
    /// the daemon announces of what this service uses.
    Resync(Vec<SessionView>, DaemonFeatures),
    Event(Box<EventEnvelope>),
    Disconnected,
    Captured(Box<Captured>),
    Completed(Completed),
    /// A session was named after its title (or wasn't).
    Retitled(Retitled),
    FeaturesRead(FeaturesCompletion),
    FeedDone(FeedCompletion),
    /// A chat read from the daemon, or why it couldn't be.
    Transcript(SessionId, Result<TranscriptPage, ErrorCode>),
    /// A chat upload finished.
    ChatDone(FeedCompletion),
    /// The session list's upload finished.
    SessionsDone(FeedCompletion),
    /// What the daemon said everything uses now (rtop), or why it couldn't.
    Resources(Result<ResourceSample, ErrorCode>),
    /// A reading's upload finished.
    ResourcesDone(FeedCompletion),
    /// The realtime token request finished.
    RealtimeDone(FeedCompletion),
    /// What realtime connection `generation` did.
    Poke(u64, PokeEvent),
    /// A replies read or a reply's status report finished.
    ReplyDone(FeedCompletion),
    /// A reply was typed into its session, or the daemon refused it.
    ReplySubmitted(String, Result<(), ErrorCode>),
    /// The daemon gave an answer (its reply's id) to its agent's question, or didn't.
    AnswerGiven(String, Result<(), ErrorCode>),
    /// One of a reply's files was fetched and saved (or wasn't), for the
    /// account of that revision.
    Fetched(CredentialRevision, String, Fetched),
    /// A reply written in a TUI was stored (or not), for the TUI waiting on it.
    ReplyWritten(
        Result<FeedReply, FeedFailure>,
        Sender<Result<FeedReply, FeedFailure>>,
    ),
    /// A session message written in a TUI was stored (or not), likewise.
    MessageWritten(
        Result<SessionMessage, FeedFailure>,
        Sender<Result<(), FeedFailure>>,
    ),
    /// The daemon was told to capture replies or not (or refused: an older one).
    CaptureSet(bool),
    /// Replies the daemon kept, for catching up.
    Replies(Vec<AgentReply>),
    /// The daemon started the session a request from rakumux.com (its id)
    /// asked for, or refused.
    NewSessionCreated(String, Result<SessionView, ErrorCode>),
    /// A new session's status report finished.
    NewSessionReported(FeedCompletion),
    SignIn(u64, SignInEvent),
    Renewed(RenewalEvent),
    /// An enrolled machine's sign-in with its credential finished.
    EnrolledSignIn(Result<AccountSession, AccountError>),
    Command(Command),
    Stop,
}

enum Work<A, C> {
    Capture(CaptureWork),
    Dispatch(Dispatch<A, C>),
    /// Name a session after the title a request brought.
    Retitle(Retitle),
    Features(FeaturesJob<A, C>),
    Feed(FeedJob<A, C>),
    /// Read a session's chat after a cursor.
    /// Read a chat after a cursor, with its subagents' messages when asked.
    Transcript(SessionId, Option<TranscriptCursor>, bool),
    /// Upload a chat's messages.
    ChatUpload(FeedJob<A, C>),
    /// Read a turn for a TUI, answered straight to it.
    Turn(FeedJob<A, C>, Sender<Result<TurnRead, FeedFailure>>),
    /// Read replies, or report one's status.
    ReplyRequest(FeedJob<A, C>),
    /// Upload this machine's session list.
    SessionsUpload(FeedJob<A, C>),
    /// Ask the daemon what everything uses now (rtop).
    ReadResources,
    /// Upload what this machine uses.
    ResourcesUpload(FeedJob<A, C>),
    /// Ask where to connect to be poked.
    Realtime(FeedJob<A, C>),
    /// Type a reply (its id) into a session while the program seen in its
    /// foreground is still there.
    Submit(String, SessionId, String, Option<String>),
    /// Give a reply's answers (its id) to the question (its call) a session's
    /// agent asks in its own dialog.
    Answer(String, SessionId, String, Vec<QuestionAnswer>),
    /// Fetch one of a reply's files and save it.
    Fetch(FeedJob<A, C>, Fetch, Uploads),
    /// Remove a reply's saved files.
    Discard(Uploads, String),
    /// Remove replies' files kept long enough.
    CleanUploads(Uploads),
    /// Store a reply written in a TUI, then answer it.
    WriteReply(FeedJob<A, C>, Sender<Result<FeedReply, FeedFailure>>),
    /// Store a session message written in a TUI, then answer it.
    WriteMessage(FeedJob<A, C>, Sender<Result<(), FeedFailure>>),
    /// Enrol machine (its id) of this account, answering with its credential.
    Enrol(
        FeedJob<A, C>,
        String,
        AccountIdentity,
        Sender<Result<String, FeedFailure>>,
    ),
    /// Revoke an enrolled machine's credential, then answer.
    Revoke(FeedJob<A, C>, Sender<Result<(), FeedFailure>>),
    /// Turn the daemon's reply capture on or off.
    ReplyCapture(bool),
    /// Read the replies the daemon kept after this one (all of them for `None`).
    Replies(Option<ReplyId>),
    /// Start the session a request from rakumux.com (its id) asks for.
    CreateSession(String, CreateSession),
    /// Report a new session's status.
    NewSessionReport(FeedJob<A, C>),
}

/// Where the owner sends work. Replies being typed in and replies or session
/// messages written in a TUI have a worker of their own, so they never wait
/// behind the shared workers' file downloads (up to a minute each), AI
/// descriptions and chat uploads: a TUI waits 30 s for its answer, and gives
/// up on one the cloud then stores all the same. rtop's readings go there
/// too, every couple of seconds while someone watches.
struct Workers<A, C> {
    shared: Sender<Work<A, C>>,
    replies: Sender<Work<A, C>>,
}

impl<A, C> Workers<A, C> {
    fn send(&self, work: Work<A, C>) -> Result<(), SendError<Work<A, C>>> {
        match work {
            Work::Submit(..)
            | Work::Answer(..)
            | Work::WriteReply(..)
            | Work::WriteMessage(..)
            | Work::Enrol(..)
            | Work::Revoke(..)
            | Work::ReadResources
            | Work::ResourcesUpload(..)
            | Work::CreateSession(..) => self.replies.send(work),
            work => self.shared.send(work),
        }
    }
}

/// The current device sign-in, held by the owner thread.
struct SignIn {
    attempt: u64,
    cancel: Arc<AtomicBool>,
    state: SignInState,
}

/// Runs a [`StatusRefresher`] for one Rakumux home: an owner thread holding the
/// coordinator, a thread following the daemon's sessions, two workers for
/// captures and requests, and one for replies being typed in and written in a
/// TUI ([`Workers`]). Not an installed service or an IPC boundary: the embedding
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
        let (shared, queue) = mpsc::channel::<Work<A, C>>();
        let queue = Arc::new(Mutex::new(queue));
        let (replies, reply_queue) = mpsc::channel::<Work<A, C>>();
        let reply_queue = Mutex::new(reply_queue);
        let work = Workers { shared, replies };
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
        let (reply_api, reply_messages) = (api.clone(), messages.clone());
        spawn("rakumux-status-reply", move || {
            perform(&reply_queue, &*reply_api, &reply_messages)
        })?;
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

    /// Renew the account's access token now rather than when it's due.
    pub fn renew(&self) -> Result<(), RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Renew(reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// Abandon an unfinished sign-in; an existing account stays signed in.
    pub fn sign_in_cancel(&self) -> Result<(), RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::SignInCancel(reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// Make a new title (and description) of `session` now (the ↻ or `s`): it
    /// renames the session whatever its name.
    pub fn summarize(&self, session: SessionId) -> Result<(), RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Summarize(session, reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// Change the account's feature switches; `state` shows them changed at once,
    /// and saving them to the account follows (see `StatusRefresher::change_features`).
    pub fn set_features(&self, change: FeatureChange) -> Result<(), RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::SetFeatures(change, reply))?;
        Ok(answer.recv().map_err(|_| RefreshError::Stopped)??)
    }

    /// Write `text` back to feed entry `entry`: stored on rakumux.com for the
    /// entry's machine to type into its session (here at once when it runs here).
    /// `id`, a ULID, names the reply (one is made without it), so the same id
    /// written again stores nothing more.
    pub fn feed_reply(
        &self,
        entry: String,
        text: String,
        id: Option<String>,
    ) -> Result<Result<FeedReply, FeedFailure>, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::FeedReply(entry, text, id, reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// Write `text` to session `session` of `machine` itself (a session
    /// message): stored on rakumux.com for that machine to type into the
    /// session once an agent there waits (here at once when it runs here).
    /// `id` as [`RefreshClient::feed_reply`]'s.
    pub fn session_message(
        &self,
        machine: String,
        session: String,
        text: String,
        id: Option<String>,
    ) -> Result<Result<(), FeedFailure>, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::SessionMessage(machine, session, text, id, reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// Enrol machine `machine_id` of this account as `name`, woken at
    /// `wake_url`: the contents of its credential file
    /// ([`crate::enrolment::CREDENTIAL_FILE`]), holding its secret.
    pub fn machine_enrol(
        &self,
        machine_id: String,
        name: String,
        wake_url: Option<String>,
    ) -> Result<Result<String, FeedFailure>, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::MachineEnrol(machine_id, name, wake_url, reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// Revoke enrolled machine `machine_id`'s credential.
    pub fn machine_revoke(
        &self,
        machine_id: String,
    ) -> Result<Result<(), FeedFailure>, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::MachineRevoke(machine_id, reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// The turn feed entry `entry` finished, from rakumux.com, past `after`
    /// when given.
    pub fn feed_turn(
        &self,
        entry: String,
        after: Option<u64>,
    ) -> Result<Result<TurnRead, FeedFailure>, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::FeedTurn(entry, after, reply))?;
        answer.recv().map_err(|_| RefreshError::Stopped)
    }

    /// The feed entries that changed after `after` for a TUI of `generation`.
    /// Asking while `shown` says a TUI shows the feed, so it is read more often.
    pub fn feed(
        &self,
        after: Option<u64>,
        generation: Option<u64>,
        shown: bool,
    ) -> Result<FeedPage, RefreshError> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Feed(after, generation, shown, reply))?;
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
    work: &Workers<A, C>,
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
    let mut renewal = Renewal::default();
    let mut identity = refresher.take_machine();
    let mut machine = identity.name();
    let mut renamed_at: Option<Duration> = None;
    let mut feed = FeedSync::new(machine.clone());
    if refresher.machine_tokens().is_some() {
        feed = feed.without_reads();
    }
    let mut enrolled = EnrolledSignIn::default();
    let mut awake = Awake::new();
    let mut last_tick = origin.elapsed();
    let mut chats = ChatSync::new(machine.clone(), refresher.chat_cursors());
    let mut replies = ReplySync::new(machine.clone(), refresher.reply_state());
    let mut new_sessions = NewSessionSync::new(machine.clone(), refresher.new_session_state());
    let uploads = refresher.uploads().map(Uploads::new);
    let mut sessions = SessionSync::new(machine.clone(), uploads.is_some())
        .handling(crate::feed::NEW_SESSIONS)
        .handling(crate::feed::NEW_SESSION_AGENTS);
    let mut resources = ResourceSync::new();
    let mut pokes = PokeSync::new();
    // The running realtime connection's stop, if one runs.
    let mut poke_stop: Option<Arc<AtomicBool>> = None;
    let uploads = uploads.as_ref();
    let mut next_clean = Duration::ZERO;
    loop {
        let now = origin.elapsed();
        if now >= next_tick {
            // Paused (a Sprite woken by a request to its URL) or asleep: stay up
            // a while, read replies at once and connect to be poked again.
            if Awake::woke(last_tick, now) {
                awake.hold(now);
                replies.poked(now);
                if let Some(stop) = poke_stop.take() {
                    stop.store(true, Ordering::SeqCst);
                    pokes.closed(now);
                    replies.set_pushed(false, now);
                }
            }
            last_tick = now;
            awake.tick(now);
            renew(&refresher, &mut renewal, messages);
            sign_in_enrolled(&refresher, &mut enrolled, messages, now);
            if now >= next_clean {
                if let Some(uploads) = uploads {
                    let _ = work.send(Work::CleanUploads(uploads.clone()));
                }
                next_clean = now.saturating_add(crate::feed::CLEAN_EVERY);
            }
            for capture in refresher.tick(now) {
                // Workers outlive the owner, so the queue only closes with it.
                let _ = work.send(Work::Capture(capture));
            }
            if let Some(job) = refresher.features_job(now) {
                let _ = work.send(Work::Features(job));
            }
            follow_feed(&refresher, &mut feed, work, now);
            follow_chats(&refresher, &mut chats, replies.watched(), work, now);
            follow_replies(
                &refresher,
                &mut replies,
                sessions.confirmed(),
                uploads,
                work,
                now,
            );
            follow_sessions(&refresher, &mut sessions, work, now);
            follow_resources(&refresher, &mut resources, &sessions, work, now);
            let confirmed = sessions.confirmed();
            follow_new_sessions(&refresher, &mut new_sessions, confirmed, work, now);
            follow_pokes(
                &refresher,
                &mut pokes,
                &mut poke_stop,
                &mut replies,
                confirmed,
                work,
                now,
            );
            next_tick = now.saturating_add(TICK);
        }
        let message = match inbox.recv_timeout(next_tick.saturating_sub(origin.elapsed())) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let now = origin.elapsed();
        match message {
            Message::Stop => {
                if let Some(stop) = poke_stop.take() {
                    stop.store(true, Ordering::SeqCst);
                }
                return;
            }
            Message::RealtimeDone(completion) => {
                let current = refresher.is_current(completion.revision);
                let outcome = completion.outcome.and_then(|result| match result {
                    FeedResult::Realtime(token) => Ok(token),
                    _ => Err(FeedFailure::Invalid),
                });
                if let Some((token, generation)) = pokes.token_done(current, outcome, now) {
                    poke_stop = Some(start_listening(token, generation, messages));
                }
            }
            Message::Poke(generation, event) => {
                if pokes.current(generation) {
                    match event {
                        PokeEvent::Connected => {
                            pokes.connected();
                            replies.set_pushed(true, now);
                        }
                        PokeEvent::Poked => replies.poked(now),
                        PokeEvent::Closed => {
                            pokes.closed(now);
                            poke_stop = None;
                            replies.set_pushed(false, now);
                        }
                    }
                    follow_replies(
                        &refresher,
                        &mut replies,
                        sessions.confirmed(),
                        uploads,
                        work,
                        now,
                    );
                }
            }
            Message::Resync(views, daemon) => {
                sessions.set_resources(daemon.resources);
                sessions.set_answers(daemon.answers);
                replies.set_answers(daemon.answers);
                // Every session the daemon has: what's kept of others goes.
                refresher.keep_only(views.iter().map(|view| &view.id));
                let views: Vec<SessionView> = views
                    .into_iter()
                    .filter(|view| view.status != SessionStatus::Dead)
                    .collect();
                let _ = refresher.resync(&views, now);
                let after = feed.connected(&views);
                chats.connected(&views);
                replies.connected(&views, daemon.guards_prompts);
                sessions.connected(&views);
                resources.connected();
                new_sessions.connected(&views);
                let _ = work.send(Work::Replies(after));
            }
            Message::Event(event) => {
                match &event.event {
                    SessionEvent::Reply { reply } => {
                        let reply_to = replies.answered(reply);
                        feed.reply(reply.clone(), reply_to);
                    }
                    SessionEvent::Created { session } | SessionEvent::Updated { session } => {
                        feed.observe(session);
                        chats.observe(session);
                        replies.observe(session);
                        sessions.observe(session);
                        new_sessions.observe(session);
                    }
                    SessionEvent::Removed => {
                        feed.forget(&event.session_id);
                        chats.forget(&event.session_id);
                        replies.forget(&event.session_id);
                        sessions.forget(&event.session_id);
                        new_sessions.forget(&event.session_id);
                    }
                    _ => {}
                }
                let _ = refresher.apply_event(&event, now);
                follow_chats(&refresher, &mut chats, replies.watched(), work, now);
                follow_replies(
                    &refresher,
                    &mut replies,
                    sessions.confirmed(),
                    uploads,
                    work,
                    now,
                );
                follow_sessions(&refresher, &mut sessions, work, now);
            }
            Message::SessionsDone(completion) => {
                let current = refresher.is_current(completion.revision);
                let taken = current && machine_taken(&completion.outcome);
                let outcome = completion.outcome.and_then(|result| match result {
                    FeedResult::SessionsStored(stored) => Ok(stored),
                    _ => Err(FeedFailure::Invalid),
                });
                sessions.done(current, outcome, now);
                chats.set_subagents(sessions.subagents(), now);
                if taken && renamed_at.is_none_or(|at| now.saturating_sub(at) >= RENAME_EVERY) {
                    renamed_at = Some(now);
                    machine = identity.rename();
                    rename(
                        &machine,
                        &mut feed,
                        &mut chats,
                        &mut replies,
                        &mut sessions,
                        &mut new_sessions,
                        now,
                    );
                }
            }
            Message::Resources(result) => {
                // An older daemon after all: the session list stops saying it reports.
                if result.as_ref().err() == Some(&ErrorCode::Unsupported) {
                    sessions.set_resources(false);
                }
                let listed = |id: &SessionId| sessions.lists(id);
                resources.read_done(result, &machine, &listed, now);
                follow_resources(&refresher, &mut resources, &sessions, work, now);
            }
            Message::ResourcesDone(completion) => {
                let current = refresher.is_current(completion.revision);
                let outcome = completion.outcome.and_then(|result| match result {
                    FeedResult::ResourcesStored(wanted) => Ok(wanted),
                    _ => Err(FeedFailure::Invalid),
                });
                resources.done(current, outcome, now);
                follow_resources(&refresher, &mut resources, &sessions, work, now);
            }
            Message::ReplyDone(mut completion) => {
                let current = refresher.is_current(completion.revision);
                // New sessions asked of this machine come with its replies read.
                if let Ok(FeedResult::Replies(read)) = &mut completion.outcome {
                    new_sessions.read(current, std::mem::take(&mut read.new_sessions));
                }
                let taken = current && machine_taken(&completion.outcome);
                // Someone has rtop open on rakumux.com: send what this machine uses.
                if current
                    && matches!(&completion.outcome, Ok(FeedResult::Replies(read)) if read.resources_wanted)
                {
                    resources.wanted(now);
                }
                feed.merge_replies(replies.request_done(current, completion.outcome, now));
                if taken && renamed_at.is_none_or(|at| now.saturating_sub(at) >= RENAME_EVERY) {
                    renamed_at = Some(now);
                    machine = identity.rename();
                    rename(
                        &machine,
                        &mut feed,
                        &mut chats,
                        &mut replies,
                        &mut sessions,
                        &mut new_sessions,
                        now,
                    );
                }
                follow_replies(
                    &refresher,
                    &mut replies,
                    sessions.confirmed(),
                    uploads,
                    work,
                    now,
                );
                follow_resources(&refresher, &mut resources, &sessions, work, now);
                follow_new_sessions(
                    &refresher,
                    &mut new_sessions,
                    sessions.confirmed(),
                    work,
                    now,
                );
            }
            Message::NewSessionCreated(id, result) => {
                if let Ok(view) = &result {
                    // Followed at once, before its event: its first prompt
                    // (a session message) may be read before that arrives.
                    feed.observe(view);
                    chats.observe(view);
                    replies.observe(view);
                    sessions.observe(view);
                    // On a Sprite, stay up for that prompt.
                    awake.hold(now);
                }
                new_sessions.created(&id, result);
                follow_new_sessions(
                    &refresher,
                    &mut new_sessions,
                    sessions.confirmed(),
                    work,
                    now,
                );
            }
            Message::NewSessionReported(completion) => {
                let current = refresher.is_current(completion.revision);
                let outcome = completion.outcome.and_then(|result| match result {
                    FeedResult::NewSessionReported => Ok(()),
                    _ => Err(FeedFailure::Invalid),
                });
                new_sessions.reported(current, outcome, now);
                follow_new_sessions(
                    &refresher,
                    &mut new_sessions,
                    sessions.confirmed(),
                    work,
                    now,
                );
            }
            Message::Fetched(revision, reply, fetched) => {
                let current = refresher.is_current(revision);
                replies.fetched(current, reply, fetched, now);
                follow_replies(
                    &refresher,
                    &mut replies,
                    sessions.confirmed(),
                    uploads,
                    work,
                    now,
                );
            }
            Message::ReplySubmitted(reply, result) => {
                // Typed in: stay up until the daemon holds the Sprite for the
                // agent's turn.
                if replies.daemon_answered(reply, result, now, wall_ms()) {
                    awake.hold(now);
                }
                follow_replies(
                    &refresher,
                    &mut replies,
                    sessions.confirmed(),
                    uploads,
                    work,
                    now,
                );
            }
            Message::AnswerGiven(reply, result) => {
                // Given: stay up while the agent works on.
                if result.is_ok() {
                    awake.hold(now);
                }
                replies.answer_done(reply, result, now);
                follow_replies(
                    &refresher,
                    &mut replies,
                    sessions.confirmed(),
                    uploads,
                    work,
                    now,
                );
            }
            Message::ReplyWritten(outcome, answer) => {
                if let Ok(reply) = &outcome {
                    replies.written(reply, now);
                    feed.merge_replies(vec![reply.clone()]);
                }
                let _ = answer.send(outcome);
                follow_replies(
                    &refresher,
                    &mut replies,
                    sessions.confirmed(),
                    uploads,
                    work,
                    now,
                );
            }
            Message::MessageWritten(outcome, answer) => {
                if let Ok(message) = &outcome {
                    replies.written_message(message, now);
                }
                let _ = answer.send(outcome.map(|_| ()));
                follow_replies(
                    &refresher,
                    &mut replies,
                    sessions.confirmed(),
                    uploads,
                    work,
                    now,
                );
            }
            Message::Transcript(session, result) => {
                chats.read_done(session, result, now);
                follow_chats(&refresher, &mut chats, replies.watched(), work, now);
            }
            Message::ChatDone(completion) => {
                let current = refresher.is_current(completion.revision);
                let taken = current && machine_taken(&completion.outcome);
                let outcome = completion.outcome.map(|_| ());
                chats.upload_done(current, outcome, now);
                if taken && renamed_at.is_none_or(|at| now.saturating_sub(at) >= RENAME_EVERY) {
                    renamed_at = Some(now);
                    machine = identity.rename();
                    rename(
                        &machine,
                        &mut feed,
                        &mut chats,
                        &mut replies,
                        &mut sessions,
                        &mut new_sessions,
                        now,
                    );
                }
                follow_chats(&refresher, &mut chats, replies.watched(), work, now);
            }
            Message::FeedDone(completion) => {
                let current = refresher.is_current(completion.revision);
                let taken = current && machine_taken(&completion.outcome);
                feed.completed(current, completion.outcome, now);
                if taken && renamed_at.is_none_or(|at| now.saturating_sub(at) >= RENAME_EVERY) {
                    renamed_at = Some(now);
                    machine = identity.rename();
                    rename(
                        &machine,
                        &mut feed,
                        &mut chats,
                        &mut replies,
                        &mut sessions,
                        &mut new_sessions,
                        now,
                    );
                }
            }
            Message::CaptureSet(enabled) => feed.capture_changed(enabled),
            Message::Replies(captured) => {
                for reply in captured {
                    let reply_to = replies.answered(&reply);
                    feed.reply(reply, reply_to);
                }
            }
            Message::Disconnected => refresher.disconnected(),
            Message::Captured(captured) => {
                if let Some(dispatch) = refresher.captured(*captured, now) {
                    let _ = work.send(Work::Dispatch(dispatch));
                }
            }
            Message::Completed(completed) => {
                if let Some(retitle) = refresher.completed(completed, now) {
                    let _ = work.send(Work::Retitle(retitle));
                }
            }
            Message::Retitled(retitled) => refresher.retitled(retitled),
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
            Message::EnrolledSignIn(outcome) => {
                let signed_in = match outcome {
                    Ok(session) => refresher.set_account_session(Some(session)).is_ok(),
                    Err(error) => {
                        enrolled.revoked =
                            matches!(error, AccountError::Provider(AuthError::InvalidGrant));
                        // Static codes only (see `AccountError`).
                        eprintln!(
                            "rakumux-status: signing in with this machine's credential failed: {error:?}"
                        );
                        false
                    }
                };
                enrolled.finished(signed_in, now);
            }
            Message::Renewed(event) => {
                let succeeded = match event {
                    RenewalEvent::Renewed(Some(session)) => {
                        refresher.set_account_session(Some(session)).is_ok()
                    }
                    // The account changed meanwhile: the new one is planned afresh.
                    RenewalEvent::Renewed(None) => true,
                    RenewalEvent::Failed(error) => {
                        // Static codes only (see `AccountError`).
                        eprintln!("rakumux-status: renewing the sign-in failed: {error:?}");
                        false
                    }
                };
                renewal.finished(succeeded, SystemTime::now());
            }
            Message::Command(Command::Feed(after, generation, shown, reply)) => {
                let _ = reply.send(feed.page(after, generation, shown, now));
                follow_feed(&refresher, &mut feed, work, now);
            }
            Message::Command(Command::FeedReply(entry, text, id, answer)) => {
                // The TUI's id, sent again with a retry: rakumux.com answers
                // a repeated id with the reply it stored, so it's typed once.
                let id = id.map_or_else(ReplyId::generate, ReplyId::from_string);
                let upload = crate::feed::is_entry_id(&entry)
                    .then(|| crate::feed::reply_upload(id.as_str(), &entry, &text, &machine))
                    .flatten();
                match upload.map(|request| refresher.feed_job(request)) {
                    Some(Some(job)) => {
                        let _ = work.send(Work::WriteReply(job, answer));
                    }
                    Some(None) => {
                        let _ = answer.send(Err(FeedFailure::Unauthorized));
                    }
                    None => {
                        let _ = answer.send(Err(FeedFailure::Invalid));
                    }
                }
            }
            Message::Command(Command::SessionMessage(target, session, text, id, answer)) => {
                let id = id.map_or_else(ReplyId::generate, ReplyId::from_string);
                let upload = crate::feed::session_message_upload(
                    id.as_str(),
                    &target,
                    &session,
                    &text,
                    &machine,
                );
                match upload.map(|request| refresher.feed_job(request)) {
                    Some(Some(job)) => {
                        let _ = work.send(Work::WriteMessage(job, answer));
                    }
                    Some(None) => {
                        let _ = answer.send(Err(FeedFailure::Unauthorized));
                    }
                    None => {
                        let _ = answer.send(Err(FeedFailure::Invalid));
                    }
                }
            }
            Message::Command(Command::MachineEnrol(machine_id, name, wake_url, answer)) => {
                let account = refresher.account().map(|session| session.account().clone());
                let body = crate::enrolment::enrol_body(&machine_id, &name, wake_url.as_deref());
                match (account, body) {
                    (Some(account), Some(body)) => {
                        match refresher.feed_job(FeedRequest::MachineEnrol { body }) {
                            Some(job) => {
                                let _ = work.send(Work::Enrol(job, machine_id, account, answer));
                            }
                            None => {
                                let _ = answer.send(Err(FeedFailure::Unauthorized));
                            }
                        }
                    }
                    (None, _) => {
                        let _ = answer.send(Err(FeedFailure::Unauthorized));
                    }
                    (Some(_), None) => {
                        let _ = answer.send(Err(FeedFailure::Invalid));
                    }
                }
            }
            Message::Command(Command::MachineRevoke(machine, answer)) => {
                if !crate::feed::is_entry_id(&machine) {
                    let _ = answer.send(Err(FeedFailure::Invalid));
                } else {
                    match refresher.feed_job(FeedRequest::MachineRevoke { machine }) {
                        Some(job) => {
                            let _ = work.send(Work::Revoke(job, answer));
                        }
                        None => {
                            let _ = answer.send(Err(FeedFailure::Unauthorized));
                        }
                    }
                }
            }
            Message::Command(Command::FeedTurn(entry, after, reply)) => {
                match refresher.feed_job(FeedRequest::Turn { entry, after }) {
                    Some(job) => {
                        let _ = work.send(Work::Turn(job, reply));
                    }
                    None => {
                        let _ = reply.send(Err(FeedFailure::Unauthorized));
                    }
                }
            }
            Message::Command(command) => control(
                &mut refresher,
                command,
                now,
                &mut sign_in,
                &mut renewal,
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
    renewal: &mut Renewal,
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
        Command::Renew(reply) => {
            renewal.request();
            let _ = reply.send(());
        }
        // Answered by the owner loop, which holds the feed.
        Command::Feed(..)
        | Command::FeedTurn(..)
        | Command::FeedReply(..)
        | Command::SessionMessage(..)
        | Command::MachineEnrol(..)
        | Command::MachineRevoke(..) => {}
        Command::Summarize(session, reply) => {
            refresher.request(&session);
            let _ = reply.send(());
        }
        Command::SetFeatures(change, reply) => {
            let _ = reply.send(refresher.change_features(change, now));
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

/// Keep the feed in step with the account: tell the daemon whether to capture,
/// catch up on what it kept once the feed turns on, and start the request due.
fn follow_feed<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    feed: &mut FeedSync,
    work: &Workers<A, C>,
    now: Duration,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    if feed.set_account(refresher.account().is_some(), refresher.feed_switch(), now) {
        // Replies an earlier service had the daemon capture.
        let _ = work.send(Work::Replies(None));
    }
    if let Some(enabled) = feed.capture_change() {
        let _ = work.send(Work::ReplyCapture(enabled));
    }
    if let Some(request) = feed.next_request(now) {
        match refresher.feed_job(request) {
            Some(job) => {
                let _ = work.send(Work::Feed(job));
            }
            None => feed.not_started(now),
        }
    }
}

/// Ask for a realtime token when one is due: only while signed in with the
/// account's feed on and this machine's name confirmed. Turned off,
/// the running connection stops and replies are read at rakumux.com's pace.
#[allow(clippy::too_many_arguments)]
fn follow_pokes<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    pokes: &mut PokeSync,
    poke_stop: &mut Option<Arc<AtomicBool>>,
    replies: &mut ReplySync,
    confirmed: bool,
    work: &Workers<A, C>,
    now: Duration,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let enabled =
        refresher.account().is_some() && refresher.feed_switch() == Some(true) && confirmed;
    if pokes.set_enabled(enabled, now) {
        if let Some(stop) = poke_stop.take() {
            stop.store(true, Ordering::SeqCst);
        }
        replies.set_pushed(false, now);
    }
    if pokes.next_request(now) {
        match refresher.feed_job(FeedRequest::Realtime) {
            Some(job) => {
                let _ = work.send(Work::Realtime(job));
            }
            None => pokes.not_started(now),
        }
    }
}

/// Hold the realtime connection `token` opens on a thread of its own (it
/// blocks for hours), telling the owner what it does; returns its stop.
fn start_listening(
    token: RealtimeToken,
    generation: u64,
    messages: &Sender<Message>,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let (stopped, events) = (stop.clone(), messages.clone());
    let started = spawn("rakumux-status-poke", move || {
        crate::feed::listen(&token, &stopped, |event| {
            events.send(Message::Poke(generation, event)).is_ok()
        });
    });
    if started.is_err() {
        let _ = messages.send(Message::Poke(generation, PokeEvent::Closed));
    }
    stop
}

/// Whether rakumux.com refused a request because another machine of the
/// account holds this one's name.
fn machine_taken<T>(outcome: &Result<T, FeedFailure>) -> bool {
    matches!(outcome, Err(FeedFailure::MachineTaken))
}

/// This machine goes by `machine` in the feed from now on.
fn rename(
    machine: &str,
    feed: &mut FeedSync,
    chats: &mut ChatSync,
    replies: &mut ReplySync,
    sessions: &mut SessionSync,
    new_sessions: &mut NewSessionSync,
    now: Duration,
) {
    eprintln!(
        "rakumux-status: another machine of the account uses this name; this one takes another"
    );
    feed.rename(machine.to_owned(), now);
    chats.rename(machine.to_owned(), now);
    replies.rename(machine.to_owned(), now);
    sessions.rename(machine.to_owned(), now);
    new_sessions.rename(machine.to_owned());
}

/// Start the next chat read or upload, when one is due: only while signed in
/// with the account's feed on.
fn follow_chats<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    chats: &mut ChatSync,
    watched: bool,
    work: &Workers<A, C>,
    now: Duration,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let enabled = refresher.account().is_some() && refresher.feed_switch() == Some(true);
    chats.set_enabled(enabled, now);
    chats.set_watched(watched, now);
    match chats.next_work(now) {
        Some(ChatWork::Read {
            session,
            after,
            subagents,
        }) => {
            let _ = work.send(Work::Transcript(session, after, subagents));
        }
        Some(ChatWork::Upload(request)) => match refresher.feed_job(request) {
            Some(job) => {
                let _ = work.send(Work::ChatUpload(job));
            }
            None => chats.upload_done(false, Err(FeedFailure::Unauthorized), now),
        },
        None => {}
    }
}

/// Start what replies need next: reports, fetching files and typing pending
/// ones into their sessions, reads. Only while signed in with the account's
/// feed on and rakumux.com `confirmed` this machine's name (it took a
/// session list under it), but for removing files no longer needed.
fn follow_replies<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    replies: &mut ReplySync,
    confirmed: bool,
    uploads: Option<&Uploads>,
    work: &Workers<A, C>,
    now: Duration,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let enabled =
        refresher.account().is_some() && refresher.feed_switch() == Some(true) && confirmed;
    replies.set_enabled(enabled, now);
    // Typing is local and needn't wait; requests go one at a time.
    for _ in 0..16 {
        match replies.next_work(now) {
            Some(ReplyWork::Request(request)) => match refresher.feed_job(request) {
                Some(job) => {
                    let _ = work.send(Work::ReplyRequest(job));
                }
                None => {
                    replies.request_done(false, Err(FeedFailure::Unauthorized), now);
                    return;
                }
            },
            Some(ReplyWork::Submit {
                reply,
                session,
                text,
                foreground,
            }) => {
                let _ = work.send(Work::Submit(reply, session, text, foreground));
            }
            Some(ReplyWork::Answer {
                reply,
                session,
                call_id,
                answers,
            }) => {
                let _ = work.send(Work::Answer(reply, session, call_id, answers));
            }
            Some(ReplyWork::Fetch(fetch)) => {
                // Nowhere to save files: a reply never goes in without them.
                let Some(uploads) = uploads else {
                    replies.fetched(true, fetch.reply, Fetched::Unsaved, now);
                    continue;
                };
                match refresher.feed_job(fetch.request()) {
                    Some(job) => {
                        let _ = work.send(Work::Fetch(job, fetch, uploads.clone()));
                    }
                    None => {
                        let failed = Fetched::Failed(FeedFailure::Unauthorized);
                        replies.fetched(true, fetch.reply, failed, now);
                        return;
                    }
                }
            }
            Some(ReplyWork::Discard(reply)) => {
                if let Some(uploads) = uploads {
                    let _ = work.send(Work::Discard(uploads.clone(), reply));
                }
            }
            None => return,
        }
    }
}

/// Start what new sessions asked for on rakumux.com need next: start one (or
/// decide it can't be), then report. Only while signed in with the account's
/// feed on and rakumux.com `confirmed` this machine's name, as replies.
fn follow_new_sessions<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    new_sessions: &mut NewSessionSync,
    confirmed: bool,
    work: &Workers<A, C>,
    now: Duration,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let enabled =
        refresher.account().is_some() && refresher.feed_switch() == Some(true) && confirmed;
    new_sessions.set_enabled(enabled, now);
    // One start and one report out at a time: a few rounds at most.
    for _ in 0..4 {
        match new_sessions.next_work(now) {
            Some(NewSessionWork::Create { id, request }) => {
                let _ = work.send(Work::CreateSession(id, request));
            }
            Some(NewSessionWork::Report(request)) => match refresher.feed_job(request) {
                Some(job) => {
                    let _ = work.send(Work::NewSessionReport(job));
                }
                None => {
                    new_sessions.reported(false, Err(FeedFailure::Unauthorized), now);
                    return;
                }
            },
            None => return,
        }
    }
}

/// Upload the session list when it's due. Only while signed in with the
/// account's feed on.
fn follow_sessions<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    sessions: &mut SessionSync,
    work: &Workers<A, C>,
    now: Duration,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let enabled = refresher.account().is_some() && refresher.feed_switch() == Some(true);
    sessions.set_enabled(enabled, now);
    let Some(request) = sessions.next_request(now, wall_ms()) else {
        return;
    };
    match refresher.feed_job(request) {
        Some(job) => {
            let _ = work.send(Work::SessionsUpload(job));
        }
        None => sessions.done(false, Err(FeedFailure::Unauthorized), now),
    }
}

/// Start rtop's next reading or upload, when one is due: only while signed
/// in with the account's feed on and this machine's name confirmed,
/// and someone has rtop open on rakumux.com.
fn follow_resources<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    resources: &mut ResourceSync,
    sessions: &SessionSync,
    work: &Workers<A, C>,
    now: Duration,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let enabled = refresher.account().is_some()
        && refresher.feed_switch() == Some(true)
        && sessions.confirmed();
    resources.set_enabled(enabled, now);
    match resources.next_work(now) {
        Some(ResourceWork::Read) => {
            let _ = work.send(Work::ReadResources);
        }
        Some(ResourceWork::Upload(request)) => match refresher.feed_job(request) {
            Some(job) => {
                let _ = work.send(Work::ResourcesUpload(job));
            }
            None => resources.done(false, Err(FeedFailure::Unauthorized), now),
        },
        None => {}
    }
}

/// The wall clock in Unix milliseconds, as replies' times are.
fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Start renewing the adopted account's access token when it's due.
fn renew<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    renewal: &mut Renewal,
    messages: &Sender<Message>,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let now = SystemTime::now();
    let session = refresher.account();
    renewal.follow(session, now);
    let Some(session) = session.cloned() else {
        return;
    };
    if !renewal.start(now) {
        return;
    }
    let owner = messages.clone();
    let started = renewal::spawn(
        refresher.account_service(),
        session,
        refresher.machine_tokens(),
        move |event| {
            let _ = owner.send(Message::Renewed(event));
        },
    );
    if started.is_err() {
        renewal.finished(false, now);
    }
}

/// An enrolled machine signing in with its credential while it isn't signed
/// in: at the start when rakumux.com couldn't be reached, again with a
/// growing delay; never once its secret was refused (revoked: enrolling it
/// again restarts the service).
#[derive(Default)]
struct EnrolledSignIn {
    running: bool,
    due: Duration,
    failures: u32,
    revoked: bool,
}

impl EnrolledSignIn {
    const FIRST_RETRY: Duration = Duration::from_secs(10);
    const MAX_RETRY: Duration = Duration::from_secs(10 * 60);

    fn finished(&mut self, signed_in: bool, now: Duration) {
        self.running = false;
        if signed_in {
            self.failures = 0;
            return;
        }
        let delay = Self::FIRST_RETRY
            .saturating_mul(2u32.saturating_pow(self.failures))
            .min(Self::MAX_RETRY);
        self.failures = self.failures.saturating_add(1);
        self.due = now + delay;
    }
}

/// Sign an enrolled machine in with its credential when it isn't (see
/// [`EnrolledSignIn`]).
fn sign_in_enrolled<S, A, C>(
    refresher: &StatusRefresher<S, A, C>,
    state: &mut EnrolledSignIn,
    messages: &Sender<Message>,
    now: Duration,
) where
    S: StatusStore,
    A: CredentialStore + 'static,
    C: WallClock + 'static,
{
    let Some(tokens) = refresher.machine_tokens() else {
        return;
    };
    if state.running || state.revoked || refresher.account().is_some() || now < state.due {
        return;
    }
    state.running = true;
    let account = refresher.account_service();
    let owner = messages.clone();
    let started = thread::Builder::new()
        .name("rakumux-status-enrolled".into())
        .spawn(move || {
            let outcome = crate::enrolment::sign_in(&account, &tokens);
            let _ = owner.send(Message::EnrolledSignIn(outcome));
        });
    if started.is_err() {
        state.finished(false, now);
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
            Ok(Work::Retitle(retitle)) => Message::Retitled(retitle.apply(api)),
            Ok(Work::Features(job)) => match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                Ok(completion) => Message::FeaturesRead(completion),
                Err(_) => continue,
            },
            Ok(Work::Feed(job)) => match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                Ok(completion) => Message::FeedDone(completion),
                Err(_) => continue,
            },
            Ok(Work::Transcript(session, after, subagents)) => {
                let bytes = Some(crate::feed::READ_BYTES);
                let result = if subagents {
                    api.transcript_with_subagents(&session, after.as_ref(), bytes)
                } else {
                    api.transcript(&session, after.as_ref(), bytes)
                }
                .map_err(|error| error.code);
                Message::Transcript(session, result)
            }
            Ok(Work::ChatUpload(job)) => {
                let revision = job.revision;
                match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => Message::ChatDone(completion),
                    // Its chat is tried again rather than waiting forever.
                    Err(_) => Message::ChatDone(FeedCompletion {
                        revision,
                        outcome: Err(FeedFailure::Unavailable(None)),
                    }),
                }
            }
            Ok(Work::SessionsUpload(job)) => {
                let revision = job.revision;
                match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => Message::SessionsDone(completion),
                    Err(_) => Message::SessionsDone(FeedCompletion {
                        revision,
                        outcome: Err(FeedFailure::Unavailable(None)),
                    }),
                }
            }
            Ok(Work::ReadResources) => {
                Message::Resources(api.resources().map_err(|error| error.code))
            }
            Ok(Work::ResourcesUpload(job)) => {
                let revision = job.revision;
                match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => Message::ResourcesDone(completion),
                    Err(_) => Message::ResourcesDone(FeedCompletion {
                        revision,
                        outcome: Err(FeedFailure::Unavailable(None)),
                    }),
                }
            }
            Ok(Work::Realtime(job)) => {
                let revision = job.revision;
                match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => Message::RealtimeDone(completion),
                    Err(_) => Message::RealtimeDone(FeedCompletion {
                        revision,
                        outcome: Err(FeedFailure::Unavailable(None)),
                    }),
                }
            }
            Ok(Work::ReplyRequest(job)) => {
                let revision = job.revision;
                match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => Message::ReplyDone(completion),
                    Err(_) => Message::ReplyDone(FeedCompletion {
                        revision,
                        outcome: Err(FeedFailure::Unavailable(None)),
                    }),
                }
            }
            Ok(Work::Submit(reply, session, text, foreground)) => {
                // Never while its agent asks something: the Enter would
                // answer it (a daemon without the check submits anyway, so
                // replies go into an agent at work only when it has it:
                // `ReplySync::connected`).
                let result = api
                    .submit_unless_blocked(
                        &session,
                        &text,
                        crate::feed::TYPING_QUIET,
                        foreground.as_deref(),
                    )
                    .map_err(|error| error.code);
                Message::ReplySubmitted(reply, result)
            }
            Ok(Work::Answer(reply, session, call_id, answers)) => {
                // Only while nobody typed there lately: the daemon checks the
                // question is open and shows before any key.
                let result = api
                    .answer(&session, &call_id, &answers, crate::feed::TYPING_QUIET)
                    .map_err(|error| error.code);
                Message::AnswerGiven(reply, result)
            }
            Ok(Work::Fetch(job, fetch, uploads)) => {
                let revision = job.revision;
                let fetched = panic::catch_unwind(AssertUnwindSafe(|| {
                    crate::feed::fetch(job, &fetch, &uploads)
                }))
                // Tried again rather than waiting forever.
                .unwrap_or(Fetched::Failed(FeedFailure::Unavailable(None)));
                Message::Fetched(revision, fetch.reply, fetched)
            }
            Ok(Work::Discard(uploads, reply)) => {
                uploads.discard(&reply);
                continue;
            }
            Ok(Work::CleanUploads(uploads)) => {
                uploads.clean(SystemTime::now());
                continue;
            }
            Ok(Work::WriteReply(job, answer)) => {
                let outcome = match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => match completion.outcome {
                        Ok(FeedResult::Reply(reply)) => Ok(reply),
                        Ok(_) => Err(FeedFailure::Invalid),
                        Err(failure) => Err(failure),
                    },
                    Err(_) => Err(FeedFailure::Unavailable(None)),
                };
                Message::ReplyWritten(outcome, answer)
            }
            Ok(Work::WriteMessage(job, answer)) => {
                let outcome = match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => match completion.outcome {
                        Ok(FeedResult::Message(message)) => Ok(message),
                        // A reply to an entry, for a session message: not the contract.
                        Ok(_) => Err(FeedFailure::Invalid),
                        Err(failure) => Err(failure),
                    },
                    Err(_) => Err(FeedFailure::Unavailable(None)),
                };
                Message::MessageWritten(outcome, answer)
            }
            Ok(Work::Enrol(job, machine, account, answer)) => {
                let outcome = match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => match completion.outcome {
                        Ok(FeedResult::Enrolled(secret)) => {
                            crate::enrolment::MachineCredential::new(&machine, &secret, &account)
                                .map(|credential| credential.to_json())
                                .map_err(|_| FeedFailure::Invalid)
                        }
                        Ok(_) => Err(FeedFailure::Invalid),
                        Err(failure) => Err(failure),
                    },
                    Err(_) => Err(FeedFailure::Unavailable(None)),
                };
                let _ = answer.send(outcome);
                continue;
            }
            Ok(Work::Revoke(job, answer)) => {
                let outcome = match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => match completion.outcome {
                        Ok(FeedResult::Revoked) => Ok(()),
                        Ok(_) => Err(FeedFailure::Invalid),
                        Err(failure) => Err(failure),
                    },
                    Err(_) => Err(FeedFailure::Unavailable(None)),
                };
                let _ = answer.send(outcome);
                continue;
            }
            Ok(Work::Turn(job, reply)) => {
                let outcome = match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => match completion.outcome {
                        Ok(FeedResult::Turn(turn)) => Ok(turn),
                        Ok(_) => Err(FeedFailure::Invalid),
                        Err(failure) => Err(failure),
                    },
                    Err(_) => Err(FeedFailure::Unavailable(None)),
                };
                let _ = reply.send(outcome);
                continue;
            }
            Ok(Work::ReplyCapture(enabled)) => {
                // An older daemon refuses; it isn't asked again until it reconnects.
                let _ = api.set_reply_capture(enabled);
                Message::CaptureSet(enabled)
            }
            Ok(Work::Replies(after)) => {
                let replies = api
                    .replies(after.as_ref())
                    .map(|(replies, _)| replies)
                    .unwrap_or_default();
                Message::Replies(replies)
            }
            Ok(Work::CreateSession(id, request)) => {
                let result = api.create(request).map_err(|error| error.code);
                Message::NewSessionCreated(id, result)
            }
            Ok(Work::NewSessionReport(job)) => {
                let revision = job.revision;
                match panic::catch_unwind(AssertUnwindSafe(|| job.run())) {
                    Ok(completion) => Message::NewSessionReported(completion),
                    Err(_) => Message::NewSessionReported(FeedCompletion {
                        revision,
                        outcome: Err(FeedFailure::Unavailable(None)),
                    }),
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
        // Replies arrive only while the feed has the daemon capture them.
        let request = SubscribeRequest {
            all: true,
            replies: true,
            ..Default::default()
        };
        if let Ok(stream) = api.subscribe(request) {
            // Dead sessions too, for what's kept of sessions across restarts;
            // the rest of the service follows the others.
            if let Ok(views) = api.list_all(true) {
                // Read again at each subscribe: the daemon may have been
                // restarted on another build since.
                let daemon = DaemonFeatures::of(api.daemon_capabilities().as_deref());
                if owner.send(Message::Resync(views, daemon)).is_err() {
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

#[cfg(test)]
mod tests;
