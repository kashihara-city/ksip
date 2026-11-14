//! The window is a browser (WebView2), and a browser makes requests of its
//! own: it fetches its configuration from its maker and asks whether its
//! parts have updates. KSIP talks to the PBX and to nothing else, so the
//! window's browser is told not to make those requests, and is given a proxy
//! that refuses whatever it sends all the same: a listener on this machine
//! that answers 403 to every connection and notes where it was meant to go.
//!
//! The browser's arguments are Chromium's, which WebView2 passes on without
//! promising that they stay; so the page asks once, at its start, for an
//! address that exists nowhere (PROBE_URL), and the app looks whether that
//! request came here (commands.rs, browser_probed). The page's own requests
//! never come: the app answers them before the browser's network is reached.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The host the page asks for to show that the proxy is in the way; `.invalid`
/// is never resolved (RFC 2606), so with the proxy ignored the request fails
/// at the name.
pub const PROBE_HOST: &str = "probe.ksip.invalid";
pub const PROBE_URL: &str = "https://probe.ksip.invalid/";
/// What wry passes when it is given no arguments (its webview2/mod.rs): they
/// are replaced, not added to, by arguments of the app's own.
const WRY_DEFAULTS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";
/// The browser's own requests, turned off at their source.
const QUIET: &str = "--disable-background-networking --disable-component-update";
/// How long a connection has to say where it wants to go.
const HEAR: Duration = Duration::from_secs(2);
/// How long it is waited on to close once it has been refused.
const LINGER: Duration = Duration::from_millis(300);
/// The head of a request is read no further than this.
const HEAD: usize = 8192;
/// Hosts remembered at most: what comes here is a handful of names.
const HOSTS: usize = 64;

/// The arguments the window's browser is started with: through the proxy on
/// `port`, or, when no port could be had, with every name unresolvable (which
/// stops a request by name, not one to an address, and cannot be checked).
pub fn browser_arguments(port: Option<u16>) -> String {
    match port {
        Some(port) => format!("{WRY_DEFAULTS} --proxy-server=127.0.0.1:{port} {QUIET}"),
        None => format!("{WRY_DEFAULTS} --host-resolver-rules=\"MAP * ~NOTFOUND\" --no-proxy-server {QUIET}"),
    }
}

#[derive(Default)]
struct Heard {
    /// Every host asked for since the start.
    ever: BTreeSet<String>,
    /// How often each was asked for since the last look (take_refused).
    fresh: BTreeMap<String, u32>,
}
#[derive(Clone)]
pub struct RefusingProxy {
    port: u16,
    heard: Arc<Mutex<Heard>>,
}
impl RefusingProxy {
    /// Listens on a port of this machine the system picks, for as long as the
    /// app runs.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let proxy = Self { port: listener.local_addr()?.port(), heard: Arc::default() };
        let heard = proxy.heard.clone();
        std::thread::Builder::new().name("refusing-proxy".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                refuse(stream, &heard);
            }
        })?;
        Ok(proxy)
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    /// Whether a request for the host has come here since the start.
    pub fn refused(&self, host: &str) -> bool {
        self.heard.lock().unwrap().ever.contains(host)
    }
    /// The hosts asked for since the last look, with how often, for the log;
    /// the page's probe is not among them.
    pub fn take_refused(&self) -> Vec<(String, u32)> {
        std::mem::take(&mut self.heard.lock().unwrap().fresh).into_iter().filter(|(host, _)| host != PROBE_HOST).collect()
    }
}

/// What the window's browser was started behind, kept for the page's check
/// (commands.rs, browser_probe and browser_probed).
pub struct BrowserGuard {
    proxy: Option<RefusingProxy>,
    /// What the last check found, so that the log has each finding once.
    found: Mutex<Option<bool>>,
}
impl BrowserGuard {
    pub fn new(proxy: Option<RefusingProxy>) -> Self {
        Self { proxy, found: Mutex::new(None) }
    }
    /// The address for the page to ask for, when there is a proxy for the
    /// request to come to.
    pub fn probe(&self) -> Option<&'static str> {
        self.proxy.as_ref().map(|_| PROBE_URL)
    }
    /// Whether the page's request came to the proxy, and, the first time and
    /// when that changes, a line for the log saying so.
    pub fn probed(&self) -> (bool, Option<String>) {
        let Some(proxy) = &self.proxy else {
            return (false, None);
        };
        let kept = proxy.refused(PROBE_HOST);
        let mut found = self.found.lock().unwrap();
        let line = (*found != Some(kept)).then(|| {
            if kept {
                format!("ksip: window's browser: its own requests are refused (the page's probe came to the proxy on 127.0.0.1:{})", proxy.port())
            } else {
                format!("ksip: window's browser: NOT kept from the network (the page's probe did not come to the proxy on 127.0.0.1:{})", proxy.port())
            }
        });
        *found = Some(kept);
        (kept, line)
    }
}

/// Notes where the connection was meant to go, then refuses it. The note is
/// made before the answer, so that whoever sees the refusal finds it noted.
fn refuse(mut stream: TcpStream, heard: &Mutex<Heard>) {
    let _ = stream.set_write_timeout(Some(HEAR));
    // The head of the request is heard out (its first line is what is used):
    // closing on a sender that has more to say resets the connection, and
    // the refusal is then lost on its way. The wait is for the whole head,
    // not for each piece of it, so that one that trickles keeps nobody else.
    let until = std::time::Instant::now() + HEAR;
    let mut head = Vec::new();
    let mut piece = [0u8; 512];
    while head.len() < HEAD && !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut piece) {
            Ok(n) if n > 0 => head.extend_from_slice(&piece[..n]),
            _ => break,
        }
    }
    let line = head.split(|b| *b == b'\n').next().unwrap_or_default();
    if let Some(host) = requested_host(&String::from_utf8_lossy(line)) {
        let mut heard = heard.lock().unwrap();
        if heard.ever.len() < HOSTS || heard.ever.contains(&host) {
            heard.ever.insert(host.clone());
            *heard.fresh.entry(host).or_default() += 1;
        }
    }
    let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
    // This side has said all it will; what the other still sends is dropped
    // for a moment, until it closes, so that the close is a close.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(LINGER));
    let mut dropped = 0;
    while dropped < HEAD {
        match stream.read(&mut piece) {
            Ok(n) if n > 0 => dropped += n,
            _ => break,
        }
    }
}

/// The host of a request line as a proxy gets it: `CONNECT host:port HTTP/1.1`
/// for a tunnel, `GET http://host[:port]/path HTTP/1.1` for a plain request.
/// Lower case, without the port; none for anything else.
fn requested_host(line: &str) -> Option<String> {
    let mut words = line.split_ascii_whitespace();
    let (method, target) = (words.next()?, words.next()?);
    let authority = if method.eq_ignore_ascii_case("CONNECT") {
        target
    } else {
        target.split_once("://")?.1.split(['/', '?', '#']).next()?
    };
    let authority = authority.rsplit('@').next()?;
    let host = match authority.strip_prefix('[') {
        // An IPv6 address in brackets, its port after them.
        Some(rest) => rest.split(']').next()?,
        None => authority.split(':').next()?,
    };
    let fine = !host.is_empty() && host.len() <= 253 && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'));
    fine.then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ask(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }
    #[test]
    fn every_request_is_refused_and_its_host_noted() {
        let proxy = RefusingProxy::start().unwrap();
        assert!(!proxy.refused(PROBE_HOST), "nothing asked yet");
        // A tunnel, as the browser asks for an https address.
        let tunnel = ask(proxy.port(), "CONNECT Config.Example.com:443 HTTP/1.1\r\nHost: config.example.com:443\r\n\r\n");
        assert!(tunnel.starts_with("HTTP/1.1 403 "), "{tunnel}");
        assert!(proxy.refused("config.example.com"), "noted by the time the refusal is read, in lower case and without the port");
        // A plain request, twice, and the page's probe.
        for _ in 0..2 {
            assert!(ask(proxy.port(), "GET http://update.example.com/check?x=1 HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403 "));
        }
        assert!(ask(proxy.port(), &format!("CONNECT {PROBE_HOST}:443 HTTP/1.1\r\n\r\n")).starts_with("HTTP/1.1 403 "));
        assert!(proxy.refused(PROBE_HOST));
        assert_eq!(
            proxy.take_refused(),
            vec![("config.example.com".to_string(), 1), ("update.example.com".to_string(), 2)],
            "what was asked for, with how often; the probe is the page's own and is left out"
        );
        assert!(proxy.take_refused().is_empty(), "each is told once");
        assert!(proxy.refused("update.example.com"), "what was asked for stays known");
    }
    #[test]
    fn a_connection_that_says_nothing_is_closed_and_the_next_is_served() {
        let proxy = RefusingProxy::start().unwrap();
        let started = std::time::Instant::now();
        let mut silent = TcpStream::connect((Ipv4Addr::LOCALHOST, proxy.port())).unwrap();
        let mut answer = String::new();
        silent.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 403 "), "refused all the same: {answer}");
        let waited = started.elapsed();
        assert!(waited + Duration::from_millis(200) >= HEAR && waited < HEAR * 3, "after the wait, not for ever: {waited:?}");
        assert!(ask(proxy.port(), "CONNECT next.example.com:443 HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403 "));
        assert_eq!(proxy.take_refused(), vec![("next.example.com".to_string(), 1)], "nothing noted for the silent one");
    }
    #[test]
    fn only_so_many_hosts_are_remembered() {
        let proxy = RefusingProxy::start().unwrap();
        for n in 0..HOSTS + 5 {
            ask(proxy.port(), &format!("CONNECT h{n}.example.com:443 HTTP/1.1\r\n\r\n"));
        }
        assert_eq!(proxy.take_refused().len(), HOSTS);
        ask(proxy.port(), "CONNECT h0.example.com:443 HTTP/1.1\r\n\r\n");
        assert_eq!(proxy.take_refused(), vec![("h0.example.com".to_string(), 1)], "a host already known is still counted");
    }
    #[test]
    fn the_check_says_whether_the_probe_came_and_tells_the_log_once() {
        let none = BrowserGuard::new(None);
        assert_eq!(none.probe(), None, "no proxy, nothing to ask for");
        assert_eq!(none.probed(), (false, None), "and nothing kept; the start has said why");
        let proxy = RefusingProxy::start().unwrap();
        let guard = BrowserGuard::new(Some(proxy.clone()));
        assert_eq!(guard.probe(), Some(PROBE_URL));
        let (kept, line) = guard.probed();
        assert!(!kept && line.as_deref().is_some_and(|l| l.contains("NOT kept from the network")), "{line:?}");
        assert_eq!(guard.probed(), (false, None), "the same finding is not said again");
        ask(proxy.port(), &format!("CONNECT {PROBE_HOST}:443 HTTP/1.1\r\n\r\n"));
        let (kept, line) = guard.probed();
        assert!(kept && line.as_deref().is_some_and(|l| l.contains("its own requests are refused") && l.contains(&proxy.port().to_string())), "{line:?}");
        assert_eq!(guard.probed(), (true, None));
    }
    #[test]
    fn the_host_is_read_from_the_request_line() {
        assert_eq!(requested_host("CONNECT edge.example.com:443 HTTP/1.1").as_deref(), Some("edge.example.com"));
        assert_eq!(requested_host("GET http://Example.com:8080/a/b?c HTTP/1.1").as_deref(), Some("example.com"));
        assert_eq!(requested_host("POST http://user:pw@example.com/x HTTP/1.1").as_deref(), Some("example.com"));
        assert_eq!(requested_host("CONNECT [2001:db8::1]:443 HTTP/1.1").as_deref(), Some("2001:db8::1"));
        assert_eq!(requested_host("CONNECT 192.0.2.10:443 HTTP/1.1").as_deref(), Some("192.0.2.10"), "an address is noted as it is");
        assert_eq!(requested_host("GET /path HTTP/1.1"), None, "not a request to a proxy");
        assert_eq!(requested_host("CONNECT <script>:443 HTTP/1.1"), None, "nothing but what a host name or an address holds");
        assert_eq!(requested_host(""), None);
    }
    #[test]
    fn the_browser_is_started_quiet_through_the_proxy_or_with_no_names() {
        let through = browser_arguments(Some(31471));
        assert_eq!(
            through,
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required \
             --proxy-server=127.0.0.1:31471 --disable-background-networking --disable-component-update"
        );
        let without = browser_arguments(None);
        assert!(without.starts_with(WRY_DEFAULTS) && without.ends_with(QUIET), "wry's own arguments and the quiet ones either way: {without}");
        assert!(without.contains("--host-resolver-rules=\"MAP * ~NOTFOUND\" --no-proxy-server") && !without.contains("--proxy-server=127"), "{without}");
    }
}
