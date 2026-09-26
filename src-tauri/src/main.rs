#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod audio;
mod commands;
mod desktop;
mod engine_config;
mod engine_link;
mod history;
mod licenses;
mod logs;
mod message;
mod mp3;
mod native;
mod phone_actor;
mod phone_message;
mod phone_state;
mod protocol;
mod recordings;
mod settings;
mod shortcuts;
mod storage;
mod trust;
mod wav;
use app::AppState;
use message::message_with;

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
            logs::append_log(&logs::data_dir(&storage::Store::new()), format!("protocol {link} {what}"));
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
        panic_state.services.log_panic(format!("panic {info}"));
    }));
    // A toast is delivered under the application identifier, and Windows
    // looks that identifier up in the registry. This is for the current user,
    // so it needs no administrator, and a failure must not stop the app.
    if let Err(e) = shortcuts::ensure_notification_registration(state.services.data()) {
        state.services.log_app(message_with("NOTIFICATION_REGISTER_FAILED", [e]));
    }
    desktop::run(state);
    unsafe {
        CloseHandle(singleton);
    }
}
