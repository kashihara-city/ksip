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
use crate::engine_config::{endpoint_notes, AudioEndpoints};
use crate::engine_link::{EngineLink, EngineReport, StopReport};
use crate::logs::{stamp, LOG_APP, LOG_ENGINE, LOG_EVENT};
use crate::message::{message, message_with};
use crate::phone_message::{Command, LinkBody, LinkMessage, Message, Ready, Reply, Work};
use crate::phone_state::{automatic_recording_target, Mwi, PhoneState, Snapshot, Transfer};
use crate::recordings::recording_name;
use crate::settings::{dial_target, validate, CustomButton, Settings};
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
    Work,
}
/// What arrived for it.
enum Delivered {
    Response(Result<(u64, Value), String>),
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
        reply: Reply<()>,
    },
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
            Job::Save { reply, .. } | Job::SelectDevice { reply, .. } => reply.send(Err(why)),
            Job::Action { reply, .. } => reply.send(Err(why)),
            Job::Calibrate { reply, .. } => reply.send(Err(why)),
            Job::SetVolume { reply, .. } => reply.send(Err(why)),
            Job::Poll | Job::Initialize | Job::Reconnect | Job::Recover | Job::MaintenanceOff | Job::Stop => {}
        }
    }
    /// Whether the job may be dropped for waiting too long.
    fn expires(&self) -> bool {
        !matches!(self, Job::Poll | Job::Stop | Job::Initialize | Job::Recover | Job::MaintenanceOff)
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
    /// The address the engine bound, to notice when the adapter moves.
    bound: String,
    /// A look at the adapter's address is under way.
    probing: bool,
    /// The banner the polling raised, cleared once polling works again.
    polling_error: String,
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
    next_poll: Instant,
}

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
            bound: String::new(),
            probing: false,
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
    fn finish_shutdown(&mut self) -> bool {
        let done = {
            let p = self.shared.borrow();
            p.closing && p.link.is_none() && p.lost.is_none() && p.starting.is_none() && self.current.is_none() && self.queue.is_empty()
        };
        if !done {
            return false;
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
            Message::Work(Work::Address { adapter, address }) => {
                let reconnect = self.shared.borrow_mut().follow_network(&adapter, address);
                if reconnect && !self.queue.iter().any(|(_, job)| matches!(job, Job::Reconnect)) {
                    self.queue.push_back((Instant::now(), Job::Reconnect));
                }
            }
            Message::Work(work) => {
                let mut p = self.shared.borrow_mut();
                if matches!(p.waiting, Some(Waiting::Work)) && p.delivered.is_none() {
                    p.delivered = Some(Delivered::Work(work));
                } else {
                    p.stray(work);
                }
            }
        }
    }
    /// Answers a command that will not be carried out.
    fn refuse(command: Command, why: String) {
        match command {
            Command::Connect(reply) | Command::RefreshDevices(reply) | Command::Shutdown(reply) => reply.send(Err(why)),
            Command::SaveConfiguration { reply, .. } | Command::SelectAudioDevice { reply, .. } => reply.send(Err(why)),
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
                let engine = p.link.take().expect("the link that exited");
                let notice = message_with("ENGINE_EXITED", [status]);
                p.close_calls_on_exit(Some(notice.clone()));
                engine.close();
                p.services.log(LOG_APP, notice);
            }
            if let Some(Waiting::Response { deadline, .. }) = &p.waiting {
                if Instant::now() >= *deadline && p.delivered.is_none() {
                    p.delivered = Some(Delivered::Response(Err(message("ENGINE_RESPONSE_TIMEOUT"))));
                }
            }
            p.probe_network();
        }
        if self.shared.borrow().maintenance_off_owed && !self.queue.iter().any(|(_, job)| matches!(job, Job::MaintenanceOff)) {
            self.queue.push_front((Instant::now(), Job::MaintenanceOff));
        }
        // One look at the engine per interval, after whatever runs now: a long
        // flow (a calibration) does not leave the window behind for its whole
        // duration.
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
                if let Err(e) = connect(&s).await {
                    s.borrow_mut().report_error(e);
                }
            }),
            Job::Recover => Box::pin(recover(s)),
            Job::MaintenanceOff => Box::pin(async move {
                if tell_maintenance(&s, "off").await.is_ok() {
                    s.borrow_mut().maintenance_off_owed = false;
                }
            }),
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
    fn work(&self, job: impl FnOnce() -> Work + Send + 'static) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Message::Work(job()));
        });
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
                    self.derive_from_log(&text);
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
                self.view.registration = "DISCONNECTED".into();
                if matches!(&self.waiting, Some(Waiting::Response { generation, .. }) if *generation == link.generation) && self.delivered.is_none() {
                    self.delivered = Some(Delivered::Response(Err(message("ENGINE_DISCONNECTED"))));
                }
            }
        }
    }
    /// Whether a lost engine waits to be recovered; the actor queues the job.
    fn needs_recovery(&self) -> bool {
        self.lost.is_some()
    }
    /// A report nobody waits for: a worker that outlived its flow.
    fn stray(&mut self, work: Work) {
        match work {
            Work::Started { result: Ok(ready), .. } => {
                // An engine nobody will take: ended, without waiting for it.
                thread::spawn(move || {
                    let _ = ready.link.stop();
                });
            }
            Work::Started { stopped: Some(report), .. } | Work::Stopped(report) => self.log_stop(report),
            _ => {}
        }
    }
    /// What the engine's own words say about the phone's state.
    fn derive_from_log(&mut self, s: &str) -> bool {
        let v = &mut self.view;
        let mut changed = false;
        if s.contains("Google WebRTC ADM + APM initialized (processing enabled") {
            v.aec_active = true;
            changed = true;
        }
        if s.contains("ksip: microphone fallback active") {
            v.microphone_fallback = true;
            changed = true;
        }
        if s.contains("ksip: microphone input recovered") {
            v.microphone_fallback = false;
            changed = true;
        }
        if (s.contains("ksip_audio:") || s.contains("wasapi/src:") || s.contains("wasapi/play:"))
            && s.contains("failed")
            && !s.contains("using silence")
        {
            v.error = message_with("AUDIO_DEVICE_INIT_FAILED_DETAIL", [s]);
            v.aec_active = false;
            changed = true;
        }
        if s.contains("ksip_audio_filter: receive WAV closed") && !s.contains("0 dropped samples, error=0") {
            v.error = message("RECORDING_WRITE_PROBLEM");
            changed = true;
        }
        // The engine only warns when it cannot load the trust list, and then
        // fails every TLS registration with nothing else to show for it.
        if s.contains("tls_add_ca() failed") {
            v.error = message("TRUST_STORE_UNUSABLE");
            changed = true;
        }
        changed
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
        v.calls = applied.calls;
        if v.calls.is_empty() {
            v.microphone_fallback = false;
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
        applied.answer
    }
    /// What has to happen when the engine is gone without being asked,
    /// whichever way it was noticed: the calls that were up get their history
    /// rows (their closing words never came), a recording in progress is
    /// closed into an MP3, the call bookkeeping is emptied and the window
    /// shows the phone as disconnected.
    fn close_calls_on_exit(&mut self, error: Option<String>) {
        let v = &mut self.view;
        if !v.running {
            return;
        }
        v.calls.clear();
        let recording = v.recording.then(|| PathBuf::from(v.recording_path.clone()));
        let dnd = v.dnd;
        v.running = false;
        v.recording = false;
        v.recording_call.clear();
        v.aec_active = false;
        v.microphone_fallback = false;
        v.transfer = Transfer::default();
        v.parking.clear();
        v.audio_processing_stats = None;
        v.registration = "DISCONNECTED".into();
        if let Some(error) = error {
            v.error = error;
        }
        let rows = self.phone.engine_gone(dnd, now_secs());
        self.services.add_history(rows);
        if let Some(wav) = recording {
            self.services.convert_recording(wav);
        }
    }
    /// Everything of a restart that is decided before the engine is touched:
    /// the account, the settings and the numbers to watch.
    fn prepare_restart(&mut self) -> Result<(Account, Settings, String), String> {
        // A connect is asked for (the button, a saved setting) or follows an
        // automatic reason that checked first; either way the phone is wanted
        // registered from here on.
        self.view.unregistered_by_choice = false;
        let mut account = self
            .services
            .store
            .read_account()?
            .ok_or(message("SIP_ACCOUNT_REQUIRED"))?;
        account.validate()?;
        let settings = self.services.settings()?;
        // What is stored may not have passed through the dialog (a policy, a
        // hand-edited registry, an older version's values), so it is checked
        // here as well before the engine is started with it.
        validate(&settings)?;
        // The engine takes thirty comma-separated numbers to watch, empty ones included.
        let mut watched: Vec<String> = settings.watched_numbers().iter().map(|n| n.to_string()).collect();
        watched.resize(CustomButton::COUNT, String::new());
        Ok((account, settings, watched.join(",")))
    }
    /// The window's view of a phone that is connecting, and the new
    /// generation: the calls of the old engine are gone, and what the old
    /// one may still say is told apart by generation.
    fn begin_start(&mut self, settings: &Settings) -> u64 {
        self.services.logs.lock().unwrap().set_detail(settings.detail_log);
        // The window shows what the engine is actually running with, so a
        // reconnect brings the saved settings forward as well.
        let v = &mut self.view;
        v.settings = settings.clone();
        v.error.clear();
        v.aec_active = false;
        v.microphone_fallback = false;
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
        self.bound = ready.address;
        self.take_endpoints(&ready.endpoints);
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
        let v = &mut self.view;
        v.running = false;
        v.recording = false;
        v.aec_active = false;
        v.microphone_fallback = false;
        v.calls.clear();
        v.transfer = Transfer::default();
        v.parking.clear();
        v.registration = "DISCONNECTED".into();
        v.recording_call.clear();
        let _ = self.services.logs.lock().unwrap().sync(&self.services.data);
        Some(engine)
    }
    /// How the engine went is part of the record: one that had to be ended
    /// was still waiting on something, usually a SIP request the server has
    /// not answered, and the window waited with it.
    fn log_stop(&mut self, report: Result<StopReport, String>) {
        match report {
            Ok(report) if report.forced => {
                self.services.log(LOG_APP, "ksip: the engine did not quit within 3 seconds and was ended".into());
            }
            Ok(report) => {
                let code = report.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                self.services.log(LOG_APP, format!("ksip: the engine quit in {} ms, exit code {code}", report.took.as_millis()));
            }
            Err(e) => self.services.log(LOG_APP, format!("ksip: the engine could not be stopped cleanly ({e})")),
        }
    }
    /// Writes down which endpoints the engine got; the window shows the same.
    fn take_endpoints(&mut self, endpoints: &AudioEndpoints) {
        let v = &mut self.view;
        v.microphone_id = endpoints.microphone.clone();
        v.speaker_id = endpoints.speaker.clone();
        v.microphone_missing = endpoints.microphone_missing;
        v.speaker_missing = endpoints.speaker_missing;
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
async fn request(s: &Shared, command: &str, params: &str) -> Result<(u64, String), String> {
    {
        let mut p = s.borrow_mut();
        let engine = p.link.as_mut().ok_or_else(|| message("ENGINE_NOT_RUNNING"))?;
        let generation = engine.generation();
        let token = engine.send(command, params)?;
        p.waiting = Some(Waiting::Response { token, generation, deadline: Instant::now() + RESPONSE_TIMEOUT });
        p.delivered = None;
    }
    let delivered = Await(s.clone()).await;
    s.borrow_mut().waiting = None;
    let Delivered::Response(answer) = delivered else {
        return Err(message("ENGINE_DISCONNECTED"));
    };
    let (seq, response) = answer?;
    if response["ok"] != true {
        let detail = response["data"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| message("ACTION_FAILED"));
        return Err(message_with("ENGINE_COMMAND_FAILED", [command, &detail]));
    }
    Ok((seq, response["data"].as_str().unwrap_or("").into()))
}
/// Waits for the report of the worker the flow has just started.
async fn await_work(s: &Shared) -> Result<Work, String> {
    {
        let mut p = s.borrow_mut();
        p.waiting = Some(Waiting::Work);
        p.delivered = None;
    }
    let delivered = Await(s.clone()).await;
    s.borrow_mut().waiting = None;
    match delivered {
        Delivered::Work(work) => Ok(work),
        Delivered::Response(_) => Err(message("APP_CLOSING")),
    }
}
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
async fn enter_maintenance(s: &Shared) -> Result<(), String> {
    poll(s).await?;
    s.borrow_mut().phone.begin_maintenance().map_err(|()| message("CALL_IN_PROGRESS"))?;
    // The engine takes the maintenance in one step, only while it has no
    // call: a call that came in after the look above is what its refusal
    // means, and the maintenance is then not begun.
    if let Err(e) = tell_maintenance(s, "on").await {
        s.borrow_mut().phone.end_maintenance();
        return Err(if e.contains("EBUSY") || e.contains("busy") { message("CALL_IN_PROGRESS") } else { e });
    }
    Ok(())
}
async fn leave_maintenance(s: &Shared) {
    if s.borrow().phone.in_maintenance() && tell_maintenance(s, "off").await.is_err() {
        // The engine would go on refusing calls; it is told again at the next look.
        s.borrow_mut().maintenance_off_owed = true;
    }
    s.borrow_mut().phone.end_maintenance();
}
/// Tells the engine that maintenance begins or ends. Without an engine there
/// is nothing to tell, and nothing to refuse.
async fn tell_maintenance(s: &Shared, value: &str) -> Result<(), String> {
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
    s.borrow().work(move || Work::Stopped(engine.stop()));
    let report = match await_work(&s).await {
        Ok(Work::Stopped(report)) => report,
        _ => Err(message("APP_CLOSING")),
    };
    let mut p = s.borrow_mut();
    let code = match &report {
        Ok(report) if report.forced => "killed".to_string(),
        Ok(report) => report.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
        Err(e) => e.clone(),
    };
    p.log_stop(report);
    let notice = message_with("ENGINE_EXITED", [code]);
    p.close_calls_on_exit(Some(notice.clone()));
    p.services.log(LOG_APP, notice);
}
/// Starts the engine on the saved settings, stopping the one that runs. The
/// phone is in maintenance until the start has reported.
async fn connect(s: &Shared) -> Result<(), String> {
    if s.borrow().closing {
        return Err(message("APP_CLOSING"));
    }
    enter_maintenance(s).await?;
    let result = restart(s).await;
    leave_maintenance(s).await;
    result
}
async fn restart(s: &Shared) -> Result<(), String> {
    let (account, settings, watched) = s.borrow_mut().prepare_restart()?;
    // The old engine is asked to close what it holds while it can still
    // answer; the process is the worker's to end, before the new one starts
    // on the same ports.
    let old = release_link(s).await;
    let generation = s.borrow_mut().begin_start(&settings);
    {
        let p = s.borrow();
        let (services, devices, tx) = (p.services.clone(), p.view.devices.clone(), p.tx.clone());
        p.work(move || {
            let stopped = old.map(EngineLink::stop);
            let result = services.prepare_start(&settings, &account, &devices).and_then(|prepared| {
                EngineLink::start(prepared.plan, generation, tx).map(|link| {
                    Box::new(Ready {
                        link,
                        endpoints: prepared.endpoints,
                        address: prepared.address,
                        notes: prepared.notes,
                    })
                })
            });
            Work::Started { stopped, result }
        });
    }
    let started = await_work(s).await;
    let ready = {
        let mut p = s.borrow_mut();
        p.starting = None;
        let Ok(Work::Started { stopped, result, .. }) = started else {
            return Err(message("APP_CLOSING"));
        };
        if let Some(report) = stopped {
            p.log_stop(report);
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
        Ok(_) => request(s, "ksip_parking", &watched).await.map(|_| ()),
        Err(e) => Err(e),
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
    s.borrow().work(move || Work::Stopped(engine.stop()));
    if let Ok(Work::Stopped(report)) = await_work(s).await {
        s.borrow_mut().log_stop(report);
    }
}
/// Stops the engine, if one runs.
async fn stop_engine(s: &Shared) {
    if let Some(engine) = release_link(s).await {
        stop_and_wait(s, engine).await;
    }
}
/// Reads the devices again and, when the engine runs and no call is going
/// on, gives it the saved ones: a device that was unplugged and put back, or
/// a new Windows default, is only picked up that way. A saved device that
/// has come back is taken into use, and one that has gone gives way to the
/// default; the engine is only restarted when it does not take the change.
async fn refresh_devices(s: &Shared) -> Result<(), String> {
    s.borrow().work(|| Work::Devices(crate::audio::devices()));
    let Ok(Work::Devices(devices)) = await_work(s).await else {
        return Err(message("APP_CLOSING"));
    };
    s.borrow_mut().view.devices = devices?;
    if s.borrow().link.is_none() {
        return Ok(());
    }
    if enter_maintenance(s).await.is_err() {
        return Ok(());
    }
    let settings = s.borrow().services.settings();
    let taken = match settings {
        Err(e) => {
            leave_maintenance(s).await;
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
    leave_maintenance(s).await;
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
/// Hands the saved microphone and speaker to the running engine for the
/// calls from now on. The engine used to be restarted for a change of
/// device, which meant a new registration and a second or more without a
/// phone. The phone is in maintenance, so no call is up: the change reaches
/// the next call and no running one.
async fn apply_audio_endpoints(s: &Shared, settings: &Settings) -> Result<(), String> {
    let endpoints = s.borrow().services.resolve_audio_endpoints(settings)?;
    request(s, "ksip_audio_devices", &format!("{},{}", endpoints.microphone, endpoints.speaker)).await?;
    let mut p = s.borrow_mut();
    for note in endpoint_notes(settings, &endpoints, &p.view.devices) {
        p.services.log(LOG_APP, note);
    }
    p.take_endpoints(&endpoints);
    Ok(())
}
async fn select_audio_device(s: &Shared, kind: &str, device: String) -> Result<(), String> {
    enter_maintenance(s).await?;
    let saved = {
        let mut p = s.borrow_mut();
        (|| -> Result<(Settings, bool), String> {
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
        })()
    };
    let outcome = match saved {
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
    leave_maintenance(s).await;
    match outcome {
        Err(e) => Err(e),
        Ok(true) => Ok(()),
        Ok(false) => connect(s).await,
    }
}
/// The calibration plays and records for a while, so it runs on a thread of
/// its own; the phone is in maintenance until it reports back.
async fn calibrate_aec(s: &Shared, microphone: String, speaker: String, careful: bool) -> Result<Calibration, String> {
    enter_maintenance(s).await?;
    s.borrow().work(move || Work::Calibrated(crate::audio::calibrate_aec(&microphone, &speaker, careful)));
    let result = match await_work(s).await {
        Ok(Work::Calibrated(result)) => result,
        _ => Err(message("APP_CLOSING")),
    };
    leave_maintenance(s).await;
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
async fn save_configuration(s: Shared, settings: Settings, mut account: Account, reply: Reply<()>) {
    if let Err(e) = enter_maintenance(&s).await {
        return s.borrow().answer(reply, Err(e));
    }
    let saved = {
        let mut p = s.borrow_mut();
        (|| -> Result<(), String> {
            validate(&settings)?;
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
            if account.password.is_empty() {
                account.password = p.services.password_for(&account)?;
            }
            account.validate()?;
            p.services.persist_configuration(&settings, &account)?;
            p.view.settings = settings;
            p.view.account = account.public();
            p.services.apply_browser_integration();
            p.view.error.clear();
            Ok(())
        })()
    };
    leave_maintenance(&s).await;
    match saved {
        Err(e) => s.borrow().answer(reply, Err(e)),
        Ok(()) => {
            s.borrow().answer(reply, Ok(()));
            if let Err(e) = connect(&s).await {
                s.borrow_mut().report_error(e);
            }
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
        while folder.join(&name).exists() {
            count += 1;
            name = wanted.replace(".wav", &format!("-{count}.wav"));
        }
        (folder.join(&name), name)
    };
    let text = path.to_str().ok_or(message("RECORDING_PATH_INVALID"))?.to_string();
    request(s, "ksip_record", &format!("{id} {text}")).await?;
    let mut p = s.borrow_mut();
    p.phone.note_recording(id, &name);
    let v = &mut p.view;
    v.recording = true;
    v.recording_call = id.into();
    v.recording_path = path.to_string_lossy().into();
    Ok(())
}
async fn stop_recording(s: &Shared) -> Result<(), String> {
    let recording = s.borrow().view.recording;
    if recording {
        request(s, "ksip_record_stop", "").await?;
    }
    let mut p = s.borrow_mut();
    p.view.recording = false;
    p.view.recording_call.clear();
    if recording {
        let wav = PathBuf::from(p.view.recording_path.clone());
        p.services.convert_recording(wav);
    }
    Ok(())
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
            | "cancel_transfer"
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
        if name == "transfer" && (!calls.iter().any(|c| c.id == id && c.line == 1) || !calls.iter().any(|c| c.id == value && c.line == 2)) {
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
    let payload = serde_json::to_string(&json!({"op":name,"id":id,"value":target})).map_err(err)?;
    let (_, result) = request(s, "ksip_action", &payload).await?;
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
    #[test]
    fn microphone_fallback_log_updates_visible_state() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        assert!(p.derive_from_log("ksip: microphone fallback active"));
        assert!(p.view.microphone_fallback);
        assert!(p.derive_from_log("ksip: microphone input recovered"));
        assert!(!p.view.microphone_fallback);
        assert!(!p.derive_from_log("ksip: something else"));
    }
    #[test]
    fn words_of_an_engine_no_longer_held_change_nothing() {
        let a = actor();
        let mut p = a.shared.borrow_mut();
        // No engine is held, so no generation is current: the line is logged
        // and nothing else.
        let line = |body| LinkMessage { generation: 7, seq: 1, body };
        p.handle_link(line(LinkBody::Log("ksip: microphone fallback active".into())));
        assert!(!p.view.microphone_fallback);
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
        p.handle_link(line(LinkBody::Log("Google WebRTC ADM + APM initialized (processing enabled)".into())));
        assert!(p.view.aec_active);
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
        assert!(matches!(a.shared.borrow_mut().delivered.take(), Some(Delivered::Response(Err(e))) if e == message("ENGINE_RESPONSE_TIMEOUT")));
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
        run_now(enter_maintenance(&a.shared)).unwrap();
        assert!(a.shared.borrow().phone.in_maintenance());
        run_now(leave_maintenance(&a.shared));
        assert!(!a.shared.borrow().phone.in_maintenance());
    }
    #[test]
    fn a_request_without_an_engine_is_refused_at_once() {
        let a = actor();
        assert_eq!(run_now(request(&a.shared, "ksip_state", "")), Err(message("ENGINE_NOT_RUNNING")));
        assert!(a.shared.borrow().waiting.is_none());
    }
}
