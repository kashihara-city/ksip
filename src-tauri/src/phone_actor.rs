//! The phone's one updater. Commands from the window and the desktop, what
//! the engine link reports, and what finished work brings back all arrive
//! on one queue and are handled here, one at a time, in order. The actor
//! owns the call bookkeeping (`PhoneState`), the engine link and the
//! snapshot the window sees; nothing outside this module writes to any of
//! them. The window reads a published copy of the snapshot, and everyone
//! else sends a `Command` through a `PhoneHandle`.
//!
//! Requests to the engine are answered on the same queue, so the actor
//! waits for an answer by taking what the link delivers ahead of it, in
//! receive order, and sets everything else aside for afterwards. Work that
//! takes long (the echo calibration, turning a recording into an MP3) runs
//! on a thread of its own and reports back as a message.
use crate::audio::{Calibration, Volume};
use crate::app::Services;
use crate::engine_config::{endpoint_notes, AudioEndpoints};
use crate::engine_link::{EngineLink, EngineReport};
use crate::logs::{stamp, LOG_APP, LOG_ENGINE, LOG_EVENT};
use crate::phone_state::{Mwi, Snapshot, Transfer};
use crate::recordings::recording_name;
use crate::settings::{dial_target, validate, CustomButton, Settings};
use crate::message::{message, message_with};
use crate::phone_message::{Command, LinkBody, LinkMessage, Message, Reply};
use crate::phone_state::{automatic_recording_target, PhoneState};
use crate::storage::Account;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the engine is asked for its state while it runs.
const POLL: Duration = Duration::from_millis(500);
/// How long an answer from the engine is waited for.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(8);

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
    let actor = Actor::new(services, initial, rx, tx.clone(), published.clone());
    thread::Builder::new()
        .name("phone".into())
        .spawn(move || actor.run())
        .expect("the phone thread starts");
    (PhoneHandle(tx), published)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

struct Actor {
    rx: Receiver<Message>,
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
    /// The generation whose exit is awaited after its connection was lost.
    watched: Option<u64>,
    /// The address the engine bound, to notice when the adapter moves.
    bound: String,
    /// The banner the polling raised, cleared once polling works again.
    polling_error: String,
    /// Operations that arrived while the phone was in maintenance, run in
    /// order once it ends; answered APP_CLOSING if the app leaves first.
    deferred: VecDeque<Command>,
    /// Messages set aside while an answer was awaited, handled next.
    stash: VecDeque<Message>,
    closing: bool,
    next_poll: Instant,
}

impl Actor {
    fn new(services: Services, view: Snapshot, rx: Receiver<Message>, tx: Sender<Message>, published: Published) -> Self {
        Self {
            rx,
            tx,
            services,
            phone: PhoneState::default(),
            view,
            published,
            link: None,
            watched: None,
            bound: String::new(),
            polling_error: String::new(),
            deferred: VecDeque::new(),
            stash: VecDeque::new(),
            closing: false,
            next_poll: Instant::now() + POLL,
        }
    }
    fn run(mut self) {
        loop {
            let message = match self.stash.pop_front() {
                Some(message) => message,
                None => {
                    let now = Instant::now();
                    if now >= self.next_poll {
                        self.tick();
                        self.next_poll = Instant::now() + POLL;
                        self.publish();
                        continue;
                    }
                    match self.rx.recv_timeout(self.next_poll - now) {
                        Ok(message) => message,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            };
            if self.handle(message) {
                self.publish();
            }
            self.run_deferred();
            if self.closing {
                break;
            }
        }
        for command in self.deferred.drain(..) {
            Self::refuse(command);
        }
        for message in self.stash.drain(..).chain(std::iter::from_fn(|| self.rx.try_recv().ok())) {
            if let Message::Command(command) = message {
                Self::refuse(command);
            }
        }
    }
    /// Answers a command once the snapshot shows what it did, so that the
    /// window's next look, right after the answer, is not behind it.
    fn answer<T>(&self, reply: Reply<T>, result: Result<T, String>) {
        self.publish();
        reply.send(result);
    }
    /// The operations that waited for the phone to be free, in order. One of
    /// them can begin maintenance again, and the rest wait for that too.
    fn run_deferred(&mut self) {
        while !self.phone.in_maintenance() && !self.closing {
            let Some(command) = self.deferred.pop_front() else {
                break;
            };
            self.handle_command(command);
            self.publish();
        }
    }
    fn publish(&self) {
        self.published.set(&self.view);
    }
    /// Answers a command that will not be carried out because the app is leaving.
    fn refuse(command: Command) {
        let closing = message("APP_CLOSING");
        match command {
            Command::Connect(reply) | Command::RefreshDevices(reply) | Command::Shutdown(reply) => reply.send(Err(closing)),
            Command::SaveConfiguration { reply, .. } | Command::SelectAudioDevice { reply, .. } => reply.send(Err(closing)),
            Command::Action { reply, .. } => reply.send(Err(closing)),
            Command::CalibrateAec { reply, .. } => reply.send(Err(closing)),
            Command::SetVolume { reply, .. } => reply.send(Err(closing)),
            Command::Initialize | Command::WindowVisible(_) | Command::ShowError(_) | Command::ReportError(_) => {}
        }
    }
    /// Whether a command works on the phone, and so waits while the phone is
    /// in maintenance.
    fn is_operation(command: &Command) -> bool {
        !matches!(
            command,
            Command::WindowVisible(_) | Command::ShowError(_) | Command::ReportError(_) | Command::Shutdown(_)
        )
    }
    /// Returns whether the snapshot may have changed.
    fn handle(&mut self, message: Message) -> bool {
        match message {
            Message::Command(command) => {
                self.handle_command(command);
                true
            }
            Message::Link(link) => self.handle_link(link),
            Message::Calibrated { result, reply } => {
                self.leave_maintenance();
                self.answer(reply, result);
                true
            }
        }
    }
    fn handle_command(&mut self, command: Command) {
        if self.phone.in_maintenance() && Self::is_operation(&command) {
            self.deferred.push_back(command);
            return;
        }
        match command {
            Command::Initialize => self.initialize(),
            Command::Connect(reply) => {
                let result = self.connect();
                self.answer(reply, result);
            }
            Command::Action { name, id, value, line, reply } => {
                let result = self.action(&name, &id, &value, line);
                self.answer(reply, result);
            }
            Command::SaveConfiguration { settings, account, reply } => {
                let result = self.save_configuration(*settings, account);
                self.answer(reply, result);
            }
            Command::SelectAudioDevice { kind, device, reply } => {
                let result = self.select_audio_device(&kind, device);
                self.answer(reply, result);
            }
            Command::RefreshDevices(reply) => {
                let result = self.refresh_devices();
                self.answer(reply, result);
            }
            Command::CalibrateAec { microphone, speaker, careful, reply } => self.calibrate_aec(microphone, speaker, careful, reply),
            Command::SetVolume { kind, device, level, mute, reply } => {
                let result = self.set_volume(&kind, &device, level, mute);
                self.answer(reply, result);
            }
            Command::WindowVisible(visible) => self.view.window_visible = visible,
            Command::ShowError(error) => self.show_error(error),
            Command::ReportError(error) => self.report_error(error),
            Command::Shutdown(reply) => {
                self.closing = true;
                for command in self.deferred.drain(..) {
                    Self::refuse(command);
                }
                let _ = self.stop_engine();
                self.answer(reply, Ok(()));
            }
        }
    }
    /// What the link delivered. Only the engine the actor holds can change
    /// the phone; the words of an earlier one are still worth their log line.
    fn handle_link(&mut self, link: LinkMessage) -> bool {
        let current = self.link.as_ref().map(EngineLink::generation) == Some(link.generation);
        match link.body {
            LinkBody::Log(text) => {
                let changed = current && self.derive_from_log(&text);
                self.services.log(LOG_ENGINE, text);
                changed
            }
            LinkBody::Event(event) => {
                if current {
                    self.event(&event);
                }
                current
            }
            // An answer nobody waits for any more: it came after its time ran out.
            LinkBody::Response { .. } => false,
            LinkBody::Lost => {
                if !current {
                    return false;
                }
                self.services.log(LOG_APP, message("ENGINE_CONTROL_LOST"));
                let engine = self.link.take().expect("the current link");
                self.close_calls_on_exit(None);
                self.watched = Some(link.generation);
                engine.watch_exit(self.tx.clone());
                true
            }
            LinkBody::Exited(status) => {
                if self.watched != Some(link.generation) {
                    return false;
                }
                self.watched = None;
                let notice = message_with("ENGINE_EXITED", [status]);
                // A reconnect may have come first; the exit is still on record.
                if self.link.is_none() {
                    self.view.error = notice.clone();
                }
                self.services.log(LOG_APP, notice);
                true
            }
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
        if s.contains("postlab: receive WAV closed") && !s.contains("0 dropped samples, error=0") {
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
    /// Once every poll interval: an engine that ended without being asked, the
    /// engine's state, and the network the engine is bound to.
    fn tick(&mut self) {
        if let Some(status) = self.link.as_mut().and_then(EngineLink::exited) {
            let engine = self.link.take().expect("the link that exited");
            let notice = message_with("ENGINE_EXITED", [status]);
            self.close_calls_on_exit(Some(notice.clone()));
            engine.close();
            self.services.log(LOG_APP, notice);
        }
        match self.poll() {
            Err(e) => self.report_error(e),
            Ok(()) => self.clear_polling_error(),
        }
        self.follow_network();
    }
    /// Sends a request and waits for its answer, taking what the link delivers
    /// ahead of it in order (events, log lines, the loss of the connection)
    /// and setting everything else aside. Returns the answer's receive number
    /// with its data, so that a state report can be placed in the order.
    fn request(&mut self, command: &str, params: &str) -> Result<(u64, String), String> {
        let engine = self.link.as_mut().ok_or_else(|| message("ENGINE_NOT_RUNNING"))?;
        let generation = engine.generation();
        let token = engine.send(command, params)?;
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        let (seq, response) = loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(message("ENGINE_RESPONSE_TIMEOUT"));
            }
            let received = match self.rx.recv_timeout(deadline - now) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => return Err(message("ENGINE_RESPONSE_TIMEOUT")),
                Err(RecvTimeoutError::Disconnected) => return Err(message("APP_CLOSING")),
            };
            match received {
                Message::Link(LinkMessage {
                    generation: g,
                    seq,
                    body: LinkBody::Response { token: t, value },
                }) if g == generation && t == token => break (seq, value),
                Message::Link(link) => {
                    let lost = link.generation == generation && matches!(link.body, LinkBody::Lost);
                    if self.handle_link(link) {
                        self.publish();
                    }
                    if lost {
                        return Err(message("ENGINE_DISCONNECTED"));
                    }
                }
                other => self.stash.push_back(other),
            }
        };
        if response["ok"] != true {
            let detail = response["data"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| message("ACTION_FAILED"));
            return Err(message_with("ENGINE_COMMAND_FAILED", [command, &detail]));
        }
        Ok((seq, response["data"].as_str().unwrap_or("").into()))
    }
    /// Asks the engine for its state and applies the report.
    fn poll(&mut self) -> Result<(), String> {
        if self.link.is_none() {
            return Ok(());
        }
        let (seq, data) = self.request("ksip_state", "")?;
        let report: EngineReport = serde_json::from_str(&data).map_err(err)?;
        // The phone state takes the report: which calls ended and what their
        // rows say, which line each call is on, which incoming calls the
        // automatic answer takes. A report received before one already
        // applied changes nothing.
        let auto_answer = self.view.settings.auto_answer;
        let generation = self.phone.generation();
        let Some(applied) = self.phone.apply_report(generation, seq, report.calls, report.dnd, auto_answer, now_secs()) else {
            return Ok(());
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
            self.services.history.lock().unwrap().add(&self.services.data, applied.ended)?;
        }
        for id in applied.answer {
            let payload =
                serde_json::to_string(&json!({"op":"answer","id":id,"value":""})).map_err(err)?;
            // A failed automatic answer is reported once; the call can still be answered manually.
            if let Err(e) = self.request("ksip_action", &payload) {
                self.services.log(LOG_APP, format!("auto answer failed {}", e));
            }
        }
        self.sync_auto_record()
    }
    /// Something that needs the phone quiet begins: only while no call is
    /// up, after a fresh look at the engine (a call that has just come in is
    /// not in the last report yet). The engine is told, so that a call
    /// arriving meanwhile is refused as busy rather than left ringing at a
    /// phone that is being worked on.
    fn enter_maintenance(&mut self) -> Result<(), String> {
        self.poll()?;
        self.phone.begin_maintenance().map_err(|()| message("CALL_IN_PROGRESS"))?;
        self.tell_maintenance("on");
        Ok(())
    }
    fn leave_maintenance(&mut self) {
        if self.phone.in_maintenance() {
            self.tell_maintenance("off");
        }
        self.phone.end_maintenance();
    }
    fn tell_maintenance(&mut self, value: &str) {
        if self.link.is_none() {
            return;
        }
        let payload = json!({"op":"maintenance","id":"","value":value}).to_string();
        if let Err(e) = self.request("ksip_action", &payload) {
            self.services.log(LOG_APP, format!("ksip: maintenance {value} not taken by the engine ({e})"));
        }
    }
    fn initialize(&mut self) {
        if let Err(e) = self.refresh_devices() {
            self.show_error(e);
        }
        if !self.view.error.is_empty() {
            return;
        }
        if self.view.account.has_password {
            if let Err(e) = self.connect() {
                self.show_error(e);
            }
        }
    }
    fn connect(&mut self) -> Result<(), String> {
        if self.closing {
            return Err(message("APP_CLOSING"));
        }
        self.enter_maintenance()?;
        let result = self.connect_engine();
        self.leave_maintenance();
        result
    }
    fn connect_engine(&mut self) -> Result<(), String> {
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
        let watched = watched.join(",");
        self.stop_engine()?;
        // The window shows what the engine is actually running with, so a
        // reconnect brings the saved settings forward as well.
        self.view.settings = settings.clone();
        self.start_engine(settings, &account)?;
        if let Err(e) = self.request("ksip_login", "") {
            let _ = self.stop_engine();
            return Err(e);
        }
        if let Err(e) = self.request("ksip_parking", &watched) {
            let _ = self.stop_engine();
            return Err(e);
        }
        Ok(())
    }
    fn start_engine(&mut self, s: Settings, account: &Account) -> Result<(), String> {
        validate(&s)?;
        self.services.logs.lock().unwrap().set_detail(s.detail_log);
        if self.link.is_some() {
            return Err(message("ENGINE_ALREADY_RUNNING"));
        }
        let prepared = self.services.prepare_start(&s, account, &self.view.devices)?;
        for note in prepared.notes {
            self.services.log(LOG_APP, note);
        }
        self.bound = prepared.address;
        self.take_endpoints(&prepared.endpoints);
        {
            let v = &mut self.view;
            v.settings = s;
            v.error.clear();
            v.aec_active = false;
            v.microphone_fallback = false;
            v.recording = false;
            v.calls.clear();
            v.transfer = Transfer::default();
            v.parking.clear();
            v.registration = "CONNECTING".into();
            v.recording_call.clear();
        }
        // A new engine: the calls of the old one are gone, and what the old
        // one may still say is told apart by generation.
        let generation = self.phone.new_engine();
        let engine = EngineLink::start(prepared.plan, generation, self.tx.clone())?;
        self.link = Some(engine);
        self.view.running = true;
        Ok(())
    }
    /// Stops the engine, if one runs. The calls still up get their rows,
    /// since the engine takes them down with it.
    fn stop_engine(&mut self) -> Result<(), String> {
        if self.link.is_some() {
            let _ = self.request("lab_stop", "");
            // Parking dialog subscriptions keep baresip's SIP stack alive.
            // Release them before quit so shutdown does not hit the kill timeout.
            let _ = self.request("ksip_shutdown", "");
        }
        if let Some(engine) = self.link.take() {
            let rows = self.phone.engine_gone(self.view.dnd, now_secs());
            if !rows.is_empty() {
                if let Err(e) = self.services.history.lock().unwrap().add(&self.services.data, rows) {
                    self.services.log(LOG_APP, e);
                }
            }
            let report = engine.stop()?;
            // How the engine went is part of the record: one that had to be
            // ended was still waiting on something, usually a SIP request the
            // server has not answered, and the window waited with it.
            if report.forced {
                self.services.log(LOG_APP, "ksip: the engine did not quit within 3 seconds and was ended".into());
            } else {
                let code = report.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                self.services.log(LOG_APP, format!("ksip: the engine quit in {} ms, exit code {code}", report.took.as_millis()));
            }
        }
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
        Ok(())
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
        if !rows.is_empty() {
            if let Err(e) = self.services.history.lock().unwrap().add(&self.services.data, rows) {
                self.services.log(LOG_APP, e);
            }
        }
        if let Some(wav) = recording {
            self.services.convert_recording(wav);
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
    /// Hands the saved microphone and speaker to the running engine for the
    /// calls from now on. The engine used to be restarted for a change of
    /// device, which meant a new registration and a second or more without a
    /// phone. The phone is in maintenance, so no call is up: the change
    /// reaches the next call and no running one.
    fn apply_audio_endpoints(&mut self, s: &Settings) -> Result<(), String> {
        let endpoints = self.services.resolve_audio_endpoints(s)?;
        self.request(
            "ksip_audio_devices",
            &format!("{},{}", endpoints.microphone, endpoints.speaker),
        )?;
        for note in endpoint_notes(s, &endpoints, &self.view.devices) {
            self.services.log(LOG_APP, note);
        }
        self.take_endpoints(&endpoints);
        Ok(())
    }
    /// Reads the devices again and, when no call is going on, gives the
    /// engine the saved ones: a device that was unplugged and put back, or
    /// a new Windows default, is only picked up that way.
    fn refresh_devices(&mut self) -> Result<(), String> {
        self.view.devices = crate::audio::devices()?;
        if self.link.is_none() {
            return Ok(());
        }
        // A saved device that has come back is taken into use, and one that
        // has gone gives way to the default; the engine is only restarted
        // when it does not take the change.
        if self.enter_maintenance().is_err() {
            return Ok(());
        }
        let taken = (|| -> Result<bool, String> {
            let settings = self.services.settings()?;
            Ok(match self.apply_audio_endpoints(&settings) {
                Ok(()) => true,
                Err(e) => {
                    self.services.log(
                        LOG_APP,
                        format!("ksip: the engine did not take the audio devices ({e}), restarting it"),
                    );
                    false
                }
            })
        })();
        self.leave_maintenance();
        // Restarting the engine would register again; someone who unregistered
        // on purpose keeps the devices they have until they connect themselves.
        if !taken? && !self.view.unregistered_by_choice {
            self.connect()?;
        }
        Ok(())
    }
    fn select_audio_device(&mut self, kind: &str, device: String) -> Result<(), String> {
        self.enter_maintenance()?;
        let outcome = (|| -> Result<bool, String> {
            if !matches!(kind, "microphone" | "speaker")
                || (device != "default"
                    && !self
                        .view
                        .devices
                        .iter()
                        .any(|entry| entry.kind == kind && entry.id == device))
            {
                return Err(message("SETTINGS_AUDIO_DEVICE_INVALID"));
            }
            let mut settings = self.services.settings()?;
            if kind == "microphone" {
                settings.microphone = device;
            } else {
                settings.speaker = device;
            }
            validate(&settings)?;
            self.services.save_settings(&settings)?;
            self.view.settings = settings.clone();
            if self.link.is_some() {
                match self.apply_audio_endpoints(&settings) {
                    Ok(()) => return Ok(true),
                    Err(e) => self.services.log(
                        LOG_APP,
                        format!("ksip: the engine did not take the audio devices ({e}), restarting it"),
                    ),
                }
            }
            Ok(false)
        })();
        self.leave_maintenance();
        if outcome? {
            return Ok(());
        }
        self.connect()
    }
    /// The calibration plays and records for a while, so it runs on a thread
    /// of its own; the phone is in maintenance until it reports back.
    fn calibrate_aec(&mut self, microphone: String, speaker: String, careful: bool, reply: Reply<Calibration>) {
        if let Err(e) = self.enter_maintenance() {
            self.answer(reply, Err(e));
            return;
        }
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = crate::audio::calibrate_aec(&microphone, &speaker, careful);
            let _ = tx.send(Message::Calibrated { result, reply });
        });
    }
    fn set_volume(&mut self, kind: &str, device: &str, level: Option<u16>, mute: Option<bool>) -> Result<Volume, String> {
        let mut result =
            crate::audio::volume(kind, device, level.map(|value| value.min(100) as u8), mute)?;
        if let Some(level) = level {
            let gain = level.max(100);
            if self.link.is_some() {
                self.request("lab_gain", &format!("{kind} {gain}"))?;
            }
            let mut settings = self.services.settings()?;
            if kind == "microphone" {
                settings.microphone_gain = gain;
            } else {
                settings.speaker_gain = gain;
            }
            validate(&settings)?;
            self.services.save_settings(&settings)?;
            self.view.settings = settings;
            result.level = level;
        } else {
            let settings = &self.view.settings;
            let gain = if kind == "microphone" {
                settings.microphone_gain
            } else {
                settings.speaker_gain
            };
            if gain > 100 {
                result.level = gain;
            }
        }
        Ok(result)
    }
    fn save_configuration(&mut self, settings: Settings, mut account: Account) -> Result<(), String> {
        self.enter_maintenance()?;
        let saved = (|| -> Result<(), String> {
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
                account.password = self.services.password_for(&account)?;
            }
            account.validate()?;
            self.services.persist_configuration(&settings, &account)?;
            self.view.settings = settings;
            self.view.account = account.public();
            self.services.apply_browser_integration();
            self.view.error.clear();
            Ok(())
        })();
        self.leave_maintenance();
        saved?;
        // Saved. Whether the engine then comes up on the new settings is a
        // separate matter, told in the window's own error line, so that the
        // dialog can close on what did succeed.
        if let Err(e) = self.connect() {
            self.report_error(e);
        }
        Ok(())
    }
    fn start_recording(&mut self, id: &str) -> Result<(), String> {
        // The folder appears next to the executable the first time something is recorded.
        let folder = self.services.data.join("recordings");
        std::fs::create_dir_all(&folder).map_err(err)?;
        let peer = self
            .phone
            .calls()
            .iter()
            .find(|call| call.id == id)
            .map(|call| call.peer.clone())
            .unwrap_or_default();
        // Two recordings within a second with the same peer are told apart by a count.
        let wanted = recording_name(&stamp(), &peer);
        let mut name = wanted.clone();
        let mut count = 1;
        while folder.join(&name).exists() {
            count += 1;
            name = wanted.replace(".wav", &format!("-{count}.wav"));
        }
        let path = folder.join(&name);
        self.request(
            "lab_record",
            &format!("{id} {}", path.to_str().ok_or(message("RECORDING_PATH_INVALID"))?),
        )?;
        self.phone.note_recording(id, &name);
        let v = &mut self.view;
        v.recording = true;
        v.recording_call = id.into();
        v.recording_path = path.to_string_lossy().into();
        Ok(())
    }
    fn stop_recording(&mut self) -> Result<(), String> {
        let recording = self.view.recording;
        if recording {
            self.request("lab_stop", "")?;
        }
        self.view.recording = false;
        self.view.recording_call.clear();
        if recording {
            self.services.convert_recording(PathBuf::from(self.view.recording_path.clone()));
        }
        Ok(())
    }
    fn sync_auto_record(&mut self) -> Result<(), String> {
        let auto_record = self.view.settings.auto_record;
        let target = automatic_recording_target(auto_record, self.phone.calls());
        let recording = self.view.recording;
        if recording && (!auto_record || self.phone.calls().is_empty()) {
            self.stop_recording()?;
            return Ok(());
        }
        if recording && target.as_deref() != Some(self.view.recording_call.as_str()) {
            self.request("lab_record_select", target.as_deref().unwrap_or("-"))?;
            // The file goes on with the call it was switched to.
            if let Some(id) = &target {
                let name = Path::new(&self.view.recording_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.phone.note_recording(id, &name);
            }
            self.view.recording_call = target.unwrap_or_default();
        } else if !recording {
            if let Some(id) = target {
                self.start_recording(&id)?;
            }
        }
        Ok(())
    }
    fn action(&mut self, name: &str, id: &str, value: &str, line: u8) -> Result<String, String> {
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
        if !(1..=2).contains(&line)
            || id.len() > 200
            || id.chars().any(char::is_control)
            || value.len() > 200
            || value.chars().any(char::is_control)
        {
            return Err(message("ACTION_ARGUMENT_INVALID"));
        }
        if name == "auto_record" {
            let enabled = match value {
                "on" => true,
                "off" => false,
                _ => return Err(message("AUTO_RECORD_ARGUMENT_INVALID")),
            };
            let mut settings = self.services.settings()?;
            settings.auto_record = enabled;
            self.services.save_settings(&settings)?;
            self.view.settings = settings;
            self.sync_auto_record()?;
            return Ok(if enabled {
                message("AUTO_RECORD_ON")
            } else {
                message("AUTO_RECORD_OFF")
            });
        }
        if name == "dnd" && !matches!(value, "on" | "off") {
            return Err(message("ACTION_ARGUMENT_INVALID"));
        }
        let calls = self.phone.calls();
        if name == "dial" && calls.iter().any(|c| c.line == line) {
            return Err(message("CALL_LINE_BUSY"));
        }
        if name == "transfer"
            && (!calls.iter().any(|c| c.id == id && c.line == 1) || !calls.iter().any(|c| c.id == value && c.line == 2))
        {
            return Err(message("TRANSFER_NEEDS_TWO_CALLS"));
        }
        // What the engine is sent: the value as it came, or, for a button,
        // the address the button was set up with.
        let mut target = value.to_string();
        if name == "blind_transfer" {
            // Only a target one of the buttons was set up with, and only a
            // call that is actually in progress.
            let wanted = CustomButton::address(value);
            let known = self
                .view
                .settings
                .buttons
                .iter()
                .find(|b| b.transfer_target() == Some(wanted))
                .and_then(|b| b.transfer_text());
            match known {
                Some(t) if calls.iter().any(|c| c.id == id && c.state == "ESTABLISHED" && !c.held) => {
                    target = t.to_string();
                }
                _ => return Err(message("BUTTON_NEEDS_CALL_AND_TARGET")),
            }
        }
        if name == "dial" {
            // The dial box, a button and a link are held to the same rule.
            target = dial_target(value)?;
        }
        let (_, result) = self.request(
            "ksip_action",
            &serde_json::to_string(&json!({"op":name,"id":id,"value":target})).map_err(err)?,
        )?;
        if name == "dnd" {
            self.services.log(LOG_APP, if value == "on" { message("DND_ON") } else { message("DND_OFF") });
        }
        if name == "unregister" {
            self.view.unregistered_by_choice = true;
        }
        if name == "dial" {
            self.phone.note_dialled(result.trim(), line);
        }
        self.poll()?;
        // An operation that went through supersedes whatever the banner said;
        // choosing a line is not an operation on the phone.
        if name != "select" {
            self.view.error.clear();
        }
        Ok(result)
    }
    /// The engine binds one address, so a new one means the binding is stale.
    /// Re-registering does not re-bind; only a restart does, and that is what
    /// the reconnect does. A call is never cut short for this.
    fn follow_network(&mut self) {
        let Ok(settings) = self.services.settings() else {
            return;
        };
        if settings.network_adapter.trim().is_empty() || self.link.is_none() {
            return;
        }
        // Unregistered on purpose: a new address changes nothing about that.
        if self.view.unregistered_by_choice {
            return;
        }
        let Ok(current) = crate::native::adapter_address(settings.network_adapter.trim()) else {
            return;
        };
        if self.bound == current {
            return;
        }
        if !self.phone.calls().is_empty() {
            self.report_error(message("NETWORK_CHANGED"));
            return;
        }
        self.services.log(
            LOG_APP,
            format!("ksip: adapter address changed, reconnecting on {current}"),
        );
        if let Err(e) = self.connect() {
            self.report_error(e);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> Actor {
        let (services, view) = Services::open();
        let (tx, rx) = mpsc::channel();
        let published = Published(Arc::new(Mutex::new(view.clone())));
        Actor::new(services, view, rx, tx, published)
    }

    #[test]
    fn polling_errors_clear_themselves_but_leave_other_errors() {
        let mut a = actor();
        a.report_error(message("TEMPORARY"));
        assert_eq!(a.view.error, "TEMPORARY");
        a.clear_polling_error();
        assert_eq!(a.view.error, "");
        a.report_error(message("TEMPORARY"));
        a.view.error = message("AUDIO_DEVICE_INIT_FAILED");
        a.clear_polling_error();
        assert_eq!(a.view.error, "AUDIO_DEVICE_INIT_FAILED");
    }
    #[test]
    fn microphone_fallback_log_updates_visible_state() {
        let mut a = actor();
        assert!(a.derive_from_log("ksip: microphone fallback active"));
        assert!(a.view.microphone_fallback);
        assert!(a.derive_from_log("ksip: microphone input recovered"));
        assert!(!a.view.microphone_fallback);
        assert!(!a.derive_from_log("ksip: something else"));
    }
    #[test]
    fn words_of_an_engine_no_longer_held_change_nothing() {
        let mut a = actor();
        // No engine is held, so no generation is current: the line is logged
        // and nothing else.
        let line = |body| LinkMessage { generation: 7, seq: 1, body };
        assert!(!a.handle_link(line(LinkBody::Log("ksip: microphone fallback active".into()))));
        assert!(!a.view.microphone_fallback);
        assert!(!a.handle_link(line(LinkBody::Event(json!({"event":true,"type":"REGISTER_OK","param":""})))));
        assert_eq!(a.view.registration, "UNCONFIGURED");
        assert!(!a.handle_link(line(LinkBody::Lost)));
        assert!(a.view.error.is_empty());
    }
    #[test]
    fn operations_wait_for_maintenance_to_end_and_run_in_order() {
        let mut a = actor();
        a.phone.begin_maintenance().unwrap();
        let (first, first_rx) = Reply::channel();
        let (second, second_rx) = Reply::channel();
        a.handle_command(Command::Action { name: "record".into(), id: String::new(), value: String::new(), line: 1, reply: first });
        a.handle_command(Command::Action { name: "answer".into(), id: String::new(), value: String::new(), line: 9, reply: second });
        assert!(first_rx.try_recv().is_err(), "nothing runs during maintenance");
        assert_eq!(a.deferred.len(), 2);
        // The window's questions are answered meanwhile.
        a.handle_command(Command::WindowVisible(false));
        assert!(!a.view.window_visible);
        a.phone.end_maintenance();
        a.run_deferred();
        assert_eq!(first_rx.recv().unwrap(), Err(message("ACTION_UNSUPPORTED")));
        assert_eq!(second_rx.recv().unwrap(), Err(message("ACTION_ARGUMENT_INVALID")));
    }
    #[test]
    fn leaving_answers_what_was_still_waiting() {
        let mut a = actor();
        a.phone.begin_maintenance().unwrap();
        let (waiting, waiting_rx) = Reply::channel();
        a.handle_command(Command::Connect(waiting));
        let (done, done_rx) = Reply::channel();
        a.handle_command(Command::Shutdown(done));
        assert_eq!(done_rx.recv().unwrap(), Ok(()));
        assert_eq!(waiting_rx.recv().unwrap(), Err(message("APP_CLOSING")));
        assert!(a.closing);
    }
    #[test]
    fn a_call_is_refused_while_the_phone_is_worked_on_and_the_engine_is_asked_first() {
        let mut a = actor();
        // Without an engine there is nothing to ask; the rule alone applies.
        assert!(a.enter_maintenance().is_ok());
        assert!(a.phone.in_maintenance());
        a.leave_maintenance();
        assert!(!a.phone.in_maintenance());
    }
}
