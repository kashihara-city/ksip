//! The engine process and the line to it: starting the process, the
//! control connection, its output, and stopping it. Everything the engine
//! says comes out as `LinkMessage`s on the actor's queue, in the order it
//! arrived, each tagged with this link's generation and its receive number.
//! The link keeps no phone state and changes none: what a message means for
//! the calls is the actor's to decide.
use crate::message::{message, message_with};
use crate::phone_message::{LinkBody, LinkMessage, Message};
use crate::phone_state::{AudioProcessingStats, CallInfo, ParkingInfo, Transfer};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// What the engine answers to `ksip_state`.
#[derive(Deserialize)]
pub struct EngineReport {
    pub registration: String,
    #[serde(default)]
    pub dnd: bool,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub media_encryption: String,
    pub calls: Vec<CallInfo>,
    pub transfer: Transfer,
    #[serde(default)]
    pub parking: Vec<ParkingInfo>,
    #[serde(default)]
    pub audio_processing_stats: Option<AudioProcessingStats>,
    #[serde(default)]
    pub mwi_summary: String,
}
/// Everything a start needs, worked out before the process exists: the
/// executable, the profile folder with the config already written, and the
/// control port, held until the moment of the spawn so nothing else takes it.
pub struct StartPlan {
    pub exe: PathBuf,
    pub profile: PathBuf,
    /// The app's own listener for the engine's control connection, bound on
    /// the loopback address before the engine starts and held until the
    /// engine has connected: no other process can take the port meanwhile.
    pub control: TcpListener,
    pub credential_target: String,
    /// The secret this engine takes commands for: made for this start, handed
    /// to the process in its environment, said first on the control
    /// connection, and written nowhere else (not the profile, not the log).
    pub control_secret: String,
}

/// How an engine process ended. One of these exists only for a process that
/// has ended: `EngineLink::stop` returns Err rather than a report when it
/// could not confirm the end.
pub struct StopReport {
    /// It did not quit within three seconds and was killed.
    pub forced: bool,
    pub took: Duration,
    pub code: Option<i32>,
}

pub struct EngineLink {
    generation: u64,
    child: Child,
    writer: TcpStream,
    threads: Vec<thread::JoinHandle<()>>,
    /// Numbers the requests, so that an answer can be told apart from the
    /// others of this engine.
    serial: u64,
}

fn next(seq: &AtomicU64) -> u64 {
    seq.fetch_add(1, Ordering::Relaxed) + 1
}
fn deliver(sink: &Sender<Message>, generation: u64, seq: &AtomicU64, body: LinkBody) {
    let _ = sink.send(Message::Link(LinkMessage {
        generation,
        seq: next(seq),
        body,
    }));
}

impl EngineLink {
    /// Spawns the engine on the plan and connects to its control port. The
    /// threads started here deliver its output lines, its answers and its
    /// events to `sink`, and `Lost` when the connection ends.
    pub fn start(plan: StartPlan, generation: u64, sink: Sender<Message>) -> Result<Self, String> {
        let profile = plan
            .profile
            .to_str()
            .ok_or(message("ENGINE_PROFILE_PATH_INVALID"))?
            .to_string();
        let seq = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();
        let mut child = Command::new(&plan.exe)
            .args(["--engine", &std::process::id().to_string(), "-f", &profile])
            .current_dir(plan.exe.parent().unwrap())
            .env("KSIP_CREDENTIAL_TARGET", &plan.credential_target)
            .env("KSIP_CONTROL_SECRET", &plan.control_secret)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(0x08000000)
            .spawn()
            .map_err(err)?;
        for pipe in [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let (sink, seq) = (sink.clone(), seq.clone());
            threads.push(thread::spawn(move || {
                for line in BufReader::new(pipe).split(b'\n') {
                    let Ok(bytes) = line else {
                        break;
                    };
                    let text = without_colour(&String::from_utf8_lossy(&bytes)).trim().to_string();
                    deliver(&sink, generation, &seq, LinkBody::Log(text));
                }
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(12);
        let accepted = accept_engine(&plan.control, &plan.control_secret, deadline, || {
            child.try_wait().map(|status| status.map(|s| s.to_string())).map_err(err)
        });
        // The listener goes with the plan: no further connection is taken.
        drop(plan.control);
        let writer = match accepted {
            Ok(stream) => stream,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        writer
            .set_write_timeout(Some(Duration::from_secs(3)))
            .map_err(err)?;
        let mut reader = BufReader::new(writer.try_clone().map_err(err)?);
        let (s, q) = (sink.clone(), seq.clone());
        threads.push(thread::spawn(move || {
            while let Ok(data) = read_netstring(&mut reader) {
                let Ok(v) = serde_json::from_slice::<Value>(&data) else {
                    continue;
                };
                let body = match v.get("token").and_then(Value::as_str) {
                    Some(token) => LinkBody::Response {
                        token: token.to_string(),
                        value: v,
                    },
                    None if v.get("event").and_then(Value::as_bool) == Some(true) => LinkBody::Event(v),
                    None => continue,
                };
                deliver(&s, generation, &q, body);
            }
            deliver(&s, generation, &q, LinkBody::Lost);
        }));
        Ok(Self {
            generation,
            child,
            writer,
            threads,
            serial: 1,
        })
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Sends a request. The answer comes back on the queue as a `Response`
    /// with the token returned here; nothing is waited for.
    pub fn send(&mut self, command: &str, params: &str) -> Result<String, String> {
        let token = format!("{}-{}", self.generation, self.serial);
        self.serial += 1;
        let payload = serde_json::to_vec(&json!({"command":command,"params":params,"token":token}))
            .map_err(err)?;
        write_netstring(&mut self.writer, &payload)?;
        Ok(token)
    }
    /// Whether the process has ended on its own.
    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }
    /// Asks the engine to quit and gives it three seconds before ending it.
    /// The requests that should go first (stopping a recording, releasing
    /// the subscriptions) are the caller's, since they want answers.
    /// Ends the process: asked to quit, given three seconds, then killed.
    /// Ok is the word that the process has ended (the wait on it returned),
    /// and the link's threads are joined. Err means that could not be
    /// confirmed (its state could not be read, the kill or the wait failed):
    /// the caller must not take the files the engine had open as closed nor
    /// its ports as free, and gets the link back to try again.
    pub fn stop(mut self) -> Result<StopReport, Box<(EngineLink, String)>> {
        match self.end() {
            Ok(report) => {
                self.close_threads();
                Ok(report)
            }
            Err(e) => Err(Box::new((self, e))),
        }
    }
    fn end(&mut self) -> Result<StopReport, String> {
        let payload = serde_json::to_vec(&json!({"command":"quit","token":"quit"})).unwrap();
        let _ = write_netstring(&mut self.writer, &payload);
        let asked = Instant::now();
        let end = asked + Duration::from_secs(3);
        let mut forced = false;
        while self.child.try_wait().map_err(err)?.is_none() {
            if Instant::now() > end {
                self.child.kill().map_err(err)?;
                forced = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let took = asked.elapsed();
        let status = self.child.wait().map_err(err)?;
        Ok(StopReport { forced, took, code: status.code() })
    }
    /// The process has already ended: closes the connection and joins the threads.
    pub fn close(mut self) {
        self.close_threads();
    }
    fn close_threads(&mut self) {
        let _ = self.writer.shutdown(Shutdown::Both);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Waits on the app's own listener for the engine to connect and name the
/// secret in its first frame (`{"hello":"ksip_ctrl","secret":…}`), and
/// returns that connection. The app says nothing on a connection before it
/// has heard the secret, so the secret never goes to anyone; a connection
/// that says anything else, or nothing within two seconds, is closed and the
/// wait goes on. `exited` tells an engine that ended before connecting.
pub(crate) fn accept_engine(
    listener: &TcpListener,
    secret: &str,
    deadline: Instant,
    mut exited: impl FnMut() -> Result<Option<String>, String>,
) -> Result<TcpStream, String> {
    listener.set_nonblocking(true).map_err(err)?;
    loop {
        match listener.accept() {
            Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                // An accepted socket takes the listener's mode; the greeting
                // is read blocking, within its own time.
                stream.set_nonblocking(false).map_err(err)?;
                stream.set_read_timeout(Some(Duration::from_secs(2))).map_err(err)?;
                let greeted = read_netstring(&mut stream)
                    .ok()
                    .and_then(|frame| serde_json::from_slice::<Value>(&frame).ok())
                    .is_some_and(|hello| hello["hello"] == "ksip_ctrl" && same_secret(hello["secret"].as_str().unwrap_or(""), secret));
                if greeted {
                    stream.set_read_timeout(None).map_err(err)?;
                    return Ok(stream);
                }
                // Anyone else is closed as the stream drops.
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(err(e)),
        }
        if let Some(status) = exited()? {
            return Err(message_with("ENGINE_START_FAILED", [status]));
        }
        if Instant::now() > deadline {
            return Err(message("ENGINE_CONNECT_TIMEOUT"));
        }
        thread::sleep(Duration::from_millis(50));
    }
}
/// Compares the secret in time that does not depend on where it differs.
fn same_secret(said: &str, secret: &str) -> bool {
    said.len() == secret.len() && said.bytes().zip(secret.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}
/// The engine colours its warnings for a terminal; the log wants the words.
pub fn without_colour(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

pub fn read_netstring(r: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut header = String::new();
    loop {
        let mut b = [0];
        r.read_exact(&mut b).map_err(err)?;
        if b[0] == b':' {
            break;
        }
        if !b[0].is_ascii_digit() || header.len() >= 7 {
            return Err("Invalid IPC frame length".into());
        }
        header.push(b[0] as char);
    }
    if header.is_empty() || (header.len() > 1 && header.starts_with('0')) {
        return Err("Invalid IPC frame header".into());
    }
    let n: usize = header.parse().map_err(err)?;
    if n > 1024 * 1024 {
        return Err("IPC frame too large".into());
    }
    let mut buf = vec![0; n];
    r.read_exact(&mut buf).map_err(err)?;
    let mut end = [0];
    r.read_exact(&mut end).map_err(err)?;
    if end[0] != b',' {
        return Err("Invalid IPC frame terminator".into());
    }
    Ok(buf)
}
pub fn write_netstring(w: &mut impl Write, data: &[u8]) -> Result<(), String> {
    write!(w, "{}:", data.len()).map_err(err)?;
    w.write_all(data).map_err(err)?;
    w.write_all(b",").map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netstrings_are_byte_counted_and_sequential() {
        let mut wire = Vec::new();
        write_netstring(&mut wire, "日本語".as_bytes()).unwrap();
        write_netstring(&mut wire, b"{}").unwrap();
        let mut r = std::io::Cursor::new(wire);
        assert_eq!(read_netstring(&mut r).unwrap(), "日本語".as_bytes());
        assert_eq!(read_netstring(&mut r).unwrap(), b"{}");
    }
    #[test]
    fn reject_malformed_frames() {
        for data in [
            b"9999999:x,".as_slice(),
            b"01:a,",
            b"2:{};",
            b"x:{}",
            b"2:{",
        ] {
            assert!(read_netstring(&mut &data[..]).is_err());
        }
    }
    #[test]
    fn the_engine_colours_are_left_out_of_the_log() {
        assert_eq!(
            without_colour("\x1b[m\x1b[31mctrl_tcp: error processing command\x1b[;m"),
            "ctrl_tcp: error processing command"
        );
        assert_eq!(without_colour("plain [text]"), "plain [text]");
    }
    #[test]
    fn webrtc_audio_processing_statistics_are_parsed_from_native_state() {
        let state: EngineReport = serde_json::from_str(
            r#"{"registration":"OK","calls":[],"transfer":{"original":"","consultation":"","pending":false,"outcome":""},"audio_processing_stats":{"echo_return_loss":12.5,"echo_return_loss_enhancement":28.75,"residual_echo_likelihood":0.04,"render_rms_dbfs":-18.5,"capture_input_rms_dbfs":-24.0,"capture_output_rms_dbfs":-51.5,"delay_ms":84,"delay_median_ms":82,"delay_standard_deviation_ms":3,"stream_delay_ms":20,"stream_delay_from_device":true,"render_frames":1200,"capture_frames":1198,"render_errors":0,"capture_errors":0}}"#,
        )
        .unwrap();
        let stats = state.audio_processing_stats.unwrap();
        assert_eq!(stats.echo_return_loss, Some(12.5));
        assert_eq!(stats.echo_return_loss_enhancement, Some(28.75));
        assert_eq!(stats.delay_ms, Some(84));
        assert_eq!(stats.stream_delay_ms, 20);
        assert_eq!(stats.residual_echo_likelihood, Some(0.04));
        assert_eq!(stats.render_rms_dbfs, Some(-18.5));
        assert_eq!(stats.capture_input_rms_dbfs, Some(-24.0));
        assert_eq!(stats.capture_output_rms_dbfs, Some(-51.5));
        assert_eq!(stats.delay_median_ms, Some(82));
        assert_eq!(stats.delay_standard_deviation_ms, Some(3));
        assert!(stats.stream_delay_from_device);
        assert_eq!(stats.render_frames, 1200);
        assert_eq!(stats.capture_frames, 1198);
        assert_eq!(stats.render_errors + stats.capture_errors, 0);
    }
}

#[cfg(test)]
mod accept_tests {
    use super::*;
    use std::net::Ipv4Addr;
    fn greet(port: u16, payload: Value) -> TcpStream {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        write_netstring(&mut s, &serde_json::to_vec(&payload).unwrap()).unwrap();
        s
    }
    #[test]
    fn only_the_connection_that_names_the_secret_is_taken_and_the_app_says_nothing_first() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let secret = "0123456789abcdef";
        // Strangers first: a wrong secret, a wrong greeting, and one that says nothing.
        let mut wrong = greet(port, json!({"hello":"ksip_ctrl","secret":"fedcba9876543210"}));
        let mut other = greet(port, json!({"command":"quit"}));
        let silent = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let right = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            greet(port, json!({"hello":"ksip_ctrl","secret":"0123456789abcdef"}))
        });
        let mut taken = accept_engine(&listener, secret, Instant::now() + Duration::from_secs(10), || Ok(None)).expect("the engine is taken");
        let mut engine = right.join().unwrap();
        // The stranger heard nothing, and was closed.
        wrong.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 16];
        assert!(matches!(std::io::Read::read(&mut wrong, &mut buf), Ok(0) | Err(_)), "a stranger is closed without a word");
        other.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(std::io::Read::read(&mut other, &mut buf), Ok(0) | Err(_)));
        drop(silent);
        // The connection taken is the engine's.
        write_netstring(&mut taken, b"{\"command\":\"help\"}").unwrap();
        engine.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(read_netstring(&mut engine).unwrap(), b"{\"command\":\"help\"}");
    }
    #[test]
    fn an_engine_that_ends_or_never_connects_is_reported() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let ended = accept_engine(&listener, "s", Instant::now() + Duration::from_secs(5), || Ok(Some("exit code: 1".into())));
        assert_eq!(ended.err(), Some(message_with("ENGINE_START_FAILED", ["exit code: 1"])));
        let late = accept_engine(&listener, "s", Instant::now() + Duration::from_millis(200), || Ok(None));
        assert_eq!(late.err(), Some(message("ENGINE_CONNECT_TIMEOUT")));
    }
}
