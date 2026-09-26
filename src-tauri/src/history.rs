//! The call history: one row per call, newest first in the tab, appended
//! to a file that the next run reads back.
use crate::app::Services;
use crate::logs::{append, keep_last_lines, lines};
use serde::{Deserialize, Serialize};
use std::path::Path;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CallHistory {
    pub ended_at: u64,
    pub direction: String,
    pub peer: String,
    #[serde(default)]
    pub name: String,
    pub duration: u32,
    /// The file in `recordings/` that holds this call, if it was recorded.
    /// A file can hold several calls: automatic recording follows the call
    /// in progress, through a transfer for instance.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recording: String,
    /// How a call that never connected ended: the name of it (busy, not
    /// found, missed...), empty for a call that was talked on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
}
const HISTORY_LIMIT: usize = 1000;
const HISTORY_FILE: &str = "call-history.jsonl";
// Log lines and call history are served by sequence so a poll only moves what is new.
/// The call history and its file: one call per line, oldest first, appended
/// as calls end. The tab shows it newest first. Past twice the limit the file
/// is written afresh with its last lines. Nothing else writes it.
pub struct History {
    /// Newest first, as the tab shows it.
    rows: Vec<CallHistory>,
    sequence: u64,
    file_lines: usize,
    /// Rows the file does not have yet, oldest first: what could not be
    /// appended is kept and tried again with the next rows, or at the next
    /// flush, rather than being lost with the call state it was made from.
    pending: Vec<CallHistory>,
}
impl History {
    /// Reads the file, oldest first, into the tab's order.
    pub fn open(data: &Path) -> Self {
        let mut rows: Vec<CallHistory> = Vec::new();
        let mut file_lines = 0;
        if let Ok(bytes) = std::fs::read(data.join(HISTORY_FILE)) {
            for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                file_lines += 1;
                if let Ok(row) = serde_json::from_slice::<CallHistory>(line) {
                    rows.push(row);
                }
            }
        }
        rows.reverse();
        rows.truncate(HISTORY_LIMIT);
        Self {
            rows,
            sequence: 0,
            file_lines,
            pending: Vec::new(),
        }
    }
    /// The calls that ended since the last look, in the order the tab shows
    /// them. The tab gets them at once; the file, being oldest first, gets
    /// them the other way round, together with whatever is still waiting.
    pub fn add(&mut self, data: &Path, ended: Vec<CallHistory>) -> Result<(), String> {
        for entry in ended.iter().rev() {
            self.rows.insert(0, entry.clone());
        }
        self.rows.truncate(HISTORY_LIMIT);
        self.sequence += 1;
        self.pending.extend(ended.into_iter().rev());
        let excess = self.pending.len().saturating_sub(HISTORY_LIMIT);
        self.pending.drain(..excess);
        self.flush(data)
    }
    /// Writes the rows still waiting for the file, if any.
    pub fn flush(&mut self, data: &Path) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let path = data.join(HISTORY_FILE);
        std::fs::create_dir_all(data).map_err(err)?;
        append(&path, &lines(self.pending.iter())).map_err(err)?;
        self.file_lines += self.pending.len();
        self.pending.clear();
        if self.file_lines > 2 * HISTORY_LIMIT {
            if let Ok((_, kept)) = keep_last_lines(&path, HISTORY_LIMIT) {
                self.file_lines = kept;
            }
        }
        Ok(())
    }
    fn clear(&mut self, data: &Path) -> Result<(), String> {
        self.rows.clear();
        self.pending.clear();
        self.sequence += 1;
        self.file_lines = 0;
        std::fs::create_dir_all(data).map_err(err)?;
        std::fs::write(data.join(HISTORY_FILE), b"").map_err(err)
    }
    /// How many rows have been added this run: the tab reloads when it moves.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}
impl Services {
    /// Throws the call history away, here and in its file.
    pub fn clear_call_history(&self) -> Result<(), String> {
        self.history.lock().unwrap().clear(&self.data)
    }
    /// The rows as the tab shows them: a recording is named only while its
    /// file is still there to be played.
    pub fn read_call_history(&self) -> Vec<CallHistory> {
        let mut rows = self.history.lock().unwrap().rows.clone();
        for row in &mut rows {
            if !row.recording.is_empty() && self.recording_file(&row.recording).is_none() {
                row.recording.clear();
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_call_history_is_appended_to_and_read_back_newest_first() {
        let dir = std::env::temp_dir().join(format!("ksip-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let call = |n: u64| CallHistory {
            ended_at: n,
            direction: "OUTGOING".into(),
            peer: format!("sip:{n}@pbx"),
            name: String::new(),
            duration: 1,
            recording: String::new(),
            outcome: String::new(),
        };
        let path = dir.join(HISTORY_FILE);
        let mut history = History::open(&dir);
        assert!(history.rows.is_empty());
        history.add(&dir, vec![call(1)]).unwrap();
        history.add(&dir, vec![call(2)]).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), 2);
        assert!(written.lines().next().unwrap().contains("\"ended_at\":1"), "oldest first in the file");
        // Calls that end are appended, in the order the tab shows them; the
        // next start reads the file and shows the same order.
        history.add(&dir, vec![call(3), call(4)]).unwrap();
        let ended: Vec<u64> = history.rows.iter().map(|r| r.ended_at).collect();
        assert_eq!(ended, [3, 4, 2, 1]);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 4);
        let again = History::open(&dir);
        assert_eq!(again.rows.iter().map(|r| r.ended_at).collect::<Vec<_>>(), [3, 4, 2, 1]);
        assert_eq!(again.file_lines, 4);
        // Past twice the limit the file keeps its last lines: with four lines
        // already there, the 1997th call takes it to 2001, and three follow.
        for n in 0..2 * HISTORY_LIMIT as u64 {
            history.add(&dir, vec![call(100 + n)]).unwrap();
        }
        assert_eq!(history.file_lines, HISTORY_LIMIT + 3);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), HISTORY_LIMIT + 3);
        assert_eq!(history.rows.len(), HISTORY_LIMIT);
        assert_eq!(history.rows[0].ended_at, 100 + 2 * HISTORY_LIMIT as u64 - 1);
        history.clear(&dir).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert!(history.rows.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn rows_that_could_not_be_written_wait_for_the_next_add() {
        let dir = std::env::temp_dir().join(format!("ksip-history-pending-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A folder where the file should be: every append fails.
        std::fs::create_dir_all(dir.join(HISTORY_FILE)).unwrap();
        let mut history = History::open(&dir);
        let row = |n: u64| CallHistory { ended_at: n, direction: "OUTGOING".into(), peer: format!("sip:{n}@pbx"), name: String::new(), duration: 1, recording: String::new(), outcome: String::new() };
        assert!(history.add(&dir, vec![row(1)]).is_err());
        assert_eq!(history.rows.len(), 1, "the tab shows the row all the same");
        assert_eq!(history.pending.len(), 1, "the row waits for the file");
        // The way is clear again: what waited is written with the next row.
        std::fs::remove_dir(dir.join(HISTORY_FILE)).unwrap();
        assert!(history.add(&dir, vec![row(2)]).is_ok());
        assert!(history.pending.is_empty());
        let reread = History::open(&dir);
        assert_eq!(reread.rows.iter().map(|r| r.ended_at).collect::<Vec<_>>(), vec![2, 1], "both rows, newest first");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
