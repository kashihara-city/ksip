//! What the app says, without saying it.
//!
//! Rust names what happened and hands over the values that belong in the
//! sentence; the window owns the words, in whatever language it is showing.
//! The engine module names its own results the same way, so one table in the
//! window resolves both layers.
//!
//! A message travels as a single string, so that every `Result<_, String>` in
//! the app keeps working unchanged. Most messages carry no values and are just
//! the name. One that carries values is written as JSON, because a value can
//! hold anything at all: commas, quotes, paths, and whatever Windows put in an
//! error text. A separator character would be eaten by the first layer that
//! sanitises what it is given, starting with our own log.

use serde_json::json;
use std::sync::RwLock;

/// A message that is only a name, such as `SIP_ACCOUNT_REQUIRED`.
pub fn message(code: &str) -> String {
    code.to_string()
}

/// A message with the values its sentence needs, in the order it uses them.
pub fn message_with<I>(code: &str, values: I) -> String
where
    I: IntoIterator,
    I::Item: std::fmt::Display,
{
    let values: Vec<String> = values.into_iter().map(|v| v.to_string()).collect();
    json!({ "code": code, "args": values }).to_string()
}

/// Whether the text is a message of ours rather than a line of prose. The
/// engine's own output is prose and travels through the log untouched.
pub fn is_code(text: &str) -> bool {
    if text.starts_with('{') {
        return split(text).0.chars().next().is_some();
    }
    !text.is_empty()
        && text.starts_with(|c: char| c.is_ascii_uppercase())
        && text
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Splits a message into its name and its values. Prose has neither.
pub fn split(text: &str) -> (String, Vec<String>) {
    if text.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(code) = value.get("code").and_then(|c| c.as_str()) {
                let args = value
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|v| v.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                return (code.to_string(), args);
            }
        }
        return (String::new(), vec![]);
    }
    (text.to_string(), vec![])
}

/// The languages the tables below speak, matched by their first part, so that
/// `ja-JP` finds Japanese and `zh-HK` finds Traditional Chinese, as the window does.
const LANGUAGES: [&str; 3] = ["ja", "en", "zh-TW"];

/// The language for what Windows draws. It is set once the settings are read,
/// and again whenever they are saved, so it follows the window.
static LANGUAGE: RwLock<&'static str> = RwLock::new("ja");

/// The table a language tag lands in, if any table speaks it.
fn known(tag: &str) -> Option<&'static str> {
    let lower = tag.trim().to_ascii_lowercase();
    let primary = lower.split(['-', '_']).next().unwrap_or("");
    if primary.is_empty() {
        return None;
    }
    LANGUAGES
        .iter()
        .copied()
        .find(|language| language.split('-').next().unwrap_or("") == primary)
}

/// The language Windows itself is shown in, such as `ja-JP`.
fn windows_locale() -> String {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    // LOCALE_NAME_MAX_LENGTH, including the terminator.
    let mut name = [0u16; 85];
    // SAFETY: the buffer and its length are handed over together.
    let written = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), name.len() as i32) };
    if written <= 1 {
        return String::new();
    }
    String::from_utf16_lossy(&name[..(written - 1) as usize])
}

/// Chooses the language for what Windows draws: the saved one, or the one
/// Windows is shown in, or Japanese.
pub fn set_windows_language(saved: &str) {
    let chosen = known(saved)
        .or_else(|| known(&windows_locale()))
        .unwrap_or("ja");
    *LANGUAGE.write().unwrap() = chosen;
}

/// The words for what Windows itself draws: the tray menu, the open dialog
/// and the toast. The window's table cannot reach these, because they exist
/// without a window. Each language has a table of its own here.
pub fn windows_text(code: &str) -> &str {
    let language = *LANGUAGE.read().unwrap();
    text_in(language, code).unwrap_or(code)
}

fn text_in(language: &str, code: &str) -> Option<&'static str> {
    let text = match language {
        "en" => match code {
            "TRAY_OPEN" => "Show KSIP",
            "TRAY_QUIT" => "Quit",
            "TRAY_LICENSES" => "Licenses",
            "DIALOG_CA_FILE" => "Certificate authority to trust",
            "DIALOG_CA_FILTER" => "Certificates (*.crt;*.pem;*.cer)\0*.crt;*.pem;*.cer\0",
            "DIALOG_SOUND_FILE" => "Sound file for the phone",
            "DIALOG_SOUND_FILTER" => "WAV (*.wav)\0*.wav\0",
            "NOTIFY_INCOMING" => "Incoming call from {0}",
            _ => return None,
        },
        "zh-TW" => match code {
            "TRAY_OPEN" => "顯示 KSIP",
            "TRAY_QUIT" => "結束",
            "TRAY_LICENSES" => "授權",
            "DIALOG_CA_FILE" => "信任的憑證授權單位憑證",
            "DIALOG_CA_FILTER" => "憑證 (*.crt;*.pem;*.cer)\0*.crt;*.pem;*.cer\0",
            "DIALOG_SOUND_FILE" => "電話鈴聲的檔案",
            "DIALOG_SOUND_FILTER" => "WAV (*.wav)\0*.wav\0",
            "NOTIFY_INCOMING" => "{0} 來電",
            _ => return None,
        },
        _ => match code {
            "TRAY_OPEN" => "KSIPを表示",
            "TRAY_QUIT" => "終了",
            "TRAY_LICENSES" => "ライセンス",
            "DIALOG_CA_FILE" => "信頼する認証局の証明書",
            "DIALOG_CA_FILTER" => "証明書 (*.crt;*.pem;*.cer)\0*.crt;*.pem;*.cer\0",
            "DIALOG_SOUND_FILE" => "着信音に使うファイル",
            "DIALOG_SOUND_FILTER" => "WAV (*.wav)\0*.wav\0",
            "NOTIFY_INCOMING" => "{0} から着信",
            _ => return None,
        },
    };
    Some(text)
}

/// The same words with the values filled in, for what Windows draws.
pub fn windows_text_with<I>(code: &str, values: I) -> String
where
    I: IntoIterator,
    I::Item: std::fmt::Display,
{
    let mut text = windows_text(code).to_string();
    for (index, value) in values.into_iter().enumerate() {
        text = text.replace(&format!("{{{index}}}"), &value.to_string());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_its_name_and_its_values() {
        assert_eq!(message("ADAPTER_MISSING"), "ADAPTER_MISSING");
        let text = message_with("ENGINE_EXITED", ["3"]);
        assert_eq!(split(&text), ("ENGINE_EXITED".into(), vec!["3".to_string()]));
    }

    #[test]
    fn a_value_survives_whatever_it_contains() {
        let awkward = "C:\\x, \"y\"\nz";
        let text = message_with("CREDENTIAL_WRITE_FAILED", [awkward]);
        assert!(!text.contains('\n'), "the message stays on one line: {text}");
        assert_eq!(split(&text).1, vec![awkward.to_string()]);
    }

    #[test]
    fn what_windows_draws_has_words_of_its_own() {
        assert_eq!(text_in("ja", "TRAY_QUIT"), Some("終了"));
        assert_eq!(text_in("en", "TRAY_QUIT"), Some("Quit"));
        assert_eq!(text_in("zh-TW", "TRAY_QUIT"), Some("結束"));
        // Every language answers every name, with the same values.
        for code in ["TRAY_OPEN", "TRAY_QUIT", "TRAY_LICENSES", "DIALOG_CA_FILE",
            "DIALOG_CA_FILTER", "DIALOG_SOUND_FILE", "DIALOG_SOUND_FILTER", "NOTIFY_INCOMING"]
        {
            let japanese = text_in("ja", code).unwrap();
            for language in LANGUAGES {
                let text = text_in(language, code).unwrap();
                assert_eq!(text.contains("{0}"), japanese.contains("{0}"), "{language} {code}");
            }
        }
        // An unknown name is shown as it is, rather than as nothing at all.
        assert_eq!(windows_text("NO_SUCH_NAME"), "NO_SUCH_NAME");
    }

    #[test]
    fn a_language_tag_finds_its_table_by_its_first_part() {
        assert_eq!(known("ja-JP"), Some("ja"));
        assert_eq!(known("en_US"), Some("en"));
        assert_eq!(known("zh-HK"), Some("zh-TW"));
        assert_eq!(known("fr-FR"), None);
        assert_eq!(known(""), None);
    }

    #[test]
    fn prose_is_not_a_message() {
        assert!(is_code("REGISTER_OK"));
        assert!(is_code(&message_with("ENGINE_EXITED", ["3"])));
        assert!(!is_code("ua: SIP register failed"));
        assert!(!is_code("REGISTER_OK 200 OK"));
        assert!(!is_code(""));
    }
}
