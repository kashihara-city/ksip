//! What holds the app together: the services that are not the phone (the
//! files, the store, the log, the history, the conversions), and the state
//! the window side is given, which only connects: a way to send commands to
//! the phone actor and a way to read the snapshot it publishes.
use crate::history::{CallHistory, History};
use crate::logs::{data_dir, keep_last_lines, stamp, LogLine, Logs, LOG_APP, LOG_FILE};
use crate::message::{message, message_with};
use crate::phone_actor::{PhoneHandle, Published};
use crate::phone_message::Command;
use std::sync::OnceLock;
use crate::phone_state::{Mwi, Snapshot, Transfer};
use crate::settings::{Settings, SAVE_MARK};
use crate::storage::{Account, Store};
use std::{
    path::{Path, PathBuf},
    process::Command as Process,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
/// The parts of the app that are not the phone: where the files go, the
/// registry and the vault, the log, the call history and the recordings
/// being converted. Shared by the window side (`AppState`) and the phone
/// actor; none of it is phone state, and none of it changes the phone.
#[derive(Clone)]
pub struct Services {
    pub data: PathBuf,
    pub store: Store,
    pub logs: Arc<Mutex<Logs>>,
    pub history: Arc<Mutex<History>>,
    /// Recordings being turned into MP3 at the moment.
    pub converting: Arc<AtomicU64>,
    /// How far the log and the history have moved, readable without their
    /// locks: the window's snapshot reads these while the file thread may be
    /// on the disk.
    pub log_sequence: Arc<AtomicU64>,
    pub history_sequence: Arc<AtomicU64>,
    /// The thread that writes the files that must keep their order (the
    /// history) and keeps the log and the history files moving, so that
    /// neither the phone nor the window waits on the disk.
    files: Sender<FileJob>,
    /// The way to the phone, once it exists: what the files thread has to
    /// say to the window goes through it.
    phone: Arc<OnceLock<PhoneHandle>>,
}
/// What the file thread is asked to do, in the order asked.
enum FileJob {
    History(Vec<CallHistory>),
    /// The history is emptied, on disk first; answered once done.
    Clear(Sender<Result<(), String>>),
    /// The log is emptied, tab and file; answered once done. On this thread
    /// so that no sync is out on the disk while it happens.
    ClearLogs(Sender<()>),
    /// The last lines and rows are written and the thread ends; answered
    /// with the first failure, if any.
    Stop(Sender<Result<(), String>>),
}
/// What the file thread holds: the files and the way to the window, not the
/// whole of the services (which would hold its own queue open).
struct FileWorker {
    data: PathBuf,
    logs: Arc<Mutex<Logs>>,
    history: Arc<Mutex<History>>,
    log_sequence: Arc<AtomicU64>,
    history_sequence: Arc<AtomicU64>,
    phone: Arc<OnceLock<PhoneHandle>>,
}
impl FileWorker {
    fn log(&self, text: String) {
        let mut logs = self.logs.lock().unwrap();
        logs.push(LogLine::new(stamp(), LOG_APP, text));
        self.log_sequence.store(logs.sequence(), Ordering::Relaxed);
    }
    fn show_error(&self, error: String) {
        match self.phone.get() {
            Some(phone) => phone.send(Command::ShowError(error)),
            None => self.log(error),
        }
    }
    /// Once a second, and at the end: the log is written from log(), so a
    /// quiet moment would leave the last lines only in memory, and what
    /// another process appended unseen; rows the history could not write
    /// earlier get another try. The first failure of either file is said
    /// once, in the log and the window, and returned. The log's lock is
    /// held only to plan and to book the sync, never while the disk is
    /// touched, so that the actor's log() never waits on the disk.
    fn move_files_along(&self, force: bool) -> Result<(), String> {
        let plan = {
            let mut logs = self.logs.lock().unwrap();
            (force || logs.due()).then(|| logs.plan_sync())
        };
        let mut journal = Ok(());
        if let Some(plan) = plan {
            let outcome = Logs::perform_sync(&self.data, &plan);
            let (booked, limit) = {
                let mut logs = self.logs.lock().unwrap();
                let booked = logs.commit_sync(plan, outcome);
                self.log_sequence.store(logs.sequence(), Ordering::Relaxed);
                (booked, logs.file_limit())
            };
            match booked {
                Ok(true) => {
                    if let Ok((length, lines)) = keep_last_lines(&self.data.join(LOG_FILE), limit) {
                        self.logs.lock().unwrap().file_shortened(length, lines);
                    }
                }
                Ok(false) => {}
                Err(failure) => {
                    // A failure is a failure every time, for whoever asks (the
                    // Stop at the end); the person hears of it once.
                    if failure.first {
                        self.show_error(message_with("JOURNAL_WRITE_FAILED", [&failure.error]));
                    }
                    journal = Err(failure.error);
                }
            }
        }
        let history = {
            let mut history = self.history.lock().unwrap();
            let result = history.flush(&self.data);
            self.history_sequence.store(history.sequence(), Ordering::Relaxed);
            result
        };
        if let Err(e) = &history {
            self.show_error(message_with("JOURNAL_WRITE_FAILED", [e]));
        }
        journal.and(history)
    }
    fn run(self, jobs: Receiver<FileJob>) {
        loop {
            match jobs.recv_timeout(Duration::from_secs(1)) {
                Ok(FileJob::History(rows)) => {
                    // A row that cannot be written waits in the history for the
                    // next try; the failure is on record here, and the window
                    // hears of it from the next flush.
                    let result = {
                        let mut history = self.history.lock().unwrap();
                        let result = history.add(&self.data, rows);
                        self.history_sequence.store(history.sequence(), Ordering::Relaxed);
                        result
                    };
                    if let Err(e) = result {
                        self.log(e);
                    }
                }
                Ok(FileJob::ClearLogs(reply)) => {
                    {
                        let mut logs = self.logs.lock().unwrap();
                        logs.clear(&self.data);
                        self.log_sequence.store(logs.sequence(), Ordering::Relaxed);
                    }
                    let _ = reply.send(());
                }
                Ok(FileJob::Clear(reply)) => {
                    let result = {
                        let mut history = self.history.lock().unwrap();
                        let result = history.clear(&self.data);
                        self.history_sequence.store(history.sequence(), Ordering::Relaxed);
                        result
                    };
                    let _ = reply.send(result);
                }
                Ok(FileJob::Stop(reply)) => {
                    // The last word says what is left unwritten, if anything.
                    let result = self.move_files_along(true);
                    let owed = self.logs.lock().unwrap().owed();
                    let _ = reply.send(match result {
                        Err(e) if owed > 0 => Err(format!("{e} ({owed} log lines unwritten)")),
                        other => other,
                    });
                    return;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let _ = self.move_files_along(false);
        }
    }
}
impl Services {
    /// Opens the store, the log and the history, and makes the snapshot the
    /// app starts with. Nothing here may panic: the panic hook is only
    /// installed once this exists.
    pub fn open() -> (Self, Snapshot) {
        let store = Store::new();
        let data = data_dir(&store);
        let mut logs = Logs::open(&data);
        // Earlier versions kept most settings as one JSON document in the value
        // `Settings`. It is not read any more, only removed: every setting now
        // has a value of its own.
        let _ = store.delete_value("Settings");
        let (settings, unreadable) = Settings::read_stored(|name| store.read_effective(name));
        let mut startup_error = if let Some(e) = store.policy.error() {
            message_with("POLICY_UNREADABLE", [e])
        } else if unreadable.is_empty() {
            String::new()
        } else {
            message_with("SETTINGS_VALUE_INVALID", [unreadable.join(", ")])
        };
        logs.set_detail(settings.detail_log);
        let account = match store.read_account() {
            Ok(Some(a)) => a.public(),
            Ok(None) => Account::default().public(),
            Err(e) => {
                startup_error = e;
                Account::default().public()
            }
        };
        // A save that did not finish (the process ended in the middle, or a
        // value could not be put back) leaves its mark: the stored values may
        // be old and new mixed, and the phone does not connect on them until
        // a save has gone through whole.
        if startup_error.is_empty() && !store.read_text(SAVE_MARK).is_empty() {
            startup_error = message("SETTINGS_SAVE_INTERRUPTED");
        }
        let history = History::open(&data);
        let view = Snapshot {
            running: false,
            window_visible: true,
            recording: false,
            recording_path: String::new(),
            converting: 0,
            error: startup_error,
            settings,
            devices: vec![],
            log_sequence: 0,
            data_dir: data.to_string_lossy().into(),
            aec_active: false,
            detail_log_active: false,
            transport: String::new(),
            media_encryption: String::new(),
            microphone_fallback: false,
            microphone_raw: None,
            speaker_raw: None,
            microphone_call: None,
            speaker_call: None,
            account,
            calls: vec![],
            transfer: Transfer::default(),
            registration: "UNCONFIGURED".into(),
            recording_call: String::new(),
            dnd: false,
            unregistered_by_choice: false,
            history_sequence: 0,
            parking: vec![],
            audio_processing_stats: None,
            mwi: Mwi::default(),
        };
        let (files, jobs) = mpsc::channel();
        let services = Self {
            data,
            store,
            logs: Arc::new(Mutex::new(logs)),
            history: Arc::new(Mutex::new(history)),
            converting: Arc::new(AtomicU64::new(0)),
            log_sequence: Arc::new(AtomicU64::new(0)),
            history_sequence: Arc::new(AtomicU64::new(0)),
            files,
            phone: Arc::new(OnceLock::new()),
        };
        let worker = FileWorker {
            data: services.data.clone(),
            logs: services.logs.clone(),
            history: services.history.clone(),
            log_sequence: services.log_sequence.clone(),
            history_sequence: services.history_sequence.clone(),
            phone: services.phone.clone(),
        };
        thread::Builder::new()
            .name("files".into())
            .spawn(move || worker.run(jobs))
            .expect("the file thread starts");
        if !view.error.is_empty() {
            services.log(LOG_APP, view.error.clone());
        }
        (services, view)
    }
    /// Hands the rows of calls that ended to the file thread, in order. The
    /// tab sees them as soon as they are written into the history.
    pub fn add_history(&self, rows: Vec<CallHistory>) {
        if !rows.is_empty() {
            let _ = self.files.send(FileJob::History(rows));
        }
    }
    /// Throws the call history away, through the same queue as the rows that
    /// are added, so that a row still on its way cannot come back after the
    /// clearing; answered once the file is empty.
    pub fn clear_call_history(&self) -> Result<(), String> {
        let (reply, done) = mpsc::channel();
        self.files.send(FileJob::Clear(reply)).map_err(|_| message("APP_CLOSING"))?;
        done.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| Err(message_with("JOURNAL_WRITE_FAILED", ["timeout"])))
    }
    /// Empties the log, tab and file, on the file thread, so that a sync out
    /// on the disk cannot bring cleared lines back; waited for.
    pub fn clear_logs(&self) {
        let (reply, done) = mpsc::channel();
        if self.files.send(FileJob::ClearLogs(reply)).is_ok() {
            let _ = done.recv_timeout(Duration::from_secs(5));
        }
    }
    /// The app is leaving: the last lines and rows are written and the file
    /// thread ends. Waited for up to three seconds; a disk that does not
    /// answer does not hold the exit forever, and then what was not written
    /// is lost with the process (the history rows of ended calls went to the
    /// queue ahead of the Stop, so they are written before it if anything is).
    /// Err says what could not be written, or that the wait ran out.
    pub fn stop_files(&self) -> Result<(), String> {
        let (reply, done) = mpsc::channel();
        self.files.send(FileJob::Stop(reply)).map_err(|_| message("APP_CLOSING"))?;
        done.recv_timeout(Duration::from_secs(3)).unwrap_or_else(|_| Err(message_with("JOURNAL_WRITE_FAILED", ["timeout"])))
    }
    /// Told once the phone actor exists, so that the services can reach the
    /// window through it.
    pub fn attach_phone(&self, phone: PhoneHandle) {
        let _ = self.phone.set(phone);
    }
    /// The folder everything the app writes goes into.
    pub fn data(&self) -> &Path {
        &self.data
    }
    pub fn open_licenses(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.data).map_err(err)?;
        let path = self.data.join("THIRD_PARTY_NOTICES.txt");
        std::fs::write(&path, crate::licenses::text()?).map_err(err)?;
        Process::new("notepad.exe").arg(path).spawn().map_err(err)?;
        Ok(())
    }
    pub fn open_sound_control(&self) -> Result<(), String> {
        Process::new("control.exe")
            .arg("mmsys.cpl")
            .spawn()
            .map_err(err)?;
        Ok(())
    }
    /// Windows' Settings at Privacy & security > Microphone, where the
    /// switches that refuse the microphone are turned on.
    pub fn open_microphone_privacy(&self) -> Result<(), String> {
        Process::new("explorer.exe")
            .arg("ms-settings:privacy-microphone")
            .spawn()
            .map_err(err)?;
        Ok(())
    }
}
/// What the window and the desktop hold. It connects and nothing more: the
/// services, the way into the phone actor, and the snapshot it publishes.
/// Every question about the phone is read from the snapshot; every change
/// to it is a command sent to the actor.
#[derive(Clone)]
pub struct AppState {
    pub services: Services,
    pub phone: PhoneHandle,
    published: Published,
    closing: Arc<AtomicBool>,
}
impl AppState {
    pub fn new() -> Self {
        let (services, initial) = Services::open();
        let (phone, published) = crate::phone_actor::spawn(services.clone(), initial);
        services.attach_phone(phone.clone());
        Self {
            services,
            phone,
            published,
            closing: Arc::new(AtomicBool::new(false)),
        }
    }
    /// The phone as the actor last published it, with the counters of the
    /// services laid over. A read and nothing else: the files are moved
    /// along by the file thread, not by whoever looks.
    pub fn snapshot(&self) -> Snapshot {
        let mut view = self.published.read();
        view.converting = self.services.converting.load(Ordering::Relaxed) as u32;
        view.log_sequence = self.services.log_sequence.load(Ordering::Relaxed);
        view.history_sequence = self.services.history_sequence.load(Ordering::Relaxed);
        view
    }
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }
    /// What the window's volume, mute and meter for a kind of device go by:
    /// the saved choice, and the endpoint a call's stream is on, when one is.
    /// Read off the published snapshot without copying the rest of it: the
    /// meter asks ten times a second.
    pub fn audio_choice(&self, kind: &str) -> (String, Option<crate::phone_state::CallEndpoint>) {
        self.published.with(|view| {
            if kind == "microphone" {
                (view.settings.microphone.clone(), view.microphone_call.clone())
            } else {
                (view.settings.speaker.clone(), view.speaker_call.clone())
            }
        })
    }
    /// The saved software gain for a kind of device, in percent.
    pub fn audio_gain(&self, kind: &str) -> u16 {
        self.published.with(|view| if kind == "microphone" { view.settings.microphone_gain } else { view.settings.speaker_gain })
    }
    /// The app is leaving: the phone is stopped through the actor, then a
    /// conversion still running gets a moment to finish. One that does not
    /// make it leaves its WAV and a partial MP3, and the next start converts
    /// the WAV again, so nothing is lost by leaving.
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.phone.call(Command::Shutdown);
        // The engine is stopped and its last calls' rows are on the file
        // queue. The conversions still running get a moment to finish and
        // say so; only then does the file thread write its last lines and
        // end, so that what they said is in the file. If even that cannot be
        // written, the failure goes to the file directly, as a last resort.
        let end = Instant::now() + Duration::from_secs(5);
        while self.services.converting.load(Ordering::Relaxed) > 0 && Instant::now() < end {
            thread::sleep(Duration::from_millis(100));
        }
        if let Err(e) = self.services.stop_files() {
            crate::logs::append_log(self.services.data(), e);
        }
    }
}
