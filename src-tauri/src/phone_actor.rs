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
use crate::audio::{Calibration, Target, Volume};
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
/// How long after the last change seen in Windows' audio devices the engine
/// is handed the saved devices again (tick): the notices of one device come
/// several in a row, and WebRTC's own move off a device that went is over
/// by then.
const DEVICES_SETTLE: Duration = Duration::from_secs(1);
/// How long after Windows' word that an IPv4 address came or went the
/// phone looks again before it acts (tick, network_seen): a cable put back
/// or a lease renewed has the address back within this.
const NETWORK_SETTLE: Duration = Duration::from_secs(5);
/// How many times in a row a NetReset the engine did not take is tried
/// again, a settle apart, before the addresses are let be.
const NETWORK_RETRIES: u8 = 6;
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

/// What a look at the machine's addresses calls for (network_seen).
#[derive(Debug, PartialEq)]
enum NetworkAction {
    Nothing,
    /// An adapter is chosen and lost the address the engine bound.
    Restart,
    /// No adapter is chosen and the addresses changed.
    Reset,
}
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
    /// Windows' devices changed a second ago: the saved devices are handed
    /// to the engine again, which moves a call onto a device that is back,
    /// off one that is gone, or after a default that moved.
    FollowDevices,
    /// The microphone's mute as the window read it is told to the engine.
    TellMute,
    /// No adapter chosen and the machine's addresses changed: the engine
    /// binds its SIP transports anew and registers again (net_reset).
    NetReset,
    Calibrate {
        microphone: String,
        speaker: String,
        careful: bool,
        reply: Reply<Calibration>,
    },
    SetVolume {
        kind: String,
        level: Option<u16>,
        mute: Option<bool>,
        expected: Option<String>,
        reply: Reply<(Volume, Target)>,
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
            Job::Poll | Job::Initialize | Job::Reconnect | Job::ApplySaved | Job::Recover | Job::MaintenanceOff | Job::Stop | Job::FollowDevices | Job::TellMute | Job::NetReset => {}
        }
    }
    /// Whether the job may be dropped for waiting too long.
    fn expires(&self) -> bool {
        !matches!(self, Job::Poll | Job::Stop | Job::Initialize | Job::Recover | Job::MaintenanceOff | Job::ApplySaved | Job::FollowDevices | Job::TellMute | Job::NetReset)
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
    /// The machine's IPv4 addresses the engine was started among, while no
    /// adapter is chosen: a change in them has the engine's SIP transports
    /// bound anew (NetReset). None while no engine runs.
    net_addresses: Option<Vec<String>>,
    /// A worker is reading the machine's addresses (look_at_network), and a
    /// notice came while it was. One worker at a time.
    network_look_out: bool,
    network_look_owed: bool,
    /// Until when the phone waits before acting on an address that went or
    /// changed (NETWORK_SETTLE): the look at the deadline decides.
    network_pending: Option<Instant>,
    /// The addresses a NetReset was decided on, to become `net_addresses`
    /// once the engine has taken them; and how many times in a row the
    /// engine did not, each tried again after the settle, up to
    /// NETWORK_RETRIES.
    net_addresses_seen: Option<Vec<String>>,
    network_retries: u8,
    /// The chosen adapter has lost the address the engine bound, or the
    /// machine's addresses changed, and the window was told (the notice
    /// stands until another): what makes the address coming back something
    /// to say (network_settled), rather than every notice with it present.
    address_lost: bool,
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
    /// A worker is reading the audio devices (look_at_devices), and a notice
    /// came while it was: the list is read once more when it reports. One
    /// worker at a time, however many notices Windows sends.
    devices_look_out: bool,
    devices_look_owed: bool,
    /// When a look last found the devices changed; the engine is handed the
    /// saved devices again a second after the last change (FollowDevices),
    /// so that WebRTC's own move off a device that went, and a device still
    /// settling, are over by then.
    devices_changed_at: Option<Instant>,
    /// The microphone's mute as the window last read it off its endpoint
    /// (Command::MicrophoneMuted), and what the current engine was told.
    microphone_muted: Option<bool>,
    mute_told: Option<bool>,
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
    /// Windows' notices of audio device changes, held for as long as the
    /// actor runs (run); none when the registration failed, said in the log.
    devices_watch: Option<crate::audio::DeviceWatch>,
    /// Windows' notices of IPv4 addresses coming and going, likewise.
    network_watch: Option<crate::native::AddressWatch>,
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
    /// The same for the alert sounds' player.
    alert_failures: u64,
}
impl EngineNotices {
    /// What a report has to say that was not said: the audio module is not up,
    /// the speaker or the ringtone would not start (a microphone that would
    /// not start is the silence notice, not a banner), the trust store for
    /// TLS is empty.
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
            if audio.alert.failures > self.alert_failures {
                self.alert_failures = audio.alert.failures;
                let result = audio.alert.last_result.map(|r| r.to_string()).unwrap_or_default();
                said.push(message_with("ALERT_START_FAILED", [result]));
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
            net_addresses: None,
            network_look_out: false,
            network_look_owed: false,
            network_pending: None,
            net_addresses_seen: None,
            network_retries: 0,
            address_lost: false,
            work_serial: 0,
            exclusive: Vec::new(),
            devices_look_out: false,
            devices_look_owed: false,
            devices_changed_at: None,
            microphone_muted: None,
            mute_told: None,
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
            devices_watch: None,
            network_watch: None,
        }
    }
    fn run(mut self) {
        self.watch_devices();
        self.watch_network();
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
            Message::Work(_, Work::Address { adapter, adapter_addresses, all }) => {
                let action = {
                    let mut p = self.shared.borrow_mut();
                    p.network_look_out = false;
                    if p.network_look_owed {
                        p.network_look_owed = false;
                        p.look_at_network();
                    }
                    // Nothing to follow without an engine, or for someone
                    // who unregistered on purpose: a connect later binds
                    // anew. One exception: a start that failed for the
                    // chosen adapter having no address is made again once
                    // it has one.
                    if p.link.is_none() || p.closing || p.view.unregistered_by_choice {
                        p.network_pending = None;
                        if p.adapter_back(&adapter, &adapter_addresses) {
                            p.services.log(LOG_APP, format!("ksip: the adapter has {} now, connecting again", adapter_addresses.join(", ")));
                            NetworkAction::Restart
                        } else {
                            NetworkAction::Nothing
                        }
                    } else {
                        p.network_seen(&adapter, &adapter_addresses, all)
                    }
                };
                let job = match action {
                    NetworkAction::Nothing => None,
                    NetworkAction::Restart => Some(Job::Reconnect),
                    NetworkAction::Reset => Some(Job::NetReset),
                };
                if let Some(job) = job {
                    if !self.queue.iter().any(|(_, queued)| std::mem::discriminant(queued) == std::mem::discriminant(&job)) {
                        self.queue.push_back((Instant::now(), job));
                    }
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
            Command::Initialize | Command::WindowVisible(_) | Command::ShowError(_) | Command::ReportError(_) | Command::DevicesChanged | Command::NetworkChanged | Command::MicrophoneMuted(_) => {}
        }
    }
    /// Asks Windows for its notices of audio device changes, each of which
    /// has the list read again (Command::DevicesChanged). Without them (the
    /// registration failed) the lists follow the refresh button only, and
    /// the log says so once.
    fn watch_devices(&mut self) {
        let tx = self.shared.borrow().tx.clone();
        match crate::audio::watch_devices(move || {
            let _ = tx.send(Message::Command(Command::DevicesChanged));
        }) {
            Ok(watch) => self.devices_watch = Some(watch),
            Err(e) => self.shared.borrow().services.log(
                LOG_APP,
                format!("ksip: Windows' notices of audio device changes could not be had ({e}); the device lists follow the refresh button only"),
            ),
        }
    }
    /// Asks Windows for its notices of IPv4 addresses coming and going,
    /// each of which has the addresses read again (Command::NetworkChanged).
    /// Without them (the registration failed) an address that changes is
    /// not followed, and the log says so once.
    fn watch_network(&mut self) {
        let tx = self.shared.borrow().tx.clone();
        match crate::native::watch_addresses(move || {
            let _ = tx.send(Message::Command(Command::NetworkChanged));
        }) {
            Ok(watch) => self.network_watch = Some(watch),
            Err(e) => self.shared.borrow().services.log(
                LOG_APP,
                format!("ksip: Windows' notices of address changes could not be had ({e}); an address that changes is not followed"),
            ),
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
            Command::DevicesChanged => {
                self.shared.borrow_mut().look_at_devices();
                return;
            }
            Command::NetworkChanged => {
                self.shared.borrow_mut().look_at_network();
                return;
            }
            Command::MicrophoneMuted(muted) => {
                self.shared.borrow_mut().microphone_muted = Some(muted);
                return;
            }
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
            Command::SetVolume { kind, level, mute, expected, reply } => Job::SetVolume { kind, level, mute, expected, reply },
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
            // An address that went or changed is acted on once the settle
            // is over: looked at again then, and network_seen decides on
            // what that finds.
            if p.network_pending.is_some_and(|deadline| Instant::now() >= deadline) && !p.network_look_out {
                p.look_at_network();
            }
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
        // A second after Windows' audio devices were last seen changed
        // (the notices, look_at_devices, stray), the engine follows; and the
        // microphone's mute the window read goes to the engine once it
        // differs from what it was told. The engine is asked from its own
        // thread for none of these: asking Windows there stalled the calls'
        // audio.
        let (follow, mute) = {
            let mut p = self.shared.borrow_mut();
            let due = Phone::follow_due(p.devices_changed_at, Instant::now());
            if due && p.link.is_none() {
                // No engine to follow; a start hands it the saved devices anyway.
                p.devices_changed_at = None;
            }
            (due && p.link.is_some() && !p.phone.in_maintenance(),
             p.link.is_some() && p.microphone_muted.is_some() && p.microphone_muted != p.mute_told)
        };
        if follow && !self.queue.iter().any(|(_, job)| matches!(job, Job::FollowDevices)) {
            self.shared.borrow_mut().devices_changed_at = None;
            self.queue.push_back((Instant::now(), Job::FollowDevices));
        }
        if mute && !self.queue.iter().any(|(_, job)| matches!(job, Job::TellMute)) {
            self.queue.push_back((Instant::now(), Job::TellMute));
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
            Job::FollowDevices => Box::pin(async move { follow_devices(&s).await }),
            Job::TellMute => Box::pin(async move { tell_mute(&s).await }),
            Job::NetReset => Box::pin(async move { net_reset(&s).await }),
            Job::Calibrate { microphone, speaker, careful, reply } => Box::pin(async move {
                let result = calibrate_aec(&s, microphone, speaker, careful).await;
                s.borrow().answer(reply, result);
            }),
            Job::SetVolume { kind, level, mute, expected, reply } => Box::pin(async move {
                let result = set_volume(&s, &kind, level, mute, expected.as_deref()).await;
                s.borrow().answer(reply, result);
            }),
            Job::Reconnect => Box::pin(async move {
                // Queued when the adapter lost the address the engine bound,
                // or has one again after a start failed for its lack; it
                // waits its turn behind the flow under way, and by then the
                // reason may be gone (reconnect_still_decided). Then it is
                // not done. A call that is up is not waited for: its
                // sockets went with the address, and it cannot be saved.
                let still_wanted = {
                    let p = s.borrow();
                    p.reconnect_still_decided() && (p.link.is_some() || p.adapter_start_failed())
                };
                if !still_wanted {
                    return;
                }
                // With a call up, the restart goes without the maintenance a
                // connect takes (refused for a call): the call is on the
                // address that went, and ends with the engine.
                let in_call = !s.borrow().phone.calls().is_empty();
                if in_call {
                    s.borrow().services.log(LOG_APP, "ksip: the call on the address that went ends with the engine's restart".into());
                }
                let result = if in_call { restart(&s).await } else { connect(&s).await };
                match result {
                    Ok(()) => s.borrow_mut().network_settled(message("NETWORK_ENGINE_RESTARTED")),
                    // Shown, as the notice was: why the engine is not back
                    // stays up (the adapter without an address) until it is.
                    Err(e) => s.borrow_mut().show_error(e),
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
    /// The audio devices as Windows lists them now, for the window's lists:
    /// from the tick's look (Work::Devices, in stray) and the refresh
    /// button's (refresh_devices) alike.
    fn take_devices(&mut self, devices: Vec<crate::audio::Device>) {
        // A change is written down with the names, so that a log tells what
        // the machine had when (a headset gone in the night, a dock's hub).
        if crate::audio::devices_differ(&self.view.devices, &devices) {
            let names = |kind: &str| devices.iter().filter(|d| d.kind == kind).map(|d| d.name.as_str()).collect::<Vec<_>>().join("; ");
            self.services.log(LOG_APP, format!("ksip: audio devices now: microphones [{}], speakers [{}]", names("microphone"), names("speaker")));
        }
        self.view.devices = devices;
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
                // Logged, and that is all: every state the window shows
                // comes in the state report (apply_report), the ringtone's
                // player included.
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
            // The look at the devices a notice asked for: the lists follow,
            // and a change starts the second after which the engine is told.
            // A look that fails changes nothing. A notice that came while
            // the look was out has the list read once more.
            Work::Devices(result) => {
                self.devices_look_out = false;
                if let Ok(devices) = result {
                    if crate::audio::devices_differ(&self.view.devices, &devices) {
                        self.devices_changed_at = Some(Instant::now());
                    }
                    self.take_devices(devices);
                    self.publish();
                }
                if self.devices_look_owed {
                    self.devices_look_owed = false;
                    self.look_at_devices();
                }
            }
            _ => {}
        }
    }
    /// Windows said its devices changed (Command::DevicesChanged): the list
    /// is read on a worker, whose report comes back as a stray
    /// Work::Devices. One worker at a time: a notice while one is out is
    /// remembered, and the list read once more when it reports, so that a
    /// burst of notices (a USB headset makes a dozen) costs two looks.
    fn look_at_devices(&mut self) {
        if self.closing {
            return;
        }
        if self.devices_look_out {
            self.devices_look_owed = true;
            return;
        }
        self.devices_look_out = true;
        self.work(|| Work::Devices(crate::audio::devices()));
    }
    /// The engine is to follow the devices: a change was seen, and a second
    /// has passed since the last one.
    fn follow_due(changed_at: Option<Instant>, now: Instant) -> bool {
        changed_at.is_some_and(|at| now.duration_since(at) >= DEVICES_SETTLE)
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
        self.net_addresses = Some(ready.addresses);
        self.net_addresses_seen = None;
        self.network_retries = 0;
        self.network_pending = None;
        self.address_lost = false;
        self.link = Some(ready.link);
        self.view.running = true;
        // The new engine has not been told the microphone's mute: the tick
        // tells it what the window last read.
        self.mute_told = None;
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
    /// Windows said an IPv4 address came or went (Command::NetworkChanged),
    /// or the settle is over (tick): the machine's addresses are read on a
    /// worker, whose report comes back as Work::Address. One worker at a
    /// time, as with the devices; a notice meanwhile has them read once
    /// more when it reports.
    fn look_at_network(&mut self) {
        if self.closing {
            return;
        }
        if self.network_look_out {
            self.network_look_owed = true;
            return;
        }
        self.network_look_out = true;
        let chosen = self.view.settings.network_adapter.trim().to_string();
        self.work(move || {
            let adapters = crate::native::adapters();
            let adapter_addresses =
                adapters.iter().find(|a| a.name.eq_ignore_ascii_case(&chosen)).map(|a| a.addresses.clone()).unwrap_or_default();
            Work::Address { adapter: chosen, adapter_addresses, all: crate::native::ipv4_addresses(adapters) }
        });
    }
    /// What the addresses read say (Work::Address), and what to do about
    /// it. With an adapter chosen, the engine's SIP and RTP are bound to
    /// the address it had (`bound`). While that is still among the
    /// adapter's: nothing. Gone, the window and the log say so; then, with
    /// another address on the adapter in its place, the engine is
    /// restarted on that once NETWORK_SETTLE has passed with it still so,
    /// a call or not (a call on the address that went cannot be saved: its
    /// sockets went with it); with no address on the adapter at all,
    /// nothing is done, there being nothing to bind to: the sockets wait,
    /// alive again should the same address come back, and the next notice
    /// decides again. With no adapter chosen, the engine's SIP transports
    /// are on every address the machine had at its start: once the
    /// addresses differ from those (and there are some) and the settle has
    /// passed, the engine binds them anew (NetReset), moving a call that is
    /// up along. An address back within the settle ends the wait, said in
    /// the log.
    fn network_seen(&mut self, adapter: &str, adapter_addresses: &[String], all: Vec<String>) -> NetworkAction {
        let chosen = self.view.settings.network_adapter.trim().to_string();
        if adapter != chosen {
            // Read for an adapter no longer chosen: the next notice reads
            // for the right one.
            return NetworkAction::Nothing;
        }
        let now = Instant::now();
        if !chosen.is_empty() && !adapter_addresses.contains(&self.bound) && adapter_addresses.is_empty() {
            // Nothing to bind to: said once, and waited for.
            self.network_pending = None;
            if !self.address_lost {
                self.address_lost = true;
                self.show_error(message("NETWORK_ADDRESS_LOST"));
                self.services.log(LOG_APP, format!("ksip: the adapter has no address; its sockets wait for {} to come back, or another address to restart on", self.bound));
            }
            return NetworkAction::Nothing;
        }
        let changed = if chosen.is_empty() {
            !all.is_empty() && self.net_addresses.as_ref().is_some_and(|were| *were != all)
        } else {
            !adapter_addresses.contains(&self.bound)
        };
        match (changed, self.network_pending) {
            (false, None) => {
                // Back, or never gone; a notice up from the wait for an
                // address is answered.
                if self.address_lost {
                    self.network_settled(message("NETWORK_ADDRESS_BACK"));
                }
                NetworkAction::Nothing
            }
            (false, Some(_)) => {
                self.network_pending = None;
                self.network_settled(message("NETWORK_ADDRESS_BACK"));
                NetworkAction::Nothing
            }
            (true, None) => {
                self.network_pending = Some(now + NETWORK_SETTLE);
                // Shown, not reported: a reported banner goes with the next
                // look at the engine, which goes on answering (its link is
                // local); this one stands until the wait ends or is acted on.
                if !self.address_lost {
                    self.address_lost = true;
                    self.show_error(if chosen.is_empty() { message("NETWORK_ADDRESSES_CHANGED") } else { message("NETWORK_ADDRESS_LOST") });
                }
                NetworkAction::Nothing
            }
            (true, Some(deadline)) if now >= deadline => {
                self.network_pending = None;
                if chosen.is_empty() {
                    self.services.log(
                        LOG_APP,
                        format!("ksip: the machine's addresses are [{}] now, the engine binds its SIP transports anew", all.join(", ")),
                    );
                    self.net_addresses_seen = Some(all);
                    NetworkAction::Reset
                } else {
                    self.services.log(
                        LOG_APP,
                        format!("ksip: the adapter has {} in place of {}, the engine is restarted on it", adapter_addresses.join(", "), self.bound),
                    );
                    NetworkAction::Restart
                }
            }
            (true, Some(_)) => NetworkAction::Nothing,
        }
    }
    /// No engine runs because the start failed for the chosen adapter
    /// having no address (or not being there), and the addresses read say
    /// it has one now: the start is made again (Job::Reconnect).
    fn adapter_back(&self, adapter: &str, adapter_addresses: &[String]) -> bool {
        self.link.is_none()
            && !self.closing
            && !self.view.unregistered_by_choice
            && !adapter.is_empty()
            && adapter == self.view.settings.network_adapter.trim()
            && !adapter_addresses.is_empty()
            && self.adapter_start_failed()
    }
    /// The last start failed for the chosen adapter having no address, or
    /// not being there.
    fn adapter_start_failed(&self) -> bool {
        self.view.error == message("ADAPTER_NO_ADDRESS") || self.view.error == message("ADAPTER_NOT_FOUND")
    }
    /// Whether a NetReset decided on (network_seen) is still to be made by
    /// its turn: nobody unregistered on purpose since (the engine bound anew
    /// registers again), no adapter is chosen now (its address is followed
    /// by a restart instead), and the decision stands: a connect since read
    /// the addresses afresh and dropped it, the engine being another one.
    fn net_reset_still_decided(&self) -> bool {
        !self.closing && !self.view.unregistered_by_choice && self.view.settings.network_adapter.trim().is_empty() && self.net_addresses_seen.is_some()
    }
    /// Whether a Reconnect decided on is still to be made by its turn:
    /// nobody unregistered on purpose since, the adapter is chosen still,
    /// and the reason holds: a start to make again for the adapter having
    /// an address now, or the address the engine bound lost still. The
    /// address back since (said so in the window) ends that reason, as does
    /// a connect since, which bound anew; a wait begun since (lost again)
    /// decides afresh when it ends.
    fn reconnect_still_decided(&self) -> bool {
        !self.closing
            && !self.view.unregistered_by_choice
            && !self.view.settings.network_adapter.trim().is_empty()
            && (self.adapter_start_failed() || (self.address_lost && self.network_pending.is_none()))
    }
    /// How an address that went or changed was answered (it is back, the
    /// engine was restarted, the transports bound anew), said in the window
    /// and the log. The notices of the change stand in the window until
    /// this (phone.js, STANDING); this one goes by itself after a moment,
    /// as any other.
    fn network_settled(&mut self, outcome: String) {
        self.address_lost = false;
        self.show_error(outcome);
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
/// The engine's state, asked for and taken in (apply_report): the calls,
/// the endpoints the call streams are on, the rest of the report. What the
/// report asks of the phone (calls to answer automatically) is returned for
/// the caller to carry out. Nothing to ask without an engine. Its failure
/// (no answer in time, none that reads) is the caller's to weigh: the view
/// keeps the last report.
async fn refresh_state(s: &Shared) -> Result<Vec<String>, String> {
    if s.borrow().link.is_none() {
        return Ok(Vec::new());
    }
    let (seq, data) = request(s, "ksip_state", "").await?;
    let report: EngineReport = serde_json::from_str(&data).map_err(err)?;
    Ok(s.borrow_mut().apply_report(seq, report))
}
/// The calls the report asked to have answered automatically.
async fn auto_answer(s: &Shared, calls: Vec<String>) -> Result<(), String> {
    for id in calls {
        let payload = serde_json::to_string(&json!({"op":"answer","id":id,"value":""})).map_err(err)?;
        // A failed automatic answer is reported once; the call can still be answered manually.
        if let Err(e) = request(s, "ksip_action", &payload).await {
            s.borrow().services.log(LOG_APP, format!("auto answer failed {}", e));
        }
    }
    Ok(())
}
async fn poll(s: &Shared) -> Result<(), String> {
    let answer = refresh_state(s).await?;
    auto_answer(s, answer).await?;
    sync_auto_record(s).await
}
/// A fresh look at the engine for an operation that goes by the call's
/// endpoints: taken in, with what it asked for carried out; or not to be
/// had, which the operation decides on. Without an engine to ask, fresh
/// only while no call is left over from one (the control connection lost
/// before the engine's end was seen keeps the calls and their endpoints,
/// which say nothing about now). Calls the report had answered
/// automatically open their media, so the report taken in is the one after
/// those answers; answers that keep coming leave nothing to go by.
async fn refresh_for(s: &Shared, what: &str) -> bool {
    for _ in 0..2 {
        // Looked at before each ask, not once: the connection can go while
        // the answers are awaited, and an engine gone then leaves the calls
        // it had, which are not a report of now.
        if s.borrow().link.is_none() {
            return s.borrow().view.calls.is_empty();
        }
        match refresh_state(s).await {
            Ok(answer) if answer.is_empty() => return true,
            Ok(answer) => {
                let _ = auto_answer(s, answer).await;
            }
            Err(e) => {
                s.borrow().services.log(LOG_APP, format!("ksip: the call's devices could not be read {what} ({e})"));
                return false;
            }
        }
    }
    s.borrow().services.log(LOG_APP, format!("ksip: the call's devices kept changing {what}"));
    false
}
/// Whether a volume or mute change may be made: always while no call is up
/// (the device chosen, Windows' own); with one up (any call in the last
/// report, or a stream on an endpoint: a call whose microphone is on
/// silence has no endpoint and is a call still), only when the engine could
/// just be asked which endpoint the call is on, since the last report's may
/// be one the call has left, or none where it has one now.
fn volume_change_allowed(fresh: bool, call_up: bool) -> Result<(), String> {
    if call_up && !fresh {
        return Err(message("AUDIO_VOLUME_STATE_UNKNOWN"));
    }
    Ok(())
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
                        addresses: prepared.addresses,
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
/// The refresh button: reads the devices again for the lists (as a notice
/// from Windows does, look_at_devices) and, when the engine runs, gives it
/// the saved ones once more (a choice changed outside the window reaches it
/// this way). A device unplugged and put back, or a new Windows default,
/// needs none of this: Windows' notice has the engine follow a second later
/// (follow_devices); the button is for a notice that did not come. During a
/// call the devices are handed over as the notices' path does, with no
/// maintenance; with no call, under maintenance, and the engine is
/// restarted when it does not take them.
async fn refresh_devices(s: &Shared) -> Result<(), String> {
    s.borrow_mut().work(|| Work::Devices(crate::audio::devices()));
    let devices = match await_work(s, DEVICES_TIMEOUT).await {
        Ok(Work::Devices(devices)) => devices,
        Ok(_) => return Err(message("APP_CLOSING")),
        Err(e) => return Err(e),
    };
    s.borrow_mut().take_devices(devices?);
    if s.borrow().link.is_none() {
        return Ok(());
    }
    let owner = match enter_maintenance(s).await {
        Ok(owner) => owner,
        Err(e) if e == message("CALL_IN_PROGRESS") => {
            let settings = s.borrow().services.settings()?;
            return apply_audio_endpoints(s, &settings).await;
        }
        Err(_) => return Ok(()),
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
/// Windows' devices changed a second ago (Job::FollowDevices): the saved
/// devices go to the engine again, as a choice made in the window does. The
/// engine takes a stream that still serves its device as it is, and opens
/// the others anew: a call on the default in place of a device that is back
/// goes back to it, one on a device that went goes to the default (or, with
/// no device at all, the microphone to silence), and one asked to be on the
/// default follows the default when that moved. Nothing of this is decided
/// here, and no device is looked at from the engine's own thread.
async fn follow_devices(s: &Shared) {
    if s.borrow().link.is_none() {
        return;
    }
    let settings = match s.borrow().services.settings() {
        Ok(settings) => settings,
        Err(e) => {
            s.borrow().services.log(LOG_APP, format!("ksip: the devices changed, but the settings could not be read ({e})"));
            return;
        }
    };
    if let Err(e) = apply_audio_endpoints(s, &settings).await {
        s.borrow().services.log(LOG_APP, format!("ksip: the devices changed, but the engine did not take them ({e})"));
    }
}
/// The microphone's mute as the window last read it (Command::MicrophoneMuted)
/// goes to the engine (Job::TellMute), which mutes the calls' audio while it
/// is so. What was told is remembered for the engine it was told to, so that
/// a change, or a new engine, is what has it told again.
async fn tell_mute(s: &Shared) {
    let Some(muted) = s.borrow().microphone_muted else {
        return;
    };
    if s.borrow().link.is_none() {
        return;
    }
    match request(s, "ksip_audio_mute", if muted { "on" } else { "off" }).await {
        Ok(_) => s.borrow_mut().mute_told = Some(muted),
        Err(e) => {
            // Not asked again until the mute changes or the engine does:
            // an engine that does not know the command would be asked
            // every half second otherwise.
            let mut p = s.borrow_mut();
            p.mute_told = Some(muted);
            p.services.log(LOG_APP, format!("ksip: the engine did not take the microphone's mute ({e})"));
        }
    }
}
/// No adapter chosen and the machine's addresses changed (Job::NetReset):
/// the engine reads its local addresses again, binds its SIP transports on
/// them and registers again, and tells a call that is up the new address
    /// (a re-INVITE) when the route to its peer leaves from another one now.
/// An engine that does not take it (an address still held by a request in
/// flight, with fixed ports; no way to a call's peer yet) is asked again
/// after a settle, up to NETWORK_RETRIES times, and then restarted when no
/// call is up (a restart would be refused for one).
async fn net_reset(s: &Shared) {
    // Decided on earlier; by its turn the reason may be gone
    // (net_reset_still_decided). Then it is not done.
    if s.borrow().link.is_none() || !s.borrow().net_reset_still_decided() {
        return;
    }
    match request(s, "ksip_net_reset", "").await {
        Ok(_) => {
            let mut p = s.borrow_mut();
            if let Some(taken) = p.net_addresses_seen.take() {
                p.net_addresses = Some(taken);
            }
            p.network_retries = 0;
            p.network_settled(message("NETWORK_RESET_DONE"));
        }
        Err(e) => {
            let again = {
                let mut p = s.borrow_mut();
                p.network_retries += 1;
                if p.network_retries < NETWORK_RETRIES {
                    // The addresses it was decided on stay unmet: the look
                    // after the settle finds them changed still and asks again.
                    p.network_pending = Some(Instant::now() + NETWORK_SETTLE);
                    true
                } else {
                    p.network_retries = 0;
                    if let Some(given_up) = p.net_addresses_seen.take() {
                        p.net_addresses = Some(given_up);
                    }
                    // The standing notice is answered: the window would show
                    // it until the next change otherwise.
                    p.network_settled(message("NETWORK_RESET_FAILED"));
                    false
                }
            };
            let in_call = !s.borrow().phone.calls().is_empty();
            let then = match (again, in_call) {
                (true, _) => "; asked again after the wait",
                (false, true) => "; left as it is, the next change asks again",
                (false, false) => ", restarting it",
            };
            s.borrow().services.log(LOG_APP, format!("ksip: the engine did not take the network reset ({e}){then}"));
            if again || in_call {
                return;
            }
            // Not against an unregister made while the engine was asked.
            if s.borrow().closing || s.borrow().view.unregistered_by_choice {
                return;
            }
            if let Err(e) = connect(s).await {
                s.borrow_mut().show_error(e);
            }
        }
    }
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
    {
        let p = s.borrow();
        for note in endpoint_notes(settings, &endpoints, &p.view.devices) {
            p.services.log(LOG_APP, note);
        }
        if let Some(note) = switch_note(&answer) {
            p.services.log(LOG_APP, note);
        }
    }
    // The endpoints the call is on now, from the engine's state, before the
    // next command is taken: a volume or mute change queued right behind
    // the choice must see the endpoint the call moved to, not the one of
    // the last periodic look (and makes no change while it cannot).
    refresh_for(s, "after the choice of devices").await;
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
    let microphone = crate::audio::resolve("microphone", &microphone)?;
    let speaker = crate::audio::resolve("speaker", &speaker)?;
    // The result says which endpoints were measured, by the names the
    // window lists, so that a delay measured on a stand-in is seen as such.
    let named = |id: &str| {
        let p = s.borrow();
        p.view.devices.iter().find(|d| d.id == id).map_or_else(|| id.to_string(), |d| d.name.clone())
    };
    let (microphone_name, speaker_name) = (named(&microphone.id), named(&speaker.id));
    let stand_in = microphone.stand_in || speaker.stand_in;
    let (microphone, speaker) = (microphone.id, speaker.id);
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
    result.map(|calibration| Calibration { microphone: microphone_name, speaker: speaker_name, stand_in, ..calibration })
}
/// A volume or mute change for the endpoint in use for the kind at this
/// moment, as the phone's own view has it (the engine's last report), not as
/// the window saw it when it asked: a change meant for another endpoint
/// than the one in use now (`expected`) is not made, and what is in use is
/// answered along with its volume.
async fn set_volume(s: &Shared, kind: &str, level: Option<u16>, mute: Option<bool>, expected: Option<&str>) -> Result<(Volume, Target), String> {
    // A fresh look at the engine first: the call's endpoint can have moved
    // since the last report (a device chosen, the engine's own move to the
    // default or back), and the periodic look may not have caught up. A
    // call whose endpoint cannot be confirmed now has no change made.
    let fresh = refresh_for(s, "before the volume change").await;
    let (choice, call, call_up) = {
        let p = s.borrow();
        let (choice, call) = p.view.audio_choice(kind);
        let call_up = !p.view.calls.is_empty() || call.is_some();
        (choice, call, call_up)
    };
    volume_change_allowed(fresh, call_up)?;
    let target = crate::audio::target(kind, &choice, call.as_ref(), true)?;
    let (level, mute) = crate::audio::volume_change_for(&target, expected, level, mute);
    let mut result = crate::audio::volume(kind, &target.id, level.map(|value| value.min(100) as u8), mute)?;
    let Some(level) = level else {
        let p = s.borrow();
        let gain = if kind == "microphone" { p.view.settings.microphone_gain } else { p.view.settings.speaker_gain };
        if gain > 100 {
            result.level = gain;
        }
        return Ok((result, target));
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
    Ok((result, target))
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
    // A volume or mute change during a call is made only when the engine
    // could just be asked which endpoint the call is on; without a call the
    // device chosen is Windows' own, engine or no engine.
    #[test]
    fn a_volume_change_during_a_call_needs_a_fresh_look_at_the_engine() {
        assert_eq!(volume_change_allowed(false, true), Err(message("AUDIO_VOLUME_STATE_UNKNOWN")), "the state could not be read: not changed");
        assert!(volume_change_allowed(true, true).is_ok(), "read: changed on the endpoint it says");
        assert!(volume_change_allowed(false, false).is_ok(), "no call: the device chosen, whatever the engine");
        // Without an engine there is nothing to ask and nothing stale, as
        // long as no call is left over from one: the control connection
        // lost before the engine's end was seen keeps the calls, and those
        // say nothing about now.
        let a = actor();
        assert_eq!(run_now(refresh_state(&a.shared)), Ok(Vec::new()));
        assert!(run_now(refresh_for(&a.shared, "in the test")), "no engine, no call: fresh");
        let call: crate::phone_state::CallInfo = serde_json::from_value(json!({"id": "1", "peer": "sip:1002@pbx.example", "state": "ESTABLISHED", "held": false, "duration": 3})).unwrap();
        a.shared.borrow_mut().view.calls.push(call);
        assert!(!run_now(refresh_for(&a.shared, "in the test")), "no engine, a call left over: not fresh");
        let p = a.shared.borrow();
        assert!(!p.view.calls.is_empty() && p.view.microphone_call.is_none(), "a call with no endpoint of its own is a call still");
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
        // The ring tone's player included: its failures come in the report.
        p.handle_link(line("wasapi/play: IAudioClient_Initialize failed (0x88890008)"));
        assert!(p.view.error.is_empty() && !p.view.aec_active);
    }
    #[test]
    fn a_ringtone_that_would_not_start_is_said_once_from_the_report() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        let report = |failures: u64| {
            report_with(
                json!({"ready": true, "processing": true,
                       "microphone": {"input": "none", "failures": 0},
                       "speaker": {"playing": false, "failures": 0},
                       "alert": {"playing": true, "endpoint": "{spk}", "stand_in": true, "failures": failures, "last_result": -3}}),
                None,
            )
        };
        p.apply_report(1, report(0));
        assert!(p.view.error.is_empty());
        p.apply_report(2, report(1));
        assert_eq!(p.view.error, message_with("ALERT_START_FAILED", ["-3"]));
        p.view.error.clear();
        p.apply_report(3, report(1));
        assert!(p.view.error.is_empty(), "a failure already said is not said again");
        p.apply_report(4, report(2));
        assert_eq!(p.view.error, message_with("ALERT_START_FAILED", ["-3"]), "a later one is");
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
        assert!(p.view.error.is_empty(), "the words of an engine being started are logged and no more");
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
    /// The tick's look at the devices: a change starts the second after
    /// which the engine is handed the saved devices again; the same lists
    /// seen again start nothing; and with no engine, the change is let go.
    #[test]
    fn a_change_in_the_devices_is_followed_a_second_later_while_an_engine_runs() {
        let mut a = actor();
        let d = |id: &str, name: &str, kind: &str, default: bool| crate::audio::Device { id: id.into(), name: name.into(), kind: kind.into(), default };
        let mic = d("{m1}", "Mic", "microphone", true);
        let spk = d("{s1}", "Speaker", "speaker", true);
        let mut p = a.shared.borrow_mut();
        p.stray(Work::Devices(Ok(vec![mic.clone(), spk.clone()])));
        let first = p.devices_changed_at.expect("the first look differs from the empty lists");
        assert_eq!(p.view.devices.len(), 2, "the lists follow");
        p.stray(Work::Devices(Ok(vec![spk.clone(), mic.clone()])));
        assert_eq!(p.devices_changed_at, Some(first), "the same devices in another order are no change");
        p.stray(Work::Devices(Err("no COM".into())));
        assert_eq!(p.view.devices.len(), 2, "a look that fails changes nothing");
        // The default moved to another speaker: a change, as a device would be.
        p.stray(Work::Devices(Ok(vec![mic.clone(), d("{s1}", "Speaker", "speaker", false), d("{s2}", "HDMI", "speaker", true)])));
        let moved = p.devices_changed_at.unwrap();
        assert!(moved >= first);
        assert!(!Phone::follow_due(Some(moved), moved + DEVICES_SETTLE / 2), "not before the second is over");
        assert!(Phone::follow_due(Some(moved), moved + DEVICES_SETTLE), "due once it is");
        assert!(!Phone::follow_due(None, moved + DEVICES_SETTLE), "nothing changed, nothing due");
        // With no engine the change is let go: a start hands the engine the
        // saved devices anyway.
        p.devices_changed_at = Some(Instant::now() - DEVICES_SETTLE);
        drop(p);
        a.tick();
        assert!(a.shared.borrow().devices_changed_at.is_none());
        assert!(!a.queue.iter().any(|(_, job)| matches!(job, Job::FollowDevices)));
    }
    /// Windows' notices of a change have the list read on one worker at a
    /// time: a burst of notices is two looks, the second after the first
    /// reports, whatever that report was.
    #[test]
    fn a_burst_of_notices_is_two_looks() {
        let mut a = actor();
        a.handle_command(Command::DevicesChanged);
        a.handle_command(Command::DevicesChanged);
        a.handle_command(Command::DevicesChanged);
        assert!(a.shared.borrow().devices_look_out && a.shared.borrow().devices_look_owed, "one look out, one owed");
        a.shared.borrow_mut().stray(Work::Devices(Err("no COM".into())));
        assert!(a.shared.borrow().devices_look_out && !a.shared.borrow().devices_look_owed, "the owed look is out now");
        a.shared.borrow_mut().stray(Work::Devices(Ok(vec![])));
        assert!(!a.shared.borrow().devices_look_out, "nothing owed: no more looks");
        // Closing: no look is started.
        a.shared.borrow_mut().closing = true;
        a.shared.borrow_mut().look_at_devices();
        assert!(!a.shared.borrow().devices_look_out);
    }
    /// With an adapter chosen, the engine is restarted only once another
    /// address has stood in for the one it bound for the settle; the bound
    /// one back within it ends the wait with the notice. Another address
    /// beside it is no change, and no address at all is waited for with
    /// the notice up, there being nothing to restart on.
    #[test]
    fn an_adapter_that_lost_the_bound_address_for_another_restarts_the_engine_on_it() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        p.view.settings.network_adapter = "{nic}".into();
        p.bound = "192.0.2.5".into();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.5", "192.0.2.6"]), vec![]), NetworkAction::Nothing, "still there, with another");
        assert_eq!(p.network_seen("{other}", &s(&[]), vec![]), NetworkAction::Nothing, "read for another adapter: ignored");
        // No address at all: the notice, and a wait with no deadline.
        assert_eq!(p.network_seen("{nic}", &s(&[]), vec![]), NetworkAction::Nothing, "no address: nothing to restart on");
        assert!(p.network_pending.is_none() && p.view.error == message("NETWORK_ADDRESS_LOST"));
        assert_eq!(p.network_seen("{nic}", &s(&[]), vec![]), NetworkAction::Nothing, "still none: nothing more");
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.5"]), vec![]), NetworkAction::Nothing, "the same address back: carried on");
        assert_eq!(p.view.error, message("NETWORK_ADDRESS_BACK"), "the notice is answered");
        // Another address in its place: the wait, then the restart.
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.6"]), vec![]), NetworkAction::Nothing, "another address: the wait starts");
        assert!(p.network_pending.is_some() && p.view.error == message("NETWORK_ADDRESS_LOST"));
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.6"]), vec![]), NetworkAction::Nothing, "the wait not over");
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.5"]), vec![]), NetworkAction::Nothing, "back within the wait");
        assert!(p.network_pending.is_none() && p.view.error == message("NETWORK_ADDRESS_BACK"), "the wait ends and the notice is answered");
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.6"]), vec![]), NetworkAction::Nothing);
        p.network_pending = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.6"]), vec![]), NetworkAction::Restart, "another address for the settle: restarted");
        assert!(p.network_pending.is_none());
    }
    /// A start that failed for the chosen adapter having no address is made
    /// again once the adapter has one; not for other failures, nor for
    /// another adapter.
    #[test]
    fn an_adapter_with_an_address_again_has_the_start_made_again() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        p.view.settings.network_adapter = "{nic}".into();
        let one = vec!["192.0.2.5".to_string()];
        assert!(!p.adapter_back("{nic}", &one), "no failure to mend");
        p.view.error = message("ADAPTER_NO_ADDRESS");
        assert!(p.adapter_back("{nic}", &one));
        assert!(!p.adapter_back("{nic}", &[]), "still without an address");
        assert!(!p.adapter_back("{other}", &one), "read for another adapter");
        p.view.error = message("ADAPTER_NOT_FOUND");
        assert!(p.adapter_back("{nic}", &one), "an adapter that is there again, with an address");
        p.view.error = message("REGISTER_FAILED");
        assert!(!p.adapter_back("{nic}", &one), "another failure is not mended by an address");
    }
    /// With no adapter chosen, the engine binds its transports anew once the
    /// machine's addresses have differed from those at its start for the
    /// settle; the same set again, in any order, is no change.
    #[test]
    fn a_change_in_the_machines_addresses_for_the_settle_resets_the_transports() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(p.network_seen("", &[], s(&["192.0.2.5"])), NetworkAction::Nothing, "no engine started yet: no baseline");
        p.net_addresses = Some(s(&["192.0.2.5", "198.51.100.2"]));
        assert_eq!(p.network_seen("", &[], s(&["192.0.2.5", "198.51.100.2"])), NetworkAction::Nothing, "the same");
        assert_eq!(p.network_seen("", &[], s(&[])), NetworkAction::Nothing, "no address at all: nothing to bind to, waited for");
        assert!(p.network_pending.is_none());
        assert_eq!(p.network_seen("", &[], s(&["192.0.2.7", "198.51.100.2"])), NetworkAction::Nothing, "changed: the wait starts");
        assert!(p.network_pending.is_some() && p.view.error == message("NETWORK_ADDRESSES_CHANGED"));
        p.network_pending = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(p.network_seen("", &[], s(&["192.0.2.7", "198.51.100.2"])), NetworkAction::Reset, "changed for the settle: reset");
        assert_eq!(p.net_addresses_seen.as_deref(), Some(&s(&["192.0.2.7", "198.51.100.2"])[..]), "the new addresses wait to become the baseline");
        assert_eq!(p.net_addresses.as_deref(), Some(&s(&["192.0.2.5", "198.51.100.2"])[..]), "until the engine has taken them");
        assert!(p.network_pending.is_none());
        // Not taken: asked again after the settle, which finds them changed still.
        p.network_pending = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(p.network_seen("", &[], s(&["192.0.2.7", "198.51.100.2"])), NetworkAction::Reset, "asked again");
    }
    /// A reset or restart decided on waits its turn behind the flow under
    /// way; by then its reason may be gone, and it is not made: the person
    /// unregistered, an adapter was chosen, the address is back (said so),
    /// or a connect since bound anew.
    #[test]
    fn a_reset_or_restart_decided_on_is_not_made_once_its_reason_is_gone() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // No adapter chosen: a reset decided on.
        p.net_addresses = Some(s(&["192.0.2.5"]));
        p.network_pending = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(p.network_seen("", &[], s(&["192.0.2.7"])), NetworkAction::Reset);
        assert!(p.net_reset_still_decided(), "decided, nothing since");
        p.view.unregistered_by_choice = true;
        assert!(!p.net_reset_still_decided(), "unregistered since: bound anew, the engine would register again");
        p.view.unregistered_by_choice = false;
        p.view.settings.network_adapter = "{nic}".into();
        assert!(!p.net_reset_still_decided(), "an adapter chosen since: its address is followed, not the machine's");
        p.view.settings.network_adapter.clear();
        p.net_addresses_seen = None;
        assert!(!p.net_reset_still_decided(), "a connect since read the addresses afresh: the decision went with the old engine");
        // An adapter chosen: a restart decided on, then the address is back.
        p.view.settings.network_adapter = "{nic}".into();
        p.bound = "192.0.2.5".into();
        p.address_lost = true;
        p.network_pending = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.6"]), vec![]), NetworkAction::Restart);
        assert!(p.reconnect_still_decided(), "decided, nothing since");
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.5"]), vec![]), NetworkAction::Nothing, "the address back before the restart's turn");
        assert_eq!(p.view.error, message("NETWORK_ADDRESS_BACK"));
        assert!(!p.reconnect_still_decided(), "back: a call on it goes on, the engine is not restarted");
        assert_eq!(p.network_seen("{nic}", &s(&["192.0.2.6"]), vec![]), NetworkAction::Nothing, "lost again: a new wait");
        assert!(!p.reconnect_still_decided(), "the wait decides afresh when it ends");
        p.network_pending = None;
        assert!(p.reconnect_still_decided());
        p.view.unregistered_by_choice = true;
        assert!(!p.reconnect_still_decided(), "unregistered since");
        p.view.unregistered_by_choice = false;
        p.address_lost = false;
        assert!(!p.reconnect_still_decided(), "a connect since bound anew");
        p.view.error = message("ADAPTER_NO_ADDRESS");
        assert!(p.reconnect_still_decided(), "a start to make again stands on its own");
    }
    /// The microphone's mute the window read is kept for the engine, and
    /// told only while one runs and it differs from what that one was told.
    #[test]
    fn the_microphone_mute_waits_for_an_engine_to_tell() {
        let mut a = actor();
        a.handle_command(Command::MicrophoneMuted(true));
        assert_eq!(a.shared.borrow().microphone_muted, Some(true));
        a.tick();
        assert!(!a.queue.iter().any(|(_, job)| matches!(job, Job::TellMute)), "no engine: nobody to tell");
        // Told (as the job would record), the same mute is not queued again.
        a.shared.borrow_mut().mute_told = Some(true);
        assert_eq!(a.shared.borrow().microphone_muted, a.shared.borrow().mute_told);
        a.handle_command(Command::MicrophoneMuted(false));
        assert_ne!(a.shared.borrow().microphone_muted, a.shared.borrow().mute_told, "a change is what has it told again");
    }
}
