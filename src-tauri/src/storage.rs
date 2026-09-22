//! User-scoped registry settings and Windows generic credentials.
use crate::message::{message, message_with};
use serde::{Deserialize, Serialize};
use std::{io, ptr, slice};
use windows_sys::Win32::Security::Credentials::*;
use winreg::{enums::*, RegKey};

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
impl Account {
    pub fn validate(&mut self) -> Result<(), String> {
        self.server = self.server.trim().to_string();
        self.extension = self.extension.trim().to_string();
        self.auth_user = self.auth_user.trim().to_string();
        // The extension may be left out: the account then registers under the
        // authentication user, which is the value the credential vault holds.
        if self.extension.is_empty() {
            self.extension = self.auth_user.clone();
        }
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
        for value in [&self.extension, &self.auth_user] {
            if value.is_empty()
                || value.len() > 100
                || !value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
            {
                return Err(message("ACCOUNT_EXTENSION_INVALID"));
            }
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
}
pub fn read_wide(value: *const u16) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut length = 0;
    // SAFETY: the credential API returns a null-terminated string.
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { slice::from_raw_parts(value, length) })
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
impl Store {
    pub fn delete_account(&self) -> Result<(), String> {
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
                return Self {
                    key: format!(r"Software\KashiharaCity\ksip\Test\{profile}"),
                    target: format!("KSIP/Test/{profile}"),
                };
            }
        }
        Self {
            key: r"Software\KashiharaCity\ksip".into(),
            target: "KSIP/SIP/default".into(),
        }
    }
    /// Reads one policy value. These sit on their own rather than inside the
    /// settings document so that a group policy can push them individually.
    pub fn read_text(&self, name: &str) -> String {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(&self.key)
            .and_then(|key| key.get_value::<String, _>(name))
            .map(|value| value.trim().to_string())
            .unwrap_or_default()
    }
    pub fn write_text(&self, name: &str, value: &str) -> Result<(), String> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&self.key)
            .map_err(|e| e.to_string())?;
        key.set_value(name, &value.to_string())
            .map_err(|e| e.to_string())?;
        let actual: String = key.get_value(name).map_err(|e| e.to_string())?;
        if actual != value {
            return Err(message_with("STORAGE_VERIFY_FAILED", [name]));
        }
        Ok(())
    }
    pub fn read_settings<T: serde::de::DeserializeOwned + Default>(&self) -> Result<T, String> {
        let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(&self.key) {
            Ok(key) => key,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
            Err(e) => return Err(e.to_string()),
        };
        match key.get_value::<String, _>("Settings") {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|_| message("SETTINGS_READ_FAILED"))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(e.to_string()),
        }
    }
    pub fn write_settings<T: Serialize>(&self, settings: &T) -> Result<(), String> {
        let text = serde_json::to_string(settings).map_err(|e| e.to_string())?;
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&self.key)
            .map_err(|e| e.to_string())?;
        key.set_value("Settings", &text)
            .map_err(|e| e.to_string())?;
        let actual: String = key.get_value("Settings").map_err(|e| e.to_string())?;
        if actual != text {
            return Err(message("SETTINGS_VERIFY_FAILED"));
        }
        Ok(())
    }
    /// The address comes from the registry and only the sign-in secret from the
    /// vault. Without a stored secret the account counts as unconfigured.
    pub fn read_account(&self) -> Result<Option<Account>, String> {
        let Some((auth_user, password)) = self.read_secret()? else {
            return Ok(None);
        };
        let server = self.read_text("server");
        let port = self.read_text("port");
        Ok(Some(Account {
            server: if server.is_empty() {
                DEFAULT_SERVER.into()
            } else {
                server
            },
            port: port.parse().unwrap_or(DEFAULT_PORT),
            extension: self.read_text("extension"),
            auth_user,
            password,
        }))
    }
    fn read_secret(&self) -> Result<Option<(String, String)>, String> {
        let target = wide(&self.target);
        let mut item = ptr::null_mut();
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
        let mut blob = account.password.clone().into_bytes();
        if blob.len() > 1024 {
            return Err(message("CREDENTIAL_PASSWORD_TOO_LONG"));
        }
        let mut target = wide(&self.target);
        let mut username = wide(&account.auth_user);
        let mut item: CREDENTIALW = unsafe { std::mem::zeroed() };
        item.Type = CRED_TYPE_GENERIC;
        item.TargetName = target.as_mut_ptr();
        item.UserName = username.as_mut_ptr();
        item.CredentialBlob = blob.as_mut_ptr();
        item.CredentialBlobSize = blob.len() as u32;
        item.Persist = CRED_PERSIST_LOCAL_MACHINE;
        let ok = unsafe { CredWriteW(&item, 0) };
        let error = io::Error::last_os_error();
        for b in &mut blob {
            unsafe {
                ptr::write_volatile(b, 0);
            }
        }
        if ok == 0 {
            return Err(message_with("CREDENTIAL_WRITE_FAILED", [error]));
        }
        self.write_text("server", &account.server)?;
        self.write_text("port", &account.port.to_string())?;
        self.write_text("extension", &account.extension)?;
        let actual = self
            .read_account()?
            .ok_or(message("CREDENTIAL_VERIFY_FAILED"))?;
        if actual.auth_user != account.auth_user || actual.password != account.password {
            return Err(message("CREDENTIAL_VERIFY_FAILED"));
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn cleanup_test(&self) {
        assert!(self.target.starts_with("KSIP/Test/"));
        unsafe {
            CredDeleteW(wide(&self.target).as_ptr(), CRED_TYPE_GENERIC, 0);
        }
        let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.key);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_and_credential_roundtrip_are_separate() {
        let suffix = format!("test-storage-{}", std::process::id());
        let store = Store {
            key: format!(r"Software\KashiharaCity\ksip\Test\{suffix}"),
            target: format!("KSIP/Test/{suffix}"),
        };
        let account = Account {
            server: "192.0.2.10".into(),
            port: 5070,
            extension: "101".into(),
            auth_user: "auth101".into(),
            password: "local-test-only-秘密".into(),
        };
        let result = (|| -> Result<(), String> {
            store.write_account(&account)?;
            store.write_settings(&serde_json::json!({"aec":true}))?;
            let general: serde_json::Value = store.read_settings()?;
            assert_eq!(general, serde_json::json!({"aec":true}));
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
