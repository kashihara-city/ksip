#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crate::message::message_with;
mod audio;
mod engine;
mod licenses;
mod message;
mod mp3;
mod native;
mod phone_state;
mod protocol;
mod shortcuts;
mod storage;
mod trust;
mod wav;
use engine::{AppState, Settings};
use storage::Account;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State};

#[tauri::command]
fn snapshot(state: State<AppState>) -> engine::Snapshot {
    state.snapshot()
}
/// Link events for the page, held until it says it listens. Windows starts
/// the app for a link before the page has loaded, and an event emitted into
/// a page without listeners reaches nobody; `ui_ready` lets them go, in order.
struct LinkQueue(std::sync::Mutex<Option<Vec<(&'static str, serde_json::Value)>>>);
fn emit_link(app: &tauri::AppHandle, event: &'static str, payload: serde_json::Value) {
    if let Some(queue) = app.try_state::<LinkQueue>() {
        let mut held = queue.0.lock().unwrap();
        if let Some(pending) = held.as_mut() {
            pending.push((event, payload));
            return;
        }
    }
    let _ = app.emit(event, payload);
}
#[tauri::command]
fn ui_ready(app: tauri::AppHandle) {
    let pending = app.try_state::<LinkQueue>().and_then(|queue| queue.0.lock().unwrap().take());
    for (event, payload) in pending.unwrap_or_default() {
        let _ = app.emit(event, payload);
    }
}
#[tauri::command]
async fn reconnect(state: State<'_, AppState>) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.connect())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn save_configuration(
    settings: Settings,
    account: Account,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state = state.inner().clone();
    let previous = state.snapshot().settings;
    let language = settings.language.clone();
    let layout = settings.clone();
    if let Err(e) = shortcuts::apply(&app, &settings) {
        // Nothing was stored, so the keys go back to the ones that worked.
        let _ = shortcuts::apply(&app, &previous);
        return Err(e);
    }
    let saved = tauri::async_runtime::spawn_blocking(move || {
        state.save_configuration(settings, account)
    })
    .await
    .map_err(|e| e.to_string())?;
    if saved.is_err() {
        let _ = shortcuts::apply(&app, &previous);
    } else {
        // What Windows draws follows the window's language from now on.
        message::set_windows_language(&language);
        relabel_tray(&app);
        fit_window(&app, &layout);
    }
    saved
}
/// The tray menu's items, kept so that their words can follow the language.
struct TrayMenu {
    open: MenuItem<tauri::Wry>,
    licenses: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}
fn relabel_tray(app: &tauri::AppHandle) {
    let text = message::windows_text;
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.open.set_text(text("TRAY_OPEN"));
        let _ = menu.licenses.set_text(text("TRAY_LICENSES"));
        let _ = menu.quit.set_text(text("TRAY_QUIT"));
    }
}
#[tauri::command]
async fn action(
    name: String,
    id: String,
    value: String,
    line: u8,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.action(&name, &id, &value, line))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
fn open_recordings(state: State<AppState>) -> Result<(), String> {
    state.open_recordings()
}
#[tauri::command]
fn open_recording(name: String, state: State<AppState>) -> Result<(), String> {
    state.open_recording(&name)
}
#[tauri::command]
fn open_recording_location(name: String, state: State<AppState>) -> Result<(), String> {
    state.open_recording_location(&name)
}
#[tauri::command]
async fn choose_sound_file(kind: Option<String>) -> Result<String, String> {
    let kind = kind.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || native::choose_file(&kind).unwrap_or_default())
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn clear_call_history(state: State<AppState>) -> Result<(), String> {
    state.clear_call_history()
}
#[tauri::command]
fn clear_logs(state: State<AppState>) {
    state.clear_logs();
}
#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    native::copy_text(&text)
}
#[tauri::command]
fn read_logs(after: u64, state: State<AppState>) -> engine::LogPage {
    state.read_logs(after)
}
#[tauri::command]
async fn list_adapters() -> Result<Vec<native::Adapter>, String> {
    tauri::async_runtime::spawn_blocking(native::adapters)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn log_ui(text: String, state: State<AppState>) {
    state.log_ui(text);
}
#[tauri::command]
fn read_call_history(state: State<AppState>) -> Vec<engine::CallHistory> {
    state.read_call_history()
}
#[tauri::command]
async fn refresh_devices(state: State<'_, AppState>) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.refresh_devices())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn audio_volume(
    kind: String,
    device: String,
    level: Option<u16>,
    mute: Option<bool>,
    state: State<'_, AppState>,
) -> Result<engine::Volume, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.volume(&kind, &device, level, mute))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn audio_peak(
    kind: String,
    device: String,
    state: State<'_, AppState>,
) -> Result<engine::Peak, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.peak(&kind, &device))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn calibrate_aec(
    microphone: String,
    speaker: String,
    careful: bool,
    state: State<'_, AppState>,
) -> Result<engine::Calibration, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.calibrate_aec(&microphone, &speaker, careful)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn select_audio_device(
    kind: String,
    device: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.select_audio_device(&kind, device))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
fn open_sound_control(state: State<AppState>) -> Result<(), String> {
    state.open_sound_control()
}
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}
/// Opens the web address a button was set up with, and no other.
#[tauri::command]
fn open_link(url: String, state: State<AppState>) -> Result<(), String> {
    let wanted = url.trim();
    let known = state
        .snapshot()
        .settings
        .buttons
        .iter()
        .any(|b| b.link_target() == Some(wanted));
    if !known {
        return Err(message::message("LINK_TARGET_INVALID"));
    }
    state.log_app(format!("ksip: opening {wanted}"));
    native::open_url(wanted)
}
pub fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
/// The window is twice as wide while the panel beside the phone has buttons,
/// and back to its own width once it has none. A width the person chose in
/// between is left alone: only a change of state moves it.
fn fit_window(app: &tauri::AppHandle, settings: &Settings) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let extended = settings
        .buttons
        .iter()
        .skip(engine::CustomButton::MAIN)
        .any(|b| b.configured());
    let scale = window.scale_factor().unwrap_or(1.0);
    let Ok(size) = window.inner_size() else {
        return;
    };
    let (width, height) = (size.width as f64 / scale, size.height as f64 / scale);
    let wanted = if extended { 1040.0 } else { 520.0 };
    if (extended && width < 840.0) || (!extended && width > 800.0) {
        let _ = window.set_min_size(Some(tauri::LogicalSize::new(if extended { 840.0 } else { 420.0 }, 620.0)));
        let _ = window.set_size(tauri::LogicalSize::new(wanted, height));
    }
}
fn is_visible(app: &tauri::AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(true)
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--engine") {
        std::process::exit(native::run_engine(&args[1..]));
    }
    // A link is handed over as it came, so that the running app can record
    // exactly what the browser passed before it reads anything into it.
    let link = match args.first() {
        None => String::new(),
        Some(first) if protocol::is_link(first) => first.clone(),
        Some(_) => std::process::exit(2),
    };
    if !link.is_empty() {
        // Hand the command over if the app is running. If it is not, start it
        // and wait for the pipe, so that a link works from a cold start too.
        // This process has no window; what goes wrong is put in the app's log
        // file, which the app picks up.
        let failed = |what: &str| -> ! {
            engine::append_log(&engine::data_dir(&storage::Store::new()), format!("protocol {link} {what}"));
            std::process::exit(3)
        };
        match protocol::send(&link) {
            Ok(true) => std::process::exit(0),
            Err(e) => failed(&format!("could not be handed over: {e}")),
            Ok(false) => {}
        }
        if protocol::parse(&link) == Ok(protocol::Link::Quit) {
            std::process::exit(0);
        }
        let Ok(exe) = std::env::current_exe() else {
            failed("could not start the app");
        };
        if std::process::Command::new(exe).spawn().is_err() {
            failed("could not start the app");
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if matches!(protocol::send(&link), Ok(true)) {
                std::process::exit(0);
            }
        }
        failed("was not taken within 25 seconds of starting the app");
    }

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
    };
    let name = format!("Local\\{}", storage::Store::new().target.replace('/', "_"));
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let singleton = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if singleton.is_null() {
        return;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(singleton) };
        return;
    }
    let state = AppState::new();
    message::set_windows_language(&state.snapshot().settings.language);
    // Without this the process would die silently: the window subsystem has no
    // console for the default panic message. Nothing before this line panics.
    let panic_state = state.clone();
    std::panic::set_hook(Box::new(move |info| {
        panic_state.log_panic(format!("panic {info}"));
    }));
    // A toast is delivered under the application identifier, and Windows
    // looks that identifier up in the registry. This is for the current user,
    // so it needs no administrator, and a failure must not stop the app.
    if let Err(e) = shortcuts::ensure_notification_registration(state.data()) {
        state.log_app(message_with("NOTIFICATION_REGISTER_FAILED", [e]));
    }
    let exit_state = state.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            reconnect,
            save_configuration,
            ui_ready,
            action,
            open_recordings,
            open_recording,
            open_recording_location,
            choose_sound_file,
            read_logs,
            clear_call_history,
            clear_logs,
            copy_text,
            log_ui,
            list_adapters,
            read_call_history,
            refresh_devices,
            audio_volume,
            audio_peak,
            calibrate_aec,
            select_audio_device,
            open_sound_control,
            open_link,
            quit_app
        ])
        .setup(move |app| {
            let text = message::windows_text;
            let open = MenuItem::with_id(app, "open", text("TRAY_OPEN"), true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", text("TRAY_QUIT"), true, None::<&str>)?;
            let licenses =
                MenuItem::with_id(app, "licenses", text("TRAY_LICENSES"), true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &licenses, &quit])?;
            app.manage(TrayMenu {
                open: open.clone(),
                licenses: licenses.clone(),
                quit: quit.clone(),
            });
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("KSIP")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => show(app),
                    "quit" => app.exit(0),
                    "licenses" => {
                        let state = app.state::<AppState>();
                        if let Err(e) = state.open_licenses() {
                            state.report_error(e);
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, e| {
                    if matches!(
                        e,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show(tray.app_handle());
                    }
                })
                .build(app)?;
            // Registering a hot key has to happen on this thread, and an
            // empty setting simply leaves the key alone.
            if let Err(e) = shortcuts::apply_now(app.handle(), &state.snapshot().settings) {
                state.log_app(e);
            }
            fit_window(app.handle(), &state.snapshot().settings);
            app.manage(LinkQueue(std::sync::Mutex::new(Some(Vec::new()))));
            let served = app.handle().clone();
            protocol::serve(move |link| {
                let state = served.state::<AppState>();
                state.log_protocol(&link);
                match protocol::parse(&link) {
                    Ok(protocol::Link::ShowWindow) => show(&served),
                    Ok(protocol::Link::Quit) => served.exit(0),
                    Ok(protocol::Link::Answer) => emit_link(&served, "ksip-link", serde_json::json!("ANSWER")),
                    Ok(protocol::Link::Hangup) => emit_link(&served, "ksip-link", serde_json::json!("HANGUP")),
                    Ok(protocol::Link::Dial(text)) => match engine::dial_target(&text) {
                        Ok(target) => {
                            // Dialling asks first unless that was turned off, and
                            // always for a URI, which a page can point anywhere.
                            // The window asks; a question put from the tray would
                            // go unseen, so the window comes out before it is asked.
                            let confirm = state.snapshot().settings.browser_dial_confirm
                                || engine::CustomButton::is_uri(&target);
                            if confirm {
                                show(&served);
                            }
                            emit_link(&served, "ksip-dial", serde_json::json!({"target": target, "confirm": confirm}));
                        }
                        Err(e) => {
                            // What was refused goes into the dial box beside
                            // the error, so that the person sees what came.
                            show(&served);
                            state.show_error(e);
                            emit_link(&served, "ksip-dial", serde_json::json!({"target": text, "refused": true}));
                        }
                    },
                    Err(e) => {
                        // Not a polling error, which the next poll would clear:
                        // it stays until the person sees it.
                        show(&served);
                        state.show_error(e);
                    }
                }
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                state.initialize();
                let mut previous_incoming = std::collections::HashSet::new();
                // Calls the person was told about rather than shown; answering
                // one of them brings the window back.
                let mut announced = std::collections::HashSet::new();
                // The window goes to the tray a set time after the last call
                // ends, if the settings ask for it; a new call calls it off.
                let mut had_calls = false;
                let mut hide_at: Option<std::time::Instant> = None;
                while !state.is_closing() {
                    // The window stops asking for the microphone level while it
                    // is in the tray, which lets the microphone close.
                    state.set_window_visible(is_visible(&handle));
                    match state.sync_phone() {
                        Err(e) => state.report_error(e),
                        Ok(()) => state.clear_polling_error(),
                    }
                    state.follow_network();
                    let snapshot = state.snapshot();
                    let incoming: std::collections::HashSet<String> = snapshot
                        .calls
                        .iter()
                        .filter(|c| c.state == "INCOMING")
                        .map(|c| c.id.clone())
                        .collect();
                    let fresh: Vec<&engine::CallInfo> = snapshot
                        .calls
                        .iter()
                        .filter(|c| c.state == "INCOMING" && !previous_incoming.contains(&c.id))
                        .collect();
                    if !fresh.is_empty() {
                        if snapshot.settings.incoming_action == "notify" && !is_visible(&handle) {
                            for call in &fresh {
                                shortcuts::notify_incoming(&handle, &engine::caller_label(call));
                                announced.insert(call.id.clone());
                            }
                        } else {
                            show(&handle);
                        }
                    }
                    if !announced.is_empty() {
                        if snapshot
                            .calls
                            .iter()
                            .any(|c| announced.contains(&c.id) && c.state != "INCOMING")
                        {
                            show(&handle);
                        }
                        announced.retain(|id| incoming.contains(id));
                    }
                    previous_incoming = incoming;
                    let in_call = !snapshot.calls.is_empty();
                    if in_call {
                        hide_at = None;
                    } else if had_calls && snapshot.settings.tray_after_call >= 0 {
                        hide_at = Some(
                            std::time::Instant::now()
                                + std::time::Duration::from_secs(snapshot.settings.tray_after_call as u64),
                        );
                    }
                    had_calls = in_call;
                    if hide_at.is_some_and(|at| std::time::Instant::now() >= at) {
                        hide_at = None;
                        if is_visible(&handle) {
                            if let Some(window) = handle.get_webview_window("main") {
                                let _ = window.hide();
                                state.log_app("ksip: window to the tray after the call".into());
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("Unable to start KSIP")
        .run(move |_, event| {
            if let tauri::RunEvent::Exit = event {
                exit_state.shutdown();
            }
        });
    unsafe {
        CloseHandle(singleton);
    }
}
