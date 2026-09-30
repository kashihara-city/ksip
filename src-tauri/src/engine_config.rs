//! What the engine is started with: the audio endpoints for the saved
//! devices, the config written from the settings, the sounds and the trust
//! list. The process itself is the link's to start.
use crate::app::Services;
use crate::audio::Device;
use crate::engine_link::StartPlan;
use crate::message::message;
use crate::settings::Settings;
use crate::storage::Account;
use std::{net::TcpListener, path::PathBuf};

/// A secret for one engine start, from the system's random source: 256 bits
/// as hex. The engine takes commands only on the connection that says it.
fn control_secret() -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let mut bytes = [0u8; 32];
    // SAFETY: a null algorithm handle with BCRYPT_USE_SYSTEM_PREFERRED_RNG asks
    // the system's generator to fill the buffer, whose length is given.
    let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status != 0 {
        return Err(crate::message::message_with("ENGINE_START_FAILED", ["no random source for the control secret"]));
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
/// The endpoints the engine is given for a saved choice, see
/// `resolve_audio_endpoints`.
pub struct AudioEndpoints {
    pub microphone: String,
    pub speaker: String,
    pub microphone_missing: bool,
    pub speaker_missing: bool,
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
    /// The sounds folder as the settings want it: the built-in sounds written
    /// out, and each chosen one over its built-in namesake. baresip only treats
    /// a path starting with "/" as absolute, so a chosen file cannot be named
    /// directly on Windows; the sounds are replaced in place instead. The
    /// engine reads a sound when it plays it, so this also changes the sounds
    /// of a running engine. With the folder, what could not be replaced.
    pub fn prepare_sounds(&self, s: &Settings) -> (Option<PathBuf>, Vec<String>) {
        let mut notes = Vec::new();
        let dir = crate::native::sounds();
        if let Some(dir) = &dir {
            for (key, chosen) in Settings::SOUND_KEYS.iter().zip(s.sounds()) {
                if let Err(e) = self.replace_sound(dir, key, chosen) {
                    notes.push(format!("ksip: {key} sound not replaced, {e}"));
                }
            }
        }
        (dir, notes)
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
        // The app's listener for the engine's control connection: held from
        // here until the engine has connected, so the port is never free.
        let listener = TcpListener::bind("127.0.0.1:0").map_err(err)?;
        let ctrl = listener.local_addr().map_err(err)?.port();
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
        // Requests from anywhere but the registrar are refused (403), and none
        // are taken before registering; over TCP and TLS nothing is listened
        // for, the registration's own connection carrying the calls.
        if s.pbx_only {
            put("filter_registrar UDP,TCP,TLS".into());
        }
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
        put(format!("ksip_keepalive {}", s.keepalive));
        put(format!("ksip_keepalive_interval {}", s.keepalive_interval));
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
        put(format!("ksip_ctrl_connect 127.0.0.1:{ctrl}"));
        for module in [
            "g711", "libg722", "opus", "wasapi", "ksip_audio", "ksip_audio_filter", "auconv", "auresamp",
            "ksip_ctrl", "menu", "srtp", "dtls_srtp", "ksip",
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
                trust_note = Some((store.included, store.left_out, store.distrusted));
                let path = profile.join("windows-trust.pem");
                std::fs::write(&path, store.pem).map_err(err)?;
                path.to_string_lossy().replace('\\', "/")
            } else {
                chosen.replace('\\', "/")
            };
            put(format!("sip_cafile {trust}"));
            put("sip_verify_server yes".into());
        }
        let (sounds, sound_notes) = self.prepare_sounds(s);
        notes.extend(sound_notes);
        if let Some(dir) = sounds {
            put(format!("audio_path {}", dir.to_string_lossy().replace('\\', "/")));
        }
        std::fs::write(profile.join("config"), config).map_err(err)?;
        notes.extend(endpoint_notes(s, &endpoints, devices));
        if let Some((included, left_out, distrusted)) = trust_note {
            notes.push(format!(
                "ksip: windows trust store, {included} certificates, {left_out} left out as unusable, {distrusted} left out as distrusted by Windows"
            ));
        }
        Ok(Prepared {
            plan: StartPlan {
                exe,
                profile,
                control: listener,
                credential_target: self.store.target.clone(),
                control_secret: control_secret()?,
            },
            endpoints,
            address,
            notes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;
    use std::time::{Duration, Instant};
    #[test]
    #[ignore = "requires scripts/build/native.ps1; uses isolated temp/build/rust-engine-test"]
    fn real_engine_starts_stops_and_restarts() {
        use crate::engine_link::{AudioState, EngineLink, EngineReport, MicrophoneState, SpeakerState};
        use crate::phone_message::{LinkBody, Message};
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let root = project.join("temp/build/rust-engine-test");
        let mut services = Services::open().0;
        services.data = root.join("data");
        services.store = Store::at(r"Software\KashiharaCity\ksip\Test\test-engine".into(), "KSIP/Test/test-engine".into());
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
            assert!(config.contains("filter_registrar UDP,TCP,TLS"), "requests only from the registrar, by default");
            assert!(config.contains("ksip_high_pass yes"));
            assert!(config.contains("ksip_noise_suppression high"));
            assert!(config.contains("ksip_agc no"));
            assert!(config.contains("ksip_keepalive crlf") && config.contains("ksip_keepalive_interval 60"), "a blank line every minute, by default");
            assert!(config.contains("ksip_microphone_gain 100"));
            assert!(config.contains("ksip_speaker_gain 100"));
            let mut link = EngineLink::start(prepared.plan, generation, tx.clone()).unwrap();
            assert_eq!(link.generation(), generation);
            // A request is answered on the queue, under this engine's generation
            // and the request's token, in receive order.
            let token = link.send("ksip_record_stop", "").unwrap();
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
            // The state report carries the audio module's state from the
            // start, before any call: up, with the processing on (the
            // default settings), no input yet, nothing failed.
            let token = link.send("ksip_state", "").unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            let state: EngineReport = loop {
                let message = rx.recv_timeout(deadline - Instant::now().min(deadline)).expect("the state in time");
                if let Message::Link(delivered) = message {
                    if let LinkBody::Response { token: t, value } = delivered.body {
                        if t == token {
                            break serde_json::from_str(value["data"].as_str().unwrap_or_default()).expect("a state report");
                        }
                    }
                }
            };
            // The detail log is switched while the engine runs, on and off.
            for value in ["on", "off"] {
                let token = link.send("ksip_detail_log", value).unwrap();
                let deadline = Instant::now() + Duration::from_secs(8);
                loop {
                    let message = rx.recv_timeout(deadline - Instant::now().min(deadline)).expect("an answer to the detail log");
                    if let Message::Link(delivered) = message {
                        if let LinkBody::Response { token: t, value: answer } = delivered.body {
                            if t == token {
                                assert_eq!(answer["ok"], true, "ksip_detail_log {value}: {answer}");
                                break;
                            }
                        }
                    }
                }
            }
            let audio = state.audio.expect("the audio module's state");
            assert_eq!(
                audio,
                AudioState {
                    ready: true,
                    processing: true,
                    microphone: MicrophoneState { input: "none".into(), failures: 0, last_result: None },
                    speaker: SpeakerState { playing: false, failures: 0, last_result: None },
                    // No call has opened the microphone yet.
                    capture_raw: None,
                }
            );
            let report = link.stop().map_err(|back| back.1).unwrap();
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
}
