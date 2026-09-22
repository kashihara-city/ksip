use crate::message::{message, message_with};
use crate::storage::{Account, AccountView, Store};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, Shutdown, TcpStream},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub network_adapter: String,
    pub sip_port: u16,
    pub rtp_port: u16,
    pub microphone: String,
    pub speaker: String,
    pub microphone_gain: u16,
    pub speaker_gain: u16,
    pub auto_record: bool,
    pub park_slot_1: String,
    pub park_slot_2: String,
    pub park_slot_3: String,
    pub transport: String,
    pub ca_file: String,
    pub media_encryption: String,
    pub auto_answer: bool,
    pub aec: bool,
    pub aec_delay_ms: u16,
    pub register_interval: u16,
    pub detail_log: bool,
    pub browser_integration: bool,
    pub browser_dial_confirm: bool,
    pub shortcut_window: String,
    pub shortcut_call: String,
    pub incoming_action: String,
    pub language: String,
    pub sound_ring: String,
    pub sound_ringback: String,
    pub sound_callwaiting: String,
    pub sound_busy: String,
    pub sound_notfound: String,
    pub sound_error: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            network_adapter: String::new(),
            sip_port: 5060,
            rtp_port: 10000,
            microphone: "default".into(),
            speaker: "default".into(),
            microphone_gain: 100,
            speaker_gain: 100,
            auto_record: false,
            park_slot_1: String::new(),
            park_slot_2: String::new(),
            park_slot_3: String::new(),
            auto_answer: false,
            aec: true,
            aec_delay_ms: 20,
            register_interval: 300,
            detail_log: false,
            browser_integration: false,
            browser_dial_confirm: true,
            shortcut_window: String::new(),
            shortcut_call: String::new(),
            incoming_action: "show".into(),
            language: String::new(),
            sound_ring: String::new(),
            sound_ringback: String::new(),
            sound_callwaiting: String::new(),
            sound_busy: String::new(),
            sound_notfound: String::new(),
            sound_error: String::new(),
            transport: String::new(),
            ca_file: String::new(),
            media_encryption: String::new(),
        }
    }
}
impl Settings {
    /// Values a group policy can set one at a time. They live in their own
    /// registry values rather than inside the settings document.
    pub const POLICY_KEYS: [&'static str; 7] = [
        "park_slot_1",
        "park_slot_2",
        "park_slot_3",
        "transport",
        "ca_file",
        "media_encryption",
        "browser_integration",
    ];
    pub fn policy_values(&self) -> [&str; 7] {
        [
            &self.park_slot_1,
            &self.park_slot_2,
            &self.park_slot_3,
            &self.transport,
            &self.ca_file,
            &self.media_encryption,
            if self.browser_integration { "true" } else { "false" },
        ]
    }
    pub fn set_policy_values(&mut self, values: [String; 7]) {
        let [first, second, third, transport, ca_file, media_encryption, browser] = values;
        self.park_slot_1 = first;
        self.park_slot_2 = second;
        self.park_slot_3 = third;
        self.transport = transport;
        self.ca_file = ca_file;
        self.media_encryption = media_encryption;
        // A policy writes text, and the ways of saying yes are worth accepting.
        self.browser_integration = matches!(
            browser.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        );
    }
    pub fn parking(&self) -> [&str; 3] {
        [&self.park_slot_1, &self.park_slot_2, &self.park_slot_3]
    }
    /// The SIP transport baresip should use. Empty means the historic UDP.
    pub fn sip_transport(&self) -> &str {
        if self.transport.eq_ignore_ascii_case("tls") {
            "TLS"
        } else {
            "UDP"
        }
    }
    /// baresip's media encryption name, or None when calls stay in the clear.
    pub fn mediaenc(&self) -> Option<&str> {
        match self.media_encryption.as_str() {
            "sdes" => Some("srtp"),
            "dtls" => Some("dtls_srtp"),
            _ => None,
        }
    }
    pub const SOUND_KEYS: [&'static str; 6] =
        ["ring", "ringback", "callwaiting", "busy", "notfound", "error"];
    /// The chosen replacement for each built-in sound, in SOUND_KEYS order.
    pub fn sounds(&self) -> [&str; 6] {
        [
            &self.sound_ring,
            &self.sound_ringback,
            &self.sound_callwaiting,
            &self.sound_busy,
            &self.sound_notfound,
            &self.sound_error,
        ]
    }
}
pub use crate::audio::{Calibration, Device, Peak, Volume};
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub running: bool,
    pub recording: bool,
    pub recording_path: String,
    pub error: String,
    pub settings: Settings,
    pub devices: Vec<Device>,
    pub log_sequence: u64,
    pub data_dir: String,
    pub aec_active: bool,
    pub transport: String,
    pub media_encryption: String,
    pub microphone_fallback: bool,
    pub microphone_id: String,
    pub speaker_id: String,
    pub account: AccountView,
    pub calls: Vec<CallInfo>,
    pub transfer: Transfer,
    pub registration: String,
    pub recording_call: String,
    pub history_sequence: u64,
    pub parking: Vec<ParkingInfo>,
    pub audio_processing_stats: Option<AudioProcessingStats>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CallInfo {
    pub id: String,
    pub peer: String,
    pub state: String,
    pub held: bool,
    pub duration: u32,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub line: u8,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CallHistory {
    pub ended_at: u64,
    pub direction: String,
    pub peer: String,
    pub duration: u32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ParkingInfo {
    pub number: String,
    pub state: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioProcessingStats {
    pub echo_return_loss: Option<f64>,
    pub echo_return_loss_enhancement: Option<f64>,
    pub divergent_filter_fraction: Option<f64>,
    pub residual_echo_likelihood: Option<f64>,
    pub residual_echo_likelihood_recent_max: Option<f64>,
    pub render_rms_dbfs: Option<f64>,
    pub capture_device_rms_dbfs: Option<f64>,
    pub capture_mono_rms_dbfs: Option<f64>,
    pub capture_input_rms_dbfs: Option<f64>,
    pub capture_output_rms_dbfs: Option<f64>,
    pub delay_ms: Option<i32>,
    pub delay_median_ms: Option<i32>,
    pub delay_standard_deviation_ms: Option<i32>,
    pub stream_delay_ms: u32,
    pub stream_delay_from_device: bool,
    pub render_frames: u64,
    pub capture_frames: u64,
    pub render_errors: u32,
    pub capture_errors: u32,
    pub capture_device_rate: u32,
    pub capture_device_channels: u32,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Transfer {
    pub original: String,
    pub consultation: String,
    pub pending: bool,
    pub outcome: String,
}
#[derive(Deserialize)]
struct PhoneState {
    registration: String,
    #[serde(default)]
    transport: String,
    #[serde(default)]
    media_encryption: String,
    calls: Vec<CallInfo>,
    transfer: Transfer,
    #[serde(default)]
    parking: Vec<ParkingInfo>,
    #[serde(default)]
    audio_processing_stats: Option<AudioProcessingStats>,
}
fn automatic_answer_targets(
    enabled: bool,
    calls: &[CallInfo],
    answered: &HashSet<String>,
) -> Vec<String> {
    if !enabled {
        return vec![];
    }
    calls
        .iter()
        .filter(|call| call.state == "INCOMING" && !answered.contains(&call.id))
        .map(|call| call.id.clone())
        .collect()
}
fn automatic_recording_target(enabled: bool, calls: &[CallInfo]) -> Option<String> {
    enabled
        .then(|| {
            calls
                .iter()
                .filter(|call| call.state == "ESTABLISHED" && !call.held)
                .min_by_key(|call| call.line)
                .map(|call| call.id.clone())
        })
        .flatten()
}
const LOG_LIMIT: usize = 1000;
// Which layer a log line came from. It is written into the line so that a
// support log shows at a glance whether the app or the engine said it.
const LOG_APP: &str = "app";
const LOG_ENGINE: &str = "engine";
const LOG_EVENT: &str = "event";
const LOG_UI: &str = "ui";
const HISTORY_LIMIT: usize = 1000;
// Log lines and call history are served by sequence so a poll only moves what is new.
/// One line of the log. The parts are kept apart so that the window can
/// present them as it likes, and so that a reader can sort and search them.
#[derive(Clone, Serialize, Deserialize)]
pub struct LogLine {
    /// Local time with its offset from UTC (RFC 3339), because a log is often
    /// read on another machine, in another month, in another country.
    pub time: String,
    pub src: String,
    /// A line the engine wrote, in its own words.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// The name of what the app itself reported, and the values that go with
    /// it. The window turns these into a sentence in the language it shows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}
impl LogLine {
    fn new(time: String, src: &str, body: String) -> Self {
        let (code, args) = if crate::message::is_code(&body) {
            crate::message::split(&body)
        } else {
            (String::new(), vec![])
        };
        Self {
            time,
            src: src.into(),
            text: if code.is_empty() { body } else { String::new() },
            code,
            args,
        }
    }
}
struct Logs {
    entries: VecDeque<LogLine>,
    sequence: u64,
    flushed: Instant,
    dirty: bool,
}
struct History {
    rows: Vec<CallHistory>,
    sequence: u64,
}
#[derive(Clone, Serialize)]
pub struct LogPage {
    pub from: u64,
    pub entries: Vec<LogLine>,
}
type Pending = Arc<Mutex<HashMap<String, mpsc::Sender<Result<Value, String>>>>>;
struct Engine {
    child: Child,
    writer: TcpStream,
    pending: Pending,
    threads: Vec<thread::JoinHandle<()>>,
}
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Mutex<Option<Engine>>>,
    view: Arc<Mutex<Snapshot>>,
    serial: Arc<AtomicU64>,
    data: PathBuf,
    store: Store,
    operations: Arc<Mutex<()>>,
    lines: Arc<Mutex<HashMap<String, u8>>>,
    call_directions: Arc<Mutex<HashMap<String, String>>>,
    auto_answered: Arc<Mutex<HashSet<String>>>,
    logs: Arc<Mutex<Logs>>,
    history: Arc<Mutex<History>>,
    polling_error: Arc<Mutex<String>>,
    bound: Arc<Mutex<String>>,
    closing: Arc<AtomicBool>,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub fn read_netstring(r: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut header = String::new();
    loop {
        let mut b = [0];
        r.read_exact(&mut b).map_err(err)?;
        if b[0] == b':' {
            break;
        }
        if !b[0].is_ascii_digit() || header.len() >= 7 {
            return Err("Invalid IPC frame length".into());
        }
        header.push(b[0] as char);
    }
    if header.is_empty() || (header.len() > 1 && header.starts_with('0')) {
        return Err("Invalid IPC frame header".into());
    }
    let n: usize = header.parse().map_err(err)?;
    if n > 1024 * 1024 {
        return Err("IPC frame too large".into());
    }
    let mut buf = vec![0; n];
    r.read_exact(&mut buf).map_err(err)?;
    let mut end = [0];
    r.read_exact(&mut end).map_err(err)?;
    if end[0] != b',' {
        return Err("Invalid IPC frame terminator".into());
    }
    Ok(buf)
}
fn write_netstring(w: &mut impl Write, data: &[u8]) -> Result<(), String> {
    write!(w, "{}:", data.len()).map_err(err)?;
    w.write_all(data).map_err(err)?;
    w.write_all(b",").map_err(err)
}
impl AppState {
    pub fn new() -> Self {
        let store = Store::new();
        let data = if store.target.starts_with("KSIP/Test/") {
            // Test profiles keep their data in the repository's temp/build. The
            // repository is found from the executable, which the tests always run
            // from inside it (release/, temp/build/<test>/, temp/cargo-target/).
            // Naming it at compile time would put the build machine's path into
            // the product and make the binary depend on where it was built.
            let exe = std::env::current_exe().expect("executable path is required");
            exe.ancestors()
                .find(|dir| dir.join("src-tauri/Cargo.toml").is_file())
                .expect("test profiles run from inside the repository")
                .join("temp/build")
                .join(store.target.rsplit('/').next().unwrap())
        } else {
            // Everything the running app reads or writes stays beside the executable,
            // so a deployment is one folder that can be copied as it is.
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(PathBuf::from))
                .expect("executable directory is required")
        };
        let loaded = store.read_settings::<Settings>();
        let mut startup_error = loaded.as_ref().err().cloned().unwrap_or_default();
        let mut settings = loaded.unwrap_or_default();
        // The park slots live in their own values, also on the first snapshot.
        settings.set_policy_values(Settings::POLICY_KEYS.map(|key| store.read_text(key)));
        let account = match store.read_account() {
            Ok(Some(a)) => a.public(),
            Ok(None) => Account::default().public(),
            Err(e) => {
                startup_error = e;
                Account::default().public()
            }
        };
        let call_history = std::fs::read(data.join("call-history.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let state = Self {
            inner: Arc::new(Mutex::new(None)),
            view: Arc::new(Mutex::new(Snapshot {
                running: false,
                recording: false,
                recording_path: String::new(),
                error: startup_error,
                settings,
                devices: vec![],
                log_sequence: 0,
                data_dir: data.to_string_lossy().into(),
                aec_active: false,
                transport: String::new(),
                media_encryption: String::new(),
                microphone_fallback: false,
                microphone_id: String::new(),
                speaker_id: String::new(),
                account,
                calls: vec![],
                transfer: Transfer::default(),
                registration: "UNCONFIGURED".into(),
                recording_call: String::new(),
                history_sequence: 0,
                parking: vec![],
                audio_processing_stats: None,
            })),
            serial: Arc::new(AtomicU64::new(1)),
            data,
            store,
            operations: Arc::new(Mutex::new(())),
            lines: Arc::new(Mutex::new(HashMap::new())),
            call_directions: Arc::new(Mutex::new(HashMap::new())),
            auto_answered: Arc::new(Mutex::new(HashSet::new())),
            logs: Arc::new(Mutex::new(Logs {
                entries: VecDeque::new(),
                sequence: 0,
                flushed: Instant::now(),
                dirty: false,
            })),
            history: Arc::new(Mutex::new(History {
                rows: call_history,
                sequence: 0,
            })),
            polling_error: Arc::new(Mutex::new(String::new())),
            bound: Arc::new(Mutex::new(String::new())),
            closing: Arc::new(AtomicBool::new(false)),
        };
        if !state.view.lock().unwrap().error.is_empty() {
            let error = state.view.lock().unwrap().error.clone();
            state.log(LOG_APP, error);
        }
        state
    }
    /// Settings come from the JSON document plus the values a policy can set.
    fn settings(&self) -> Result<Settings, String> {
        let mut settings = self.store.read_settings::<Settings>()?;
        settings.set_policy_values(Settings::POLICY_KEYS.map(|key| self.store.read_text(key)));
        Ok(settings)
    }
    /// The park slots own a registry value each, so they are removed from the
    /// settings document instead of being stored twice.
    fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let mut document = serde_json::to_value(settings).map_err(|e| e.to_string())?;
        if let Some(fields) = document.as_object_mut() {
            for key in Settings::POLICY_KEYS {
                fields.remove(key);
            }
        }
        self.store.write_settings(&document)?;
        for (key, value) in Settings::POLICY_KEYS.iter().zip(settings.policy_values()) {
            self.store.write_text(key, value)?;
        }
        Ok(())
    }
    /// Local wall clock for a log line, without pulling in a date crate.
    fn stamp() -> String {
        use windows_sys::Win32::System::Time::{
            GetTimeZoneInformation, TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
        };
        // TIME_ZONE_ID_DAYLIGHT, whose constant lives in a feature nothing else needs.
        const DAYLIGHT: u32 = 2;
        let mut now: SYSTEMTIME = unsafe { std::mem::zeroed() };
        let mut zone: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: both calls only fill the structures above.
        unsafe { GetLocalTime(&mut now) };
        let kind = unsafe { GetTimeZoneInformation(&mut zone) };
        // The bias says how many minutes local time is behind UTC, which is the
        // opposite sign of the offset written after the time.
        let bias = match kind {
            TIME_ZONE_ID_INVALID => 0,
            DAYLIGHT => zone.Bias + zone.DaylightBias,
            _ => zone.Bias + zone.StandardBias,
        };
        let offset = -bias;
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
            now.wYear,
            now.wMonth,
            now.wDay,
            now.wHour,
            now.wMinute,
            now.wSecond,
            if offset < 0 { '-' } else { '+' },
            offset.abs() / 60,
            offset.abs() % 60
        )
    }
    /// Everything the banner shows also belongs in the log, so that a report
    /// made afterwards still explains what the user saw.
    fn show_error(&self, error: String) {
        self.log(LOG_APP, error.clone());
        self.view.lock().unwrap().error = error;
    }
    /// What the window and the shortcuts do belongs in the same log.
    pub fn log_app(&self, text: String) {
        let clean: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
        if !clean.trim().is_empty() {
            self.log(LOG_APP, clean);
        }
    }
    /// Every command that arrives through a browser link is recorded: it comes
    /// from outside the app and explains what happened afterwards.
    pub fn log_protocol(&self, command: &str) {
        let clean: String = command.chars().filter(|c| !c.is_control()).take(100).collect();
        self.log(LOG_APP, format!("protocol {clean}"));
    }
    /// The web view has no log of its own and hands its failures to this one.
    pub fn log_ui(&self, text: String) {
        let clean: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
        if !clean.trim().is_empty() {
            self.log(LOG_UI, clean);
        }
    }
    /// A panic may be the last thing this process does and may already hold a
    /// lock, so this neither waits nor unwraps, and writes the file at once.
    pub fn log_panic(&self, text: String) {
        let Ok(mut logs) = self.logs.try_lock() else {
            return;
        };
        let line = text.replace(['\r', '\n'], " ");
        logs.entries.push_back(LogLine::new(Self::stamp(), LOG_APP, line));
        logs.sequence += 1;
        logs.dirty = true;
        Self::write_logs(&self.data, &mut logs);
    }
    fn log(&self, source: &str, s: String) {
        let mut v = self.view.lock().unwrap();
        if s.contains("Google WebRTC ADM + APM initialized (processing enabled)") {
            v.aec_active = true;
        }
        if s.contains("ksip: microphone fallback active") {
            v.microphone_fallback = true;
        }
        if s.contains("ksip: microphone input recovered") {
            v.microphone_fallback = false;
        }
        if (s.contains("ksip_audio:") || s.contains("wasapi/src:") || s.contains("wasapi/play:"))
            && s.contains("failed")
            && !s.contains("using silence")
        {
            v.error = message_with("AUDIO_DEVICE_INIT_FAILED_DETAIL", [&s]);
            v.aec_active = false;
        }
        if s.contains("postlab: receive WAV closed") && !s.contains("0 dropped samples, error=0") {
            v.error = message("RECORDING_WRITE_PROBLEM");
        }
        drop(v);
        let mut logs = self.logs.lock().unwrap();
        logs.entries.push_back(LogLine::new(Self::stamp(), source, s));
        logs.sequence += 1;
        while logs.entries.len() > LOG_LIMIT {
            logs.entries.pop_front();
        }
        logs.dirty = true;
        // The file mirrors the log tab. Writing at most once a second keeps SIP tracing cheap.
        if logs.flushed.elapsed() >= Duration::from_secs(1) {
            Self::write_logs(&self.data, &mut logs);
        }
    }
    fn write_logs(data: &PathBuf, logs: &mut Logs) {
        if !logs.dirty {
            return;
        }
        logs.flushed = Instant::now();
        logs.dirty = false;
        let rows: Vec<&LogLine> = logs.entries.iter().collect();
        if let Ok(bytes) = serde_json::to_vec_pretty(&rows) {
            let _ = std::fs::create_dir_all(data);
            let _ = std::fs::write(data.join("ksip-log.json"), bytes);
        }
    }
    pub fn clear_logs(&self) {
        let mut logs = self.logs.lock().unwrap();
        logs.entries.clear();
        // Restarting the sequence tells the window that its copy is stale.
        logs.sequence = 0;
        logs.dirty = true;
        Self::write_logs(&self.data, &mut logs);
    }
    pub fn read_logs(&self, after: u64) -> LogPage {
        let logs = self.logs.lock().unwrap();
        let start = logs.sequence - logs.entries.len() as u64;
        if after >= start && after <= logs.sequence {
            LogPage {
                from: after,
                entries: logs
                    .entries
                    .iter()
                    .skip((after - start) as usize)
                    .cloned()
                    .collect(),
            }
        } else {
            LogPage {
                from: start,
                entries: logs.entries.iter().cloned().collect(),
            }
        }
    }
    /// Writes the chosen file over the built-in sound, converted to what the
    /// engine can play. A missing or unreadable file leaves the built-in one.
    fn replace_sound(&self, dir: &std::path::Path, key: &str, chosen: &str) -> Result<(), String> {
        if chosen.is_empty() {
            return Ok(());
        }
        let source = std::path::Path::new(chosen);
        if !source.is_file() {
            return Err(message("FILE_NOT_FOUND"));
        }
        let bytes = std::fs::read(source).map_err(err)?;
        let converted = crate::wav::to_pcm16(&bytes)?;
        std::fs::write(dir.join(format!("{key}.wav")), converted).map_err(err)
    }
    /// Where the engine's generated config lives. It is rebuilt on every connect
    /// from the stored settings, so it is scratch space rather than user data.
    pub fn profile_dir(&self) -> PathBuf {
        let name = self.store.target.rsplit('/').next().unwrap_or("default");
        std::env::temp_dir().join("ksip-profile").join(name)
    }
    /// Throws the call history away, here and in the file beside the exe.
    pub fn clear_call_history(&self) -> Result<(), String> {
        let mut history = self.history.lock().unwrap();
        history.rows.clear();
        history.sequence += 1;
        std::fs::create_dir_all(&self.data).map_err(err)?;
        std::fs::write(self.data.join("call-history.json"), b"[]").map_err(err)
    }
    pub fn read_call_history(&self) -> Vec<CallHistory> {
        self.history.lock().unwrap().rows.clone()
    }
    pub fn snapshot(&self) -> Snapshot {
        {
            // The file is written from log(), so a quiet moment would leave the
            // last lines only in memory. Polling pushes them out.
            let mut logs = self.logs.lock().unwrap();
            if logs.flushed.elapsed() >= Duration::from_secs(1) {
                Self::write_logs(&self.data, &mut logs);
            }
        }
        if let Ok(mut guard) = self.inner.try_lock() {
            if let Some(e) = guard.as_mut() {
                if let Ok(Some(status)) = e.child.try_wait() {
                    let mut v = self.view.lock().unwrap();
                    v.running = false;
                    v.recording = false;
                    v.aec_active = false;
                    v.microphone_fallback = false;
                    v.calls.clear();
                    v.transfer = Transfer::default();
                    v.parking.clear();
                    v.audio_processing_stats = None;
                    v.registration = "DISCONNECTED".into();
                    let error = message_with("ENGINE_EXITED", [status]);
                    v.error = error.clone();
                    drop(v);
                    self.log(LOG_APP, error);
                }
            }
        }
        let mut view = self.view.lock().unwrap().clone();
        view.log_sequence = self.logs.lock().unwrap().sequence;
        view.history_sequence = self.history.lock().unwrap().sequence;
        view
    }
    pub fn refresh_devices(&self) -> Result<(), String> {
        self.view.lock().unwrap().devices = crate::audio::devices()?;
        Ok(())
    }
    pub fn volume(&self, kind: &str, device: &str, level: Option<u16>) -> Result<Volume, String> {
        if !matches!(kind, "microphone" | "speaker") || level.is_some_and(|value| value > 200) {
            return Err(message("AUDIO_VOLUME_ARGUMENT_INVALID"));
        }
        let _operation = level.map(|_| self.operations.lock().unwrap());
        let mut result =
            crate::audio::volume(kind, device, level.map(|value| value.min(100) as u8))?;
        if let Some(level) = level {
            let gain = level.max(100);
            if self.snapshot().running {
                self.request("lab_gain", &format!("{kind} {gain}"))?;
            }
            let mut settings = self.settings()?;
            if kind == "microphone" {
                settings.microphone_gain = gain;
            } else {
                settings.speaker_gain = gain;
            }
            Self::validate(&settings)?;
            self.save_settings(&settings)?;
            self.view.lock().unwrap().settings = settings;
            result.level = level;
        } else {
            let settings = &self.view.lock().unwrap().settings;
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
    pub fn peak(&self, kind: &str, device: &str) -> Result<Peak, String> {
        crate::audio::peak(kind, device)
    }
    pub fn calibrate_aec(
        &self,
        microphone: &str,
        speaker: &str,
        careful: bool,
    ) -> Result<Calibration, String> {
        let _operation = self.operations.lock().unwrap();
        self.ensure_idle()?;
        crate::audio::calibrate_aec(microphone, speaker, careful)
    }
    pub fn select_audio_device(&self, kind: &str, device: String) -> Result<(), String> {
        {
            let _operation = self.operations.lock().unwrap();
            self.ensure_idle()?;
            if !matches!(kind, "microphone" | "speaker")
                || (device != "default"
                    && !self
                        .snapshot()
                        .devices
                        .iter()
                        .any(|entry| entry.kind == kind && entry.id == device))
            {
                return Err(message("SETTINGS_AUDIO_DEVICE_INVALID"));
            }
            let mut settings = self.settings()?;
            if kind == "microphone" {
                settings.microphone = device;
            } else {
                settings.speaker = device;
            }
            Self::validate(&settings)?;
            self.save_settings(&settings)?;
            self.view.lock().unwrap().settings = settings;
        }
        self.connect()
    }
    fn validate(s: &Settings) -> Result<(), String> {
        // A chosen adapter has to exist and hold an address, here and again
        // when the engine starts: what is plugged in can change meanwhile.
        if !s.network_adapter.trim().is_empty() {
            crate::native::adapter_address(s.network_adapter.trim())?;
        }
        if s.sip_port < 1024 || s.rtp_port < 1024 || s.rtp_port > 65400 || s.rtp_port % 2 != 0 {
            return Err(message("SETTINGS_PORT_RANGE"));
        }
        if (s.rtp_port..=s.rtp_port + 20).contains(&s.sip_port) {
            return Err(message("SETTINGS_PORT_OVERLAP"));
        }
        for dev in [&s.microphone, &s.speaker] {
            if dev.len() > 500 || dev.contains(['\r', '\n', '\0']) {
                return Err(message("SETTINGS_AUDIO_DEVICE_INVALID"));
            }
        }
        if !(100..=200).contains(&s.microphone_gain) || !(100..=200).contains(&s.speaker_gain) {
            return Err(message("SETTINGS_GAIN_RANGE"));
        }
        if s.aec_delay_ms > 500 {
            return Err(message("SETTINGS_AEC_DELAY_RANGE"));
        }
        if !(30..=3600).contains(&s.register_interval) {
            return Err(message("SETTINGS_REGISTER_INTERVAL_RANGE"));
        }
        // The keys are only read here; registering them needs the window.
        crate::shortcuts::parse_settings(s)?;
        if !matches!(s.incoming_action.as_str(), "show" | "notify") {
            return Err(message("SETTINGS_INCOMING_ACTION_INVALID"));
        }
        // A language tag, or empty to follow Windows. The window owns the list
        // of languages it has words for; this only rejects nonsense.
        if !s.language.is_empty()
            && (s.language.len() > 35
                || !s
                    .language
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-'))
        {
            return Err(message("SETTINGS_LANGUAGE_INVALID"));
        }
        for sound in s.sounds() {
            if sound.len() > 400 || sound.chars().any(char::is_control) {
                return Err(message("SETTINGS_SOUND_PATH_INVALID"));
            }
        }
        // An empty parking slot leaves that button unconfigured instead of failing the save.
        let configured: Vec<&String> = [&s.park_slot_1, &s.park_slot_2, &s.park_slot_3]
            .into_iter()
            .filter(|slot| !slot.is_empty())
            .collect();
        for slot in &configured {
            if slot.len() > 20 || !slot.bytes().all(|b| b.is_ascii_digit()) {
                return Err(message("SETTINGS_PARK_NUMBER_INVALID"));
            }
        }
        if configured
            .iter()
            .enumerate()
            .any(|(i, slot)| configured[i + 1..].contains(slot))
        {
            return Err(message("SETTINGS_PARK_NUMBER_DUPLICATE"));
        }
        Ok(())
    }
    /// The address to bind to, and the adapter to hand to the engine. An
    /// unset adapter keeps the historic behaviour of binding everything.
    fn binding(settings: &Settings) -> Result<(String, String), String> {
        let chosen = settings.network_adapter.trim();
        if chosen.is_empty() {
            return Ok(("0.0.0.0".into(), String::new()));
        }
        Ok((crate::native::adapter_address(chosen)?, chosen.to_string()))
    }
    pub fn start(&self, mut s: Settings, account: &Account) -> Result<(), String> {
        Self::validate(&s)?;
        let mut guard = self.inner.lock().unwrap();
        if let Some(e) = guard.as_mut() {
            if e.child.try_wait().map_err(err)?.is_none() {
                return Err(message("ENGINE_ALREADY_RUNNING"));
            }
        }
        if let Some(e) = guard.take() {
            let _ = e.writer.shutdown(Shutdown::Both);
            for t in e.threads {
                let _ = t.join();
            }
        }
        let exe = crate::native::engine_exe()?;
        // Resolve the communications defaults once: the volume controls must
        // target the same endpoints that the running engine actually opens.
        let mut repaired = false;
        let microphone = match self.volume("microphone", &s.microphone, None) {
            Ok(volume) => volume.id,
            Err(_) => {
                if s.microphone != "default" {
                    repaired = true;
                    s.microphone = "default".into();
                }
                // The native source emits timed silent PCM when Windows has no
                // usable capture endpoint, so a missing microphone must not
                // prevent SIP registration or RTP transmission.
                "default".into()
            }
        };
        let speaker = match self.volume("speaker", &s.speaker, None) {
            Ok(volume) => volume.id,
            Err(_) if s.speaker != "default" => {
                repaired = true;
                s.speaker = "default".into();
                self.volume("speaker", "default", None)?.id
            }
            Err(error) => return Err(error),
        };
        if repaired {
            self.save_settings(&s)?;
        }
        let profile = self.profile_dir();
        std::fs::create_dir_all(&profile).map_err(err)?;
        // Reserve an ephemeral control port until immediately before spawn.
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").map_err(err)?;
        let ctrl = reservation.local_addr().map_err(err)?.port();
        if ctrl == s.sip_port || (s.rtp_port..=s.rtp_port + 20).contains(&ctrl) {
            return Err(message("ENGINE_CONTROL_PORT_TAKEN"));
        }
        let (address, adapter) = Self::binding(&s)?;
        if !adapter.is_empty() {
            let label = crate::native::adapters()
                .into_iter()
                .find(|a| a.name.eq_ignore_ascii_case(&adapter))
                .map(|a| a.label)
                .unwrap_or_default();
            self.log(LOG_APP, format!("ksip: adapter {label} {adapter} {address}"));
        }
        *self.bound.lock().unwrap() = address.clone();
        let aec = if s.aec { "yes" } else { "no" };
        let mut config=format!("ksip_sip_server {}\nksip_sip_port {}\nksip_extension {}\nsip_listen {}:{}\nsip_transports {}\nsip_cuser_random no\ncall_max_calls 2\ncall_hold_other_calls yes\ncall_accept no\ncall_local_timeout 120\naudio_player ksip_audio,{}\naudio_source ksip_audio,{}\naudio_alert wasapi,{}\nausrc_srate 48000\nauplay_srate 48000\nausrc_channels 1\nauplay_channels 1\nausrc_format s16\nauplay_format s16\nauenc_format s16\naudec_format s16\naudio_buffer 20-160\naudio_jitter_buffer_type fixed\naudio_jitter_buffer_ms 40-80\nwebrtc_aec_delay_ms {}\nksip_aec_enabled {}\nksip_register_interval {}\nksip_detail_log {}\nksip_sip_transport {}\nksip_mediaenc {}\nopus_stereo no\nopus_sprop_stereo no\nopus_bitrate 32000\nopus_inbandfec yes\nopus_packet_loss 10\nopus_dtx no\nopus_application voip\nksip_microphone_gain {}\nksip_speaker_gain {}\nrtp_ports {}-{}\nrtp_timeout 60\nctrl_tcp_listen 127.0.0.1:{}\nmodule g711.dll\nmodule libg722.dll\nmodule opus.dll\nmodule wasapi.dll\nmodule ksip_audio.dll\nmodule postlab.dll\nmodule auconv.dll\nmodule auresamp.dll\nmodule ctrl_tcp.dll\nmodule menu.dll\nmodule srtp.dll\nmodule dtls_srtp.dll\nmodule ksip.dll\n",account.server,account.port,account.extension,address,s.sip_port,s.sip_transport(),speaker,microphone,speaker,s.aec_delay_ms,aec,s.register_interval,if s.detail_log {"yes"} else {"no"},s.sip_transport(),s.mediaenc().unwrap_or(""),s.microphone_gain,s.speaker_gain,s.rtp_port,s.rtp_port+20,ctrl);
        if !adapter.is_empty() {
            config.push_str(&format!("net_interface {adapter}\n"));
        }
        // baresip verifies the server only when it is given a trust anchor.
        if !s.ca_file.trim().is_empty() {
            config.push_str(&format!(
                "sip_cafile {}\nsip_verify_server yes\n",
                s.ca_file.trim().replace('\\', "/")
            ));
        }
        // baresip only treats a path starting with "/" as absolute, so a chosen file
        // cannot be named directly on Windows. The sounds are replaced in place instead.
        if let Some(dir) = crate::native::sounds() {
            for (key, chosen) in Settings::SOUND_KEYS.iter().zip(s.sounds()) {
                if let Err(e) = self.replace_sound(&dir, key, chosen) {
                    self.log(LOG_APP, format!("ksip: {key} sound not replaced, {e}"));
                }
            }
            config.push_str(&format!(
                "audio_path {}\n",
                dir.to_string_lossy().replace('\\', "/")
            ));
        }
        std::fs::write(profile.join("config"), config).map_err(err)?;
        self.clear_logs();
        {
            let mut v = self.view.lock().unwrap();
            v.settings = s;
            v.error.clear();
            v.microphone_id = microphone.clone();
            v.speaker_id = speaker.clone();
            v.aec_active = false;
            v.microphone_fallback = false;
            v.recording = false;
            v.calls.clear();
            v.transfer = Transfer::default();
            v.parking.clear();
            v.registration = "CONNECTING".into();
            v.recording_call.clear();
        }
        drop(reservation);
        let mut threads = Vec::new();
        let mut child = Command::new(&exe)
            .args([
                "--engine",
                &std::process::id().to_string(),
                "-f",
                profile.to_str().ok_or(message("ENGINE_PROFILE_PATH_INVALID"))?,
            ])
            .current_dir(exe.parent().unwrap())
            .env("KSIP_CREDENTIAL_TARGET", &self.store.target)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(0x08000000)
            .spawn()
            .map_err(err)?;
        for pipe in [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let me = self.clone();
            threads.push(thread::spawn(move || {
                for line in BufReader::new(pipe).split(b'\n') {
                    match line {
                        Ok(bytes) => me.log(LOG_ENGINE, String::from_utf8_lossy(&bytes).trim().into()),
                        Err(_) => break,
                    }
                }
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(12);
        let writer = loop {
            if let Ok(socket) = TcpStream::connect((Ipv4Addr::LOCALHOST, ctrl)) {
                break socket;
            }
            if let Some(status) = child.try_wait().map_err(err)? {
                return Err(message_with("ENGINE_START_FAILED", [status]));
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(message("ENGINE_CONNECT_TIMEOUT"));
            }
            thread::sleep(Duration::from_millis(100));
        };
        writer
            .set_write_timeout(Some(Duration::from_secs(3)))
            .map_err(err)?;
        let mut reader = BufReader::new(writer.try_clone().map_err(err)?);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let p = pending.clone();
        let me = self.clone();
        threads.push(thread::spawn(move || {
            while let Ok(data) = read_netstring(&mut reader) {
                let Ok(v) = serde_json::from_slice::<Value>(&data) else {
                    continue;
                };
                if let Some(token) = v.get("token").and_then(Value::as_str) {
                    if let Some(tx) = p.lock().unwrap().remove(token) {
                        let _ = tx.send(Ok(v.clone()));
                    }
                }
                if v.get("event").and_then(Value::as_bool) == Some(true) {
                    me.event(&v);
                }
            }
            for (_, tx) in p.lock().unwrap().drain() {
                let _ = tx.send(Err(message("ENGINE_DISCONNECTED")));
            }
            me.log(LOG_APP, message("ENGINE_CONTROL_LOST"));
            let mut v = me.view.lock().unwrap();
            v.running = false;
            v.recording = false;
            v.microphone_fallback = false;
            v.calls.clear();
            v.transfer = Transfer::default();
            v.parking.clear();
            v.registration = "DISCONNECTED".into();
        }));
        *guard = Some(Engine {
            child,
            writer,
            pending,
            threads,
        });
        {
            let mut v = self.view.lock().unwrap();
            v.running = true;
        }
        Ok(())
    }
    fn event(&self, e: &Value) {
        let kind = e["type"].as_str().unwrap_or("");
        // Credentials never enter events or command parameters.
        self.log(
            LOG_EVENT,
            format!("{} {}", kind, e["param"].as_str().unwrap_or("")),
        );
        let mut v = self.view.lock().unwrap();
        if matches!(
            kind,
            "REGISTERING" | "REGISTER_OK" | "REGISTER_FAIL" | "UNREGISTERING"
        ) {
            v.registration = kind.into();
        }
        if kind == "AUDIO_ERROR" {
            v.error = message("AUDIO_DEVICE_INIT_FAILED");
        }
    }
    pub fn sync_phone(&self) -> Result<(), String> {
        // Serialize UI operations with polling so a stale snapshot cannot overwrite an action.
        let _operation = self.operations.lock().unwrap();
        self.update_phone()
    }
    fn update_phone(&self) -> Result<(), String> {
        if !self.snapshot().running {
            return Ok(());
        }
        let mut phone: PhoneState =
            serde_json::from_str(&self.request("ksip_state", "")?).map_err(err)?;
        let previous = self.view.lock().unwrap().calls.clone();
        let mut directions = self.call_directions.lock().unwrap();
        for call in &phone.calls {
            if call.state == "INCOMING" {
                directions.insert(call.id.clone(), message("HISTORY_INCOMING"));
            } else if matches!(call.state.as_str(), "OUTGOING" | "RINGING" | "EARLY") {
                directions
                    .entry(call.id.clone())
                    .or_insert_with(|| message("HISTORY_OUTGOING"));
            }
        }
        let ended: Vec<CallHistory> = previous
            .iter()
            .filter(|old| !phone.calls.iter().any(|call| call.id == old.id))
            .map(|old| CallHistory {
                ended_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                direction: directions.remove(&old.id).unwrap_or_else(|| message("HISTORY_CALL")),
                peer: old.peer.clone(),
                duration: old.duration,
            })
            .collect();
        directions.retain(|id, _| phone.calls.iter().any(|call| call.id == *id));
        drop(directions);
        let mut lines = self.lines.lock().unwrap();
        lines.retain(|id, _| phone.calls.iter().any(|c| c.id == *id));
        for c in &mut phone.calls {
            let free = (1..=8)
                .find(|i| !lines.values().any(|n| n == i))
                .unwrap_or(8);
            c.line = *lines.entry(c.id.clone()).or_insert(free);
        }
        let mut v = self.view.lock().unwrap();
        v.transport = phone.transport;
        v.media_encryption = phone.media_encryption;
        v.calls = phone.calls;
        if v.calls.is_empty() {
            v.microphone_fallback = false;
        }
        // Targets are claimed while the calls are visible so the next poll cannot answer twice.
        let answer_targets = {
            let mut answered = self.auto_answered.lock().unwrap();
            answered.retain(|id| v.calls.iter().any(|call| call.id == *id));
            let targets = automatic_answer_targets(v.settings.auto_answer, &v.calls, &answered);
            answered.extend(targets.iter().cloned());
            targets
        };
        v.transfer = phone.transfer;
        v.parking = phone.parking;
        v.audio_processing_stats = phone.audio_processing_stats;
        v.registration = phone.registration;
        drop(v);
        if !ended.is_empty() {
            let mut history = self.history.lock().unwrap();
            for entry in ended.into_iter().rev() {
                history.rows.insert(0, entry);
            }
            history.rows.truncate(HISTORY_LIMIT);
            history.sequence += 1;
            std::fs::create_dir_all(&self.data).map_err(err)?;
            std::fs::write(
                self.data.join("call-history.json"),
                serde_json::to_vec_pretty(&history.rows).map_err(err)?,
            )
            .map_err(err)?;
        }
        for id in answer_targets {
            let payload =
                serde_json::to_string(&json!({"op":"answer","id":id,"value":""})).map_err(err)?;
            // A failed automatic answer is reported once; the call can still be answered manually.
            if let Err(e) = self.request("ksip_action", &payload) {
                self.log(LOG_APP, format!("auto answer failed {}", e));
            }
        }
        self.sync_auto_record()
    }
    /// Keeps the `ksip:` registration in step with the setting.
    pub fn apply_browser_integration(&self) {
        let Ok(settings) = self.settings() else {
            return;
        };
        if let Err(e) = crate::protocol::register(settings.browser_integration) {
            self.log(LOG_APP, format!("ksip: browser integration {e}"));
        }
    }
    pub fn initialize(&self) {
        self.apply_browser_integration();
        if let Err(e) = self.refresh_devices() {
            self.show_error(e);
        }
        if !self.view.lock().unwrap().error.is_empty() {
            return;
        }
        if self.snapshot().account.has_password {
            if let Err(e) = self.connect() {
                self.show_error(e);
            }
        }
    }
    fn ensure_idle(&self) -> Result<(), String> {
        if self.snapshot().running {
            let state: PhoneState =
                serde_json::from_str(&self.request("ksip_state", "")?).map_err(err)?;
            if !state.calls.is_empty() {
                return Err(message("CALL_IN_PROGRESS"));
            }
        }
        Ok(())
    }
    pub fn connect(&self) -> Result<(), String> {
        let _operation = self.operations.lock().unwrap();
        if self.is_closing() {
            return Err(message("APP_CLOSING"));
        }
        self.ensure_idle()?;
        let mut account = self
            .store
            .read_account()?
            .ok_or(message("SIP_ACCOUNT_REQUIRED"))?;
        account.validate()?;
        let settings = self.settings()?;
        let parking = settings.parking().join(",");
        self.stop()?;
        // The window shows what the engine is actually running with, so a
        // reconnect brings the saved settings forward as well.
        self.view.lock().unwrap().settings = settings.clone();
        self.start(settings, &account)?;
        if let Err(e) = self.request("ksip_login", "") {
            let _ = self.stop();
            return Err(e);
        }
        if let Err(e) = self.request("ksip_parking", &parking) {
            let _ = self.stop();
            return Err(e);
        }
        Ok(())
    }
    /// The stored password, when the account still belongs to the same
    /// authentication user. The vault holds that user and the password; the
    /// address lives in the registry, so changing it asks for nothing.
    fn password_for(&self, account: &Account) -> Result<String, String> {
        let previous = self
            .store
            .read_account()?
            .filter(|a| a.auth_user == account.auth_user.trim())
            .ok_or(message("ACCOUNT_PASSWORD_REQUIRED"))?;
        Ok(previous.password)
    }
    pub fn save_configuration(
        &self,
        settings: Settings,
        mut account: Account,
    ) -> Result<(), String> {
        {
            let _operation = self.operations.lock().unwrap();
            self.ensure_idle()?;
            Self::validate(&settings)?;
            // SDES puts the keys in the signalling, so it needs TLS to mean anything.
            if settings.mediaenc() == Some("srtp") && settings.sip_transport() != "TLS" {
                return Err(message("SETTINGS_SDES_NEEDS_TLS"));
            }
            let ca = settings.ca_file.trim();
            if !ca.is_empty() && !std::path::Path::new(ca).is_file() {
                return Err(message("SETTINGS_CA_FILE_MISSING"));
            }
            let old = self.store.read_account()?;
            // An empty extension registers under the authentication user.
            if account.extension.trim().is_empty() {
                account.extension = account.auth_user.trim().into();
            }
            if account.password.is_empty() {
                account.password = self.password_for(&account)?;
            }
            account.validate()?;
            self.store.write_account(&account)?;
            if let Err(e) = self.save_settings(&settings) {
                let rollback = if let Some(previous) = old {
                    self.store.write_account(&previous)
                } else {
                    self.store.delete_account()
                };
                return Err(if rollback.is_ok() {
                    e
                } else {
                    message_with("ACCOUNT_ROLLBACK_FAILED", [e])
                });
            }
            let mut v = self.view.lock().unwrap();
            v.settings = settings;
            v.account = account.public();
            drop(v);
            self.apply_browser_integration();
            let mut v = self.view.lock().unwrap();
            v.error.clear();
        }
        self.connect()
    }
    fn request(&self, command: &str, params: &str) -> Result<String, String> {
        let token = self.serial.fetch_add(1, Ordering::Relaxed).to_string();
        let (tx, rx) = mpsc::channel();
        let pending;
        {
            let mut guard = self.inner.lock().unwrap();
            let e = guard.as_mut().ok_or(message("ENGINE_NOT_RUNNING"))?;
            pending = e.pending.clone();
            pending.lock().unwrap().insert(token.clone(), tx);
            let payload =
                serde_json::to_vec(&json!({"command":command,"params":params,"token":token}))
                    .map_err(err)?;
            if let Err(error) = write_netstring(&mut e.writer, &payload) {
                pending.lock().unwrap().remove(&token);
                return Err(error);
            }
        }
        let result = rx.recv_timeout(Duration::from_secs(8));
        pending.lock().unwrap().remove(&token);
        let response = result.map_err(|_| message("ENGINE_RESPONSE_TIMEOUT"))??;
        if response["ok"] != true {
            let detail = response["data"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| message("ACTION_FAILED"));
            return Err(message_with("ENGINE_COMMAND_FAILED", [command, &detail]));
        }
        Ok(response["data"].as_str().unwrap_or("").into())
    }
    fn start_recording(&self, id: &str) -> Result<(), String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(err)?
            .as_millis();
        // The folder appears next to the executable the first time something is recorded.
        let folder = self.data.join("recordings");
        std::fs::create_dir_all(&folder).map_err(err)?;
        let path = folder.join(format!("receive-{now}.wav"));
        self.request(
            "lab_record",
            &format!("{id} {}", path.to_str().ok_or(message("RECORDING_PATH_INVALID"))?),
        )?;
        let mut v = self.view.lock().unwrap();
        v.recording = true;
        v.recording_call = id.into();
        v.recording_path = path.to_string_lossy().into();
        Ok(())
    }
    fn stop_recording(&self) -> Result<(), String> {
        if self.snapshot().recording {
            self.request("lab_stop", "")?;
        }
        let mut v = self.view.lock().unwrap();
        v.recording = false;
        v.recording_call.clear();
        Ok(())
    }
    fn sync_auto_record(&self) -> Result<(), String> {
        let snapshot = self.snapshot();
        let target = automatic_recording_target(snapshot.settings.auto_record, &snapshot.calls);
        if snapshot.recording && (!snapshot.settings.auto_record || snapshot.calls.is_empty()) {
            self.stop_recording()?;
            return Ok(());
        }
        if snapshot.recording && target.as_deref() != Some(snapshot.recording_call.as_str()) {
            self.request("lab_record_select", target.as_deref().unwrap_or("-"))?;
            self.view.lock().unwrap().recording_call = target.unwrap_or_default();
        } else if !snapshot.recording {
            if let Some(id) = target {
                self.start_recording(&id)?;
            }
        }
        Ok(())
    }
    pub fn action(&self, name: &str, id: &str, value: &str, line: u8) -> Result<String, String> {
        let _operation = self.operations.lock().unwrap();
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
                | "park"
                | "auto_record"
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
            let mut settings = self.settings()?;
            settings.auto_record = enabled;
            self.save_settings(&settings)?;
            self.view.lock().unwrap().settings = settings;
            self.sync_auto_record()?;
            return Ok(if enabled {
                message("AUTO_RECORD_ON")
            } else {
                message("AUTO_RECORD_OFF")
            }
            .into());
        }
        if name == "dial" && self.snapshot().calls.iter().any(|c| c.line == line) {
            return Err(message("CALL_LINE_BUSY"));
        }
        if name == "transfer" {
            let v = self.snapshot();
            if !v.calls.iter().any(|c| c.id == id && c.line == 1)
                || !v.calls.iter().any(|c| c.id == value && c.line == 2)
            {
                return Err(message("TRANSFER_NEEDS_TWO_CALLS"));
            }
        }
        if name == "park" {
            let v = self.snapshot();
            let configured = [
                &v.settings.park_slot_1,
                &v.settings.park_slot_2,
                &v.settings.park_slot_3,
            ];
            if !configured.iter().any(|slot| slot.as_str() == value)
                || !v
                    .calls
                    .iter()
                    .any(|c| c.id == id && c.state == "ESTABLISHED" && !c.held)
            {
                return Err(message("PARK_NEEDS_CALL_AND_SLOT"));
            }
        }
        if matches!(name, "dial" | "consult")
            && (value.is_empty()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"*#+_.-".contains(&b)))
        {
            return Err(message("DIAL_TARGET_INVALID"));
        }
        let result = self.request(
            "ksip_action",
            &serde_json::to_string(&json!({"op":name,"id":id,"value":value})).map_err(err)?,
        )?;
        if name == "dial" {
            self.lines
                .lock()
                .unwrap()
                .insert(result.trim().into(), line);
        }
        self.update_phone()?;
        Ok(result)
    }
    pub fn stop(&self) -> Result<(), String> {
        if self.inner.lock().unwrap().is_some() {
            let _ = self.request("lab_stop", "");
            // Parking dialog subscriptions keep baresip's SIP stack alive.
            // Release them before quit so shutdown does not hit the kill timeout.
            let _ = self.request("ksip_shutdown", "");
        }
        let mut guard = self.inner.lock().unwrap();
        if let Some(mut e) = guard.take() {
            let payload = serde_json::to_vec(&json!({"command":"quit","token":"quit"})).unwrap();
            let _ = write_netstring(&mut e.writer, &payload);
            let end = Instant::now() + Duration::from_secs(3);
            while e.child.try_wait().map_err(err)?.is_none() {
                if Instant::now() > end {
                    let _ = e.child.kill();
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = e.child.wait();
            let _ = e.writer.shutdown(Shutdown::Both);
            for t in e.threads {
                let _ = t.join();
            }
        }
        let mut v = self.view.lock().unwrap();
        v.running = false;
        v.recording = false;
        v.aec_active = false;
        v.microphone_fallback = false;
        v.calls.clear();
        v.transfer = Transfer::default();
        v.parking.clear();
        v.registration = "DISCONNECTED".into();
        v.recording_call.clear();
        drop(v);
        let mut logs = self.logs.lock().unwrap();
        Self::write_logs(&self.data, &mut logs);
        Ok(())
    }
    pub fn open_licenses(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.data).map_err(err)?;
        let path = self.data.join("THIRD_PARTY_NOTICES.txt");
        std::fs::write(&path, crate::licenses::text()?).map_err(err)?;
        Command::new("notepad.exe").arg(path).spawn().map_err(err)?;
        Ok(())
    }
    pub fn open_recordings(&self) -> Result<(), String> {
        let path = self.data.join("recordings");
        std::fs::create_dir_all(&path).map_err(err)?;
        std::fs::create_dir_all(&path).map_err(err)?;
        Command::new("explorer.exe")
            .arg(explorer_path(&path))
            .spawn()
            .map_err(err)?;
        Ok(())
    }
    pub fn open_sound_control(&self) -> Result<(), String> {
        Command::new("control.exe")
            .arg("mmsys.cpl")
            .spawn()
            .map_err(err)?;
        Ok(())
    }
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _operation = self.operations.lock().unwrap();
        let _ = self.stop();
    }
    /// The engine binds one address, so a new one means the binding is stale.
    /// Re-registering does not re-bind; only a restart does, and that is what
    /// the reconnect does. A call is never cut short for this.
    pub fn follow_network(&self) {
        let settings = match self.settings() {
            Ok(settings) => settings,
            Err(_) => return,
        };
        if settings.network_adapter.trim().is_empty() || !self.snapshot().running {
            return;
        }
        let Ok(current) = crate::native::adapter_address(settings.network_adapter.trim()) else {
            return;
        };
        if *self.bound.lock().unwrap() == current {
            return;
        }
        if !self.snapshot().calls.is_empty() {
            self.report_error(message("NETWORK_CHANGED"));
            return;
        }
        self.log(
            LOG_APP,
            format!("ksip: adapter address changed, reconnecting on {current}"),
        );
        if let Err(e) = self.connect() {
            self.report_error(e);
        }
    }
    pub fn report_error(&self, error: String) {
        let mut reported = self.polling_error.lock().unwrap();
        if *reported != error {
            self.log(LOG_APP, error.clone());
        }
        *reported = error.clone();
        self.view.lock().unwrap().error = error;
    }
    /// Drops the banner the polling loop raised once polling works again. An error
    /// another path put on screen is left alone.
    pub fn clear_polling_error(&self) {
        let mut reported = self.polling_error.lock().unwrap();
        if reported.is_empty() {
            return;
        }
        let mut v = self.view.lock().unwrap();
        if v.error == *reported {
            v.error.clear();
        }
        reported.clear();
    }
}
// Explorer interprets forward slashes as switches and does not support the
// verbatim path prefix used by some Windows filesystem APIs.
fn explorer_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if let Some(unc) = text.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        text.strip_prefix("\\\\?\\").unwrap_or(&text).to_string()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explorer_receives_windows_folder_paths() {
        assert_eq!(
            explorer_path(std::path::Path::new("C:\\AEC Lab\\data/recordings")),
            "C:\\AEC Lab\\data\\recordings"
        );
        assert_eq!(
            explorer_path(std::path::Path::new("\\\\?\\C:\\AEC Lab\\data/recordings")),
            "C:\\AEC Lab\\data\\recordings"
        );
        assert_eq!(
            explorer_path(std::path::Path::new("\\\\?\\UNC\\server\\share/recordings")),
            "\\\\server\\share\\recordings"
        );
    }
    #[test]
    fn only_a_new_authentication_user_asks_for_the_password_again() {
        let suffix = format!("test-password-{}", std::process::id());
        let mut app = AppState::new();
        app.store = Store {
            key: format!(r"Software\KashiharaCity\ksip\Test\{suffix}"),
            target: format!("KSIP/Test/{suffix}"),
        };
        let account = Account {
            server: "127.0.0.1".into(),
            port: 5060,
            extension: "1001".into(),
            auth_user: "1001".into(),
            password: "local-test-only".into(),
        };
        let result = (|| -> Result<(), String> {
            app.store.write_account(&account)?;
            // The address moved out of the vault, so changing it needs no password.
            let moved = Account {
                port: 5061,
                server: "192.0.2.10".into(),
                extension: "1002".into(),
                password: String::new(),
                ..account.clone()
            };
            let kept = app.password_for(&moved)?;
            assert_eq!(kept, account.password);
            // The password belongs to the authentication user, so that one does.
            let other = Account {
                auth_user: "1099".into(),
                password: String::new(),
                ..account.clone()
            };
            assert!(app.password_for(&other).is_err());
            Ok(())
        })();
        app.store.cleanup_test();
        result.unwrap();
    }
    #[test]
    #[ignore = "requires scripts/build-native.ps1; uses isolated temp/build/rust-engine-test"]
    fn real_engine_starts_stops_and_restarts() {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let root = project.join("temp/build/rust-engine-test");
        let mut app = AppState::new();
        app.data = root.join("data");
        app.store = Store {
            key: r"Software\KashiharaCity\ksip\Test\test-engine".into(),
            target: "KSIP/Test/test-engine".into(),
        };
        let settings = Settings {
            sip_port: 17060,
            rtp_port: 17100,
            ..Settings::default()
        };
        for _ in 0..2 {
            app.refresh_devices().unwrap();
            assert!(!app.snapshot().running);
            assert!(app.inner.lock().unwrap().is_none());
            assert!(!app.snapshot().devices.is_empty());
            let account = Account {
                server: "127.0.0.1".into(),
                port: 5060,
                extension: "1001".into(),
                auth_user: "1001".into(),
                password: "secret".into(),
            };
            app.start(settings.clone(), &account).unwrap();
            assert!(app.snapshot().running);
            let snapshot = app.snapshot();
            let config = std::fs::read_to_string(app.profile_dir().join("config")).unwrap();
            assert!(config.contains(&format!(
                "audio_source ksip_audio,{}",
                snapshot.microphone_id
            )));
            assert!(config.contains(&format!("audio_player ksip_audio,{}", snapshot.speaker_id)));
            assert!(config.contains("webrtc_aec_delay_ms 20"));
            assert!(config.contains("ksip_aec_enabled yes"));
            assert!(config.contains("ksip_microphone_gain 100"));
            assert!(config.contains("ksip_speaker_gain 100"));
            assert!(app.request("lab_stop", "").is_ok());
            assert!(app.action("record", "", "", 1).is_err());
            app.stop().unwrap();
            assert!(!app.snapshot().running);
            assert!(app.inner.lock().unwrap().is_none());
        }
    }
    #[test]
    fn netstrings_are_byte_counted_and_sequential() {
        let mut wire = Vec::new();
        write_netstring(&mut wire, "日本語".as_bytes()).unwrap();
        write_netstring(&mut wire, b"{}").unwrap();
        let mut r = std::io::Cursor::new(wire);
        assert_eq!(read_netstring(&mut r).unwrap(), "日本語".as_bytes());
        assert_eq!(read_netstring(&mut r).unwrap(), b"{}");
    }
    #[test]
    fn webrtc_audio_processing_statistics_are_parsed_from_native_state() {
        let state: PhoneState = serde_json::from_str(
            r#"{"registration":"OK","calls":[],"transfer":{"original":"","consultation":"","pending":false,"outcome":""},"audio_processing_stats":{"echo_return_loss":12.5,"echo_return_loss_enhancement":28.75,"residual_echo_likelihood":0.04,"render_rms_dbfs":-18.5,"capture_input_rms_dbfs":-24.0,"capture_output_rms_dbfs":-51.5,"delay_ms":84,"delay_median_ms":82,"delay_standard_deviation_ms":3,"stream_delay_ms":20,"stream_delay_from_device":true,"render_frames":1200,"capture_frames":1198,"render_errors":0,"capture_errors":0}}"#,
        )
        .unwrap();
        let stats = state.audio_processing_stats.unwrap();
        assert_eq!(stats.echo_return_loss, Some(12.5));
        assert_eq!(stats.echo_return_loss_enhancement, Some(28.75));
        assert_eq!(stats.delay_ms, Some(84));
        assert_eq!(stats.stream_delay_ms, 20);
        assert_eq!(stats.residual_echo_likelihood, Some(0.04));
        assert_eq!(stats.render_rms_dbfs, Some(-18.5));
        assert_eq!(stats.capture_input_rms_dbfs, Some(-24.0));
        assert_eq!(stats.capture_output_rms_dbfs, Some(-51.5));
        assert_eq!(stats.delay_median_ms, Some(82));
        assert_eq!(stats.delay_standard_deviation_ms, Some(3));
        assert!(stats.stream_delay_from_device);
        assert_eq!(stats.render_frames, 1200);
        assert_eq!(stats.capture_frames, 1198);
        assert_eq!(stats.render_errors + stats.capture_errors, 0);
    }
    #[test]
    fn reject_malformed_frames() {
        for data in [
            b"9999999:x,".as_slice(),
            b"01:a,",
            b"2:{};",
            b"x:{}",
            b"2:{",
        ] {
            assert!(read_netstring(&mut &data[..]).is_err());
        }
    }
    #[test]
    fn reject_config_injection_and_port_overlap() {
        let mut s = Settings::default();
        s.microphone = "default\nmodule evil".into();
        assert!(AppState::validate(&s).is_err());
        s = Settings::default();
        s.sip_port = s.rtp_port;
        assert!(AppState::validate(&s).is_err());
        s = Settings::default();
        s.aec_delay_ms = 501;
        assert!(AppState::validate(&s).is_err());
        assert!(AppState::validate(&Settings::default()).is_ok());
    }
    #[test]
    fn reject_invalid_volume_without_launching_helper() {
        let app = AppState::new();
        assert!(app.volume("other", "default", None).is_err());
        assert!(app.volume("speaker", "default", Some(201)).is_err());
        assert!(app.volume("microphone", "bad\0id", None).is_err());
        assert!(app.peak("other", "default").is_err());
        assert!(app.peak("microphone", "bad\0id").is_err());
    }
    #[test]
    fn logs_move_by_sequence_and_reset_when_lines_are_dropped() {
        let app = AppState::new();
        app.log(LOG_APP, "one".into());
        app.log(LOG_APP, "two".into());
        let page = app.read_logs(0);
        assert_eq!(page.from, 0);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].src, LOG_APP);
        assert_eq!(page.entries[0].text, "one");
        // 2026-09-21T09:12:54+09:00 の形であること。
        let stamp = &page.entries[0].time;
        let (date, rest) = stamp.split_once('T').expect("date and time");
        assert!(date.split('-').all(|p| p.parse::<u32>().is_ok()));
        let (time, offset) = rest.split_at(8);
        assert_eq!(time.split(':').count(), 3);
        assert!(
            offset.starts_with(['+', '-']) && offset.len() == 6 && offset[1..].contains(':'),
            "offset was {offset}"
        );
        let page = app.read_logs(1);
        assert_eq!(page.from, 1);
        assert_eq!(page.entries[0].text, "two");
        assert!(app.read_logs(2).entries.is_empty());
        for i in 0..LOG_LIMIT {
            app.log(LOG_APP, format!("line {i}"));
        }
        let page = app.read_logs(1);
        assert_eq!(page.entries.len(), LOG_LIMIT);
        assert!(page.from > 1, "the window is told which lines it lost");
        assert_eq!(app.snapshot().log_sequence, LOG_LIMIT as u64 + 2);
    }
    #[test]
    fn ui_and_panic_lines_are_labelled_and_kept_readable() {
        let app = AppState::new();
        app.log_ui("failed\u{7}\nwith a control char".into());
        app.log_panic("panic at 'boom'\nsecond line".into());
        let entries = app.read_logs(0).entries;
        assert_eq!(entries[0].src, LOG_UI);
        assert_eq!(entries[0].text, "failedwith a control char");
        // A panic stays on one line so its time is next to it.
        assert_eq!(entries[1].src, LOG_APP);
        assert_eq!(entries[1].text, "panic at 'boom' second line");
        // Blank input is not worth a line of its own.
        app.log_ui("   ".into());
        assert_eq!(app.read_logs(0).entries.len(), 2);
    }
    #[test]
    fn polling_errors_clear_themselves_but_leave_other_errors() {
        let app = AppState::new();
        app.report_error(message("TEMPORARY"));
        assert_eq!(app.snapshot().error, "TEMPORARY");
        app.clear_polling_error();
        assert_eq!(app.snapshot().error, "");
        app.report_error(message("TEMPORARY"));
        app.view.lock().unwrap().error = message("AUDIO_DEVICE_INIT_FAILED");
        app.clear_polling_error();
        assert_eq!(app.snapshot().error, "AUDIO_DEVICE_INIT_FAILED");
    }
    #[test]
    fn a_chosen_sound_replaces_the_built_in_one() {
        let app = AppState::new();
        let dir = std::env::temp_dir().join("ksip-sound-test");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("chosen.wav");
        let mut float_wav = b"RIFF".to_vec();
        let data = 1.0f32.to_le_bytes();
        float_wav.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        float_wav.extend_from_slice(b"WAVEfmt ");
        float_wav.extend_from_slice(&16u32.to_le_bytes());
        for value in [3u16, 1u16] {
            float_wav.extend_from_slice(&value.to_le_bytes());
        }
        float_wav.extend_from_slice(&8000u32.to_le_bytes());
        float_wav.extend_from_slice(&32000u32.to_le_bytes());
        for value in [4u16, 32u16] {
            float_wav.extend_from_slice(&value.to_le_bytes());
        }
        float_wav.extend_from_slice(b"data");
        float_wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        float_wav.extend_from_slice(&data);
        std::fs::write(&source, &float_wav).unwrap();
        std::fs::write(dir.join("ring.wav"), b"built-in").unwrap();
        app.replace_sound(&dir, "ring", source.to_str().unwrap()).unwrap();
        let written = std::fs::read(dir.join("ring.wav")).unwrap();
        assert_eq!(&written[..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([written[34], written[35]]), 16);
        // An empty choice and a missing file both leave the built-in sound alone.
        std::fs::write(dir.join("ring.wav"), b"built-in").unwrap();
        app.replace_sound(&dir, "ring", "").unwrap();
        assert!(app.replace_sound(&dir, "ring", "C:/no/such/file.wav").is_err());
        assert_eq!(std::fs::read(dir.join("ring.wav")).unwrap(), b"built-in");
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn microphone_fallback_log_updates_visible_state() {
        let app = AppState::new();
        app.log(LOG_ENGINE, "ksip: microphone fallback active".into());
        assert!(app.snapshot().microphone_fallback);
        app.log(LOG_ENGINE, "ksip: microphone input recovered".into());
        assert!(!app.snapshot().microphone_fallback);
    }
    #[test]
    fn settings_migrate_and_auto_record_selects_active_line() {
        let settings: Settings = serde_json::from_str(
            r#"{"sip_port":5060,"rtp_port":10000,"microphone":"default","speaker":"default","aec":true}"#,
        )
        .unwrap();
        assert_eq!(settings.microphone_gain, 100);
        assert_eq!(settings.speaker_gain, 100);
        assert_eq!(settings.aec_delay_ms, 20);
        assert_eq!(settings.register_interval, 300);
        assert!(!settings.detail_log);
        assert!(!settings.auto_record);
        assert!(!settings.auto_answer);
        assert!(settings.park_slot_1.is_empty());
        assert!(settings.park_slot_2.is_empty());
        assert!(settings.park_slot_3.is_empty());
        let calls = vec![
            CallInfo {
                id: "held".into(),
                peer: "sip:1@local".into(),
                state: "ESTABLISHED".into(),
                held: true,
                duration: 1,
                codec: String::new(),
                secure: false,
                transport: "UDP".into(),
                line: 1,
            },
            CallInfo {
                id: "active".into(),
                peer: "sip:2@local".into(),
                state: "ESTABLISHED".into(),
                held: false,
                duration: 1,
                codec: String::new(),
                secure: false,
                transport: "UDP".into(),
                line: 2,
            },
        ];
        assert_eq!(
            automatic_recording_target(true, &calls).as_deref(),
            Some("active")
        );
        assert!(automatic_recording_target(false, &calls).is_none());
    }
    #[test]
    fn automatic_answer_selects_each_incoming_call_once() {
        let calls = vec![
            CallInfo {
                id: "incoming".into(),
                peer: "sip:1@local".into(),
                state: "INCOMING".into(),
                held: false,
                duration: 0,
                codec: String::new(),
                secure: false,
                transport: "UDP".into(),
                line: 1,
            },
            CallInfo {
                id: "talking".into(),
                peer: "sip:2@local".into(),
                state: "ESTABLISHED".into(),
                held: false,
                duration: 5,
                codec: String::new(),
                secure: false,
                transport: "UDP".into(),
                line: 2,
            },
        ];
        let mut answered = HashSet::new();
        assert!(automatic_answer_targets(false, &calls, &answered).is_empty());
        let targets = automatic_answer_targets(true, &calls, &answered);
        assert_eq!(targets, vec!["incoming".to_string()]);
        answered.extend(targets);
        assert!(automatic_answer_targets(true, &calls, &answered).is_empty());
    }
}
