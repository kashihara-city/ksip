//! The phone's one updater. Commands from the window and the desktop, what
//! the engine link reports, and what finished work brings back all arrive
//! on one queue and are handled here, one at a time, in order. The actor
//! owns the call bookkeeping (`PhoneState`), the engine link and the
//! snapshot the window sees; nothing outside this module writes to any of
//! them. The window reads a published copy of the snapshot, and everyone
//! else sends a `Command` through a `PhoneHandle`.
//!
//! The actor never waits. An operation on the phone (an action, a connect,
//! a settings change...) runs as a *flow*: a future that lives on the
//! actor's thread and is resumed each time the thing it awaits arrives on
//! the queue, an answer from the engine or the report of a worker. One
//! flow runs at a time, the others wait in order behind it, so the phone
//! is never worked on from two sides; but between two steps of a flow the
//! actor is free, and what needs no flow (the engine's events and log
//! lines, the window's questions, the app leaving) is handled at once.
//! Starting and stopping the engine process, reading the audio devices,
//! asking the network for the adapter's address, the echo calibration and
//! the writing of the history run on threads of their own. While the phone
//! is being worked on (a restart, a settings change, a calibration) it is
//! in maintenance: the engine refuses calls meanwhile.
use crate::app::Services;
use crate::audio::{Calibration, Volume};
use crate::engine_config::endpoint_notes;
use crate::engine_link::{AudioState, EngineLink, EngineReport};
use crate::logs::{stamp, LOG_APP, LOG_ENGINE, LOG_EVENT};
use crate::message::{message, message_with};
use crate::phone_message::{Command, LinkBody, LinkMessage, Message, Ready, Reply, StopOutcome, Work};
use crate::phone_state::{automatic_recording_target, CallEndpoint, MaintenanceRefused, Mwi, PhoneState, Snapshot, Transfer};
use crate::recordings::recording_name;
use crate::settings::{connect_prerequisites, dial_target, validate, validate_buttons, CustomButton, Settings, SAVE_MARK};
use crate::storage::Account;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the engine is asked for its state while it runs.
const POLL: Duration = Duration::from_millis(500);
/// How long an answer from the engine is waited for.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(8);
/// How long an operation waits for its turn before it is dropped. A restart
/// takes seconds at most; a calibration under a minute.
const WAIT_TIMEOUT: Duration = Duration::from_secs(90);

/// The way in: sends commands to the actor. Cheap to clone.
#[derive(Clone)]
pub struct PhoneHandle(Sender<Message>);
impl PhoneHandle {
    /// A command that wants no answer.
    pub fn send(&self, command: Command) {
        let _ = self.0.send(Message::Command(command));
    }
    /// A command with an answer, waited for. An actor that is gone answers
    /// APP_CLOSING.
    pub fn call<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply, rx) = Reply::channel();
        self.send(command(reply));
        rx.recv().unwrap_or_else(|_| Err(message("APP_CLOSING")))
    }
}

/// The snapshot as the actor last published it: read by anyone, written
/// only from this module.
#[derive(Clone)]
pub struct Published(Arc<Mutex<Snapshot>>);
impl Published {
    pub fn read(&self) -> Snapshot {
        self.0.lock().unwrap().clone()
    }
    /// A look at the snapshot without copying it.
    pub fn with<R>(&self, look: impl FnOnce(&Snapshot) -> R) -> R {
        look(&self.0.lock().unwrap())
    }
    fn set(&self, view: &Snapshot) {
        *self.0.lock().unwrap() = view.clone();
    }
}

/// Starts the actor on its own thread with the snapshot the app opened with.
pub fn spawn(services: Services, initial: Snapshot) -> (PhoneHandle, Published) {
    let (tx, rx) = mpsc::channel();
    let published = Published(Arc::new(Mutex::new(initial.clone())));
    let (shared_tx, shared_published) = (tx.clone(), published.clone());
    thread::Builder::new()
        .name("phone".into())
        .spawn(move || Actor::new(services, initial, rx, shared_tx, shared_published).run())
        .expect("the phone thread starts");
    (PhoneHandle(tx), published)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The phone, shared between the actor and the flow that runs. Borrowed
/// briefly and never across an await.
type Shared = Rc<RefCell<Phone>>;
/// An operation in progress, resumed by the actor when what it awaits arrives.
type Flow = Pin<Box<dyn Future<Output = ()>>>;

/// What the running flow waits for.
enum Waiting {
    Response { token: String, generation: u64, deadline: Instant },
    /// A worker's report, expected within its deadline; the number tells the
    /// report of a worker that was given up on from the one waited for.
    Work { serial: u64, deadline: Instant },
}
/// Why a request to the engine brought no usable answer. The kind matters
/// after the fact: a remote error is an answer (the engine is there and said
/// no), while a timeout or a lost link is not one (the engine may have
/// carried the command out, or may still be at it), and a flow that acts
/// on what the engine holds (a recording, the maintenance) must not take
/// either for a "no".
#[derive(Debug, Clone, PartialEq)]
enum RequestError {
    /// No engine is held.
    NotRunning,
    /// The command could not be written to the link.
    Send(String),
    /// The link was lost while the answer was awaited.
    Disconnected,
    /// The engine did not answer within RESPONSE_TIMEOUT.
    Timeout,
    /// The engine answered that the command failed, in its own words.
    Remote { command: String, detail: String },
}
impl RequestError {
    /// Whether the engine's side of the command is unknown: it may have been
    /// carried out, or may still be under way.
    fn unknown(&self) -> bool {
        matches!(self, Self::Disconnected | Self::Timeout)
    }
    /// The engine refused because a call is up.
    fn busy(&self) -> bool {
        matches!(self, Self::Remote { detail, .. } if detail.contains("EBUSY") || detail.to_ascii_lowercase().contains("busy"))
    }
}
impl From<RequestError> for String {
    fn from(e: RequestError) -> String {
        match e {
            RequestError::NotRunning => message("ENGINE_NOT_RUNNING"),
            RequestError::Send(e) => e,
            RequestError::Disconnected => message("ENGINE_DISCONNECTED"),
            RequestError::Timeout => message("ENGINE_RESPONSE_TIMEOUT"),
            RequestError::Remote { command, detail } => message_with("ENGINE_COMMAND_FAILED", [command, detail]),
        }
    }
}
impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from(self.clone()))
    }
}
/// A recording start out with the engine, and what has become of it
/// meanwhile: an engine's word about the file (failed, closed) that came
/// before the start's answer is the word that counts.
struct PendingStart {
    path: PathBuf,
    /// The engine the start went to.
    generation: u64,
    ended: Option<PendingEnd>,
}
enum PendingEnd {
    /// The file could not be opened: there is no recording.
    Failed,
    /// The file was opened and has already been closed.
    Closed { complete: bool },
}
/// What arrived for it.
enum Delivered {
    Response(Result<(u64, Value), RequestError>),
    Work(Work),
}
/// Resolves once the actor has put what the flow waits for into `delivered`.
struct Await(Shared);
impl Future for Await {
    type Output = Delivered;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Delivered> {
        match self.0.borrow_mut().delivered.take() {
            Some(delivered) => Poll::Ready(delivered),
            None => Poll::Pending,
        }
    }
}

/// An operation waiting for its turn.
enum Job {
    Poll,
    Initialize,
    Connect(Reply<()>),
    Action {
        name: String,
        id: String,
        value: String,
        line: u8,
        reply: Reply<String>,
    },
    Save {
        settings: Box<Settings>,
        account: Account,
        reply: Reply<String>,
    },
    SaveButtons {
        buttons: Vec<CustomButton>,
        reply: Reply<()>,
    },
    /// Settings the engine takes only at a start were saved while a call was
    /// up or ringing: now that none is left, the engine is started on them.
    ApplySaved,
    SelectDevice {
        kind: String,
        device: String,
        reply: Reply<()>,
    },
    RefreshDevices(Reply<()>),
    Calibrate {
        microphone: String,
        speaker: String,
        careful: bool,
        reply: Reply<Calibration>,
    },
    SetVolume {
        kind: String,
        device: String,
        level: Option<u16>,
        mute: Option<bool>,
        reply: Reply<Volume>,
    },
    /// The adapter's address moved: connect again.
    Reconnect,
    /// The control connection was lost: the process is ended before the
    /// engine counts as gone.
    Recover,
    /// The engine did not take the end of maintenance: told again.
    MaintenanceOff,
    /// The app is leaving: the engine goes.
    Stop,
}
impl Job {
    /// Answers a job that will not be carried out.
    fn refuse(self, why: String) {
        match self {
            Job::Connect(reply) | Job::RefreshDevices(reply) => reply.send(Err(why)),
            Job::Save { reply, .. } => reply.send(Err(why)),
            Job::SaveButtons { reply, .. } | Job::SelectDevice { reply, .. } => reply.send(Err(why)),
            Job::Action { reply, .. } => reply.send(Err(why)),
            Job::Calibrate { reply, .. } => reply.send(Err(why)),
            Job::SetVolume { reply, .. } => reply.send(Err(why)),
            Job::Poll | Job::Initialize | Job::Reconnect | Job::ApplySaved | Job::Recover | Job::MaintenanceOff | Job::Stop => {}
        }
    }
    /// Whether the job may be dropped for waiting too long.
    fn expires(&self) -> bool {
        !matches!(self, Job::Poll | Job::Stop | Job::Initialize | Job::Recover | Job::MaintenanceOff | Job::ApplySaved)
    }
    /// Whether the job still has to run when the app is leaving.
    fn survives_closing(&self) -> bool {
        matches!(self, Job::Recover | Job::Stop)
    }
}

/// The phone as the actor holds it.
struct Phone {
    /// For the link and the workers, which report back on the queue.
    tx: Sender<Message>,
    services: Services,
    /// The calls and the rules that move them.
    phone: PhoneState,
    /// What the window sees; published after every change.
    view: Snapshot,
    published: Published,
    /// The engine, while one runs.
    link: Option<EngineLink>,
    /// The generation of an engine being started on a worker, until the
    /// worker reports; its first words are already this phone's.
    starting: Option<u64>,
    /// The engine being started lost its connection before it was taken.
    starting_lost: bool,
    /// An engine whose control connection was lost, until the recovery has
    /// ended its process: the engine is not gone while this is held.
    lost: Option<EngineLink>,
    /// The engine refused or did not answer the end of maintenance: told
    /// again at the next look, so that it does not go on refusing calls.
    maintenance_off_owed: bool,
    /// Settings the engine reads only at a start were saved while a call was
    /// up or ringing: the engine is started on them once no call is left
    /// (Job::ApplySaved). Any start takes them, and clears this.
    reconnect_owed: bool,
    /// A save's settings did not all reach the running engine (a command
    /// refused or unanswered; unanswered, it may have run or not): the error
    /// the window was shown. The next save tells the engine everything it
    /// takes while running again, changed or not, the commands being the
    /// same whether said once or twice; that going through clears this, and
    /// the error with it. A new engine reads them all when it starts.
    live_owed: Option<String>,
    /// The WAV the engine has open for the recording, once it has said so
    /// (a start answered "started", or the "active" event of a reservation
    /// that came true). Converted to an MP3 exactly once, when the file is
    /// known to be closed: by the engine's answer to the stop, by its
    /// "closed" event, or by the confirmed end of the process. An answer
    /// that never came confirms nothing, and the file waits.
    open_recording: Option<PathBuf>,
    /// The call whose recording the engine could not open: the automatic
    /// recording does not try the same call again and again.
    recording_refused: Option<String>,
    /// The WAVs handed over for conversion this run. A recording's end can be
    /// learnt more than once (the engine's closed event, a stop's answer, a
    /// start's continuation, the process's end); the file is converted on the
    /// first and never again (convert_once).
    converted: std::collections::HashSet<PathBuf>,
    /// The engine was lost and its end has not been confirmed: the calls
    /// stay as the window shows them until a stop reports the process ended
    /// (recover, or a late report in stray), since it may still be there.
    exit_unsettled: bool,
    /// When a lost engine may be given another stop: at once after the loss,
    /// a while after a stop that could not confirm its end.
    recover_after: Instant,
    /// The maintenance is kept for a calibration worker that outlived its
    /// wait: it still has the devices, and the maintenance (this operation's,
    /// by number) ends with its report (see stray). Nothing else can end it
    /// meanwhile, so a connect or a settings change in between is refused.
    maintenance_held: Option<u64>,
    /// A recording whose start has been sent and not answered yet: the
    /// engine's events about its file are matched to it and what they say
    /// is kept for the start's continuation, and the engine it was sent to
    /// is remembered, so that an answer that never came does not revive a
    /// recording of an engine that has since ended.
    recording_pending: Option<PendingStart>,
    /// The address the engine bound, to notice when the adapter moves.
    bound: String,
    /// A look at the adapter's address is under way.
    probing: bool,
    /// The number of the last work started; each report carries its own.
    work_serial: u64,
    /// The exclusive workers (an engine start, an engine stop, a calibration)
    /// whose report has not come: they hold the engine's ports or the audio
    /// devices. With a lost engine not yet confirmed ended (`lost`), they are
    /// what engine_busy() counts: no start or calibration begins while any
    /// is outstanding, however long ago its wait was given up. A stop is
    /// never refused. A worker leaves the ledger with its report; what the
    /// report hands back (an engine to stop, a link to try again) goes
    /// straight onto it or into `lost`, in the same step.
    exclusive: Vec<u64>,
    /// The banner the polling raised, cleared once polling works again.
    polling_error: String,
    /// What the current engine's reports have been said about.
    notices: EngineNotices,
    closing: bool,
    waiting: Option<Waiting>,
    delivered: Option<Delivered>,
}

struct Actor {
    rx: Receiver<Message>,
    shared: Shared,
    /// The flow that runs, if one does.
    current: Option<Flow>,
    /// The operations waiting for their turn, with when they arrived: run in
    /// order, dropped after WAIT_TIMEOUT, answered APP_CLOSING if the app
    /// leaves first.
    queue: VecDeque<(Instant, Job)>,
    /// The shutdown waiting for the engine to be gone.
    shutdown: Option<Reply<()>>,
    /// Until when the shutdown waits for workers still holding the engine's
    /// resources; after it the app leaves and says so.
    shutdown_deadline: Option<Instant>,
    next_poll: Instant,
}
/// What an engine's state reports have been said about. A report repeats how
/// things stand every 300 ms; each thing is said once for each engine, and a
/// failed device start once for each failure however many reports carry it.
#[derive(Default)]
struct EngineNotices {
    audio_down: bool,
    no_trust: bool,
    /// The speaker's failed starts already said, by the speaker's own count.
    speaker_failures: u64,
}
impl EngineNotices {
    /// What a report has to say that was not said: the audio module is not up,
    /// the speaker would not start (a microphone that would not start is the
    /// silence notice, not a banner), the trust store for TLS is empty.
    fn news(&mut self, audio: Option<&AudioState>, trust: Option<u64>) -> Vec<String> {
        let mut said = Vec::new();
        if let Some(audio) = audio {
            if !audio.ready && !self.audio_down {
                self.audio_down = true;
                said.push(message("AUDIO_DEVICE_INIT_FAILED"));
            }
            if audio.speaker.failures > self.speaker_failures {
                self.speaker_failures = audio.speaker.failures;
                let result = audio.speaker.last_result.map(|r| r.to_string()).unwrap_or_default();
                said.push(message_with("AUDIO_SPEAKER_START_FAILED", [result]));
            }
        }
        // baresip only warns when it cannot load the trust list, and then
        // fails every TLS registration with nothing else to show for it.
        if trust == Some(0) && !self.no_trust {
            self.no_trust = true;
            said.push(message("TRUST_STORE_UNUSABLE"));
        }
        said
    }
}
/// How long a lost engine waits for another stop after one that could not
/// confirm its end.
const RECOVER_RETRY: Duration = Duration::from_secs(5);
/// How long the shutdown waits for unrecovered workers and lost engines.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

impl Actor {
    fn new(services: Services, view: Snapshot, rx: Receiver<Message>, tx: Sender<Message>, published: Published) -> Self {
        let phone = Phone {
            tx,
            services,
            phone: PhoneState::default(),
            view,
            published,
            link: None,
            starting: None,
            starting_lost: false,
            lost: None,
            maintenance_off_owed: false,
            reconnect_owed: false,
            live_owed: None,
            notices: EngineNotices::default(),
            open_recording: None,
            recording_refused: None,
            converted: std::collections::HashSet::new(),
            exit_unsettled: false,
            recover_after: Instant::now(),
            maintenance_held: None,
            recording_pending: None,
            bound: String::new(),
            probing: false,
            work_serial: 0,
            exclusive: Vec::new(),
            polling_error: String::new(),
            closing: false,
            waiting: None,
            delivered: None,
        };
        Self {
            rx,
            shared: Rc::new(RefCell::new(phone)),
            current: None,
            queue: VecDeque::new(),
            shutdown: None,
            shutdown_deadline: None,
            next_poll: Instant::now() + POLL,
        }
    }
    fn run(mut self) {
        loop {
            let now = Instant::now();
            if now >= self.next_poll {
                self.tick();
                self.next_poll = Instant::now() + POLL;
            } else {
                match self.rx.recv_timeout(self.next_poll - now) {
                    Ok(message) => self.handle(message),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            self.drive();
            self.shared.borrow().publish();
            if self.finish_shutdown() {
                break;
            }
        }
        for (_, job) in self.queue.drain(..) {
            job.refuse(message("APP_CLOSING"));
        }
        while let Ok(Message::Command(command)) = self.rx.try_recv() {
            Self::refuse(command, message("APP_CLOSING"));
        }
    }
    /// Runs the flow in progress as far as it gets, and starts the next
    /// waiting job once it is done.
    fn drive(&mut self) {
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Some(flow) = self.current.as_mut() {
                if flow.as_mut().poll(&mut cx).is_pending() {
                    return;
                }
                self.current = None;
            }
            let Some((_, job)) = self.queue.pop_front() else {
                return;
            };
            self.current = Some(self.flow(job));
        }
    }
    /// The shutdown is answered once nothing of the engine is left; true when
    /// the actor is done.
    /// The shutdown is over when the flows are done and the engine is gone.
    /// Workers still holding the engine's ports or the devices, and a lost
    /// engine not yet confirmed ended, are waited for up to SHUTDOWN_GRACE;
    /// then the app leaves and writes down what it left: the engine watches
    /// for the app's end and quits on its own, which is the fallback, not
    /// the recovery.
    fn finish_shutdown(&mut self) -> bool {
        let (settled, unrecovered) = {
            let p = self.shared.borrow();
            (
                p.closing && p.link.is_none() && p.starting.is_none() && self.current.is_none() && self.queue.is_empty(),
                p.exclusive.len() + usize::from(p.lost.is_some()),
            )
        };
        if !settled {
            return false;
        }
        if unrecovered > 0 {
            match self.shutdown_deadline {
                Some(deadline) if Instant::now() >= deadline => {
                    self.shared.borrow().services.log(
                        LOG_APP,
                        format!("ksip: leaving with {unrecovered} engine worker(s) or engine(s) unrecovered; the engine quits on its own once the app is gone"),
                    );
                }
                _ => return false,
            }
        }
        if let Some(reply) = self.shutdown.take() {
            self.shared.borrow().answer(reply, Ok(()));
        }
        true
    }
    fn handle(&mut self, message: Message) {
        match message {
            Message::Command(command) => self.handle_command(command),
            Message::Link(link) => {
                self.shared.borrow_mut().handle_link(link);
                if self.shared.borrow().needs_recovery() && !self.queue.iter().any(|(_, job)| matches!(job, Job::Recover)) {
                    self.queue.push_front((Instant::now(), Job::Recover));
                }
            }
            Message::Work(_, Work::Address { adapter, address }) => {
                let reconnect = self.shared.borrow_mut().follow_network(&adapter, address);
                if reconnect && !self.queue.iter().any(|(_, job)| matches!(job, Job::Reconnect)) {
                    self.queue.push_back((Instant::now(), Job::Reconnect));
                }
            }
            Message::Work(serial, work) => {
                let mut p = self.shared.borrow_mut();
                // The worker is done, whatever it was doing: off the ledger.
                p.exclusive.retain(|s| *s != serial);
                // Only the report the flow waits for; one from a worker that
                // was given up on, arriving late, is not the answer to the
                // next wait.
                if matches!(p.waiting, Some(Waiting::Work { serial: wanted, .. }) if wanted == serial) && p.delivered.is_none() {
                    p.delivered = Some(Delivered::Work(work));
                } else {
                    p.stray(work);
                }
            }
        }
        // A reconnect owed to a save made during a call is queued once the
        // last call has gone, whatever message said so.
        if self.shared.borrow().reconnect_due() && !self.queue.iter().any(|(_, job)| matches!(job, Job::ApplySaved)) {
            self.queue.push_back((Instant::now(), Job::ApplySaved));
        }
    }
    /// Answers a command that will not be carried out.
    fn refuse(command: Command, why: String) {
        match command {
            Command::Connect(reply) | Command::RefreshDevices(reply) | Command::Shutdown(reply) => reply.send(Err(why)),
            Command::SaveConfiguration { reply, .. } => reply.send(Err(why)),
            Command::SaveButtons { reply, .. } | Command::SelectAudioDevice { reply, .. } => reply.send(Err(why)),
            Command::Action { reply, .. } => reply.send(Err(why)),
            Command::CalibrateAec { reply, .. } => reply.send(Err(why)),
            Command::SetVolume { reply, .. } => reply.send(Err(why)),
            Command::Initialize | Command::WindowVisible(_) | Command::ShowError(_) | Command::ReportError(_) => {}
        }
    }
    /// What needs no flow is done here and now; the rest waits its turn.
    fn handle_command(&mut self, command: Command) {
        if self.shared.borrow().closing {
            Self::refuse(command, message("APP_CLOSING"));
            return;
        }
        let job = match command {
            Command::WindowVisible(visible) => {
                self.shared.borrow_mut().view.window_visible = visible;
                return;
            }
            Command::ShowError(error) => return self.shared.borrow_mut().show_error(error),
            Command::ReportError(error) => return self.shared.borrow_mut().report_error(error),
            Command::Shutdown(reply) => {
                self.shared.borrow_mut().closing = true;
                self.shutdown = Some(reply);
                self.shutdown_deadline = Some(Instant::now() + SHUTDOWN_GRACE);
                let mut kept = VecDeque::new();
                for (since, job) in self.queue.drain(..) {
                    if job.survives_closing() {
                        kept.push_back((since, job));
                    } else {
                        job.refuse(message("APP_CLOSING"));
                    }
                }
                self.queue = kept;
                // The flow that runs finishes first (a start reports and its
                // engine is stopped); then the engine goes.
                Job::Stop
            }
            Command::Initialize => Job::Initialize,
            Command::Connect(reply) => Job::Connect(reply),
            Command::Action { name, id, value, line, reply } => Job::Action { name, id, value, line, reply },
            Command::SaveConfiguration { settings, account, reply } => Job::Save { settings, account, reply },
            Command::SaveButtons { buttons, reply } => Job::SaveButtons { buttons, reply },
            Command::SelectAudioDevice { kind, device, reply } => Job::SelectDevice { kind, device, reply },
            Command::RefreshDevices(reply) => Job::RefreshDevices(reply),
            Command::CalibrateAec { microphone, speaker, careful, reply } => Job::Calibrate { microphone, speaker, careful, reply },
            Command::SetVolume { kind, device, level, mute, reply } => Job::SetVolume { kind, device, level, mute, reply },
        };
        self.queue.push_back((Instant::now(), job));
    }
    /// Once every poll interval: an engine that ended without being asked,
    /// an answer that did not come in time, the engine's state, the network
    /// the engine is bound to, and the operations that have waited too long.
    fn tick(&mut self) {
        {
            let mut p = self.shared.borrow_mut();
            if let Some(status) = p.link.as_mut().and_then(EngineLink::exited) {
                // The process ended on its own: this is its confirmed end,
                // the same as a stop's Ok, and everything that follows an
                // end follows here (the recording's file, the calls' rows).
                let engine = p.link.take().expect("the link that exited");
                // The request out with it, if any, gets no answer from it.
                if matches!(&p.waiting, Some(Waiting::Response { generation, .. }) if *generation == engine.generation()) && p.delivered.is_none() {
                    p.delivered = Some(Delivered::Response(Err(RequestError::Disconnected)));
                }
                engine.close();
                let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| status.to_string());
                p.services.log(LOG_APP, format!("ksip: the engine ended on its own, exit code {code}"));
                p.engine_ended();
                p.settle_exit(code);
            }
            match &p.waiting {
                Some(Waiting::Response { deadline, .. }) if Instant::now() >= *deadline && p.delivered.is_none() => {
                    p.delivered = Some(Delivered::Response(Err(RequestError::Timeout)));
                }
                Some(Waiting::Work { deadline, .. }) if Instant::now() >= *deadline && p.delivered.is_none() => {
                    // The worker's report, if it comes later, finds nobody
                    // waiting and goes to stray; its serial stays on the
                    // ledger until then.
                    p.delivered = Some(Delivered::Response(Err(RequestError::Timeout)));
                }
                _ => {}
            }
            p.probe_network();
        }
        if self.shared.borrow().maintenance_off_due() && !self.queue.iter().any(|(_, job)| matches!(job, Job::MaintenanceOff)) {
            self.queue.push_front((Instant::now(), Job::MaintenanceOff));
        }
        // A lost engine whose stop could not confirm its end is given another.
        let retry = {
            let p = self.shared.borrow();
            p.needs_recovery() && Instant::now() >= p.recover_after
        };
        if retry && !self.queue.iter().any(|(_, job)| matches!(job, Job::Recover)) {
            self.queue.push_front((Instant::now(), Job::Recover));
        }
        // One look at the engine per interval, queued behind whatever runs
        // now. A long flow (a calibration, a restart) holds the phone in
        // maintenance, so no call is up meanwhile and the registration events
        // still reach the window at once; the look itself waits its turn.
        if !self.shared.borrow().closing && !self.queue.iter().any(|(_, job)| matches!(job, Job::Poll)) {
            self.queue.push_back((Instant::now(), Job::Poll));
        }
        let now = Instant::now();
        let mut kept = VecDeque::new();
        for (since, job) in self.queue.drain(..) {
            if job.expires() && now.duration_since(since) > WAIT_TIMEOUT {
                self.shared.borrow().services.log(LOG_APP, message("OPERATION_WAIT_TIMEOUT"));
                job.refuse(message("OPERATION_WAIT_TIMEOUT"));
            } else {
                kept.push_back((since, job));
            }
        }
        self.queue = kept;
    }
    /// The flow for a job.
    fn flow(&self, job: Job) -> Flow {
        let s = self.shared.clone();
        match job {
            Job::Poll => Box::pin(async move {
                match poll(&s).await {
                    Err(e) => s.borrow_mut().report_error(e),
                    Ok(()) => s.borrow_mut().clear_polling_error(),
                }
            }),
            Job::Initialize => Box::pin(initialize(s)),
            Job::Connect(reply) => Box::pin(async move {
                let result = connect(&s).await;
                s.borrow().answer(reply, result);
            }),
            Job::Action { name, id, value, line, reply } => Box::pin(async move {
                let result = action(&s, &name, &id, &value, line).await;
                s.borrow().answer(reply, result);
            }),
            Job::Save { settings, account, reply } => Box::pin(save_configuration(s, *settings, account, reply)),
            Job::SaveButtons { buttons, reply } => Box::pin(save_buttons(s, buttons, reply)),
            Job::ApplySaved => Box::pin(async move {
                // A call may have come in since this was queued; then the
                // reconnect stays owed until it too has gone. One the person
                // stopped by unregistering is not made for them either: the
                // saved settings are used at their next connect.
                let go = {
                    let mut p = s.borrow_mut();
                    if !p.reconnect_due() {
                        false
                    } else {
                        p.reconnect_owed = false;
                        !p.view.unregistered_by_choice
                    }
                };
                if go {
                    if let Err(e) = connect(&s).await {
                        s.borrow_mut().report_error(e);
                    }
                }
            }),
            Job::SelectDevice { kind, device, reply } => Box::pin(async move {
                let result = select_audio_device(&s, &kind, device).await;
                s.borrow().answer(reply, result);
            }),
            Job::RefreshDevices(reply) => Box::pin(async move {
                let result = refresh_devices(&s).await;
                s.borrow().answer(reply, result);
            }),
            Job::Calibrate { microphone, speaker, careful, reply } => Box::pin(async move {
                let result = calibrate_aec(&s, microphone, speaker, careful).await;
                s.borrow().answer(reply, result);
            }),
            Job::SetVolume { kind, device, level, mute, reply } => Box::pin(async move {
                let result = set_volume(&s, &kind, &device, level, mute).await;
                s.borrow().answer(reply, result);
            }),
            Job::Reconnect => Box::pin(async move {
                // Queued when the adapter's address moved; by its turn the
                // person may have unregistered, a call may be up, or the
                // adapter may no longer be chosen. Then it is not done.
                let still_wanted = {
                    let p = s.borrow();
                    !p.closing && !p.view.unregistered_by_choice && p.link.is_some() && p.phone.calls().is_empty()
                        && !p.view.settings.network_adapter.trim().is_empty()
                };
                if !still_wanted {
                    return;
                }
                if let Err(e) = connect(&s).await {
                    s.borrow_mut().report_error(e);
                }
            }),
            Job::Recover => Box::pin(recover(s)),
            Job::MaintenanceOff => Box::pin(async move { maintenance_off(&s).await }),
            Job::Stop => Box::pin(async move {
                stop_engine(&s).await;
            }),
        }
    }
}

impl Phone {
    fn publish(&self) {
        self.published.set(&self.view);
    }
    /// Answers a command once the snapshot shows what it did, so that the
    /// window's next look, right after the answer, is not behind it.
    fn answer<T>(&self, reply: Reply<T>, result: Result<T, String>) {
        self.publish();
        reply.send(result);
    }
    /// Hands a worker a job whose result comes back on the queue.
    fn work(&mut self, job: impl FnOnce() -> Work + Send + 'static) -> u64 {
        let tx = self.tx.clone();
        self.work_serial += 1;
        let serial = self.work_serial;
        thread::spawn(move || {
            let _ = tx.send(Message::Work(serial, job()));
        });
        serial
    }
    /// A worker that holds something only one may hold (the engine's ports,
    /// the audio devices): on the ledger until its report comes.
    fn work_exclusive(&mut self, job: impl FnOnce() -> Work + Send + 'static) {
        let serial = self.work(job);
        self.exclusive.push(serial);
    }
    /// Whether the engine's resources are held by something not yet done: an
    /// exclusive worker still out, or a lost engine not yet confirmed ended.
    /// A start or a calibration does not begin then, or two engines would
    /// fight for the ports.
    fn engine_busy(&self) -> bool {
        !self.exclusive.is_empty() || self.lost.is_some()
    }
    /// What the link delivered. Only the engine the actor holds, or the one
    /// it is starting, can change the phone; the words of an earlier one are
    /// still worth their log line. An answer goes to the flow that waits for
    /// it; the loss of the connection ends that wait as well.
    fn handle_link(&mut self, link: LinkMessage) {
        let held = self.link.as_ref().map(EngineLink::generation) == Some(link.generation);
        let starting = self.starting == Some(link.generation);
        match link.body {
            LinkBody::Log(text) => {
                if held || starting {
                    self.alert_failure_from_log(&text);
                }
                self.services.log(LOG_ENGINE, text);
            }
            LinkBody::Event(event) => {
                if held {
                    self.event(&event);
                }
            }
            LinkBody::Response { token, value } => {
                let wanted = matches!(&self.waiting, Some(Waiting::Response { token: t, generation, .. }) if *t == token && *generation == link.generation);
                if wanted && self.delivered.is_none() {
                    self.delivered = Some(Delivered::Response(Ok((link.seq, value))));
                }
                // Otherwise an answer nobody waits for any more: it came after its time ran out.
            }
            LinkBody::Lost => {
                if starting {
                    // Noted; the start reports next and finds its engine gone.
                    self.starting_lost = true;
                    return;
                }
                if !held {
                    return;
                }
                // The connection is gone; the process may not be. Nothing is
                // said about the calls or the recording until the recovery has
                // ended the process (Job::Recover, queued by the actor): an
                // engine still running would otherwise go on with a call the
                // window shows as over, and fight the next start for its ports.
                self.services.log(LOG_APP, message("ENGINE_CONTROL_LOST"));
                self.lost = self.link.take();
                self.exit_unsettled = true;
                self.recover_after = Instant::now();
                self.view.registration = "DISCONNECTED".into();
                if matches!(&self.waiting, Some(Waiting::Response { generation, .. }) if *generation == link.generation) && self.delivered.is_none() {
                    self.delivered = Some(Delivered::Response(Err(RequestError::Disconnected)));
                }
            }
        }
    }
    /// Whether a lost engine waits to be recovered; the actor queues the job.
    fn needs_recovery(&self) -> bool {
        self.lost.is_some()
    }
    /// A report nobody waits for: a worker that outlived its flow.
    /// A worker's report that no flow waits for: one whose wait was given
    /// up, or one that came after the flow moved on. What it says still
    /// counts: a stop that came late is the word the recovery waited for,
    /// and an engine nobody will take is ended.
    fn stray(&mut self, work: Work) {
        match work {
            Work::Started { stopped, result } => {
                if let Some(outcome) = stopped {
                    self.finish_recovery(outcome);
                }
                if let Ok(ready) = result {
                    // An engine nobody will take: stopped on the ledger like
                    // any other, so that no start begins over its ports
                    // before its end is confirmed.
                    self.work_exclusive(move || Work::Stopped(ready.link.stop()));
                }
            }
            Work::Stopped(outcome) => self.finish_recovery(outcome),
            Work::Calibrated(_) => self.release_held_maintenance(),
            _ => {}
        }
    }
    /// An end of maintenance the engine has not taken yet is owed, and nobody
    /// holds the maintenance now: one that is held is its owner's to end.
    fn maintenance_off_due(&self) -> bool {
        self.maintenance_off_owed && !self.phone.in_maintenance()
    }
    /// The calibration that was given up on has ended: the devices are free,
    /// and the maintenance held for it ends now, by its own number.
    fn release_held_maintenance(&mut self) {
        if let Some(owner) = self.maintenance_held.take() {
            if self.phone.end_maintenance(owner) {
                self.maintenance_off_owed = true;
            }
        }
    }
    /// A stop's outcome: taken in (take_stop), and when it confirms the end
    /// of an engine whose loss left the calls unsettled, they are settled.
    fn finish_recovery(&mut self, outcome: StopOutcome) {
        let confirmed = outcome.is_ok();
        let code = exit_code(&outcome);
        self.take_stop(outcome);
        if confirmed && self.exit_unsettled {
            self.exit_unsettled = false;
            self.settle_exit(code);
        }
    }
    /// A lost engine's process has ended: the calls that were up get their
    /// rows, and the window its notice.
    fn settle_exit(&mut self, code: String) {
        let notice = message_with("ENGINE_EXITED", [code]);
        self.close_calls_on_exit(Some(notice.clone()));
        self.services.log(LOG_APP, notice);
    }
    /// The one thing still read from the engine's words. The ring tone plays
    /// through baresip's own wasapi module (audio_alert), which says that
    /// its device would not start only in its log. Everything else about
    /// the audio and the trust store comes in the state report (apply_report).
    /// Only the banner: the log changes no state.
    fn alert_failure_from_log(&mut self, s: &str) -> bool {
        if s.contains("wasapi/play:") && s.contains("failed") {
            self.view.error = message_with("AUDIO_DEVICE_INIT_FAILED_DETAIL", [s]);
            return true;
        }
        false
    }
    fn event(&mut self, e: &Value) {
        let kind = e["type"].as_str().unwrap_or("");
        // Credentials never enter events or command parameters.
        self.services.log(
            LOG_EVENT,
            format!("{} {}", kind, e["param"].as_str().unwrap_or("")),
        );
        if let Some(id) = e["id"].as_str() {
            let param = e["param"].as_str().unwrap_or("");
            match kind {
                "CALL_CLOSED" => self.phone.note_closed(id, param),
                "CALL_OUTGOING" => self.phone.note_announced(id, "HISTORY_OUTGOING", param),
                "CALL_INCOMING" => self.phone.note_announced(id, "HISTORY_INCOMING", param),
                _ => {}
            }
        }
        if matches!(
            kind,
            "REGISTERING" | "REGISTER_OK" | "REGISTER_FAIL" | "UNREGISTERING"
        ) {
            self.view.registration = kind.into();
        }
        if kind == "AUDIO_ERROR" {
            self.view.error = message("AUDIO_DEVICE_INIT_FAILED");
        }
        // The recording session's word (recording_session.h): the moments
        // the commands cannot answer for, and the word that counts when an
        // answer was lost.
        if kind == "MODULE" {
            let param = e["param"].as_str().unwrap_or("");
            if let Some(rest) = param.strip_prefix("ksip_audio_filter,recording,") {
                let (what, detail) = rest.split_once(' ').unwrap_or((rest, ""));
                // The file named is the recording's identity: the one whose
                // start is out (answered or not), or the one shown. A word
                // about any other file is not about this phone's recording.
                // What is said about a start still out is kept for it.
                let (what, path) = match what {
                    // "complete|incomplete <bytes> <dropped> <path>"
                    "closed" => (what, detail.splitn(4, ' ').nth(3).unwrap_or("")),
                    _ => (what, detail),
                };
                let pending = self.recording_pending.as_ref().is_some_and(|x| x.path.to_string_lossy() == path);
                let shown = self.view.recording && self.view.recording_path == path;
                let open = self.open_recording.as_deref().is_some_and(|x| x.to_string_lossy() == path);
                match what {
                    "active" if (pending || shown) && self.open_recording.is_none() => {
                        self.open_recording = Some(PathBuf::from(path));
                    }
                    "failed" if pending || shown => {
                        if let Some(start) = self.recording_pending.as_mut() {
                            start.ended = Some(PendingEnd::Failed);
                        }
                        if shown {
                            self.recording_failed(path);
                        }
                    }
                    "closed" if pending || shown || open => {
                        let complete = detail.starts_with("complete");
                        if let Some(start) = self.recording_pending.as_mut() {
                            start.ended = Some(PendingEnd::Closed { complete });
                        }
                        if shown || open {
                            self.recording_closed(complete);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    /// Takes one state report: which calls ended and what their rows say,
    /// which line each call is on, which incoming calls the automatic answer
    /// takes (returned). A report received before one already applied
    /// changes nothing.
    fn apply_report(&mut self, seq: u64, report: EngineReport) -> Vec<String> {
        let auto_answer = self.view.settings.auto_answer;
        let generation = self.phone.generation();
        let Some(applied) = self.phone.apply_report(generation, seq, report.calls, report.dnd, auto_answer, now_secs()) else {
            return Vec::new();
        };
        let v = &mut self.view;
        v.transport = report.transport;
        v.media_encryption = report.media_encryption;
        v.detail_log_active = report.detail_log;
        v.calls = applied.calls;
        // How the audio stands now, as the module says: a microphone that
        // came back, a speaker handed back after a failed start, are there
        // in the next report, and nothing the log said stands against them.
        if let Some(audio) = &report.audio {
            v.aec_active = audio.ready && audio.processing;
            v.microphone_fallback = audio.microphone.input == "silence";
            // Only while a call's microphone is the device, open as the engine says.
            v.microphone_raw = if audio.microphone.input == "device" { audio.capture_raw } else { None };
            v.speaker_raw = if audio.speaker.playing { audio.playout_raw } else { None };
            // The endpoint each call stream is on, from the engine alone: the
            // window's volume and mute go to what the call really uses, not
            // to what the choice would open now.
            let call = |up: bool, endpoint: &Option<String>, stand_in: bool| {
                endpoint.as_ref().filter(|_| up).map(|id| CallEndpoint { id: id.clone(), stand_in })
            };
            v.microphone_call = call(audio.microphone.input == "device", &audio.microphone.endpoint, audio.microphone.stand_in);
            v.speaker_call = call(audio.speaker.playing, &audio.speaker.endpoint, audio.speaker.stand_in);
        }
        v.transfer = report.transfer;
        v.parking = report.parking;
        v.mwi = Mwi::parse(&report.mwi_summary);
        v.audio_processing_stats = report.audio_processing_stats;
        v.registration = report.registration;
        v.dnd = report.dnd;
        if !applied.ended.is_empty() {
            // A call of ours that never connected is said so, with the engine's
            // words beside the name; the history keeps the name.
            for call in &applied.ended {
                if !call.outcome.is_empty() && call.direction == message("HISTORY_OUTGOING") && call.outcome != message("HISTORY_CANCELLED") {
                    self.show_error(message_with("CALL_NOT_CONNECTED", [&call.outcome]));
                }
            }
            self.services.add_history(applied.ended);
        }
        for notice in self.notices.news(report.audio.as_ref(), report.tls_trust_certificates) {
            self.show_error(notice);
        }
        applied.answer
    }
    /// What has to happen when the engine is gone without being asked,
    /// whichever way it was noticed: the calls that were up get their history
    /// rows (their closing words never came), the call bookkeeping is emptied
    /// and the window shows the phone as disconnected. The recording is not
    /// touched here: its file is converted by log_stop, once the process is
    /// known to have ended and so to have closed it.
    fn close_calls_on_exit(&mut self, error: Option<String>) {
        let v = &mut self.view;
        if !v.running {
            return;
        }
        v.calls.clear();
        let dnd = v.dnd;
        v.running = false;
        v.recording = false;
        v.recording_call.clear();
        v.aec_active = false;
        v.detail_log_active = false;
        v.microphone_fallback = false;
        v.microphone_raw = None;
        v.speaker_raw = None;
        v.microphone_call = None;
        v.speaker_call = None;
        v.transfer = Transfer::default();
        v.parking.clear();
        v.audio_processing_stats = None;
        v.registration = "DISCONNECTED".into();
        if let Some(error) = error {
            v.error = error;
        }
        let rows = self.phone.engine_gone(dnd, now_secs());
        self.services.add_history(rows);
    }
    /// Everything of a restart that is decided before the engine is touched:
    /// the account, the settings and the numbers to watch.
    fn prepare_restart(&mut self) -> Result<(Account, Settings, String), String> {
        // A save that did not finish leaves its mark: the stored values may
        // be old and new mixed, and the phone does not connect on them, by
        // hand or otherwise, until a save has gone through whole.
        if !self.services.store.read_text(SAVE_MARK).is_empty() {
            return Err(message("SETTINGS_SAVE_INTERRUPTED"));
        }
        // A connect is asked for (the button, a saved setting) or follows an
        // automatic reason that checked first; either way the phone is wanted
        // registered from here on.
        self.view.unregistered_by_choice = false;
        // The same check the export reports on: the mark again, the
        // credential, the account, and the settings read and checked.
        let (account, settings) = connect_prerequisites(&self.services.store)?;
        let watched = watch_list(&settings);
        Ok((account, settings, watched))
    }
    /// The window's view of a phone that is connecting, and the new
    /// generation: the calls of the old engine are gone, and what the old
    /// one may still say is told apart by generation.
    /// Whether the owed start can be made now: no call is left.
    fn reconnect_due(&self) -> bool {
        self.reconnect_owed && self.phone.calls().is_empty() && !self.closing
    }
    fn begin_start(&mut self, settings: &Settings) -> u64 {
        self.reconnect_owed = false;
        self.services.logs.lock().unwrap().set_detail(settings.detail_log);
        // The window shows what the engine is actually running with, so a
        // reconnect brings the saved settings forward as well.
        self.notices = EngineNotices::default();
        let v = &mut self.view;
        v.settings = settings.clone();
        v.error.clear();
        v.aec_active = false;
        v.detail_log_active = false;
        v.microphone_fallback = false;
        v.microphone_raw = None;
        v.speaker_raw = None;
        v.microphone_call = None;
        v.speaker_call = None;
        v.recording = false;
        v.calls.clear();
        v.transfer = Transfer::default();
        v.parking.clear();
        v.registration = "CONNECTING".into();
        v.recording_call.clear();
        let generation = self.phone.new_engine();
        self.starting = Some(generation);
        self.starting_lost = false;
        generation
    }
    /// The started engine is taken; registering it is the flow's next step.
    fn take_ready(&mut self, ready: Ready) {
        for note in ready.notes {
            self.services.log(LOG_APP, note);
        }
        // A new engine starts outside maintenance, and on the saved settings
        // as a whole; nothing is owed to it.
        self.maintenance_off_owed = false;
        self.live_owed = None;
        self.bound = ready.address;
        self.link = Some(ready.link);
        self.view.running = true;
    }
    /// Takes the engine out of the phone: the calls still up get their rows
    /// (the engine takes them down with it), and the window shows the phone
    /// as disconnected. The process itself is still to be ended.
    fn take_link(&mut self) -> Option<EngineLink> {
        let engine = self.link.take()?;
        let rows = self.phone.engine_gone(self.view.dnd, now_secs());
        self.services.add_history(rows);
        self.recording_refused = None;
        let v = &mut self.view;
        v.running = false;
        v.recording = false;
        v.aec_active = false;
        v.detail_log_active = false;
        v.microphone_fallback = false;
        v.microphone_raw = None;
        v.speaker_raw = None;
        v.microphone_call = None;
        v.speaker_call = None;
        v.calls.clear();
        v.transfer = Transfer::default();
        v.parking.clear();
        v.registration = "DISCONNECTED".into();
        v.recording_call.clear();
        Some(engine)
    }
    /// A stop worker's outcome. Ok: the process has ended (see
    /// EngineLink::stop); how it went is part of the record (one that had to
    /// be killed was still waiting on something, usually a SIP request the
    /// server has not answered), and what it had open is closed
    /// (engine_ended). Err: its end could not be confirmed; the link comes
    /// back and is kept as `lost`, to be stopped again after RECOVER_RETRY,
    /// and until then the engine's resources count as held (engine_busy)
    /// and the window is told.
    fn take_stop(&mut self, outcome: StopOutcome) {
        match outcome {
            Ok(report) => {
                if report.forced {
                    self.services.log(LOG_APP, "ksip: the engine did not quit within 3 seconds and was ended".into());
                } else {
                    let code = report.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                    self.services.log(LOG_APP, format!("ksip: the engine quit in {} ms, exit code {code}", report.took.as_millis()));
                }
                self.engine_ended();
            }
            Err(back) => {
                let (link, e) = *back;
                self.services.log(LOG_APP, format!("ksip: the engine's end could not be confirmed ({e}); it is tried again"));
                self.view.error = message_with("ENGINE_STOP_UNCONFIRMED", [e]);
                self.lost = Some(link);
                self.recover_after = Instant::now() + RECOVER_RETRY;
            }
        }
    }
    /// The confirmed end of the engine process, whichever way it was learnt:
    /// the WAV it may have been writing is closed for certain and becomes an
    /// MP3, once.
    fn engine_ended(&mut self) {
        if let Some(wav) = self.open_recording.take() {
            self.convert_once(wav);
        }
    }
    /// Hands a closed WAV over for conversion, once per run: whichever word
    /// of its end comes first converts it, and the later ones find it done.
    fn convert_once(&mut self, wav: PathBuf) {
        if self.converted.insert(wav.clone()) {
            self.services.convert_recording(wav);
        }
    }
    /// The engine has closed the recording's file, by its answer to the stop
    /// or by its own word (the "closed" event): the window stops showing the
    /// recording, and the file becomes an MP3, once. An incomplete file
    /// (dropped samples, a write error) is converted all the same and the
    /// window told.
    fn recording_closed(&mut self, complete: bool) {
        self.view.recording = false;
        self.view.recording_call.clear();
        if let Some(wav) = self.open_recording.take() {
            self.convert_once(wav);
        }
        if !complete {
            self.view.error = message("RECORDING_WRITE_PROBLEM");
        }
    }
    /// The engine could not open the file of a recording it had reserved:
    /// there is no recording, and none is tried again for the same call.
    fn recording_failed(&mut self, detail: &str) {
        self.services.log(LOG_APP, format!("ksip: the recording could not be opened ({detail})"));
        self.recording_refused = Some(std::mem::take(&mut self.view.recording_call));
        self.view.recording = false;
        self.open_recording = None;
        self.view.error = message("RECORDING_START_FAILED");
    }
    /// Asks a worker for the chosen adapter's address, once per look, while
    /// the engine runs on an adapter and is wanted registered.
    fn probe_network(&mut self) {
        let adapter = self.view.settings.network_adapter.trim().to_string();
        if adapter.is_empty()
            || self.probing
            || self.closing
            || self.link.is_none()
            || self.starting.is_some()
            || self.view.unregistered_by_choice
        {
            return;
        }
        self.probing = true;
        self.work(move || {
            let address = crate::native::adapter_address(&adapter).ok();
            Work::Address { adapter, address }
        });
    }
    /// The engine binds one address, so a new one means the binding is stale.
    /// Re-registering does not re-bind; only a restart does, and that is what
    /// the reconnect does. A call is never cut short for this. Returns
    /// whether to connect again.
    fn follow_network(&mut self, adapter: &str, address: Option<String>) -> bool {
        self.probing = false;
        let Some(current) = address else {
            return false;
        };
        if adapter != self.view.settings.network_adapter.trim()
            || self.link.is_none()
            || self.view.unregistered_by_choice
            || self.bound == current
        {
            return false;
        }
        if !self.phone.calls().is_empty() {
            self.report_error(message("NETWORK_CHANGED"));
            return false;
        }
        self.services.log(
            LOG_APP,
            format!("ksip: adapter address changed, reconnecting on {current}"),
        );
        true
    }
    /// Everything the banner shows also belongs in the log, so that a report
    /// made afterwards still explains what the user saw.
    fn show_error(&mut self, error: String) {
        self.services.log(LOG_APP, error.clone());
        self.view.error = error;
    }
    fn report_error(&mut self, error: String) {
        if self.polling_error != error {
            self.services.log(LOG_APP, error.clone());
        }
        self.polling_error = error.clone();
        self.view.error = error;
    }
    /// Drops the banner the polling raised once polling works again. An error
    /// another path put on screen is left alone.
    fn clear_polling_error(&mut self) {
        if self.polling_error.is_empty() {
            return;
        }
        if self.view.error == self.polling_error {
            self.view.error.clear();
        }
        self.polling_error.clear();
    }
}

// ---- the steps a flow is made of

/// Sends a request and waits for its answer: the receive number with the
/// data, so that a state report can be placed in the order. The wait ends
/// with the answer, with the loss of the connection, or with the timeout.
/// One command to the engine and its answer. A `?` on the result turns the
/// error into the app's message; a flow that has to know whether the engine
/// heard the command looks at the RequestError itself.
async fn request(s: &Shared, command: &str, params: &str) -> Result<(u64, String), RequestError> {
    {
        let mut p = s.borrow_mut();
        let engine = p.link.as_mut().ok_or(RequestError::NotRunning)?;
        let generation = engine.generation();
        let token = engine.send(command, params).map_err(RequestError::Send)?;
        p.waiting = Some(Waiting::Response { token, generation, deadline: Instant::now() + RESPONSE_TIMEOUT });
        p.delivered = None;
    }
    let delivered = Await(s.clone()).await;
    s.borrow_mut().waiting = None;
    let Delivered::Response(answer) = delivered else {
        return Err(RequestError::Disconnected);
    };
    let (seq, response) = answer?;
    if response["ok"] != true {
        let detail = response["data"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| message("ACTION_FAILED"));
        return Err(RequestError::Remote { command: command.to_string(), detail });
    }
    Ok((seq, response["data"].as_str().unwrap_or("").into()))
}
/// Waits for the report of the worker the flow has just started, for at
/// most `timeout`: a worker that does not report in time is given up on, and
/// its report, should it come, finds nobody waiting.
async fn await_work(s: &Shared, timeout: Duration) -> Result<Work, String> {
    {
        let mut p = s.borrow_mut();
        p.waiting = Some(Waiting::Work { serial: p.work_serial, deadline: Instant::now() + timeout });
        p.delivered = None;
    }
    let delivered = Await(s.clone()).await;
    s.borrow_mut().waiting = None;
    match delivered {
        Delivered::Work(work) => Ok(work),
        Delivered::Response(Err(RequestError::Timeout)) => Err(message("WORK_TIMEOUT")),
        Delivered::Response(Err(e)) => Err(e.into()),
        Delivered::Response(Ok(_)) => Err(message("APP_CLOSING")),
    }
}
/// How the engine's end is written down: the exit code, "killed", or the
/// reason it could not be confirmed.
fn exit_code(outcome: &StopOutcome) -> String {
    match outcome {
        Ok(report) if report.forced => "killed".to_string(),
        Ok(report) => report.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
        Err(back) => back.1.clone(),
    }
}
/// How long each kind of work may take: the engine start has its own twelve
/// seconds and the stop before it three; the calibration plays and measures
/// for up to a minute; the rest is a few Windows calls.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const DEVICES_TIMEOUT: Duration = Duration::from_secs(15);
const CALIBRATION_TIMEOUT: Duration = Duration::from_secs(120);
/// Asks the engine for its state and applies the report.
async fn poll(s: &Shared) -> Result<(), String> {
    if s.borrow().link.is_none() {
        return Ok(());
    }
    let (seq, data) = request(s, "ksip_state", "").await?;
    let report: EngineReport = serde_json::from_str(&data).map_err(err)?;
    let answer = s.borrow_mut().apply_report(seq, report);
    for id in answer {
        let payload = serde_json::to_string(&json!({"op":"answer","id":id,"value":""})).map_err(err)?;
        // A failed automatic answer is reported once; the call can still be answered manually.
        if let Err(e) = request(s, "ksip_action", &payload).await {
            s.borrow().services.log(LOG_APP, format!("auto answer failed {}", e));
        }
    }
    sync_auto_record(s).await
}
/// Something that needs the phone quiet begins: only while no call is up,
/// after a fresh look at the engine (a call that has just come in is not in
/// the last report yet). The engine is told, so that a call arriving
/// meanwhile is refused as busy rather than left ringing at a phone that is
/// being worked on.
/// Begins maintenance for the operation that calls, and returns its number:
/// the operation ends it with leave_maintenance and nothing else can. Refused
/// while a call is up, or while another operation holds it (a calibration
/// whose worker is still out).
async fn enter_maintenance(s: &Shared) -> Result<u64, String> {
    poll(s).await?;
    let owner = s.borrow_mut().phone.begin_maintenance().map_err(|refused| match refused {
        MaintenanceRefused::CallInProgress => message("CALL_IN_PROGRESS"),
        MaintenanceRefused::Held => message("MAINTENANCE_IN_PROGRESS"),
    })?;
    // The engine takes the maintenance in one step, only while it has no
    // call: a call that came in after the look above is what its refusal
    // means, and the maintenance is then not begun.
    if let Err(e) = tell_maintenance(s, "on").await {
        let mut p = s.borrow_mut();
        p.phone.end_maintenance(owner);
        // No answer is not a "no": the engine may have taken the maintenance
        // and be refusing calls on it. It is told to end it at the next look,
        // as after a failed leave.
        if e.unknown() {
            p.maintenance_off_owed = true;
        }
        return Err(if e.busy() { message("CALL_IN_PROGRESS") } else { e.into() });
    }
    // The engine has taken this owner's maintenance: an end owed from an
    // earlier one is superseded, and this owner's leave says off in its turn.
    s.borrow_mut().maintenance_off_owed = false;
    Ok(owner)
}
/// Tells the engine again that maintenance has ended, when that is still
/// owed when this job's turn comes and nobody has begun another meanwhile:
/// a maintenance held now is not ended by an earlier owner's retry.
async fn maintenance_off(s: &Shared) {
    if !s.borrow().maintenance_off_due() {
        return;
    }
    if tell_maintenance(s, "off").await.is_ok() && !s.borrow().phone.in_maintenance() {
        s.borrow_mut().maintenance_off_owed = false;
    }
}
/// Ends the maintenance `owner` began. Another number ends nothing, and the
/// engine is not told off then: the maintenance is somebody else's.
async fn leave_maintenance(s: &Shared, owner: u64) {
    if !s.borrow_mut().phone.end_maintenance(owner) {
        return;
    }
    if tell_maintenance(s, "off").await.is_err() {
        // The engine would go on refusing calls; it is told again at the next look.
        s.borrow_mut().maintenance_off_owed = true;
    }
}
/// Tells the engine that maintenance begins or ends. Without an engine there
/// is nothing to tell, and nothing to refuse.
async fn tell_maintenance(s: &Shared, value: &str) -> Result<(), RequestError> {
    if s.borrow().link.is_none() {
        return Ok(());
    }
    let payload = json!({"op":"maintenance","id":"","value":value}).to_string();
    match request(s, "ksip_action", &payload).await {
        Ok(_) => Ok(()),
        Err(e) => {
            s.borrow().services.log(LOG_APP, format!("ksip: maintenance {value} not taken by the engine ({e})"));
            Err(e)
        }
    }
}
/// The control connection was lost: the process is ended (asked to quit,
/// then given three seconds before it is killed) and only then do the calls
/// that were up get their rows, the recording its conversion, and the window
/// the notice. A reconnect cannot start meanwhile: this runs as a flow, ahead
/// of everything that waits.
async fn recover(s: Shared) {
    let Some(engine) = s.borrow_mut().lost.take() else {
        return;
    };
    s.borrow_mut().work_exclusive(move || Work::Stopped(engine.stop()));
    let report = match await_work(&s, STOP_TIMEOUT).await {
        Ok(Work::Stopped(report)) => Some(report),
        _ => None,
    };
    let mut p = s.borrow_mut();
    match report {
        // Confirmed ended: the calls are settled. Not confirmed: the link is
        // back in `lost` and tried again after a while, the calls unsettled.
        Some(outcome) => p.finish_recovery(outcome),
        None => {
            // The stop worker has not reported: whether the process is gone
            // is unknown, so nothing is said to be over yet. The report,
            // when it comes, settles it (stray); the window is told meanwhile.
            p.services.log(LOG_APP, message("ENGINE_STOP_PENDING"));
            p.view.error = message("ENGINE_STOP_PENDING");
        }
    }
}
/// Starts the engine on the saved settings, stopping the one that runs. The
/// phone is in maintenance until the start has reported.
async fn connect(s: &Shared) -> Result<(), String> {
    if s.borrow().closing {
        return Err(message("APP_CLOSING"));
    }
    let owner = enter_maintenance(s).await?;
    let result = restart(s).await;
    leave_maintenance(s, owner).await;
    result
}
async fn restart(s: &Shared) -> Result<(), String> {
    let started = start_engine(s).await;
    // A start that failed before an engine was taken (it could not be
    // prepared, launched or reached) leaves no engine: the window says
    // disconnected, as for one that was lost, so that connecting again is
    // offered. One that failed later (the login) has already been taken out.
    if started.is_err() {
        let mut p = s.borrow_mut();
        if p.link.is_none() && p.view.registration == "CONNECTING" {
            p.view.running = false;
            p.view.registration = "DISCONNECTED".into();
        }
    }
    started
}
async fn start_engine(s: &Shared) -> Result<(), String> {
    // A start or stop given up on, or a lost engine not yet confirmed ended,
    // may still hold the ports: no new engine until that is settled.
    if s.borrow().engine_busy() {
        return Err(message("WORKER_OUTSTANDING"));
    }
    let (account, settings, watched) = s.borrow_mut().prepare_restart()?;
    // The old engine is asked to close what it holds while it can still
    // answer; the process is the worker's to end, before the new one starts
    // on the same ports.
    let old = release_link(s).await;
    let generation = s.borrow_mut().begin_start(&settings);
    {
        let mut p = s.borrow_mut();
        let (services, devices, tx) = (p.services.clone(), p.view.devices.clone(), p.tx.clone());
        p.work_exclusive(move || {
            let stopped = old.map(EngineLink::stop);
            if let Some(Err(back)) = &stopped {
                // The old engine could not be confirmed ended: no new one
                // over its ports. The link comes back with the report.
                let error = message_with("ENGINE_STOP_UNCONFIRMED", [back.1.clone()]);
                return Work::Started { stopped, result: Err(error) };
            }
            let result = services.prepare_start(&settings, &account, &devices).and_then(|prepared| {
                EngineLink::start(prepared.plan, generation, tx).map(|link| {
                    Box::new(Ready {
                        link,
                        address: prepared.address,
                        notes: prepared.notes,
                    })
                })
            });
            Work::Started { stopped, result }
        });
    }
    let started = await_work(s, START_TIMEOUT).await;
    let ready = {
        let mut p = s.borrow_mut();
        p.starting = None;
        let Ok(Work::Started { stopped, result, .. }) = started else {
            return Err(message("APP_CLOSING"));
        };
        if let Some(outcome) = stopped {
            p.take_stop(outcome);
        }
        *result?
    };
    let (closing, lost) = {
        let mut p = s.borrow_mut();
        (p.closing, std::mem::take(&mut p.starting_lost))
    };
    if closing || lost {
        stop_and_wait(s, ready.link).await;
        return Err(message(if closing { "APP_CLOSING" } else { "ENGINE_CONTROL_LOST" }));
    }
    s.borrow_mut().take_ready(ready);
    let registered = match request(s, "ksip_login", "").await {
        Ok(_) => request(s, "ksip_parking", &watched).await.map(|_| ()).map_err(String::from),
        Err(e) => Err(e.into()),
    };
    if let Err(e) = registered {
        if let Some(engine) = release_link(s).await {
            stop_and_wait(s, engine).await;
        }
        return Err(e);
    }
    Ok(())
}
/// Asks the engine to close what it holds while it can still answer, then
/// takes it out of the phone. The process is the caller's to end.
async fn release_link(s: &Shared) -> Option<EngineLink> {
    if s.borrow().link.is_some() {
        let _ = request(s, "ksip_record_stop", "").await;
        // Parking dialog subscriptions keep baresip's SIP stack alive.
        // Release them before quit so shutdown does not hit the kill timeout.
        let _ = request(s, "ksip_shutdown", "").await;
    }
    s.borrow_mut().take_link()
}
/// Ends an engine process that is no longer the phone's, on a worker, and
/// waits for it to be gone.
async fn stop_and_wait(s: &Shared, engine: EngineLink) {
    s.borrow_mut().work_exclusive(move || Work::Stopped(engine.stop()));
    match await_work(s, STOP_TIMEOUT).await {
        Ok(Work::Stopped(outcome)) => s.borrow_mut().take_stop(outcome),
        Ok(_) => {}
        // Not reported in time: the worker stays on the ledger, and its
        // report, when it comes, is taken in stray.
        Err(e) => s.borrow().services.log(LOG_APP, format!("ksip: the engine's stop has not reported yet ({e}); its report is awaited")),
    }
}
/// Stops the engine, if one runs.
async fn stop_engine(s: &Shared) {
    if let Some(engine) = release_link(s).await {
        stop_and_wait(s, engine).await;
    }
}
/// Reads the devices again for the lists and, when the engine runs and no
/// call is going on, gives it the saved ones once more (a choice changed
/// outside the window reaches it this way). A device unplugged and put back,
/// or a new Windows default, needs none of this: each stream picks its
/// endpoint as it opens (resolve_audio_endpoints). The engine is only
/// restarted when it does not take the devices.
async fn refresh_devices(s: &Shared) -> Result<(), String> {
    s.borrow_mut().work(|| Work::Devices(crate::audio::devices()));
    let devices = match await_work(s, DEVICES_TIMEOUT).await {
        Ok(Work::Devices(devices)) => devices,
        Ok(_) => return Err(message("APP_CLOSING")),
        Err(e) => return Err(e),
    };
    s.borrow_mut().view.devices = devices?;
    if s.borrow().link.is_none() {
        return Ok(());
    }
    let Ok(owner) = enter_maintenance(s).await else {
        return Ok(());
    };
    let settings = s.borrow().services.settings();
    let taken = match settings {
        Err(e) => {
            leave_maintenance(s, owner).await;
            return Err(e);
        }
        Ok(settings) => match apply_audio_endpoints(s, &settings).await {
            Ok(()) => true,
            Err(e) => {
                s.borrow().services.log(LOG_APP, format!("ksip: the engine did not take the audio devices ({e}), restarting it"));
                false
            }
        },
    };
    leave_maintenance(s, owner).await;
    // Restarting the engine would register again; someone who unregistered
    // on purpose keeps the devices they have until they connect themselves.
    if !taken && !s.borrow().view.unregistered_by_choice {
        return connect(s).await;
    }
    Ok(())
}
/// The first thing after the window is up: the devices, then the saved account.
async fn initialize(s: Shared) {
    if let Err(e) = refresh_devices(&s).await {
        s.borrow_mut().show_error(e);
    }
    let (error, has_password) = {
        let p = s.borrow();
        (!p.view.error.is_empty(), p.view.account.has_password)
    };
    if error || !has_password {
        return;
    }
    if let Err(e) = connect(&s).await {
        s.borrow_mut().show_error(e);
    }
}
/// Hands the saved microphone and speaker to the running engine: for the
/// calls to come, and for a call that is up, which the engine moves onto them
/// at once (ksip_audio_switch). What came of that for each side of the call
/// is the engine's answer, logged; the window follows the engine's state
/// report, which says which endpoint the call is on. The engine used to be
/// restarted for a change of device, which meant a new registration and a
/// second or more without a phone.
async fn apply_audio_endpoints(s: &Shared, settings: &Settings) -> Result<(), String> {
    let endpoints = s.borrow().services.resolve_audio_endpoints(settings);
    let (_, answer) = request(s, "ksip_audio_devices", &format!("{},{}", endpoints.microphone, endpoints.speaker)).await?;
    let p = s.borrow();
    for note in endpoint_notes(settings, &endpoints, &p.view.devices) {
        p.services.log(LOG_APP, note);
    }
    if let Some(note) = switch_note(&answer) {
        p.services.log(LOG_APP, note);
    }
    Ok(())
}
/// The engine's answer to the devices handed to it, as a line for the log:
/// what came of each side of a call that is up (device_module.cpp's
/// words), or nothing when no call was up to move.
fn switch_note(answer: &str) -> Option<String> {
    let outcome: serde_json::Value = serde_json::from_str(answer).ok()?;
    let side = |name: &str| outcome[name].as_str().unwrap_or("not_up").to_string();
    let (microphone, speaker) = (side("microphone"), side("speaker"));
    if microphone == "not_up" && speaker == "not_up" {
        return None;
    }
    Some(format!("ksip: the call's devices: microphone {microphone}, speaker {speaker}"))
}
/// Saves a microphone or speaker chosen from the list (or "default"), and
/// says whether an engine runs to hand it to.
fn save_audio_choice(s: &Shared, kind: &str, device: String) -> Result<(Settings, bool), String> {
    let mut p = s.borrow_mut();
    if !matches!(kind, "microphone" | "speaker")
        || (device != "default" && !p.view.devices.iter().any(|entry| entry.kind == kind && entry.id == device))
    {
        return Err(message("SETTINGS_AUDIO_DEVICE_INVALID"));
    }
    let mut settings = p.services.settings()?;
    if kind == "microphone" {
        settings.microphone = device;
    } else {
        settings.speaker = device;
    }
    validate(&settings)?;
    p.services.save_settings(&settings)?;
    p.view.settings = settings.clone();
    Ok((settings, p.link.is_some()))
}
async fn select_audio_device(s: &Shared, kind: &str, device: String) -> Result<(), String> {
    let owner = match enter_maintenance(s).await {
        Ok(owner) => owner,
        // A call is up: the choice goes to the engine at once, which moves
        // the call onto it. Without the maintenance, which a call refuses,
        // and without a restart when the engine does not take it, which would
        // end the call; the choice is saved either way, for the next start.
        Err(e) if e == message("CALL_IN_PROGRESS") => {
            let (settings, running) = save_audio_choice(s, kind, device)?;
            return if running { apply_audio_endpoints(s, &settings).await } else { Ok(()) };
        }
        Err(e) => return Err(e),
    };
    let outcome = match save_audio_choice(s, kind, device) {
        Err(e) => Err(e),
        Ok((_, false)) => Ok(false),
        Ok((settings, true)) => match apply_audio_endpoints(s, &settings).await {
            Ok(()) => Ok(true),
            Err(e) => {
                s.borrow().services.log(LOG_APP, format!("ksip: the engine did not take the audio devices ({e}), restarting it"));
                Ok(false)
            }
        },
    };
    leave_maintenance(s, owner).await;
    match outcome {
        Err(e) => Err(e),
        Ok(true) => Ok(()),
        Ok(false) => connect(s).await,
    }
}
/// The calibration plays and records for a while, so it runs on a thread of
/// its own; the phone is in maintenance until it reports back. It uses the
/// endpoints the choices stand for now, as a call would (the default in
/// place of a device that is not there).
async fn calibrate_aec(s: &Shared, microphone: String, speaker: String, careful: bool) -> Result<Calibration, String> {
    // A calibration given up on may still hold the devices.
    if s.borrow().engine_busy() {
        return Err(message("WORKER_OUTSTANDING"));
    }
    let microphone = crate::audio::resolve("microphone", &microphone)?.id;
    let speaker = crate::audio::resolve("speaker", &speaker)?.id;
    let owner = enter_maintenance(s).await?;
    s.borrow_mut().work_exclusive(move || Work::Calibrated(crate::audio::calibrate_aec(&microphone, &speaker, careful)));
    let result = match await_work(s, CALIBRATION_TIMEOUT).await {
        Ok(Work::Calibrated(result)) => result,
        Ok(_) => Err(message("APP_CLOSING")),
        Err(e) => {
            // The worker still has the devices: the maintenance stays, this
            // operation's, until its report comes (stray ends it then), and
            // no other operation can enter or end it meanwhile.
            s.borrow_mut().maintenance_held = Some(owner);
            return Err(e);
        }
    };
    leave_maintenance(s, owner).await;
    result
}
async fn set_volume(s: &Shared, kind: &str, device: &str, level: Option<u16>, mute: Option<bool>) -> Result<Volume, String> {
    let mut result = crate::audio::volume(kind, device, level.map(|value| value.min(100) as u8), mute)?;
    let Some(level) = level else {
        let p = s.borrow();
        let gain = if kind == "microphone" { p.view.settings.microphone_gain } else { p.view.settings.speaker_gain };
        if gain > 100 {
            result.level = gain;
        }
        return Ok(result);
    };
    let gain = level.max(100);
    if s.borrow().link.is_some() {
        request(s, "ksip_gain", &format!("{kind} {gain}")).await?;
    }
    let mut p = s.borrow_mut();
    let mut settings = p.services.settings()?;
    if kind == "microphone" {
        settings.microphone_gain = gain;
    } else {
        settings.speaker_gain = gain;
    }
    validate(&settings)?;
    p.services.save_settings(&settings)?;
    p.view.settings = settings;
    result.level = level;
    Ok(result)
}
/// Saves the settings and the account, and answers; whether the engine then
/// comes up on the new settings is a separate matter, told in the window's
/// own error line, so that the dialog can close on what did succeed.
/// The dialog's settings and account, checked and made whole: the CA file
/// is there, a ringback tone is one the call's player takes, an empty
/// extension is the authentication user, and an empty password is the one
/// stored for this user. Answers whether the account is another than the
/// one the phone registers with (another address or user, a password typed
/// in, or none stored yet).
fn check_configuration(p: &Phone, settings: &Settings, account: &mut Account) -> Result<bool, String> {
    validate(settings)?;
    let ca = settings.ca_file.trim();
    if !ca.is_empty() && !Path::new(ca).is_file() {
        return Err(message("SETTINGS_CA_FILE_MISSING"));
    }
    // The ringback tone plays through the call's player, which takes
    // 48 kHz mono only, so a chosen file is refused here rather than
    // found silent later. A file that is not there falls back to the
    // built-in tone, as every sound does.
    let ringback = settings.sound_ringback.trim();
    if !ringback.is_empty() {
        if let Ok(bytes) = std::fs::read(ringback) {
            if crate::wav::layout(&bytes)? != (48000, 1) {
                return Err(message("SETTINGS_SOUND_RINGBACK_FORMAT"));
            }
        }
    }
    // An empty extension registers under the authentication user.
    if account.extension.trim().is_empty() {
        account.extension = account.auth_user.trim().into();
    }
    let known = &p.view.account;
    let changed = !account.password.is_empty()
        || !known.has_password
        || account.server != known.server
        || account.port != known.port
        || account.extension != known.extension
        || account.auth_user != known.auth_user;
    if account.password.is_empty() {
        account.password = p.services.password_for(account)?;
    }
    account.validate()?;
    Ok(changed)
}
/// The settings dialog was saved. What the engine reads only at a start (see
/// Settings::RESTART), or another account, needs the engine started again;
/// everything else is taken in at once, with the engine left running and a
/// call up or not. A start would end the calls up or ringing, so while there
/// are any the settings are saved and the start is owed until none is left.
/// The answer is a notice for the window, empty when there is nothing to say.
async fn save_configuration(s: Shared, settings: Settings, mut account: Account, reply: Reply<String>) {
    let checked = {
        let p = s.borrow();
        check_configuration(&p, &settings, &mut account)
    };
    let account_changed = match checked {
        Ok(changed) => changed,
        Err(e) => return s.borrow().answer(reply, Err(e)),
    };
    let (old, restart, calls_up) = {
        let p = s.borrow();
        let old = p.view.settings.clone();
        let restart = account_changed || old.needs_restart(&settings);
        (old, restart, !p.phone.calls().is_empty())
    };
    if !restart {
        let saved = {
            let mut p = s.borrow_mut();
            p.services.persist_settings(&settings).map(|()| {
                p.view.settings = settings.clone();
                p.services.apply_browser_integration();
                p.view.error.clear();
            })
        };
        match saved {
            Err(e) => s.borrow().answer(reply, Err(e)),
            Ok(()) => {
                s.borrow().answer(reply, Ok(String::new()));
                apply_live(&s, &old, &settings).await;
            }
        }
        return;
    }
    if calls_up {
        let saved = {
            let mut p = s.borrow_mut();
            p.services.persist_configuration(&settings, &account).map(|()| {
                p.view.settings = settings;
                p.view.account = account.public();
                p.services.apply_browser_integration();
                p.view.error.clear();
                p.reconnect_owed = true;
            })
        };
        let notice = saved.map(|()| message("SETTINGS_RECONNECT_AFTER_CALLS"));
        return s.borrow().answer(reply, notice);
    }
    let owner = match enter_maintenance(&s).await {
        Ok(owner) => owner,
        Err(e) => return s.borrow().answer(reply, Err(e)),
    };
    let saved = {
        let mut p = s.borrow_mut();
        p.services.persist_configuration(&settings, &account).map(|()| {
            p.view.settings = settings;
            p.view.account = account.public();
            p.services.apply_browser_integration();
            p.view.error.clear();
        })
    };
    leave_maintenance(&s, owner).await;
    match saved {
        Err(e) => s.borrow().answer(reply, Err(e)),
        Ok(()) => {
            s.borrow().answer(reply, Ok(String::new()));
            if let Err(e) = connect(&s).await {
                s.borrow_mut().report_error(e);
            }
        }
    }
}
/// What changed of the settings the running engine is told of, told to it
/// (all of it, changed or not, while an earlier save's is owed: see
/// live_owed); the window's own settings need nothing more than the
/// snapshot. The save itself is done by now: what did not reach the engine
/// is shown as an error, and is owed to the next save.
async fn apply_live(s: &Shared, old: &Settings, new: &Settings) {
    let all = s.borrow().live_owed.is_some();
    let told = tell_engine(s, old, new, all).await;
    let mut p = s.borrow_mut();
    match told {
        Ok(()) => {
            // The engine has it all now: the error that said it had not goes.
            if let Some(shown) = p.live_owed.take() {
                if p.view.error == shown {
                    p.view.error.clear();
                }
            }
        }
        Err(e) => {
            p.live_owed = Some(e.clone());
            p.show_error(e);
        }
    }
}
/// The engine's part of apply_live. The sound files are replaced in place,
/// as a start does: the engine reads each file when it plays it.
async fn tell_engine(s: &Shared, old: &Settings, new: &Settings, all: bool) -> Result<(), String> {
    s.borrow().services.logs.lock().unwrap().set_detail(new.detail_log);
    if old.sounds() != new.sounds() {
        let notes = s.borrow().services.prepare_sounds(new).1;
        for note in notes {
            s.borrow().services.log(LOG_APP, note);
        }
    }
    if s.borrow().link.is_none() {
        return Ok(());
    }
    if all || old.detail_log != new.detail_log {
        request(s, "ksip_detail_log", if new.detail_log { "on" } else { "off" }).await?;
    }
    for (kind, before, after) in [("microphone", old.microphone_gain, new.microphone_gain), ("speaker", old.speaker_gain, new.speaker_gain)] {
        if all || before != after {
            request(s, "ksip_gain", &format!("{kind} {after}")).await?;
        }
    }
    let watched = watch_list(new);
    if all || watch_list(old) != watched {
        request(s, "ksip_parking", &watched).await?;
    }
    Ok(())
}
/// The numbers the engine watches, as ksip_parking takes them: one for each button,
/// comma-separated, empty ones included.
fn watch_list(settings: &Settings) -> String {
    let mut watched: Vec<String> = settings.watched_numbers().iter().map(|n| n.to_string()).collect();
    watched.resize(CustomButton::COUNT, String::new());
    watched.join(",")
}
/// The custom buttons alone, from the window's button editing: checked as
/// the buttons, saved whole or not at all, taken in at once, a call up or
/// not; the engine watches the numbers they name from now on.
async fn save_buttons(s: Shared, buttons: Vec<CustomButton>, reply: Reply<()>) {
    let saved = {
        let mut p = s.borrow_mut();
        (|| -> Result<Settings, String> {
            if buttons.len() != CustomButton::COUNT {
                return Err(message("SETTINGS_BUTTON_KIND_INVALID"));
            }
            validate_buttons(&buttons)?;
            p.services.save_buttons(&buttons)?;
            let old = p.view.settings.clone();
            p.view.settings.buttons = buttons;
            Ok(old)
        })()
    };
    match saved {
        Err(e) => s.borrow().answer(reply, Err(e)),
        Ok(old) => {
            s.borrow().answer(reply, Ok(()));
            let new = s.borrow().view.settings.clone();
            apply_live(&s, &old, &new).await;
        }
    }
}
// ---- recording
async fn start_recording(s: &Shared, id: &str) -> Result<(), String> {
    let (path, name) = {
        let p = s.borrow();
        // The folder appears next to the executable the first time something is recorded.
        let folder = p.services.data.join("recordings");
        std::fs::create_dir_all(&folder).map_err(err)?;
        let peer = p.phone.calls().iter().find(|call| call.id == id).map(|call| call.peer.clone()).unwrap_or_default();
        // Two recordings within a second with the same peer are told apart by a count.
        let wanted = recording_name(&stamp(), &peer);
        let mut name = wanted.clone();
        let mut count = 1;
        // The name is taken if any form of it is there: the WAV, the MP3 it
        // became, the MP3 still being made from it, or the WAV set aside as empty.
        let taken = |name: &str| {
            let wav = folder.join(name);
            ["wav", "mp3", "converting.mp3", "empty.wav"].iter().any(|ext| wav.with_extension(ext).exists())
        };
        while taken(&name) {
            count += 1;
            name = wanted.replace(".wav", &format!("-{count}.wav"));
        }
        (folder.join(&name), name)
    };
    let text = path.to_str().ok_or(message("RECORDING_PATH_INVALID"))?.to_string();
    // The file's name goes out first: the engine's events about it (an
    // "active" that comes before the answer, a "failed" or "closed" that
    // settles it) are matched to it meanwhile, and the engine it goes to is
    // remembered.
    {
        let mut p = s.borrow_mut();
        let generation = p.link.as_ref().map(EngineLink::generation).ok_or_else(|| message("ENGINE_NOT_RUNNING"))?;
        p.recording_pending = Some(PendingStart { path: path.clone(), generation, ended: None });
    }
    let answer = request(s, "ksip_record", &format!("{id} {text}")).await;
    let mut p = s.borrow_mut();
    let start = p.recording_pending.take().expect("the start that was out");
    // What the engine said about the file while the start was out comes
    // first: it is the later word.
    match start.ended {
        Some(PendingEnd::Failed) => {
            p.recording_refused = Some(id.to_string());
            return Err(message("RECORDING_START_FAILED"));
        }
        Some(PendingEnd::Closed { complete }) => {
            // Opened and already closed (its call ended at once): the file is
            // there, its call's history row names it, and it becomes an MP3
            // once, whether the closed event already handed it over or not.
            // Nothing is shown as recording.
            if p.open_recording.as_ref() == Some(&path) {
                p.open_recording = None;
            }
            p.phone.note_recording(id, &name);
            p.convert_once(path);
            if !complete {
                p.view.error = message("RECORDING_WRITE_PROBLEM");
            }
            return Ok(());
        }
        None => {}
    }
    let alive = p.link.as_ref().is_some_and(|link| link.generation() == start.generation);
    let answered = match answer {
        Ok((_, answer)) => Some(answer),
        Err(e) if e.unknown() && alive => {
            // No answer, and the engine is still there: it may well be
            // recording. It is shown as recording, with the file known open
            // only if the engine's "active" has said so, until its word or a
            // stop settles it.
            p.services.log(LOG_APP, format!("ksip: the recording start was not answered ({e})"));
            None
        }
        // No answer and the engine is gone: nothing was started that is
        // still running, and whatever file it opened is the confirmed end's
        // to convert (engine_ended). Nothing is shown.
        Err(e) if e.unknown() => return Err(e.into()),
        Err(e) => return Err(e.into()),
    };
    p.phone.note_recording(id, &name);
    p.recording_refused = None;
    // "started": the file is open. "reserved": the call has no audio yet, and
    // the engine's "active" event will say when the file is open (or "failed"
    // that it never will be).
    if answered.as_deref().is_some_and(|a| !a.contains("reserved")) {
        p.open_recording = Some(path.clone());
    }
    let v = &mut p.view;
    v.recording = true;
    v.recording_call = id.into();
    v.recording_path = path.to_string_lossy().into();
    if answered.is_none() {
        return Err(message("RECORDING_START_UNCONFIRMED"));
    }
    Ok(())
}
/// Ends the recording. The engine's answer says what became of the file:
/// Ok, it is closed and whole; an error from the engine, it is closed but
/// incomplete (samples dropped or a write failed). No answer says nothing:
/// the engine may still be writing, so the recording stays as the window
/// shows it and the file is left alone until the engine's "closed" event,
/// a later stop, or the confirmed end of the process.
async fn stop_recording(s: &Shared) -> Result<(), String> {
    if !s.borrow().view.recording {
        return Ok(());
    }
    match request(s, "ksip_record_stop", "").await {
        Ok(_) => {
            s.borrow_mut().recording_closed(true);
            Ok(())
        }
        Err(e @ RequestError::Remote { .. }) => {
            // The engine answered: the file is closed, but not whole.
            let mut p = s.borrow_mut();
            p.services.log(LOG_APP, format!("ksip: the recording did not end cleanly ({e})"));
            p.recording_closed(false);
            Ok(())
        }
        Err(e) => {
            // No answer, or none could be asked for: nothing is known about
            // the file. It is converted once the engine's "closed" event or
            // the confirmed end of its process says it is closed.
            let p = s.borrow();
            p.services.log(LOG_APP, format!("ksip: the recording stop was not answered ({e})"));
            Err(message("RECORDING_STOP_UNCONFIRMED"))
        }
    }
}
async fn sync_auto_record(s: &Shared) -> Result<(), String> {
    let (auto_record, target, recording, recording_call, idle) = {
        let p = s.borrow();
        let auto_record = p.view.settings.auto_record;
        (
            auto_record,
            automatic_recording_target(auto_record, p.phone.calls()),
            p.view.recording,
            p.view.recording_call.clone(),
            p.phone.calls().is_empty(),
        )
    };
    if recording && (!auto_record || idle) {
        return stop_recording(s).await;
    }
    if recording && target.as_deref() != Some(recording_call.as_str()) {
        request(s, "ksip_record_select", target.as_deref().unwrap_or("-")).await?;
        let mut p = s.borrow_mut();
        // The file goes on with the call it was switched to.
        if let Some(id) = &target {
            let name = Path::new(&p.view.recording_path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            p.phone.note_recording(id, &name);
        }
        p.view.recording_call = target.unwrap_or_default();
    } else if !recording {
        if let Some(id) = target {
            // A call whose file the engine could not open is not tried again.
            if s.borrow().recording_refused.as_deref() == Some(id.as_str()) {
                return Ok(());
            }
            start_recording(s, &id).await?;
        }
    }
    Ok(())
}
// ---- the window's operations
async fn action(s: &Shared, name: &str, id: &str, value: &str, line: u8) -> Result<String, String> {
    if !matches!(
        name,
        "dial"
            | "answer"
            | "hangup"
            | "hold"
            | "resume"
            | "select"
            | "transfer"
            | "unregister"
            | "dtmf"
            | "blind_transfer"
            | "auto_record"
            | "dnd"
    ) {
        return Err(message("ACTION_UNSUPPORTED"));
    }
    if !(1..=2).contains(&line) || id.len() > 200 || id.chars().any(char::is_control) || value.len() > 200 || value.chars().any(char::is_control) {
        return Err(message("ACTION_ARGUMENT_INVALID"));
    }
    if name == "auto_record" {
        let enabled = match value {
            "on" => true,
            "off" => false,
            _ => return Err(message("AUTO_RECORD_ARGUMENT_INVALID")),
        };
        {
            let mut p = s.borrow_mut();
            let mut settings = p.services.settings()?;
            settings.auto_record = enabled;
            p.services.save_settings(&settings)?;
            p.view.settings = settings;
        }
        sync_auto_record(s).await?;
        return Ok(if enabled { message("AUTO_RECORD_ON") } else { message("AUTO_RECORD_OFF") });
    }
    if name == "dnd" && !matches!(value, "on" | "off") {
        return Err(message("ACTION_ARGUMENT_INVALID"));
    }
    let target = {
        let p = s.borrow();
        let calls = p.phone.calls();
        if name == "dial" && calls.iter().any(|c| c.line == line) {
            return Err(message("CALL_LINE_BUSY"));
        }
        // The two calls in either order: the engine refers the one
        // established first, whichever line it is on.
        if name == "transfer" && (id == value || !calls.iter().any(|c| c.id == id) || !calls.iter().any(|c| c.id == value)) {
            return Err(message("TRANSFER_NEEDS_TWO_CALLS"));
        }
        // What the engine is sent: the value as it came, or, for a button,
        // the address the button was set up with.
        let mut target = value.to_string();
        if name == "blind_transfer" {
            // Only a target one of the buttons was set up with, and only a
            // call that is actually in progress.
            let wanted = CustomButton::address(value);
            let known = p.view.settings.buttons.iter().find(|b| b.transfer_target() == Some(wanted)).and_then(|b| b.transfer_text());
            match known {
                Some(t) if calls.iter().any(|c| c.id == id && c.state == "ESTABLISHED" && !c.held) => target = t.to_string(),
                _ => return Err(message("BUTTON_NEEDS_CALL_AND_TARGET")),
            }
        }
        if name == "dial" {
            // The dial box, a button and a link are held to the same rule.
            target = dial_target(value)?;
        }
        target
    };
    // A digit goes the way the settings say (read for each one, so a saved
    // change needs no restart); the engine says when the peer took no
    // RFC 4733 events, which would otherwise drop the digit silently.
    let mode = if name == "dtmf" { s.borrow().view.settings.dtmf_mode.clone() } else { String::new() };
    let payload = serde_json::to_string(&json!({"op":name,"id":id,"value":target,"mode":mode})).map_err(err)?;
    let (_, result) = match request(s, "ksip_action", &payload).await {
        Err(RequestError::Remote { detail, .. }) if detail.trim() == "DTMF_RTP_NOT_OFFERED" => return Err(message("DTMF_RTP_NOT_OFFERED")),
        other => other?,
    };
    {
        let mut p = s.borrow_mut();
        if name == "dnd" {
            p.services.log(LOG_APP, if value == "on" { message("DND_ON") } else { message("DND_OFF") });
        }
        if name == "unregister" {
            p.view.unregistered_by_choice = true;
        }
        if name == "dial" {
            p.phone.note_dialled(result.trim(), line);
        }
    }
    poll(s).await?;
    // An operation that went through supersedes whatever the banner said;
    // choosing a line is not an operation on the phone.
    if name != "select" {
        s.borrow_mut().view.error.clear();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> Actor {
        let (services, view) = Services::open();
        let (tx, rx) = mpsc::channel();
        let published = Published(Arc::new(Mutex::new(view.clone())));
        Actor::new(services, view, rx, tx, published)
    }
    /// Runs a flow that needs nothing from outside to completion.
    fn run_now<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the flow waited for something"),
        }
    }
    /// A flow that never finishes: something long is being done.
    fn busy(a: &mut Actor) {
        a.current = Some(Box::pin(std::future::pending()));
    }

    #[test]
    fn polling_errors_clear_themselves_but_leave_other_errors() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        p.report_error(message("TEMPORARY"));
        assert_eq!(p.view.error, "TEMPORARY");
        p.clear_polling_error();
        assert_eq!(p.view.error, "");
        p.report_error(message("TEMPORARY"));
        p.view.error = message("AUDIO_DEVICE_INIT_FAILED");
        p.clear_polling_error();
        assert_eq!(p.view.error, "AUDIO_DEVICE_INIT_FAILED");
    }
    /// A state report as the engine writes it, with the audio module's state.
    fn report_with(audio: Value, trust: Option<u64>) -> EngineReport {
        let transfer = serde_json::to_value(Transfer::default()).unwrap();
        let mut report = json!({"registration": "REGISTER_OK", "calls": [], "transfer": transfer, "audio": audio});
        if let Some(n) = trust {
            report["tls_trust_certificates"] = json!(n);
        }
        serde_json::from_value(report).unwrap()
    }
    #[test]
    fn the_detail_log_mark_follows_what_the_engine_says_not_the_setting() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        p.view.running = true;
        p.view.settings.detail_log = true;
        let mut report = report_with(json!(null), None);
        p.apply_report(1, report);
        assert!(!p.view.detail_log_active, "a report that does not say it is on is off, whatever the settings say");
        report = report_with(json!(null), None);
        report.detail_log = true;
        p.apply_report(2, report);
        assert!(p.view.detail_log_active);
        // The engine gone, the mark goes with it.
        p.close_calls_on_exit(None);
        assert!(!p.view.detail_log_active);
    }
    #[test]
    fn whether_the_microphone_and_speaker_are_raw_is_shown_only_while_a_call_has_each() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        p.view.running = true;
        let audio = |input: &str, raw: Option<bool>| {
            let mut audio = json!({"ready": true, "processing": true,
                                   "microphone": {"input": input, "failures": 0},
                                   "speaker": {"playing": false, "failures": 0}});
            if let Some(raw) = raw {
                audio["capture_raw"] = json!(raw);
            }
            audio
        };
        // The speaker: shown while a call plays on it, as the engine says.
        let mut playing = audio("device", Some(true));
        playing["speaker"]["playing"] = json!(true);
        playing["playout_raw"] = json!(false);
        p.apply_report(1, report_with(playing.clone(), None));
        assert_eq!((p.view.microphone_raw, p.view.speaker_raw), (Some(true), Some(false)));
        playing["speaker"]["playing"] = json!(false);
        p.apply_report(2, report_with(playing, None));
        assert_eq!(p.view.speaker_raw, None, "nothing plays, nothing is said of the speaker");
        p.apply_report(3, report_with(audio("device", None), None));
        assert_eq!(p.view.microphone_raw, None, "nothing is said until the engine says it");
        p.apply_report(4, report_with(audio("device", Some(false)), None));
        assert_eq!(p.view.microphone_raw, Some(false));
        p.apply_report(5, report_with(audio("device", Some(true)), None));
        assert_eq!(p.view.microphone_raw, Some(true));
        // The calls over, the microphone closed: gone.
        p.apply_report(6, report_with(audio("none", Some(true)), None));
        assert_eq!(p.view.microphone_raw, None);
        // Silence standing in for a microphone that would not start is not the device.
        p.apply_report(7, report_with(audio("silence", Some(true)), None));
        assert_eq!(p.view.microphone_raw, None);
        p.apply_report(8, report_with(audio("device", Some(true)), None));
        p.close_calls_on_exit(None);
        assert_eq!(p.view.microphone_raw, None, "the engine gone, so is what it said");
    }
    #[test]
    fn the_audio_state_follows_the_reports_through_failure_and_recovery() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        // The module's state as audio_state.h writes it: the microphone's input
        // and failures, the speaker's failures.
        // The endpoint each stream is on is in the report whatever the input
        // says; the window takes it only while the stream is up.
        let audio = |input: &str, microphone: u64, speaker: u64| {
            json!({"ready": true, "processing": true,
                   "microphone": {"input": input, "endpoint": "{mic}", "stand_in": microphone > 0, "failures": microphone, "last_result": -5},
                   "speaker": {"playing": true, "endpoint": "{spk}", "stand_in": false, "failures": speaker, "last_result": -3}})
        };
        p.apply_report(1, report_with(audio("device", 0, 0), None));
        assert!(p.view.aec_active && !p.view.microphone_fallback && p.view.error.is_empty());
        assert_eq!(p.view.microphone_call, Some(CallEndpoint { id: "{mic}".into(), stand_in: false }), "the call's microphone, as the engine opened it");
        assert_eq!(p.view.speaker_call, Some(CallEndpoint { id: "{spk}".into(), stand_in: false }));
        // The microphone would not start: silence stands in, and says so by the
        // notice, not by a banner; no endpoint has the call's microphone then.
        p.apply_report(2, report_with(audio("silence", 1, 0), None));
        assert!(p.view.microphone_fallback && p.view.aec_active && p.view.error.is_empty());
        assert_eq!(p.view.microphone_call, None, "silence is on no endpoint");
        // Silence replaced by silence is still silence; the device back is the device.
        p.apply_report(3, report_with(audio("silence", 2, 0), None));
        assert!(p.view.microphone_fallback);
        p.apply_report(4, report_with(audio("device", 2, 0), None));
        assert!(!p.view.microphone_fallback, "the microphone that came back is shown as back");
        assert_eq!(p.view.microphone_call, Some(CallEndpoint { id: "{mic}".into(), stand_in: true }), "on an endpoint standing in for the one chosen");
        // A speaker that would not start is said once, and the processing stays
        // on: the stream was handed back.
        p.apply_report(5, report_with(audio("device", 2, 1), None));
        assert_eq!(p.view.error, message_with("AUDIO_SPEAKER_START_FAILED", ["-3"]));
        assert!(p.view.aec_active, "a failed start does not turn the processing off");
        p.view.error.clear();
        p.apply_report(6, report_with(audio("device", 2, 1), None));
        assert!(p.view.error.is_empty(), "a failure already said is not said again by the next report");
        // A report older than one applied changes nothing.
        p.apply_report(5, report_with(audio("silence", 2, 1), None));
        assert!(!p.view.microphone_fallback);
        // Calls gone and the source with them: the module says none.
        p.apply_report(7, report_with(audio("none", 2, 1), None));
        assert!(!p.view.microphone_fallback);
        assert_eq!(p.view.microphone_call, None, "no call, no endpoint of its own");
        assert!(p.view.speaker_call.is_some(), "the speaker plays on (a tone, say)");
        // The engine gone, so is what it said.
        p.view.running = true;
        p.close_calls_on_exit(None);
        assert!(p.view.microphone_call.is_none() && p.view.speaker_call.is_none());
    }
    #[test]
    fn the_engines_answer_to_a_switch_is_a_line_for_the_log() {
        assert_eq!(switch_note(r#"{"microphone":"moved","speaker":"kept"}"#), Some("ksip: the call's devices: microphone moved, speaker kept".into()));
        assert_eq!(switch_note(r#"{"microphone":"not_up","speaker":"not_up"}"#), None, "no call was up: nothing to say");
        assert_eq!(switch_note("{}"), None, "the audio module not up: nothing to say");
        assert_eq!(switch_note(""), None, "no answer: nothing to say");
        assert_eq!(switch_note(r#"{"speaker":"unchanged"}"#), Some("ksip: the call's devices: microphone not_up, speaker unchanged".into()));
    }
    #[test]
    fn a_speaker_failure_is_said_though_the_microphone_failed_after_it_before_the_report() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        let report = |microphone: u64, speaker: u64, input: &str| {
            report_with(
                json!({"ready": true, "processing": true,
                       "microphone": {"input": input, "failures": microphone, "last_result": -5},
                       "speaker": {"playing": false, "failures": speaker, "last_result": -3}}),
                None,
            )
        };
        p.apply_report(1, report(0, 0, "none"));
        // Between two reports the speaker failed and then the microphone: both
        // are in the next report, each counted on its own.
        p.apply_report(2, report(1, 1, "silence"));
        assert_eq!(p.view.error, message_with("AUDIO_SPEAKER_START_FAILED", ["-3"]));
        assert!(p.view.microphone_fallback);
        // The microphone recovering does not take the speaker's failure back,
        // and a later failure of the speaker (a hand-back, say) is said again.
        p.view.error.clear();
        p.apply_report(3, report(1, 1, "device"));
        assert!(p.view.error.is_empty() && !p.view.microphone_fallback);
        p.apply_report(4, report(1, 2, "device"));
        assert_eq!(p.view.error, message_with("AUDIO_SPEAKER_START_FAILED", ["-3"]));
    }
    #[test]
    fn a_module_that_is_not_up_and_an_empty_trust_store_are_said_once() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        let down = json!({"ready": false, "processing": false,
                          "microphone": {"input": "none", "failures": 0}, "speaker": {"playing": false, "failures": 0}});
        p.apply_report(1, report_with(down.clone(), Some(0)));
        assert!(!p.view.aec_active);
        assert_eq!(p.view.error, message("TRUST_STORE_UNUSABLE"), "the last thing said stands");
        p.view.error.clear();
        p.apply_report(2, report_with(down.clone(), Some(0)));
        assert!(p.view.error.is_empty(), "neither is said again by the same engine");
        // A new engine is told apart: what it reports is said anew.
        p.begin_start(&Settings::default());
        p.apply_report(1, report_with(down, Some(120)));
        assert_eq!(p.view.error, message("AUDIO_DEVICE_INIT_FAILED"));
        // No report of the audio at all (an engine without the module's
        // report) changes and says nothing.
        p.view.error.clear();
        let transfer = serde_json::to_value(Transfer::default()).unwrap();
        p.apply_report(2, serde_json::from_value(json!({"registration": "REGISTER_OK", "calls": [], "transfer": transfer})).unwrap());
        assert!(p.view.error.is_empty());
    }
    #[test]
    fn the_engine_log_changes_no_state() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        p.starting = Some(3);
        let line = |text: &str| LinkMessage { generation: 3, seq: 1, body: LinkBody::Log(text.into()) };
        for text in [
            "ksip: microphone fallback active",
            "ksip_audio: Google WebRTC ADM + APM initialized (processing enabled: aec on)",
            "ksip_audio: start playout failed (-3) for {speaker}",
            "ksip_audio: WebRTC ADM playout handed back after a failed start",
            "ua: tls_add_ca() failed: No such file",
        ] {
            p.handle_link(line(text));
        }
        assert!(!p.view.microphone_fallback && !p.view.aec_active && p.view.error.is_empty(), "the words are logged, and that is all");
        // The ring tone's player is the one thing the log still speaks for.
        p.handle_link(line("wasapi/play: IAudioClient_Initialize failed (0x88890008)"));
        assert!(p.view.error.contains("IAudioClient_Initialize"));
        assert!(!p.view.aec_active);
    }
    #[test]
    fn words_of_an_engine_no_longer_held_change_nothing() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        // No engine is held, so no generation is current: the line is logged
        // and nothing else.
        let line = |body| LinkMessage { generation: 7, seq: 1, body };
        p.handle_link(line(LinkBody::Log("wasapi/play: IAudioClient_Start failed".into())));
        assert!(p.view.error.is_empty());
        p.handle_link(line(LinkBody::Event(json!({"event":true,"type":"REGISTER_OK","param":""}))));
        assert_eq!(p.view.registration, "UNCONFIGURED");
        p.handle_link(line(LinkBody::Lost));
        assert!(p.view.error.is_empty());
    }
    #[test]
    fn the_first_words_of_an_engine_being_started_count_and_its_loss_is_noted() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        p.starting = Some(3);
        let line = |body| LinkMessage { generation: 3, seq: 1, body };
        p.handle_link(line(LinkBody::Log("wasapi/play: IAudioClient_Start failed".into())));
        assert!(p.view.error.contains("IAudioClient_Start"), "the ring tone's failure of an engine being started counts");
        p.view.error.clear();
        p.handle_link(line(LinkBody::Lost));
        assert!(p.starting_lost, "the start finds its engine gone when it reports");
        assert!(p.view.error.is_empty(), "nothing is said until the start reports");
    }
    #[test]
    fn an_answer_reaches_the_flow_that_waits_for_it_and_no_other() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        p.waiting = Some(Waiting::Response { token: "1-1".into(), generation: 1, deadline: Instant::now() + RESPONSE_TIMEOUT });
        p.handle_link(LinkMessage { generation: 1, seq: 4, body: LinkBody::Response { token: "1-9".into(), value: json!({"ok":true}) } });
        assert!(p.delivered.is_none(), "another token is not the answer");
        p.handle_link(LinkMessage { generation: 2, seq: 5, body: LinkBody::Response { token: "1-1".into(), value: json!({"ok":true}) } });
        assert!(p.delivered.is_none(), "another engine's answer is not the answer");
        p.handle_link(LinkMessage { generation: 1, seq: 6, body: LinkBody::Response { token: "1-1".into(), value: json!({"ok":true,"data":"x"}) } });
        match p.delivered.take() {
            Some(Delivered::Response(Ok((seq, value)))) => {
                assert_eq!(seq, 6);
                assert_eq!(value["data"], "x");
            }
            _ => panic!("the answer was not delivered"),
        }
    }
    #[test]
    fn operations_wait_their_turn_and_run_in_order_while_questions_are_answered_at_once() {
        let mut a = actor();
        busy(&mut a);
        let (first, first_rx) = Reply::channel();
        let (second, second_rx) = Reply::channel();
        a.handle_command(Command::Action { name: "record".into(), id: String::new(), value: String::new(), line: 1, reply: first });
        a.handle_command(Command::Action { name: "answer".into(), id: String::new(), value: String::new(), line: 9, reply: second });
        a.drive();
        assert!(first_rx.try_recv().is_err(), "nothing runs while the phone is busy");
        assert_eq!(a.queue.len(), 2);
        // The window's questions are answered meanwhile.
        a.handle_command(Command::WindowVisible(false));
        assert!(!a.shared.borrow().view.window_visible);
        a.current = None;
        a.drive();
        assert_eq!(first_rx.recv().unwrap(), Err(message("ACTION_UNSUPPORTED")));
        assert_eq!(second_rx.recv().unwrap(), Err(message("ACTION_ARGUMENT_INVALID")));
        assert!(a.current.is_none() && a.queue.is_empty());
    }
    /// A save that did not reach the engine (its answer timed out) is told
    /// again, whole, by the next save, even of the same settings; once that
    /// goes through, the error it left is gone. With the real engine, as the
    /// other real_engine test: `cargo test real_engine -- --ignored`.
    #[test]
    #[ignore]
    fn real_engine_live_settings_that_did_not_reach_it_are_told_again_by_the_next_save() {
        let _one_at_a_time = crate::storage::store_tests_one_at_a_time();
        let a = actor();
        let store = crate::storage::Store::at(r"Software\KashiharaCity\ksip\Test\test-live-owed".into(), "KSIP/Test/test-live-owed".into());
        store.cleanup_test();
        a.shared.borrow_mut().services.store = store.clone();
        let settings = Settings { sip_port: 18062, rtp_port: 18102, ..Settings::default() };
        let account = Account { server: "127.0.0.1".into(), port: 18999, extension: "1001".into(), auth_user: "1001".into(), password: "test-only".into() };
        let devices = crate::audio::devices().unwrap();
        let prepared = a.shared.borrow().services.prepare_start(&settings, &account, &devices).unwrap();
        let (tx, _rx) = mpsc::channel();
        a.shared.borrow_mut().link = Some(EngineLink::start(prepared.plan, 100, tx).unwrap());
        a.shared.borrow_mut().view.settings = settings.clone();
        let mut buttons = settings.buttons;
        buttons[0] = CustomButton { kind: "dial".into(), number: "1002".into(), ..CustomButton::default() };
        let mut cx = Context::from_waker(Waker::noop());
        // The save is answered before the engine is told; the telling times out.
        let (reply, rx) = Reply::channel();
        let mut first = Box::pin(save_buttons(a.shared.clone(), buttons.clone(), reply));
        assert!(first.as_mut().poll(&mut cx).is_pending());
        assert_eq!(rx.try_recv().unwrap(), Ok(()), "saved before the engine answers");
        a.shared.borrow_mut().delivered = Some(Delivered::Response(Err(RequestError::Timeout)));
        assert!(first.as_mut().poll(&mut cx).is_ready());
        drop(first);
        let shown = a.shared.borrow().view.error.clone();
        assert!(!shown.is_empty() && a.shared.borrow().live_owed.as_deref() == Some(shown.as_str()));
        // The same buttons saved again: the engine is told everything again.
        let (reply, _rx) = Reply::channel();
        let mut retry = Box::pin(save_buttons(a.shared.clone(), buttons, reply));
        let mut told = Vec::new();
        while retry.as_mut().poll(&mut cx).is_pending() {
            assert!(matches!(a.shared.borrow().waiting, Some(Waiting::Response { .. })), "the retry waits for an answer");
            told.push(a.shared.borrow().link.as_ref().and_then(|l| l.sent.last().cloned()).unwrap_or_default());
            a.shared.borrow_mut().delivered = Some(Delivered::Response(Ok((1, serde_json::json!({"ok": true, "data": ""})))));
        }
        drop(retry);
        let link = a.shared.borrow_mut().link.take().unwrap();
        let stopped = link.stop();
        store.cleanup_test();
        assert_eq!(told, ["ksip_detail_log", "ksip_gain", "ksip_gain", "ksip_parking"], "every command the engine takes while running is sent again");
        assert!(a.shared.borrow().live_owed.is_none() && a.shared.borrow().view.error.is_empty(), "told, the error goes");
        assert!(stopped.is_ok());
    }
    /// A start that fails before there is an engine (here the profile folder
    /// cannot be made, a file being in its place) leaves the phone shown as
    /// disconnected, not connecting, so that the window offers to connect again.
    #[test]
    fn a_start_that_fails_before_there_is_an_engine_leaves_the_phone_disconnected() {
        let _one_at_a_time = crate::storage::store_tests_one_at_a_time();
        let mut a = actor();
        let name = format!("test-failed-start-{}", std::process::id());
        let store = crate::storage::Store::at(format!(r"Software\KashiharaCity\ksip\Test\{name}"), format!("KSIP/Test/{name}"));
        let account = Account { server: "pbx.example".into(), port: 5060, extension: "1001".into(), auth_user: "1001".into(), password: "test-only".into() };
        store.write_account(&account).unwrap();
        {
            let mut p = a.shared.borrow_mut();
            p.services.store = store.clone();
            p.view.account = account.public();
            p.view.settings = Settings::default();
        }
        let profile = a.shared.borrow().services.profile_dir();
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&profile);
        std::fs::write(&profile, b"in the way of the engine's profile folder").unwrap();
        let s = a.shared.clone();
        let mut future = Box::pin(connect(&s));
        let mut cx = Context::from_waker(Waker::noop());
        let result = loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(result) => break result,
                Poll::Pending => {
                    let msg = a.rx.recv_timeout(Duration::from_secs(20)).unwrap();
                    a.handle(msg);
                }
            }
        };
        let (registration, running, link) = {
            let p = s.borrow();
            (p.view.registration.clone(), p.view.running, p.link.is_some())
        };
        std::fs::remove_file(&profile).unwrap();
        store.cleanup_test();
        assert!(result.is_err(), "the start fails");
        assert_eq!((registration.as_str(), running, link), ("DISCONNECTED", false, false), "no engine, and shown as disconnected");
    }
    #[test]
    fn a_reconnect_owed_waits_for_the_last_call_and_is_queued_once() {
        let mut a = actor();
        busy(&mut a);
        let transfer = serde_json::to_value(Transfer::default()).unwrap();
        let report = |calls: Value| -> EngineReport {
            serde_json::from_value(json!({"registration": "REGISTER_OK", "calls": calls, "transfer": transfer.clone()})).unwrap()
        };
        let ringing = json!([{"id": "c1", "peer": "sip:1002@pbx.example", "state": "INCOMING", "held": false, "duration": 0, "line": 1}]);
        let owed = |a: &Actor| a.queue.iter().filter(|(_, job)| matches!(job, Job::ApplySaved)).count();
        {
            let mut p = a.shared.borrow_mut();
            p.apply_report(1, report(ringing));
            p.reconnect_owed = true;
            assert!(!p.reconnect_due(), "not while a call rings");
        }
        a.handle(Message::Command(Command::WindowVisible(true)));
        assert_eq!(owed(&a), 0, "nothing is queued while a call is left");
        a.shared.borrow_mut().apply_report(2, report(json!([])));
        a.handle(Message::Command(Command::WindowVisible(true)));
        a.handle(Message::Command(Command::WindowVisible(true)));
        assert_eq!(owed(&a), 1, "queued once the last call has gone, and only once");
        // Any start takes the saved settings, and clears what was owed.
        a.shared.borrow_mut().begin_start(&Settings::default());
        assert!(!a.shared.borrow().reconnect_owed);
    }
    #[test]
    fn an_operation_that_waited_too_long_is_dropped() {
        let mut a = actor();
        busy(&mut a);
        let (reply, rx) = Reply::channel();
        a.handle_command(Command::Action { name: "hangup".into(), id: String::new(), value: String::new(), line: 1, reply });
        a.queue[0].0 = Instant::now() - WAIT_TIMEOUT - Duration::from_secs(1);
        a.tick();
        assert!(a.queue.iter().all(|(_, job)| matches!(job, Job::Poll)), "only the look at the engine is left waiting");
        assert_eq!(rx.recv().unwrap(), Err(message("OPERATION_WAIT_TIMEOUT")));
    }
    #[test]
    fn an_answer_that_does_not_come_in_time_ends_the_wait() {
        let mut a = actor();
        a.shared.borrow_mut().waiting = Some(Waiting::Response { token: "1-1".into(), generation: 1, deadline: Instant::now() - Duration::from_secs(1) });
        a.tick();
        assert!(matches!(a.shared.borrow_mut().delivered.take(), Some(Delivered::Response(Err(RequestError::Timeout)))));
    }
    #[test]
    fn leaving_answers_what_was_still_waiting_once_the_engine_is_gone() {
        let mut a = actor();
        busy(&mut a);
        let (waiting, waiting_rx) = Reply::channel();
        a.handle_command(Command::Connect(waiting));
        let (done, done_rx) = Reply::channel();
        a.handle_command(Command::Shutdown(done));
        assert_eq!(waiting_rx.recv().unwrap(), Err(message("APP_CLOSING")));
        assert!(a.shared.borrow().closing);
        assert!(!a.finish_shutdown(), "the flow in progress finishes first");
        // The long thing is done: the engine (none) is stopped and the
        // shutdown answered.
        a.current = None;
        a.drive();
        assert!(a.finish_shutdown());
        assert_eq!(done_rx.recv().unwrap(), Ok(()));
        // Whatever comes afterwards is refused.
        let (late, late_rx) = Reply::channel();
        a.handle_command(Command::Connect(late));
        assert_eq!(late_rx.recv().unwrap(), Err(message("APP_CLOSING")));
    }
    #[test]
    fn a_connect_while_leaving_is_refused_before_anything_is_read() {
        let a = actor();
        a.shared.borrow_mut().closing = true;
        assert_eq!(run_now(connect(&a.shared)), Err(message("APP_CLOSING")));
        assert!(a.shared.borrow().starting.is_none());
    }
    #[test]
    fn maintenance_is_entered_and_left_without_an_engine() {
        let a = actor();
        let owner = run_now(enter_maintenance(&a.shared)).unwrap();
        assert!(a.shared.borrow().phone.in_maintenance());
        // Held for one operation: another is refused, and cannot end it.
        assert_eq!(run_now(enter_maintenance(&a.shared)), Err(message("MAINTENANCE_IN_PROGRESS")));
        run_now(leave_maintenance(&a.shared, owner + 1));
        assert!(a.shared.borrow().phone.in_maintenance());
        run_now(leave_maintenance(&a.shared, owner));
        assert!(!a.shared.borrow().phone.in_maintenance());
    }
    #[test]
    fn an_owed_end_of_maintenance_does_not_end_one_held_by_another() {
        let a = actor();
        // An earlier owner's off was not taken: it is owed.
        a.shared.borrow_mut().maintenance_off_owed = true;
        assert!(a.shared.borrow().maintenance_off_due());
        // A new owner takes the maintenance: the owed off is superseded.
        let owner = run_now(enter_maintenance(&a.shared)).unwrap();
        assert!(!a.shared.borrow().maintenance_off_owed, "the new on supersedes the owed off");
        // Owed again while held (a retry already queued): it waits for the holder.
        a.shared.borrow_mut().maintenance_off_owed = true;
        assert!(!a.shared.borrow().maintenance_off_due());
        run_now(maintenance_off(&a.shared));
        assert!(a.shared.borrow().phone.in_maintenance(), "the holder's maintenance stands");
        assert!(a.shared.borrow().maintenance_off_owed, "the retry did nothing and is still owed");
        // Once the holder leaves, the retry goes through.
        run_now(leave_maintenance(&a.shared, owner));
        run_now(maintenance_off(&a.shared));
        assert!(!a.shared.borrow().maintenance_off_owed);
    }
    #[test]
    fn a_recording_whose_end_is_learnt_twice_is_converted_once() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        let wav = std::env::temp_dir().join(format!("ksip-once-{}.wav", std::process::id()));
        p.convert_once(wav.clone());
        p.convert_once(wav.clone());
        assert_eq!(p.converted.len(), 1);
        assert!(p.converted.contains(&wav));
    }
    #[test]
    fn a_maintenance_held_for_a_late_worker_is_released_by_its_report_only() {
        let a = actor();
        let owner = run_now(enter_maintenance(&a.shared)).unwrap();
        a.shared.borrow_mut().maintenance_held = Some(owner);
        // A connect meanwhile finds the phone held, and ends nothing.
        assert_eq!(run_now(enter_maintenance(&a.shared)), Err(message("MAINTENANCE_IN_PROGRESS")));
        assert!(a.shared.borrow().phone.in_maintenance());
        // The late report frees it, once.
        a.shared.borrow_mut().stray(Work::Calibrated(Err("late".into())));
        assert!(!a.shared.borrow().phone.in_maintenance());
        assert!(a.shared.borrow().maintenance_off_owed, "the engine is told off at the next look");
        assert!(a.shared.borrow().maintenance_held.is_none());
    }
    #[test]
    fn a_request_without_an_engine_is_refused_at_once() {
        let a = actor();
        assert_eq!(run_now(request(&a.shared, "ksip_state", "")), Err(RequestError::NotRunning));
        assert!(a.shared.borrow().waiting.is_none());
    }
}
