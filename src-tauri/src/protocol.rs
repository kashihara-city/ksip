//! The browser integration: a `ksip:` link reaches the running app.
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE,
};

/// The browser hands a `ksip:` link to a new process. That process passes the
/// command to the one already running through this pipe, then exits.
pub fn pipe_name() -> String {
    format!(
        r"\\.\pipe\{}",
        crate::storage::Store::new().target.replace('/', "_")
    )
}

const SCHEME: &str = "ksip:";

/// Whether an argument is a link. Windows hands the link over whole, scheme
/// and all, and a scheme is the same in any case.
pub fn is_link(argument: &str) -> bool {
    argument.len() >= SCHEME.len() && argument[..SCHEME.len()].eq_ignore_ascii_case(SCHEME)
}

/// What a link asks for.
#[derive(Debug, PartialEq)]
pub enum Link {
    Answer,
    Hangup,
    ShowWindow,
    Quit,
    /// What is to be dialled, as it was written; the dial box's rule is
    /// applied to it afterwards, so that what was refused can still be shown.
    Dial(String),
}

/// Reads a link. The browser escapes what it passes on, so `%xx` is undone
/// first; `+` stays `+`, since that convention belongs to forms, not links,
/// and `+81` is a number. The four commands are matched as written; anything
/// else is to be dialled.
pub fn parse(link: &str) -> Result<Link, String> {
    let malformed = || crate::message::message_with("PROTOCOL_MALFORMED", [link]);
    if !is_link(link) {
        return Err(malformed());
    }
    let escaped = &link.as_bytes()[SCHEME.len()..];
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut i = 0;
    while i < escaped.len() {
        if escaped[i] == b'%' {
            let hex = escaped.get(i + 1..i + 3).ok_or_else(malformed)?;
            let text = std::str::from_utf8(hex).map_err(|_| malformed())?;
            bytes.push(u8::from_str_radix(text, 16).map_err(|_| malformed())?);
            i += 3;
        } else {
            bytes.push(escaped[i]);
            i += 1;
        }
    }
    let text = String::from_utf8(bytes).map_err(|_| malformed())?;
    if text.chars().any(char::is_control) {
        return Err(malformed());
    }
    Ok(match text.trim() {
        "ANSWER" => Link::Answer,
        "HANGUP" => Link::Hangup,
        "SHOWWINDOW" => Link::ShowWindow,
        "APP_QUIT" => Link::Quit,
        target => Link::Dial(target.to_string()),
    })
}

/// Sends a command to the running app. Ok(false) means nothing was listening.
///
/// The app serves one link at a time. A link that arrives while another is
/// being read finds the pipe busy, and in the instant between two links it
/// finds no pipe at all. Both pass within milliseconds, so the sender waits a
/// little before it concludes anything.
pub fn send(command: &str) -> Result<bool, String> {
    send_to(&pipe_name(), command)
}

fn send_to(name: &str, command: &str) -> Result<bool, String> {
    use std::io::Write;
    let started = Instant::now();
    const ABSENT_FOR: Duration = Duration::from_millis(300);
    const BUSY_FOR: Duration = Duration::from_secs(3);
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(name) {
            Ok(mut pipe) => {
                pipe.write_all(command.as_bytes()).map_err(|e| e.to_string())?;
                pipe.flush().map_err(|e| e.to_string())?;
                return Ok(true);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if started.elapsed() >= ABSENT_FOR {
                    return Ok(false);
                }
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                if started.elapsed() >= BUSY_FOR {
                    return Err(e.to_string());
                }
            }
            Err(e) => return Err(e.to_string()),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn create(name: &[u16]) -> Option<HANDLE> {
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    let pipe = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            1024,
            1024,
            0,
            std::ptr::null(),
        )
    };
    (!pipe.is_null() && pipe != INVALID_HANDLE_VALUE).then_some(pipe)
}

/// Reads what a connected client sent, within a bounded time. A client that
/// connects and then says nothing must not hold every later link up.
fn read_command(pipe: HANDLE) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut available = 0u32;
        let peeked = unsafe {
            PeekNamedPipe(
                pipe,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } != 0;
        // A peek fails once the client has closed its end, and what it wrote
        // before closing is still there to be read, without blocking.
        if !peeked || available > 0 {
            let mut buffer = [0u8; 1024];
            let mut read = 0u32;
            let ok = unsafe {
                ReadFile(
                    pipe,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            } != 0;
            return (ok && read > 0)
                .then(|| String::from_utf8_lossy(&buffer[..read as usize]).into_owned());
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Serves the pipe for as long as the app runs, handing each command to `deliver`.
pub fn serve(deliver: impl Fn(String) + Send + 'static) {
    serve_named(&pipe_name(), deliver)
}

fn serve_named(name: &str, deliver: impl Fn(String) + Send + 'static) {
    use windows_sys::Win32::System::Pipes::ConnectNamedPipe;
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    std::thread::spawn(move || {
        let mut pipe = create(&wide);
        loop {
            let Some(current) = pipe else {
                std::thread::sleep(Duration::from_secs(1));
                pipe = create(&wide);
                continue;
            };
            // A client that connected before this call is reported as an error
            // with a name of its own, and is connected all the same. One that
            // has already written and left is reported as "no data", and what
            // it wrote is still there to be read.
            let connected = unsafe { ConnectNamedPipe(current, std::ptr::null_mut()) } != 0
                || matches!(unsafe { GetLastError() }, ERROR_PIPE_CONNECTED | ERROR_NO_DATA);
            // The next instance is ready before this one is read, so a link that
            // arrives meanwhile finds a pipe instead of nothing.
            let next = create(&wide);
            if connected {
                if let Some(command) = read_command(current) {
                    deliver(command);
                }
            }
            // Closing the handle ends this instance; a disconnect first would
            // leave an instant in which a new client could attach to it and be
            // lost with it.
            unsafe { CloseHandle(current) };
            pipe = next;
        }
    });
}

/// Registers or removes a URL protocol (`ksip`, or a test profile's own
/// `ksip-test`) for this user. The command points at the running executable,
/// so a folder that has been moved fixes itself.
pub fn register(enabled: bool, scheme: &str) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS};
    use winreg::RegKey;
    let classes = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Classes", KEY_ALL_ACCESS)
        .map_err(|e| e.to_string())?;
    if !enabled {
        return match classes.delete_subkey_all(scheme) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().to_string();
    let (protocol, _) = classes.create_subkey(scheme).map_err(|e| e.to_string())?;
    protocol
        .set_value("", &"URL:KSIP Protocol".to_string())
        .map_err(|e| e.to_string())?;
    protocol
        .set_value("URL Protocol", &String::new())
        .map_err(|e| e.to_string())?;
    let (icon, _) = protocol
        .create_subkey("DefaultIcon")
        .map_err(|e| e.to_string())?;
    icon.set_value("", &format!("{exe},0"))
        .map_err(|e| e.to_string())?;
    let (command, _) = protocol
        .create_subkey(r"shell\open\command")
        .map_err(|e| e.to_string())?;
    command
        .set_value("", &format!("\"{exe}\" \"%1\""))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_is_told_by_its_scheme_in_any_case() {
        assert!(is_link("ksip:9001"));
        assert!(is_link("KSIP:ANSWER"));
        assert!(!is_link("/ksip=ksip:9001"));
        assert!(!is_link("9001"));
        assert!(!is_link(""));
    }

    #[test]
    fn links_are_read_as_the_browser_passes_them() {
        assert_eq!(parse("ksip:ANSWER"), Ok(Link::Answer));
        assert_eq!(parse("KSIP:HANGUP"), Ok(Link::Hangup));
        assert_eq!(parse("ksip:SHOWWINDOW"), Ok(Link::ShowWindow));
        assert_eq!(parse("ksip:APP_QUIT"), Ok(Link::Quit));
        assert_eq!(parse("ksip:0742-22-1101"), Ok(Link::Dial("0742-22-1101".into())));
        assert_eq!(parse("ksip:(06)%201234.5678%20"), Ok(Link::Dial("(06) 1234.5678".into())));
        assert_eq!(parse("ksip:%2B81+6"), Ok(Link::Dial("+81+6".into())));
        assert_eq!(parse("ksip:*21%23"), Ok(Link::Dial("*21#".into())));
        assert_eq!(parse("ksip:%EF%BC%99001"), Ok(Link::Dial("９001".into())));
        // A command in the wrong case is not one; it is shown as what it is.
        assert_eq!(parse("ksip:answer"), Ok(Link::Dial("answer".into())));
        assert_eq!(parse("ksip:"), Ok(Link::Dial(String::new())));
        assert_eq!(
            parse("ksip:sip:1001@pbx.example;transport=tcp"),
            Ok(Link::Dial("sip:1001@pbx.example;transport=tcp".into()))
        );
    }

    #[test]
    fn a_link_that_cannot_be_read_is_refused() {
        let malformed = |link: &str| crate::message::message_with("PROTOCOL_MALFORMED", [link]);
        assert_eq!(parse("9001"), Err(malformed("9001")));
        assert_eq!(parse("ksip:90%2"), Err(malformed("ksip:90%2")));
        assert_eq!(parse("ksip:90%G1"), Err(malformed("ksip:90%G1")));
        assert_eq!(parse("ksip:%FF"), Err(malformed("ksip:%FF")));
        assert_eq!(parse("ksip:9001%0A"), Err(malformed("ksip:9001%0A")));
    }

    #[test]
    fn a_link_arriving_while_another_is_read_is_still_delivered() {
        // A pipe of its own, so that the app's pipe and other tests are left alone.
        let name = format!(r"\\.\pipe\ksip-test-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        serve_named(&name, move |command| {
            let _ = tx.send(command);
        });
        // The server thread creates the pipe in its own time; a slow machine
        // is given up to five seconds rather than a fixed pause. WaitNamedPipe
        // only waits on a pipe that exists, so it is asked again until one does.
        use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { WaitNamedPipeW(wide.as_ptr(), 100) } == 0 {
            assert!(Instant::now() < deadline, "the pipe appeared");
            std::thread::sleep(Duration::from_millis(20));
        }
        // A client that connects and never writes must not block the next one.
        let silent = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        let workers: Vec<_> = (0..4)
            .map(|i| {
                let name = name.clone();
                std::thread::spawn(move || send_to(&name, &format!("LINK{i}")).unwrap())
            })
            .collect();
        for worker in workers {
            assert!(worker.join().unwrap(), "the app was listening");
        }
        let mut received: Vec<String> = (0..4)
            .map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap())
            .collect();
        received.sort();
        assert_eq!(received, ["LINK0", "LINK1", "LINK2", "LINK3"]);
        drop(silent);
    }
}
