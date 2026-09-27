//! The desktop side: the window, the tray, the notifications, the links
//! that arrive while the app runs, and the loop that watches the snapshot
//! for what the desktop has to do about it. It reads the phone and sends
//! it commands; what happens to a call is the actor's to decide.
use crate::app::AppState;
use crate::phone_message::Command;
use crate::settings::Settings;
use crate::{commands, message, phone_state, protocol, settings, shortcuts};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;

/// The tray menu's items, kept so that their words can follow the language.
struct TrayMenu {
    open: MenuItem<tauri::Wry>,
    licenses: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}
pub fn relabel_tray(app: &tauri::AppHandle) {
    let text = message::windows_text;
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.open.set_text(text("TRAY_OPEN"));
        let _ = menu.licenses.set_text(text("TRAY_LICENSES"));
        let _ = menu.quit.set_text(text("TRAY_QUIT"));
    }
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
pub fn fit_window(app: &tauri::AppHandle, settings: &Settings) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let extended = settings
        .buttons
        .iter()
        .skip(settings::CustomButton::MAIN)
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
/// The first thing after the window is up: the link registration, the
/// recordings earlier runs left, then the phone itself (devices, then the
/// saved account).
fn initialize(state: &AppState) {
    state.services.apply_browser_integration();
    state.services.sweep_recordings();
    state.phone.send(Command::Initialize);
}
/// Builds the window and the tray and runs the app until it exits.
pub fn run(state: AppState) {
    let exit_state = state.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::reconnect,
            commands::save_configuration,
            commands::ui_ready,
            commands::action,
            commands::open_recordings,
            commands::open_recording,
            commands::open_recording_location,
            commands::choose_sound_file,
            commands::export_settings_file,
            commands::import_settings_file,
            commands::read_logs,
            commands::clear_call_history,
            commands::clear_logs,
            commands::copy_text,
            commands::log_ui,
            commands::list_adapters,
            commands::read_call_history,
            commands::refresh_devices,
            commands::audio_volume,
            commands::audio_peak,
            commands::calibrate_aec,
            commands::select_audio_device,
            commands::open_sound_control,
            commands::open_link,
            commands::quit_app,
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
                        if let Err(e) = state.services.open_licenses() {
                            state.phone.send(Command::ReportError(e));
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
                state.services.log_app(e);
            }
            fit_window(app.handle(), &state.snapshot().settings);
            app.manage(commands::LinkQueue::new());
            let served = app.handle().clone();
            protocol::serve(move |link| {
                let state = served.state::<AppState>();
                state.services.log_protocol(&link);
                // Operating the phone from outside is program integration's;
                // the link process checks the stored setting first, and this
                // checks the one the app runs with, for any other client of
                // the pipe.
                if protocol::operates_phone(&link) && !state.snapshot().settings.program_integration {
                    state.services.log_app(message::message_with("PROGRAM_INTEGRATION_OFF", [&link]));
                    return;
                }
                match protocol::parse(&link) {
                    Ok(protocol::Link::ShowWindow) => show(&served),
                    Ok(protocol::Link::Quit) => served.exit(0),
                    Ok(protocol::Link::Answer) => commands::emit_link(&served, "ksip-link", serde_json::json!("ANSWER")),
                    Ok(protocol::Link::Hangup) => commands::emit_link(&served, "ksip-link", serde_json::json!("HANGUP")),
                    Ok(protocol::Link::Dial(text)) => match settings::dial_target(&text) {
                        Ok(target) => {
                            // Dialling asks first unless that was turned off, and
                            // always for a URI, which a page can point anywhere.
                            // The window asks; a question put from the tray would
                            // go unseen, so the window comes out before it is asked.
                            let confirm = state.snapshot().settings.browser_dial_confirm
                                || settings::CustomButton::is_uri(&target);
                            if confirm {
                                show(&served);
                            }
                            commands::emit_link(&served, "ksip-dial", serde_json::json!({"target": target, "confirm": confirm}));
                        }
                        Err(e) => {
                            // What was refused goes into the dial box beside
                            // the error, so that the person sees what came.
                            show(&served);
                            state.phone.send(Command::ShowError(e));
                            commands::emit_link(&served, "ksip-dial", serde_json::json!({"target": text, "refused": true}));
                        }
                    },
                    Err(e) => {
                        // Not a polling error, which the next poll would clear:
                        // it stays until the person sees it.
                        show(&served);
                        state.phone.send(Command::ShowError(e));
                    }
                }
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                initialize(&state);
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
                    state.phone.send(Command::WindowVisible(is_visible(&handle)));
                    let snapshot = state.snapshot();
                    let incoming: std::collections::HashSet<String> = snapshot
                        .calls
                        .iter()
                        .filter(|c| c.state == "INCOMING")
                        .map(|c| c.id.clone())
                        .collect();
                    let fresh: Vec<&phone_state::CallInfo> = snapshot
                        .calls
                        .iter()
                        .filter(|c| c.state == "INCOMING" && !previous_incoming.contains(&c.id))
                        .collect();
                    if !fresh.is_empty() {
                        if snapshot.settings.incoming_action == "notify" && !is_visible(&handle) {
                            for call in &fresh {
                                shortcuts::notify_incoming(&handle, &phone_state::caller_label(call));
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
                                state.services.log_app("ksip: window to the tray after the call".into());
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
}
