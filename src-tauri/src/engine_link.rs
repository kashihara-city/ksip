//! The engine process and the line to it: starting the process, the
//! control connection, its output, and stopping it. Everything the engine
//! says comes out as `LinkMessage`s on the actor's queue, in the order it
//! arrived, each tagged with this link's generation and its receive number.
//! The link keeps no phone state and changes none: what a message means for
//! the calls is the actor's to decide.
use crate::message::{message, message_with};
use crate::phone_message::{LinkBody, LinkMessage, Message};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
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

/// Everything a start needs, worked out before the process exists: the
/// executable, the profile folder with the config already written, and the
/// control port, held until the moment of the spawn so nothing else takes it.
pub struct StartPlan {
    pub exe: PathBuf,
    pub profile: PathBuf,
    pub control: TcpListener,
    pub credential_target: String,
}

/// How a stop went: whether the engine had to be ended, how long it took to
/// go, and what it exited with.
pub struct StopReport {
    pub forced: bool,
    pub took: Duration,
    pub code: Option<i32>,
}

pub struct EngineLink {
    generation: u64,
    child: Child,
    writer: TcpStream,
    threads: Vec<thread::JoinHandle<()>>,
    /// The receive counter, shared by the threads that deliver to the queue.
    seq: Arc<AtomicU64>,
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
        let ctrl = plan.control.local_addr().map_err(err)?.port();
        let profile = plan
            .profile
            .to_str()
            .ok_or(message("ENGINE_PROFILE_PATH_INVALID"))?
            .to_string();
        let seq = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();
        drop(plan.control);
        let mut child = Command::new(&plan.exe)
            .args(["--engine", &std::process::id().to_string(), "-f", &profile])
            .current_dir(plan.exe.parent().unwrap())
            .env("KSIP_CREDENTIAL_TARGET", &plan.credential_target)
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
        let writer = loop {
            if let Ok(socket) = TcpStream::connect((Ipv4Addr::LOCALHOST, ctrl)) {
                break socket;
            }
            if let Some(status) = child.try_wait().map_err(err)? {
                return Err(message_with("ENGINE_START_FAILED", [status]));
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(message("ENGINE_CONNECT_TIMEOUT"));
            }
            thread::sleep(Duration::from_millis(100));
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
            seq,
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
    pub fn stop(mut self) -> Result<StopReport, String> {
        let payload = serde_json::to_vec(&json!({"command":"quit","token":"quit"})).unwrap();
        let _ = write_netstring(&mut self.writer, &payload);
        let asked = Instant::now();
        let end = asked + Duration::from_secs(3);
        let mut forced = false;
        while self.child.try_wait().map_err(err)?.is_none() {
            if Instant::now() > end {
                let _ = self.child.kill();
                forced = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let took = asked.elapsed();
        let status = self.child.wait();
        self.close_threads();
        Ok(StopReport {
            forced,
            took,
            code: status.ok().and_then(|s| s.code()),
        })
    }
    /// The process has already ended: closes the connection and joins the threads.
    pub fn close(mut self) {
        self.close_threads();
    }
    /// After the control connection was lost: waits for the process to end,
    /// on a thread of its own, and reports `Exited` on the queue.
    pub fn watch_exit(mut self, sink: Sender<Message>) {
        thread::spawn(move || {
            let status = self.child.wait();
            self.close_threads();
            if let Ok(status) = status {
                deliver(&sink, self.generation, &self.seq, LinkBody::Exited(status));
            }
        });
    }
    fn close_threads(&mut self) {
        let _ = self.writer.shutdown(Shutdown::Both);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
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
}
