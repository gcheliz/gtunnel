use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use crate::config::Config;
use crate::daemon;
use crate::gcp::monitoring::LbStats;
use crate::gcp::{monitoring, GcpClient};
use crate::state::{PersistentTunnel, TunnelSpec, TunnelStatus};

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const TCP_TIMEOUT: Duration = Duration::from_secs(2);
// How many minutes of load balancer traffic one metrics read covers
pub const LB_WINDOW_MINUTES: u32 = 10;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HealthStatus {
    #[default]
    Unknown,
    Healthy,
    Unhealthy,
    Checking,
}

impl HealthStatus {
    pub fn symbol(&self) -> &'static str {
        match self {
            HealthStatus::Unknown => "?",
            HealthStatus::Healthy => "\u{2713}",
            HealthStatus::Unhealthy => "\u{2717}",
            HealthStatus::Checking => "\u{2026}",
        }
    }
}

// How long the far end takes to answer, or None when it does not answer at
// all. The local accept is no measurement: gcloud's listener accepts whether
// or not IAP, the bastion or the network are reachable, so the probe asks the
// far end for something and times the round trip it costs.
pub async fn tcp_probe(addr: &str, timeout: Duration) -> Option<Duration> {
    let connect = tokio::net::TcpStream::connect(addr);
    let mut stream = match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(stream)) => stream,
        _ => return None,
    };
    if let Ok(local) = stream.local_addr() {
        register_probe(addr, local.port());
    }
    close_politely(&mut stream, timeout).await
}

/// How long a probe's source port stays in the ledger. A probe is over in
/// seconds; a minute covers a slow close without letting a recycled port
/// excuse a real client for long.
const PROBE_TTL_SECS: u64 = 60;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where probes of local ports announce themselves: one file per target port,
/// next to the supervisors' ready files.
pub fn probes_dir() -> Option<std::path::PathBuf> {
    Some(crate::config::config_dir().ok()?.join("run").join("probes"))
}

/// The supervisor opens its port's ledger for business. A probe registers only
/// where this exists, so a probe of a port no supervisor watches (every test,
/// a tunnel gtunnel does not run) writes nothing anywhere.
pub fn watch_probes(dir: &std::path::Path, target_port: u16) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(target_port.to_string()), "")
}

pub fn unwatch_probes(dir: &std::path::Path, target_port: u16) {
    let _ = std::fs::remove_file(dir.join(target_port.to_string()));
}

// The connection's source port is only in the tunnel's log at the close, after
// the probe is long gone, so the probe leaves its own port where the
// supervisor can find it. Written before the half-close, so the entry exists
// before any close line can.
fn register_probe(addr: &str, src_port: u16) {
    let Ok(sock) = addr.parse::<std::net::SocketAddr>() else {
        return;
    };
    if sock.ip() != std::net::Ipv4Addr::LOCALHOST {
        return;
    }
    if let Some(dir) = probes_dir() {
        register_probe_in(&dir, sock.port(), src_port);
    }
}

/// Append `src_port` to the ledger of `target_port`, dropping entries past
/// their time to live. Read-modify-write without a lock: two probes landing in
/// the same instant can lose one entry, which costs one extra `conn` line and
/// nothing else.
pub fn register_probe_in(dir: &std::path::Path, target_port: u16, src_port: u16) {
    let file = dir.join(target_port.to_string());
    let Ok(text) = std::fs::read_to_string(&file) else {
        return;
    };
    let now = unix_now();
    let mut kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            l.split_once(' ')
                .and_then(|(_, t)| t.parse::<u64>().ok())
                .is_some_and(|t| t + PROBE_TTL_SECS >= now)
        })
        .collect();
    let entry = format!("{src_port} {now}");
    kept.push(&entry);
    let _ = crate::fsutil::write_atomic(&file, &(kept.join("\n") + "\n"));
}

/// Whether `src_port` is a recent probe of `target_port`.
pub fn is_probe_in(dir: &std::path::Path, target_port: u16, src_port: u16) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(target_port.to_string())) else {
        return false;
    };
    let now = unix_now();
    text.lines().any(|l| {
        l.split_once(' ').is_some_and(|(p, t)| {
            p.parse::<u16>().ok() == Some(src_port)
                && t.parse::<u64>().is_ok_and(|t| t + PROBE_TTL_SECS >= now)
        })
    })
}

// A socket dropped while the peer's greeting is still unread closes with RST,
// and so does one closed just before a greeting still in flight arrives. Either
// way the peer reports a client error: gcloud logs a traceback for every probe
// and the tunnel's log fills with them. Half-close so the peer sees a FIN, then
// read until its own close arrives. Returns how long the far end took to say
// anything back, which is the round trip the probe reports.
async fn close_politely(stream: &mut tokio::net::TcpStream, budget: Duration) -> Option<Duration> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if stream.shutdown().await.is_err() {
        return None;
    }
    let started = std::time::Instant::now();
    let mut answered = None;
    let drain = async {
        // Anything from the far end is its answer: a greeting, or its own close
        // relayed back. Read on to EOF so nothing is left unread.
        let mut scratch = [0u8; 1024];
        while let Ok(read) = stream.read(&mut scratch).await {
            answered.get_or_insert_with(|| started.elapsed());
            if read == 0 {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(budget, drain).await;
    answered
}

// A 4xx still proves the request reached the service; only 5xx and a failure
// to connect mean the tunnel is not carrying traffic
pub async fn http_probe(url: &str) -> HealthStatus {
    let client = match reqwest::Client::builder().timeout(HTTP_TIMEOUT).build() {
        Ok(client) => client,
        Err(_) => return HealthStatus::Unknown,
    };
    match client.get(url).send().await {
        Ok(response) if response.status().is_server_error() => HealthStatus::Unhealthy,
        Ok(_) => HealthStatus::Healthy,
        Err(_) => HealthStatus::Unhealthy,
    }
}

/// How many clients an established-connection listing shows. Both ends of a
/// loopback connection carry the tunnel's port — the listener's accepted socket
/// and the client's own — and `lsof -iTCP:<port>` returns a row for each, so
/// counting rows reported one client as two. Only the row whose *local* side is
/// the port is the tunnel's end of it.
pub fn parse_lsof_established(text: &str, port: u16) -> u64 {
    let suffix = format!(":{port}");
    text.lines()
        .filter(|line| line.contains("ESTABLISHED"))
        .filter(|line| {
            line.split_whitespace()
                .find(|field| field.contains("->"))
                .and_then(|name| name.split("->").next())
                .is_some_and(|local| local.ends_with(&suffix))
        })
        .count() as u64
}

/// The same count as `parse_lsof_established`, for every port at once. The TUI
/// samples far more often than it renders, so one listing without a port
/// filter costs one process for the whole list rather than one per tunnel.
///
/// Keyed by the *local* port for the reason `parse_lsof_established` filters on
/// it: both ends of a loopback connection are in the listing, and only the row
/// whose local side is the tunnel's port is the tunnel's end of it. The client
/// ends land under their own ephemeral ports, which nobody looks up.
///
/// `exclude` leaves one process's sockets out, and the caller passes its own:
/// `tcp_probe` opens a connection to the very port being measured, and the
/// tunnel's end of that connection is ESTABLISHED on the tunnel's port.
/// `measure` keeps the probe out of its own count by reading before the probe
/// opens; a sampler on a clock of its own cannot, so it drops every row whose
/// peer is a socket of the excluded process instead.
pub fn parse_lsof_by_port(text: &str, exclude: Option<u32>) -> HashMap<u16, u64> {
    let rows: Vec<(&str, u32, &str, &str)> = established_rows(text).collect();
    let ours: std::collections::HashSet<&str> = match exclude {
        Some(pid) => rows
            .iter()
            .filter(|(_, owner, ..)| *owner == pid)
            .map(|(_, _, local, _)| *local)
            .collect(),
        None => std::collections::HashSet::new(),
    };
    let mut counts = HashMap::new();
    for (_, owner, local, peer) in rows {
        if exclude == Some(owner) || ours.contains(peer) {
            continue;
        }
        if let Some(port) = endpoint_port(local) {
            *counts.entry(port).or_insert(0) += 1;
        }
    }
    counts
}

// Who owns each established connection, and its two ends. The pid is the first
// numeric field: a command with a space in its name is escaped (`Slack\x20`),
// so no field but COMMAND can precede it - which is also why COMMAND is simply
// the first field.
fn established_rows(text: &str) -> impl Iterator<Item = (&str, u32, &str, &str)> {
    text.lines()
        .filter(|line| line.contains("ESTABLISHED"))
        .filter_map(|line| {
            let command = line.split_whitespace().next()?;
            let pid = line
                .split_whitespace()
                .find_map(|f| f.parse::<u32>().ok())?;
            let (local, peer) = line
                .split_whitespace()
                .find(|field| field.contains("->"))?
                .split_once("->")?;
            Some((command, pid, local, peer))
        })
}

/// A process holding connections to one local port, and how many.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub command: String,
    pub count: u64,
}

/// Who is holding a tunnel's connections open, worst offender first.
///
/// The holder is read off the *client* row, not the server row: every server
/// row belongs to the one ssh or gcloud the tunnel runs, so attributing by
/// local port would only ever name the tunnel itself. A client's row has the
/// tunnel's port as its *peer*.
///
/// Empty when nothing can be attributed - no `lsof`, an unparseable listing,
/// or a client on another machine, whose row exists only on that machine.
/// Naming nobody is correct there; naming a guess would not be.
pub fn parse_lsof_holders(text: &str, port: u16, exclude: Option<u32>) -> Vec<Holder> {
    let mut by_pid: HashMap<u32, (String, u64)> = HashMap::new();
    for (command, pid, _, peer) in established_rows(text) {
        if exclude == Some(pid) || endpoint_port(peer) != Some(port) {
            continue;
        }
        let entry = by_pid
            .entry(pid)
            .or_insert_with(|| (command.to_string(), 0));
        entry.1 += 1;
    }
    let mut holders: Vec<Holder> = by_pid
        .into_iter()
        .map(|(pid, (command, count))| Holder {
            pid,
            command,
            count,
        })
        .collect();
    // Ties broken by pid so the order is the same on every run; a message
    // that names a different process each time is not evidence of anything
    holders.sort_by(|a, b| b.count.cmp(&a.count).then(a.pid.cmp(&b.pid)));
    holders
}

/// [`parse_lsof_holders`] against the machine, excluding this process.
///
/// The watchdog is its consumer and lands with the breaker; until then only
/// the parser above is exercised, which is where the judgement lives.
#[allow(dead_code)]
pub fn holders_for_port(port: u16) -> Vec<Holder> {
    lsof_listing()
        .map(|text| parse_lsof_holders(&text, port, Some(std::process::id())))
        .unwrap_or_default()
}

// `127.0.0.1:8443` and `[::1]:8443` both end in the port, so what follows the
// last colon is it either way
fn endpoint_port(endpoint: &str) -> Option<u16> {
    endpoint.rsplit(':').next()?.parse().ok()
}

/// Established connections per local port, for every tunnel in one process.
///
/// Empty when `lsof` is not there to ask, which is the Linux case: the caller
/// falls back to `active_connections`, whose own `ss` branch still answers per
/// port.
pub fn established_by_port() -> HashMap<u16, u64> {
    lsof_listing()
        .map(|text| parse_lsof_by_port(&text, Some(std::process::id())))
        .unwrap_or_default()
}

// Every established TCP connection on the machine, with no port filter: the
// filter saves lsof no work, and one listing is one process for the whole list.
//
// `+c 0` prints COMMAND whole. Without it lsof cuts the name to nine
// characters, so the process holding a tunnel's connections reads as
// `mcp-serve` - a name that matches nothing a person could then grep or kill.
// Widening the column does not disturb the counting parsers: a command with a
// space in it is escaped, so COMMAND is still one whitespace-separated field.
const LSOF_LISTING_ARGS: [&str; 5] = ["-nP", "+c", "0", "-iTCP", "-sTCP:ESTABLISHED"];

// A local tool that is missing, fails to start or outlasts its bound reads the
// same to every caller here: no answer, which each already treats as zero
// or `None`. The distinction is not worth a variant.
fn local_tool(cmd: std::process::Command, what: &str) -> Option<std::process::Output> {
    crate::proc::output_within_blocking(cmd, crate::proc::LOCAL_TOOL_BOUND, what).ok()
}

fn lsof_listing() -> Option<String> {
    let mut cmd = std::process::Command::new("lsof");
    cmd.args(LSOF_LISTING_ARGS);
    listing(&local_tool(cmd, "lsof")?)
}

// What a listing said, whatever it exited with. `lsof` exits 1 when nothing
// matched, and non-zero again when it merely warns about something it could
// not stat while still printing the rows it found: gating on the status
// discarded a real count.
fn listing(out: &std::process::Output) -> Option<String> {
    (!out.stdout.is_empty()).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn parse_ss_established(text: &str) -> u64 {
    text.lines()
        .filter(|line| {
            let first = line.split_whitespace().next().unwrap_or_default();
            first == "ESTAB" || first == "ESTABLISHED"
        })
        .count() as u64
}

// How many clients are using the tunnel right now
pub fn active_connections(port: u16) -> u64 {
    let mut lsof = std::process::Command::new("lsof");
    lsof.args(["-nP", &format!("-iTCP:{port}"), "-sTCP:ESTABLISHED"]);
    if let Some(out) = local_tool(lsof, "lsof")
        && let Some(text) = listing(&out)
    {
        return parse_lsof_established(&text, port);
    }
    let mut ss = std::process::Command::new("ss");
    ss.args(["-tnH", "state", "established", &format!("sport = :{port}")]);
    if let Some(out) = local_tool(ss, "ss")
        && let Some(text) = listing(&out)
    {
        // `ss -H` drops the header, so every row counts
        let rows = text.lines().filter(|l| !l.trim().is_empty()).count() as u64;
        return rows.max(parse_ss_established(&text));
    }
    0
}

// What the pane reports as `active`: the most clients the sampler saw since
// the last tick, because a reading taken once every five seconds lands between
// two short requests and reports nothing of a busy tunnel. A port the sampler
// has no figure for is read directly, so the first tick, and a machine with no
// `lsof`, are no worse than before there was a sampler.
fn connections_for(peaks: Option<&HashMap<u16, u64>>, port: u16) -> u64 {
    peaks
        .and_then(|p| p.get(&port).copied())
        .unwrap_or_else(|| active_connections(port))
}

// `lsof` and `ps` are processes that can hang on a wedged filesystem, and a
// metrics round awaits them: run on the blocking pool, so a stuck one holds a
// pool thread rather than a worker that every other poller shares.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

// `ps -o etime=` prints MM:SS, HH:MM:SS or D-HH:MM:SS
pub fn parse_etime(text: &str) -> Option<Duration> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [m, s] => (0, m.parse::<u64>().ok()?, s.parse::<u64>().ok()?),
        [h, m, s] => (
            h.parse::<u64>().ok()?,
            m.parse::<u64>().ok()?,
            s.parse::<u64>().ok()?,
        ),
        _ => return None,
    };
    Some(Duration::from_secs(
        days * 86_400 + hours * 3_600 + minutes * 60 + seconds,
    ))
}

pub fn process_uptime(pid: u32) -> Option<Duration> {
    let mut cmd = std::process::Command::new("ps");
    cmd.args(["-o", "etime=", "-p", &pid.to_string()]);
    let out = local_tool(cmd, "ps")?;
    if !out.status.success() {
        return None;
    }
    parse_etime(&String::from_utf8_lossy(&out.stdout))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TunnelMetrics {
    // How long the far end took to answer, not how long the local accept took
    pub latency: Option<Duration>,
    pub active_connections: u64,
    pub uptime: Option<Duration>,
    pub restarts: u32,
    pub lb: Option<LbStats>,
    pub available: bool,
}

pub async fn collect_metrics(
    cfg: &Config,
    t: &PersistentTunnel,
    include_lb: bool,
    peaks: Option<&HashMap<u16, u64>>,
) -> TunnelMetrics {
    let label = daemon::platform_label(&t.account_name, &t.name);
    let status = daemon::status(&label).await;
    let runtime = daemon::runtime_info(&label).await;
    measure(cfg, t, include_lb, status, runtime, peaks).await
}

// An unmanaged tunnel has no service to ask: discovery saw the process, and
// `ps` already gave the pid its uptime is read from. Nothing restarts it, so a
// restart count would be a number gtunnel invented.
pub async fn collect_unmanaged(
    cfg: &Config,
    t: &PersistentTunnel,
    pid: u32,
    peaks: Option<&HashMap<u16, u64>>,
) -> TunnelMetrics {
    let runtime = daemon::RuntimeInfo {
        pid: Some(pid),
        restarts: 0,
    };
    measure(cfg, t, false, TunnelStatus::Running, runtime, peaks).await
}

async fn measure(
    cfg: &Config,
    t: &PersistentTunnel,
    include_lb: bool,
    status: TunnelStatus,
    runtime: daemon::RuntimeInfo,
    peaks: Option<&HashMap<u16, u64>>,
) -> TunnelMetrics {
    let mut metrics = TunnelMetrics {
        restarts: runtime.restarts,
        uptime: match runtime.pid {
            Some(pid) => blocking(move || process_uptime(pid)).await.flatten(),
            None => None,
        },
        available: status == TunnelStatus::Running,
        ..TunnelMetrics::default()
    };

    match &t.spec {
        TunnelSpec::Access { local_port, .. } => {
            // Read before the probe opens its own socket, so the probe is
            // never one of the clients it reports
            let port = *local_port;
            let peaks = peaks.cloned();
            metrics.active_connections = blocking(move || connections_for(peaks.as_ref(), port))
                .await
                .unwrap_or(0);
            if let Some(rtt) = tcp_probe(&format!("127.0.0.1:{local_port}"), TCP_TIMEOUT).await {
                metrics.latency = Some(rtt);
            } else {
                metrics.available = false;
            }
        }
        TunnelSpec::Expose { hostname, .. } => {
            if include_lb && let Ok(acct) = cfg.get_account(Some(&t.account_name)) {
                let client = GcpClient::for_account(acct);
                let backend = crate::expose::naming::Names::new(&acct.name).backend(&t.name);
                metrics.lb = monitoring::lb_request_stats(
                    &client,
                    &acct.project,
                    &backend,
                    LB_WINDOW_MINUTES,
                )
                .await
                .ok();
            }
            let probe = http_probe(&format!("https://{hostname}")).await;
            metrics.available = metrics.available && probe == HealthStatus::Healthy;
        }
    }

    metrics
}

// A fixed-length window of samples, for the TUI's sparkline
#[derive(Debug, Clone, Default)]
pub struct MetricsHistory {
    capacity: usize,
    samples: VecDeque<u64>,
}

impl MetricsHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            samples: VecDeque::new(),
        }
    }

    pub fn record(&mut self, value: u64) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
    }

    pub fn samples(&self) -> Vec<u64> {
        self.samples.iter().copied().collect()
    }

    pub fn peak(&self) -> u64 {
        self.samples.iter().copied().max().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // A tunnel's own health check must not look like a client crash to the
    // process on the other end: a socket dropped with the peer's greeting
    // still unread closes with RST, and gcloud logs a traceback per probe
    #[tokio::test]
    async fn tcp_probe_leaves_the_peer_with_a_clean_eof() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            // Every SSH server greets before the client says anything
            sock.write_all(b"SSH-2.0-OpenSSH_10.0p2 Debian-7\r\n")
                .await
                .expect("banner");
            let mut buf = [0u8; 64];
            sock.read(&mut buf).await
        });

        assert!(
            tcp_probe(&addr, TCP_TIMEOUT).await.is_some(),
            "the probe did not connect"
        );

        let observed = server.await.expect("server task");
        match observed {
            Ok(0) => {}
            other => panic!("peer saw {other:?} instead of a clean EOF"),
        }
    }

    // Accepting a connection on the loopback port proves nothing: gcloud's
    // listener accepts whether or not IAP, the bastion or the network are
    // reachable. What the probe reports has to be the far end's own answer,
    // which costs a round trip through the tunnel
    #[tokio::test]
    async fn the_latency_reported_is_the_far_end_answering() {
        let measured = tcp_probe(&slow_greeter().await.to_string(), TCP_TIMEOUT)
            .await
            .expect("the peer greeted, so the probe has a measurement");
        assert!(
            measured >= Duration::from_millis(300),
            "reported {measured:?}, which is the local accept rather than the round trip"
        );
    }

    // A listener with nothing behind it accepts and then says nothing at all:
    // the case that reported `Health: OK Latency: 2 ms` for a port held by a
    // foreign process
    #[tokio::test]
    async fn a_peer_that_never_answers_is_not_a_working_tunnel() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let _held = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept");
            // Accepted and held open, saying nothing and never closing
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(sock);
        });

        assert_eq!(
            tcp_probe(&addr, Duration::from_millis(300)).await,
            None,
            "a port that answers nothing was reported as a live tunnel"
        );
    }

    // A silent service is still reachable: its close crossed the tunnel, so the
    // round trip is measurable even though no greeting ever came
    #[tokio::test]
    async fn a_peer_that_only_closes_still_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept");
            drop(sock);
        });

        assert!(
            tcp_probe(&addr, TCP_TIMEOUT).await.is_some(),
            "a reachable service that does not greet was reported as down"
        );
    }

    // A peer that greets a round trip after the probe half-closes, the way a
    // bastion behind an IAP tunnel does
    async fn slow_greeter() -> std::net::SocketAddr {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            // What an IAP tunnel to a bastion measures: ~260 ms to first byte
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = sock.write_all(b"SSH-2.0-OpenSSH_10.0p2 Debian-7\r\n").await;
        });
        addr
    }

    // Closing cleanly costs a round trip of its own, and the probe that can
    // afford to wait that long is the one that said how long it would wait: a
    // close that gives up first leaves the greeting arriving at a closed
    // socket, which answers with RST — the client error gcloud logs per probe
    #[tokio::test]
    async fn closing_waits_as_long_as_the_probe_was_given() {
        let mut patient = tokio::net::TcpStream::connect(slow_greeter().await)
            .await
            .expect("the probe did not connect");
        assert!(
            close_politely(&mut patient, TCP_TIMEOUT).await.is_some(),
            "closed before the peer's greeting and its own close arrived"
        );

        let mut hasty = tokio::net::TcpStream::connect(slow_greeter().await)
            .await
            .expect("the probe did not connect");
        assert!(
            close_politely(&mut hasty, Duration::from_millis(50))
                .await
                .is_none(),
            "a close given 50 ms reported an answer 300 ms away, so the budget is not the caller's"
        );
    }

    #[test]
    fn etime_parses_every_ps_format() {
        assert_eq!(parse_etime("05:06"), Some(Duration::from_secs(306)));
        assert_eq!(parse_etime("01:05:06"), Some(Duration::from_secs(3906)));
        assert_eq!(parse_etime("1-02:03:04"), Some(Duration::from_secs(93_784)));
        assert_eq!(parse_etime("  05:06  "), Some(Duration::from_secs(306)));
    }

    #[test]
    fn etime_rejects_what_it_cannot_read() {
        assert_eq!(parse_etime(""), None);
        assert_eq!(parse_etime("not-a-time"), None);
    }

    #[test]
    fn lsof_counts_only_established_rows() {
        let fixture = "\
gcloud 123 me 7u IPv4 0x1 0t0 TCP 127.0.0.1:15432->127.0.0.1:52001 (ESTABLISHED)
gcloud 123 me 8u IPv4 0x2 0t0 TCP 127.0.0.1:15432->127.0.0.1:52002 (ESTABLISHED)
gcloud 123 me 9u IPv4 0x3 0t0 TCP 127.0.0.1:15432->127.0.0.1:52003 (ESTABLISHED)
gcloud 123 me 6u IPv4 0x4 0t0 TCP 127.0.0.1:15432 (LISTEN)
";
        assert_eq!(parse_lsof_established(fixture, 15432), 3);
    }

    // Both ends of a loopback connection carry the same port, so `lsof -iTCP:8443`
    // returns two rows for one client and the count read double
    #[test]
    fn one_loopback_client_counts_once() {
        let real = "\
COMMAND   PID    USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
ssh      2146     you    8u  IPv4 0x3a6c8252f144aa71      0t0  TCP 127.0.0.1:8443->127.0.0.1:64306 (ESTABLISHED)
Python  12253     you    3u  IPv4 0xc78928f83ee41eb7      0t0  TCP 127.0.0.1:64306->127.0.0.1:8443 (ESTABLISHED)
";
        assert_eq!(parse_lsof_established(real, 8443), 1);
    }

    // A client on another machine reaching a tunnel bound to all interfaces has
    // only one row, and it is still one connection
    #[test]
    fn a_remote_client_counts_once() {
        let text = "ssh 2146 you 8u IPv4 0x1 0t0 TCP 10.0.0.2:8443->10.0.0.9:51234 (ESTABLISHED)\n";
        assert_eq!(parse_lsof_established(text, 8443), 1);
    }

    // One listing covers every tunnel on the machine, so the parser tells the
    // ports apart itself. Both loopback ends of a connection are in it, and
    // only the row whose *local* side is the tunnel's port is the tunnel's end
    #[test]
    fn one_listing_counts_every_port_by_its_local_side() {
        let counts = parse_lsof_by_port(TWO_TUNNELS, None);
        assert_eq!(
            counts.get(&8443).copied(),
            Some(2),
            "one IPv4 client and one IPv6 client reach the same tunnel"
        );
        assert_eq!(counts.get(&15432).copied(), Some(1));
        assert_eq!(
            counts.get(&5432).copied(),
            None,
            "a port that only listens carries no client"
        );
    }

    // gtunnel's own probe opens a connection to the tunnel it is measuring, and
    // the tunnel's end of that connection is ESTABLISHED on the tunnel's port.
    // `measure` reads the count before the probe opens so it never counts
    // itself; a sampler on its own clock cannot, so it drops whatever answers a
    // socket of this process instead
    #[test]
    fn a_sample_does_not_count_gtunnels_own_probe() {
        let probed = format!(
            "{TWO_TUNNELS}\
gtunnel  9100     you    9u  IPv4 0x7              0t0  TCP 127.0.0.1:52999->127.0.0.1:8443 (ESTABLISHED)
ssh      2146     you   11u  IPv4 0x8              0t0  TCP 127.0.0.1:8443->127.0.0.1:52999 (ESTABLISHED)
"
        );
        assert_eq!(
            parse_lsof_by_port(&probed, None).get(&8443).copied(),
            Some(3),
            "the listing has both ends of the probe's connection in it"
        );
        let counts = parse_lsof_by_port(&probed, Some(9100));
        assert_eq!(
            counts.get(&8443).copied(),
            Some(2),
            "the probe counted itself as a third client"
        );
        assert_eq!(
            counts.get(&52999).copied(),
            None,
            "the probe's own end of it is gone too"
        );
    }

    // What the sampler reads: two tunnels, each with a client of its own, plus
    // a port that only listens
    // One client holding many connections is the shape that exhausts a
    // tunnel's descriptors, so the answer has to name it, not just count it.
    // The holder is on the *client* row: every server row belongs to ssh.
    const ONE_HOG: &str = "\
COMMAND            PID    USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
ssh               2146     you    8u  IPv4 0x1              0t0  TCP 127.0.0.1:8443->127.0.0.1:64301 (ESTABLISHED)
connection-hog   19191     you   23u  IPv4 0x2              0t0  TCP 127.0.0.1:64301->127.0.0.1:8443 (ESTABLISHED)
ssh               2146     you    9u  IPv4 0x3              0t0  TCP 127.0.0.1:8443->127.0.0.1:64302 (ESTABLISHED)
connection-hog   19191     you   24u  IPv4 0x4              0t0  TCP 127.0.0.1:64302->127.0.0.1:8443 (ESTABLISHED)
ssh               2146     you   10u  IPv4 0x5              0t0  TCP 127.0.0.1:8443->127.0.0.1:64303 (ESTABLISHED)
connection-hog   19191     you   25u  IPv4 0x6              0t0  TCP 127.0.0.1:64303->127.0.0.1:8443 (ESTABLISHED)
ssh               2146     you   11u  IPv4 0x7              0t0  TCP 127.0.0.1:8443->127.0.0.1:64304 (ESTABLISHED)
kubectl           2200     you    3u  IPv4 0x8              0t0  TCP 127.0.0.1:64304->127.0.0.1:8443 (ESTABLISHED)
ssh               2148     you   12u  IPv4 0x9              0t0  TCP 127.0.0.1:15432->127.0.0.1:64305 (ESTABLISHED)
psql              2400     you    4u  IPv4 0xa              0t0  TCP 127.0.0.1:64305->127.0.0.1:15432 (ESTABLISHED)
";

    #[test]
    fn the_holder_of_a_ports_connections_is_named_with_its_count() {
        let holders = parse_lsof_holders(ONE_HOG, 8443, None);
        assert_eq!(holders[0].command, "connection-hog");
        assert_eq!(holders[0].pid, 19191);
        assert_eq!(holders[0].count, 3);
    }

    // `lsof` truncates COMMAND to nine characters without `+c 0`, so the
    // culprit reads `connecti` and a `pgrep` for it finds nothing. The
    // listing asks for the full name; the parser must not re-truncate it.
    #[test]
    fn a_command_longer_than_lsofs_default_column_is_kept_whole() {
        assert!(
            parse_lsof_holders(ONE_HOG, 8443, None)[0].command.len() > 9,
            "the name is what someone types into pgrep"
        );
    }

    // The fixture above cannot catch a truncated name, because a fixture is
    // whatever it was written to be. Only the flag we hand lsof can
    #[test]
    fn the_listing_asks_lsof_for_whole_command_names() {
        let args = super::LSOF_LISTING_ARGS;
        let width = args
            .iter()
            .position(|arg| *arg == "+c")
            .expect("lsof truncates COMMAND to nine characters unless asked otherwise");
        assert_eq!(
            args.get(width + 1),
            Some(&"0"),
            "+c takes the column width, and 0 is the whole name"
        );
    }

    // The message names one process, so the worst offender has to be the one
    // it names
    #[test]
    fn holders_are_ordered_so_the_worst_offender_is_first() {
        let holders = parse_lsof_holders(ONE_HOG, 8443, None);
        assert_eq!(holders.len(), 2, "{holders:?}");
        assert_eq!(holders[0].command, "connection-hog");
        assert_eq!(holders[1].command, "kubectl");
        assert_eq!(holders[1].count, 1);
    }

    // Only the port asked about: a second tunnel's clients are not holding
    // this one's descriptors
    #[test]
    fn a_holder_of_another_port_is_not_a_holder_of_this_one() {
        let holders = parse_lsof_holders(ONE_HOG, 15432, None);
        assert_eq!(holders.len(), 1, "{holders:?}");
        assert_eq!(holders[0].command, "psql");
    }

    // The probe connects to the very port it is attributing, so gtunnel would
    // otherwise appear in its own accusation
    #[test]
    fn gtunnels_own_probe_is_never_named_as_a_holder() {
        let probed = format!(
            "{ONE_HOG}\
gtunnel           9100     you    9u  IPv4 0xb              0t0  TCP 127.0.0.1:52999->127.0.0.1:8443 (ESTABLISHED)
ssh               2146     you   13u  IPv4 0xc              0t0  TCP 127.0.0.1:8443->127.0.0.1:52999 (ESTABLISHED)
"
        );
        assert!(
            parse_lsof_holders(&probed, 8443, None)
                .iter()
                .any(|h| h.pid == 9100),
            "without the exclusion the probe is in the listing"
        );
        let holders = parse_lsof_holders(&probed, 8443, Some(9100));
        assert!(holders.iter().all(|h| h.pid != 9100), "{holders:?}");
    }

    // A machine without lsof must degrade, not invent a culprit
    #[test]
    fn nothing_parseable_names_nobody() {
        assert!(parse_lsof_holders("", 8443, None).is_empty());
        assert!(parse_lsof_holders("lsof: command not found", 8443, None).is_empty());
    }

    const TWO_TUNNELS: &str = "\
COMMAND   PID    USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
ssh      2146     you    8u  IPv4 0x1              0t0  TCP 127.0.0.1:8443->127.0.0.1:64306 (ESTABLISHED)
kubectl  2200     you    3u  IPv4 0x2              0t0  TCP 127.0.0.1:64306->127.0.0.1:8443 (ESTABLISHED)
ssh      2146     you    9u  IPv6 0x3              0t0  TCP [::1]:8443->[::1]:64813 (ESTABLISHED)
mongosh  2300     you    5u  IPv6 0x4              0t0  TCP [::1]:64813->[::1]:8443 (ESTABLISHED)
ssh      2148     you   10u  IPv4 0x5              0t0  TCP 127.0.0.1:15432->127.0.0.1:51234 (ESTABLISHED)
ssh      2150     you    7u  IPv4 0x6              0t0  TCP 127.0.0.1:5432 (LISTEN)
";

    // The sampler against a real listing, where nothing is a fixture: it
    // proves the arguments still match, that the parse reads what `lsof`
    // prints on this machine, and that a connection this process opened is
    // left out — which is how a sample keeps `tcp_probe` from counting itself.
    // macOS only, because the Linux path has no `lsof` to ask and falls back
    // to `active_connections` and its `ss` branch
    #[cfg(target_os = "macos")]
    #[test]
    fn a_real_listing_leaves_out_this_processs_own_connections() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let (accepted, _) = listener.accept().expect("accept");

        let text = lsof_listing().expect("lsof answered");
        assert_eq!(
            parse_lsof_by_port(&text, None).get(&port).copied(),
            Some(1),
            "the listing has the listener's end of a live connection in it"
        );
        assert_eq!(
            parse_lsof_by_port(&text, Some(std::process::id()))
                .get(&port)
                .copied(),
            None,
            "a sample counted a socket of this process as a client of the port it measures"
        );
        drop(accepted);
        drop(client);
    }

    // lsof exits 1 when nothing matched, and non-zero again when it merely
    // warns about something it could not stat while still printing the rows it
    // found. Gating on the status threw a real count away
    #[test]
    fn a_probe_registers_its_source_port_only_where_a_supervisor_is_watching() {
        let dir = tempfile::tempdir().expect("tempdir");
        register_probe_in(dir.path(), 8443, 50001);
        assert!(!dir.path().join("8443").exists(), "nobody was watching");

        watch_probes(dir.path(), 8443).expect("watch");
        register_probe_in(dir.path(), 8443, 50002);
        assert!(is_probe_in(dir.path(), 8443, 50002));
        assert!(!is_probe_in(dir.path(), 8443, 50003));
        assert!(!is_probe_in(dir.path(), 9000, 50002));

        unwatch_probes(dir.path(), 8443);
        assert!(!is_probe_in(dir.path(), 8443, 50002));
    }

    #[test]
    fn a_ledger_entry_expires_and_is_pruned_on_the_next_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        watch_probes(dir.path(), 8443).expect("watch");
        let stale = unix_now() - PROBE_TTL_SECS - 5;
        std::fs::write(dir.path().join("8443"), format!("40000 {stale}\n")).expect("seed");
        assert!(!is_probe_in(dir.path(), 8443, 40000), "past its time");
        register_probe_in(dir.path(), 8443, 40001);
        let text = std::fs::read_to_string(dir.path().join("8443")).expect("read");
        assert!(!text.contains("40000"), "{text}");
        assert!(text.contains("40001"), "{text}");
    }

    #[test]
    fn a_non_local_target_is_not_registered() {
        // Parsing alone: the address is not the loopback the supervisor serves
        register_probe("10.0.0.10:8443", 50000);
        register_probe("localhost:8443", 50000);
    }

    #[cfg(unix)]
    #[test]
    fn a_listing_is_read_whatever_the_exit_status_was() {
        use std::os::unix::process::ExitStatusExt;

        let warned = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: TWO_TUNNELS.as_bytes().to_vec(),
            stderr: b"lsof: WARNING: can't stat() nfs file system\n".to_vec(),
        };
        assert_eq!(
            listing(&warned)
                .as_deref()
                .map(|text| parse_lsof_by_port(text, None)),
            Some(parse_lsof_by_port(TWO_TUNNELS, None)),
            "a warning threw away rows lsof had already found"
        );
        let nothing = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(listing(&nothing), None);
    }

    // The sampler has not run yet on the first metrics tick, and a port with no
    // traffic since the last one has no entry of its own. Neither may read
    // worse than the direct call did before there was a sampler
    #[test]
    fn a_port_the_sampler_has_not_seen_is_measured_directly() {
        let sampled = HashMap::from([(8443u16, 4u64)]);
        assert_eq!(connections_for(Some(&sampled), 8443), 4);
        assert_eq!(
            connections_for(Some(&sampled), 1),
            0,
            "port 1 has no client, and the direct reading says so"
        );
        assert_eq!(connections_for(None, 1), 0);
    }

    #[test]
    fn ss_counts_established_rows() {
        let fixture = "\
State  Recv-Q Send-Q Local Address:Port  Peer Address:Port
ESTAB  0      0      127.0.0.1:15432     127.0.0.1:52001
ESTAB  0      0      127.0.0.1:15432     127.0.0.1:52002
LISTEN 0      128    127.0.0.1:15432     0.0.0.0:*
";
        assert_eq!(parse_ss_established(fixture), 2);
        assert_eq!(parse_ss_established(""), 0);
    }

    // A 404 means the tunnel carried the request: the service answered
    #[tokio::test]
    async fn http_probe_treats_a_404_as_healthy_and_a_502_as_unhealthy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/broken"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&server)
            .await;

        assert_eq!(
            http_probe(&format!("{}/missing", server.uri())).await,
            HealthStatus::Healthy
        );
        assert_eq!(
            http_probe(&format!("{}/broken", server.uri())).await,
            HealthStatus::Unhealthy
        );
    }

    #[tokio::test]
    async fn http_probe_is_unhealthy_when_nothing_answers() {
        assert_eq!(
            http_probe("http://127.0.0.1:1/").await,
            HealthStatus::Unhealthy
        );
    }

    #[tokio::test]
    async fn tcp_probe_reaches_a_listening_port_and_not_a_closed_one() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        // A port is only reached when something behind it answers, so the
        // listener has to accept rather than leave the connection in its backlog
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept");
            drop(sock);
        });
        assert!(tcp_probe(&addr, Duration::from_secs(2)).await.is_some());
        assert!(tcp_probe("127.0.0.1:1", Duration::from_millis(200))
            .await
            .is_none());
    }

    #[test]
    fn history_keeps_the_most_recent_samples() {
        let mut history = MetricsHistory::new(3);
        for value in [1u64, 2, 3, 4] {
            history.record(value);
        }
        assert_eq!(history.samples(), &[2, 3, 4]);
        assert_eq!(history.peak(), 4);
    }
}
