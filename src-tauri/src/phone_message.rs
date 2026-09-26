//! The messages the phone actor takes: commands from the window, the tray
//! and the links; what the engine link reports; and what work that ran
//! outside the actor brings back. Every change to the phone's state starts
//! as one of these, and they are handled one at a time, in order.
use crate::audio::{Calibration, Volume};
use crate::settings::Settings;
use crate::storage::Account;
use serde_json::Value;
use std::process::ExitStatus;
use std::sync::mpsc;

/// Where a command's answer goes. A command that carries one is answered
/// exactly once: with its result, or with nothing when the actor is gone
/// before it got to it, which the waiting side reads as APP_CLOSING.
pub struct Reply<T>(mpsc::Sender<Result<T, String>>);
impl<T> Reply<T> {
    pub fn channel() -> (Self, mpsc::Receiver<Result<T, String>>) {
        let (tx, rx) = mpsc::channel();
        (Self(tx), rx)
    }
    pub fn send(self, result: Result<T, String>) {
        let _ = self.0.send(result);
    }
}

/// What the window, the tray, the links and the desktop loop ask of the phone.
pub enum Command {
    /// The first thing after the window is up: devices, then the saved account.
    Initialize,
    /// Start (or restart) the engine on the saved settings and register.
    Connect(Reply<()>),
    /// One operation on a call or on the account, as the window names them.
    Action {
        name: String,
        id: String,
        value: String,
        line: u8,
        reply: Reply<String>,
    },
    /// The settings dialog was saved.
    SaveConfiguration {
        /// Boxed: the settings are the largest thing a command carries.
        settings: Box<Settings>,
        account: Account,
        reply: Reply<()>,
    },
    SelectAudioDevice {
        kind: String,
        device: String,
        reply: Reply<()>,
    },
    RefreshDevices(Reply<()>),
    CalibrateAec {
        microphone: String,
        speaker: String,
        careful: bool,
        reply: Reply<Calibration>,
    },
    /// A volume or mute change; reading the volume does not go through here.
    SetVolume {
        kind: String,
        device: String,
        level: Option<u16>,
        mute: Option<bool>,
        reply: Reply<Volume>,
    },
    /// Told by the desktop loop, which can see the window; the phone cannot.
    WindowVisible(bool),
    /// Something to show in the banner and write in the log.
    ShowError(String),
    /// A banner from an automatic path, said once in the log while it lasts.
    ReportError(String),
    /// The app is leaving: stop the engine and answer nothing else.
    Shutdown(Reply<()>),
}

/// One thing the engine link received or noticed, tagged with the
/// generation of the engine it came from and its place in that link's
/// receive order. The actor drops what belongs to an engine that is no
/// longer the one it holds, and applies state reports only in order.
pub struct LinkMessage {
    pub generation: u64,
    pub seq: u64,
    pub body: LinkBody,
}
pub enum LinkBody {
    /// The engine's answer to a request, by the token the request carried.
    Response { token: String, value: Value },
    /// An event the engine raised of its own accord.
    Event(Value),
    /// A line the engine wrote to its output.
    Log(String),
    /// The control connection ended: the engine is gone or going.
    Lost,
    /// The engine process has ended, some time after `Lost`.
    Exited(ExitStatus),
}

/// Everything that reaches the actor's queue.
pub enum Message {
    Command(Command),
    Link(LinkMessage),
    /// The echo calibration that ran outside the actor has finished.
    Calibrated {
        result: Result<Calibration, String>,
        reply: Reply<Calibration>,
    },
}
