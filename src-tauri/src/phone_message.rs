//! The messages the phone actor takes: commands from the window, the tray
//! and the links; what the engine link reports; and what work that ran
//! outside the actor brings back. Every change to the phone's state starts
//! as one of these, and they are handled one at a time, in order.
use crate::audio::{Calibration, Device, Target, Volume};
use crate::engine_link::{EngineLink, StopReport};
use crate::settings::{CustomButton, Settings};
use crate::storage::Account;
use serde_json::Value;
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
    /// The settings dialog was saved. The answer is a notice for the window,
    /// empty when there is nothing to say.
    SaveConfiguration {
        /// Boxed: the settings are the largest thing a command carries.
        settings: Box<Settings>,
        account: Account,
        reply: Reply<String>,
    },
    /// The custom buttons alone, from the window's button editing.
    SaveButtons {
        buttons: Vec<CustomButton>,
        reply: Reply<()>,
    },
    SelectAudioDevice {
        kind: String,
        device: String,
        reply: Reply<()>,
    },
    RefreshDevices(Reply<()>),
    /// Windows said its audio devices changed (audio.rs, watch_devices):
    /// the list is read again on a worker, and what differs follows.
    DevicesChanged,
    /// Windows said an IPv4 address came or went (native.rs,
    /// watch_addresses): the machine's addresses are read again on a
    /// worker, and what differs is acted on (phone_actor.rs, network_seen).
    NetworkChanged,
    /// The window's look at the microphone's volume found Windows' mute of
    /// its endpoint so: the engine is told (ksip_audio_mute), so that the
    /// calls send silence while it is muted.
    MicrophoneMuted(bool),
    CalibrateAec {
        microphone: String,
        speaker: String,
        careful: bool,
        reply: Reply<Calibration>,
    },
    /// A volume or mute change; reading the volume does not go through here.
    /// The endpoint in use is looked up when the change is made (audio.rs,
    /// target), and the change is made only while that is `expected`, the
    /// one the window was shown; the answer says which it was.
    SetVolume {
        kind: String,
        level: Option<u16>,
        mute: Option<bool>,
        expected: Option<String>,
        reply: Reply<(Volume, Target)>,
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
    /// The control connection ended. The process may still run: the actor
    /// ends it before it treats the engine as gone.
    Lost,
}

/// A started engine, with what its start decided.
pub struct Ready {
    pub link: EngineLink,
    pub address: String,
    /// The machine's IPv4 addresses at the start, the engine's SIP
    /// transports being bound on all of them when no adapter is chosen.
    pub addresses: Vec<String>,
    pub notes: Vec<String>,
}

/// What work that ran outside the actor brings back. The flow that asked
/// for it is waiting for exactly this; the actor decides, the work only
/// reports.
/// How an attempt to stop an engine went: Ok, the process has ended; Err,
/// that could not be confirmed, and the link comes back to be tried again.
pub type StopOutcome = Result<StopReport, Box<(EngineLink, String)>>;
pub enum Work {
    /// The previous engine, if any, was stopped and a new one was started,
    /// or the start failed. `stopped` is how the previous one went; when it
    /// could not be confirmed ended, no new one was started over its ports.
    Started {
        stopped: Option<StopOutcome>,
        /// Boxed: the started engine is the largest thing a message carries.
        result: Result<Box<Ready>, String>,
    },
    /// An engine was stopped, or could not be confirmed stopped.
    Stopped(StopOutcome),
    /// The audio devices were read again.
    Devices(Result<Vec<Device>, String>),
    /// The machine's IPv4 addresses as they are now: the chosen adapter's
    /// (named as chosen, empty when none is) and every adapter's, sorted.
    /// Nobody waits for this one: the actor looks at it as it comes.
    Address {
        adapter: String,
        adapter_addresses: Vec<String>,
        all: Vec<String>,
    },
    /// The echo calibration has finished.
    Calibrated(Result<Calibration, String>),
}

/// Everything that reaches the actor's queue.
pub enum Message {
    Command(Command),
    Link(LinkMessage),
    /// A worker's report, numbered by the order the work was started in.
    Work(u64, Work),
}
