//! The browser integration: a `ksip:` link reaches the running app.
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE,
};

/// The browser hands a `ksip:` link to a new process. That process passes the
/// command to the one already running through this pipe, then exits. The
/// pipe is this user's, in this logon session: its name carries both, so
/// that a link goes to the app running where the browser runs and not to
/// another session's, and its access list (see `security`) lets nobody but
/// this user open or create it; remote clients are refused outright.
pub fn pipe_name() -> String {
    format!(
        r"\\.\pipe\{}-{}",
        crate::storage::Store::new().target.replace('/', "_"),
        scope()
    )
}
/// The user (by SID) and the logon session this process runs in.
fn scope() -> String {
    format!("{}-{}", user_sid().unwrap_or_else(|| "S-unknown".into()), session_id())
}
fn session_id() -> u32 {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    let mut session = 0u32;
    // SAFETY: the process id is this process's; the out pointer is valid.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    session
}
/// The SID of the user this process runs as, as text.
fn user_sid() -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: the token is this process's and closed once, before anything
    // else can return. The buffer is sized by the first call and made of u64,
    // so that the TOKEN_USER the second call writes at its start (it holds a
    // pointer) is aligned, which a Vec<u8> is not promised to be; the SID it
    // points to lies in the same buffer, alive until the text is made. The
    // text ConvertSidToStringSidW allocates is null-terminated, read once,
    // and freed with LocalFree as it asks.
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut needed = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), needed, &mut needed) != 0;
        CloseHandle(token);
        if !ok || (needed as usize) < std::mem::size_of::<TOKEN_USER>() {
            return None;
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return None;
        }
        let sid = crate::storage::read_wide(text);
        LocalFree(text.cast());
        Some(sid)
    }
}
/// A security descriptor that gives this user, and nobody else, full access
/// (SDDL: a protected DACL with one allow entry), or None when the user
/// cannot be told, in which case the system's default applies.
fn security() -> Option<*mut core::ffi::c_void> {
    use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    let sid = user_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})").encode_utf16().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: the SDDL text is null-terminated; the descriptor comes from
    // LocalAlloc and is freed by the caller with LocalFree.
    let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, std::ptr::null_mut()) } != 0;
    (ok && !descriptor.is_null()).then_some(descriptor)
}

const SCHEME: &str = "ksip:";

/// Whether an argument is a link. Windows hands the link over whole, scheme
/// and all, and a scheme is the same in any case.
pub fn is_link(argument: &str) -> bool {
    // By bytes: an argument whose fifth byte falls inside a character (any
    // non-ASCII text) is simply not a link.
    argument.as_bytes().get(..SCHEME.len()).is_some_and(|head| head.eq_ignore_ascii_case(SCHEME.as_bytes()))
}

/// Whether a link operates the phone (dial, answer, hang up, or something not
/// understood), which only program integration allows; showing the window
/// and quitting do not, and are always taken.
pub fn operates_phone(link: &str) -> bool {
    !matches!(parse(link), Ok(Link::ShowWindow | Link::Quit))
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

/// One instance of the pipe. The first instance claims the name: if another
/// process holds it already, the claim fails and is tried again later rather
/// than joining someone else's pipe. Every instance is this user's alone and
/// refuses remote clients.
fn create(name: &[u16], first: bool) -> Option<HANDLE> {
    // The name goes to Windows as a C string: it has to end with its null.
    if name.last() != Some(&0) {
        return None;
    }
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    let descriptor = security();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.unwrap_or(std::ptr::null_mut()),
        bInheritHandle: 0,
    };
    // SAFETY: the name ends with its null (checked above) and the attributes,
    // with the descriptor they point to, outlive the call; the pipe copies the
    // descriptor, which is freed after it.
    let pipe = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 },
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            1024,
            1024,
            0,
            if descriptor.is_some() { &attributes } else { std::ptr::null() },
        )
    };
    if let Some(descriptor) = descriptor {
        // SAFETY: the descriptor came from ConvertStringSecurityDescriptorToSecurityDescriptorW,
        // which allocates it with LocalAlloc, and is freed once, here.
        unsafe { LocalFree(descriptor) };
    }
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
        // SAFETY: the pipe is an open handle the caller holds for the call; no
        // data buffer is passed, and the only out pointer is a local.
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
            // SAFETY: the pipe is open; the buffer is a local of the length
            // given, and the call is synchronous (no OVERLAPPED), so nothing
            // writes to it after the call returns.
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
        let mut pipe = create(&wide, true);
        loop {
            let Some(current) = pipe else {
                std::thread::sleep(Duration::from_secs(1));
                pipe = create(&wide, true);
                continue;
            };
            // A client that connected before this call is reported as an error
            // with a name of its own, and is connected all the same. One that
            // has already written and left is reported as "no data", and what
            // it wrote is still there to be read.
            // SAFETY: `current` is an open pipe instance this thread created and
            // alone holds; the call is synchronous (no OVERLAPPED). GetLastError
            // reads what that call left on this thread.
            let connected = unsafe { ConnectNamedPipe(current, std::ptr::null_mut()) } != 0
                || matches!(unsafe { GetLastError() }, ERROR_PIPE_CONNECTED | ERROR_NO_DATA);
            // The next instance is ready before this one is read, so a link that
            // arrives meanwhile finds a pipe instead of nothing.
            let next = create(&wide, false);
            if connected {
                if let Some(command) = read_command(current) {
                    deliver(command);
                }
            }
            // Closing the handle ends this instance; a disconnect first would
            // leave an instant in which a new client could attach to it and be
            // lost with it.
            // SAFETY: `current` is open and closed once, here; it is not used after.
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
        // Text that is not ASCII is no link, wherever its characters end.
        for text in ["日本語", "ksi", "日ksip:9001", "😀", "kあsip:", "ksip"] {
            assert!(!is_link(text), "{text}");
        }
        assert!(parse("日本語").is_err());
    }

    #[test]
    fn only_showing_the_window_and_quitting_are_not_operating_the_phone() {
        // The commands are matched as written, so a command in other letters
        // is something to dial, and operates the phone.
        for link in ["ksip:1001", "ksip:ANSWER", "ksip:HANGUP", "ksip:sip:1001@pbx.example", "ksip:", "ksip:%zz", "ksip:showwindow"] {
            assert!(operates_phone(link), "{link}");
        }
        for link in ["ksip:SHOWWINDOW", "KSIP:SHOWWINDOW", "ksip:APP_QUIT"] {
            assert!(!operates_phone(link), "{link}");
        }
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
        // SAFETY: the name is null-terminated and outlives the call.
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
