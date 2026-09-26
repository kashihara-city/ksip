use crate::message::{message, message_with};
use crate::storage::{Account, AccountView, Store};
use serde::{Deserialize, Serialize};
use crate::engine_link::StartPlan;
use crate::phone_actor::{PhoneHandle, Published};
use crate::phone_message::Command;
use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command as Process,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// The endpoints the engine is given for a saved choice, see
/// `resolve_audio_endpoints`.
pub struct AudioEndpoints {
    pub microphone: String,
    pub speaker: String,
    pub microphone_missing: bool,
    pub speaker_missing: bool,
}
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
    /// The six buttons under the call controls, in the order they are shown.
    pub buttons: Vec<CustomButton>,
    pub transport: String,
    pub ca_file: String,
    pub media_encryption: String,
    /// The audio codecs to offer, by name, in order; empty offers all of them.
    pub codecs: String,
    pub auto_answer: bool,
    pub aec: bool,
    pub aec_delay_ms: u16,
    /// The other parts of the WebRTC audio processing, each its own switch.
    pub high_pass: bool,
    /// off, low, moderate, high or very_high.
    pub noise_suppression: String,
    pub agc: bool,
    pub register_interval: u16,
    pub detail_log: bool,
    pub browser_integration: bool,
    pub browser_dial_confirm: bool,
    pub shortcut_window: String,
    pub shortcut_call: String,
    pub incoming_action: String,
    /// Seconds after the last call ends before the window goes to the tray;
    /// -1 leaves the window where it is.
    pub tray_after_call: i32,
    pub language: String,
    pub sound_ring: String,
    pub sound_ringback: String,
    pub sound_busy: String,
    pub sound_notfound: String,
    pub sound_error: String,
}
/// One of the six buttons a site defines: what it says, what it does and to
/// which numbers. The number is the one the button is about: watched (BLF),
/// dialled, transferred to, or, for a link, opened. The two others refine
/// what happens around it and fall back to it when left empty.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomButton {
    pub title: String,
    /// Empty (unused), `transfer`, `dial`, `speed`, `park`, `open`, `dnd` or `mwi`.
    /// `speed` is a dial nobody watches: no BLF, so the number may be an
    /// outside line written with its separators.
    pub kind: String,
    pub number: String,
    /// For `park`: where the call in progress is sent while the watched
    /// number is free, when that differs, as with Asterisk's `*701`.
    pub transfer: String,
    /// For `dial` and `park`: what is called while the watched number is in
    /// use, when that differs: a pickup code such as Asterisk's `*8701`.
    pub pickup: String,
}
impl CustomButton {
    /// The first six sit on the phone; the rest fill the panel beside it,
    /// which appears while any of them is set.
    pub const MAIN: usize = 6;
    pub const COUNT: usize = 30;
    pub const KINDS: [&'static str; 7] = ["transfer", "dial", "speed", "park", "open", "dnd", "mwi"];
    pub fn empty_set() -> Vec<Self> {
        vec![Self::default(); Self::COUNT]
    }
    pub fn configured(&self) -> bool {
        !self.kind.is_empty()
    }
    /// Whether the engine subscribes to the number's dialog state.
    pub fn watches(&self) -> bool {
        matches!(self.kind.as_str(), "dial" | "park")
    }
    /// What the engine is given: a number, or a full SIP URI. A URI may be
    /// written between angle brackets, as a Refer-To header carries it; the
    /// engine's library wants it bare, so the brackets go here.
    pub fn address(text: &str) -> &str {
        let text = text.trim();
        text.strip_prefix('<')
            .and_then(|inner| inner.strip_suffix('>'))
            .map_or(text, str::trim)
    }
    /// Whether the text names a SIP URI rather than a number for the registrar.
    pub fn is_uri(text: &str) -> bool {
        let lower = Self::address(text).to_ascii_lowercase();
        lower.starts_with("sip:") || lower.starts_with("sips:")
    }
    /// A button holds its number the way the registrar dials it, so that what
    /// the engine watches is what the button says; an empty one is unused.
    pub fn target_ok(text: &str) -> bool {
        let address = Self::address(text);
        address.is_empty() || dial_target(address).is_ok_and(|target| target == address)
    }
    /// A number for the `speed` kind, which nobody watches: anything the dial
    /// box takes, so RFC 3966's separators and a `tel:` prefix may stay as written.
    pub fn speed_ok(text: &str) -> bool {
        dial_target(text).is_ok()
    }
    /// A web address for the `open` kind: the browser gets it, nothing else does.
    pub fn link_ok(text: &str) -> bool {
        let text = text.trim();
        let lower = text.to_ascii_lowercase();
        (lower.starts_with("http://") || lower.starts_with("https://"))
            && text.len() <= 500
            && !text.chars().any(|c| c.is_control() || c == ' ')
    }
    pub fn link_target(&self) -> Option<&str> {
        (self.kind == "open").then(|| self.number.trim())
    }
    /// The address the engine watches and calls, for the kinds that do so.
    pub fn dial_target(&self) -> Option<&str> {
        self.watches().then(|| Self::address(&self.number))
    }
    /// What is sent for the transfer: the target as it was set up, angle
    /// brackets included. Refer-To carries a URI bare or in brackets, both
    /// standard, and a PBX may only take the form its phones were set up
    /// with (the production one does).
    pub fn transfer_text(&self) -> Option<&str> {
        self.transfer_target()?;
        Some(match self.kind.as_str() {
            "park" if !Self::address(&self.transfer).is_empty() => self.transfer.trim(),
            _ => self.number.trim(),
        })
    }
    /// Where a call in progress goes when the button is pressed, if anywhere.
    pub fn transfer_target(&self) -> Option<&str> {
        match self.kind.as_str() {
            "transfer" => Some(Self::address(&self.number)),
            "park" if Self::address(&self.transfer).is_empty() => Some(Self::address(&self.number)),
            "park" => Some(Self::address(&self.transfer)),
            _ => None,
        }
    }
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
            buttons: CustomButton::empty_set(),
            auto_answer: false,
            aec: true,
            aec_delay_ms: 20,
            high_pass: true,
            noise_suppression: "high".into(),
            agc: true,
            register_interval: 300,
            detail_log: false,
            browser_integration: false,
            browser_dial_confirm: true,
            shortcut_window: String::new(),
            shortcut_call: String::new(),
            incoming_action: "show".into(),
            tray_after_call: -1,
            language: String::new(),
            sound_ring: String::new(),
            sound_ringback: String::new(),
            sound_busy: String::new(),
            sound_notfound: String::new(),
            sound_error: String::new(),
            transport: String::new(),
            ca_file: String::new(),
            media_encryption: String::new(),
            codecs: String::new(),
        }
    }
}
/// How SIP is carried, as the `transport` setting names it. TCP is as
/// unencrypted as UDP; only TLS protects the signalling, and with it the
/// keys that SDES and OSRTP put there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Udp,
    Tcp,
    Tls,
}
impl Transport {
    /// Reads the setting's text: `udp`, `tcp`, `tls`, or empty for the
    /// historic UDP. Anything else is not a transport this version knows.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "udp" => Some(Self::Udp),
            "tcp" => Some(Self::Tcp),
            "tls" => Some(Self::Tls),
            _ => None,
        }
    }
    /// What baresip's configuration calls it.
    pub fn engine_name(self) -> &'static str {
        match self {
            Self::Udp => "UDP",
            Self::Tcp => "TCP",
            Self::Tls => "TLS",
        }
    }
    pub fn encrypts_signalling(self) -> bool {
        self == Self::Tls
    }
}
/// How the media is encrypted, as the `media_encryption` setting names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaEncryption {
    /// Plain RTP.
    None,
    /// RFC 4568: the keys ride in the signalling and the media is RTP/SAVP; a
    /// call that cannot be encrypted does not go through.
    Sdes,
    /// RFC 8643: the same keys offered in RTP/AVP, and a peer that returns none
    /// gets a plain call. For the move from plain to encrypted.
    Osrtp,
    /// RFC 5763: the keys are exchanged on the media path itself.
    Dtls,
}
impl MediaEncryption {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" => Some(Self::None),
            "sdes" => Some(Self::Sdes),
            "osrtp" => Some(Self::Osrtp),
            "dtls" => Some(Self::Dtls),
            _ => None,
        }
    }
    /// baresip's mediaenc module, or None for plain RTP.
    pub fn engine_name(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Sdes => Some("srtp-mand"),
            Self::Osrtp => Some("srtp"),
            Self::Dtls => Some("dtls_srtp"),
        }
    }
    /// Whether the keys travel in the signalling, which then has to be
    /// encrypted to mean anything (RFC 4568; RFC 8643 section 4).
    pub fn keys_in_signalling(self) -> bool {
        matches!(self, Self::Sdes | Self::Osrtp)
    }
}
impl Settings {
    /// Values a group policy can set one at a time. They live in their own
    /// registry values rather than inside the settings document. The buttons
    /// are among them, as `button_1_title`, `button_1_kind`, `button_1_number`,
    /// `button_1_transfer` and `button_1_pickup` up to `button_30_…`; 1 to 6 sit on
    /// the phone, 7 to 30 in the panel beside it.
    const BUTTON_FIELDS: [&'static str; 5] = ["title", "kind", "number", "transfer", "pickup"];
    /// The document keys that are stored as policy values instead.
    pub const POLICY_DOCUMENT_KEYS: [&'static str; 6] =
        ["transport", "ca_file", "media_encryption", "codecs", "browser_integration", "buttons"];
    pub fn policy_values(&self) -> Vec<(String, String)> {
        let mut values = vec![
            ("transport".to_string(), self.transport.clone()),
            ("ca_file".to_string(), self.ca_file.clone()),
            ("media_encryption".to_string(), self.media_encryption.clone()),
            ("codecs".to_string(), self.codecs.clone()),
            (
                "browser_integration".to_string(),
                if self.browser_integration { "true" } else { "false" }.to_string(),
            ),
        ];
        for (index, button) in self.buttons.iter().enumerate().take(CustomButton::COUNT) {
            for (field, value) in Self::BUTTON_FIELDS.iter().zip([
                &button.title,
                &button.kind,
                &button.number,
                &button.transfer,
                &button.pickup,
            ]) {
                values.push((format!("button_{}_{field}", index + 1), value.clone()));
            }
        }
        values
    }
    pub fn read_policy(&mut self, read: impl Fn(&str) -> String) {
        // A policy writes text by hand; the case and the surrounding blanks
        // carry no meaning, and are not left to make a valid value look unknown.
        self.transport = read("transport").trim().to_ascii_lowercase();
        self.ca_file = read("ca_file");
        self.media_encryption = read("media_encryption").trim().to_ascii_lowercase();
        self.codecs = read("codecs");
        // A policy writes text, and the ways of saying yes are worth accepting.
        self.browser_integration = matches!(
            read("browser_integration").trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        );
        self.buttons = (1..=CustomButton::COUNT)
            .map(|index| CustomButton {
                title: read(&format!("button_{index}_title")),
                kind: read(&format!("button_{index}_kind")).trim().to_ascii_lowercase(),
                number: read(&format!("button_{index}_number")),
                transfer: read(&format!("button_{index}_transfer")),
                pickup: read(&format!("button_{index}_pickup")),
            })
            .collect();
    }
    /// The numbers the engine watches, each once, in button order.
    pub fn watched_numbers(&self) -> Vec<&str> {
        let mut numbers: Vec<&str> = Vec::new();
        for target in self.buttons.iter().filter_map(CustomButton::dial_target) {
            if !numbers.contains(&target) {
                numbers.push(target);
            }
        }
        numbers
    }
    /// The transport the setting names. The setting is checked by `validate`
    /// before the engine is started, so an unknown value is never reached here;
    /// were it, UDP is what the historic empty value meant.
    pub fn transport(&self) -> Transport {
        Transport::parse(&self.transport).unwrap_or(Transport::Udp)
    }
    /// The SIP transport as baresip's configuration names it.
    pub fn sip_transport(&self) -> &'static str {
        self.transport().engine_name()
    }
    /// The media encryption the setting names; see `transport` for the fallback.
    pub fn media_encryption(&self) -> MediaEncryption {
        MediaEncryption::parse(&self.media_encryption).unwrap_or(MediaEncryption::None)
    }
    /// The codecs the app can offer, by the names the setting uses, in the
    /// order they are offered when the setting names none.
    pub const CODECS: [&'static str; 4] = ["opus", "G722", "PCMU", "PCMA"];
    /// The strengths the noise suppression setting can name.
    pub const NOISE_SUPPRESSION_LEVELS: [&'static str; 5] =
        ["off", "low", "moderate", "high", "very_high"];
    /// The codecs to offer, in order: the setting's names, each once, or all
    /// of them when it names none. A PBX that answers one codec and sends
    /// another garbles what the far end hears; the order is how a site steers
    /// around that.
    pub fn codec_list(&self) -> Vec<&'static str> {
        let mut list: Vec<&'static str> = Vec::new();
        for name in self.codecs.split(',').map(str::trim) {
            if let Some(known) = Self::CODECS.iter().find(|c| c.eq_ignore_ascii_case(name)) {
                if !list.contains(known) {
                    list.push(known);
                }
            }
        }
        if list.is_empty() {
            Self::CODECS.to_vec()
        } else {
            list
        }
    }
    /// baresip's media encryption module name, or None when calls stay in the clear.
    pub fn mediaenc(&self) -> Option<&'static str> {
        self.media_encryption().engine_name()
    }
    pub const SOUND_KEYS: [&'static str; 5] = ["ring", "ringback", "busy", "notfound", "error"];
    /// The chosen replacement for each built-in sound, in SOUND_KEYS order.
    pub fn sounds(&self) -> [&str; 5] {
        [
            &self.sound_ring,
            &self.sound_ringback,
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
    /// Whether the window is on screen. Off, the window leaves the microphone alone.
    pub window_visible: bool,
    pub recording: bool,
    pub recording_path: String,
    /// Recordings being turned into MP3 at the moment, so the window can say
    /// what the processor is busy with.
    #[serde(default)]
    pub converting: u32,
    pub error: String,
    pub settings: Settings,
    pub devices: Vec<Device>,
    pub log_sequence: u64,
    pub data_dir: String,
    pub aec_active: bool,
    pub transport: String,
    pub media_encryption: String,
    pub microphone_fallback: bool,
    /// The saved microphone or speaker was not there when the engine started,
    /// so the default is in use. The saved choice stands; nothing is written.
    pub microphone_missing: bool,
    pub speaker_missing: bool,
    pub microphone_id: String,
    pub speaker_id: String,
    pub account: AccountView,
    pub calls: Vec<CallInfo>,
    pub transfer: Transfer,
    pub registration: String,
    /// Do not disturb: incoming calls are refused as busy while this is on.
    #[serde(default)]
    pub dnd: bool,
    /// The person asked to be unregistered. Automatic reconnects (a changed
    /// address, a device that came back) leave the phone that way; only a
    /// connect asked for, or a saved setting, registers again. Part of the
    /// phone's state, so that the window can say so too.
    #[serde(default)]
    pub unregistered_by_choice: bool,
    pub recording_call: String,
    pub history_sequence: u64,
    pub parking: Vec<ParkingInfo>,
    pub audio_processing_stats: Option<AudioProcessingStats>,
    #[serde(default)]
    pub mwi: Mwi,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CallInfo {
    pub id: String,
    pub peer: String,
    /// The caller's display name, when the call came with one.
    #[serde(default)]
    pub name: String,
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
    #[serde(default)]
    pub name: String,
    pub duration: u32,
    /// The file in `recordings/` that holds this call, if it was recorded.
    /// A file can hold several calls: automatic recording follows the call
    /// in progress, through a transfer for instance.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recording: String,
    /// How a call that never connected ended: the name of it (busy, not
    /// found, missed...), empty for a call that was talked on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
}
/// What a recording is called: when it started, and whom the call was with,
/// so that the folder reads like the history. `2026-09-23_14-30-12_1002.wav`.
/// The number in a peer address: `sip:1001@pbx;x` and `<sip:1001@pbx>` give `1001`.
pub fn peer_number(peer: &str) -> String {
    let user = peer.trim().trim_start_matches('<');
    let user = user
        .strip_prefix("sip:")
        .or_else(|| user.strip_prefix("sips:"))
        .unwrap_or(user);
    user.split(['@', ';', '>']).next().unwrap_or("").to_string()
}
/// How a call's other end is named to the person: the caller's name in front
/// of the number when the call came with one, as the window shows it.
pub fn caller_label(call: &CallInfo) -> String {
    let number = peer_number(&call.peer);
    if call.name.is_empty() {
        number
    } else {
        format!("{} {}", call.name, number)
    }
}
pub fn recording_name(stamp: &str, peer: &str) -> String {
    let time: String = stamp
        .chars()
        .take(19)
        .map(|c| match c {
            'T' => '_',
            ':' => '-',
            c => c,
        })
        .collect();
    let user: String = peer_number(peer)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '_' | '.' | '-'))
        .take(40)
        .collect();
    let user = if user.is_empty() { "call".to_string() } else { user };
    format!("{time}_{user}.wav")
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ParkingInfo {
    pub number: String,
    pub state: String,
}
/// What the voicemail box reports through its message-summary subscription.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mwi {
    pub waiting: bool,
    pub new: u32,
    pub old: u32,
}
impl Mwi {
    /// Reads an RFC 3842 summary: `Messages-Waiting: yes` and
    /// `Voice-Message: 2/5 (0/1)`, new before old, urgent ones in brackets.
    pub fn parse(summary: &str) -> Self {
        let mut mwi = Self::default();
        for line in summary.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if name.eq_ignore_ascii_case("Messages-Waiting") {
                mwi.waiting = value.eq_ignore_ascii_case("yes");
            } else if name.eq_ignore_ascii_case("Voice-Message") {
                let counts = value.split_whitespace().next().unwrap_or_default();
                let (new, old) = counts.split_once('/').unwrap_or((counts, "0"));
                mwi.new = new.trim().parse().unwrap_or(0);
                mwi.old = old.trim().parse().unwrap_or(0);
            }
        }
        mwi
    }
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
    pub agc_speech_level_dbfs: Option<f64>,
    pub agc_noise_level_dbfs: Option<f64>,
    pub agc_headroom_db: Option<f64>,
    pub agc_gain_db: Option<f64>,
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
    /// Counts the outcomes set, so the window can show the same one again.
    #[serde(default)]
    pub outcome_seq: u64,
}
/// What the engine answers to `ksip_state`.
#[derive(Deserialize)]
pub struct EngineReport {
    pub registration: String,
    #[serde(default)]
    pub dnd: bool,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub media_encryption: String,
    pub calls: Vec<CallInfo>,
    pub transfer: Transfer,
    #[serde(default)]
    pub parking: Vec<ParkingInfo>,
    #[serde(default)]
    pub audio_processing_stats: Option<AudioProcessingStats>,
    #[serde(default)]
    pub mwi_summary: String,
}
/// What the registrar is asked to call. A SIP URI is taken as written, one
/// line of visible ASCII, which is all a SIP URI ever is. A number is reduced
/// to what the registrar dials: the visual separators of RFC 3966 (`-`, `.`,
/// `(`, `)`) and spaces go, as does a `tel:` scheme in front, and what is left
/// has to be digits, `*` and `#`, with `+` only in front. Letters are not a
/// number; a name is written as a URI.
pub fn dial_target(text: &str) -> Result<String, String> {
    let address = CustomButton::address(text);
    if CustomButton::is_uri(address) {
        return (address.len() <= 200 && address.bytes().all(|b| (0x21..=0x7e).contains(&b)))
            .then(|| address.to_string())
            .ok_or_else(|| message("DIAL_TARGET_INVALID"));
    }
    let bare = address
        .get(..4)
        .filter(|scheme| scheme.eq_ignore_ascii_case("tel:"))
        .map_or(address, |_| &address[4..]);
    let number: String = bare
        .chars()
        .filter(|c| !matches!(c, '-' | '.' | '(' | ')' | ' '))
        .collect();
    let digits = number.strip_prefix('+').unwrap_or(&number);
    if digits.is_empty()
        || number.len() > 30
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'*' || b == b'#')
    {
        return Err(message("DIAL_TARGET_INVALID"));
    }
    Ok(number)
}

/// The name a conversion writes under until it is finished. It keeps the
/// `.mp3` extension, which is how the encoder picks its container, and is
/// never what a finished recording is called.
fn partial_recording(wav: &Path) -> PathBuf {
    wav.with_extension("converting.mp3")
}
/// Tidies a recordings folder from earlier runs and says which WAVs still
/// want converting: a WAV whose MP3 is there goes (the conversion went
/// through, the WAV was in use at the time), a partial MP3 goes (its
/// conversion never finished, and the WAV is still the recording), and a WAV
/// without an MP3 is handed back. Nothing else is touched.
fn sweep_recording_folder(folder: &Path) -> Vec<PathBuf> {
    let mut leftover = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return leftover;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if name.ends_with(".converting.mp3") {
            let _ = std::fs::remove_file(&path);
        } else if name.ends_with(".wav") {
            if path.with_extension("mp3").is_file() {
                let _ = std::fs::remove_file(&path);
            } else {
                leftover.push(path);
            }
        }
    }
    leftover
}
const LOG_LIMIT: usize = 1000;
/// What the file keeps after a rewrite while the detail log is on. SIP traces
/// and WebRTC's lines fill the ordinary thousand in seconds, and a report
/// needs the minutes around a failure. The tab keeps LOG_LIMIT either way.
const DETAIL_LOG_FILE_LIMIT: usize = 10000;
const LOG_FILE: &str = "ksip-log.jsonl";
// Which layer a log line came from. It is written into the line so that a
// support log shows at a glance whether the app or the engine said it.
pub const LOG_APP: &str = "app";
pub const LOG_ENGINE: &str = "engine";
pub const LOG_EVENT: &str = "event";
const LOG_UI: &str = "ui";
const HISTORY_LIMIT: usize = 1000;
const HISTORY_FILE: &str = "call-history.jsonl";
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
/// The log tab and its file. The tab holds this run's last lines; the file is
/// one JSON object per line and is only ever appended to, except when it has
/// grown to twice the limit and is written afresh with what the tab holds.
/// Another process appends to it too: the link handler, when there is
/// nothing to hand a link to. Those lines are picked up from the file.
pub struct Logs {
    entries: VecDeque<LogLine>,
    sequence: u64,
    flushed: Instant,
    /// Lines at the back of `entries` that the file does not have yet.
    unwritten: usize,
    /// The file as it was last seen: how many lines, and where it ended.
    file_lines: usize,
    offset: u64,
    /// How many lines the file keeps after a rewrite, which happens once it
    /// holds twice as many.
    file_limit: usize,
    /// Whether the last attempt to write the file failed. The unwritten lines
    /// stay in `entries` meanwhile, and are tried again with the next ones.
    write_failed: bool,
}
impl Logs {
    /// Takes stock of the file as it is. Nothing is read back: the tab shows
    /// this run, and the file keeps the one before as well.
    fn open(data: &Path) -> Self {
        let (file_lines, offset) = std::fs::read(data.join(LOG_FILE))
            .map(|bytes| (bytes.iter().filter(|b| **b == b'\n').count(), bytes.len() as u64))
            .unwrap_or((0, 0));
        Self {
            entries: VecDeque::new(),
            sequence: 0,
            flushed: Instant::now(),
            unwritten: 0,
            file_lines,
            offset,
            file_limit: LOG_LIMIT,
            write_failed: false,
        }
    }
    /// The detail log keeps ten times as many lines in the file.
    pub fn set_detail(&mut self, detail: bool) {
        self.file_limit = if detail { DETAIL_LOG_FILE_LIMIT } else { LOG_LIMIT };
    }
    fn push(&mut self, line: LogLine) {
        self.entries.push_back(line);
        self.sequence += 1;
        self.unwritten += 1;
        self.trim();
    }
    fn trim(&mut self) {
        while self.entries.len() > LOG_LIMIT {
            self.entries.pop_front();
        }
        self.unwritten = self.unwritten.min(self.entries.len());
    }
    /// Brings the file and the tab up to date with each other: lines another
    /// process appended come in, the lines of this one go out. Appending only
    /// what is new, at most once a second, is what keeps SIP tracing cheap.
    /// A write that fails (a full disk, a file held by another program) keeps
    /// the lines as unwritten, so the next sync carries them along with the
    /// new ones; the tab loses nothing but the oldest once it is full. The
    /// error is returned only when the writing has just started failing, so
    /// the caller can say so once rather than every second; the recovery is
    /// noted in the log itself.
    pub fn sync(&mut self, data: &Path) -> Result<(), String> {
        self.flushed = Instant::now();
        let path = data.join(LOG_FILE);
        self.take_foreign(&path);
        if self.unwritten > 0 {
            let bytes = lines(self.entries.iter().skip(self.entries.len() - self.unwritten));
            let _ = std::fs::create_dir_all(data);
            match append(&path, &bytes) {
                Ok(()) => {
                    self.offset += bytes.len() as u64;
                    self.file_lines += self.unwritten;
                    self.unwritten = 0;
                    if self.write_failed {
                        self.write_failed = false;
                        self.push(LogLine::new(stamp(), LOG_APP, message("JOURNAL_WRITE_RECOVERED")));
                    }
                }
                Err(e) => {
                    let first = !self.write_failed;
                    self.write_failed = true;
                    if first {
                        return Err(err(e));
                    }
                }
            }
        }
        if self.file_lines > 2 * self.file_limit {
            self.shorten(&path);
        }
        Ok(())
    }
    /// Keeps the file's last lines, up to the limit. The file rather than the
    /// tab is the source: after a start the tab holds only this run, and the
    /// run before belongs in the file as much as this one.
    fn shorten(&mut self, path: &Path) {
        if let Ok((length, lines)) = keep_last_lines(path, self.file_limit) {
            self.offset = length;
            self.file_lines = lines;
        }
    }
    /// Lines the file gained since it was last seen, from another process.
    /// A line appended between this look and this process's own append is
    /// only missed by the tab; the file has it.
    fn take_foreign(&mut self, path: &Path) {
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return;
        };
        if len < self.offset {
            // Someone emptied or replaced the file; it is taken as new.
            self.offset = 0;
            self.file_lines = 0;
        }
        if len == self.offset {
            return;
        }
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(self.offset)).is_err() || file.read_to_end(&mut bytes).is_err() {
            return;
        }
        // Only whole lines count; one still being written waits for the next look.
        let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        for line in bytes[..end].split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            self.file_lines += 1;
            if let Ok(entry) = serde_json::from_slice::<LogLine>(line) {
                // In front of the unwritten lines: the file has this one already.
                let at = self.entries.len() - self.unwritten;
                self.entries.insert(at, entry);
                self.sequence += 1;
            }
        }
        self.trim();
        self.offset += end as u64;
    }
    fn clear(&mut self, data: &Path) {
        self.entries.clear();
        // Restarting the sequence tells the window that its copy is stale.
        self.sequence = 0;
        self.unwritten = 0;
        let _ = std::fs::write(data.join(LOG_FILE), b"");
        self.offset = 0;
        self.file_lines = 0;
    }
}
/// One JSON object per line, the form both files are kept in.
fn lines<'a, T: Serialize + 'a>(entries: impl Iterator<Item = &'a T>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        if let Ok(json) = serde_json::to_vec(entry) {
            bytes.extend(json);
            bytes.push(b'\n');
        }
    }
    bytes
}
fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?
        .write_all(bytes)
}
/// Writes the file afresh with its last lines, up to the limit, and says how
/// long it is now and how many lines it holds.
fn keep_last_lines(path: &Path, limit: usize) -> std::io::Result<(u64, usize)> {
    let bytes = std::fs::read(path)?;
    // The last line ends with the last newline; the kept lines start after
    // the newline that ends the line before the first of them.
    let mut newlines = 0;
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate().rev() {
        if *b == b'\n' {
            newlines += 1;
            if newlines > limit {
                start = i + 1;
                break;
            }
        }
    }
    std::fs::write(path, &bytes[start..])?;
    Ok(((bytes.len() - start) as u64, newlines.min(limit)))
}
/// One line from a process that is not the app: the link handler, when it
/// has nothing to hand a link to. The app picks the line up from the file.
pub fn append_log(data: &Path, body: String) {
    let clean: String = body.chars().filter(|c| !c.is_control()).take(300).collect();
    let bytes = lines(std::iter::once(&LogLine::new(stamp(), LOG_APP, clean)));
    let _ = std::fs::create_dir_all(data);
    let _ = append(&data.join(LOG_FILE), &bytes);
}
/// Where the running app keeps what it writes: the person's own local
/// application data, beside the settings' place in the registry. Not beside
/// the executable: a single exe gets run from a file server sooner or later,
/// and there the history, the log and the recordings of everyone who did so
/// would end up in one folder, mixed and readable by all. Not the roaming
/// part either: recordings are big and belong to the machine they were made
/// on. Test profiles keep theirs in the repository's temp/build. The
/// repository is found from the executable, which the tests run from inside
/// it (release/, temp/build/<test>/, temp/cargo-target/). Naming it at compile
/// time would put the build machine's path into the product and make the
/// binary depend on where it was built.
pub fn data_dir(store: &Store) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let beside = exe.parent().map(PathBuf::from).unwrap_or_default();
    if !store.target.starts_with("KSIP/Test/") {
        return std::env::var_os("LOCALAPPDATA")
            .filter(|dir| !dir.is_empty())
            .map(|dir| PathBuf::from(dir).join("KashiharaCity").join("ksip"))
            .unwrap_or(beside);
    }
    exe.ancestors()
        .find(|dir| dir.join("src-tauri/Cargo.toml").is_file())
        .map(|dir| {
            dir.join("temp/build")
                .join(store.target.rsplit('/').next().unwrap_or("test"))
        })
        .unwrap_or(beside)
}
/// The call history and its file: one call per line, oldest first, appended
/// as calls end. The tab shows it newest first. Past twice the limit the file
/// is written afresh with its last lines. Nothing else writes it.
pub struct History {
    /// Newest first, as the tab shows it.
    rows: Vec<CallHistory>,
    sequence: u64,
    file_lines: usize,
    /// Rows the file does not have yet, oldest first: what could not be
    /// appended is kept and tried again with the next rows, or at the next
    /// flush, rather than being lost with the call state it was made from.
    pending: Vec<CallHistory>,
}
impl History {
    /// Reads the file, oldest first, into the tab's order.
    fn open(data: &Path) -> Self {
        let mut rows: Vec<CallHistory> = Vec::new();
        let mut file_lines = 0;
        if let Ok(bytes) = std::fs::read(data.join(HISTORY_FILE)) {
            for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                file_lines += 1;
                if let Ok(row) = serde_json::from_slice::<CallHistory>(line) {
                    rows.push(row);
                }
            }
        }
        rows.reverse();
        rows.truncate(HISTORY_LIMIT);
        Self {
            rows,
            sequence: 0,
            file_lines,
            pending: Vec::new(),
        }
    }
    /// The calls that ended since the last look, in the order the tab shows
    /// them. The tab gets them at once; the file, being oldest first, gets
    /// them the other way round, together with whatever is still waiting.
    pub fn add(&mut self, data: &Path, ended: Vec<CallHistory>) -> Result<(), String> {
        for entry in ended.iter().rev() {
            self.rows.insert(0, entry.clone());
        }
        self.rows.truncate(HISTORY_LIMIT);
        self.sequence += 1;
        self.pending.extend(ended.into_iter().rev());
        let excess = self.pending.len().saturating_sub(HISTORY_LIMIT);
        self.pending.drain(..excess);
        self.flush(data)
    }
    /// Writes the rows still waiting for the file, if any.
    pub fn flush(&mut self, data: &Path) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let path = data.join(HISTORY_FILE);
        std::fs::create_dir_all(data).map_err(err)?;
        append(&path, &lines(self.pending.iter())).map_err(err)?;
        self.file_lines += self.pending.len();
        self.pending.clear();
        if self.file_lines > 2 * HISTORY_LIMIT {
            if let Ok((_, kept)) = keep_last_lines(&path, HISTORY_LIMIT) {
                self.file_lines = kept;
            }
        }
        Ok(())
    }
    fn clear(&mut self, data: &Path) -> Result<(), String> {
        self.rows.clear();
        self.pending.clear();
        self.sequence += 1;
        self.file_lines = 0;
        std::fs::create_dir_all(data).map_err(err)?;
        std::fs::write(data.join(HISTORY_FILE), b"").map_err(err)
    }
}
#[derive(Clone, Serialize)]
pub struct LogPage {
    pub from: u64,
    pub entries: Vec<LogLine>,
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
}
/// What a start needs, worked out from the settings before the process
/// exists: the plan the link spawns from, the endpoints the engine was
/// given, the address it binds, and the log lines that explain the choices.
pub struct Prepared {
    pub plan: StartPlan,
    pub endpoints: AudioEndpoints,
    pub address: String,
    pub notes: Vec<String>,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
/// Local wall clock for a log line, without pulling in a date crate.
pub fn stamp() -> String {
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
/// Which endpoints the engine got, by name and id, and when a saved one was
/// not there. By name and id, so that a device the engine then cannot open
/// can be told apart from a wrong choice.
pub fn endpoint_notes(s: &Settings, endpoints: &AudioEndpoints, devices: &[Device]) -> Vec<String> {
    let mut notes = Vec::new();
    for (missing, kind) in [
        (endpoints.microphone_missing, "microphone"),
        (endpoints.speaker_missing, "speaker"),
    ] {
        if missing {
            notes.push(format!("ksip: the saved {kind} is not there, using the default"));
        }
    }
    for (kind, id, chosen) in [
        ("microphone", &endpoints.microphone, &s.microphone),
        ("speaker", &endpoints.speaker, &s.speaker),
    ] {
        let name = devices
            .iter()
            .find(|d| d.kind == kind && d.id == *id)
            .map(|d| format!("{} ", d.name))
            .unwrap_or_default();
        let role = if chosen.as_str() == "default" { " (the Windows default)" } else { "" };
        notes.push(format!("ksip: {kind} {name}{id}{role}"));
    }
    notes
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
        let services = Self {
            data,
            store,
            logs: Arc::new(Mutex::new(logs)),
            history: Arc::new(Mutex::new(history)),
            converting: Arc::new(AtomicU64::new(0)),
        };
        if !view.error.is_empty() {
            services.log(LOG_APP, view.error.clone());
        }
        (services, view)
    }
    /// Settings come from the JSON document plus the values a policy can set.
    pub fn settings(&self) -> Result<Settings, String> {
        let mut settings = self.store.read_settings::<Settings>()?;
        settings.read_policy(|key| self.store.read_text(key));
        Ok(settings)
    }
    /// The policy values own a registry value each, so they are removed from
    /// the settings document instead of being stored twice.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let mut document = serde_json::to_value(settings).map_err(|e| e.to_string())?;
        if let Some(fields) = document.as_object_mut() {
            for key in Settings::POLICY_DOCUMENT_KEYS {
                fields.remove(key);
            }
        }
        self.store.write_settings(&document)?;
        for (key, value) in settings.policy_values() {
            self.store.write_text(&key, &value)?;
        }
        Ok(())
    }
    /// One line into the tab and, once a second, into the file. What a line
    /// from the engine says about the phone is the actor's to read.
    pub fn log(&self, source: &str, s: String) {
        let mut logs = self.logs.lock().unwrap();
        logs.push(LogLine::new(stamp(), source, s));
        if logs.flushed.elapsed() >= Duration::from_secs(1) {
            let _ = logs.sync(&self.data);
        }
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
        logs.push(LogLine::new(stamp(), LOG_APP, line));
        // A write that fails is kept and said by the periodic sync in snapshot().
        let _ = logs.sync(&self.data);
    }
    pub fn clear_logs(&self) {
        self.logs.lock().unwrap().clear(&self.data);
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
    pub fn replace_sound(&self, dir: &std::path::Path, key: &str, chosen: &str) -> Result<(), String> {
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
    /// Throws the call history away, here and in its file.
    pub fn clear_call_history(&self) -> Result<(), String> {
        self.history.lock().unwrap().clear(&self.data)
    }
    /// The file a history row's recording is in now: the MP3 once it has
    /// been made, the WAV until then, nothing once both are gone.
    pub fn recording_file(&self, name: &str) -> Option<PathBuf> {
        let plain = !name.is_empty()
            && name.ends_with(".wav")
            && !name.contains(['/', '\\', ':'])
            && !name.starts_with('.');
        if !plain {
            return None;
        }
        let wav = self.data.join("recordings").join(name);
        [wav.with_extension("mp3"), wav].into_iter().find(|path| path.is_file())
    }
    /// The rows as the tab shows them: a recording is named only while its
    /// file is still there to be played.
    pub fn read_call_history(&self) -> Vec<CallHistory> {
        let mut rows = self.history.lock().unwrap().rows.clone();
        for row in &mut rows {
            if !row.recording.is_empty() && self.recording_file(&row.recording).is_none() {
                row.recording.clear();
            }
        }
        rows
    }
    /// Shows a recording named in the history in Explorer, selected.
    pub fn open_recording_location(&self, name: &str) -> Result<(), String> {
        let path = self.recording_file(name).ok_or_else(|| message("RECORDING_NOT_FOUND"))?;
        crate::native::show_in_folder(&path)
    }
    /// Plays a recording named in the history with whatever Windows plays
    /// sound files with. Only a file in the recordings folder can be named.
    pub fn open_recording(&self, name: &str) -> Result<(), String> {
        let path = self.recording_file(name).ok_or_else(|| message("RECORDING_NOT_FOUND"))?;
        crate::native::open_url(&path.to_string_lossy())
            .map_err(|e| message_with("RECORDING_OPEN_FAILED", [crate::message::split(&e).1.join(" ")]))
    }
    pub fn validate(s: &Settings) -> Result<(), String> {
        // A chosen adapter has to exist and hold an address, here and again
        // when the engine starts: what is plugged in can change meanwhile.
        if !s.network_adapter.trim().is_empty() {
            crate::native::adapter_address(s.network_adapter.trim())?;
        }
        if s.sip_port < 1024 || s.rtp_port < 1024 || s.rtp_port > 65400 || !s.rtp_port.is_multiple_of(2) {
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
        if !Settings::NOISE_SUPPRESSION_LEVELS.contains(&s.noise_suppression.as_str()) {
            return Err(message("SETTINGS_NOISE_SUPPRESSION_INVALID"));
        }
        if !(30..=3600).contains(&s.register_interval) {
            return Err(message("SETTINGS_REGISTER_INTERVAL_RANGE"));
        }
        if !(-1..=3600).contains(&s.tray_after_call) {
            return Err(message("SETTINGS_TRAY_AFTER_CALL_RANGE"));
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
        // The transport and the media encryption have to be values this
        // version knows. An unknown one is refused rather than read as UDP or
        // as no encryption: these come from a policy as well as from the
        // dialog, and a typo there must not quietly turn the encryption off.
        let Some(transport) = Transport::parse(&s.transport) else {
            return Err(message("SETTINGS_TRANSPORT_INVALID"));
        };
        let Some(media) = MediaEncryption::parse(&s.media_encryption) else {
            return Err(message("SETTINGS_MEDIA_ENCRYPTION_INVALID"));
        };
        // Keys in the signalling need the signalling encrypted. Checked here,
        // so that settings that never passed through the dialog cannot start
        // the engine with the keys on a plain transport.
        if media.keys_in_signalling() && !transport.encrypts_signalling() {
            return Err(message("SETTINGS_SDES_NEEDS_TLS"));
        }
        // Codec names from the known set, each at most once; none means all.
        let mut named: Vec<String> = Vec::new();
        for name in s.codecs.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let lower = name.to_ascii_lowercase();
            if !Settings::CODECS.iter().any(|c| c.eq_ignore_ascii_case(name)) || named.contains(&lower) {
                return Err(message("SETTINGS_CODECS_INVALID"));
            }
            named.push(lower);
        }
        for sound in s.sounds() {
            if sound.len() > 400 || sound.chars().any(char::is_control) {
                return Err(message("SETTINGS_SOUND_PATH_INVALID"));
            }
        }
        // A policy can write this value without passing through the dialog, and
        // it ends up on a line of the engine's config.
        if s.ca_file.len() > 400 || s.ca_file.chars().any(char::is_control) {
            return Err(message("SETTINGS_CA_FILE_INVALID"));
        }
        // A button whose kind is empty is simply not shown; the rest need a
        // number the engine can put in a SIP URI.
        if s.buttons.len() > CustomButton::COUNT {
            return Err(message("SETTINGS_BUTTON_KIND_INVALID"));
        }
        for button in &s.buttons {
            if !button.kind.is_empty() && !CustomButton::KINDS.contains(&button.kind.as_str()) {
                return Err(message("SETTINGS_BUTTON_KIND_INVALID"));
            }
            if button.title.chars().count() > 40 || button.title.chars().any(char::is_control) {
                return Err(message("SETTINGS_BUTTON_TITLE_INVALID"));
            }
            let number_ok = if button.kind == "open" {
                CustomButton::link_ok(&button.number)
            } else if button.kind == "dnd" {
                // A switch on the phone itself: nothing to name.
                CustomButton::address(&button.number).is_empty()
            } else if button.kind == "speed" {
                CustomButton::speed_ok(&button.number)
            } else {
                CustomButton::target_ok(&button.number)
                    && !(button.configured() && CustomButton::address(&button.number).is_empty())
            };
            if !number_ok
                || !CustomButton::target_ok(&button.transfer)
                || !CustomButton::target_ok(&button.pickup)
            {
                return Err(message("SETTINGS_BUTTON_NUMBER_INVALID"));
            }
        }
        // The engine subscribes to each watched number once, so two buttons
        // cannot watch the same one.
        let watched: Vec<&str> = s.buttons.iter().filter_map(CustomButton::dial_target).collect();
        if watched.iter().enumerate().any(|(i, n)| watched[i + 1..].contains(n)) {
            return Err(message("SETTINGS_BUTTON_NUMBER_DUPLICATE"));
        }
        Ok(())
    }
    /// The address to bind to, and the adapter to hand to the engine. An
    /// unset adapter keeps the historic behaviour of binding everything.
    pub fn binding(settings: &Settings) -> Result<(String, String), String> {
        let chosen = settings.network_adapter.trim();
        if chosen.is_empty() {
            return Ok(("0.0.0.0".into(), String::new()));
        }
        Ok((crate::native::adapter_address(chosen)?, chosen.to_string()))
    }
    /// The endpoints the engine is given for the saved choice: the device
    /// itself when Windows has it, the default in its place otherwise. Nothing
    /// is written back: the choice stands, and it is used again once the
    /// device is back.
    pub fn resolve_audio_endpoints(&self, s: &Settings) -> Result<AudioEndpoints, String> {
        let mut microphone_missing = false;
        let microphone = match crate::audio::volume("microphone", &s.microphone, None, None) {
            Ok(volume) => volume.id,
            Err(_) => {
                microphone_missing = s.microphone != "default";
                // The native source emits timed silent PCM when Windows has no
                // usable capture endpoint, so a missing microphone must not
                // prevent SIP registration or RTP transmission.
                "default".into()
            }
        };
        let mut speaker_missing = false;
        let speaker = match crate::audio::volume("speaker", &s.speaker, None, None) {
            Ok(volume) => volume.id,
            Err(_) if s.speaker != "default" => {
                speaker_missing = true;
                crate::audio::volume("speaker", "default", None, None)?.id
            }
            Err(error) => return Err(error),
        };
        Ok(AudioEndpoints {
            microphone,
            speaker,
            microphone_missing,
            speaker_missing,
        })
    }
    /// Works out everything a start needs and writes the engine's config:
    /// the endpoints for the saved devices, the control port, the address to
    /// bind, the trust list for TLS and the chosen sounds. The process itself
    /// is the link's to start.
    pub fn prepare_start(&self, s: &Settings, account: &Account, devices: &[Device]) -> Result<Prepared, String> {
        let exe = crate::native::engine_exe()?;
        // Resolve the communications defaults once: the volume controls must
        // target the same endpoints that the running engine actually opens.
        let endpoints = self.resolve_audio_endpoints(s)?;
        let microphone = endpoints.microphone.clone();
        let speaker = endpoints.speaker.clone();
        let profile = self.profile_dir();
        std::fs::create_dir_all(&profile).map_err(err)?;
        // Reserve an ephemeral control port until immediately before spawn.
        let reservation = TcpListener::bind("127.0.0.1:0").map_err(err)?;
        let ctrl = reservation.local_addr().map_err(err)?.port();
        if ctrl == s.sip_port || (s.rtp_port..=s.rtp_port + 20).contains(&ctrl) {
            return Err(message("ENGINE_CONTROL_PORT_TAKEN"));
        }
        let (address, adapter) = Self::binding(s)?;
        let mut notes = Vec::new();
        if !adapter.is_empty() {
            let label = crate::native::adapters()
                .into_iter()
                .find(|a| a.name.eq_ignore_ascii_case(&adapter))
                .map(|a| a.label)
                .unwrap_or_default();
            notes.push(format!("ksip: adapter {label} {adapter} {address}"));
        }
        let yes_no = |flag: bool| if flag { "yes" } else { "no" };
        // One line per setting, so that a value cannot land under the wrong name.
        let mut config = String::new();
        let mut put = |line: String| {
            config.push_str(&line);
            config.push('\n');
        };
        put(format!("ksip_sip_server {}", account.server));
        put(format!("ksip_sip_port {}", account.port));
        put(format!("ksip_extension {}", account.extension));
        put(format!("sip_listen {address}:{}", s.sip_port));
        put(format!("sip_transports {}", s.sip_transport()));
        for line in [
            "sip_cuser_random no",
            "call_max_calls 2",
            // The engine module holds the other calls itself: baresip's own
            // rule would also hold the call being talked on when a second
            // call is answered by the far end, which took the person away.
            "call_hold_other_calls no",
            "call_accept no",
            "call_local_timeout 120",
        ] {
            put(line.into());
        }
        put(format!("audio_player ksip_audio,{speaker}"));
        put(format!("audio_source ksip_audio,{microphone}"));
        put(format!("audio_alert wasapi,{speaker}"));
        // A second call during a call is shown, not sounded. baresip would send
        // that tone through the call's player, and ksip_audio cannot play a
        // tone and the call at the same time.
        put("callwaiting_aufile none".into());
        for line in [
            "ausrc_srate 48000",
            "auplay_srate 48000",
            "ausrc_channels 1",
            "auplay_channels 1",
            "ausrc_format s16",
            "auplay_format s16",
            "auenc_format s16",
            "audec_format s16",
            "audio_buffer 20-160",
            "audio_jitter_buffer_type fixed",
            "audio_jitter_buffer_ms 40-80",
        ] {
            put(line.into());
        }
        put(format!("webrtc_aec_delay_ms {}", s.aec_delay_ms));
        put(format!("ksip_aec_enabled {}", yes_no(s.aec)));
        put(format!("ksip_high_pass {}", yes_no(s.high_pass)));
        put(format!("ksip_noise_suppression {}", s.noise_suppression));
        put(format!("ksip_agc {}", yes_no(s.agc)));
        put(format!("ksip_register_interval {}", s.register_interval));
        put(format!("ksip_audio_codecs {}", s.codec_list().join(",")));
        put(format!("ksip_detail_log {}", yes_no(s.detail_log)));
        put(format!("ksip_sip_transport {}", s.sip_transport()));
        put(format!("ksip_mediaenc {}", s.mediaenc().unwrap_or("")));
        // Opus for voice on a wireless network: mono, 32 kbps, in-band FEC.
        for line in [
            "opus_stereo no",
            "opus_sprop_stereo no",
            "opus_bitrate 32000",
            "opus_inbandfec yes",
            "opus_packet_loss 10",
            "opus_dtx no",
            "opus_application voip",
        ] {
            put(line.into());
        }
        put(format!("ksip_microphone_gain {}", s.microphone_gain));
        put(format!("ksip_speaker_gain {}", s.speaker_gain));
        put(format!("rtp_ports {}-{}", s.rtp_port, s.rtp_port + 20));
        put("rtp_timeout 60".into());
        put(format!("ctrl_tcp_listen 127.0.0.1:{ctrl}"));
        for module in [
            "g711", "libg722", "opus", "wasapi", "ksip_audio", "postlab", "auconv", "auresamp",
            "ctrl_tcp", "menu", "srtp", "dtls_srtp", "ksip",
        ] {
            put(format!("module {module}.dll"));
        }
        if !adapter.is_empty() {
            put(format!("net_interface {adapter}"));
        }
        // Over TLS the server is always verified: against the chosen authority,
        // or, without one, against everything Windows trusts. The Windows store
        // is written out for each start, because it can change at any time.
        let mut trust_note = None;
        if s.transport().encrypts_signalling() {
            let chosen = s.ca_file.trim();
            let trust = if chosen.is_empty() {
                let store = crate::trust::windows_trust()?;
                trust_note = Some((store.included, store.left_out));
                let path = profile.join("windows-trust.pem");
                std::fs::write(&path, store.pem).map_err(err)?;
                path.to_string_lossy().replace('\\', "/")
            } else {
                chosen.replace('\\', "/")
            };
            put(format!("sip_cafile {trust}"));
            put("sip_verify_server yes".into());
        }
        // baresip only treats a path starting with "/" as absolute, so a chosen file
        // cannot be named directly on Windows. The sounds are replaced in place instead.
        if let Some(dir) = crate::native::sounds() {
            for (key, chosen) in Settings::SOUND_KEYS.iter().zip(s.sounds()) {
                if let Err(e) = self.replace_sound(&dir, key, chosen) {
                    notes.push(format!("ksip: {key} sound not replaced, {e}"));
                }
            }
            put(format!("audio_path {}", dir.to_string_lossy().replace('\\', "/")));
        }
        std::fs::write(profile.join("config"), config).map_err(err)?;
        notes.extend(endpoint_notes(s, &endpoints, devices));
        if let Some((included, left_out)) = trust_note {
            notes.push(format!("ksip: windows trust store, {included} certificates, {left_out} left out"));
        }
        Ok(Prepared {
            plan: StartPlan {
                exe,
                profile,
                control: reservation,
                credential_target: self.store.target.clone(),
            },
            endpoints,
            address,
            notes,
        })
    }
    /// Turns a recording that has just closed into an MP3, on a thread of
    /// its own with a lower priority, so that the next call is not disturbed.
    /// The encoder writes under a partial name, and the real name appears
    /// only once the file is finished: nothing reads a half-written MP3 as
    /// the recording, and an exit in the middle leaves the WAV as the one
    /// copy, which the next start converts. The WAV goes once the MP3 is
    /// there; if it is being played just then, it stays until the next start
    /// sweeps it away. A failure leaves the WAV.
    pub fn convert_recording(&self, wav: PathBuf) {
        let me = self.clone();
        me.converting.fetch_add(1, Ordering::Relaxed);
        thread::spawn(move || {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
            // SAFETY: the current thread's own priority is all that is touched.
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
            let mp3 = wav.with_extension("mp3");
            let partial = partial_recording(&wav);
            let name = wav.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // A recording the engine never closed says it holds no samples;
            // its header is put right from the file's length before encoding.
            if crate::wav::repair_sizes(&wav) == Ok(true) {
                me.log(LOG_APP, message_with("RECORDING_HEADER_REPAIRED", [&name]));
            }
            let finished = crate::mp3::transcode(&wav, &partial)
                .and_then(|()| std::fs::rename(&partial, &mp3).map_err(|e| message_with("RECORDING_CONVERT_FAILED", [err(e)])));
            match finished {
                Ok(()) => match std::fs::remove_file(&wav) {
                    Ok(()) => me.log(LOG_APP, message_with("RECORDING_CONVERTED", [&name])),
                    Err(_) => me.log(LOG_APP, message_with("RECORDING_WAV_KEPT", [&name])),
                },
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    me.log(LOG_APP, e);
                }
            }
            me.converting.fetch_sub(1, Ordering::Relaxed);
        });
    }
    /// What earlier runs left in the recordings folder, at start: finished
    /// conversions lose their WAV, unfinished ones lose their partial MP3, and
    /// a WAV without an MP3 is converted now.
    pub fn sweep_recordings(&self) {
        for wav in sweep_recording_folder(&self.data.join("recordings")) {
            self.convert_recording(wav);
        }
    }
    pub fn open_licenses(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.data).map_err(err)?;
        let path = self.data.join("THIRD_PARTY_NOTICES.txt");
        std::fs::write(&path, crate::licenses::text()?).map_err(err)?;
        Process::new("notepad.exe").arg(path).spawn().map_err(err)?;
        Ok(())
    }
    /// The folder everything the app writes goes into.
    pub fn data(&self) -> &Path {
        &self.data
    }
    pub fn open_recordings(&self) -> Result<(), String> {
        let path = self.data.join("recordings");
        std::fs::create_dir_all(&path).map_err(err)?;
        Process::new("explorer.exe")
            .arg(explorer_path(&path))
            .spawn()
            .map_err(err)?;
        Ok(())
    }
    pub fn open_sound_control(&self) -> Result<(), String> {
        Process::new("control.exe")
            .arg("mmsys.cpl")
            .spawn()
            .map_err(err)?;
        Ok(())
    }
    /// The stored password, when the account still belongs to the same
    /// authentication user. The vault holds that user and the password; the
    /// address lives in the registry, so changing it asks for nothing.
    pub fn password_for(&self, account: &Account) -> Result<String, String> {
        let previous = self
            .store
            .read_account()?
            .filter(|a| a.auth_user == account.auth_user.trim())
            .ok_or(message("ACCOUNT_PASSWORD_REQUIRED"))?;
        Ok(previous.password)
    }
    /// Writes the account and the settings, or leaves the store as it was.
    /// Both are several registry values and a credential written one by one,
    /// so a failure part way through would leave old and new mixed; the copies
    /// read first are then written back, every one of them, and only when
    /// that fails too is the person told to save again.
    pub fn persist_configuration(&self, settings: &Settings, account: &Account) -> Result<(), String> {
        let old_account = self.store.read_account()?;
        let old_settings = self.settings()?;
        let written = self
            .store
            .write_account(account)
            .and_then(|()| self.save_settings(settings));
        if let Err(e) = written {
            let restored = match &old_account {
                Some(previous) => self.store.write_account(previous),
                None => self.store.delete_account(),
            }
            .and_then(|()| self.save_settings(&old_settings));
            return Err(if restored.is_ok() { e } else { message_with("ACCOUNT_ROLLBACK_FAILED", [e]) });
        }
        Ok(())
    }
    /// Keeps the `ksip:` registration in step with the setting.
    pub fn apply_browser_integration(&self) {
        let Ok(settings) = self.settings() else {
            return;
        };
        // A test profile registers a scheme of its own, so that a test never
        // removes or redirects the registration the person's real KSIP has.
        let scheme = if self.store.target.starts_with("KSIP/Test/") { "ksip-test" } else { "ksip" };
        if let Err(e) = crate::protocol::register(settings.browser_integration, scheme) {
            self.log(LOG_APP, format!("ksip: browser integration {e}"));
        }
    }
}
/// What the window and the desktop hold: the services, the way into the
/// phone actor and the snapshot it publishes. Every question about the phone
/// is read from the snapshot; every change to it is a command to the actor.
#[derive(Clone)]
pub struct AppState {
    services: Services,
    phone: PhoneHandle,
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
    pub fn show_error(&self, error: String) {
        self.phone.send(Command::ShowError(error));
    }
    pub fn report_error(&self, error: String) {
        self.phone.send(Command::ReportError(error));
    }
    pub fn log_app(&self, text: String) {
        self.services.log_app(text);
    }
    pub fn log_protocol(&self, command: &str) {
        self.services.log_protocol(command);
    }
    pub fn log_ui(&self, text: String) {
        self.services.log_ui(text);
    }
    pub fn log_panic(&self, text: String) {
        self.services.log_panic(text);
    }
    pub fn clear_logs(&self) {
        self.services.clear_logs();
    }
    pub fn read_logs(&self, after: u64) -> LogPage {
        self.services.read_logs(after)
    }
    pub fn clear_call_history(&self) -> Result<(), String> {
        self.services.clear_call_history()
    }
    pub fn read_call_history(&self) -> Vec<CallHistory> {
        self.services.read_call_history()
    }
    pub fn open_recording_location(&self, name: &str) -> Result<(), String> {
        self.services.open_recording_location(name)
    }
    pub fn open_recording(&self, name: &str) -> Result<(), String> {
        self.services.open_recording(name)
    }
    /// The phone as the actor last published it, with the counters of the
    /// services laid over. Reading also moves the files along: the log is
    /// written from log(), so a quiet moment would leave the last lines only
    /// in memory, and what another process appended unseen; rows the history
    /// could not write earlier get another try. The first failure of either
    /// file is said once, in the log and in the window.
    pub fn snapshot(&self) -> Snapshot {
        let journal = {
            let mut logs = self.services.logs.lock().unwrap();
            if logs.flushed.elapsed() >= Duration::from_secs(1) {
                logs.sync(&self.services.data)
            } else {
                Ok(())
            }
        };
        let history = self.services.history.lock().unwrap().flush(&self.services.data);
        for e in [journal, history].into_iter().filter_map(Result::err) {
            self.show_error(message_with("JOURNAL_WRITE_FAILED", [e]));
        }
        let mut view = self.published.read();
        view.converting = self.services.converting.load(Ordering::Relaxed) as u32;
        view.log_sequence = self.services.logs.lock().unwrap().sequence;
        view.history_sequence = self.services.history.lock().unwrap().sequence;
        view
    }
    pub fn refresh_devices(&self) -> Result<(), String> {
        self.phone.call(Command::RefreshDevices)
    }
    /// Told by the desktop loop, which can see the window; the phone cannot.
    pub fn set_window_visible(&self, visible: bool) {
        self.phone.send(Command::WindowVisible(visible));
    }
    /// Reads a volume, or sets it. Reading asks Windows and lays the saved
    /// gain over; setting goes through the phone, since the engine and the
    /// settings take the change too.
    pub fn volume(&self, kind: &str, device: &str, level: Option<u16>, mute: Option<bool>) -> Result<Volume, String> {
        if !matches!(kind, "microphone" | "speaker") || level.is_some_and(|value| value > 200) {
            return Err(message("AUDIO_VOLUME_ARGUMENT_INVALID"));
        }
        if level.is_some() || mute.is_some() {
            let (kind, device) = (kind.to_string(), device.to_string());
            return self.phone.call(|reply| Command::SetVolume { kind, device, level, mute, reply });
        }
        let mut result = crate::audio::volume(kind, device, None, None)?;
        let settings = self.published.read().settings;
        let gain = if kind == "microphone" {
            settings.microphone_gain
        } else {
            settings.speaker_gain
        };
        if gain > 100 {
            result.level = gain;
        }
        Ok(result)
    }
    pub fn peak(&self, kind: &str, device: &str) -> Result<Peak, String> {
        crate::audio::peak(kind, device)
    }
    pub fn calibrate_aec(&self, microphone: &str, speaker: &str, careful: bool) -> Result<Calibration, String> {
        let (microphone, speaker) = (microphone.to_string(), speaker.to_string());
        self.phone.call(|reply| Command::CalibrateAec { microphone, speaker, careful, reply })
    }
    pub fn select_audio_device(&self, kind: &str, device: String) -> Result<(), String> {
        let kind = kind.to_string();
        self.phone.call(|reply| Command::SelectAudioDevice { kind, device, reply })
    }
    /// The first thing after the window is up: the link registration, the
    /// recordings earlier runs left, then the devices and the saved account.
    pub fn initialize(&self) {
        self.services.apply_browser_integration();
        self.services.sweep_recordings();
        self.phone.send(Command::Initialize);
    }
    pub fn connect(&self) -> Result<(), String> {
        self.phone.call(Command::Connect)
    }
    pub fn save_configuration(&self, settings: Settings, account: Account) -> Result<(), String> {
        self.phone.call(|reply| Command::SaveConfiguration { settings: Box::new(settings), account, reply })
    }
    pub fn action(&self, name: &str, id: &str, value: &str, line: u8) -> Result<String, String> {
        let (name, id, value) = (name.to_string(), id.to_string(), value.to_string());
        self.phone.call(|reply| Command::Action { name, id, value, line, reply })
    }
    pub fn open_licenses(&self) -> Result<(), String> {
        self.services.open_licenses()
    }
    /// The folder everything the app writes goes into.
    pub fn data(&self) -> &Path {
        self.services.data()
    }
    pub fn open_recordings(&self) -> Result<(), String> {
        self.services.open_recordings()
    }
    pub fn open_sound_control(&self) -> Result<(), String> {
        self.services.open_sound_control()
    }
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }
    /// Stops the engine through the actor, then gives a conversion still
    /// running a moment to finish. One that does not make it leaves its WAV
    /// and a partial MP3, and the next start converts the WAV again, so
    /// nothing is lost by leaving.
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.phone.call(Command::Shutdown);
        let end = Instant::now() + Duration::from_secs(5);
        while self.services.converting.load(Ordering::Relaxed) > 0 && Instant::now() < end {
            thread::sleep(Duration::from_millis(100));
        }
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
    use crate::phone_state::automatic_recording_target;
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
    fn a_save_that_fails_part_way_leaves_the_store_as_it_was() {
        let suffix = format!("test-rollback-{}", std::process::id());
        let mut app = Services::open().0;
        app.store = Store {
            key: format!(r"Software\KashiharaCity\ksip\Test\{suffix}"),
            target: format!("KSIP/Test/{suffix}"),
        };
        let account = |server: &str| Account {
            server: server.into(),
            port: 5060,
            extension: "1001".into(),
            auth_user: "1001".into(),
            password: "local-test-only".into(),
        };
        let settings = |codecs: &str| Settings { codecs: codecs.into(), ..Settings::default() };
        let result = (|| -> Result<(), String> {
            app.persist_configuration(&settings("opus"), &account("192.0.2.10"))?;
            // The codecs are a policy value, written after the account and the
            // document: failing there is the mixed state the rollback is for.
            crate::storage::fail_next_write_of("codecs");
            let failed = app.persist_configuration(&settings("PCMU"), &account("192.0.2.20"));
            crate::storage::fail_next_write_of("");
            assert!(failed.is_err(), "the injected failure must surface");
            let kept = app.store.read_account()?.expect("the account is still there");
            assert_eq!(kept.server, "192.0.2.10", "the old server is back");
            assert_eq!(app.settings()?.codecs, "opus", "the old codecs are back");
            // And with nothing in the way, the new values land whole.
            app.persist_configuration(&settings("PCMU"), &account("192.0.2.20"))?;
            assert_eq!(app.store.read_account()?.expect("account").server, "192.0.2.20");
            assert_eq!(app.settings()?.codecs, "PCMU");
            Ok(())
        })();
        app.store.cleanup_test();
        result.unwrap();
    }
    #[test]
    fn only_a_new_authentication_user_asks_for_the_password_again() {
        let suffix = format!("test-password-{}", std::process::id());
        let mut app = Services::open().0;
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
    #[ignore = "requires scripts/build/native.ps1; uses isolated temp/build/rust-engine-test"]
    fn real_engine_starts_stops_and_restarts() {
        use crate::engine_link::EngineLink;
        use crate::phone_message::{LinkBody, Message};
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let root = project.join("temp/build/rust-engine-test");
        let mut services = Services::open().0;
        services.data = root.join("data");
        services.store = Store {
            key: r"Software\KashiharaCity\ksip\Test\test-engine".into(),
            target: "KSIP/Test/test-engine".into(),
        };
        let settings = Settings {
            sip_port: 17060,
            rtp_port: 17100,
            ..Settings::default()
        };
        let devices = crate::audio::devices().unwrap();
        assert!(!devices.is_empty());
        let (tx, rx) = std::sync::mpsc::channel();
        for generation in 1..=2 {
            let account = Account {
                server: "127.0.0.1".into(),
                port: 5060,
                extension: "1001".into(),
                auth_user: "1001".into(),
                password: "secret".into(),
            };
            let prepared = services.prepare_start(&settings, &account, &devices).unwrap();
            let config = std::fs::read_to_string(services.profile_dir().join("config")).unwrap();
            assert!(config.contains(&format!(
                "audio_source ksip_audio,{}",
                prepared.endpoints.microphone
            )));
            assert!(config.contains(&format!("audio_player ksip_audio,{}", prepared.endpoints.speaker)));
            assert!(config.contains("webrtc_aec_delay_ms 20"));
            assert!(config.contains("callwaiting_aufile none"));
            assert!(config.contains("ksip_audio_codecs opus,G722,PCMU,PCMA"));
            assert!(config.contains("ksip_aec_enabled yes"));
            assert!(config.contains("ksip_high_pass yes"));
            assert!(config.contains("ksip_noise_suppression high"));
            assert!(config.contains("ksip_agc yes"));
            assert!(config.contains("ksip_microphone_gain 100"));
            assert!(config.contains("ksip_speaker_gain 100"));
            let mut link = EngineLink::start(prepared.plan, generation, tx.clone()).unwrap();
            assert_eq!(link.generation(), generation);
            // A request is answered on the queue, under this engine's generation
            // and the request's token, in receive order.
            let token = link.send("lab_stop", "").unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut last_seq = 0;
            loop {
                let message = rx.recv_timeout(deadline - Instant::now().min(deadline)).expect("an answer in time");
                let Message::Link(delivered) = message else {
                    continue;
                };
                assert_eq!(delivered.generation, generation);
                assert!(delivered.seq > last_seq, "receive numbers only grow");
                last_seq = delivered.seq;
                if let LinkBody::Response { token: t, value } = delivered.body {
                    assert_eq!(t, token);
                    assert_eq!(value["ok"], true);
                    break;
                }
            }
            let report = link.stop().unwrap();
            assert!(!report.forced, "the engine quits when asked");
            // The reader thread says the connection is gone once the engine has.
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let message = rx.recv_timeout(deadline - Instant::now().min(deadline)).expect("the loss is reported");
                if let Message::Link(delivered) = message {
                    if matches!(delivered.body, LinkBody::Lost) {
                        assert_eq!(delivered.generation, generation);
                        break;
                    }
                }
            }
        }
    }
    #[test]
    fn webrtc_audio_processing_statistics_are_parsed_from_native_state() {
        let state: EngineReport = serde_json::from_str(
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
    fn reject_config_injection_and_port_overlap() {
        let rejected = |s: Settings| Services::validate(&s).is_err();
        assert!(rejected(Settings { microphone: "default\nmodule evil".into(), ..Settings::default() }));
        assert!(rejected(Settings { sip_port: 10000, rtp_port: 10000, ..Settings::default() }));
        assert!(rejected(Settings { aec_delay_ms: 501, ..Settings::default() }));
        assert!(rejected(Settings { noise_suppression: "loud".into(), ..Settings::default() }));
        assert!(rejected(Settings { ca_file: "C:\\ca.pem\nsip_verify_server no".into(), ..Settings::default() }));
        assert!(Services::validate(&Settings::default()).is_ok());
    }
    #[test]
    fn the_recordings_folder_is_tidied_and_leftover_wavs_are_named() {
        let folder = std::env::temp_dir().join(format!("ksip-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let file = |name: &str| {
            std::fs::write(folder.join(name), b"x").unwrap();
            folder.join(name)
        };
        // Finished: the WAV goes. Unfinished: the partial goes, the WAV is handed back.
        // Never started: handed back. A finished MP3 alone stays as it is.
        file("2026-09-26_10-00-00_1002.wav");
        file("2026-09-26_10-00-00_1002.mp3");
        file("2026-09-26_10-05-00_1003.wav");
        file("2026-09-26_10-05-00_1003.converting.mp3");
        file("2026-09-26_10-10-00_1004.wav");
        file("2026-09-26_10-15-00_1005.mp3");
        let leftover = sweep_recording_folder(&folder);
        assert_eq!(
            leftover,
            vec![folder.join("2026-09-26_10-05-00_1003.wav"), folder.join("2026-09-26_10-10-00_1004.wav")]
        );
        let mut names: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "2026-09-26_10-00-00_1002.mp3",
                "2026-09-26_10-05-00_1003.wav",
                "2026-09-26_10-10-00_1004.wav",
                "2026-09-26_10-15-00_1005.mp3"
            ]
        );
        assert!(sweep_recording_folder(&folder.join("missing")).is_empty());
        assert_eq!(partial_recording(Path::new("a/b.wav")), PathBuf::from("a/b.converting.mp3"));
        let _ = std::fs::remove_dir_all(&folder);
    }
    #[test]
    fn transports_and_encryptions_are_read_from_the_settings_text() {
        assert_eq!(Transport::parse(""), Some(Transport::Udp));
        assert_eq!(Transport::parse(" TLS "), Some(Transport::Tls));
        assert_eq!(Transport::parse("ssl"), None);
        assert_eq!(Transport::Tcp.engine_name(), "TCP");
        assert!(Transport::Tls.encrypts_signalling() && !Transport::Tcp.encrypts_signalling());
        assert_eq!(MediaEncryption::parse(""), Some(MediaEncryption::None));
        assert_eq!(MediaEncryption::parse("SDES"), Some(MediaEncryption::Sdes));
        assert_eq!(MediaEncryption::parse("srtp"), None, "baresip's own name is not a setting");
        assert_eq!(MediaEncryption::Sdes.engine_name(), Some("srtp-mand"));
        assert_eq!(MediaEncryption::Osrtp.engine_name(), Some("srtp"));
        assert_eq!(MediaEncryption::Dtls.engine_name(), Some("dtls_srtp"));
        assert_eq!(MediaEncryption::None.engine_name(), None);
        assert!(MediaEncryption::Sdes.keys_in_signalling() && MediaEncryption::Osrtp.keys_in_signalling());
        assert!(!MediaEncryption::Dtls.keys_in_signalling());
        let s = Settings { transport: "tls".into(), media_encryption: "osrtp".into(), ..Settings::default() };
        assert_eq!((s.transport(), s.media_encryption(), s.sip_transport(), s.mediaenc()), (Transport::Tls, MediaEncryption::Osrtp, "TLS", Some("srtp")));
    }
    #[test]
    fn encryption_that_needs_tls_and_unknown_values_are_refused() {
        let with = |transport: &str, media: &str| Settings {
            transport: transport.into(),
            media_encryption: media.into(),
            ..Settings::default()
        };
        for (transport, media) in [("", ""), ("udp", ""), ("tcp", "dtls"), ("tls", "sdes"), ("tls", "osrtp"), ("tls", "dtls")] {
            assert!(Services::validate(&with(transport, media)).is_ok(), "{transport} {media}");
        }
        // SDES and OSRTP carry their keys in the signalling: TLS or nothing.
        for (transport, media) in [("", "sdes"), ("udp", "sdes"), ("tcp", "osrtp"), ("udp", "osrtp")] {
            assert_eq!(Services::validate(&with(transport, media)), Err(message("SETTINGS_SDES_NEEDS_TLS")), "{transport} {media}");
        }
        // A value this version does not know is refused, not read as the weak default.
        assert_eq!(Services::validate(&with("ssl", "")), Err(message("SETTINGS_TRANSPORT_INVALID")));
        assert_eq!(Services::validate(&with("tls", "srtp")), Err(message("SETTINGS_MEDIA_ENCRYPTION_INVALID")));
        assert_eq!(Services::validate(&with("tls", "srtp-mand")), Err(message("SETTINGS_MEDIA_ENCRYPTION_INVALID")));
        // Policy text is normalised on the way in, so "TLS" and " sdes " are the known values.
        let mut policy = Settings::default();
        policy.read_policy(|key| match key { "transport" => " TLS ".into(), "media_encryption" => "SDES".into(), _ => String::new() });
        assert_eq!((policy.transport.as_str(), policy.media_encryption.as_str()), ("tls", "sdes"));
        assert!(Services::validate(&policy).is_ok());
    }
    #[test]
    fn custom_buttons_are_checked_and_resolved() {
        let button = |kind: &str, number: &str, transfer: &str| CustomButton {
            title: "test".into(),
            kind: kind.into(),
            number: number.into(),
            transfer: transfer.into(),
            pickup: String::new(),
        };
        let with = |buttons: Vec<CustomButton>| Settings { buttons, ..Settings::default() };
        // The pickup is checked like a number; the window itself falls back to the number.
        let pickup = CustomButton { pickup: "*8701".into(), ..button("dial", "701", "") };
        assert!(Services::validate(&with(vec![pickup])).is_ok());
        assert!(Services::validate(&with(vec![CustomButton { pickup: "70 1".into(), ..button("dial", "701", "") }])).is_err());
        // A voicemail button names the number that plays the messages.
        assert!(Services::validate(&with(vec![button("mwi", "*97", "")])).is_ok());
        assert!(Services::validate(&with(vec![button("mwi", "", "")])).is_err());
        // A do-not-disturb switch names nothing.
        assert!(Services::validate(&with(vec![button("dnd", "", "")])).is_ok());
        assert!(Services::validate(&with(vec![button("dnd", "701", "")])).is_err());
        let ok = with(vec![button("park", "701", "*701"), button("dial", "1002", ""), button("transfer", "9001", "")]);
        assert!(Services::validate(&ok).is_ok());
        assert_eq!(ok.watched_numbers(), vec!["701", "1002"]);
        assert_eq!(ok.buttons[0].transfer_target(), Some("*701"));
        assert_eq!(ok.buttons[1].transfer_target(), None);
        assert_eq!(ok.buttons[2].transfer_target(), Some("9001"));
        assert_eq!(button("park", "701", "").transfer_target(), Some("701"));
        // What is refused: a kind that is not one of the three, a title too
        // long, a number outside the SIP user alphabet, and two watchers of
        // one number. An unused button may leave its number empty.
        assert!(Services::validate(&with(vec![button("hold", "701", "")])).is_err());
        assert!(Services::validate(&with(vec![CustomButton { title: "x".repeat(41), ..button("dial", "1", "") }])).is_err());
        assert!(Services::validate(&with(vec![button("dial", "70 1", "")])).is_err());
        assert!(Services::validate(&with(vec![button("dial", "", "")])).is_err());
        assert!(Services::validate(&with(vec![button("dial", "701", ""), button("park", "701", "")])).is_err());
        assert!(Services::validate(&with(vec![button("", "", "")])).is_ok());
        // A full SIP URI is accepted for either address, with or without the
        // angle brackets a Refer-To header would carry. It is matched bare and
        // sent as it was written.
        let odd = with(vec![button("park", "61", "<sip:61@127.0.0.1>"), button("dial", " sip:sales@pbx.example ", "")]);
        assert!(Services::validate(&odd).is_ok());
        assert_eq!(odd.buttons[0].transfer_target(), Some("sip:61@127.0.0.1"));
        assert_eq!(odd.buttons[0].transfer_text(), Some("<sip:61@127.0.0.1>"));
        assert_eq!(button("park", "701", "").transfer_text(), Some("701"));
        assert_eq!(odd.buttons[0].dial_target(), Some("61"));
        assert_eq!(odd.watched_numbers(), vec!["61", "sip:sales@pbx.example"]);
        assert!(Services::validate(&with(vec![button("transfer", "sip:61@pbx with space", "")])).is_err());
        // A dial without BLF is not watched, so its number may be written as the
        // dial box takes it: RFC 3966 separators, a leading +, a tel: scheme.
        // Letters are still no number, and an empty one is refused like any other.
        let speed = with(vec![button("speed", "06-1234-5678", ""), button("dial", "1002", "")]);
        assert!(Services::validate(&speed).is_ok());
        assert_eq!(speed.watched_numbers(), vec!["1002"]);
        assert_eq!(speed.buttons[0].transfer_target(), None);
        assert_eq!(speed.buttons[0].dial_target(), None);
        assert!(Services::validate(&with(vec![button("speed", "tel:+81-6-1234-5678", "")])).is_ok());
        assert!(Services::validate(&with(vec![button("speed", "<sip:sales@pbx.example>", "")])).is_ok());
        assert!(Services::validate(&with(vec![button("speed", "sales", "")])).is_err());
        assert!(Services::validate(&with(vec![button("speed", "", "")])).is_err());
        // A link button holds a web address and nothing the engine would dial.
        let page = with(vec![button("open", " https://pbx.example/extensions ", "")]);
        assert!(Services::validate(&page).is_ok());
        assert_eq!(page.buttons[0].link_target(), Some("https://pbx.example/extensions"));
        assert_eq!(page.buttons[0].dial_target(), None);
        assert_eq!(page.buttons[0].transfer_target(), None);
        assert!(Services::validate(&with(vec![button("open", "ftp://pbx.example/", "")])).is_err());
        assert!(Services::validate(&with(vec![button("open", "https://pbx.example/a b", "")])).is_err());
        assert!(Services::validate(&with(vec![button("dial", "https://pbx.example/", "")])).is_err());
        // The tray delay is -1 (never) or up to an hour.
        assert!(Services::validate(&Settings { tray_after_call: 0, ..Settings::default() }).is_ok());
        assert!(Services::validate(&Settings { tray_after_call: -2, ..Settings::default() }).is_err());
        assert!(Services::validate(&Settings { tray_after_call: 3601, ..Settings::default() }).is_err());
        assert!(Services::validate(&with(vec![button("transfer", &format!("sip:{}@pbx", "6".repeat(200)), "")])).is_err());
        // The policy values round-trip through their registry names.
        let mut read_back = Settings::default();
        let stored: std::collections::HashMap<String, String> = ok.policy_values().into_iter().collect();
        read_back.read_policy(|key| stored.get(key).cloned().unwrap_or_default());
        assert_eq!(read_back.buttons[..3], ok.buttons[..3]);
        assert!(read_back.buttons[3..].iter().all(|b| !b.configured()));
    }
    #[test]
    fn reject_invalid_volume_without_launching_helper() {
        let app = AppState::new();
        assert!(app.volume("other", "default", None, None).is_err());
        assert!(app.volume("speaker", "default", Some(201), None).is_err());
        assert!(app.volume("microphone", "bad\0id", None, None).is_err());
        assert!(app.peak("other", "default").is_err());
        assert!(app.peak("microphone", "bad\0id").is_err());
    }
    #[test]
    fn logs_move_by_sequence_and_reset_when_lines_are_dropped() {
        let app = Services::open().0;
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
        assert_eq!(app.logs.lock().unwrap().sequence, LOG_LIMIT as u64 + 2);
    }
    #[test]
    fn ui_and_panic_lines_are_labelled_and_kept_readable() {
        let app = Services::open().0;
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
    fn codecs_are_offered_in_the_chosen_order_and_all_by_default() {
        let mut s = Settings::default();
        assert_eq!(s.codec_list(), ["opus", "G722", "PCMU", "PCMA"]);
        s.codecs = "PCMU, opus".into();
        assert_eq!(s.codec_list(), ["PCMU", "opus"]);
        assert!(Services::validate(&s).is_ok());
        s.codecs = "PCMU,PCMU".into();
        assert!(Services::validate(&s).is_err(), "a codec named twice is refused");
        s.codecs = "G729".into();
        assert!(Services::validate(&s).is_err(), "a codec the app does not have is refused");
    }
    #[test]
    fn a_chosen_sound_replaces_the_built_in_one() {
        let app = Services::open().0;
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
    fn caller_label_puts_the_name_before_the_number() {
        let mut call = CallInfo {
            id: "c".into(),
            peer: "sip:1001@192.0.2.10;transport=udp".into(),
            name: String::new(),
            state: "INCOMING".into(),
            held: false,
            duration: 0,
            codec: String::new(),
            secure: false,
            transport: "UDP".into(),
            line: 1,
        };
        assert_eq!(caller_label(&call), "1001");
        call.name = "部署名".into();
        assert_eq!(caller_label(&call), "部署名 1001");
        assert_eq!(peer_number("<sips:117@pbx>"), "117");
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
        assert!(settings.high_pass);
        assert_eq!(settings.noise_suppression, "high");
        assert!(settings.agc);
        assert_eq!(settings.register_interval, 300);
        assert_eq!(settings.tray_after_call, -1);
        assert!(!settings.detail_log);
        assert!(!settings.auto_record);
        assert!(!settings.auto_answer);
        assert!(settings.buttons.iter().all(|b| !b.configured()));
        let calls = vec![
            CallInfo {
                id: "held".into(),
                peer: "sip:1@local".into(),
                name: String::new(),
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
                name: String::new(),
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
    fn a_number_is_reduced_to_what_the_registrar_dials() {
        assert_eq!(dial_target("9001"), Ok("9001".into()));
        assert_eq!(dial_target(" (06) 1234.5678 "), Ok("0612345678".into()));
        assert_eq!(dial_target("+81-6-1234-5678"), Ok("+81612345678".into()));
        assert_eq!(dial_target("*21#"), Ok("*21#".into()));
        assert_eq!(dial_target("tel:+81-6-1234-5678"), Ok("+81612345678".into()));
        assert_eq!(dial_target("TEL:(06) 1234-5678"), Ok("0612345678".into()));
        assert_eq!(dial_target("<sip:1001@pbx.example>"), Ok("sip:1001@pbx.example".into()));
        assert_eq!(dial_target("SIPS:1001@pbx.example"), Ok("SIPS:1001@pbx.example".into()));
        for wrong in ["", "+", "answer", "90 0a", "１２３", "12+34", "1_2", "sip:10 01@pbx", "sip:１@pbx"] {
            assert_eq!(dial_target(wrong), Err(message("DIAL_TARGET_INVALID")), "{wrong}");
        }
        assert!(dial_target(&"1".repeat(31)).is_err());
        assert!(dial_target(&format!("sip:{}@pbx", "1".repeat(200))).is_err());
        // A button keeps its number as dialled, so what it watches is what it says.
        assert!(CustomButton::target_ok(""));
        assert!(CustomButton::target_ok("*701"));
        assert!(CustomButton::target_ok(" <sip:61@pbx.example> "));
        assert!(!CustomButton::target_ok("70-1"));
        assert!(!CustomButton::target_ok("voicemail"));
    }

    #[test]
    fn the_log_file_is_appended_to_and_shared_with_another_process() {
        let dir = std::env::temp_dir().join(format!("ksip-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        let mut logs = Logs::open(&dir);
        logs.push(LogLine::new("t1".into(), LOG_APP, "one".into()));
        logs.push(LogLine::new("t2".into(), LOG_APP, "two".into()));
        logs.sync(&dir).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), 2, "{written}");
        assert!(written.lines().all(|l| l.starts_with('{') && l.ends_with('}')));
        // Another process appends a line; the next look brings it into the tab,
        // and this process's own new line is appended after it.
        append_log(&dir, "protocol ksip:x could not be handed over".into());
        logs.push(LogLine::new("t3".into(), LOG_APP, "three".into()));
        logs.sync(&dir).unwrap();
        let texts: Vec<&str> = logs.entries.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "protocol ksip:x could not be handed over", "three"]);
        assert_eq!(logs.sequence, 4);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 4);
        // Nothing is written when nothing is new, and the file is only rewritten
        // once it holds twice the limit, then with what the tab holds.
        let size = std::fs::metadata(&path).unwrap().len();
        logs.sync(&dir).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        // Lines arrive in bursts between syncs; the file grows past twice the
        // limit on the third burst and is then written afresh.
        let burst = 700;
        for i in 0..3 * burst {
            logs.push(LogLine::new("t".into(), LOG_APP, format!("line {i}")));
            if (i + 1) % burst == 0 {
                logs.sync(&dir).unwrap();
                let lines = std::fs::read_to_string(&path).unwrap().lines().count();
                assert_eq!(lines, if i + 1 < 3 * burst { 4 + i + 1 } else { LOG_LIMIT }, "after line {i}");
            }
        }
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(rewritten.lines().last().unwrap().contains(&format!("line {}", 3 * burst - 1)));
        // The file's own last lines are kept, four of which came before the bursts.
        assert!(rewritten.lines().next().unwrap().contains(&format!("\"line {}\"", 3 * burst - LOG_LIMIT)));
        // A second look at the same file starts from where it ends.
        let again = Logs::open(&dir);
        assert_eq!(again.file_lines, LOG_LIMIT);
        assert_eq!(again.offset, std::fs::metadata(&path).unwrap().len());
        // With the detail log on the file keeps ten times as much: the same
        // bursts bring no rewrite, and turning detail off brings one.
        logs.set_detail(true);
        for i in 0..3 * burst {
            logs.push(LogLine::new("t".into(), LOG_APP, format!("more {i}")));
            if (i + 1) % burst == 0 {
                logs.sync(&dir).unwrap();
            }
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), LOG_LIMIT + 3 * burst);
        logs.set_detail(false);
        logs.sync(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), LOG_LIMIT);
        logs.clear(&dir);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!((logs.sequence, logs.file_lines, logs.offset), (0, 0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_data_folder_is_the_persons_own_beside_the_registry_settings() {
        let store = Store {
            key: String::new(),
            target: "KSIP/SIP/default".into(),
        };
        let data = data_dir(&store);
        let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA"));
        assert_eq!(data, local.join("KashiharaCity").join("ksip"));
        let test = Store {
            key: String::new(),
            target: "KSIP/Test/test-unit".into(),
        };
        assert!(data_dir(&test).ends_with("temp/build/test-unit"), "{}", data_dir(&test).display());
    }

    #[test]
    fn a_recording_is_named_after_its_start_and_the_peer() {
        assert_eq!(recording_name("2026-09-23T14:30:12+09:00", "sip:1002@pbx.example:5060;transport=udp"), "2026-09-23_14-30-12_1002.wav");
        assert_eq!(recording_name("2026-09-23T14:30:12+09:00", "<sips:+81-6-1234@pbx.example>"), "2026-09-23_14-30-12_+81-6-1234.wav");
        assert_eq!(recording_name("2026-09-23T14:30:12+09:00", "sip:a b*c@pbx"), "2026-09-23_14-30-12_abc.wav");
        assert_eq!(recording_name("2026-09-23T14:30:12+09:00", ""), "2026-09-23_14-30-12_call.wav");
    }

    #[test]
    fn lines_that_could_not_be_written_wait_for_the_next_sync() {
        let dir = std::env::temp_dir().join(format!("ksip-journal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A folder where the file should be: every append fails.
        std::fs::create_dir_all(dir.join(LOG_FILE)).unwrap();
        std::fs::create_dir_all(dir.join(HISTORY_FILE)).unwrap();
        let mut logs = Logs::open(&dir);
        logs.push(LogLine::new("2026-09-26T22:00:00+09:00".into(), LOG_APP, "first".into()));
        logs.push(LogLine::new("2026-09-26T22:00:01+09:00".into(), LOG_APP, "second".into()));
        assert!(logs.sync(&dir).is_err(), "the first failure is reported");
        assert_eq!(logs.unwritten, 2, "the lines wait");
        assert!(logs.sync(&dir).is_ok(), "the same failure is not reported again");
        assert_eq!(logs.unwritten, 2);
        let mut history = History::open(&dir);
        let row = |n: u64| CallHistory { ended_at: n, direction: "OUTGOING".into(), peer: format!("sip:{n}@pbx"), name: String::new(), duration: 1, recording: String::new(), outcome: String::new() };
        assert!(history.add(&dir, vec![row(1)]).is_err());
        assert_eq!(history.rows.len(), 1, "the tab shows the row all the same");
        assert_eq!(history.pending.len(), 1, "the row waits for the file");
        // The way is clear again: everything that waited is written, in order,
        // and the log notes its own recovery.
        std::fs::remove_dir(dir.join(LOG_FILE)).unwrap();
        std::fs::remove_dir(dir.join(HISTORY_FILE)).unwrap();
        assert!(logs.sync(&dir).is_ok());
        assert_eq!(logs.unwritten, 1, "the recovery note itself is written with the next sync");
        assert!(logs.sync(&dir).is_ok());
        assert_eq!(logs.unwritten, 0);
        let written = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap();
        assert_eq!(written.lines().count(), 3);
        assert!(written.lines().next().unwrap().contains("first"));
        assert!(written.lines().last().unwrap().contains("JOURNAL_WRITE_RECOVERED"));
        assert!(history.add(&dir, vec![row(2)]).is_ok());
        assert!(history.pending.is_empty());
        let reread = History::open(&dir);
        assert_eq!(reread.rows.iter().map(|r| r.ended_at).collect::<Vec<_>>(), vec![2, 1], "both rows, newest first");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn the_call_history_is_appended_to_and_read_back_newest_first() {
        let dir = std::env::temp_dir().join(format!("ksip-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let call = |n: u64| CallHistory {
            ended_at: n,
            direction: "OUTGOING".into(),
            peer: format!("sip:{n}@pbx"),
            name: String::new(),
            duration: 1,
            recording: String::new(),
            outcome: String::new(),
        };
        let path = dir.join(HISTORY_FILE);
        let mut history = History::open(&dir);
        assert!(history.rows.is_empty());
        history.add(&dir, vec![call(1)]).unwrap();
        history.add(&dir, vec![call(2)]).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), 2);
        assert!(written.lines().next().unwrap().contains("\"ended_at\":1"), "oldest first in the file");
        // Calls that end are appended, in the order the tab shows them; the
        // next start reads the file and shows the same order.
        history.add(&dir, vec![call(3), call(4)]).unwrap();
        let ended: Vec<u64> = history.rows.iter().map(|r| r.ended_at).collect();
        assert_eq!(ended, [3, 4, 2, 1]);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 4);
        let again = History::open(&dir);
        assert_eq!(again.rows.iter().map(|r| r.ended_at).collect::<Vec<_>>(), [3, 4, 2, 1]);
        assert_eq!(again.file_lines, 4);
        // Past twice the limit the file keeps its last lines: with four lines
        // already there, the 1997th call takes it to 2001, and three follow.
        for n in 0..2 * HISTORY_LIMIT as u64 {
            history.add(&dir, vec![call(100 + n)]).unwrap();
        }
        assert_eq!(history.file_lines, HISTORY_LIMIT + 3);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), HISTORY_LIMIT + 3);
        assert_eq!(history.rows.len(), HISTORY_LIMIT);
        assert_eq!(history.rows[0].ended_at, 100 + 2 * HISTORY_LIMIT as u64 - 1);
        history.clear(&dir).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert!(history.rows.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }


    #[test]
    fn a_message_summary_is_read_into_counts() {
        let summary = "Messages-Waiting: yes\r\nMessage-Account: sip:1001@pbx\r\nVoice-Message: 2/5 (0/1)\r\n";
        assert_eq!(Mwi::parse(summary), Mwi { waiting: true, new: 2, old: 5 });
        assert_eq!(Mwi::parse("Messages-Waiting: no\r\nVoice-Message: 0/3\r\n"), Mwi { waiting: false, new: 0, old: 3 });
        assert_eq!(Mwi::parse(""), Mwi::default());
        assert_eq!(Mwi::parse("messages-waiting: YES\n"), Mwi { waiting: true, new: 0, old: 0 });
    }


}
