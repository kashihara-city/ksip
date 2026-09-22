//! Global shortcuts, and what an incoming call does while the window is away.
use crate::message::{message, message_with};
use crate::engine::{AppState, Settings};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

/// The identifier Windows knows the app by, and where a toast looks it up.
const AUMID_PATH: &str = r"Software\Classes\AppUserModelId\local.ksip.client";
const ICON_NAME: &str = "ksip-notification.ico";
const ICON: &[u8] = include_bytes!("../icons/icon.ico");

/// A shortcut is written as `SHIFT+F2`. Empty means the key is not used.
pub fn parse(text: &str) -> Result<Option<Shortcut>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<Shortcut>()
        .map(Some)
        .map_err(|_| message_with("SHORTCUT_SYNTAX", [text]))
}

/// Reads both shortcuts, and says so when they would fight each other.
pub fn parse_settings(settings: &Settings) -> Result<(Option<Shortcut>, Option<Shortcut>), String> {
    let window = parse(&settings.shortcut_window)?;
    let call = parse(&settings.shortcut_call)?;
    if window.is_some() && window == call {
        return Err(message("SHORTCUT_DUPLICATE"));
    }
    Ok((window, call))
}

/// Registers the shortcuts the settings ask for, in place of the current ones.
///
/// Windows refuses `RegisterHotKey` for a window owned by another thread, so
/// this has to run on the main one; [`apply`] is the way in from anywhere else.
pub fn apply_now(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let (window, call) = parse_settings(settings)?;
    let manager = app.global_shortcut();
    let _ = manager.unregister_all();
    if let Some(shortcut) = window {
        register(app, shortcut, &settings.shortcut_window, |app| toggle(app))?;
    }
    if let Some(shortcut) = call {
        register(app, shortcut, &settings.shortcut_call, |app| answer_or_hangup(app))?;
    }
    Ok(())
}

fn register(
    app: &AppHandle,
    shortcut: Shortcut,
    text: &str,
    action: fn(&AppHandle),
) -> Result<(), String> {
    app.global_shortcut()
        .on_shortcut(shortcut, move |app, _, event| {
            if event.state == ShortcutState::Pressed {
                action(app);
            }
        })
        .map_err(|e| {
            app.state::<AppState>()
                .log_app(format!("shortcut {} {e}", text.trim()));
            message_with("SHORTCUT_TAKEN", [text.trim()])
        })
}

/// Applies the settings from a thread that is not the main one.
pub fn apply(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    parse_settings(settings)?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let settings = settings.clone();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = sender.send(apply_now(&handle, &settings));
    })
    .map_err(|e| e.to_string())?;
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| Err(message("SHORTCUT_REGISTER_FAILED")))
}

/// Puts the window away, or brings it back.
fn toggle(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false) {
        state.log_app(message("SHORTCUT_WINDOW_HIDDEN"));
        let _ = window.hide();
    } else {
        state.log_app(message("SHORTCUT_WINDOW_SHOWN"));
        crate::show(app);
    }
}

/// One key for both ends of a call: answer while it rings, hang up otherwise.
fn answer_or_hangup(app: &AppHandle) {
    let state = app.state::<AppState>();
    let ringing = state
        .snapshot()
        .calls
        .iter()
        .any(|call| call.state == "INCOMING");
    let command = if ringing { "ANSWER" } else { "HANGUP" };
    state.log_app(format!("shortcut {command}"));
    // The window knows which line is selected, so it does the work, exactly as
    // it does for a ksip: link.
    let _ = app.emit("ksip-link", command.to_string());
}

/// Tells the person about a call while the window is in the tray.
pub fn notify_incoming(app: &AppHandle, peer: &str) {
    let state = app.state::<AppState>();
    match app
        .notification()
        .builder()
        .title("KSIP")
        .body(crate::message::windows_text_with("NOTIFY_INCOMING", [peer]))
        .show()
    {
        Ok(()) => state.log_app(message_with("NOTIFY_INCOMING_SHOWN", [peer])),
        Err(e) => state.log_app(message_with("NOTIFY_FAILED", [e])),
    }
}

/// The icon a toast shows, kept beside the executable like everything else.
fn notification_icon() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join(ICON_NAME);
    if path.is_file() {
        return Some(path);
    }
    std::fs::write(&path, ICON).ok().map(|_| path)
}

/// Registers the name and icon Windows shows on a toast, for this user only.
///
/// A toast from a plain desktop program is delivered under its identifier, and
/// Windows looks that identifier up here; without it nothing appears.
pub fn ensure_notification_registration() -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey_with_flags(AUMID_PATH, KEY_READ | KEY_WRITE)
        .map_err(|e| e.to_string())?;
    key.set_value("DisplayName", &"KSIP".to_string())
        .map_err(|e| e.to_string())?;
    // The name alone is enough to be shown; without an icon Windows uses its own.
    match notification_icon() {
        Some(icon) => key
            .set_value("IconUri", &icon.to_string_lossy().to_string())
            .map_err(|e| e.to_string()),
        None => match key.delete_value("IconUri") {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_shortcut_is_not_a_shortcut() {
        assert!(parse("").unwrap().is_none());
        assert!(parse("   ").unwrap().is_none());
    }

    #[test]
    fn a_shortcut_is_written_with_plus_signs() {
        assert!(parse("SHIFT+F2").unwrap().is_some());
        assert!(parse("CONTROL+ALT+K").unwrap().is_some());
        assert!(parse("F3").unwrap().is_some());
    }

    #[test]
    fn a_misspelled_shortcut_is_reported_with_what_was_typed() {
        let reported = parse("SHIFT F2").unwrap_err();
        let (code, args) = crate::message::split(&reported);
        assert_eq!(code, "SHORTCUT_SYNTAX");
        assert_eq!(args, vec!["SHIFT F2".to_string()]);
    }

    #[test]
    fn the_two_shortcuts_have_to_differ() {
        let mut settings = Settings::default();
        settings.shortcut_window = "SHIFT+F2".into();
        settings.shortcut_call = "shift+f2".into();
        assert!(parse_settings(&settings).is_err());
        settings.shortcut_call = "SHIFT+F3".into();
        assert!(parse_settings(&settings).is_ok());
    }
}
