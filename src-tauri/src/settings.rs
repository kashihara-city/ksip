//! The settings: the document and the values a policy sets, the custom
//! buttons, the transport and the media encryption, what is valid, and how
//! the settings and the account are stored.
use crate::app::Services;
use crate::logs::LOG_APP;
use crate::message::{message, message_with};
use crate::storage::Account;
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
    /// Settings come from the JSON document plus the values a policy can set.
    pub fn settings(&self) -> Result<Settings, String> {
        let mut settings = self.store.read_settings::<Settings>()?;
        settings.read_policy(|key| self.store.read_text(key));
        Ok(settings)
    }
    /// The policy values own a registry value each, so they are removed from
    /// the settings document instead of being stored twice. Every value is
    /// written whatever became of the ones before it, so that one value that
    /// cannot be written does not keep the rest from landing; the first
    /// failure is the one reported.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let mut document = serde_json::to_value(settings).map_err(|e| e.to_string())?;
        if let Some(fields) = document.as_object_mut() {
            for key in Settings::POLICY_DOCUMENT_KEYS {
                fields.remove(key);
            }
        }
        let mut first = self.store.write_settings(&document).err();
        for (key, value) in settings.policy_values() {
            if let Err(e) = self.store.write_text(&key, &value) {
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
    /// Both are several registry values and a credential written one by one,
    /// so a failure part way through would leave old and new mixed. What is
    /// there is read first as stored (a value that was absent stays absent),
    /// and after a failure every piece is put back on its own, so that one
    /// that cannot be put back does not keep the others from being; only
    /// when something could not be put back is the person told to save again.
    /// A mark (SAVE_MARK) stands in the store from before the first write
    /// until the save has gone through whole or been put back whole, so that
    /// a process that ends in the middle, or a rollback that failed, is seen
    /// at the next start (Services::open) and the mixed values are not
    /// connected on.
    pub fn persist_configuration(&self, settings: &Settings, account: &Account) -> Result<(), String> {
        let old_account = self.store.read_account()?;
        let old_document = self.store.read_settings::<serde_json::Value>()?;
        let mut old_policy = Vec::new();
        for (key, _) in settings.policy_values() {
            let value = self.store.read_text_raw(&key)?;
            old_policy.push((key, value));
        }
        self.store.write_text(SAVE_MARK, "1")?;
        let written = self.store.write_account(account).and_then(|()| self.save_settings(settings));
        let Err(e) = written else {
            // Saved whole: the mark goes. If it cannot go, the next start
            // would refuse to connect on values that are in fact whole, so
            // the person is asked to save again, which clears it.
            return self.store.delete_text(SAVE_MARK).map_err(|e| message_with("ACCOUNT_ROLLBACK_FAILED", [e]));
        };
        let mut failures = Vec::new();
        failures.extend(
            match &old_account {
                Some(previous) => self.store.write_account(previous),
                None => self.store.delete_account(),
            }
            .err(),
        );
        failures.extend(
            match &old_document {
                serde_json::Value::Null => self.store.delete_text("Settings"),
                document => self.store.write_settings(document),
            }
            .err(),
        );
        for (key, value) in &old_policy {
            failures.extend(
                match value {
                    Some(value) => self.store.write_text(key, value),
                    None => self.store.delete_text(key),
                }
                .err(),
            );
        }
        if failures.is_empty() {
            // Put back whole: the mark goes, with the same proviso as above.
            failures.extend(self.store.delete_text(SAVE_MARK).err());
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
    /// The tests that write the store run one at a time: the credential
    /// vault has been seen to answer a read of one target with "not found"
    /// while another target was being deleted from a second thread.
    fn store_tests_one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let mut policy = Settings::default();
        policy.read_policy(|key| match key { "transport" => " TLS ".into(), "media_encryption" => "SDES".into(), _ => String::new() });
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
        // The policy values round-trip through their registry names.
        let mut read_back = Settings::default();
        let stored: std::collections::HashMap<String, String> = ok.policy_values().into_iter().collect();
        read_back.read_policy(|key| stored.get(key).cloned().unwrap_or_default());
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
    fn a_rollback_puts_back_every_value_it_can_and_absent_ones_as_absent() {
        let _one_at_a_time = store_tests_one_at_a_time();
        let suffix = format!("test-rollback-each-{}", std::process::id());
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
        let result = (|| -> Result<(), String> {
            // Nothing stored yet: a first save that fails leaves nothing behind,
            // not even the values written before the failure.
            crate::storage::fail_next_write_of("codecs");
            let failed = app.persist_configuration(&Settings { transport: "tcp".into(), codecs: "PCMU".into(), ..Settings::default() }, &account("192.0.2.10"));
            crate::storage::fail_next_write_of("");
            assert!(failed.is_err(), "the injected failure must surface");
            assert!(app.store.read_account()?.is_none(), "no account is left behind");
            assert_eq!(app.store.read_text_raw(SAVE_MARK)?, None, "put back whole, the save leaves no mark");
            assert_eq!(app.store.read_text_raw("transport")?, None, "a value that was absent is absent again");
            assert_eq!(app.store.read_settings::<serde_json::Value>()?, serde_json::Value::Null, "no settings document is left behind");
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
            assert_eq!(app.store.read_text_raw(SAVE_MARK)?, Some("1".into()), "a rollback that failed leaves the mark for the next start");
            // A save that goes through whole takes the mark away.
            app.persist_configuration(&Settings { transport: "udp".into(), codecs: "opus".into(), ..Settings::default() }, &account("192.0.2.10"))?;
            assert_eq!(app.store.read_text_raw(SAVE_MARK)?, None, "a whole save clears the mark");
            Ok(())
        })();
        app.store.cleanup_test();
        result.unwrap();
    }
    #[test]
    fn only_a_new_authentication_user_asks_for_the_password_again() {
        let _one_at_a_time = store_tests_one_at_a_time();
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
}
