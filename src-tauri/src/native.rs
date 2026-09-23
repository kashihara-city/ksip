use crate::message::{message, message_with};
use std::{
    ffi::{c_char, CString},
    path::PathBuf,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS},
    System::Threading::{
        CreateMutexW, OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE,
    },
};
unsafe extern "C" {
    fn ksip_engine_main(argc: i32, argv: *mut *mut c_char) -> i32;
    fn ksip_stdio_init();

}
include!(concat!(env!("OUT_DIR"), "/sounds.rs"));
/// Writes the built-in sounds to TEMP and returns that folder. A failure here only
/// means the call tones stay silent, so the caller treats None as "no sounds".
pub fn sounds() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("ksip-sound");
    std::fs::create_dir_all(&dir).ok()?;
    for (name, bytes) in SOUNDS {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(*bytes) {
            std::fs::write(&path, bytes).ok()?;
        }
    }
    Some(dir)
}
/// Shows the standard open dialog and returns the chosen path, or None if the
/// person cancelled. Used for a call sound and for the certificate authority.
pub fn choose_file(kind: &str) -> Option<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW};
    let text = crate::message::windows_text;
    let (pattern, caption) = if kind == "certificate" {
        (text("DIALOG_CA_FILTER"), text("DIALOG_CA_FILE"))
    } else {
        (text("DIALOG_SOUND_FILTER"), text("DIALOG_SOUND_FILE"))
    };
    let filter: Vec<u16> = format!("{pattern}\0").encode_utf16().collect();
    let title: Vec<u16> = format!("{caption}\0").encode_utf16().collect();
    let mut file = vec![0u16; 1024];
    let mut options = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { GetOpenFileNameW(&mut options) } == 0 {
        return None;
    }
    let end = file.iter().position(|c| *c == 0).unwrap_or(file.len());
    Some(String::from_utf16_lossy(&file[..end]))
}
pub fn engine_exe() -> Result<PathBuf, String> {
    #[cfg(test)]
    {
        std::env::var_os("KSIP_ENGINE_EXE")
            .map(PathBuf::from)
            .ok_or("Set KSIP_ENGINE_EXE to the built KSIP executable".into())
    }
    #[cfg(not(test))]
    std::env::current_exe().map_err(|e| e.to_string())
}
// This runs before Tauri or the GUI singleton is initialized.
pub fn run_engine(args: &[String]) -> i32 {
    if args.len() != 3 || args[1] != "-f" {
        return 2;
    }
    let parent: u32 = match args[0].parse() {
        Ok(p) if p != 0 && p != std::process::id() => p,
        _ => return 2,
    };
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent) };
    if handle.is_null() {
        return 3;
    }
    // A retained process handle observes this exact parent even if its PID is reused.
    let raw = handle as usize;
    std::thread::spawn(move || {
        let handle = raw as _;
        unsafe {
            WaitForSingleObject(handle, INFINITE);
            CloseHandle(handle);
        }
        std::process::exit(3);
    });
    let target = match std::env::var("KSIP_CREDENTIAL_TARGET") {
        Ok(t) => t,
        Err(_) => return 2,
    };
    let name: Vec<u16> = format!("Local\\{}_engine", target.replace('/', "_"))
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let singleton = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if singleton.is_null() {
        return 3;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(singleton);
        }
        return 4;
    }
    let strings: Result<Vec<_>, _> = ["KSIP", "-f", &args[2]]
        .iter()
        .map(|s| CString::new(*s))
        .collect();
    let strings = match strings {
        Ok(s) => s,
        Err(_) => return 2,
    };
    let mut argv: Vec<_> = strings.iter().map(|s| s.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    let result = unsafe {
        ksip_stdio_init();
        ksip_engine_main(3, argv.as_mut_ptr())
    };
    unsafe {
        CloseHandle(singleton);
    }
    result
}
/// Shows a file in Explorer, selected, in the window already showing its
/// folder if there is one: what Explorer's own "open file location" does.
pub fn show_in_folder(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: the path is null-terminated, the item list is freed here, and
    // COM is left as it was found (an already initialised thread says so).
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let item = ILCreateFromPathW(wide.as_ptr());
        let result = if item.is_null() {
            Err(message("RECORDING_NOT_FOUND"))
        } else {
            let shown = SHOpenFolderAndSelectItems(item, 0, std::ptr::null(), 0);
            ILFree(item);
            if shown < 0 {
                Err(message_with("RECORDING_OPEN_FAILED", [shown]))
            } else {
                Ok(())
            }
        };
        if com.is_ok() {
            CoUninitialize();
        }
        result
    }
}

/// Hands a web address to whatever the person reads the web with.
pub fn open_url(url: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the strings are null-terminated; a value above 32 means the
    // address was handed over (SW_SHOWNORMAL is 1).
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as usize > 32 {
        Ok(())
    } else {
        Err(message_with("LINK_OPEN_FAILED", [result as usize]))
    }
}
/// One network adapter as Windows reports it. The name is the adapter's own
/// identifier, which survives a rename or a new address; the rest is for the
/// person choosing one.
#[derive(Clone, serde::Serialize)]
pub struct Adapter {
    pub name: String,
    pub label: String,
    pub address: String,
    pub up: bool,
}

/// Every adapter except the loopback, whether it is connected or not: a cable
/// can be unplugged while the setting is being made.
pub fn adapters() -> Vec<Adapter> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH, IF_TYPE_SOFTWARE_LOOPBACK,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC, SOCKADDR_IN};
    const OPER_STATUS_UP: i32 = 1;
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 32 * 1024;
    let mut buffer = vec![0u8; size as usize];
    let mut found = Vec::new();
    unsafe {
        for _ in 0..3 {
            let result = GetAdaptersAddresses(
                AF_UNSPEC as u32,
                flags,
                std::ptr::null(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            );
            if result == 111 {
                // ERROR_BUFFER_OVERFLOW: the size we were given is the one to use.
                buffer = vec![0u8; size as usize];
                continue;
            }
            if result != 0 {
                return found;
            }
            let mut current = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
            while !current.is_null() {
                let adapter = &*current;
                current = adapter.Next;
                if adapter.IfType == IF_TYPE_SOFTWARE_LOOPBACK {
                    continue;
                }
                let name = read_ansi(adapter.AdapterName);
                if name.is_empty() {
                    continue;
                }
                found.push(Adapter {
                    name,
                    label: crate::storage::read_wide(adapter.FriendlyName),
                    address: {
                        // The first ordinary IPv4 the adapter holds. Windows lists
                        // them best first, and an automatic 169.254 address means
                        // the adapter never got one.
                        let mut unicast = adapter.FirstUnicastAddress;
                        let mut address = String::new();
                        while !unicast.is_null() {
                            let socket = (*unicast).Address.lpSockaddr;
                            if !socket.is_null() && (*socket).sa_family == AF_INET {
                                let inet = &*(socket as *const SOCKADDR_IN);
                                let octets = inet.sin_addr.S_un.S_addr.to_ne_bytes();
                                if octets[0..2] != [169, 254] {
                                    address = format!(
                                        "{}.{}.{}.{}",
                                        octets[0], octets[1], octets[2], octets[3]
                                    );
                                    break;
                                }
                            }
                            unicast = (*unicast).Next;
                        }
                        address
                    },
                    up: adapter.OperStatus == OPER_STATUS_UP,
                });
            }
            break;
        }
    }
    found
}

/// The address to bind to for a saved adapter, or why it cannot be used.
pub fn adapter_address(name: &str) -> Result<String, String> {
    let adapter = adapters()
        .into_iter()
        .find(|a| a.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| message("ADAPTER_NOT_FOUND"))?;
    if adapter.address.is_empty() {
        return Err(message("ADAPTER_NO_ADDRESS"));
    }
    Ok(adapter.address)
}

fn read_ansi(value: *const u8) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut length = 0;
    // SAFETY: the adapter name is a null-terminated string from Windows.
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(value, length) }).into_owned()
}

/// Puts text on the Windows clipboard. The web view's own clipboard API needs
/// a permission prompt this app should not raise.
pub fn copy_text(value: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{GlobalFree, HWND};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    unsafe {
        // The clipboard is shared, and another program can hold it for a moment;
        // a few short retries are what other Windows programs do as well.
        let mut attempts = 0;
        while OpenClipboard(std::ptr::null_mut::<core::ffi::c_void>() as HWND) == 0 {
            attempts += 1;
            if attempts >= 10 {
                return Err(message("CLIPBOARD_UNAVAILABLE"));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let result = (|| -> Result<(), String> {
            if EmptyClipboard() == 0 {
                return Err(message("CLIPBOARD_CLEAR_FAILED"));
            }
            let bytes = std::mem::size_of_val(&wide[..]);
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle.is_null() {
                return Err(message("CLIPBOARD_MEMORY_FAILED"));
            }
            let target = GlobalLock(handle);
            if target.is_null() {
                GlobalFree(handle);
                return Err(message("CLIPBOARD_MEMORY_FAILED"));
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), target.cast::<u8>(), bytes);
            GlobalUnlock(handle);
            if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                GlobalFree(handle);
                return Err(message("CLIPBOARD_WRITE_FAILED"));
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    #[test]
    fn adapters_are_listed_without_the_loopback() {
        let found = adapters();
        assert!(!found.is_empty(), "this machine has at least one adapter");
        for adapter in &found {
            assert!(adapter.name.starts_with('{'), "the name is the adapter's own id");
            assert!(!adapter.address.starts_with("127."), "the loopback is left out");
            assert!(!adapter.address.starts_with("169.254."), "an automatic address is not usable");
        }
        // A listed adapter with an address resolves; an invented one does not.
        if let Some(usable) = found.iter().find(|a| !a.address.is_empty()) {
            assert_eq!(adapter_address(&usable.name).unwrap(), usable.address);
        }
        let missing = adapter_address("{00000000-0000-0000-0000-000000000000}");
        assert_eq!(missing.unwrap_err(), message("ADAPTER_NOT_FOUND"));
    }
}
