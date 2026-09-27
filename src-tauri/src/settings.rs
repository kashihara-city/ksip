//! The settings: what each is, the custom buttons, the transport and the
//! media encryption, what is valid, and how the settings and the account are
//! stored: every setting in a registry value of its own, named as the field,
//! so that a group policy (docs/admx) can set any of them one at a time.
use crate::app::Services;
use crate::logs::LOG_APP;
use crate::message::{message, message_with};
use crate::storage::{Account, Store, StoredValue, ACCOUNT_VALUES};
use serde::{Deserialize, Serialize};

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
            // Off unless asked for: the digital gain lifts quiet rooms and their
            // noise alike, and the microphone volume is the person's own to set.
            agc: false,
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
/// The registry value that stands while a save is under way or has failed to
/// be put back; see persist_configuration.
pub const SAVE_MARK: &str = "SaveInProgress";
impl Settings {
    /// The five values of each custom button: `button_1_title` to
    /// `button_30_pickup`. 1 to 6 sit on the phone, 7 to 30 in the panel
    /// beside it.
    pub const BUTTON_FIELDS: [&'static str; 5] = ["title", "kind", "number", "transfer", "pickup"];
    /// Every setting as the registry holds it: its value name (the field's
    /// name) and its value. Numbers and switches are REG_DWORD (a switch 1
    /// or 0; `tray_after_call`'s -1 as 0xFFFFFFFF), the rest REG_SZ. The
    /// order is the order they are written in.
    pub fn stored_values(&self) -> Vec<(String, StoredValue)> {
        use StoredValue::{Number, Text};
        let flag = |on: bool| Number(u32::from(on));
        let text = |value: &String| Text(value.clone());
        let mut values: Vec<(String, StoredValue)> = [
            ("network_adapter", text(&self.network_adapter)),
            ("sip_port", Number(self.sip_port.into())),
            ("rtp_port", Number(self.rtp_port.into())),
            ("microphone", text(&self.microphone)),
            ("speaker", text(&self.speaker)),
            ("microphone_gain", Number(self.microphone_gain.into())),
            ("speaker_gain", Number(self.speaker_gain.into())),
            ("auto_record", flag(self.auto_record)),
            ("transport", text(&self.transport)),
            ("ca_file", text(&self.ca_file)),
            ("media_encryption", text(&self.media_encryption)),
            ("codecs", text(&self.codecs)),
            ("auto_answer", flag(self.auto_answer)),
            ("aec", flag(self.aec)),
            ("aec_delay_ms", Number(self.aec_delay_ms.into())),
            ("high_pass", flag(self.high_pass)),
            ("noise_suppression", text(&self.noise_suppression)),
            ("agc", flag(self.agc)),
            ("register_interval", Number(self.register_interval.into())),
            ("detail_log", flag(self.detail_log)),
            ("browser_integration", flag(self.browser_integration)),
            ("browser_dial_confirm", flag(self.browser_dial_confirm)),
            ("shortcut_window", text(&self.shortcut_window)),
            ("shortcut_call", text(&self.shortcut_call)),
            ("incoming_action", text(&self.incoming_action)),
            // Two's complement: -1 (never) is 0xFFFFFFFF, read back as -1.
            ("tray_after_call", Number(self.tray_after_call as u32)),
            ("language", text(&self.language)),
            ("sound_ring", text(&self.sound_ring)),
            ("sound_ringback", text(&self.sound_ringback)),
            ("sound_busy", text(&self.sound_busy)),
            ("sound_notfound", text(&self.sound_notfound)),
            ("sound_error", text(&self.sound_error)),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect();
        let empty = CustomButton::default();
        for index in 0..CustomButton::COUNT {
            let button = self.buttons.get(index).unwrap_or(&empty);
            for (field, value) in Self::BUTTON_FIELDS.iter().zip([
                &button.title,
                &button.kind,
                &button.number,
                &button.transfer,
                &button.pickup,
            ]) {
                values.push((format!("button_{}_{field}", index + 1), text(value)));
            }
        }
        values
    }
    /// The settings from their registry values. A value that is absent, or
    /// text that is empty, leaves the setting at its default: a site's policy
    /// sets only what it cares about, and the first start finds nothing. The
    /// surrounding blanks of text, and the case of the names a setting picks
    /// from (transport, encryption, button kind), carry no meaning. A switch
    /// is 1 or 0, or `true`/`false`, `yes`/`no`, `on`/`off` as text; a number
    /// may be text of digits. A value that cannot be read as its setting is
    /// named in the second part of the result, and that setting keeps its
    /// default; the caller decides what that means (see Services::settings).
    pub fn read_stored(read: impl Fn(&str) -> Result<Option<StoredValue>, String>) -> (Self, Vec<String>) {
        let mut reader = StoredReader { read, unreadable: Vec::new() };
        let mut s = Self::default();
        let r = &mut reader;
        r.text("network_adapter", &mut s.network_adapter);
        r.number("sip_port", &mut s.sip_port);
        r.number("rtp_port", &mut s.rtp_port);
        r.text("microphone", &mut s.microphone);
        r.text("speaker", &mut s.speaker);
        r.number("microphone_gain", &mut s.microphone_gain);
        r.number("speaker_gain", &mut s.speaker_gain);
        r.flag("auto_record", &mut s.auto_record);
        r.text("transport", &mut s.transport);
        s.transport = s.transport.to_ascii_lowercase();
        r.text("ca_file", &mut s.ca_file);
        r.text("media_encryption", &mut s.media_encryption);
        s.media_encryption = s.media_encryption.to_ascii_lowercase();
        r.text("codecs", &mut s.codecs);
        r.flag("auto_answer", &mut s.auto_answer);
        r.flag("aec", &mut s.aec);
        r.number("aec_delay_ms", &mut s.aec_delay_ms);
        r.flag("high_pass", &mut s.high_pass);
        r.text("noise_suppression", &mut s.noise_suppression);
        r.flag("agc", &mut s.agc);
        r.number("register_interval", &mut s.register_interval);
        r.flag("detail_log", &mut s.detail_log);
        r.flag("browser_integration", &mut s.browser_integration);
        r.flag("browser_dial_confirm", &mut s.browser_dial_confirm);
        r.text("shortcut_window", &mut s.shortcut_window);
        r.text("shortcut_call", &mut s.shortcut_call);
        r.text("incoming_action", &mut s.incoming_action);
        r.signed("tray_after_call", &mut s.tray_after_call);
        r.text("language", &mut s.language);
        r.text("sound_ring", &mut s.sound_ring);
        r.text("sound_ringback", &mut s.sound_ringback);
        r.text("sound_busy", &mut s.sound_busy);
        r.text("sound_notfound", &mut s.sound_notfound);
        r.text("sound_error", &mut s.sound_error);
        for (index, button) in s.buttons.iter_mut().enumerate() {
            let name = |field: &str| format!("button_{}_{field}", index + 1);
            r.text(&name("title"), &mut button.title);
            r.text(&name("kind"), &mut button.kind);
            button.kind = button.kind.to_ascii_lowercase();
            r.text(&name("number"), &mut button.number);
            r.text(&name("transfer"), &mut button.transfer);
            r.text(&name("pickup"), &mut button.pickup);
        }
        (s, reader.unreadable)
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
/// The settings as stored: a value that cannot be read as its setting is an
/// error naming it, the same rule as for a value out of range.
pub fn stored_settings(store: &Store) -> Result<Settings, String> {
    let (settings, unreadable) = Settings::read_stored(|name| store.read_value(name));
    if !unreadable.is_empty() {
        return Err(message_with("SETTINGS_VALUE_INVALID", [unreadable.join(", ")]));
    }
    Ok(settings)
}
/// Everything a connect needs before it starts the engine, checked in the
/// order a connect checks it: a save that did not finish, a credential, an
/// account that SIP takes, settings that can be read and pass. The connect
/// (prepare_restart) and the export both come here, so that what the export
/// calls valid is what a connect would go ahead with.
pub fn connect_prerequisites(store: &Store) -> Result<(Account, Settings), String> {
    if !store.read_text(SAVE_MARK).is_empty() {
        return Err(message("SETTINGS_SAVE_INTERRUPTED"));
    }
    let mut account = store.read_account()?.ok_or(message("SIP_ACCOUNT_REQUIRED"))?;
    account.validate()?;
    let settings = stored_settings(store)?;
    // What is stored may not have passed through the dialog (a policy, a
    // hand-edited registry, an older version's values), so it is checked
    // here as well before the engine is started with it.
    validate(&settings)?;
    Ok((account, settings))
}
/// What KSIP makes of the settings in `store`, as a JSON document:
/// - `settings`: the settings as it reads them (defaults filled in, names
///   normalised); `account`: the registry's server, port and extension as a
///   connect reads them, and whether a credential is there (never the
///   password), or why the credential could not be read;
/// - `unreadable`: the values it could not read as what they are for,
///   settings and account alike;
/// - `save_complete`: no save was left unfinished;
/// - `settings_valid`/`settings_error`: the settings read and pass their
///   checks; `account_valid`/`account_error`: the server, port and extension
///   pass theirs (the credential aside, so that a profile nobody signed in
///   to can still be checked);
/// - `valid`/`error`: what a connect would decide, from the same check the
///   connect makes (connect_prerequisites);
/// - `stored`: every value as KSIP would write it back (name, registry type,
///   value).
///
/// For diagnosis, for importing into another machine's dialog, and for the
/// tests that hold the policy templates against what the app reads.
pub fn export_settings(store: &Store) -> serde_json::Value {
    use serde_json::json;
    let (settings, mut unreadable) = Settings::read_stored(|name| store.read_value(name));
    let settings_error = if unreadable.is_empty() {
        validate(&settings).err()
    } else {
        Some(message_with("SETTINGS_VALUE_INVALID", [unreadable.join(", ")]))
    };
    let ((server, port, extension), account_unreadable) = store.account_values();
    let account_error = if account_unreadable.is_empty() {
        Account { server: server.clone(), port, extension: extension.clone(), ..Account::default() }.validate_address().err()
    } else {
        Some(message_with("SETTINGS_VALUE_INVALID", [account_unreadable.join(", ")]))
    };
    unreadable.extend(account_unreadable);
    let credential = store.read_secret();
    let connect = connect_prerequisites(store).err();
    let stored: serde_json::Map<String, serde_json::Value> = settings
        .stored_values()
        .into_iter()
        .map(|(name, value)| {
            let value = match value {
                StoredValue::Text(text) => json!({"type": "REG_SZ", "value": text}),
                StoredValue::Number(number) => json!({"type": "REG_DWORD", "value": number}),
            };
            (name, value)
        })
        .collect();
    let account = json!({
        "server": server,
        "port": port,
        "extension": extension,
        "signed_in": matches!(credential, Ok(Some(_))),
        "credential_error": credential.err().unwrap_or_default(),
    });
    json!({
        "format": 1,
        "settings": settings,
        "account": account,
        "unreadable": unreadable,
        "save_complete": store.read_text(SAVE_MARK).is_empty(),
        "settings_valid": settings_error.is_none(),
        "settings_error": settings_error.unwrap_or_default(),
        "account_valid": account_error.is_none(),
        "account_error": account_error.unwrap_or_default(),
        "valid": connect.is_none(),
        "error": connect.unwrap_or_default(),
        "stored": stored,
    })
}
/// The settings a settings file (an export, or a part of one) brings to the
/// dialog, as the dialog takes them:
/// - `settings`: the settings fields the file names, each of the type the
///   field has; the custom buttons as `buttons`, a list of the buttons the
///   file names (`n`, 1 to 30, and the fields it gives);
/// - `account`: the server, port and extension the file gives;
/// - `unreadable`: the names the file names but the machine it came from
///   could not read (their values there are defaults, not the machine's);
/// - `invalid`: the names whose value is not of the field's type.
///
/// What a file leaves out is not there, so the dialog keeps what it has. The
/// microphone, the speaker and the network adapter belong to the machine and
/// are never taken. A file that is not a KSIP settings file (format 1) is refused.
pub fn import_settings(text: &str) -> Result<serde_json::Value, String> {
    use serde_json::{json, Map, Value};
    let refused = || message("SETTINGS_IMPORT_FORMAT");
    let document: Value = serde_json::from_str(text).map_err(|_| refused())?;
    if document.get("format").and_then(Value::as_u64) != Some(1) {
        return Err(refused());
    }
    let skip: Vec<String> = document["unreadable"]
        .as_array()
        .map(|names| names.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let defaults = serde_json::to_value(Settings::default()).map_err(|e| e.to_string())?;
    let same_type = |a: &Value, b: &Value| match (a, b) {
        (Value::Bool(_), Value::Bool(_)) | (Value::String(_), Value::String(_)) => true,
        (Value::Number(want), Value::Number(have)) => want.is_i64() == have.is_i64() || have.is_u64() || have.is_i64(),
        _ => false,
    };
    let mut settings = Map::new();
    let mut buttons = Vec::new();
    let mut unreadable = Vec::new();
    let mut invalid = Vec::new();
    for (name, value) in document["settings"].as_object().into_iter().flatten() {
        if matches!(name.as_str(), "microphone" | "speaker" | "network_adapter") {
            continue;
        }
        if name == "buttons" {
            for (index, button) in value.as_array().into_iter().flatten().enumerate().take(CustomButton::COUNT) {
                let mut fields = Map::new();
                fields.insert("n".into(), json!(index + 1));
                for (field, field_value) in button.as_object().into_iter().flatten() {
                    let full = format!("button_{}_{field}", index + 1);
                    if !Settings::BUTTON_FIELDS.contains(&field.as_str()) {
                        continue;
                    }
                    if skip.contains(&full) {
                        unreadable.push(full);
                    } else if field_value.is_string() {
                        fields.insert(field.clone(), field_value.clone());
                    } else {
                        invalid.push(full);
                    }
                }
                if fields.len() > 1 {
                    buttons.push(Value::Object(fields));
                }
            }
            continue;
        }
        let Some(default) = defaults.get(name) else {
            // A name this version does not know is passed over.
            continue;
        };
        if skip.contains(name) {
            unreadable.push(name.clone());
        } else if same_type(default, value) && (!value.is_number() || value.as_i64().is_some()) {
            settings.insert(name.clone(), value.clone());
        } else {
            invalid.push(name.clone());
        }
    }
    let mut account = Map::new();
    for (name, value) in document["account"].as_object().into_iter().flatten() {
        let fits = match name.as_str() {
            "server" | "extension" => value.is_string(),
            "port" => value.as_u64().is_some_and(|p| p <= u16::MAX as u64),
            _ => continue,
        };
        if skip.contains(name) {
            unreadable.push(name.clone());
        } else if fits {
            account.insert(name.clone(), value.clone());
        } else {
            invalid.push(name.clone());
        }
    }
    unreadable.sort();
    invalid.sort();
    Ok(json!({"settings": settings, "buttons": buttons, "account": account, "unreadable": unreadable, "invalid": invalid}))
}
/// `ksip.exe --export-settings <path>`: the export of the store this process
/// would use (KSIP_TEST_PROFILE chooses a test profile), written to `path` as
/// UTF-8. The exit code: 0 written, 1 the file could not be written, 2 no path.
pub fn export_command(path: Option<&str>) -> i32 {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return 2;
    };
    let document = export_settings(&Store::new());
    let text = serde_json::to_string_pretty(&document).unwrap_or_default();
    match std::fs::write(path, text) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
/// Reads the registry values into the settings, one by one, noting the ones
/// that cannot be read as what they are for.
struct StoredReader<F> {
    read: F,
    unreadable: Vec<String>,
}
impl<F: Fn(&str) -> Result<Option<StoredValue>, String>> StoredReader<F> {
    /// The value, or None when it is absent or empty text; one that cannot be
    /// read at all is noted.
    fn value(&mut self, name: &str) -> Option<StoredValue> {
        match (self.read)(name) {
            Ok(Some(StoredValue::Text(text))) if text.trim().is_empty() => None,
            Ok(Some(StoredValue::Text(text))) => Some(StoredValue::Text(text.trim().to_string())),
            Ok(value) => value,
            Err(_) => {
                self.unreadable.push(name.to_string());
                None
            }
        }
    }
    fn text(&mut self, name: &str, into: &mut String) {
        match self.value(name) {
            Some(StoredValue::Text(text)) => *into = text,
            Some(StoredValue::Number(number)) => *into = number.to_string(),
            None => {}
        }
    }
    fn number(&mut self, name: &str, into: &mut u16) {
        let read = match self.value(name) {
            Some(StoredValue::Number(number)) => u16::try_from(number).ok(),
            Some(StoredValue::Text(text)) => text.parse().ok(),
            None => return,
        };
        match read {
            Some(number) => *into = number,
            None => self.unreadable.push(name.to_string()),
        }
    }
    fn signed(&mut self, name: &str, into: &mut i32) {
        let read = match self.value(name) {
            Some(StoredValue::Number(number)) => Some(number as i32),
            Some(StoredValue::Text(text)) => text.parse().ok(),
            None => return,
        };
        match read {
            Some(number) => *into = number,
            None => self.unreadable.push(name.to_string()),
        }
    }
    fn flag(&mut self, name: &str, into: &mut bool) {
        let read = match self.value(name) {
            Some(StoredValue::Number(number)) => Some(number != 0),
            Some(StoredValue::Text(text)) => match text.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            },
            None => return,
        };
        match read {
            Some(on) => *into = on,
            None => self.unreadable.push(name.to_string()),
        }
    }
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
impl Services {
    /// The settings as stored; see stored_settings.
    pub fn settings(&self) -> Result<Settings, String> {
        stored_settings(&self.store)
    }
    /// Writes every setting to its own value. Each is written whatever
    /// became of the ones before it, so that one value that cannot be written
    /// does not keep the rest from landing; the first failure is the one
    /// reported.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let mut first = None;
        for (name, value) in settings.stored_values() {
            if let Err(e) = self.store.write_value(&name, &value) {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
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
    /// Both are many registry values and a credential written one by one,
    /// so a failure part way through would leave old and new mixed. Every
    /// value the save touches is backed up first exactly as the registry
    /// holds it, type and bytes (a value of a type the settings cannot read
    /// included, so that a save can replace it; a value that was absent
    /// stays absent), and so is the credential, on its own: an address a
    /// policy put there before anyone signed in is not the credential's to
    /// take along. After a failure every piece is put back on its own, so
    /// that one that cannot be put back does not keep the others from being;
    /// only when something could not be put back is the person told to save
    /// again.
    /// A mark (SAVE_MARK) stands in the store from before the first write
    /// until a save has gone through whole, so that a process that ends in
    /// the middle, or a rollback that failed, is seen at the next start
    /// (Services::open) and at every connect (prepare_restart), and the
    /// mixed values are not connected on. A rollback that succeeds takes the
    /// mark away only if this save put it there: values that were suspect
    /// before this save are no less suspect for having been put back.
    pub fn persist_configuration(&self, settings: &Settings, account: &Account) -> Result<(), String> {
        let marked_before = !self.store.read_text(SAVE_MARK).is_empty();
        let old_secret = self.store.read_secret()?;
        let names = settings.stored_values().into_iter().map(|(name, _)| name).chain(ACCOUNT_VALUES.iter().map(|name| name.to_string()));
        let mut old_values = Vec::new();
        for name in names {
            let value = self.store.read_raw(&name)?;
            old_values.push((name, value));
        }
        self.store.write_text(SAVE_MARK, "1")?;
        let written = self.store.write_account(account).and_then(|()| self.save_settings(settings));
        let Err(e) = written else {
            // Saved whole: the mark goes. If it cannot go, the next start
            // would refuse to connect on values that are in fact whole, so
            // the person is asked to save again, which clears it.
            return self.store.delete_value(SAVE_MARK).map_err(|e| message_with("ACCOUNT_ROLLBACK_FAILED", [e]));
        };
        let mut failures = Vec::new();
        failures.extend(
            match &old_secret {
                Some((user, password)) => self.store.write_secret(user, password),
                None => self.store.delete_account(),
            }
            .err(),
        );
        for (name, value) in &old_values {
            failures.extend(
                match value {
                    Some(value) => self.store.write_raw(name, value),
                    None => self.store.delete_value(name),
                }
                .err(),
            );
        }
        if failures.is_empty() && !marked_before {
            // Put back whole, to values that were whole: the mark goes, with
            // the same proviso as above.
            failures.extend(self.store.delete_value(SAVE_MARK).err());
        }
        Err(if failures.is_empty() { e } else { message_with("ACCOUNT_ROLLBACK_FAILED", [e]) })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone_state::{automatic_recording_target, CallInfo};
    use crate::storage::Store;
    use crate::storage::store_tests_one_at_a_time;
    /// A test's own store: named after the test, the process and a count, so
    /// that no two tests share one; emptied before the test, in case an
    /// earlier run left it behind, and after it, even when an assertion fails.
    struct StoreGuard(Store);
    impl StoreGuard {
        fn new(name: &str) -> Self {
            static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let suffix = format!("{name}-{}-{}", std::process::id(), COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            let store = Store {
                key: format!(r"Software\KashiharaCity\ksip\Test\{suffix}"),
                target: format!("KSIP/Test/{suffix}"),
            };
            store.cleanup_test();
            Self(store)
        }
        fn store(&self) -> Store {
            Store { key: self.0.key.clone(), target: self.0.target.clone() }
        }
    }
    impl Drop for StoreGuard {
        fn drop(&mut self) {
            self.0.cleanup_test();
        }
    }
    #[test]
    fn reject_config_injection_and_port_overlap() {
        let rejected = |s: Settings| validate(&s).is_err();
        assert!(rejected(Settings { microphone: "default\nmodule evil".into(), ..Settings::default() }));
        assert!(rejected(Settings { sip_port: 10000, rtp_port: 10000, ..Settings::default() }));
        assert!(rejected(Settings { aec_delay_ms: 501, ..Settings::default() }));
        assert!(rejected(Settings { noise_suppression: "loud".into(), ..Settings::default() }));
        assert!(rejected(Settings { ca_file: "C:\\ca.pem\nsip_verify_server no".into(), ..Settings::default() }));
        assert!(validate(&Settings::default()).is_ok());
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
            assert!(validate(&with(transport, media)).is_ok(), "{transport} {media}");
        }
        // SDES and OSRTP carry their keys in the signalling: TLS or nothing.
        for (transport, media) in [("", "sdes"), ("udp", "sdes"), ("tcp", "osrtp"), ("udp", "osrtp")] {
            assert_eq!(validate(&with(transport, media)), Err(message("SETTINGS_SDES_NEEDS_TLS")), "{transport} {media}");
        }
        // A value this version does not know is refused, not read as the weak default.
        assert_eq!(validate(&with("ssl", "")), Err(message("SETTINGS_TRANSPORT_INVALID")));
        assert_eq!(validate(&with("tls", "srtp")), Err(message("SETTINGS_MEDIA_ENCRYPTION_INVALID")));
        assert_eq!(validate(&with("tls", "srtp-mand")), Err(message("SETTINGS_MEDIA_ENCRYPTION_INVALID")));
        // Policy text is normalised on the way in, so "TLS" and " sdes " are the known values.
        let (policy, unreadable) = Settings::read_stored(|key| {
            Ok(match key {
                "transport" => Some(StoredValue::Text(" TLS ".into())),
                "media_encryption" => Some(StoredValue::Text("SDES".into())),
                _ => None,
            })
        });
        assert!(unreadable.is_empty());
        assert_eq!((policy.transport.as_str(), policy.media_encryption.as_str()), ("tls", "sdes"));
        assert!(validate(&policy).is_ok());
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
        assert!(validate(&with(vec![pickup])).is_ok());
        assert!(validate(&with(vec![CustomButton { pickup: "70 1".into(), ..button("dial", "701", "") }])).is_err());
        // A voicemail button names the number that plays the messages.
        assert!(validate(&with(vec![button("mwi", "*97", "")])).is_ok());
        assert!(validate(&with(vec![button("mwi", "", "")])).is_err());
        // A do-not-disturb switch names nothing.
        assert!(validate(&with(vec![button("dnd", "", "")])).is_ok());
        assert!(validate(&with(vec![button("dnd", "701", "")])).is_err());
        let ok = with(vec![button("park", "701", "*701"), button("dial", "1002", ""), button("transfer", "9001", "")]);
        assert!(validate(&ok).is_ok());
        assert_eq!(ok.watched_numbers(), vec!["701", "1002"]);
        assert_eq!(ok.buttons[0].transfer_target(), Some("*701"));
        assert_eq!(ok.buttons[1].transfer_target(), None);
        assert_eq!(ok.buttons[2].transfer_target(), Some("9001"));
        assert_eq!(button("park", "701", "").transfer_target(), Some("701"));
        // What is refused: a kind that is not one of the three, a title too
        // long, a number outside the SIP user alphabet, and two watchers of
        // one number. An unused button may leave its number empty.
        assert!(validate(&with(vec![button("hold", "701", "")])).is_err());
        assert!(validate(&with(vec![CustomButton { title: "x".repeat(41), ..button("dial", "1", "") }])).is_err());
        assert!(validate(&with(vec![button("dial", "70 1", "")])).is_err());
        assert!(validate(&with(vec![button("dial", "", "")])).is_err());
        assert!(validate(&with(vec![button("dial", "701", ""), button("park", "701", "")])).is_err());
        assert!(validate(&with(vec![button("", "", "")])).is_ok());
        // A full SIP URI is accepted for either address, with or without the
        // angle brackets a Refer-To header would carry. It is matched bare and
        // sent as it was written.
        let odd = with(vec![button("park", "61", "<sip:61@127.0.0.1>"), button("dial", " sip:sales@pbx.example ", "")]);
        assert!(validate(&odd).is_ok());
        assert_eq!(odd.buttons[0].transfer_target(), Some("sip:61@127.0.0.1"));
        assert_eq!(odd.buttons[0].transfer_text(), Some("<sip:61@127.0.0.1>"));
        assert_eq!(button("park", "701", "").transfer_text(), Some("701"));
        assert_eq!(odd.buttons[0].dial_target(), Some("61"));
        assert_eq!(odd.watched_numbers(), vec!["61", "sip:sales@pbx.example"]);
        assert!(validate(&with(vec![button("transfer", "sip:61@pbx with space", "")])).is_err());
        // A dial without BLF is not watched, so its number may be written as the
        // dial box takes it: RFC 3966 separators, a leading +, a tel: scheme.
        // Letters are still no number, and an empty one is refused like any other.
        let speed = with(vec![button("speed", "06-1234-5678", ""), button("dial", "1002", "")]);
        assert!(validate(&speed).is_ok());
        assert_eq!(speed.watched_numbers(), vec!["1002"]);
        assert_eq!(speed.buttons[0].transfer_target(), None);
        assert_eq!(speed.buttons[0].dial_target(), None);
        assert!(validate(&with(vec![button("speed", "tel:+81-6-1234-5678", "")])).is_ok());
        assert!(validate(&with(vec![button("speed", "<sip:sales@pbx.example>", "")])).is_ok());
        assert!(validate(&with(vec![button("speed", "sales", "")])).is_err());
        assert!(validate(&with(vec![button("speed", "", "")])).is_err());
        // A link button holds a web address and nothing the engine would dial.
        let page = with(vec![button("open", " https://pbx.example/extensions ", "")]);
        assert!(validate(&page).is_ok());
        assert_eq!(page.buttons[0].link_target(), Some("https://pbx.example/extensions"));
        assert_eq!(page.buttons[0].dial_target(), None);
        assert_eq!(page.buttons[0].transfer_target(), None);
        assert!(validate(&with(vec![button("open", "ftp://pbx.example/", "")])).is_err());
        assert!(validate(&with(vec![button("open", "https://pbx.example/a b", "")])).is_err());
        assert!(validate(&with(vec![button("dial", "https://pbx.example/", "")])).is_err());
        // The tray delay is -1 (never) or up to an hour.
        assert!(validate(&Settings { tray_after_call: 0, ..Settings::default() }).is_ok());
        assert!(validate(&Settings { tray_after_call: -2, ..Settings::default() }).is_err());
        assert!(validate(&Settings { tray_after_call: 3601, ..Settings::default() }).is_err());
        assert!(validate(&with(vec![button("transfer", &format!("sip:{}@pbx", "6".repeat(200)), "")])).is_err());
        // The buttons round-trip through their registry names.
        let stored: std::collections::HashMap<String, StoredValue> = ok.stored_values().into_iter().collect();
        let (read_back, _) = Settings::read_stored(|key| Ok(stored.get(key).cloned()));
        assert_eq!(read_back.buttons[..3], ok.buttons[..3]);
        assert!(read_back.buttons[3..].iter().all(|b| !b.configured()));
    }
    #[test]
    fn codecs_are_offered_in_the_chosen_order_and_all_by_default() {
        let mut s = Settings::default();
        assert_eq!(s.codec_list(), ["opus", "G722", "PCMU", "PCMA"]);
        s.codecs = "PCMU, opus".into();
        assert_eq!(s.codec_list(), ["PCMU", "opus"]);
        assert!(validate(&s).is_ok());
        s.codecs = "PCMU,PCMU".into();
        assert!(validate(&s).is_err(), "a codec named twice is refused");
        s.codecs = "G729".into();
        assert!(validate(&s).is_err(), "a codec the app does not have is refused");
    }
    #[test]
    fn settings_absent_from_the_registry_keep_their_defaults_and_auto_record_selects_active_line() {
        // A store that holds a few values: the rest are their defaults.
        let (settings, unreadable) = Settings::read_stored(|key| {
            Ok(match key {
                "sip_port" => Some(StoredValue::Number(5060)),
                "microphone" => Some(StoredValue::Text("default".into())),
                "aec" => Some(StoredValue::Number(1)),
                // Empty text is the same as no value.
                "noise_suppression" => Some(StoredValue::Text("  ".into())),
                _ => None,
            })
        });
        assert!(unreadable.is_empty());
        assert_eq!(settings.microphone_gain, 100);
        assert_eq!(settings.speaker_gain, 100);
        assert_eq!(settings.aec_delay_ms, 20);
        assert!(settings.high_pass);
        assert_eq!(settings.noise_suppression, "high");
        assert!(!settings.agc, "the automatic gain is off unless asked for");
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
    fn every_setting_round_trips_through_its_own_value() {
        let changed = Settings {
            network_adapter: "{00000000-0000-0000-0000-000000000001}".into(),
            sip_port: 5070,
            rtp_port: 12000,
            microphone: "{mic}".into(),
            speaker: "{spk}".into(),
            microphone_gain: 150,
            speaker_gain: 120,
            auto_record: true,
            buttons: {
                let mut buttons = CustomButton::empty_set();
                buttons[29] = CustomButton { title: "Last".into(), kind: "dial".into(), number: "701".into(), transfer: String::new(), pickup: "*8701".into() };
                buttons
            },
            transport: "tls".into(),
            ca_file: r"C:\ca.pem".into(),
            media_encryption: "sdes".into(),
            codecs: "PCMU,opus".into(),
            auto_answer: true,
            aec: false,
            aec_delay_ms: 40,
            high_pass: false,
            noise_suppression: "low".into(),
            agc: true,
            register_interval: 600,
            detail_log: true,
            browser_integration: true,
            browser_dial_confirm: false,
            shortcut_window: "SHIFT+F2".into(),
            shortcut_call: "SHIFT+F3".into(),
            incoming_action: "notify".into(),
            tray_after_call: -1,
            language: "en".into(),
            sound_ring: r"C:\ring.wav".into(),
            sound_ringback: r"C:\back.wav".into(),
            sound_busy: r"C:\busy.wav".into(),
            sound_notfound: r"C:\none.wav".into(),
            sound_error: r"C:\error.wav".into(),
        };
        let stored: std::collections::HashMap<String, StoredValue> = changed.stored_values().into_iter().collect();
        assert_eq!(stored.len(), changed.stored_values().len(), "no value name is used twice");
        let asked = std::cell::RefCell::new(std::collections::HashSet::new());
        let (read_back, unreadable) = Settings::read_stored(|key| {
            asked.borrow_mut().insert(key.to_string());
            Ok(stored.get(key).cloned())
        });
        assert!(unreadable.is_empty());
        assert_eq!(serde_json::to_value(&read_back).unwrap(), serde_json::to_value(&changed).unwrap(), "every setting comes back");
        let written: std::collections::HashSet<String> = stored.keys().cloned().collect();
        assert_eq!(*asked.borrow(), written, "what is written is exactly what is read");
        // The types: numbers and switches as numbers, -1 in two's complement.
        assert_eq!(stored["tray_after_call"], StoredValue::Number(u32::MAX));
        assert_eq!(stored["aec"], StoredValue::Number(0));
        assert_eq!(stored["sip_port"], StoredValue::Number(5070));
        assert_eq!(stored["language"], StoredValue::Text("en".into()));
    }
    #[test]
    fn a_value_in_the_other_type_is_read_and_one_that_means_nothing_is_named() {
        let (s, unreadable) = Settings::read_stored(|key| {
            Ok(match key {
                // Written by hand as text: still what it says.
                "sip_port" => Some(StoredValue::Text(" 5080 ".into())),
                "aec" => Some(StoredValue::Text("Off".into())),
                "browser_integration" => Some(StoredValue::Text("yes".into())),
                "tray_after_call" => Some(StoredValue::Text("-1".into())),
                "language" => Some(StoredValue::Number(7)),
                // Nothing a setting can be.
                "rtp_port" => Some(StoredValue::Number(70000)),
                "agc" => Some(StoredValue::Text("maybe".into())),
                "register_interval" => Some(StoredValue::Text("often".into())),
                "detail_log" => return Err(String::from("binary")),
                _ => None,
            })
        });
        assert_eq!((s.sip_port, s.aec, s.browser_integration, s.tray_after_call), (5080, false, true, -1));
        assert_eq!(s.language, "7");
        assert_eq!(unreadable, ["rtp_port", "agc", "register_interval", "detail_log"]);
        assert_eq!((s.rtp_port, s.agc, s.register_interval), (10000, false, 300), "what cannot be read keeps its default");
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
    fn a_save_that_fails_part_way_leaves_the_store_as_it_was() {
        let _one_at_a_time = store_tests_one_at_a_time();
        let guard = StoreGuard::new("test-rollback");
        let mut app = Services::open().0;
        app.store = guard.store();
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
        drop(guard);
        result.unwrap();
    }
    #[test]
    fn a_rollback_puts_back_every_value_it_can_and_absent_ones_as_absent() {
        let _one_at_a_time = store_tests_one_at_a_time();
        let guard = StoreGuard::new("test-rollback-each");
        let mut app = Services::open().0;
        app.store = guard.store();
        let account = |server: &str| Account {
            server: server.into(),
            port: 5060,
            extension: "1001".into(),
            auth_user: "1001".into(),
            password: "local-test-only".into(),
        };
        let result = (|| -> Result<(), String> {
            // Nothing stored yet: a first save that fails leaves nothing behind,
            // not even the values written before the failure.
            crate::storage::fail_next_write_of("codecs");
            let failed = app.persist_configuration(&Settings { transport: "tcp".into(), codecs: "PCMU".into(), ..Settings::default() }, &account("192.0.2.10"));
            crate::storage::fail_next_write_of("");
            assert!(failed.is_err(), "the injected failure must surface");
            assert!(app.store.read_account()?.is_none(), "no account is left behind");
            assert_eq!(app.store.read_value(SAVE_MARK)?, None, "put back whole, the save leaves no mark");
            assert_eq!(app.store.read_value("transport")?, None, "a value that was absent is absent again");
            assert_eq!(app.store.read_value("aec")?, None, "no setting is left behind");
            // With values stored, a failure that also keeps the rollback of
            // its own value from working still puts the other values back.
            app.persist_configuration(&Settings { transport: "udp".into(), codecs: "opus".into(), ..Settings::default() }, &account("192.0.2.10"))?;
            crate::storage::fail_next_write_of("codecs");
            let failed = app.persist_configuration(&Settings { transport: "tcp".into(), codecs: "PCMU".into(), ..Settings::default() }, &account("192.0.2.20"));
            crate::storage::fail_next_write_of("");
            let e = failed.expect_err("the injected failure must surface");
            assert!(e.contains("ACCOUNT_ROLLBACK_FAILED"), "the failed rollback of the codecs is reported: {e}");
            assert_eq!(app.store.read_account()?.expect("account").server, "192.0.2.10", "the account is back");
            assert_eq!(app.settings()?.transport, "udp", "the transport, written before the failure, is back");
            assert_eq!(app.settings()?.codecs, "opus", "the codecs never changed");
            assert_eq!(app.store.read_value(SAVE_MARK)?, Some(StoredValue::Text("1".into())), "a rollback that failed leaves the mark for the next start");
            // With the mark standing, a save that fails before it writes
            // anything (a password too long for the vault) is put back whole,
            // and the mark stays: the values it went back to were suspect already.
            let too_long = Account { password: "x".repeat(1025), ..account("192.0.2.10") };
            assert!(app.persist_configuration(&Settings { transport: "udp".into(), codecs: "opus".into(), ..Settings::default() }, &too_long).is_err());
            assert_eq!(app.store.read_value(SAVE_MARK)?, Some(StoredValue::Text("1".into())), "a rollback does not clear a mark it did not set");
            // A save that goes through whole takes the mark away.
            app.persist_configuration(&Settings { transport: "udp".into(), codecs: "opus".into(), ..Settings::default() }, &account("192.0.2.10"))?;
            assert_eq!(app.store.read_value(SAVE_MARK)?, None, "a whole save clears the mark");
            Ok(())
        })();
        drop(guard);
        result.unwrap();
    }
    #[test]
    fn a_value_of_a_type_the_settings_cannot_read_is_replaced_by_a_save_and_put_back_by_a_rollback() {
        use winreg::{enums::*, RegKey, RegValue};
        let _one_at_a_time = store_tests_one_at_a_time();
        let guard = StoreGuard::new("test-raw-type");
        let mut app = Services::open().0;
        app.store = guard.store();
        let account = Account { server: "192.0.2.10".into(), port: 5060, extension: "1001".into(), auth_user: "1001".into(), password: "local-test-only".into() };
        let binary = || RegValue { bytes: vec![1, 2, 3], vtype: REG_BINARY };
        let put_binary = |store: &Store| {
            let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(&store.key).unwrap();
            key.set_raw_value("aec", &binary()).unwrap();
        };
        let read_raw = |store: &Store| RegKey::predef(HKEY_CURRENT_USER).open_subkey(&store.key).unwrap().get_raw_value("aec").unwrap();
        let result = (|| -> Result<(), String> {
            put_binary(&app.store);
            assert!(app.settings().is_err(), "a value the settings cannot read is refused");
            // A save that fails part way puts the value back as it was, type and all.
            crate::storage::fail_next_write_of("codecs");
            let failed = app.persist_configuration(&Settings::default(), &account);
            crate::storage::fail_next_write_of("");
            assert!(failed.is_err());
            assert!(read_raw(&app.store) == binary(), "the rollback puts back the binary value byte for byte");
            // A save that goes through replaces it with what the setting is.
            app.persist_configuration(&Settings::default(), &account)?;
            assert_eq!(app.store.read_value("aec")?, Some(StoredValue::Number(1)));
            assert!(app.settings().is_ok(), "the settings read again");
            Ok(())
        })();
        drop(guard);
        result.unwrap();
    }
    #[test]
    fn a_first_sign_in_that_fails_puts_back_the_address_a_policy_had_put_there() {
        let _one_at_a_time = store_tests_one_at_a_time();
        let guard = StoreGuard::new("test-first-account");
        let mut app = Services::open().0;
        app.store = guard.store();
        let account = Account { server: "192.0.2.10".into(), port: 5060, extension: "1001".into(), auth_user: "1001".into(), password: "local-test-only".into() };
        let result = (|| -> Result<(), String> {
            // A policy put the address there; nobody has signed in yet.
            app.store.write_text("server", "pbx.example")?;
            app.store.write_text("port", "5061")?;
            assert!(app.store.read_secret()?.is_none());
            crate::storage::fail_next_write_of("codecs");
            let failed = app.persist_configuration(&Settings::default(), &account);
            crate::storage::fail_next_write_of("");
            assert!(failed.is_err());
            assert!(app.store.read_secret()?.is_none(), "no credential is left behind");
            assert_eq!(app.store.read_value("server")?, Some(StoredValue::Text("pbx.example".into())), "the policy's server is back");
            assert_eq!(app.store.read_value("port")?, Some(StoredValue::Text("5061".into())), "the policy's port is back, in its type");
            assert_eq!(app.store.read_value("extension")?, None, "a value that was absent is absent again");
            // With a credential already there, a failed save puts it back too.
            app.persist_configuration(&Settings::default(), &account)?;
            crate::storage::fail_next_write_of("codecs");
            let failed = app.persist_configuration(&Settings::default(), &Account { auth_user: "1002".into(), password: "other-test-only".into(), server: "192.0.2.20".into(), ..account.clone() });
            crate::storage::fail_next_write_of("");
            assert!(failed.is_err());
            let (user, password) = app.store.read_secret()?.expect("the credential is back");
            assert_eq!((user.as_str(), password.as_str()), ("1001", "local-test-only"));
            assert_eq!(app.store.read_text("server"), "192.0.2.10");
            Ok(())
        })();
        drop(guard);
        result.unwrap();
    }
    #[test]
    fn the_export_says_what_ksip_reads_and_never_the_password() {
        let _one_at_a_time = store_tests_one_at_a_time();
        let guard = StoreGuard::new("test-export");
        let store = guard.store();
        let result = (|| -> Result<(), String> {
            let empty = export_settings(&store);
            assert_eq!(empty["settings_valid"], true, "nothing stored is the defaults, which pass");
            assert_eq!(empty["account_valid"], true);
            assert_eq!(empty["valid"], false, "but nobody signed in: a connect would not go ahead");
            assert_eq!(empty["error"], message("SIP_ACCOUNT_REQUIRED"));
            assert_eq!(empty["settings"]["sip_port"], 5060);
            assert_eq!(empty["account"]["server"], crate::storage::DEFAULT_SERVER);
            assert_eq!(empty["account"]["signed_in"], false);
            assert_eq!(empty["stored"]["aec"], serde_json::json!({"type": "REG_DWORD", "value": 1}));
            assert_eq!(empty["stored"]["language"], serde_json::json!({"type": "REG_SZ", "value": ""}));
            store.write_account(&Account { server: "pbx.example".into(), port: 5061, extension: "1001".into(), auth_user: "1001".into(), password: "local-test-only".into() })?;
            store.write_text("transport", " TLS ")?;
            store.write_text("agc", "maybe")?;
            let read = export_settings(&store);
            assert_eq!(read["settings"]["transport"], "tls", "normalised as the app reads it");
            assert_eq!(read["account"]["port"], 5061);
            assert_eq!(read["account"]["signed_in"], true);
            assert_eq!(read["unreadable"], serde_json::json!(["agc"]));
            assert_eq!(read["valid"], false);
            assert_eq!(read["settings_valid"], false);
            assert!(!read.to_string().contains("local-test-only"), "the password never goes into the export");
            // Signed in, but an account a connect refuses: valid says so.
            store.write_text("agc", "0")?;
            store.write_text("server", "invalid/server")?;
            store.write_text("port", "0")?;
            let refused = export_settings(&store);
            assert_eq!((refused["settings_valid"].clone(), refused["account_valid"].clone(), refused["valid"].clone()), (serde_json::json!(true), serde_json::json!(false), serde_json::json!(false)));
            assert_eq!(refused["error"], message("ACCOUNT_SERVER_INVALID"));
            assert!(connect_prerequisites(&store).is_err(), "the connect agrees");
            // Account values of a type nobody writes are named, for the export and for a connect.
            {
                use winreg::{enums::*, RegKey, RegValue};
                let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(&store.key).unwrap();
                key.set_raw_value("server", &RegValue { bytes: vec![1, 2], vtype: REG_BINARY }).unwrap();
            }
            store.write_value("port", &StoredValue::Number(5060))?;
            let binary = export_settings(&store);
            assert_eq!(binary["unreadable"], serde_json::json!(["server"]));
            assert_eq!(binary["valid"], false);
            assert!(store.read_account().is_err(), "a connect does not fall back to the default server");
            // A save left unfinished: a connect refuses, and the export says why.
            store.write_text("server", "pbx.example")?;
            store.write_text(SAVE_MARK, "1")?;
            let unfinished = export_settings(&store);
            assert_eq!((unfinished["save_complete"].clone(), unfinished["valid"].clone()), (serde_json::json!(false), serde_json::json!(false)));
            assert_eq!(unfinished["error"], message("SETTINGS_SAVE_INTERRUPTED"));
            store.delete_value(SAVE_MARK)?;
            assert_eq!(export_settings(&store)["valid"], true, "with all of it in order, a connect would go ahead");
            Ok(())
        })();
        drop(guard);
        result.unwrap();
    }
    #[test]
    fn a_settings_file_brings_what_it_names_and_nothing_of_the_machine() {
        let file = serde_json::json!({
            "format": 1,
            "settings": {
                "sip_port": 5070, "aec": false, "language": "en", "agc": true,
                "microphone": "{mic}", "speaker": "{spk}", "network_adapter": "{nic}",
                "tray_after_call": -1, "register_interval": "often", "unknown_to_this_version": 1,
                "buttons": [{"title": "Park", "kind": "park", "number": "701"}, {}, {"title": 3}]
            },
            "account": {"server": "pbx.example", "port": 5061, "extension": "1001", "signed_in": true},
            "unreadable": ["agc", "extension"],
            "valid": false, "stored": {}
        });
        let read = import_settings(&file.to_string()).unwrap();
        assert_eq!(read["settings"], serde_json::json!({"sip_port": 5070, "aec": false, "language": "en", "tray_after_call": -1}));
        assert_eq!(read["buttons"], serde_json::json!([{"n": 1, "title": "Park", "kind": "park", "number": "701"}]));
        assert_eq!(read["account"], serde_json::json!({"server": "pbx.example", "port": 5061}));
        assert_eq!(read["unreadable"], serde_json::json!(["agc", "extension"]));
        assert_eq!(read["invalid"], serde_json::json!(["button_3_title", "register_interval"]));
        // What is not a KSIP settings file is refused.
        for bad in ["", "[]", "{}", r#"{"format":2,"settings":{}}"#, "not json"] {
            assert_eq!(import_settings(bad).err(), Some(message("SETTINGS_IMPORT_FORMAT")), "{bad}");
        }
        // An export of one machine is a file another can read.
        let _one_at_a_time = store_tests_one_at_a_time();
        let guard = StoreGuard::new("test-import");
        let exported = export_settings(&guard.store());
        let back = import_settings(&exported.to_string()).unwrap();
        assert!(back["invalid"].as_array().unwrap().is_empty() && back["unreadable"].as_array().unwrap().is_empty());
        assert_eq!(back["settings"]["sip_port"], 5060);
        assert_eq!(back["buttons"].as_array().unwrap().len(), CustomButton::COUNT);
    }
    #[test]
    fn only_a_new_authentication_user_asks_for_the_password_again() {
        let _one_at_a_time = store_tests_one_at_a_time();
        let guard = StoreGuard::new("test-password");
        let mut app = Services::open().0;
        app.store = guard.store();
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
        drop(guard);
        result.unwrap();
    }
}
