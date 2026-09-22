//! The browser integration: a `ksip:` link reaches the running app.
/// The browser hands a `ksip:` link to a new process. That process passes the
/// command to the one already running through this pipe, then exits.
pub fn pipe_name() -> String {
    format!(
        r"\\.\pipe\{}",
        crate::storage::Store::new().target.replace('/', "_")
    )
}

/// The command carried by a link, in the form the other side understands.
/// `/ksip=123456789`, `ksip:123456789` and `123456789` all mean the same.
pub fn command_from(argument: &str) -> String {
    let value = argument
        .trim()
        .trim_start_matches("/ksip=")
        .trim_start_matches("ksip:")
        .trim_start_matches("//")
        .trim_end_matches('/');
    // A number written for people can carry separators; the dialler wants none.
    value
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | '(' | ')' | '.'))
        .collect()
}

/// Sends a command to the running app. Ok(false) means nothing was listening.
pub fn send(command: &str) -> Result<bool, String> {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    let mut pipe = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OVERLAPPED & 0)
        .open(pipe_name())
    {
        Ok(pipe) => pipe,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    pipe.write_all(command.as_bytes()).map_err(|e| e.to_string())?;
    pipe.flush().map_err(|e| e.to_string())?;
    Ok(true)
}

/// Serves the pipe for as long as the app runs, handing each command to `deliver`.
pub fn serve(deliver: impl Fn(String) + Send + 'static) {
    use windows_sys::Win32::Storage::FileSystem::{ReadFile, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::Foundation::CloseHandle;
    let wide: Vec<u16> = pipe_name().encode_utf16().chain(Some(0)).collect();
    std::thread::spawn(move || loop {
        let pipe = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                4,
                1024,
                1024,
                0,
                std::ptr::null(),
            )
        };
        if pipe.is_null() || pipe as isize == -1 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0;
        if connected {
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
            if ok && read > 0 {
                deliver(String::from_utf8_lossy(&buffer[..read as usize]).into_owned());
            }
        }
        unsafe {
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
        }
    });
}

/// Registers or removes the `ksip:` protocol for this user. The command points
/// at the running executable, so a folder that has been moved fixes itself.
pub fn register(enabled: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS};
    use winreg::RegKey;
    let classes = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Classes", KEY_ALL_ACCESS)
        .map_err(|e| e.to_string())?;
    if !enabled {
        return match classes.delete_subkey_all("ksip") {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().to_string();
    let (protocol, _) = classes.create_subkey("ksip").map_err(|e| e.to_string())?;
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
        .set_value("", &format!("\"{exe}\" \"/ksip=%1\""))
        .map_err(|e| e.to_string())
}
