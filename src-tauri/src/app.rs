//! What holds the app together: the services that are not the phone (the
//! files, the store, the log, the history, the conversions), and the state
//! the window side is given, which only connects: a way to send commands to
//! the phone actor and a way to read the snapshot it publishes.
use crate::history::{CallHistory, History};
use crate::logs::{data_dir, Logs, LOG_APP};
use crate::message::message_with;
use crate::phone_actor::{PhoneHandle, Published};
use crate::phone_message::Command;
use crate::phone_state::{Mwi, Snapshot, Transfer};
use crate::settings::Settings;
use crate::storage::{Account, Store};
use std::{
    path::{Path, PathBuf},
    process::Command as Process,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Sender},
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
    /// The thread that writes the files that must keep their order (the
    /// history), so that the phone never waits on the disk.
    files: Sender<FileJob>,
}
/// What the file thread is asked to write.
enum FileJob {
    History(Vec<CallHistory>),
}
impl Services {
    /// Opens the store, the log and the history, and makes the snapshot the
    /// app starts with. Nothing here may panic: the panic hook is only
    /// installed once this exists.
    pub fn open() -> (Self, Snapshot) {
        let store = Store::new();
        let data = data_dir(&store);
        let mut logs = Logs::open(&data);
        let loaded = store.read_settings::<Settings>();
        let mut startup_error = loaded.as_ref().err().cloned().unwrap_or_default();
        let mut settings = loaded.unwrap_or_default();
        // The policy values live outside the document, also on the first snapshot.
        settings.read_policy(|key| store.read_text(key));
        logs.set_detail(settings.detail_log);
        let account = match store.read_account() {
            Ok(Some(a)) => a.public(),
            Ok(None) => Account::default().public(),
            Err(e) => {
                startup_error = e;
                Account::default().public()
            }
        };
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
            transport: String::new(),
            media_encryption: String::new(),
            microphone_fallback: false,
            microphone_missing: false,
            speaker_missing: false,
            microphone_id: String::new(),
            speaker_id: String::new(),
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
            files,
        };
        let writer = services.clone();
        thread::Builder::new()
            .name("files".into())
            .spawn(move || {
                for job in jobs {
                    match job {
                        FileJob::History(rows) => {
                            // A row that cannot be written waits in the history
                            // for the next try; the failure is on record here,
                            // and the window hears of it from the next flush.
                            if let Err(e) = writer.history.lock().unwrap().add(&writer.data, rows) {
                                writer.log(LOG_APP, e);
                            }
                        }
                    }
                }
            })
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
        Self {
            services,
            phone,
            published,
            closing: Arc::new(AtomicBool::new(false)),
        }
    }
    /// The phone as the actor last published it, with the counters of the
    /// services laid over. Reading also moves the files along: the log's
    /// last lines and the history rows that could not be written get their
    /// turn, and the first failure of either file is said once, in the log
    /// and in the window.
    pub fn snapshot(&self) -> Snapshot {
        let journal = self.services.logs.lock().unwrap().sync_if_due(&self.services.data);
        let history = self.services.history.lock().unwrap().flush(&self.services.data);
        for e in [journal, history].into_iter().filter_map(Result::err) {
            self.phone.send(Command::ShowError(message_with("JOURNAL_WRITE_FAILED", [e])));
        }
        let mut view = self.published.read();
        view.converting = self.services.converting.load(Ordering::Relaxed) as u32;
        view.log_sequence = self.services.logs.lock().unwrap().sequence();
        view.history_sequence = self.services.history.lock().unwrap().sequence();
        view
    }
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }
    /// The app is leaving: the phone is stopped through the actor, then a
    /// conversion still running gets a moment to finish. One that does not
    /// make it leaves its WAV and a partial MP3, and the next start converts
    /// the WAV again, so nothing is lost by leaving.
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.phone.call(Command::Shutdown);
        let end = Instant::now() + Duration::from_secs(5);
        while self.services.converting.load(Ordering::Relaxed) > 0 && Instant::now() < end {
            thread::sleep(Duration::from_millis(100));
        }
    }
}
