//! The Tauri commands: what the page asks for. Each one sends a command to
//! the phone actor or asks a service; none of them holds or changes any
//! phone state of its own.
use crate::app::AppState;
use crate::audio::{Calibration, Peak, Target};
use crate::history::CallHistory;
use crate::logs::{LogPage, LOG_APP};
use crate::message::message;
use crate::phone_message::Command;
use crate::phone_state::Snapshot;
use crate::settings::Settings;
use crate::storage::Account;
use crate::{desktop, native, shortcuts};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

#[tauri::command]
pub fn snapshot(state: State<AppState>) -> Snapshot {
    state.snapshot()
}
/// Link events for the page, held until it says it listens. Windows starts
/// the app for a link before the page has loaded, and an event emitted into
/// a page without listeners reaches nobody; `ui_ready` lets them go, in the
/// order they came. An event that arrives while the held ones are being let
/// go joins the end of the line rather than overtaking them.
pub struct LinkQueue(std::sync::Mutex<LinkGate>);
struct LinkGate {
    /// The page listens; events go straight through.
    ready: bool,
    pending: Vec<(&'static str, serde_json::Value)>,
}
impl LinkQueue {
    pub fn new() -> Self {
        Self(std::sync::Mutex::new(LinkGate { ready: false, pending: Vec::new() }))
    }
}
impl Default for LinkQueue {
    fn default() -> Self {
        Self::new()
    }
}
pub fn emit_link(app: &tauri::AppHandle, event: &'static str, payload: serde_json::Value) {
    if let Some(queue) = app.try_state::<LinkQueue>() {
        let mut gate = queue.0.lock().unwrap();
        if !gate.ready {
            gate.pending.push((event, payload));
            return;
        }
    }
    let _ = app.emit(event, payload);
}
/// The page's measure of how tall the window should start; see
/// desktop::fit_start_height. Only the first one is taken.
#[tauri::command]
pub fn fit_start_height(height: f64, window: tauri::WebviewWindow) {
    crate::desktop::fit_start_height(&window, height);
}
#[tauri::command]
pub fn ui_ready(app: tauri::AppHandle) {
    let Some(queue) = app.try_state::<LinkQueue>() else {
        return;
    };
    // Let go in rounds: what arrives while a round is emitted is held, and
    // goes in the next round; the gate opens only once nothing is held.
    loop {
        let round = {
            let mut gate = queue.0.lock().unwrap();
            if gate.pending.is_empty() {
                gate.ready = true;
                return;
            }
            std::mem::take(&mut gate.pending)
        };
        for (event, payload) in round {
            let _ = app.emit(event, payload);
        }
    }
}
/// Runs a call to the phone off the async runtime, since it waits for the answer.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn reconnect(state: State<'_, AppState>) -> Result<(), String> {
    let phone = state.phone.clone();
    blocking(move || phone.call(Command::Connect)).await
}
#[tauri::command]
pub async fn save_configuration(
    settings: Settings,
    account: Account,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let phone = state.phone.clone();
    let previous = state.snapshot().settings;
    let language = settings.language.clone();
    let layout = settings.clone();
    if let Err(e) = shortcuts::apply(&app, &settings) {
        // Nothing was stored, so the keys go back to the ones that worked.
        let _ = shortcuts::apply(&app, &previous);
        return Err(e);
    }
    let saved = blocking(move || {
        phone.call(|reply| Command::SaveConfiguration { settings: Box::new(settings), account, reply })
    })
    .await;
    if saved.is_err() {
        let _ = shortcuts::apply(&app, &previous);
    } else {
        // What Windows draws follows the window's language from now on.
        crate::message::set_windows_language(&language);
        desktop::relabel_tray(&app);
        desktop::fit_window(&app, &layout);
    }
    saved
}
/// The custom buttons alone, from the window's button editing; saved and
/// taken in at once, a call up or not.
#[tauri::command]
pub async fn save_buttons(buttons: Vec<crate::settings::CustomButton>, app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let phone = state.phone.clone();
    let saved = blocking(move || phone.call(|reply| Command::SaveButtons { buttons, reply })).await;
    if saved.is_ok() {
        desktop::fit_window(&app, &state.snapshot().settings);
    }
    saved
}
/// The window's button editing began or ended: while it lasts, the window
/// is wide enough for the panel's empty slots too.
#[tauri::command]
pub fn set_button_editing(editing: bool, app: tauri::AppHandle, state: State<'_, AppState>) {
    desktop::set_button_editing(editing);
    desktop::fit_window(&app, &state.snapshot().settings);
}
/// The settings a policy fixes, by name (a button's five values each): read
/// when KSIP started, and the same until it starts again. The window locks
/// them; a save checks them again (Services::unmanaged).
#[tauri::command]
pub fn managed_settings(state: State<AppState>) -> Vec<String> {
    state.services.store.policy.fixed()
}
/// The settings a save needs the engine started again for, by name, so that
/// the window can say which save reconnects.
#[tauri::command]
pub fn restart_settings() -> Vec<&'static str> {
    crate::settings::Settings::RESTART.to_vec()
}
#[tauri::command]
pub async fn action(
    name: String,
    id: String,
    value: String,
    line: u8,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let phone = state.phone.clone();
    blocking(move || phone.call(|reply| Command::Action { name, id, value, line, reply })).await
}
#[tauri::command]
pub fn open_recordings(state: State<AppState>) -> Result<(), String> {
    state.services.open_recordings()
}
#[tauri::command]
pub fn open_recording(name: String, state: State<AppState>) -> Result<(), String> {
    state.services.open_recording(&name)
}
#[tauri::command]
pub fn open_recording_location(name: String, state: State<AppState>) -> Result<(), String> {
    state.services.open_recording_location(&name)
}
#[tauri::command]
pub async fn choose_sound_file(kind: Option<String>) -> Result<String, String> {
    let kind = kind.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || native::choose_file(&kind).unwrap_or_default())
        .await
        .map_err(|e| e.to_string())
}
/// Writes the saved settings (what KSIP uses, not what the dialog holds
/// unsaved) to a file the person chooses; the same document as
/// `ksip.exe --export-settings`. The path written, or empty if cancelled.
#[tauri::command]
pub async fn export_settings_file(state: State<'_, AppState>) -> Result<String, String> {
    let document = crate::settings::export_settings(&state.services.store);
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = native::choose_save_file("ksip-settings.json") else {
            return Ok(String::new());
        };
        let text = serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| crate::message::message_with("SETTINGS_EXPORT_FAILED", [e]))?;
        Ok(path)
    })
    .await
    .map_err(|e| e.to_string())?
}
/// Reads a settings file the person chooses into what the dialog takes (see
/// settings::import_settings), or null if cancelled. Nothing is saved here.
/// What a policy fixes on this machine is left out.
#[tauri::command]
pub async fn import_settings_file(state: State<'_, AppState>) -> Result<Option<serde_json::Value>, String> {
    let store = state.services.store.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = native::choose_file("settings") else {
            return Ok(None);
        };
        let bytes = std::fs::read(&path).map_err(|e| crate::message::message_with("SETTINGS_IMPORT_FAILED", [e]))?;
        if bytes.len() > 1024 * 1024 {
            return Err(crate::message::message("SETTINGS_IMPORT_FORMAT"));
        }
        let text = String::from_utf8_lossy(&bytes);
        crate::settings::import_settings(text.trim_start_matches('\u{feff}'), |name| store.managed(name)).map(Some)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn clear_call_history(state: State<AppState>) -> Result<(), String> {
    state.services.clear_call_history()
}
#[tauri::command]
pub fn clear_logs(state: State<AppState>) {
    state.services.clear_logs();
}
#[tauri::command]
pub fn copy_text(text: String) -> Result<(), String> {
    native::copy_text(&text)
}
#[tauri::command]
pub fn read_logs(after: u64, state: State<AppState>) -> LogPage {
    state.services.read_logs(after)
}
#[tauri::command]
pub async fn list_adapters() -> Result<Vec<native::Adapter>, String> {
    tauri::async_runtime::spawn_blocking(native::adapters)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn log_ui(text: String, state: State<AppState>) {
    state.services.log_ui(text);
}
#[tauri::command]
pub fn read_call_history(state: State<AppState>) -> Vec<CallHistory> {
    state.services.read_call_history()
}
#[tauri::command]
pub async fn refresh_devices(state: State<'_, AppState>) -> Result<(), String> {
    let phone = state.phone.clone();
    blocking(move || phone.call(Command::RefreshDevices)).await
}
/// Where the window's volume, mute and meter for a kind of device go now
/// (audio.rs, target), from the published snapshot: the saved choice, and
/// the call's endpoint from the engine's state report.
pub fn target(state: &AppState, kind: &str, classify: bool) -> Result<Target, String> {
    let (choice, call) = state.audio_choice(kind);
    crate::audio::target(kind, &choice, call.as_ref(), classify)
}
/// The device chosen not there, and there again: said once each way in the
/// app's log, not once a second for as long as it is gone.
fn note_stand_in(state: &AppState, kind: &str, target: &Target) {
    static ABSENT: Mutex<[bool; 2]> = Mutex::new([false, false]);
    let index = usize::from(kind == "speaker");
    let absent = target.stand_in == Some("absent");
    let mut noted = ABSENT.lock().unwrap();
    if noted[index] == absent {
        return;
    }
    noted[index] = absent;
    state.services.log(
        LOG_APP,
        if absent {
            format!("ksip: the {kind} chosen is not there; {} serves in its place", target.id)
        } else {
            format!("ksip: the {kind} chosen is there again")
        },
    );
}
/// What the window shows for a kind of device: Windows' level and mute of
/// the endpoint in use, the saved gain laid over, and whose endpoint it is.
#[derive(Clone, Serialize)]
pub struct VolumeView {
    pub id: String,
    pub level: u16,
    pub muted: bool,
    pub origin: &'static str,
    pub stand_in: Option<&'static str>,
}
/// Reads a volume, or sets it. Reading asks Windows and lays the saved gain
/// over; setting goes through the phone, since the engine and the settings
/// take the change too, and the phone looks the endpoint in use up when it
/// gets to the change, not before it is queued: a change meant for another
/// endpoint than the one in use then (`expected`, the one the window was
/// shown; the call moved meanwhile) is not made, and the answer says what
/// is in use. The arguments are checked here, before anything is asked of
/// Windows.
pub fn volume(state: &AppState, kind: &str, level: Option<u16>, mute: Option<bool>, expected: Option<&str>) -> Result<VolumeView, String> {
    if !matches!(kind, "microphone" | "speaker") || level.is_some_and(|value| value > 200) {
        return Err(message("AUDIO_VOLUME_ARGUMENT_INVALID"));
    }
    let (result, target) = if level.is_some() || mute.is_some() {
        let (kind, expected) = (kind.to_string(), expected.map(str::to_string));
        state.phone.call(|reply| Command::SetVolume { kind, level, mute, expected, reply })?
    } else {
        let target = target(state, kind, true)?;
        let mut result = crate::audio::volume(kind, &target.id, None, None)?;
        let gain = state.audio_gain(kind);
        if gain > 100 {
            result.level = gain;
        }
        (result, target)
    };
    note_stand_in(state, kind, &target);
    Ok(VolumeView { id: result.id, level: result.level, muted: result.muted, origin: target.origin, stand_in: target.stand_in })
}
#[tauri::command]
pub async fn audio_volume(
    kind: String,
    level: Option<u16>,
    mute: Option<bool>,
    expected: Option<String>,
    state: State<'_, AppState>,
) -> Result<VolumeView, String> {
    let state = state.inner().clone();
    blocking(move || volume(&state, &kind, level, mute, expected.as_deref())).await
}
/// The level of the endpoint in use for the kind (target), ten times a second.
#[tauri::command]
pub async fn audio_peak(kind: String, state: State<'_, AppState>) -> Result<Peak, String> {
    let state = state.inner().clone();
    blocking(move || {
        let target = target(&state, &kind, false)?;
        crate::audio::peak(&kind, &target.id)
    })
    .await
}
#[tauri::command]
pub async fn calibrate_aec(
    microphone: String,
    speaker: String,
    careful: bool,
    state: State<'_, AppState>,
) -> Result<Calibration, String> {
    let phone = state.phone.clone();
    blocking(move || phone.call(|reply| Command::CalibrateAec { microphone, speaker, careful, reply })).await
}
#[tauri::command]
pub async fn select_audio_device(
    kind: String,
    device: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let phone = state.phone.clone();
    blocking(move || phone.call(|reply| Command::SelectAudioDevice { kind, device, reply })).await
}
#[tauri::command]
pub fn open_sound_control(state: State<AppState>) -> Result<(), String> {
    state.services.open_sound_control()
}
#[tauri::command]
pub fn open_microphone_privacy(state: State<AppState>) -> Result<(), String> {
    state.services.open_microphone_privacy()
}
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}
/// Opens the web address a button was set up with, and no other.
#[tauri::command]
pub fn open_link(url: String, state: State<AppState>) -> Result<(), String> {
    let wanted = url.trim();
    let known = state
        .snapshot()
        .settings
        .buttons
        .iter()
        .any(|b| b.link_target() == Some(wanted));
    if !known {
        return Err(message("LINK_TARGET_INVALID"));
    }
    state.services.log_app(format!("ksip: opening {wanted}"));
    native::open_url(wanted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_invalid_volume_without_launching_helper() {
        let app = AppState::new();
        assert!(volume(&app, "other", None, None, None).is_err());
        assert!(volume(&app, "speaker", Some(201), None, None).is_err());
        assert!(target(&app, "other", true).is_err());
        assert!(crate::audio::resolve("microphone", "bad\0id").is_err());
        assert!(crate::audio::peak("other", "default").is_err());
        assert!(crate::audio::peak("microphone", "bad\0id").is_err());
    }
}
