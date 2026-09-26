//! The phone's call state and the rules that move it: what the engine
//! reports, what the history gets out of it, which call is recorded or
//! answered on its own. One holder owns the instance (today `AppState`
//! behind one mutex; the actor later), and nothing outside reads or writes
//! the bookkeeping directly: reports and events go in through methods, the
//! calls to show and the rows to write come out of them.
use crate::engine::{CallHistory, CallInfo};
use crate::message::message;
use std::collections::HashMap;

/// What is known about one call beyond what the engine reports about it.
#[derive(Clone, Default)]
struct CallRecord {
    /// The history direction, once it is known.
    direction: Option<String>,
    /// The line the window shows the call on.
    line: Option<u8>,
    /// The recording file the call was put in, if it was recorded.
    recording: Option<String>,
    /// The engine's closing words, until the history has taken them.
    closing: Option<String>,
    /// The engine announced the call (CALL_OUTGOING / CALL_INCOMING) with this
    /// history direction and peer; kept until the call shows up in a report,
    /// or makes its row without ever doing so.
    announced: Option<(String, String)>,
    /// Whether the automatic answer already claimed the call.
    auto_answered: bool,
}

/// What one report from the engine changed.
pub struct Applied {
    /// The calls as the window shows them, with their lines.
    pub calls: Vec<CallInfo>,
    /// The history rows for the calls that ended since the last report, in
    /// the order the tab shows them.
    pub ended: Vec<CallHistory>,
    /// The incoming calls the automatic answer takes this time.
    pub answer: Vec<String>,
}

#[derive(Default)]
pub struct PhoneState {
    /// The calls as of the last report, lines assigned.
    calls: Vec<CallInfo>,
    records: HashMap<String, CallRecord>,
    /// Which engine process the calls belong to. A report from an earlier
    /// generation is stale and changes nothing.
    generation: u64,
    /// Something that needs the phone quiet (a settings change, a device
    /// calibration) is going on. Begun only while no call is up. Wired in
    /// with the actor (step 2); until then only the rule and its test exist.
    #[allow(dead_code)]
    maintenance: bool,
}

impl PhoneState {
    /// A new engine process: the calls of the old one are gone with it, and
    /// its reports, should any still arrive, are told apart by generation.
    pub fn new_engine(&mut self) -> u64 {
        self.calls.clear();
        self.records.clear();
        self.generation += 1;
        self.generation
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    #[allow(dead_code)] // The actor reads the calls from here (step 2).
    pub fn calls(&self) -> &[CallInfo] {
        &self.calls
    }
    /// The engine's closing words for a call, kept for its history row.
    pub fn note_closed(&mut self, id: &str, reason: &str) {
        self.records.entry(id.to_string()).or_default().closing = Some(reason.to_string());
    }
    /// The engine announced a call, before any report shows it.
    pub fn note_announced(&mut self, id: &str, direction_code: &str, peer: &str) {
        self.records.entry(id.to_string()).or_default().announced = Some((message(direction_code), peer.to_string()));
    }
    /// A call was dialled from this line; the report that follows shows it there.
    pub fn note_dialled(&mut self, id: &str, line: u8) {
        self.records.entry(id.to_string()).or_default().line = Some(line);
    }
    /// A call went into this recording file, so its history row can point at it.
    pub fn note_recording(&mut self, id: &str, file: &str) {
        self.records.entry(id.to_string()).or_default().recording = Some(file.to_string());
    }
    /// Maintenance (a settings change, a calibration) starts only while no
    /// call is up, and marks the phone so until it ends.
    #[allow(dead_code)] // Wired in with the actor (step 2).
    pub fn begin_maintenance(&mut self) -> Result<(), ()> {
        if !self.calls.is_empty() {
            return Err(());
        }
        self.maintenance = true;
        Ok(())
    }
    #[allow(dead_code)]
    pub fn end_maintenance(&mut self) {
        self.maintenance = false;
    }
    #[allow(dead_code)]
    pub fn in_maintenance(&self) -> bool {
        self.maintenance
    }
    /// Takes one report of the engine's calls. Returns what changed, or None
    /// for a report of an earlier engine, which is ignored whole.
    pub fn apply_report(&mut self, generation: u64, mut calls: Vec<CallInfo>, dnd: bool, auto_answer: bool, now: u64) -> Option<Applied> {
        if generation != self.generation {
            return None;
        }
        let present = |records: &HashMap<String, CallRecord>, id: &str| records.contains_key(id);
        let _ = present;
        for call in &calls {
            let record = self.records.entry(call.id.clone()).or_default();
            if call.state == "INCOMING" {
                record.direction = Some(message("HISTORY_INCOMING"));
            } else if matches!(call.state.as_str(), "OUTGOING" | "RINGING" | "EARLY") {
                record.direction.get_or_insert_with(|| message("HISTORY_OUTGOING"));
            }
        }
        // Calls in the last report and not in this one have ended.
        let mut ended: Vec<CallHistory> = Vec::new();
        for old in &self.calls {
            if calls.iter().any(|call| call.id == old.id) {
                continue;
            }
            let record = self.records.remove(&old.id).unwrap_or_default();
            ended.push(CallHistory {
                ended_at: now,
                direction: record.direction.unwrap_or_else(|| message("HISTORY_CALL")),
                peer: old.peer.clone(),
                name: old.name.clone(),
                duration: old.duration,
                recording: record.recording.unwrap_or_default(),
                outcome: call_outcome(&old.state, old.state == "INCOMING", record.closing.as_deref().unwrap_or(""), dnd),
            });
        }
        // A call that came and went between two reports was never seen in
        // one: an outgoing call answered with an error at once, an incoming
        // one that stopped ringing within the interval, or one refused under
        // do not disturb. Its announcement and closing words make its row.
        let previous: Vec<String> = self.calls.iter().map(|c| c.id.clone()).collect();
        let current: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
        let mut instant: Vec<String> = self
            .records
            .iter()
            .filter(|(id, record)| record.announced.is_some() && record.closing.is_some() && !previous.contains(id) && !current.contains(id))
            .map(|(id, _)| id.clone())
            .collect();
        instant.sort();
        for id in instant {
            let record = self.records.remove(&id).unwrap_or_default();
            let (direction, peer) = record.announced.unwrap_or_default();
            let incoming = direction == message("HISTORY_INCOMING");
            ended.push(CallHistory {
                ended_at: now,
                direction,
                peer,
                name: String::new(),
                duration: 0,
                recording: String::new(),
                outcome: call_outcome(if incoming { "INCOMING" } else { "OUTGOING" }, incoming, record.closing.as_deref().unwrap_or(""), dnd),
            });
        }
        // What is kept: the records of calls in this report, and announcements
        // of calls not yet seen whose closing has not come. A record of a call
        // that was seen and has gone made its row above.
        self.records.retain(|id, record| {
            current.contains(id) || (!previous.contains(id) && record.announced.is_some() && record.closing.is_none())
        });
        // Lines: a call keeps the line it has; a new one takes the lowest free.
        for call in &mut calls {
            let taken: Vec<u8> = self.records.values().filter_map(|r| r.line).collect();
            let record = self.records.entry(call.id.clone()).or_default();
            let line = *record.line.get_or_insert_with(|| (1..=8).find(|i| !taken.contains(i)).unwrap_or(8));
            call.line = line;
        }
        // The automatic answer takes each incoming call once; it is claimed
        // here, so that the next report cannot answer it twice.
        let answer: Vec<String> = if auto_answer {
            calls
                .iter()
                .filter(|call| call.state == "INCOMING" && !self.records.get(&call.id).is_some_and(|r| r.auto_answered))
                .map(|call| call.id.clone())
                .collect()
        } else {
            Vec::new()
        };
        for id in &answer {
            self.records.entry(id.clone()).or_default().auto_answered = true;
        }
        self.calls = calls.clone();
        Some(Applied { calls, ended, answer })
    }
    /// The engine is gone without being asked: the calls of the last report
    /// get their rows (their closing words never came), and the bookkeeping
    /// is emptied.
    pub fn engine_gone(&mut self, dnd: bool, now: u64) -> Vec<CallHistory> {
        let rows = self
            .calls
            .iter()
            .map(|old| {
                let record = self.records.get(&old.id).cloned().unwrap_or_default();
                CallHistory {
                    ended_at: now,
                    direction: record.direction.unwrap_or_else(|| message("HISTORY_CALL")),
                    peer: old.peer.clone(),
                    name: old.name.clone(),
                    duration: old.duration,
                    recording: record.recording.unwrap_or_default(),
                    outcome: call_outcome(&old.state, old.state == "INCOMING", record.closing.as_deref().unwrap_or(""), dnd),
                }
            })
            .collect();
        self.calls.clear();
        self.records.clear();
        rows
    }
}

/// Names what happened to a call that closed without being talked on, from
/// the state it was in and the engine's closing words (a SIP answer such
/// as `486 Busy Here`, or a note of its own). An incoming call this phone
/// answered with `Busy Here` itself was refused under do not disturb, even
/// if the switch has been turned off since.
pub fn call_outcome(state: &str, incoming: bool, reason: &str, dnd: bool) -> String {
    if state == "ESTABLISHED" {
        return String::new();
    }
    if incoming {
        return if dnd || reason.starts_with("Busy Here") {
            message("HISTORY_REFUSED")
        } else if answered_elsewhere(reason) {
            message("HISTORY_ELSEWHERE")
        } else {
            message("HISTORY_MISSED")
        };
    }
    let code: u16 = reason.split_whitespace().next().and_then(|c| c.parse().ok()).unwrap_or(0);
    match code {
        486 | 600 => message("HISTORY_BUSY"),
        // 604 says the number exists nowhere (RFC 3261), which is what a caller
        // hears as "no such number", the same as 404; 3CX answers it for one.
        404 | 484 | 604 => message("HISTORY_NOT_FOUND"),
        480 | 502 | 503 => message("HISTORY_UNAVAILABLE"),
        403 | 603 => message("HISTORY_DECLINED"),
        408 => message("HISTORY_NO_ANSWER"),
        0 | 487 => message("HISTORY_CANCELLED"),
        _ => message("HISTORY_FAILED"),
    }
}
/// Whether the closing words say another phone took the call. A PBX that
/// cancels a group ring because someone else answered puts a `Reason`
/// header (RFC 3326) on the CANCEL, `SIP;cause=200` or `Q.850;cause=26`,
/// and baresip appends that header after a comma. Only the numbers count;
/// the text is free-form.
fn answered_elsewhere(reason: &str) -> bool {
    reason.split(',').any(|part| {
        let mut fields = part.split(';').map(str::trim);
        let protocol = fields.next().unwrap_or("").to_ascii_uppercase();
        let cause = fields
            .map(str::to_ascii_lowercase)
            .find_map(|f| f.strip_prefix("cause=").and_then(|c| c.trim().parse::<u16>().ok()));
        matches!((protocol.as_str(), cause), ("SIP", Some(200)) | ("Q.850", Some(26)))
    })
}
/// The call the automatic recording follows: the established, unheld call on
/// the lowest line, or none.
pub fn automatic_recording_target(enabled: bool, calls: &[CallInfo]) -> Option<String> {
    enabled
        .then(|| {
            calls
                .iter()
                .filter(|call| call.state == "ESTABLISHED" && !call.held)
                .min_by_key(|call| call.line)
                .map(|call| call.id.clone())
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, state: &str, peer: &str) -> CallInfo {
        CallInfo {
            id: id.into(),
            peer: peer.into(),
            name: String::new(),
            state: state.into(),
            held: false,
            duration: 0,
            codec: String::new(),
            secure: false,
            transport: "UDP".into(),
            line: 0,
        }
    }

    #[test]
    fn a_call_that_rings_and_ends_between_reports_still_makes_its_row_once() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        // Announced and closed with no report in between: a missed call.
        phone.note_announced("i1", "HISTORY_INCOMING", "sip:1003@pbx.example");
        phone.note_closed("i1", "connection reset [108],SIP;cause=487");
        // Announced and closed by our own 486: refused under do not disturb.
        phone.note_announced("i2", "HISTORY_INCOMING", "sip:1004@pbx.example");
        phone.note_closed("i2", "Busy Here");
        // An outgoing call answered with an error at once.
        phone.note_announced("o1", "HISTORY_OUTGOING", "sip:1002@pbx.example");
        phone.note_closed("o1", "486 Busy Here");
        let applied = phone.apply_report(g, vec![], false, false, 10).expect("current generation");
        let outcomes: Vec<(String, String)> = applied.ended.iter().map(|r| (r.peer.clone(), r.outcome.clone())).collect();
        assert_eq!(outcomes, vec![
            ("sip:1003@pbx.example".into(), message("HISTORY_MISSED")),
            ("sip:1004@pbx.example".into(), message("HISTORY_REFUSED")),
            ("sip:1002@pbx.example".into(), message("HISTORY_BUSY")),
        ]);
        assert!(applied.ended.iter().all(|r| r.ended_at == 10));
        // Nothing is left behind, and the next report makes no second row.
        assert!(phone.records.is_empty());
        assert!(phone.apply_report(g, vec![], false, false, 11).unwrap().ended.is_empty());
    }

    #[test]
    fn a_call_seen_in_a_report_makes_its_row_when_it_goes_and_keeps_its_line() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        phone.note_announced("o1", "HISTORY_OUTGOING", "sip:1002@pbx.example");
        phone.note_dialled("o1", 2);
        phone.note_recording("o1", "2026-09-26_23-00-00_1002.wav");
        let a = phone.apply_report(g, vec![call("o1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 20).unwrap();
        assert_eq!(a.calls[0].line, 2, "the line chosen for the dial");
        assert!(a.ended.is_empty());
        // A second call takes the lowest free line, which is 1.
        let a = phone.apply_report(g, vec![call("o1", "ESTABLISHED", "sip:1002@pbx.example"), call("i9", "INCOMING", "sip:1005@pbx.example")], false, false, 21).unwrap();
        assert_eq!(a.calls.iter().map(|c| c.line).collect::<Vec<_>>(), vec![2, 1]);
        // The first ends normally, the incoming one is answered elsewhere.
        phone.note_closed("i9", "connection reset [108],SIP;cause=200;text=\"Call completed elsewhere\"");
        let a = phone.apply_report(g, vec![], false, false, 30).unwrap();
        let rows: Vec<(String, String, String)> = a.ended.iter().map(|r| (r.peer.clone(), r.outcome.clone(), r.recording.clone())).collect();
        assert_eq!(rows, vec![
            ("sip:1002@pbx.example".into(), String::new(), "2026-09-26_23-00-00_1002.wav".into()),
            ("sip:1005@pbx.example".into(), message("HISTORY_ELSEWHERE"), String::new()),
        ]);
        assert!(phone.records.is_empty(), "nothing lingers once the calls have gone");
    }

    #[test]
    fn a_report_from_an_earlier_engine_changes_nothing() {
        let mut phone = PhoneState::default();
        let old = phone.new_engine();
        phone.apply_report(old, vec![call("c1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 1).unwrap();
        let new = phone.new_engine();
        assert!(phone.calls().is_empty(), "a new engine starts with no calls");
        assert!(phone.apply_report(old, vec![call("c1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 2).is_none());
        assert!(phone.calls().is_empty());
        assert!(phone.apply_report(new, vec![], false, false, 3).is_some());
    }

    #[test]
    fn the_automatic_answer_takes_each_incoming_call_once() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        let ringing = vec![call("i1", "INCOMING", "a"), call("i2", "INCOMING", "b")];
        assert!(phone.apply_report(g, ringing.clone(), false, false, 1).unwrap().answer.is_empty(), "off: nobody is answered");
        assert_eq!(phone.apply_report(g, ringing.clone(), false, true, 2).unwrap().answer, vec!["i1", "i2"]);
        assert!(phone.apply_report(g, ringing, false, true, 3).unwrap().answer.is_empty(), "claimed calls are not answered twice");
    }

    #[test]
    fn an_engine_that_dies_leaves_rows_for_the_calls_that_were_up() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        phone.note_dialled("o1", 1);
        phone.note_recording("o1", "rec.wav");
        phone.apply_report(g, vec![call("o1", "ESTABLISHED", "sip:1002@pbx.example"), call("i1", "INCOMING", "sip:1003@pbx.example")], false, false, 5).unwrap();
        let rows = phone.engine_gone(false, 9);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].recording.as_str(), rows[0].outcome.as_str()), ("rec.wav", ""));
        assert_eq!(rows[1].outcome, message("HISTORY_MISSED"));
        assert!(phone.calls().is_empty() && phone.records.is_empty());
    }

    #[test]
    fn maintenance_begins_only_while_no_call_is_up() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        assert!(phone.begin_maintenance().is_ok());
        assert!(phone.in_maintenance());
        phone.end_maintenance();
        phone.apply_report(g, vec![call("c1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 1).unwrap();
        assert!(phone.begin_maintenance().is_err());
        assert!(!phone.in_maintenance());
    }

    #[test]
    fn how_a_call_ended_is_named() {
        assert_eq!(call_outcome("ESTABLISHED", false, "Connection reset", false), "");
        assert_eq!(call_outcome("INCOMING", true, "Busy Here", false), message("HISTORY_REFUSED"));
        assert_eq!(call_outcome("OUTGOING", false, "486 Busy Here", false), message("HISTORY_BUSY"));
        assert_eq!(call_outcome("RINGING", false, "404 Not Found", false), message("HISTORY_NOT_FOUND"));
        assert_eq!(call_outcome("OUTGOING", false, "604 Does Not Exist Anywhere", false), message("HISTORY_NOT_FOUND"));
        assert_eq!(call_outcome("OUTGOING", false, "480 Temporarily Unavailable", false), message("HISTORY_UNAVAILABLE"));
        assert_eq!(call_outcome("OUTGOING", false, "603 Decline", false), message("HISTORY_DECLINED"));
        assert_eq!(call_outcome("OUTGOING", false, "408 Request Timeout", false), message("HISTORY_NO_ANSWER"));
        assert_eq!(call_outcome("RINGING", false, "Rejected by user", false), message("HISTORY_CANCELLED"));
        assert_eq!(call_outcome("OUTGOING", false, "500 Server Internal Error", false), message("HISTORY_FAILED"));
        assert_eq!(call_outcome("INCOMING", true, "", false), message("HISTORY_MISSED"));
        assert_eq!(call_outcome("INCOMING", true, "", true), message("HISTORY_REFUSED"));
        assert_eq!(call_outcome("INCOMING", true, "connection reset [108],SIP;cause=200;text=\"Call completed elsewhere\"", false), message("HISTORY_ELSEWHERE"));
        assert_eq!(call_outcome("INCOMING", true, "connection reset [108],Q.850;cause=26", false), message("HISTORY_ELSEWHERE"));
        assert_eq!(call_outcome("INCOMING", true, "connection reset [108],SIP;cause=487;text=\"ORIGINATOR_CANCEL\"", false), message("HISTORY_MISSED"));
    }

    #[test]
    fn the_recording_follows_the_established_call_on_the_lowest_line() {
        let mut calls = vec![call("a", "ESTABLISHED", "x"), call("b", "ESTABLISHED", "y")];
        calls[0].line = 2;
        calls[1].line = 1;
        assert_eq!(automatic_recording_target(true, &calls).as_deref(), Some("b"));
        calls[1].held = true;
        assert_eq!(automatic_recording_target(true, &calls).as_deref(), Some("a"));
        assert!(automatic_recording_target(false, &calls).is_none());
        assert!(automatic_recording_target(true, &[call("c", "INCOMING", "z")]).is_none());
    }
}
