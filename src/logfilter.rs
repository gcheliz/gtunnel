//! Line classifier and filtering pump for supervised tunnel child processes.
//!
//! - OpenSSH invoked with `-o LogLevel=DEBUG` emits the lines required to verify
//!   port forwards and track active connections, but also thousands of internal
//!   channel bookkeeping lines (`debug1: channel free`, `debug1: kex`, etc.).
//! - `gcloud compute start-iap-tunnel` emits routine per-connection notices (`INFO: [N]`)
//!   and a known teardown traceback (`AttributeError: 'NoneType' object has no attribute 'close'`)
//!   when a probe or client disconnects.
//!
//! This module classifies and filters those lines, timestamping the kept lines.

use crate::loglevel::LogLevel;
use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// What one line from a child process is.
///
/// A predicate could say keep or drop; it could not pair an open with its
/// close, nor rewrite a line, which is what a connection log needs.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Drop,
    /// Logged as it is, minus ssh's `debug1: ` prefix.
    Keep(String),
    /// The local listener is up, on this port.
    Ready {
        port: u16,
    },
    Conn(ConnEvent),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ConnEvent {
    pub id: String,
    pub source: Option<SocketAddr>,
    pub phase: Phase,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Phase {
    Open,
    Close,
}

/// `127.0.0.1 port 59453` or `::1 port 5000`, starting at `text`.
fn parse_host_port(text: &str) -> Option<SocketAddr> {
    let mut words = text.split_whitespace();
    let host: IpAddr = words.next()?.parse().ok()?;
    if words.next()? != "port" {
        return None;
    }
    let digits: String = words
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some(SocketAddr::new(host, digits.parse().ok()?))
}

/// The port in `... port 8443.`, the way ssh ends its listening line.
fn trailing_port(text: &str) -> Option<u16> {
    let digits = text.trim_end().trim_end_matches('.').rsplit(' ').next()?;
    digits.parse().ok()
}

/// `[('127.0.0.1', 54321)]` and the IPv6 form `[('::1', 5001, 0, 0)]`, which
/// is what Python prints for the address of an accepted socket.
fn parse_python_addr(text: &str) -> Option<SocketAddr> {
    let rest = text.split_once("('")?.1;
    let (host, rest) = rest.split_once('\'')?;
    let port: String = rest
        .trim_start_matches(", ")
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some(SocketAddr::new(host.parse().ok()?, port.parse().ok()?))
}

fn ssh_channel(body: &str) -> Option<(&str, &str)> {
    let rest = body.strip_prefix("channel ")?;
    let (id, tail) = rest.split_once(": ")?;
    id.chars().all(|c| c.is_ascii_digit()).then_some((id, tail))
}

/// Classify one line of a child's output. Pure: pairing and counting belong
/// to [`Correlator`].
///
/// - `debug2:` and `debug3:` are dropped, as is every `debug1:` line except
///   the listener and the `direct-tcpip` channel `new`/`free` pair.
/// - gcloud's per-connection `INFO: [N]` lines become `Conn` events; its other
///   routine notices and the known WebSocket teardown lines are dropped.
/// - Everything else (errors, warnings, real tracebacks) is kept.
pub fn classify(line: &str) -> Line {
    let trimmed = line.trim();
    if trimmed.starts_with("debug2:") || trimmed.starts_with("debug3:") {
        return Line::Drop;
    }
    if let Some(body) = trimmed.strip_prefix("debug1: ") {
        return classify_ssh(body);
    }
    if trimmed.starts_with("debug1:") {
        return Line::Drop;
    }
    if let Some(port) = trimmed
        .strip_prefix("Listening on port [")
        .and_then(|r| r.split_once(']'))
        .and_then(|(p, _)| p.parse().ok())
    {
        return Line::Ready { port };
    }
    if let Some(rest) = trimmed.strip_prefix("INFO: [")
        && let Some((id, tail)) = rest.split_once("] ")
    {
        return classify_iap(id, tail);
    }
    if trimmed == "client_loop: send disconnect: Broken pipe" {
        return Line::Drop;
    }
    if trimmed.contains("iap_tunnel_lightweight_websocket")
        || trimmed.contains("iap_tunnel_websocket_helper")
        || trimmed.contains("AttributeError: 'NoneType' object has no attribute 'close'")
        || trimmed.contains("OSError: [Errno 9] Bad file descriptor")
        || trimmed.contains("self._wait_for_socket_to_ready")
        || trimmed.contains("self._websocket.run_forever")
        || trimmed.contains("self.sock.close()")
        || trimmed.contains("select.select([self.sock]")
        || trimmed.contains("During handling of the above exception, another exception occurred:")
    {
        return Line::Drop;
    }
    Line::Keep(trimmed.to_string())
}

fn classify_ssh(body: &str) -> Line {
    if body.contains("Local forwarding listening on")
        && let Some(port) = trailing_port(body)
    {
        return Line::Ready { port };
    }
    let Some((id, tail)) = ssh_channel(body) else {
        return Line::Drop;
    };
    // The source rides in the channel's name, which `free:` certainly prints
    // and `new` may not, so take it from whichever line has it.
    let source = tail
        .split_once("connect from ")
        .and_then(|(_, r)| parse_host_port(r));
    let phase = if tail.starts_with("new direct-tcpip") {
        Phase::Open
    } else if tail.starts_with("free: direct-tcpip") {
        Phase::Close
    } else {
        return Line::Drop;
    };
    Line::Conn(ConnEvent {
        id: format!("ssh-{id}"),
        source,
        phase,
    })
}

fn classify_iap(id: &str, tail: &str) -> Line {
    // -1 is gcloud testing the tunnel before it listens: not a client
    let numbered = id.parse::<i64>().ok().filter(|n| *n >= 0);
    if tail.starts_with("Connecting with URL") {
        return match numbered {
            Some(n) => Line::Conn(ConnEvent {
                id: format!("iap-{n}"),
                source: None,
                phase: Phase::Open,
            }),
            None => Line::Drop,
        };
    }
    if tail.starts_with("Client closed connection") {
        return match numbered {
            Some(n) => Line::Conn(ConnEvent {
                id: format!("iap-{n}"),
                source: parse_python_addr(tail),
                phase: Phase::Close,
            }),
            None => Line::Drop,
        };
    }
    // An ordinary close is `[9: 9]` and floods the log. IAP's own close codes
    // are 4xxx and say why a session was cut (4004 is a reauthentication), so
    // they stay: they are the only trace of a reconnect.
    if tail.contains("Received WebSocket Close message [4") {
        return Line::Keep(format!("INFO: [{id}] {tail}"));
    }
    if tail.contains("Received WebSocket Close")
        || tail.contains("Error while receiving from WebSocket")
        || tail.contains("Bad file descriptor")
    {
        return Line::Drop;
    }
    Line::Keep(format!("INFO: [{id}] {tail}"))
}

/// Whether a line is logged verbatim. What a traceback's own lines are held
/// to; connection events never occur inside one.
pub fn keep(line: &str) -> bool {
    matches!(classify(line), Line::Keep(_) | Line::Ready { .. })
}

/// How long an open waits for its close before it is believed. A health probe
/// connects and closes in well under this, and its source port is only known
/// at the close, so an open written at once would put every probe in the log.
const HOLD: Duration = Duration::from_secs(2);

/// Which transport carries a tunnel's clients. A jump tunnel runs ssh over
/// gcloud's IAP transport, so its stderr holds an IAP connection that is the
/// tunnel itself and never closes; only the ssh channels are clients there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    Ssh,
    Iap,
}

impl Backend {
    fn prefix(self) -> &'static str {
        match self {
            Backend::Ssh => "ssh-",
            Backend::Iap => "iap-",
        }
    }
}

/// At most `limit` connection lines per window; the rest are counted and
/// reported once when the window ends. A port scan or a client that reconnects
/// in a loop must not bury the lines that explain a failure.
struct RateCap {
    limit: u32,
    window: Duration,
    start: Option<Instant>,
    used: u32,
    dropped: u32,
}

impl RateCap {
    fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            start: None,
            used: 0,
            dropped: 0,
        }
    }

    /// Ends the window if it is over; the number it swallowed, if any.
    fn roll(&mut self, now: Instant) -> Option<u32> {
        let start = self.start?;
        if now.saturating_duration_since(start) < self.window {
            return None;
        }
        let dropped = self.dropped;
        *self = Self::new(self.limit, self.window);
        (dropped > 0).then_some(dropped)
    }

    fn admit(&mut self, now: Instant) -> bool {
        self.start.get_or_insert(now);
        if self.used < self.limit {
            self.used += 1;
            true
        } else {
            self.dropped += 1;
            false
        }
    }
}

struct OpenConn {
    at: Instant,
    source: Option<SocketAddr>,
    said: bool,
    probe: bool,
}

/// Pairs each connection's open with its close and writes the one normalised
/// line shape both backends share. One per supervisor, shared by its stdout and
/// stderr pumps, because ssh's output reaches the supervisor on gcloud's
/// stderr but gcloud's own lines may arrive on either.
pub struct Correlator {
    level: LogLevel,
    open: HashMap<String, OpenConn>,
    probes: Option<(PathBuf, u16)>,
    cap: RateCap,
    ready: Option<(PathBuf, Option<u16>)>,
    backend: Option<Backend>,
    last_stopping: Option<(Instant, crate::failure::Failure)>,
    auth_exit_window: Duration,
}

pub type SharedCorrelator = Arc<Mutex<Correlator>>;

impl Correlator {
    pub fn new(level: LogLevel) -> Self {
        Self {
            level,
            open: HashMap::new(),
            probes: None,
            cap: RateCap::new(
                crate::settings::LOGS_LINES_PER_MINUTE as u32,
                Duration::from_secs(60),
            ),
            ready: None,
            backend: None,
            last_stopping: None,
            auth_exit_window: crate::settings::LOGS_AUTH_EXIT_WINDOW,
        }
    }

    /// The rate cap and the credential window the user configured. Separate
    /// from `new` so the many tests that build a bare correlator keep the
    /// defaults, and only the supervisor, which owns the process, reads config.
    pub fn with_settings(mut self, settings: &crate::settings::Settings) -> Self {
        self.cap = RateCap::new(settings.logs_lines_per_minute, Duration::from_secs(60));
        self.auth_exit_window = settings.logs_auth_exit_window;
        self
    }

    /// Count only this transport's connections. Unset, as for a unit written
    /// before the flag existed, both are kept.
    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn share(self) -> SharedCorrelator {
        Arc::new(Mutex::new(self))
    }

    /// Where probes of this tunnel's port register themselves.
    pub fn with_probes(mut self, dir: PathBuf, port: u16) -> Self {
        self.probes = Some((dir, port));
        self
    }

    #[cfg(test)]
    pub fn with_cap(mut self, limit: u32, window: Duration) -> Self {
        self.cap = RateCap::new(limit, window);
        self
    }

    /// Write `file` when the listener for `port` (any port when `None`) is up.
    pub fn with_ready(mut self, file: PathBuf, port: Option<u16>) -> Self {
        self.ready = Some((file, port));
        self
    }

    fn is_probe(&self, source: Option<SocketAddr>) -> bool {
        match (&self.probes, source) {
            (Some((dir, port)), Some(src)) => crate::probe::is_probe_in(dir, *port, src.port()),
            _ => false,
        }
    }

    fn say(&mut self, out: &mut Vec<String>, text: String, now: Instant) {
        if let Some(n) = self.cap.roll(now) {
            out.push(format_line(&format!(
                "gtunnel: conn ... {n} more in the last minute"
            )));
        }
        if self.cap.admit(now) {
            out.push(format_line(&text));
        }
    }

    /// The last failure a restart cannot fix (an expired login, a denied
    /// impersonation, a bad key, a missing configuration) that a line within
    /// `logs.auth_exit_window` of `now` named. Read by the supervisor when
    /// gcloud exits: this sees both streams, so the answer does not depend on
    /// which one gcloud chose to complain on. A line from hours ago is not the
    /// reason for this exit: the user may have logged in again since and the
    /// tunnel died of the network. Only stop-class lines are kept, so a network
    /// line printed after the login one does not hide it.
    pub fn stopping_failure(&self, now: Instant) -> Option<crate::failure::Failure> {
        self.last_stopping
            .filter(|(at, _)| now.saturating_duration_since(*at) <= self.auth_exit_window)
            .map(|(_, failure)| failure)
    }

    /// One line in, the formatted lines it produces out.
    pub fn feed(&mut self, raw: &str, now: Instant) -> Vec<String> {
        let mut out = self.tick(now);
        if let Some(failure) = crate::failure::Failure::from_line(raw).filter(|f| f.is_stopping()) {
            self.last_stopping = Some((now, failure));
        }
        match classify(raw) {
            Line::Drop => {}
            Line::Keep(text) => out.push(format_line(&text)),
            Line::Ready { port } => {
                self.mark_ready(port);
                out.push(format_line(raw.trim()));
            }
            Line::Conn(ev) => {
                if self.level != LogLevel::Quiet {
                    self.conn(ev, now, &mut out);
                }
            }
        }
        out
    }

    fn mark_ready(&self, port: u16) {
        if let Some((file, want)) = &self.ready
            && want.is_none_or(|w| w == port)
        {
            let _ = crate::fsutil::write_atomic(file, &format!("{port}\n"));
        }
    }

    fn conn(&mut self, ev: ConnEvent, now: Instant, out: &mut Vec<String>) {
        if self.backend.is_some_and(|b| !ev.id.starts_with(b.prefix())) {
            return;
        }
        match ev.phase {
            Phase::Open => {
                let probe = self.is_probe(ev.source);
                self.open.insert(
                    ev.id,
                    OpenConn {
                        at: now,
                        source: ev.source,
                        said: false,
                        probe,
                    },
                );
            }
            Phase::Close => {
                let held = self.open.remove(&ev.id);
                let source = ev.source.or(held.as_ref().and_then(|h| h.source));
                if held.as_ref().is_some_and(|h| h.probe) || self.is_probe(source) {
                    return;
                }
                let from = source.map(|s| format!(" from {s}")).unwrap_or_default();
                if let Some(h) = &held
                    && !h.said
                {
                    self.say(out, open_text(&ev.id, h.source), now);
                }
                let after = held
                    .map(|h| {
                        format!(
                            " after {:.1}s",
                            now.saturating_duration_since(h.at).as_secs_f64()
                        )
                    })
                    .unwrap_or_default();
                self.say(
                    out,
                    format!("gtunnel: conn {}{from} closed{after}", ev.id),
                    now,
                );
            }
        }
    }

    /// Time passing: opens that outlived the hold are real, and a cap window
    /// that ended owes its summary.
    pub fn tick(&mut self, now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        let mut due: Vec<(String, Option<SocketAddr>)> = self
            .open
            .iter_mut()
            .filter(|(_, h)| !h.said && !h.probe && now.saturating_duration_since(h.at) >= HOLD)
            .map(|(id, h)| {
                h.said = true;
                (id.clone(), h.source)
            })
            .collect();
        due.sort();
        for (id, source) in due {
            self.say(&mut out, open_text(&id, source), now);
        }
        if let Some(n) = self.cap.roll(now) {
            out.push(format_line(&format!(
                "gtunnel: conn ... {n} more in the last minute"
            )));
        }
        out
    }

    /// The child is gone: say what is still open, and settle the cap.
    pub fn finish(&mut self) -> Vec<String> {
        self.tick(Instant::now() + HOLD + self.cap.window)
    }
}

fn open_text(id: &str, source: Option<SocketAddr>) -> String {
    match source {
        Some(s) => format!("gtunnel: conn {id} open from {s}"),
        None => format!("gtunnel: conn {id} open"),
    }
}

/// Format a kept log line by sanitizing control characters and prefixing an RFC 3339 timestamp.
pub fn format_line(line: &str) -> String {
    let clean = line
        .trim_start()
        .strip_prefix("debug1: ")
        .unwrap_or(line.trim_start());
    let sanitized: String = clean
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let stamp = humantime::format_rfc3339_seconds(std::time::SystemTime::now());
    format!("{stamp} {sanitized}\n")
}

/// A stateful filter that buffers Python tracebacks and discards known IAP teardown errors
/// while preserving genuine unhandled application tracebacks.
#[derive(Default)]
pub struct Filter {
    traceback_buf: Vec<String>,
    in_traceback: bool,
}

impl Filter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process_line(&mut self, line: &str, conn: &mut Correlator) -> Vec<String> {
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if self.in_traceback {
            if trimmed.starts_with("  File ")
                || trimmed.starts_with("    ")
                || trimmed.starts_with("Traceback ")
                || trimmed.starts_with("During handling of ")
                || trimmed.is_empty()
                || trimmed.starts_with("OSError:")
                || trimmed.starts_with("AttributeError:")
                || trimmed.starts_with("TypeError:")
                || trimmed.starts_with("ValueError:")
            {
                self.traceback_buf.push(trimmed.to_string());
                return Vec::new();
            }

            // End of traceback encountered
            self.in_traceback = false;
            let is_iap_teardown = self.traceback_buf.iter().any(|l| {
                l.contains("iap_tunnel_lightweight_websocket")
                    || l.contains("iap_tunnel_websocket_helper")
            });
            let mut out = Vec::new();
            if !is_iap_teardown {
                for tb_line in self.traceback_buf.drain(..) {
                    if keep(&tb_line) {
                        out.push(format_line(&tb_line));
                    }
                }
            } else {
                self.traceback_buf.clear();
            }
            out.extend(conn.feed(trimmed, Instant::now()));
            return out;
        }

        if trimmed == "Traceback (most recent call last):" {
            self.in_traceback = true;
            self.traceback_buf.clear();
            self.traceback_buf.push(trimmed.to_string());
            return Vec::new();
        }

        conn.feed(trimmed, Instant::now())
    }

    pub fn flush_eof(&mut self) -> Vec<String> {
        if self.in_traceback {
            self.in_traceback = false;
            let is_iap_teardown = self.traceback_buf.iter().any(|l| {
                l.contains("iap_tunnel_lightweight_websocket")
                    || l.contains("iap_tunnel_websocket_helper")
            });
            if !is_iap_teardown {
                return self
                    .traceback_buf
                    .drain(..)
                    .filter(|l| keep(l))
                    .map(|l| format_line(&l))
                    .collect();
            }
            self.traceback_buf.clear();
        }
        Vec::new()
    }
}

/// Read lines from an async reader, filter via [`Filter`], and write formatted lines to `out`.
///
/// `conn` is shared with the other stream of the same child, so a connection
/// opened on one and closed on the other still pairs.
///
/// A failing `out` (a full disk, a closed log) costs the output and nothing
/// else. Returning would drop the reader and close the child's pipe, and the
/// correlator would stop seeing an expired login, the ready line and the
/// probes, with nothing saying why. So the reading goes on, and the first
/// error is said once on stderr, which the service manager logs separately.
#[allow(clippy::print_stderr)]
pub async fn pump<R: AsyncBufRead + Unpin, W: Write>(
    mut reader: R,
    out: &mut W,
    conn: SharedCorrelator,
) -> std::io::Result<()> {
    let mut filter = Filter::new();
    let mut buf = Vec::new();
    let mut broken = false;
    let mut emit = |lines: Vec<String>, out: &mut W| {
        for formatted in lines {
            if broken {
                return;
            }
            let wrote = out
                .write_all(formatted.as_bytes())
                .and_then(|()| out.flush());
            if let Err(e) = wrote {
                broken = true;
                eprintln!("gtunnel: log output failed, dropping it: {e}");
            }
        }
    };
    loop {
        buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut buf).await?;
        if bytes_read == 0 {
            break;
        }
        let line_str = String::from_utf8_lossy(&buf);
        // The guard is dropped before the write: it is a std mutex and must
        // never be held across an await, and the write is blocking enough
        let lines = filter.process_line(&line_str, &mut lock(&conn));
        emit(lines, out);
    }
    emit(filter.flush_eof(), out);
    Ok(())
}

/// A poisoned lock means another pump panicked mid-line; the correlator's
/// maps are still sound, and a log writer must not die of someone else's panic.
pub fn lock(conn: &SharedCorrelator) -> std::sync::MutexGuard<'_, Correlator> {
    conn.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Failure;
    use crate::loglevel::LogLevel;
    use std::io::Cursor;
    use std::time::{Duration, Instant};

    #[test]
    fn an_old_credential_line_does_not_explain_a_later_exit() {
        let t0 = Instant::now();
        let mut c = Correlator::new(LogLevel::Info);
        assert_eq!(c.stopping_failure(t0), None);
        c.feed("ERROR: Reauthentication required.", t0);
        assert_eq!(
            c.stopping_failure(t0 + Duration::from_secs(5)),
            Some(Failure::LoginExpired)
        );
        assert_eq!(
            c.stopping_failure(
                t0 + crate::settings::LOGS_AUTH_EXIT_WINDOW + Duration::from_secs(1)
            ),
            None
        );
        // A fresh line renews it
        let later = t0 + Duration::from_secs(3600);
        c.feed("Please run: gcloud auth login", later);
        assert_eq!(
            c.stopping_failure(later + Duration::from_secs(1)),
            Some(Failure::LoginExpired)
        );
    }

    #[test]
    fn the_last_stop_class_line_names_the_failure_and_a_network_line_does_not_hide_it() {
        let t0 = Instant::now();
        let mut c = Correlator::new(LogLevel::Info);
        c.feed("ERROR: Failed to impersonate [x].", t0);
        assert_eq!(c.stopping_failure(t0), Some(Failure::ImpersonationDenied));
        // Not stop-class: a restart may well mend it, so it is not recorded
        c.feed(
            "curl: (6) Could not resolve host: oauth2.googleapis.com",
            t0,
        );
        assert_eq!(c.stopping_failure(t0), Some(Failure::ImpersonationDenied));
        // The newer of two stop-class lines is the reason
        c.feed(
            "ERROR: Reauthentication required.",
            t0 + Duration::from_secs(1),
        );
        assert_eq!(
            c.stopping_failure(t0 + Duration::from_secs(2)),
            Some(Failure::LoginExpired)
        );
    }

    #[test]
    fn drops_noisy_debug_lines() {
        assert!(!keep(
            "debug1: channel 200: free: direct-tcpip: listening port 8443"
        ));
        assert!(!keep("debug1: channel 2: new direct-tcpip"));
        assert!(!keep("debug1: kex: algorithm: curve25519-sha256"));
        assert!(!keep("debug1: Exit status 0"));
        assert!(!keep("debug2: fd 3 setting O_NONBLOCK"));
        assert!(!keep("debug3: send packet: type 1"));
    }

    #[test]
    fn keeps_the_forwarding_line_and_reads_no_connection_from_it() {
        assert!(keep(
            "debug1: Local forwarding listening on 127.0.0.1 port 8443."
        ));
        // OpenSSH prints this on every accept, and it says nothing the
        // `channel N: new` line does not, so it is dropped as a duplicate
        assert_eq!(
            classify("debug1: Connection to port 8443 forwarding to 10.0.0.10 port 443 requested."),
            Line::Drop
        );
    }

    // Lines built from the format strings in OpenSSH 10.3 and the gcloud SDK's
    // iap_tunnel.py, with placeholder hosts. The channel name's
    // `%s: listening port %d for %.100s port %d, connect from %.200s port %d to
    // %.100s port %d` is what `free:` prints; the `new` line prints the channel
    // type and may or may not repeat the name, so both shapes are fixtures.
    const SSH_NEW: &str =
        "debug1: channel 2: new direct-tcpip [direct-tcpip] (inactive timeout: 0)";
    const SSH_FREE: &str = "debug1: channel 2: free: direct-tcpip: listening port 8443 for 10.0.0.10 port 443, connect from 127.0.0.1 port 59453 to 127.0.0.1 port 8443, nchannels 3";
    const IAP_OPEN: &str = "INFO: [7] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect?project=example-dev-1a2b&port=22&zone=europe-west1-b&instance=bastion&interface=nic0']";
    const IAP_CLOSE: &str = "INFO: [7] Client closed connection from [('127.0.0.1', 54321)].";

    fn run(c: &mut Correlator, lines: &[&str], at: Instant) -> Vec<String> {
        lines.iter().flat_map(|l| c.feed(l, at)).collect()
    }

    fn bodies(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.split_once(' ')
                    .map(|x| x.1)
                    .unwrap_or("")
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_real_ssh_forward_yields_one_open_and_one_close_with_the_source_port() {
        let t0 = Instant::now();
        let mut c = Correlator::new(LogLevel::Info);
        let mut out = run(
            &mut c,
            &[
                "debug1: Connection to port 8443 forwarding to 10.0.0.10 port 443 requested.",
                SSH_NEW,
            ],
            t0,
        );
        // The open is held back so a probe's never reaches the log
        assert!(out.is_empty(), "{out:?}");
        out.extend(c.feed(SSH_FREE, t0 + Duration::from_millis(1500)));
        assert_eq!(
            bodies(&out),
            vec![
                "gtunnel: conn ssh-2 open",
                "gtunnel: conn ssh-2 from 127.0.0.1:59453 closed after 1.5s",
            ]
        );
    }

    #[test]
    fn an_ssh_new_line_that_names_the_source_puts_it_on_the_open() {
        let mut c = Correlator::new(LogLevel::Info);
        let t0 = Instant::now();
        let new = "debug1: channel 4: new direct-tcpip [direct-tcpip: listening port 8443 for 10.0.0.10 port 443, connect from 127.0.0.1 port 60001 to 127.0.0.1 port 8443] (inactive timeout: 0)";
        run(&mut c, &[new], t0);
        let out = c.tick(t0 + Duration::from_secs(3));
        assert_eq!(
            bodies(&out),
            vec!["gtunnel: conn ssh-4 open from 127.0.0.1:60001"]
        );
    }

    #[test]
    fn a_jump_tunnel_logs_no_conn_line_for_its_own_iap_transport() {
        let t0 = Instant::now();
        let mut c = Correlator::new(LogLevel::Info).with_backend(Backend::Ssh);
        let mut out = run(&mut c, &[IAP_OPEN, SSH_NEW], t0);
        out.extend(c.feed(SSH_FREE, t0 + Duration::from_millis(1000)));
        out.extend(c.tick(t0 + Duration::from_secs(5)));
        assert_eq!(
            bodies(&out),
            vec![
                "gtunnel: conn ssh-2 open",
                "gtunnel: conn ssh-2 from 127.0.0.1:59453 closed after 1.0s"
            ]
        );
        // The other way round: an IAP tunnel has no ssh channels of its own
        let mut c = Correlator::new(LogLevel::Info).with_backend(Backend::Iap);
        let out = run(&mut c, &[SSH_NEW, SSH_FREE], t0);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_close_with_no_open_still_logs() {
        let mut c = Correlator::new(LogLevel::Info);
        let out = run(&mut c, &[SSH_FREE], Instant::now());
        assert_eq!(
            bodies(&out),
            vec!["gtunnel: conn ssh-2 from 127.0.0.1:59453 closed"]
        );
    }

    #[test]
    fn a_connection_still_open_after_the_hold_is_logged_by_the_tick() {
        let mut c = Correlator::new(LogLevel::Info);
        let t0 = Instant::now();
        run(&mut c, &[IAP_OPEN], t0);
        assert!(c.tick(t0 + Duration::from_millis(500)).is_empty());
        let out = c.tick(t0 + Duration::from_secs(3));
        assert_eq!(bodies(&out), vec!["gtunnel: conn iap-7 open"]);
        // Once said, it is not said again
        assert!(c.tick(t0 + Duration::from_secs(4)).is_empty());
    }

    #[test]
    fn a_real_gcloud_session_yields_its_close_with_the_client_address() {
        let mut c = Correlator::new(LogLevel::Info);
        let t0 = Instant::now();
        let mut out = run(
            &mut c,
            &[
                "Testing if tunnel connection works.",
                "INFO: [-1] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect?project=example-dev-1a2b']",
                "Listening on port [2222].",
                "INFO: [-1] Client closed connection from [('127.0.0.1', 50000)].",
                IAP_OPEN,
            ],
            t0,
        );
        out.extend(c.feed(IAP_CLOSE, t0 + Duration::from_secs(12)));
        let b = bodies(&out);
        assert_eq!(b[0], "Testing if tunnel connection works.");
        assert_eq!(b[1], "Listening on port [2222].");
        // The id -1 pair is gcloud's own start-up self-test, not a client
        assert_eq!(
            &b[2..],
            [
                "gtunnel: conn iap-7 open",
                "gtunnel: conn iap-7 from 127.0.0.1:54321 closed after 12.0s"
            ]
        );
    }

    #[test]
    fn the_listening_line_is_readiness() {
        assert_eq!(
            classify("debug1: Local forwarding listening on 127.0.0.1 port 8443."),
            Line::Ready { port: 8443 }
        );
        assert_eq!(
            classify("Listening on port [15432]."),
            Line::Ready { port: 15432 }
        );
    }

    #[test]
    fn readiness_writes_the_ready_file_once_for_the_tunnels_own_port() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("com.gtunnel.dev.db.ready");
        let mut c = Correlator::new(LogLevel::Info).with_ready(file.clone(), Some(8443));
        c.feed("Listening on port [9999].", Instant::now());
        assert!(!file.exists(), "another port is not this tunnel");
        c.feed("Listening on port [8443].", Instant::now());
        assert_eq!(
            std::fs::read_to_string(&file).expect("ready").trim(),
            "8443"
        );
    }

    #[test]
    fn a_probe_connection_writes_no_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::probe::watch_probes(dir.path(), 8443).expect("watch");
        crate::probe::register_probe_in(dir.path(), 8443, 59453);
        let t0 = Instant::now();
        let mut c = Correlator::new(LogLevel::Info).with_probes(dir.path().to_path_buf(), 8443);
        let mut out = run(&mut c, &[SSH_NEW], t0);
        out.extend(c.feed(SSH_FREE, t0 + Duration::from_millis(40)));
        out.extend(c.tick(t0 + Duration::from_secs(5)));
        assert!(out.is_empty(), "{out:?}");

        // A different client on the same port is not a probe
        let mut c = Correlator::new(LogLevel::Info).with_probes(dir.path().to_path_buf(), 8443);
        let other = SSH_FREE.replace("59453", "59999");
        let out = c.feed(&other, t0);
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn a_burst_past_the_cap_becomes_one_summary_line() {
        let mut c = Correlator::new(LogLevel::Info).with_cap(5, Duration::from_secs(60));
        let t0 = Instant::now();
        let mut out = Vec::new();
        for n in 0..10 {
            out.extend(c.feed(&format!("INFO: [{n}] Connecting with URL ['x']"), t0));
            out.extend(c.feed(
                &format!(
                    "INFO: [{n}] Client closed connection from [('127.0.0.1', {})].",
                    40000 + n
                ),
                t0,
            ));
        }
        assert_eq!(out.len(), 5, "{out:?}");
        let out = c.tick(t0 + Duration::from_secs(61));
        assert_eq!(
            bodies(&out),
            vec!["gtunnel: conn ... 15 more in the last minute"]
        );
        assert!(c.tick(t0 + Duration::from_secs(130)).is_empty());
    }

    #[test]
    fn quiet_drops_connection_lines() {
        let mut c = Correlator::new(LogLevel::Quiet);
        let t0 = Instant::now();
        let mut out = run(&mut c, &[SSH_NEW, IAP_OPEN], t0);
        out.extend(c.feed(SSH_FREE, t0));
        out.extend(c.feed(IAP_CLOSE, t0));
        out.extend(c.tick(t0 + Duration::from_secs(9)));
        assert!(out.is_empty(), "{out:?}");
        // Failures still get through
        assert_eq!(c.feed("ERROR: failed to connect", t0).len(), 1);
    }

    #[test]
    fn an_ipv6_client_is_read_from_both_backends() {
        let ssh = classify("debug1: channel 3: free: direct-tcpip: listening port 8443 for db.internal port 443, connect from ::1 port 5000 to ::1 port 8443, nchannels 2");
        let Line::Conn(ev) = ssh else {
            panic!("{ssh:?}")
        };
        assert_eq!(ev.source, Some("[::1]:5000".parse().unwrap()));
        let iap = classify("INFO: [3] Client closed connection from [('::1', 5001, 0, 0)].");
        let Line::Conn(ev) = iap else {
            panic!("{iap:?}")
        };
        assert_eq!(ev.source, Some("[::1]:5001".parse().unwrap()));
    }

    #[test]
    fn unrelated_ssh_channels_are_not_connections() {
        assert_eq!(
            classify("debug1: channel 0: new session [client-session] (inactive timeout: 0)"),
            Line::Drop
        );
        assert_eq!(
            classify("debug1: channel 0: free: client-session, nchannels 1"),
            Line::Drop
        );
    }

    #[test]
    fn the_connection_line_formats_are_measured_in_cells() {
        let line = format_line("gtunnel: conn ssh-2 open");
        assert_eq!(
            line.chars().count(),
            "2026-10-27T01:50:00Z ".chars().count() + 25
        );
    }

    #[test]
    fn an_iap_close_code_is_kept_and_an_ordinary_close_is_not() {
        assert!(keep(
            "INFO: [3] Received WebSocket Close message [4004: 'reauthentication required']."
        ));
        assert!(!keep("INFO: [0] Received WebSocket Close message [9: 9]."));
    }

    // Characterization: these are kept today, and the hint rows rely on it
    #[test]
    fn reconnect_lines_are_kept() {
        assert!(keep(
            "INFO: [3] Reconnecting with URL ['wss://tunnel.cloudproxy.app/v4/reconnect?sid=x']"
        ));
        assert!(keep("WARNING: [3] Unable to reconnect within [900000] ms"));
    }

    #[test]
    fn keeps_gcloud_and_general_lines() {
        assert!(keep("Listening on port [15432]."));
        assert!(keep("WARNING: connection retry"));
        assert!(keep("ERROR: failed to connect"));
        assert!(keep("Traceback (most recent call last):"));
    }

    #[test]
    fn drops_routine_iap_info_lines() {
        assert!(!keep(
            "INFO: [0] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect...']"
        ));
        assert!(!keep(
            "INFO: [0] Client closed connection from [('127.0.0.1', 59102)]."
        ));
        assert!(!keep("INFO: [0] Received WebSocket Close message [9: 9]."));
        assert!(!keep("INFO: [0] Error while receiving from WebSocket."));
        assert!(!keep(
            "INFO: [31] Error [[Errno 9] Bad file descriptor] while sending to client."
        ));
    }

    #[test]
    fn drops_known_iap_websocket_teardown_lines() {
        assert!(!keep("  File \"/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_lightweight_websocket.py\", line 240, in run_forever"));
        assert!(!keep(
            "AttributeError: 'NoneType' object has no attribute 'close'"
        ));
        assert!(!keep("OSError: [Errno 9] Bad file descriptor"));
    }

    #[test]
    fn format_line_adds_timestamp_and_sanitizes() {
        let formatted = format_line("hello\tworld\n");
        assert!(formatted.ends_with("hello world \n"));
        // RFC3339 timestamp prefix like "2026-10-25T01:50:00Z "
        assert!(formatted.contains("Z hello"));
    }

    #[tokio::test]
    async fn pump_filters_and_formats_stream() {
        let input = "debug1: noise\ndebug1: Local forwarding listening on 127.0.0.1 port 8443.\ndebug2: more noise\nListening on port [15432].\n";
        let reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();

        pump(reader, &mut out, Correlator::new(LogLevel::Info).share())
            .await
            .expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(!output.contains("noise"));
        assert!(output.contains("Local forwarding listening on"));
        assert!(output.contains("Listening on port [15432]."));
    }

    struct FullDisk;
    impl Write for FullDisk {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("no space left on device"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // The pipe closing under a child is what hid an expired login from the
    // supervisor, so the proof is the last line, read after the first failure
    #[tokio::test]
    async fn a_pump_whose_writer_fails_keeps_reading_and_still_sees_expired_credentials() {
        let conn = Correlator::new(LogLevel::Info).share();
        let input = b"first line\nsecond line\nERROR: Reauthentication required.\n";
        let reader = tokio::io::BufReader::new(&input[..]);
        pump(reader, &mut FullDisk, conn.clone())
            .await
            .expect("a write error is not a read error");
        assert_eq!(
            lock(&conn).stopping_failure(std::time::Instant::now()),
            Some(Failure::LoginExpired)
        );
    }

    #[tokio::test]
    async fn pump_handles_partial_line_at_eof() {
        let input = "partial line without newline";
        let reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();

        pump(reader, &mut out, Correlator::new(LogLevel::Info).share())
            .await
            .expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(output.contains("partial line without newline"));
        assert!(output.ends_with('\n'));
    }

    #[tokio::test]
    async fn pump_handles_lossy_utf8() {
        let input = b"invalid \xff\xfe utf8\n";
        let reader = Cursor::new(&input[..]);
        let mut out = Vec::new();

        pump(reader, &mut out, Correlator::new(LogLevel::Info).share())
            .await
            .expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(output.contains("invalid"));
        assert!(output.contains('\u{FFFD}'));
    }

    #[tokio::test]
    async fn pump_drops_iap_websocket_teardown_tracebacks_and_info_lines() {
        let input = r#"Testing if tunnel connection works.
INFO: [-1] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect?project=example-dev-1a2b&port=22&newWebsocket=True&zone=europe-west1-b&instance=bastion&interface=nic0']
Listening on port [2222].
INFO: [-1] Error while receiving from WebSocket.
Traceback (most recent call last):
  File "/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_lightweight_websocket.py", line 240, in run_forever
    self._wait_for_socket_to_ready(timeout=WEBSOCKET_RETRY_TIMEOUT_SECS)
OSError: [Errno 9] Bad file descriptor

During handling of the above exception, another exception occurred:

Traceback (most recent call last):
  File "/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_websocket_helper.py", line 269, in _ReceiveFromWebSocket
    self._websocket.run_forever(origin=TUNNEL_CLOUDPROXY_ORIGIN, sslopt=self._sslopt)
  File "/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_lightweight_websocket.py", line 253, in run_forever
    self.close()
  File "/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_lightweight_websocket.py", line 194, in close
    self.sock.close()
AttributeError: 'NoneType' object has no attribute 'close'
INFO: [0] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect?project=example-dev-1a2b&port=22&newWebsocket=True&zone=europe-west1-b&instance=bastion&interface=nic0']
INFO: [0] Client closed connection from [('127.0.0.1', 59102)].
INFO: [0] Received WebSocket Close message [9: 9].
"#;
        let reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();
        pump(reader, &mut out, Correlator::new(LogLevel::Info).share())
            .await
            .expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(output.contains("Listening on port [2222]."));
        assert!(!output.contains("Traceback"));
        assert!(!output.contains("AttributeError"));
        assert!(!output.contains("INFO: ["));
    }

    #[tokio::test]
    async fn pump_preserves_real_exceptions() {
        let input = "Traceback (most recent call last):\n  File \"script.py\", line 10, in <module>\nValueError: invalid literal for int()\n";
        let reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();
        pump(reader, &mut out, Correlator::new(LogLevel::Info).share())
            .await
            .expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(output.contains("Traceback"));
        assert!(output.contains("ValueError: invalid literal for int()"));
    }
}
