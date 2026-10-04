//! The phone's call state and the rules that move it: what the engine
//! reports, what the history gets out of it, which call is recorded or
//! answered on its own. The actor owns the one instance, and nothing outside
//! reads or writes the bookkeeping directly: reports and events go in
//! through methods, the calls to show and the rows to write come out of
//! them. The shapes the window reads the phone in (the snapshot, a call, the
//! parking and message counters) are defined here too.
use crate::audio::Device;
use crate::history::CallHistory;
use crate::message::message;
use crate::settings::Settings;
use crate::storage::AccountView;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The endpoint a call's stream is on, as the engine opened it: its id, and
/// whether it is another than the device chosen (the default in its place
/// while that device is not there or would not start).
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Default)]
pub struct CallEndpoint {
    pub id: String,
    pub stand_in: bool,
}
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub running: bool,
    /// Whether the window is on screen. Off, the window leaves the microphone alone.
    pub window_visible: bool,
    pub recording: bool,
    pub recording_path: String,
    /// Recordings being turned into MP3 at the moment, so the window can say
    /// what the processor is busy with.
    #[serde(default)]
    pub converting: u32,
    pub error: String,
    pub settings: Settings,
    pub devices: Vec<Device>,
    pub log_sequence: u64,
    pub data_dir: String,
    pub aec_active: bool,
    /// The engine's detail log is running, as its last report said.
    #[serde(default)]
    pub detail_log_active: bool,
    pub transport: String,
    pub media_encryption: String,
    pub microphone_fallback: bool,
    /// The call's microphone is open in RAW mode (false: the device's effects
    /// process it first); none while no call has the device.
    #[serde(default)]
    pub microphone_raw: Option<bool>,
    /// The same for the speaker, while a call plays on it.
    #[serde(default)]
    pub speaker_raw: Option<bool>,
    /// The endpoint the call's microphone stream is on, as the engine
    /// reports it, while a call has the device; none otherwise (no call, the
    /// silence standing in, the engine gone). The window's volume, mute and
    /// meter go there while it is set, and to what the saved choice stands
    /// for otherwise (commands.rs, target).
    #[serde(default)]
    pub microphone_call: Option<CallEndpoint>,
    /// The same for the speaker, while a call plays on it.
    #[serde(default)]
    pub speaker_call: Option<CallEndpoint>,
    pub account: AccountView,
    pub calls: Vec<CallInfo>,
    pub transfer: Transfer,
    pub registration: String,
    /// Do not disturb: incoming calls are refused as busy while this is on.
    #[serde(default)]
    pub dnd: bool,
    /// The person asked to be unregistered. Automatic reconnects (a changed
    /// address, a device that came back) leave the phone that way; only a
    /// connect asked for, or a saved setting, registers again. Part of the
    /// phone's state, so that the window can say so too.
    #[serde(default)]
    pub unregistered_by_choice: bool,
    pub recording_call: String,
    pub history_sequence: u64,
    pub parking: Vec<ParkingInfo>,
    pub audio_processing_stats: Option<AudioProcessingStats>,
    #[serde(default)]
    pub mwi: Mwi,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CallInfo {
    pub id: String,
    pub peer: String,
    /// The caller's display name, when the call came with one.
    #[serde(default)]
    pub name: String,
    pub state: String,
    pub held: bool,
    pub duration: u32,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub line: u8,
}
/// What a recording is called: when it started, and whom the call was with,
/// so that the folder reads like the history. `2026-09-23_14-30-12_1002.wav`.
/// The number in a peer address: `sip:1001@pbx;x` and `<sip:1001@pbx>` give `1001`.
pub fn peer_number(peer: &str) -> String {
    let user = peer.trim().trim_start_matches('<');
    let user = user
        .strip_prefix("sip:")
        .or_else(|| user.strip_prefix("sips:"))
        .unwrap_or(user);
    user.split(['@', ';', '>']).next().unwrap_or("").to_string()
}
/// How a call's other end is named to the person: the caller's name in front
/// of the number when the call came with one, as the window shows it.
pub fn caller_label(call: &CallInfo) -> String {
    let number = peer_number(&call.peer);
    if call.name.is_empty() {
        number
    } else {
        format!("{} {}", call.name, number)
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ParkingInfo {
    pub number: String,
    pub state: String,
}
/// What the voicemail box reports through its message-summary subscription.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mwi {
    pub waiting: bool,
    pub new: u32,
    pub old: u32,
}
impl Mwi {
    /// Reads an RFC 3842 summary: `Messages-Waiting: yes` and
    /// `Voice-Message: 2/5 (0/1)`, new before old, urgent ones in brackets.
    pub fn parse(summary: &str) -> Self {
        let mut mwi = Self::default();
        for line in summary.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if name.eq_ignore_ascii_case("Messages-Waiting") {
                mwi.waiting = value.eq_ignore_ascii_case("yes");
            } else if name.eq_ignore_ascii_case("Voice-Message") {
                let counts = value.split_whitespace().next().unwrap_or_default();
                let (new, old) = counts.split_once('/').unwrap_or((counts, "0"));
                mwi.new = new.trim().parse().unwrap_or(0);
                mwi.old = old.trim().parse().unwrap_or(0);
            }
        }
        mwi
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioProcessingStats {
    pub echo_return_loss: Option<f64>,
    pub echo_return_loss_enhancement: Option<f64>,
    pub divergent_filter_fraction: Option<f64>,
    pub residual_echo_likelihood: Option<f64>,
    pub residual_echo_likelihood_recent_max: Option<f64>,
    pub render_rms_dbfs: Option<f64>,
    pub capture_device_rms_dbfs: Option<f64>,
    pub capture_mono_rms_dbfs: Option<f64>,
    pub capture_input_rms_dbfs: Option<f64>,
    pub capture_output_rms_dbfs: Option<f64>,
    pub agc_speech_level_dbfs: Option<f64>,
    pub agc_noise_level_dbfs: Option<f64>,
    pub agc_headroom_db: Option<f64>,
    pub agc_gain_db: Option<f64>,
    pub delay_ms: Option<i32>,
    pub delay_median_ms: Option<i32>,
    pub delay_standard_deviation_ms: Option<i32>,
    pub stream_delay_ms: u32,
    pub stream_delay_from_device: bool,
    pub render_frames: u64,
    pub capture_frames: u64,
    pub render_errors: u32,
    pub capture_errors: u32,
    pub capture_device_rate: u32,
    pub capture_device_channels: u32,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Transfer {
    pub original: String,
    pub consultation: String,
    pub pending: bool,
    pub outcome: String,
    /// Counts the outcomes set, so the window can show the same one again.
    #[serde(default)]
    pub outcome_seq: u64,
}

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
    /// The receive number of the last report applied. One received before
    /// it, however late it is looked at, changes nothing either.
    last_report: u64,
    /// Something that needs the phone quiet (a settings change, a device
    /// calibration) is going on: the number of the operation that began it,
    /// which alone can end it. Begun only while no call is up and nothing
    /// else holds it.
    maintenance: Option<u64>,
    next_maintenance: u64,
}
/// Why maintenance could not begin.
#[derive(Debug, PartialEq)]
pub enum MaintenanceRefused {
    CallInProgress,
    /// Another operation holds it: one whose worker is still out, for one.
    Held,
}

impl PhoneState {
    /// A new engine process: the calls of the old one are gone with it, and
    /// its reports, should any still arrive, are told apart by generation.
    pub fn new_engine(&mut self) -> u64 {
        self.calls.clear();
        self.records.clear();
        self.generation += 1;
        self.last_report = 0;
        self.generation
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn calls(&self) -> &[CallInfo] {
        &self.calls
    }
    /// The engine's closing words for a call, kept for its history row.
    pub fn note_closed(&mut self, id: &str, reason: &str) {
        self.records.entry(id.to_string()).or_default().closing = Some(reason.to_string());
    }
    /// The engine announced a call, before any report shows it. The direction
    /// is known from here on, whatever state the call is first seen in (the
    /// first report may already show it answered).
    pub fn note_announced(&mut self, id: &str, direction_code: &str, peer: &str) {
        let record = self.records.entry(id.to_string()).or_default();
        record.announced = Some((message(direction_code), peer.to_string()));
        record.direction.get_or_insert_with(|| message(direction_code));
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
    /// call is up and nobody holds it, and marks the phone so until the
    /// operation that began it ends it. Returns that operation's number.
    pub fn begin_maintenance(&mut self) -> Result<u64, MaintenanceRefused> {
        if self.maintenance.is_some() {
            return Err(MaintenanceRefused::Held);
        }
        if !self.calls.is_empty() {
            return Err(MaintenanceRefused::CallInProgress);
        }
        self.next_maintenance += 1;
        self.maintenance = Some(self.next_maintenance);
        Ok(self.next_maintenance)
    }
    /// Ends the maintenance, if `owner` is the operation that began it: true
    /// when it did end. Another operation's number ends nothing, so a
    /// maintenance held for a worker outlives the operations that come and
    /// go meanwhile.
    pub fn end_maintenance(&mut self, owner: u64) -> bool {
        if self.maintenance == Some(owner) {
            self.maintenance = None;
            true
        } else {
            false
        }
    }
    pub fn in_maintenance(&self) -> bool {
        self.maintenance.is_some()
    }
    /// Takes one report of the engine's calls, received as number `seq` of
    /// its engine's messages. Returns what changed, or None for a report of
    /// an earlier engine or one received before the last applied, which is
    /// ignored whole.
    pub fn apply_report(&mut self, generation: u64, seq: u64, mut calls: Vec<CallInfo>, dnd: bool, auto_answer: bool, now: u64) -> Option<Applied> {
        if generation != self.generation || seq <= self.last_report {
            return None;
        }
        self.last_report = seq;
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
    /// get their rows (their closing words never came), so do the calls it
    /// announced and never showed in a report (no report will come to make
    /// them), and the bookkeeping is emptied.
    pub fn engine_gone(&mut self, dnd: bool, now: u64) -> Vec<CallHistory> {
        let mut rows: Vec<CallHistory> = self
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
        let seen: Vec<&String> = self.calls.iter().map(|c| &c.id).collect();
        let mut unseen: Vec<(&String, &CallRecord)> =
            self.records.iter().filter(|(id, record)| record.announced.is_some() && !seen.contains(id)).collect();
        unseen.sort_by(|a, b| a.0.cmp(b.0));
        for (_, record) in unseen {
            let (direction, peer) = record.announced.clone().unwrap_or_default();
            let incoming = direction == message("HISTORY_INCOMING");
            rows.push(CallHistory {
                ended_at: now,
                direction,
                peer,
                name: String::new(),
                duration: 0,
                recording: String::new(),
                outcome: call_outcome(if incoming { "INCOMING" } else { "OUTGOING" }, incoming, record.closing.as_deref().unwrap_or(""), dnd),
            });
        }
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
        let applied = phone.apply_report(g, 10, vec![], false, false, 10).expect("current generation");
        let outcomes: Vec<(String, String)> = applied.ended.iter().map(|r| (r.peer.clone(), r.outcome.clone())).collect();
        assert_eq!(outcomes, vec![
            ("sip:1003@pbx.example".into(), message("HISTORY_MISSED")),
            ("sip:1004@pbx.example".into(), message("HISTORY_REFUSED")),
            ("sip:1002@pbx.example".into(), message("HISTORY_BUSY")),
        ]);
        assert!(applied.ended.iter().all(|r| r.ended_at == 10));
        // Nothing is left behind, and the next report makes no second row.
        assert!(phone.records.is_empty());
        assert!(phone.apply_report(g, 11, vec![], false, false, 11).unwrap().ended.is_empty());
    }

    #[test]
    fn a_call_seen_in_a_report_makes_its_row_when_it_goes_and_keeps_its_line() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        phone.note_announced("o1", "HISTORY_OUTGOING", "sip:1002@pbx.example");
        phone.note_dialled("o1", 2);
        phone.note_recording("o1", "2026-09-26_23-00-00_1002.wav");
        let a = phone.apply_report(g, 20, vec![call("o1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 20).unwrap();
        assert_eq!(a.calls[0].line, 2, "the line chosen for the dial");
        assert!(a.ended.is_empty());
        // A second call takes the lowest free line, which is 1.
        let a = phone.apply_report(g, 21, vec![call("o1", "ESTABLISHED", "sip:1002@pbx.example"), call("i9", "INCOMING", "sip:1005@pbx.example")], false, false, 21).unwrap();
        assert_eq!(a.calls.iter().map(|c| c.line).collect::<Vec<_>>(), vec![2, 1]);
        // The first ends normally, the incoming one is answered elsewhere.
        phone.note_closed("i9", "connection reset [108],SIP;cause=200;text=\"Call completed elsewhere\"");
        let a = phone.apply_report(g, 30, vec![], false, false, 30).unwrap();
        let rows: Vec<(String, String, String)> = a.ended.iter().map(|r| (r.peer.clone(), r.outcome.clone(), r.recording.clone())).collect();
        assert_eq!(rows, vec![
            ("sip:1002@pbx.example".into(), String::new(), "2026-09-26_23-00-00_1002.wav".into()),
            ("sip:1005@pbx.example".into(), message("HISTORY_ELSEWHERE"), String::new()),
        ]);
        assert!(phone.records.is_empty(), "nothing lingers once the calls have gone");
    }

    #[test]
    fn a_call_first_seen_answered_keeps_the_direction_it_was_announced_with() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        phone.note_announced("i1", "HISTORY_INCOMING", "sip:1003@pbx.example");
        // The first report already shows it answered, a state that says
        // nothing about the direction.
        phone.apply_report(g, 1, vec![call("i1", "ESTABLISHED", "sip:1003@pbx.example")], false, false, 1).unwrap();
        let a = phone.apply_report(g, 2, vec![], false, false, 2).unwrap();
        assert_eq!(a.ended.len(), 1);
        assert_eq!(a.ended[0].direction, message("HISTORY_INCOMING"));
    }

    #[test]
    fn an_engine_that_dies_before_a_report_still_makes_the_rows_of_what_it_announced() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        phone.apply_report(g, 1, vec![call("e1", "ESTABLISHED", "sip:1005@pbx.example")], false, false, 1).unwrap();
        // Announced after that report, and gone with the engine before the next.
        phone.note_announced("o1", "HISTORY_OUTGOING", "sip:1002@pbx.example");
        phone.note_announced("i1", "HISTORY_INCOMING", "sip:1003@pbx.example");
        phone.note_closed("i1", "connection reset [108],SIP;cause=487");
        let rows = phone.engine_gone(false, 9);
        let got: Vec<(String, String, String)> = rows.iter().map(|r| (r.peer.clone(), r.direction.clone(), r.outcome.clone())).collect();
        assert_eq!(got, vec![
            ("sip:1005@pbx.example".into(), message("HISTORY_CALL"), String::new()),
            ("sip:1003@pbx.example".into(), message("HISTORY_INCOMING"), message("HISTORY_MISSED")),
            ("sip:1002@pbx.example".into(), message("HISTORY_OUTGOING"), message("HISTORY_CANCELLED")),
        ]);
        assert!(rows.iter().all(|r| r.ended_at == 9));
        assert!(phone.records.is_empty() && phone.calls().is_empty(), "nothing lingers");
    }

    #[test]
    fn a_report_received_earlier_than_the_last_applied_changes_nothing() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        assert!(phone.apply_report(g, 5, vec![call("c1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 1).is_some());
        // Received as number 3, looked at after number 5: the call did not end.
        assert!(phone.apply_report(g, 3, vec![], false, false, 2).is_none());
        assert_eq!(phone.calls().len(), 1);
        assert!(phone.apply_report(g, 5, vec![], false, false, 3).is_none(), "the same report twice is once");
        let ended = phone.apply_report(g, 6, vec![], false, false, 4).unwrap().ended;
        assert_eq!(ended.len(), 1);
        // A new engine starts the count again.
        let g = phone.new_engine();
        assert!(phone.apply_report(g, 1, vec![], false, false, 5).is_some());
    }
    #[test]
    fn a_report_from_an_earlier_engine_changes_nothing() {
        let mut phone = PhoneState::default();
        let old = phone.new_engine();
        phone.apply_report(old, 1, vec![call("c1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 1).unwrap();
        let new = phone.new_engine();
        assert!(phone.calls().is_empty(), "a new engine starts with no calls");
        assert!(phone.apply_report(old, 2, vec![call("c1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 2).is_none());
        assert!(phone.calls().is_empty());
        assert!(phone.apply_report(new, 3, vec![], false, false, 3).is_some());
    }

    #[test]
    fn the_automatic_answer_takes_each_incoming_call_once() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        let ringing = vec![call("i1", "INCOMING", "a"), call("i2", "INCOMING", "b")];
        assert!(phone.apply_report(g, 1, ringing.clone(), false, false, 1).unwrap().answer.is_empty(), "off: nobody is answered");
        assert_eq!(phone.apply_report(g, 2, ringing.clone(), false, true, 2).unwrap().answer, vec!["i1", "i2"]);
        assert!(phone.apply_report(g, 3, ringing, false, true, 3).unwrap().answer.is_empty(), "claimed calls are not answered twice");
    }

    #[test]
    fn an_engine_that_dies_leaves_rows_for_the_calls_that_were_up() {
        let mut phone = PhoneState::default();
        let g = phone.new_engine();
        phone.note_dialled("o1", 1);
        phone.note_recording("o1", "rec.wav");
        phone.apply_report(g, 5, vec![call("o1", "ESTABLISHED", "sip:1002@pbx.example"), call("i1", "INCOMING", "sip:1003@pbx.example")], false, false, 5).unwrap();
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
        let owner = phone.begin_maintenance().expect("nothing is up");
        assert!(phone.in_maintenance());
        // Held: nobody else begins one, and nobody else ends it.
        assert_eq!(phone.begin_maintenance(), Err(MaintenanceRefused::Held));
        assert!(!phone.end_maintenance(owner + 1));
        assert!(phone.in_maintenance());
        assert!(phone.end_maintenance(owner));
        assert!(!phone.in_maintenance());
        phone.apply_report(g, 1, vec![call("c1", "ESTABLISHED", "sip:1002@pbx.example")], false, false, 1).unwrap();
        assert_eq!(phone.begin_maintenance(), Err(MaintenanceRefused::CallInProgress));
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
    #[test]
    fn caller_label_puts_the_name_before_the_number() {
        let mut call = CallInfo {
            id: "c".into(),
            peer: "sip:1001@192.0.2.10;transport=udp".into(),
            name: String::new(),
            state: "INCOMING".into(),
            held: false,
            duration: 0,
            codec: String::new(),
            secure: false,
            transport: "UDP".into(),
            line: 1,
        };
        assert_eq!(caller_label(&call), "1001");
        call.name = "部署名".into();
        assert_eq!(caller_label(&call), "部署名 1001");
        assert_eq!(peer_number("<sips:117@pbx>"), "117");
    }
    #[test]
    fn a_message_summary_is_read_into_counts() {
        let summary = "Messages-Waiting: yes\r\nMessage-Account: sip:1001@pbx\r\nVoice-Message: 2/5 (0/1)\r\n";
        assert_eq!(Mwi::parse(summary), Mwi { waiting: true, new: 2, old: 5 });
        assert_eq!(Mwi::parse("Messages-Waiting: no\r\nVoice-Message: 0/3\r\n"), Mwi { waiting: false, new: 0, old: 3 });
        assert_eq!(Mwi::parse(""), Mwi::default());
        assert_eq!(Mwi::parse("messages-waiting: YES\n"), Mwi { waiting: true, new: 0, old: 0 });
    }
}
