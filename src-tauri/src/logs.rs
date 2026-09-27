//! The log: the tab's lines and the file, which the link handler process
//! appends to as well; the app's own lines; the clock they are stamped
//! with; and where the data folder is.
use crate::app::Services;
use crate::message::message;
use crate::storage::Store;
use serde::{Deserialize, Serialize};
use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

const LOG_LIMIT: usize = 1000;
/// What the file keeps after a rewrite while the detail log is on. SIP traces
/// and WebRTC's lines fill the ordinary thousand in seconds, and a report
/// needs the minutes around a failure. The tab keeps LOG_LIMIT either way.
const DETAIL_LOG_FILE_LIMIT: usize = 10000;
pub(crate) const LOG_FILE: &str = "ksip-log.jsonl";
// Which layer a log line came from. It is written into the line so that a
// support log shows at a glance whether the app or the engine said it.
pub const LOG_APP: &str = "app";
pub const LOG_ENGINE: &str = "engine";
pub const LOG_EVENT: &str = "event";
const LOG_UI: &str = "ui";
/// What a sync is to write: decided under the lock, written without it.
pub struct SyncPlan {
    bytes: Vec<u8>,
    count: usize,
    offset: u64,
}
/// What the disk part of a sync found and did.
pub struct SyncOutcome {
    foreign: Foreign,
    appended: Result<(), String>,
}
/// Lines the file gained from another process since it was last seen.
#[derive(Default)]
struct Foreign {
    /// The file is shorter than where it was last seen to end: emptied or
    /// replaced, so it counts as new.
    replaced: bool,
    entries: Vec<LogLine>,
    /// Whole lines found, readable or not, and the bytes they took.
    lines: usize,
    consumed: u64,
}
fn read_foreign(path: &Path, mut offset: u64) -> Foreign {
    let mut foreign = Foreign::default();
    let Ok(mut file) = std::fs::File::open(path) else {
        return foreign;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return foreign;
    };
    if len < offset {
        foreign.replaced = true;
        offset = 0;
    }
    if len == offset {
        return foreign;
    }
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(offset)).is_err() || file.read_to_end(&mut bytes).is_err() {
        return foreign;
    }
    // Only whole lines count; one still being written waits for the next look.
    let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    for line in bytes[..end].split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        foreign.lines += 1;
        if let Ok(entry) = serde_json::from_slice::<LogLine>(line) {
            foreign.entries.push(entry);
        }
    }
    foreign.consumed = end as u64;
    foreign
}
/// One line of the log. The parts are kept apart so that the window can
/// present them as it likes, and so that a reader can sort and search them.
#[derive(Clone, Serialize, Deserialize)]
pub struct LogLine {
    /// Local time with its offset from UTC (RFC 3339), because a log is often
    /// read on another machine, in another month, in another country.
    pub time: String,
    pub src: String,
    /// A line the engine wrote, in its own words.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// The name of what the app itself reported, and the values that go with
    /// it. The window turns these into a sentence in the language it shows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}
impl LogLine {
    pub(crate) fn new(time: String, src: &str, body: String) -> Self {
        let (code, args) = if crate::message::is_code(&body) {
            crate::message::split(&body)
        } else {
            (String::new(), vec![])
        };
        Self {
            time,
            src: src.into(),
            text: if code.is_empty() { body } else { String::new() },
            code,
            args,
        }
    }
}
/// The log tab and its file. The tab holds this run's last lines; the file is
/// one JSON object per line and is only ever appended to, except when it has
/// grown to twice the limit and is written afresh with what the tab holds.
/// Another process appends to it too: the link handler, when there is
/// nothing to hand a link to. Those lines are picked up from the file.
pub struct Logs {
    entries: VecDeque<LogLine>,
    sequence: u64,
    flushed: Instant,
    /// Lines at the back of `entries` that the file does not have yet.
    unwritten: usize,
    /// The file as it was last seen: how many lines, and where it ended.
    file_lines: usize,
    offset: u64,
    /// How many lines the file keeps after a rewrite, which happens once it
    /// holds twice as many.
    file_limit: usize,
    /// Whether the last attempt to write the file failed. The unwritten lines
    /// stay in `entries` meanwhile, and are tried again with the next ones.
    write_failed: bool,
}
impl Logs {
    /// Takes stock of the file as it is. Nothing is read back: the tab shows
    /// this run, and the file keeps the one before as well.
    pub fn open(data: &Path) -> Self {
        let (file_lines, offset) = std::fs::read(data.join(LOG_FILE))
            .map(|bytes| (bytes.iter().filter(|b| **b == b'\n').count(), bytes.len() as u64))
            .unwrap_or((0, 0));
        Self {
            entries: VecDeque::new(),
            sequence: 0,
            flushed: Instant::now(),
            unwritten: 0,
            file_lines,
            offset,
            file_limit: LOG_LIMIT,
            write_failed: false,
        }
    }
    /// The detail log keeps ten times as many lines in the file.
    pub fn set_detail(&mut self, detail: bool) {
        self.file_limit = if detail { DETAIL_LOG_FILE_LIMIT } else { LOG_LIMIT };
    }
    pub(crate) fn push(&mut self, line: LogLine) {
        self.entries.push_back(line);
        self.sequence += 1;
        self.unwritten += 1;
        self.trim();
    }
    fn trim(&mut self) {
        while self.entries.len() > LOG_LIMIT {
            self.entries.pop_front();
        }
        self.unwritten = self.unwritten.min(self.entries.len());
    }
    /// Brings the file and the tab up to date with each other: lines another
    /// process appended come in, the lines of this one go out. Appending only
    /// what is new, at most once a second, is what keeps SIP tracing cheap.
    /// A write that fails (a full disk, a file held by another program) keeps
    /// the lines as unwritten, so the next sync carries them along with the
    /// new ones; the tab loses nothing but the oldest once it is full. The
    /// error is returned only when the writing has just started failing, so
    /// the caller can say so once rather than every second; the recovery is
    /// noted in the log itself.
    pub fn sync(&mut self, data: &Path) -> Result<(), String> {
        let plan = self.plan_sync();
        let outcome = Self::perform_sync(data, &plan);
        if self.commit_sync(plan, outcome)? {
            let path = data.join(LOG_FILE);
            if let Ok((length, lines)) = keep_last_lines(&path, self.file_limit) {
                self.file_shortened(length, lines);
            }
        }
        Ok(())
    }
    /// Whether a second has passed since the file was last written.
    pub fn due(&self) -> bool {
        self.flushed.elapsed() >= Duration::from_secs(1)
    }
    /// A sync in three steps, so that whoever holds the log's lock is never
    /// made to wait on the disk: `plan_sync` under the lock says what is to
    /// be written, `perform_sync` reads and writes the file with no lock
    /// held, and `commit_sync` under the lock takes the outcome into the
    /// books. Lines pushed in between wait for the next sync. `sync` does
    /// the three in a row, for a caller that holds nothing else up.
    pub fn plan_sync(&mut self) -> SyncPlan {
        self.flushed = Instant::now();
        SyncPlan {
            bytes: lines(self.entries.iter().skip(self.entries.len() - self.unwritten)),
            count: self.unwritten,
            offset: self.offset,
        }
    }
    /// The disk part: what another process appended since the file was last
    /// seen (a line appended between this look and this process's own append
    /// is only missed by the tab; the file has it), then this process's own
    /// lines appended.
    pub fn perform_sync(data: &Path, plan: &SyncPlan) -> SyncOutcome {
        let path = data.join(LOG_FILE);
        let foreign = read_foreign(&path, plan.offset);
        let appended = if plan.count > 0 {
            let _ = std::fs::create_dir_all(data);
            append(&path, &plan.bytes).map_err(err)
        } else {
            Ok(())
        };
        SyncOutcome { foreign, appended }
    }
    /// Takes a sync's outcome into the books. Err on the first failure to
    /// write since the last success (the lines stay and are tried again;
    /// the same failure is not reported twice). Ok(true) when the file has
    /// grown to twice the limit and wants shortening (`keep_last_lines` and
    /// then `file_shortened`), which the caller does without the lock.
    pub fn commit_sync(&mut self, plan: SyncPlan, outcome: SyncOutcome) -> Result<bool, String> {
        let foreign = outcome.foreign;
        if foreign.replaced {
            // Someone emptied or replaced the file; it is taken as new.
            self.offset = 0;
            self.file_lines = 0;
        }
        for entry in foreign.entries {
            // In front of the unwritten lines: the file has this one already.
            let at = self.entries.len() - self.unwritten;
            self.entries.insert(at, entry);
            self.sequence += 1;
        }
        self.file_lines += foreign.lines;
        self.offset += foreign.consumed;
        self.trim();
        match outcome.appended {
            Ok(()) => {
                self.offset += plan.bytes.len() as u64;
                self.file_lines += plan.count;
                self.unwritten -= plan.count;
                if self.write_failed {
                    self.write_failed = false;
                    self.push(LogLine::new(stamp(), LOG_APP, message("JOURNAL_WRITE_RECOVERED")));
                }
            }
            Err(e) => {
                let first = !self.write_failed;
                self.write_failed = true;
                if first {
                    return Err(e);
                }
            }
        }
        Ok(self.file_lines > 2 * self.file_limit)
    }
    /// How many lines the file keeps after a rewrite.
    pub fn file_limit(&self) -> usize {
        self.file_limit
    }
    /// The file was shortened to its last lines (`keep_last_lines`): where it
    /// ends now, and how many lines it holds. The file rather than the tab is
    /// the source: after a start the tab holds only this run, and the run
    /// before belongs in the file as much as this one.
    pub fn file_shortened(&mut self, length: u64, lines: usize) {
        self.offset = length;
        self.file_lines = lines;
    }
    fn clear(&mut self, data: &Path) {
        self.entries.clear();
        // Restarting the sequence tells the window that its copy is stale.
        self.sequence = 0;
        self.unwritten = 0;
        let _ = std::fs::write(data.join(LOG_FILE), b"");
        self.offset = 0;
        self.file_lines = 0;
    }
    /// How many lines have been logged this run: the tab reloads when it moves.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}
/// One JSON object per line, the form both files are kept in.
pub(crate) fn lines<'a, T: Serialize + 'a>(entries: impl Iterator<Item = &'a T>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        if let Ok(json) = serde_json::to_vec(entry) {
            bytes.extend(json);
            bytes.push(b'\n');
        }
    }
    bytes
}
pub(crate) fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?
        .write_all(bytes)
}
/// Writes the file afresh with its last lines, up to the limit, and says how
/// long it is now and how many lines it holds.
pub(crate) fn keep_last_lines(path: &Path, limit: usize) -> std::io::Result<(u64, usize)> {
    let bytes = std::fs::read(path)?;
    // The last line ends with the last newline; the kept lines start after
    // the newline that ends the line before the first of them.
    let mut newlines = 0;
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate().rev() {
        if *b == b'\n' {
            newlines += 1;
            if newlines > limit {
                start = i + 1;
                break;
            }
        }
    }
    std::fs::write(path, &bytes[start..])?;
    Ok(((bytes.len() - start) as u64, newlines.min(limit)))
}
/// One line from a process that is not the app: the link handler, when it
/// has nothing to hand a link to. The app picks the line up from the file.
pub fn append_log(data: &Path, body: String) {
    let clean: String = body.chars().filter(|c| !c.is_control()).take(300).collect();
    let bytes = lines(std::iter::once(&LogLine::new(stamp(), LOG_APP, clean)));
    let _ = std::fs::create_dir_all(data);
    let _ = append(&data.join(LOG_FILE), &bytes);
}
/// Where the running app keeps what it writes: the person's own local
/// application data, beside the settings' place in the registry. Not beside
/// the executable: a single exe gets run from a file server sooner or later,
/// and there the history, the log and the recordings of everyone who did so
/// would end up in one folder, mixed and readable by all. Not the roaming
/// part either: recordings are big and belong to the machine they were made
/// on. Test profiles keep theirs in the repository's temp/build. The
/// repository is found from the executable, which the tests run from inside
/// it (release/, temp/build/<test>/, temp/cargo-target/). Naming it at compile
/// time would put the build machine's path into the product and make the
/// binary depend on where it was built.
pub fn data_dir(store: &Store) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let beside = exe.parent().map(PathBuf::from).unwrap_or_default();
    if !store.target.starts_with("KSIP/Test/") {
        return std::env::var_os("LOCALAPPDATA")
            .filter(|dir| !dir.is_empty())
            .map(|dir| PathBuf::from(dir).join("KashiharaCity").join("ksip"))
            .unwrap_or(beside);
    }
    exe.ancestors()
        .find(|dir| dir.join("src-tauri/Cargo.toml").is_file())
        .map(|dir| {
            dir.join("temp/build")
                .join(store.target.rsplit('/').next().unwrap_or("test"))
        })
        .unwrap_or(beside)
}
#[derive(Clone, Serialize)]
pub struct LogPage {
    pub from: u64,
    pub entries: Vec<LogLine>,
}
/// Local wall clock for a log line, without pulling in a date crate.
pub fn stamp() -> String {
    use windows_sys::Win32::System::Time::{
        GetTimeZoneInformation, TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
    };
    // TIME_ZONE_ID_DAYLIGHT, whose constant lives in a feature nothing else needs.
    const DAYLIGHT: u32 = 2;
    let mut now: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let mut zone: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: both calls only fill the structures above.
    unsafe { GetLocalTime(&mut now) };
    let kind = unsafe { GetTimeZoneInformation(&mut zone) };
    // The bias says how many minutes local time is behind UTC, which is the
    // opposite sign of the offset written after the time.
    let bias = match kind {
        TIME_ZONE_ID_INVALID => 0,
        DAYLIGHT => zone.Bias + zone.DaylightBias,
        _ => zone.Bias + zone.StandardBias,
    };
    let offset = -bias;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
        now.wYear,
        now.wMonth,
        now.wDay,
        now.wHour,
        now.wMinute,
        now.wSecond,
        if offset < 0 { '-' } else { '+' },
        offset.abs() / 60,
        offset.abs() % 60
    )
}
impl Services {
    /// One line into the tab and, once a second, into the file. What a line
    /// from the engine says about the phone is the actor's to read.
    pub fn log(&self, source: &str, s: String) {
        // Into the tab only; the file thread writes the file once a second,
        // so that no caller waits on the disk.
        let mut logs = self.logs.lock().unwrap();
        logs.push(LogLine::new(stamp(), source, s));
        self.log_sequence.store(logs.sequence(), std::sync::atomic::Ordering::Relaxed);
    }
    /// What the window and the shortcuts do belongs in the same log.
    pub fn log_app(&self, text: String) {
        let clean: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
        if !clean.trim().is_empty() {
            self.log(LOG_APP, clean);
        }
    }
    /// Every command that arrives through a browser link is recorded: it comes
    /// from outside the app and explains what happened afterwards.
    pub fn log_protocol(&self, command: &str) {
        let clean: String = command.chars().filter(|c| !c.is_control()).take(100).collect();
        self.log(LOG_APP, format!("protocol {clean}"));
    }
    /// The web view has no log of its own and hands its failures to this one.
    pub fn log_ui(&self, text: String) {
        let clean: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
        if !clean.trim().is_empty() {
            self.log(LOG_UI, clean);
        }
    }
    /// A panic may be the last thing this process does and may already hold a
    /// lock, so this neither waits nor unwraps, and writes the file at once.
    pub fn log_panic(&self, text: String) {
        let Ok(mut logs) = self.logs.try_lock() else {
            return;
        };
        let line = text.replace(['\r', '\n'], " ");
        logs.push(LogLine::new(stamp(), LOG_APP, line));
        // A write that fails is kept and said by the file thread's next sync.
        let _ = logs.sync(&self.data);
    }
    pub fn clear_logs(&self) {
        let mut logs = self.logs.lock().unwrap();
        logs.clear(&self.data);
        self.log_sequence.store(logs.sequence(), std::sync::atomic::Ordering::Relaxed);
    }
    pub fn read_logs(&self, after: u64) -> LogPage {
        let logs = self.logs.lock().unwrap();
        let start = logs.sequence - logs.entries.len() as u64;
        if after >= start && after <= logs.sequence {
            LogPage {
                from: after,
                entries: logs
                    .entries
                    .iter()
                    .skip((after - start) as usize)
                    .cloned()
                    .collect(),
            }
        } else {
            LogPage {
                from: start,
                entries: logs.entries.iter().cloned().collect(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logs_move_by_sequence_and_reset_when_lines_are_dropped() {
        let app = Services::open().0;
        app.log(LOG_APP, "one".into());
        app.log(LOG_APP, "two".into());
        let page = app.read_logs(0);
        assert_eq!(page.from, 0);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].src, LOG_APP);
        assert_eq!(page.entries[0].text, "one");
        // 2026-09-21T09:12:54+09:00 の形であること。
        let stamp = &page.entries[0].time;
        let (date, rest) = stamp.split_once('T').expect("date and time");
        assert!(date.split('-').all(|p| p.parse::<u32>().is_ok()));
        let (time, offset) = rest.split_at(8);
        assert_eq!(time.split(':').count(), 3);
        assert!(
            offset.starts_with(['+', '-']) && offset.len() == 6 && offset[1..].contains(':'),
            "offset was {offset}"
        );
        let page = app.read_logs(1);
        assert_eq!(page.from, 1);
        assert_eq!(page.entries[0].text, "two");
        assert!(app.read_logs(2).entries.is_empty());
        for i in 0..LOG_LIMIT {
            app.log(LOG_APP, format!("line {i}"));
        }
        let page = app.read_logs(1);
        assert_eq!(page.entries.len(), LOG_LIMIT);
        assert!(page.from > 1, "the window is told which lines it lost");
        assert_eq!(app.logs.lock().unwrap().sequence(), LOG_LIMIT as u64 + 2);
    }
    #[test]
    fn ui_and_panic_lines_are_labelled_and_kept_readable() {
        let app = Services::open().0;
        app.log_ui("failed\u{7}\nwith a control char".into());
        app.log_panic("panic at 'boom'\nsecond line".into());
        let entries = app.read_logs(0).entries;
        assert_eq!(entries[0].src, LOG_UI);
        assert_eq!(entries[0].text, "failedwith a control char");
        // A panic stays on one line so its time is next to it.
        assert_eq!(entries[1].src, LOG_APP);
        assert_eq!(entries[1].text, "panic at 'boom' second line");
        // Blank input is not worth a line of its own.
        app.log_ui("   ".into());
        assert_eq!(app.read_logs(0).entries.len(), 2);
    }
    #[test]
    fn the_log_file_is_appended_to_and_shared_with_another_process() {
        let dir = std::env::temp_dir().join(format!("ksip-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        let mut logs = Logs::open(&dir);
        logs.push(LogLine::new("t1".into(), LOG_APP, "one".into()));
        logs.push(LogLine::new("t2".into(), LOG_APP, "two".into()));
        logs.sync(&dir).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), 2, "{written}");
        assert!(written.lines().all(|l| l.starts_with('{') && l.ends_with('}')));
        // Another process appends a line; the next look brings it into the tab,
        // and this process's own new line is appended after it.
        append_log(&dir, "protocol ksip:x could not be handed over".into());
        logs.push(LogLine::new("t3".into(), LOG_APP, "three".into()));
        logs.sync(&dir).unwrap();
        let texts: Vec<&str> = logs.entries.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "protocol ksip:x could not be handed over", "three"]);
        assert_eq!(logs.sequence, 4);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 4);
        // Nothing is written when nothing is new, and the file is only rewritten
        // once it holds twice the limit, then with what the tab holds.
        let size = std::fs::metadata(&path).unwrap().len();
        logs.sync(&dir).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        // Lines arrive in bursts between syncs; the file grows past twice the
        // limit on the third burst and is then written afresh.
        let burst = 700;
        for i in 0..3 * burst {
            logs.push(LogLine::new("t".into(), LOG_APP, format!("line {i}")));
            if (i + 1) % burst == 0 {
                logs.sync(&dir).unwrap();
                let lines = std::fs::read_to_string(&path).unwrap().lines().count();
                assert_eq!(lines, if i + 1 < 3 * burst { 4 + i + 1 } else { LOG_LIMIT }, "after line {i}");
            }
        }
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(rewritten.lines().last().unwrap().contains(&format!("line {}", 3 * burst - 1)));
        // The file's own last lines are kept, four of which came before the bursts.
        assert!(rewritten.lines().next().unwrap().contains(&format!("\"line {}\"", 3 * burst - LOG_LIMIT)));
        // A second look at the same file starts from where it ends.
        let again = Logs::open(&dir);
        assert_eq!(again.file_lines, LOG_LIMIT);
        assert_eq!(again.offset, std::fs::metadata(&path).unwrap().len());
        // With the detail log on the file keeps ten times as much: the same
        // bursts bring no rewrite, and turning detail off brings one.
        logs.set_detail(true);
        for i in 0..3 * burst {
            logs.push(LogLine::new("t".into(), LOG_APP, format!("more {i}")));
            if (i + 1) % burst == 0 {
                logs.sync(&dir).unwrap();
            }
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), LOG_LIMIT + 3 * burst);
        logs.set_detail(false);
        logs.sync(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), LOG_LIMIT);
        logs.clear(&dir);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!((logs.sequence, logs.file_lines, logs.offset), (0, 0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn the_data_folder_is_the_persons_own_beside_the_registry_settings() {
        let store = Store {
            key: String::new(),
            target: "KSIP/SIP/default".into(),
        };
        let data = data_dir(&store);
        let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA"));
        assert_eq!(data, local.join("KashiharaCity").join("ksip"));
        let test = Store {
            key: String::new(),
            target: "KSIP/Test/test-unit".into(),
        };
        assert!(data_dir(&test).ends_with("temp/build/test-unit"), "{}", data_dir(&test).display());
    }
    #[test]
    fn lines_that_could_not_be_written_wait_for_the_next_sync() {
        let dir = std::env::temp_dir().join(format!("ksip-journal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A folder where the file should be: every append fails.
        std::fs::create_dir_all(dir.join(LOG_FILE)).unwrap();
        let mut logs = Logs::open(&dir);
        logs.push(LogLine::new("2026-09-26T22:00:00+09:00".into(), LOG_APP, "first".into()));
        logs.push(LogLine::new("2026-09-26T22:00:01+09:00".into(), LOG_APP, "second".into()));
        assert!(logs.sync(&dir).is_err(), "the first failure is reported");
        assert_eq!(logs.unwritten, 2, "the lines wait");
        assert!(logs.sync(&dir).is_ok(), "the same failure is not reported again");
        assert_eq!(logs.unwritten, 2);
        // The way is clear again: everything that waited is written, in order,
        // and the log notes its own recovery.
        std::fs::remove_dir(dir.join(LOG_FILE)).unwrap();
        assert!(logs.sync(&dir).is_ok());
        assert_eq!(logs.unwritten, 1, "the recovery note itself is written with the next sync");
        assert!(logs.sync(&dir).is_ok());
        assert_eq!(logs.unwritten, 0);
        let written = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap();
        assert_eq!(written.lines().count(), 3);
        assert!(written.lines().next().unwrap().contains("first"));
        assert!(written.lines().last().unwrap().contains("JOURNAL_WRITE_RECOVERED"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
