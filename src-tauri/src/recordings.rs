//! The recordings: what a file is called, turning a finished WAV into an
//! MP3 and tidying the folder, and finding or opening a recording the
//! history names.
use crate::app::Services;
use crate::logs::LOG_APP;
use crate::message::{message, message_with};
use crate::phone_state::peer_number;
use std::{
    path::{Path, PathBuf},
    process::Command as Process,
    sync::atomic::Ordering,
    thread,
};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub fn recording_name(stamp: &str, peer: &str) -> String {
    let time: String = stamp
        .chars()
        .take(19)
        .map(|c| match c {
            'T' => '_',
            ':' => '-',
            c => c,
        })
        .collect();
    let user: String = peer_number(peer)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '_' | '.' | '-'))
        .take(40)
        .collect();
    let user = if user.is_empty() { "call".to_string() } else { user };
    format!("{time}_{user}.wav")
}
/// The name a conversion writes under until it is finished. It keeps the
/// `.mp3` extension, which is how the encoder picks its container, and is
/// never what a finished recording is called.
fn partial_recording(wav: &Path) -> PathBuf {
    wav.with_extension("converting.mp3")
}
/// Tidies a recordings folder from earlier runs and says which WAVs still
/// want converting: a partial MP3 goes (its conversion never finished, and
/// the WAV is still the recording), and every WAV is handed back, the ones
/// with an MP3 beside them included. An MP3 next to its WAV may be one an
/// earlier version wrote straight under the final name and never finished,
/// so it is not taken as proof of anything: the WAV is converted again, the
/// MP3 replaced by the new one, and only then does the WAV go. Nothing else
/// is touched.
fn sweep_recording_folder(folder: &Path) -> Vec<PathBuf> {
    let mut leftover = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return leftover;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if name.ends_with(".converting.mp3") {
            let _ = std::fs::remove_file(&path);
        } else if name.ends_with(".wav") {
            leftover.push(path);
        }
    }
    leftover
}
fn explorer_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if let Some(unc) = text.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        text.strip_prefix("\\\\?\\").unwrap_or(&text).to_string()
    }
}
impl Services {
    /// The file a history row's recording is in now: the MP3 once it has
    /// been made, the WAV until then, nothing once both are gone.
    pub fn recording_file(&self, name: &str) -> Option<PathBuf> {
        let plain = !name.is_empty()
            && name.ends_with(".wav")
            && !name.contains(['/', '\\', ':'])
            && !name.starts_with('.');
        if !plain {
            return None;
        }
        let wav = self.data.join("recordings").join(name);
        [wav.with_extension("mp3"), wav].into_iter().find(|path| path.is_file())
    }
    /// Shows a recording named in the history in Explorer, selected.
    pub fn open_recording_location(&self, name: &str) -> Result<(), String> {
        let path = self.recording_file(name).ok_or_else(|| message("RECORDING_NOT_FOUND"))?;
        crate::native::show_in_folder(&path)
    }
    /// Plays a recording named in the history with whatever Windows plays
    /// sound files with. Only a file in the recordings folder can be named.
    pub fn open_recording(&self, name: &str) -> Result<(), String> {
        let path = self.recording_file(name).ok_or_else(|| message("RECORDING_NOT_FOUND"))?;
        crate::native::open_url(&path.to_string_lossy())
            .map_err(|e| message_with("RECORDING_OPEN_FAILED", [crate::message::split(&e).1.join(" ")]))
    }
    /// Turns a recording that has just closed into an MP3, on a thread of
    /// its own with a lower priority, so that the next call is not disturbed.
    /// The encoder writes under a partial name, and the real name appears
    /// only once the file is finished: nothing reads a half-written MP3 as
    /// the recording, and an exit in the middle leaves the WAV as the one
    /// copy, which the next start converts. The WAV goes once the MP3 is
    /// there; if it is being played just then, it stays until the next start
    /// sweeps it away. A failure leaves the WAV.
    pub fn convert_recording(&self, wav: PathBuf) {
        let me = self.clone();
        me.converting.fetch_add(1, Ordering::Relaxed);
        thread::spawn(move || {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
            // SAFETY: the current thread's own priority is all that is touched;
            // GetCurrentThread's pseudo-handle needs no closing.
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
            let mp3 = wav.with_extension("mp3");
            let partial = partial_recording(&wav);
            let name = wav.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // A recording the engine never closed says it holds no samples;
            // its header is put right from the file's length before encoding.
            if crate::wav::repair_sizes(&wav) == Ok(true) {
                me.log(LOG_APP, message_with("RECORDING_HEADER_REPAIRED", [&name]));
            }
            let finished = crate::mp3::transcode(&wav, &partial)
                .and_then(|()| std::fs::rename(&partial, &mp3).map_err(|e| message_with("RECORDING_CONVERT_FAILED", [err(e)])));
            match finished {
                Ok(()) => match std::fs::remove_file(&wav) {
                    Ok(()) => me.log(LOG_APP, message_with("RECORDING_CONVERTED", [&name])),
                    Err(_) => me.log(LOG_APP, message_with("RECORDING_WAV_KEPT", [&name])),
                },
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    me.log(LOG_APP, e);
                }
            }
            me.converting.fetch_sub(1, Ordering::Relaxed);
        });
    }
    /// What earlier runs left in the recordings folder, at start: partial
    /// MP3s go, and every WAV that is left is converted now, one with an MP3
    /// beside it again (the WAV goes only once its new MP3 is whole). A WAV
    /// an engine that could not be confirmed stopped was writing is among
    /// them: converting it here is the best that can be done for it.
    pub fn sweep_recordings(&self) {
        for wav in sweep_recording_folder(&self.data.join("recordings")) {
            self.convert_recording(wav);
        }
    }
    pub fn open_recordings(&self) -> Result<(), String> {
        let path = self.data.join("recordings");
        std::fs::create_dir_all(&path).map_err(err)?;
        Process::new("explorer.exe")
            .arg(explorer_path(&path))
            .spawn()
            .map_err(err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explorer_receives_windows_folder_paths() {
        assert_eq!(
            explorer_path(std::path::Path::new("C:\\AEC Lab\\data/recordings")),
            "C:\\AEC Lab\\data\\recordings"
        );
        assert_eq!(
            explorer_path(std::path::Path::new("\\\\?\\C:\\AEC Lab\\data/recordings")),
            "C:\\AEC Lab\\data\\recordings"
        );
        assert_eq!(
            explorer_path(std::path::Path::new("\\\\?\\UNC\\server\\share/recordings")),
            "\\\\server\\share\\recordings"
        );
    }
    #[test]
    fn the_recordings_folder_is_tidied_and_leftover_wavs_are_named() {
        let folder = std::env::temp_dir().join(format!("ksip-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let file = |name: &str| {
            std::fs::write(folder.join(name), b"x").unwrap();
            folder.join(name)
        };
        // Finished: the WAV goes. Unfinished: the partial goes, the WAV is handed back.
        // Never started: handed back. A finished MP3 alone stays as it is.
        file("2026-09-26_10-00-00_1002.wav");
        file("2026-09-26_10-00-00_1002.mp3");
        file("2026-09-26_10-05-00_1003.wav");
        file("2026-09-26_10-05-00_1003.converting.mp3");
        file("2026-09-26_10-10-00_1004.wav");
        file("2026-09-26_10-15-00_1005.mp3");
        let leftover = sweep_recording_folder(&folder);
        assert_eq!(
            leftover,
            vec![folder.join("2026-09-26_10-00-00_1002.wav"), folder.join("2026-09-26_10-05-00_1003.wav"), folder.join("2026-09-26_10-10-00_1004.wav")],
            "a WAV beside an MP3 is converted again rather than trusted away"
        );
        let mut names: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "2026-09-26_10-00-00_1002.mp3",
                "2026-09-26_10-00-00_1002.wav",
                "2026-09-26_10-05-00_1003.wav",
                "2026-09-26_10-10-00_1004.wav",
                "2026-09-26_10-15-00_1005.mp3"
            ]
        );
        assert!(sweep_recording_folder(&folder.join("missing")).is_empty());
        assert_eq!(partial_recording(Path::new("a/b.wav")), PathBuf::from("a/b.converting.mp3"));
        let _ = std::fs::remove_dir_all(&folder);
    }
    #[test]
    fn a_recording_is_named_after_its_start_and_the_peer() {
        assert_eq!(recording_name("2026-09-23T14:30:12+09:00", "sip:1002@pbx.example:5060;transport=udp"), "2026-09-23_14-30-12_1002.wav");
        assert_eq!(recording_name("2026-09-23T14:30:12+09:00", "<sips:+81-6-1234@pbx.example>"), "2026-09-23_14-30-12_+81-6-1234.wav");
        assert_eq!(recording_name("2026-09-23T14:30:12+09:00", "sip:a b*c@pbx"), "2026-09-23_14-30-12_abc.wav");
        assert_eq!(recording_name("2026-09-23T14:30:12+09:00", ""), "2026-09-23_14-30-12_call.wav");
    }
}
