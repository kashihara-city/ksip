//! User-scoped registry settings, the policy that fixes some of them, and
//! Windows generic credentials.
use crate::message::{message, message_with};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io, ptr, slice, sync::Arc};
use windows_sys::Win32::Security::Credentials::*;
use winreg::{enums::*, RegKey};

/// A value as the registry holds it: text (REG_SZ, or REG_EXPAND_SZ when
/// read) or a number (REG_DWORD). The app writes numbers and switches as
/// numbers and the rest as text; a value written by hand or by a policy in
/// the other type is still read (see Settings::read_stored).
#[derive(Clone, Debug, PartialEq)]
pub enum StoredValue {
    Text(String),
    Number(u32),
}

/// A value exactly as the registry holds it, whatever its type (one the
/// settings cannot read included): what a save backs up and a rollback puts
/// back, byte for byte.
pub struct RawValue(winreg::RegValue);
/// The account's values in the registry; the credential holds the rest.
pub const ACCOUNT_VALUES: [&str; 3] = ["server", "port", "extension"];

pub const DEFAULT_SERVER: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 5060;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub server: String,
    pub port: u16,
    pub extension: String,
    pub auth_user: String,
    pub password: String,
}
impl Default for Account {
    fn default() -> Self {
        Self {
            server: DEFAULT_SERVER.into(),
            port: DEFAULT_PORT,
            extension: String::new(),
            auth_user: String::new(),
            password: String::new(),
        }
    }
}
/// Whether a user part (extension or authentication user) is one SIP takes here.
fn user_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 100 && value.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
}
impl Account {
    /// The part of the account that lives in the registry and that a policy
    /// can set: the server, the port, and the extension when there is one.
    /// Checked without the credential, for the export as well as a connect.
    pub fn validate_address(&mut self) -> Result<(), String> {
        self.server = self.server.trim().to_string();
        self.extension = self.extension.trim().to_string();
        if self.server.is_empty()
            || self.server.len() > 253
            || !self
                .server
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
            || self.port == 0
        {
            return Err(message("ACCOUNT_SERVER_INVALID"));
        }
        if !self.extension.is_empty() && !user_ok(&self.extension) {
            return Err(message("ACCOUNT_EXTENSION_INVALID"));
        }
        Ok(())
    }
    pub fn validate(&mut self) -> Result<(), String> {
        self.validate_address()?;
        self.auth_user = self.auth_user.trim().to_string();
        // The extension may be left out: the account then registers under the
        // authentication user, which is the value the credential vault holds.
        if self.extension.is_empty() {
            self.extension = self.auth_user.clone();
        }
        if !user_ok(&self.extension) || !user_ok(&self.auth_user) {
            return Err(message("ACCOUNT_EXTENSION_INVALID"));
        }
        if self.password.is_empty()
            || self.password.len() > 512
            || self.password.chars().any(char::is_control)
        {
            return Err(message("ACCOUNT_PASSWORD_INVALID"));
        }
        Ok(())
    }
    pub fn public(&self) -> AccountView {
        AccountView {
            server: self.server.clone(),
            port: self.port,
            extension: self.extension.clone(),
            auth_user: self.auth_user.clone(),
            has_password: !self.password.is_empty(),
        }
    }
}
#[derive(Clone, Serialize)]
pub struct AccountView {
    pub server: String,
    pub port: u16,
    pub extension: String,
    pub auth_user: String,
    pub has_password: bool,
}
#[derive(Clone)]
pub struct Store {
    pub key: String,
    pub target: String,
    /// What the administrator fixes, read when the store is made: once a
    /// process, so that the dialog, a save and a connect all see the same.
    pub policy: Arc<Policy>,
}
/// The settings a policy can fix (the ADMX's, besides the custom buttons):
/// what the person may not change while one is set. The gains and the echo
/// delay are the person's alone, for they are adjusted where the phone is used.
pub const POLICY_SETTINGS: [&str; 33] = [
    "server",
    "port",
    "sip_port",
    "rtp_port",
    "transport",
    "ca_file",
    "media_encryption",
    "codecs",
    "dtmf_mode",
    "register_interval",
    "keepalive",
    "keepalive_interval",
    "pbx_only",
    "aec",
    "high_pass",
    "noise_suppression",
    "agc",
    "sound_ring",
    "sound_ringback",
    "sound_busy",
    "sound_notfound",
    "sound_error",
    "auto_answer",
    "auto_record",
    "incoming_action",
    "tray_after_call",
    "shortcut_window",
    "shortcut_call",
    "language",
    "program_integration",
    "browser_integration",
    "browser_dial_confirm",
    "detail_log",
];
/// The custom button a value belongs to (1 to COUNT), for the five values
/// of a button; None for any other name.
pub fn button_slot(name: &str) -> Option<usize> {
    let (n, field) = name.strip_prefix("button_")?.split_once('_')?;
    if n.starts_with('0') || !crate::settings::Settings::BUTTON_FIELDS.contains(&field) {
        return None;
    }
    n.parse().ok().filter(|n| (1..=crate::settings::CustomButton::COUNT).contains(n))
}
/// The values under the policy key (HKCU\Software\Policies\KashiharaCity\ksip,
/// where the ADMX writes): a value there fixes that setting, whatever it is
/// (0, false and empty text included); one that is not there leaves the
/// setting to the person. A custom button is one policy, so one value of a
/// button fixes the whole button, its values absent there being empty. The
/// values are never copied into the person's own key, so that the person's
/// return when the policy is lifted.
#[derive(Debug, Default)]
pub struct Policy {
    values: HashMap<String, StoredValue>,
    /// Values there of a type the settings cannot read: fixed, and wrong.
    unreadable: Vec<String>,
    /// The key is there but could not be read.
    error: Option<String>,
}
impl Policy {
    pub fn load(key: &str) -> Self {
        let mut policy = Self::default();
        let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(key) {
            Ok(key) => key,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return policy,
            Err(e) => {
                policy.error = Some(e.to_string());
                return policy;
            }
        };
        for item in key.enum_values() {
            let (name, raw) = match item {
                Ok(item) => item,
                Err(e) => {
                    policy.error = Some(e.to_string());
                    continue;
                }
            };
            if !POLICY_SETTINGS.contains(&name.as_str()) && button_slot(&name).is_none() {
                // Not a setting a policy fixes (the gains, a name of a later version).
                continue;
            }
            match raw.vtype {
                REG_SZ | REG_EXPAND_SZ => match key.get_value::<String, _>(&name) {
                    Ok(text) => {
                        policy.values.insert(name, StoredValue::Text(text));
                    }
                    Err(_) => policy.unreadable.push(name),
                },
                REG_DWORD => match key.get_value::<u32, _>(&name) {
                    Ok(number) => {
                        policy.values.insert(name, StoredValue::Number(number));
                    }
                    Err(_) => policy.unreadable.push(name),
                },
                _ => policy.unreadable.push(name),
            }
        }
        policy
    }
    fn names(&self) -> impl Iterator<Item = &String> {
        self.values.keys().chain(&self.unreadable)
    }
    /// Whether the policy fixes this setting.
    pub fn fixes(&self, name: &str) -> bool {
        if POLICY_SETTINGS.contains(&name) {
            return self.names().any(|n| n == name);
        }
        button_slot(name).is_some_and(|slot| self.names().any(|n| button_slot(n) == Some(slot)))
    }
    /// The setting's value when the policy fixes it (None: the person's), in
    /// the form Store::read_value gives.
    fn read(&self, name: &str) -> Option<Result<Option<StoredValue>, String>> {
        if !self.fixes(name) {
            return None;
        }
        if self.unreadable.iter().any(|n| n == name) {
            return Some(Err(message_with("SETTINGS_VALUE_INVALID", [name])));
        }
        Some(Ok(self.values.get(name).cloned()))
    }
    /// Every setting the policy fixes, a button's five values each.
    pub fn fixed(&self) -> Vec<String> {
        let mut names: Vec<String> = POLICY_SETTINGS.iter().filter(|n| self.fixes(n)).map(|n| n.to_string()).collect();
        for slot in 1..=crate::settings::CustomButton::COUNT {
            if self.fixes(&format!("button_{slot}_kind")) {
                names.extend(crate::settings::Settings::BUTTON_FIELDS.iter().map(|f| format!("button_{slot}_{f}")));
            }
        }
        names
    }
    /// Why the policy key could not be read, when it could not.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}
/// Where the policy for a store's key is. A test profile has one of its own
/// beside it, which the person can write (Software\Policies cannot be written
/// without an administrator), so that a test never touches the real one.
fn policy_key(key: &str) -> String {
    match key.split_once(r"\Test\") {
        Some((base, profile)) => format!(r"{base}\TestPolicies\{profile}"),
        None => key.replacen(r"Software\", r"Software\Policies\", 1),
    }
}
#[cfg(test)]
thread_local! {
    /// A registry value name whose write is made to fail, for the tests of
    /// what a save does when it cannot finish. Empty: nothing fails.
    static FAIL_WRITE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}
/// Makes the next writes of this registry value fail on this thread; an
/// empty name lifts it. Test builds only.
#[cfg(test)]
pub fn fail_next_write_of(name: &str) {
    FAIL_WRITE.with(|fail| *fail.borrow_mut() = (!name.is_empty()).then(|| name.to_string()));
}
/// A null-terminated UTF-16 string, as text; empty for a null pointer.
///
/// # Safety
/// `value` must be null or point to a null-terminated UTF-16 string, aligned
/// for u16, that stays valid and unchanged for the call: what Windows hands
/// out as a PWSTR (a credential's user name, an adapter's name, a SID's text).
pub unsafe fn read_wide(value: *const u16) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut length = 0;
    // SAFETY: every unit up to and including the null is part of the string
    // (the caller's promise), and the loop stops at the null.
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: the `length` units before the null were just read one by one.
    String::from_utf16_lossy(unsafe { slice::from_raw_parts(value, length) })
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
impl Store {
    pub fn delete_account(&self) -> Result<(), String> {
        // SAFETY: the target is null-terminated and lives to the end of the
        // statement, past the call.
        if unsafe { CredDeleteW(wide(&self.target).as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(1168) {
                return Err(e.to_string());
            }
        }
        Ok(())
    }
    pub fn new() -> Self {
        if let Ok(profile) = std::env::var("KSIP_TEST_PROFILE") {
            if profile.starts_with("test-")
                && profile.len() < 80
                && profile
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                return Self::at(format!(r"Software\KashiharaCity\ksip\Test\{profile}"), format!("KSIP/Test/{profile}"));
            }
        }
        Self::at(r"Software\KashiharaCity\ksip".into(), "KSIP/SIP/default".into())
    }
    /// The store of that key and credential, with the policy that goes with
    /// the key, read now.
    pub fn at(key: String, target: String) -> Self {
        let policy = Arc::new(Policy::load(&policy_key(&key)));
        Self { key, target, policy }
    }
    /// A setting as KSIP uses it: the policy's value when a policy fixes it,
    /// the person's otherwise. read_value is the person's alone, which is
    /// what a save backs up and writes.
    pub fn read_effective(&self, name: &str) -> Result<Option<StoredValue>, String> {
        match self.policy.read(name) {
            Some(value) => value,
            None => self.read_value(name),
        }
    }
    /// Whether a policy fixes this setting.
    pub fn managed(&self, name: &str) -> bool {
        self.policy.fixes(name)
    }
    /// One value as it is stored, or None when there is no such value: what
    /// the settings are read from, and what a rollback has to put back,
    /// absence included. A value of a type the app never writes (binary,
    /// multi-string, QWORD) is an error naming it.
    pub fn read_value(&self, name: &str) -> Result<Option<StoredValue>, String> {
        let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(&self.key) {
            Ok(key) => key,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let raw = match key.get_raw_value(name) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        match raw.vtype {
            REG_SZ | REG_EXPAND_SZ => key.get_value::<String, _>(name).map(|t| Some(StoredValue::Text(t))).map_err(|e| e.to_string()),
            REG_DWORD => key.get_value::<u32, _>(name).map(|n| Some(StoredValue::Number(n))).map_err(|e| e.to_string()),
            _ => Err(message_with("SETTINGS_VALUE_INVALID", [name])),
        }
    }
    /// One value as the registry holds it, or None when there is no such
    /// value. Never refuses a type: a save has to be able to back up, and
    /// then replace, a value the settings cannot read.
    pub fn read_raw(&self, name: &str) -> Result<Option<RawValue>, String> {
        let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(&self.key) {
            Ok(key) => key,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        match key.get_raw_value(name) {
            Ok(raw) => Ok(Some(RawValue(raw))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    /// Puts a value back exactly as it was, type and bytes, and reads it back.
    pub fn write_raw(&self, name: &str, value: &RawValue) -> Result<(), String> {
        #[cfg(test)]
        if FAIL_WRITE.with(|fail| fail.borrow().as_deref() == Some(name)) {
            return Err(message_with("STORAGE_VERIFY_FAILED", [name]));
        }
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&self.key)
            .map_err(|e| e.to_string())?;
        key.set_raw_value(name, &value.0).map_err(|e| e.to_string())?;
        if key.get_raw_value(name).map_err(|e| e.to_string())? != value.0 {
            return Err(message_with("STORAGE_VERIFY_FAILED", [name]));
        }
        Ok(())
    }
    /// Writes one value in its type and reads it back.
    pub fn write_value(&self, name: &str, value: &StoredValue) -> Result<(), String> {
        #[cfg(test)]
        if FAIL_WRITE.with(|fail| fail.borrow().as_deref() == Some(name)) {
            return Err(message_with("STORAGE_VERIFY_FAILED", [name]));
        }
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&self.key)
            .map_err(|e| e.to_string())?;
        match value {
            StoredValue::Text(text) => key.set_value(name, text),
            StoredValue::Number(number) => key.set_value(name, number),
        }
        .map_err(|e| e.to_string())?;
        if self.read_value(name)?.as_ref() != Some(value) {
            return Err(message_with("STORAGE_VERIFY_FAILED", [name]));
        }
        Ok(())
    }
    /// Removes one value; one that is not there is no error.
    pub fn delete_value(&self, name: &str) -> Result<(), String> {
        let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(&self.key, KEY_SET_VALUE) {
            Ok(key) => key,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        match key.delete_value(name) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
    /// One value as text, trimmed, whichever type it is stored in (a number
    /// reads as its decimal digits); empty when absent or unreadable.
    pub fn read_text(&self, name: &str) -> String {
        match self.read_value(name) {
            Ok(Some(StoredValue::Text(text))) => text.trim().to_string(),
            Ok(Some(StoredValue::Number(number))) => number.to_string(),
            _ => String::new(),
        }
    }
    pub fn write_text(&self, name: &str, value: &str) -> Result<(), String> {
        self.write_value(name, &StoredValue::Text(value.to_string()))
    }
    /// The address comes from the registry and only the sign-in secret from the
    /// vault. Without a stored secret the account counts as unconfigured. A
    /// registry value that cannot be read as what it is for (a type the app
    /// never writes, a port that is not a port) is an error naming it, not a
    /// quiet fall back to the defaults: the phone would register elsewhere.
    pub fn read_account(&self) -> Result<Option<Account>, String> {
        let Some((auth_user, password)) = self.read_secret()? else {
            return Ok(None);
        };
        let (server, port, extension) = self.read_address()?;
        Ok(Some(Account { server, port, extension, auth_user, password }))
    }
    /// The account's registry values, strictly: see account_values.
    pub fn read_address(&self) -> Result<(String, u16, String), String> {
        let (address, unreadable) = self.account_values();
        if unreadable.is_empty() {
            Ok(address)
        } else {
            Err(message_with("SETTINGS_VALUE_INVALID", [unreadable.join(", ")]))
        }
    }
    /// The account's registry values as a connect reads them (server, port,
    /// extension; absent or empty is the default; the server and the port a
    /// policy's when it fixes them), and the names of those
    /// that cannot be read as what they are for, which keep their defaults.
    pub fn account_values(&self) -> ((String, u16, String), Vec<String>) {
        let mut unreadable = Vec::new();
        let mut text = |name: &str, default: &str| match self.read_effective(name) {
            Ok(Some(StoredValue::Text(t))) if !t.trim().is_empty() => t.trim().to_string(),
            Ok(Some(StoredValue::Number(n))) if name == "extension" => n.to_string(),
            Ok(None) | Ok(Some(StoredValue::Text(_))) => default.to_string(),
            _ => {
                unreadable.push(name.to_string());
                default.to_string()
            }
        };
        let server = text("server", DEFAULT_SERVER);
        let extension = text("extension", "");
        let port = match self.read_effective("port") {
            Ok(None) => DEFAULT_PORT,
            Ok(Some(StoredValue::Text(t))) if t.trim().is_empty() => DEFAULT_PORT,
            Ok(Some(StoredValue::Number(n))) if u16::try_from(n).is_ok() => n as u16,
            Ok(Some(StoredValue::Text(t))) if t.trim().parse::<u16>().is_ok() => t.trim().parse().unwrap_or(DEFAULT_PORT),
            _ => {
                unreadable.push("port".to_string());
                DEFAULT_PORT
            }
        };
        ((server, port, extension), unreadable)
    }
    /// The credential alone: authentication user and password, or None.
    pub fn read_secret(&self) -> Result<Option<(String, String)>, String> {
        let target = wide(&self.target);
        let mut item = ptr::null_mut();
        // SAFETY: the target is null-terminated and outlives the call. The
        // credential CredReadW allocates is read only while it is alive, freed
        // once with CredFree on every path that has one, and not used after:
        // its blob only for CredentialBlobSize bytes (checked to be at most
        // 1024, and zeroed before the free), its user name as a
        // null-terminated string or null.
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut item) == 0 {
                let e = io::Error::last_os_error();
                return if e.raw_os_error() == Some(1168) {
                    Ok(None)
                } else {
                    Err(message_with("CREDENTIAL_READ_FAILED", [e]))
                };
            }
            if item.is_null() {
                return Err(message("CREDENTIAL_INVALID"));
            }
            let size = (*item).CredentialBlobSize as usize;
            let result = if size == 0 || size > 1024 || (*item).CredentialBlob.is_null() {
                Err(message("CREDENTIAL_PASSWORD_INVALID"))
            } else {
                match String::from_utf8(
                    slice::from_raw_parts((*item).CredentialBlob, size).to_vec(),
                ) {
                    Ok(password) => Ok(Some((read_wide((*item).UserName), password))),
                    Err(_) => Err(message("CREDENTIAL_PASSWORD_UNREADABLE")),
                }
            };
            if size <= 1024 && !(*item).CredentialBlob.is_null() {
                for i in 0..size {
                    ptr::write_volatile((*item).CredentialBlob.add(i), 0);
                }
            }
            CredFree(item.cast());
            result
        }
    }
    pub fn write_account(&self, account: &Account) -> Result<(), String> {
        self.write_secret(&account.auth_user, &account.password)?;
        // What a policy fixes is the policy's; the person's own stays as it was.
        if !self.managed("server") {
            self.write_text("server", &account.server)?;
        }
        if !self.managed("port") {
            self.write_value("port", &StoredValue::Number(account.port.into()))?;
        }
        self.write_text("extension", &account.extension)?;
        let actual = self
            .read_account()?
            .ok_or(message("CREDENTIAL_VERIFY_FAILED"))?;
        if actual.auth_user != account.auth_user || actual.password != account.password {
            return Err(message("CREDENTIAL_VERIFY_FAILED"));
        }
        Ok(())
    }
    /// The credential alone (authentication user and password), without the
    /// account's registry values: what a rollback puts back on its own.
    pub fn write_secret(&self, auth_user: &str, password: &str) -> Result<(), String> {
        let mut blob = password.to_string().into_bytes();
        if blob.len() > 1024 {
            return Err(message("CREDENTIAL_PASSWORD_TOO_LONG"));
        }
        let mut target = wide(&self.target);
        let mut username = wide(auth_user);
        // SAFETY: CREDENTIALW is plain data, and all zero is a valid value of
        // every field: null pointers, zero sizes and counts.
        let mut item: CREDENTIALW = unsafe { std::mem::zeroed() };
        item.Type = CRED_TYPE_GENERIC;
        item.TargetName = target.as_mut_ptr();
        item.UserName = username.as_mut_ptr();
        item.CredentialBlob = blob.as_mut_ptr();
        item.CredentialBlobSize = blob.len() as u32;
        item.Persist = CRED_PERSIST_LOCAL_MACHINE;
        // SAFETY: every pointer in `item` points into a buffer that outlives
        // the call: the target and user name null-terminated, the blob of the
        // size given. CredWriteW copies what it keeps.
        let ok = unsafe { CredWriteW(&item, 0) };
        let error = io::Error::last_os_error();
        for b in &mut blob {
            // SAFETY: `b` is a valid, exclusive reference to a byte of the blob;
            // the volatile write keeps the zeroing from being optimised away.
            unsafe {
                ptr::write_volatile(b, 0);
            }
        }
        if ok == 0 {
            return Err(message_with("CREDENTIAL_WRITE_FAILED", [error]));
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn cleanup_test(&self) {
        assert!(self.target.starts_with("KSIP/Test/"));
        // SAFETY: the target is null-terminated and lives to the end of the
        // statement, past the call.
        unsafe {
            CredDeleteW(wide(&self.target).as_ptr(), CRED_TYPE_GENERIC, 0);
        }
        let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.key);
        let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(policy_key(&self.key));
    }
    /// Sets the test profile's policy value (Text or Number), for a test that
    /// starts KSIP again under it with Store::at. A test profile's alone.
    #[cfg(test)]
    pub fn set_test_policy(&self, name: &str, value: Option<&StoredValue>) {
        assert!(self.target.starts_with("KSIP/Test/"));
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(policy_key(&self.key)).unwrap();
        match value {
            Some(StoredValue::Text(text)) => key.set_value(name, text).unwrap(),
            Some(StoredValue::Number(number)) => key.set_value(name, number).unwrap(),
            None => {
                let _ = key.delete_value(name);
            }
        }
    }
    /// The same store with its policy read again, as a new start of KSIP reads it.
    #[cfg(test)]
    pub fn restarted(&self) -> Self {
        Self::at(self.key.clone(), self.target.clone())
    }
}
/// The tests that write the store run one at a time, in this module and in
/// settings alike: the credential vault has been seen to answer a read of one
/// target with "not found" while another target was being written or deleted
/// from a second thread.
#[cfg(test)]
pub fn store_tests_one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_and_credential_roundtrip_are_separate() {
        let _one_at_a_time = store_tests_one_at_a_time();
        let suffix = format!("test-storage-{}", std::process::id());
        let store = Store::at(format!(r"Software\KashiharaCity\ksip\Test\{suffix}"), format!("KSIP/Test/{suffix}"));
        let account = Account {
            server: "192.0.2.10".into(),
            port: 5070,
            extension: "101".into(),
            auth_user: "auth101".into(),
            password: "local-test-only-秘密".into(),
        };
        let result = (|| -> Result<(), String> {
            store.write_account(&account)?;
            // A value keeps its type, and text is read whichever type it is in.
            store.write_value("aec", &StoredValue::Number(1))?;
            assert_eq!(store.read_value("aec")?, Some(StoredValue::Number(1)));
            assert_eq!(store.read_value("absent")?, None);
            assert_eq!(store.read_text("port"), "5070", "the port is a number and reads as its digits");
            store.delete_value("aec")?;
            assert_eq!(store.read_value("aec")?, None);
            store.delete_value("aec")?;
            let stored = store.read_account()?.unwrap();
            assert!(stored.password == account.password);
            // The address and the buttons are single values a policy can set.
            assert_eq!(store.read_text("server"), "192.0.2.10");
            assert_eq!(store.read_text("port"), "5070");
            assert_eq!(store.read_text("extension"), "101");
            assert_eq!(stored.server, account.server);
            assert_eq!(stored.port, account.port);
            store.write_text("button_1_number", "701")?;
            assert_eq!(store.read_text("button_1_number"), "701");
            assert!(!serde_json::to_string(&account.public())
                .unwrap()
                .contains(&account.password));
            Ok(())
        })();
        store.cleanup_test();
        result.unwrap();
    }
}
