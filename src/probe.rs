use std::collections::VecDeque;
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
    close_politely(&mut stream, timeout).await
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

pub fn parse_lsof_established(text: &str) -> u64 {
    text.lines()
        .filter(|line| line.contains("ESTABLISHED"))
        .count() as u64
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
    if let Ok(out) = std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:ESTABLISHED"])
        .output()
    {
        if out.status.success() {
            return parse_lsof_established(&String::from_utf8_lossy(&out.stdout));
        }
    }
    if let Ok(out) = std::process::Command::new("ss")
        .args(["-tnH", "state", "established", &format!("sport = :{port}")])
        .output()
    {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            // `ss -H` drops the header, so every row counts
            let rows = text.lines().filter(|l| !l.trim().is_empty()).count() as u64;
            return rows.max(parse_ss_established(&text));
        }
    }
    0
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
    let out = std::process::Command::new("ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .output()
        .ok()?;
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
) -> TunnelMetrics {
    let label = daemon::platform_label(&t.account_name, &t.name);
    let status = daemon::status(&label).await;
    let runtime = daemon::runtime_info(&label).await;
    measure(cfg, t, include_lb, status, runtime).await
}

// An unmanaged tunnel has no service to ask: discovery saw the process, and
// `ps` already gave the pid its uptime is read from. Nothing restarts it, so a
// restart count would be a number gtunnel invented.
pub async fn collect_unmanaged(cfg: &Config, t: &PersistentTunnel, pid: u32) -> TunnelMetrics {
    let runtime = daemon::RuntimeInfo {
        pid: Some(pid),
        restarts: 0,
    };
    measure(cfg, t, false, TunnelStatus::Running, runtime).await
}

async fn measure(
    cfg: &Config,
    t: &PersistentTunnel,
    include_lb: bool,
    status: TunnelStatus,
    runtime: daemon::RuntimeInfo,
) -> TunnelMetrics {
    let mut metrics = TunnelMetrics {
        restarts: runtime.restarts,
        uptime: runtime.pid.and_then(process_uptime),
        available: status == TunnelStatus::Running,
        ..TunnelMetrics::default()
    };

    match &t.spec {
        TunnelSpec::Access { local_port, .. } => {
            metrics.active_connections = active_connections(*local_port);
            if let Some(rtt) = tcp_probe(&format!("127.0.0.1:{local_port}"), TCP_TIMEOUT).await {
                metrics.latency = Some(rtt);
            } else {
                metrics.available = false;
            }
        }
        TunnelSpec::Expose { hostname, .. } => {
            if include_lb {
                if let Ok(acct) = cfg.get_account(Some(&t.account_name)) {
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
        assert_eq!(parse_lsof_established(fixture), 3);
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
