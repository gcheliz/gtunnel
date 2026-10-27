use anyhow::{bail, Context, Result};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config::config_dir;
use crate::expose::routes;
use crate::gcp::{gcloud, GcpClient};
use crate::state::{
    allocate_port, local_port_is_free, AccessTarget, PersistentTunnel, TunnelSpec, TunnelState,
    ACCESS_PORT_BASE, ACCESS_PORT_SPAN, RELAY_PORT_BASE, RELAY_PORT_SPAN,
};
use crate::tunnel::{access_argv, diagnose_log_line, expose_argv};

// A tunnel that lives only as long as the foreground process that started it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EphemeralRecord {
    pub name: String,
    pub account_name: String,
    pub pid: u32,
    pub spec: TunnelSpec,
    pub started_at: u64,
    /// Seconds the run was told to live. Absent for a run with no `--ttl`, and
    /// for every record an older version wrote, so those still parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

pub fn run_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("run");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir)
}

impl EphemeralRecord {
    /// The run as a tunnel, so the TUI, `list` and the MCP server render it
    /// with the same code as a declared one instead of each rebuilding it.
    pub fn as_tunnel(&self) -> PersistentTunnel {
        PersistentTunnel {
            name: self.name.clone(),
            account_name: self.account_name.clone(),
            auto_start: false,
            spec: self.spec.clone(),
        }
    }

    /// Unix seconds at which the TTL ends the run; None when it has no TTL.
    pub fn expires_at(&self) -> Option<u64> {
        self.ttl_secs.map(|ttl| self.started_at + ttl)
    }

    pub fn path(&self) -> Result<PathBuf> {
        Ok(run_dir()?.join(format!("{}-{}.json", self.account_name, self.name)))
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path()?;
        let json = serde_json::to_string_pretty(self).context("Failed to encode the run record")?;
        crate::fsutil::write_atomic(&path, &json)
    }

    pub fn remove(&self) -> Result<()> {
        let path = self.path()?;
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
        }
        Ok(())
    }
}

// Signal 0 asks the kernel whether the process exists without disturbing it
// Poll until the process is gone, up to `budget`. True means it exited; false
// means it outlived the budget and the caller decides what that is worth.
pub async fn wait_for_pid_exit(pid: u32, budget: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if !pid_is_alive(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// The only way a recorded pid becomes an argument to `kill(2)`. A pid is a
/// number read back from a file, and kill gives three of its values a meaning
/// that is not "this process": 0 signals our own process group, a negative
/// value signals a group, and -1 signals everything we may signal. A record
/// that holds 0 (a spawn that lost its child id) or a value past `i32::MAX`
/// (which wraps negative) must therefore read as "no process", never as a
/// broadcast.
pub fn signalable(pid: u32) -> Option<libc::pid_t> {
    i32::try_from(pid).ok().filter(|p| *p > 0)
}

pub fn pid_is_alive(pid: u32) -> bool {
    let Some(pid) = signalable(pid) else {
        return false;
    };
    // SAFETY: kill(2) with signal 0 takes two integers and no pointer, and sends
    // nothing: it only reports whether the pid exists and may be signalled, so
    // there is no memory or process state to corrupt. `signalable` guarantees
    // the pid is positive, so it names one process and never a group.
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn list_ephemeral() -> Result<Vec<(EphemeralRecord, bool)>> {
    let dir = run_dir()?;
    let mut out = Vec::new();
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // A record written by another version is skipped, not fatal
        let Ok(record) = serde_json::from_str::<EphemeralRecord>(&text) else {
            continue;
        };
        let alive = pid_is_alive(record.pid);
        out.push((record, alive));
    }
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    Ok(out)
}

/// The longest a run may be told to live. A TTL is a safety net for a tunnel
/// nobody is watching, so an unbounded one defeats its purpose; a day is
/// already longer than any session this is meant for.
pub const MAX_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// `90s`, `30m` or `2h`; a bare integer is minutes, because the MCP tool and
/// the people asking for "an hour or so" both think in minutes. Zero is
/// refused: a run that ends at once is a mistake, not a request.
pub fn parse_ttl(text: &str) -> Result<std::time::Duration> {
    let text = text.trim();
    let (digits, unit) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1u64),
        Some('m') => (&text[..text.len() - 1], 60),
        Some('h') => (&text[..text.len() - 1], 3600),
        _ => (text, 60),
    };
    let invalid = || format!("Invalid TTL '{text}': use 90s, 30m or 2h (a bare number is minutes)");
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        bail!("{}", invalid());
    }
    let amount: u64 = digits.parse().with_context(invalid)?;
    let secs = amount
        .checked_mul(unit)
        .with_context(|| format!("TTL '{text}' is too large"))?;
    if secs == 0 {
        bail!("TTL must be longer than zero");
    }
    let ttl = std::time::Duration::from_secs(secs);
    if ttl > MAX_TTL {
        bail!("TTL '{text}' is longer than the 24h maximum");
    }
    Ok(ttl)
}

// With no TTL the arm never fires, so the select treats it like any other
// run that waits for a signal
async fn ttl_elapsed(ttl: Option<std::time::Duration>) {
    match ttl {
        Some(ttl) => tokio::time::sleep(ttl).await,
        None => std::future::pending().await,
    }
}

/// The runs of one account whose process is still there. A dead record is a
/// run that is gone but may still own a route, which is `doctor`'s business and
/// not something to list as a tunnel you can connect to.
pub fn live_runs(account: &str) -> Result<Vec<EphemeralRecord>> {
    Ok(list_ephemeral()?
        .into_iter()
        .filter(|(record, alive)| *alive && record.account_name == account)
        .map(|(record, _)| record)
        .collect())
}

const NAME_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

// An unnamed `run` still needs a name for the log line and the record
pub fn generated_name() -> String {
    let mut rng = rand::rng();
    let suffix: String = (0..6)
        .map(|_| NAME_ALPHABET[rng.random_range(0..NAME_ALPHABET.len())] as char)
        .collect();
    format!("gt-{suffix}")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Every record counts, alive or not: a record is removed only once its relay
// route is gone, so a dead one's port must not be handed to a new tunnel that
// would then share the old hostname's named port
// Relay ports are bound on the relay VM, so nothing local can probe them:
// state and the live runs are the only record, and `expose` has to read both
pub fn used_relay_ports(
    state: &TunnelState,
    account: &str,
) -> Result<std::collections::HashSet<u16>> {
    let mut used = state.used_relay_ports(account);
    used.extend(ephemeral_ports(account, true)?);
    Ok(used)
}

fn ephemeral_ports(account: &str, relay: bool) -> Result<std::collections::HashSet<u16>> {
    let mut used = std::collections::HashSet::new();
    for (record, _alive) in list_ephemeral()? {
        if record.account_name != account {
            continue;
        }
        match (&record.spec, relay) {
            (TunnelSpec::Expose { relay_port, .. }, true) => {
                used.insert(*relay_port);
            }
            (TunnelSpec::Access { local_port, .. }, false) => {
                used.insert(*local_port);
            }
            _ => {}
        }
    }
    Ok(used)
}

// Ctrl+C, SIGTERM and SIGHUP all have to reach teardown: a route left behind
// keeps resolving for everyone
#[cfg(unix)]
async fn stop_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    let mut hup = match signal(SignalKind::hangup()) {
        Ok(stream) => stream,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = hup.recv() => {}
    }
}

#[cfg(not(unix))]
async fn stop_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

// Spawn gcloud and write the run record; both have to succeed together
fn start_child(
    gcloud_path: &std::path::Path,
    acct: &crate::config::Account,
    expose_config: &Option<crate::config::ExposeConfig>,
    tunnel: &PersistentTunnel,
    spec: TunnelSpec,
    name: &str,
    ttl: Option<std::time::Duration>,
) -> Result<(tokio::process::Child, EphemeralRecord)> {
    let level = crate::loglevel::current();
    let argv = match expose_config {
        Some(expose) => expose_argv(acct, expose, tunnel, level)?,
        None => access_argv(acct, tunnel, level)?,
    };

    let mut child = Command::new(gcloud_path)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run {}", gcloud_path.display()))?;

    // No id means the child was already reaped, so there is nothing to record
    // and a recorded 0 would later read as a process group
    let Some(pid) = child.id() else {
        bail!(
            "{} exited before it could be recorded",
            gcloud_path.display()
        );
    };
    let record = EphemeralRecord {
        name: name.to_string(),
        account_name: acct.name.clone(),
        pid,
        spec,
        started_at: now_secs(),
        ttl_secs: ttl.map(|t| t.as_secs()),
    };
    if let Err(e) = record.save() {
        let _ = child.start_kill();
        return Err(e);
    }
    Ok((child, record))
}

pub async fn cmd_run(
    account: Option<&str>,
    args: &[String],
    zone: Option<&str>,
    local_port: Option<u16>,
    public: bool,
    domain: Option<&str>,
    ttl: Option<std::time::Duration>,
) -> Result<()> {
    let (name, target) = match args {
        [target] => (generated_name(), target.clone()),
        [name, target] => (name.clone(), target.clone()),
        _ => bail!("Usage: gtunnel run [NAME] TARGET"),
    };
    crate::state::validate_tunnel_name(&name)?;

    let (_, acct) = crate::commands::resolve(account)?;
    let state = TunnelState::load()?;
    let gcloud_path = gcloud::require_gcloud()?;

    let (spec, expose_config) = if public {
        let expose = acct.expose.clone().with_context(|| {
            format!(
                "No relay for account '{}'. Run `gtunnel relay up`",
                acct.name
            )
        })?;
        let domain = expose.domain(domain)?.clone();
        let used = used_relay_ports(&state, &acct.name)?;
        let relay_port = allocate_port(&name, RELAY_PORT_BASE, RELAY_PORT_SPAN, &used, |_| true)?;
        (
            TunnelSpec::Expose {
                local_target: target.clone(),
                domain: domain.domain.clone(),
                hostname: format!("{name}.{}", domain.domain),
                relay_port,
                private: false,
            },
            Some(expose),
        )
    } else {
        let (host, remote_port) = crate::commands::access::parse_access_target(&target)?;
        let mut used = state.used_local_ports();
        used.extend(ephemeral_ports(&acct.name, false)?);
        let local_port = match local_port {
            Some(port) => port,
            None => allocate_port(
                &name,
                ACCESS_PORT_BASE,
                ACCESS_PORT_SPAN,
                &used,
                local_port_is_free,
            )?,
        };
        (
            TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: host,
                    zone: zone.unwrap_or(&acct.default_zone).to_string(),
                },
                remote_port,
                local_port,
            },
            None,
        )
    };

    let tunnel = PersistentTunnel {
        name: name.clone(),
        account_name: acct.name.clone(),
        auto_start: false,
        spec: spec.clone(),
    };

    let client = GcpClient::for_account(&acct);
    if let Some(expose) = &expose_config {
        routes::add_route(&client, &acct, expose, &tunnel).await?;
    }

    let started = start_child(
        &gcloud_path,
        &acct,
        &expose_config,
        &tunnel,
        spec,
        &name,
        ttl,
    );
    let (mut child, record) = match started {
        Ok(started) => started,
        Err(e) => {
            // The route is already published; leaving it would serve a
            // hostname with nothing behind it
            if let Some(expose) = &expose_config
                && let Err(cleanup) = routes::remove_route(&client, &acct, expose, &tunnel).await
            {
                eprintln!("Failed to remove the relay route: {cleanup:#}");
                eprintln!("Run `gtunnel doctor --fix` to clean it up");
            }
            return Err(e);
        }
    };

    println!(
        "{}  {} -> {}",
        name,
        tunnel.display_url(),
        tunnel.display_target()
    );
    match ttl {
        Some(ttl) => println!(
            "Stops by itself in {}, or Ctrl+C",
            humantime::format_duration(ttl)
        ),
        None => println!("Ctrl+C to stop"),
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let pump = async move {
        let mut out_lines = stdout.map(|s| BufReader::new(s).lines());
        let mut err_lines = stderr.map(|s| BufReader::new(s).lines());
        loop {
            let line = match (&mut out_lines, &mut err_lines) {
                (Some(out), Some(err)) => tokio::select! {
                    line = out.next_line() => line,
                    line = err.next_line() => line,
                },
                (Some(out), None) => out.next_line().await,
                (None, Some(err)) => err.next_line().await,
                (None, None) => return,
            };
            match line {
                Ok(Some(line)) => match diagnose_log_line(&line) {
                    Some(hint) => println!("{line}\n  {hint}"),
                    None => println!("{line}"),
                },
                Ok(None) => return,
                Err(e) => {
                    eprintln!("Failed to read gcloud output: {e}");
                    return;
                }
            }
        }
    };

    tokio::select! {
        _ = pump => {}
        status = child.wait() => {
            if let Ok(status) = status
                && !status.success() {
                    println!("gcloud exited with {status}");
                }
        }
        _ = stop_signal() => {
            println!();
        }
        // The same exit as Ctrl+C, so the route and the record go the same way
        _ = ttl_elapsed(ttl) => {
            println!("TTL reached, stopping");
        }
    }

    let _ = child.start_kill();
    let _ = child.wait().await;
    teardown(&client, &acct, expose_config.as_ref(), &tunnel, &record).await;
    Ok(())
}

// Every removal is reported as it happened: a DNS record that is still there
// must never be announced as deleted
async fn teardown(
    client: &GcpClient,
    acct: &crate::config::Account,
    expose: Option<&crate::config::ExposeConfig>,
    tunnel: &PersistentTunnel,
    record: &EphemeralRecord,
) {
    if let Some(expose) = expose {
        match routes::remove_route(client, acct, expose, tunnel).await {
            Ok(removal) => {
                if removal.dns {
                    println!("Removed DNS record for {}", tunnel.display_url());
                }
                if removal.backend {
                    println!("Removed backend service");
                }
                if removal.host_rule {
                    println!("Removed the load balancer host rule");
                }
            }
            Err(e) => {
                eprintln!("Failed to remove the relay route: {e:#}");
                eprintln!("Run `gtunnel doctor --fix` to clean it up");
            }
        }
    }
    if let Err(e) = record.remove() {
        eprintln!("Failed to remove the run record: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::testing::TempConfigDir;
    use crate::state::AccessTarget;
    use std::time::Duration;

    fn spec() -> TunnelSpec {
        TunnelSpec::Access {
            target: AccessTarget::Instance {
                instance: "my-vm".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 5432,
            local_port: 15432,
        }
    }

    #[test]
    fn a_record_round_trips_through_the_run_directory() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-abc123".to_string(),
            account_name: "dev".to_string(),
            pid: std::process::id(),
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
        };
        record.save().expect("save");

        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, record);
        assert!(found[0].1, "the current process is alive");

        record.remove().expect("remove");
        assert!(list_ephemeral().expect("list").is_empty());
    }

    #[test]
    fn a_pid_that_would_signal_a_group_is_never_signalable() {
        assert_eq!(signalable(0), None, "kill(0) signals our own process group");
        assert_eq!(
            signalable(u32::MAX),
            None,
            "u32::MAX wraps to -1, which signals every process we may signal"
        );
        assert_eq!(signalable(i32::MAX as u32 + 1), None, "negative: a group");
        assert_eq!(signalable(1234), Some(1234));
    }

    #[test]
    fn a_record_with_pid_zero_is_not_running() {
        assert!(!pid_is_alive(0), "kill(0, 0) succeeds for our own group");
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-zero00".to_string(),
            account_name: "dev".to_string(),
            pid: 0,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
        };
        record.save().expect("save");
        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1, "an old record with pid 0 still parses");
        assert!(!found[0].1, "pid 0 must not read as a live run");
    }

    // Importing a run SIGTERMs it, and the dying process removes its relay
    // route on the way out. Rebuilding the route before it is gone loses the
    // race, so the import has to wait for the pid first.
    #[tokio::test]
    async fn wait_for_pid_exit_waits_for_a_live_process_and_reports_a_dead_one() {
        // Orphaned so the kernel reaps it: a child of this test would linger
        // as a zombie and still answer kill(pid, 0)
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("/bin/sleep 30 >/dev/null 2>&1 & echo $!")
            .output()
            .expect("spawn");
        let pid: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("pid");
        assert!(pid_is_alive(pid), "the sleep should be running");

        assert!(
            !wait_for_pid_exit(pid, Duration::from_millis(300)).await,
            "a live pid must not be reported as exited"
        );

        // SAFETY: kill(2) takes two integers and no pointer. The pid is the child
        // this test spawned a few lines up and still holds, so it is not a
        // recycled pid belonging to another process.
        unsafe { libc::kill(signalable(pid).expect("a real pid"), libc::SIGTERM) };
        assert!(
            wait_for_pid_exit(pid, Duration::from_secs(5)).await,
            "a killed pid must be reported as exited"
        );
    }

    // `expose` read tunnels.toml alone, so it handed out a relay port a live
    // `gtunnel run --public` was already bound to on the relay VM
    #[test]
    fn used_relay_ports_counts_a_live_run() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-live".to_string(),
            account_name: "dev".to_string(),
            pid: std::process::id(),
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "gt-live.dev.example.com".to_string(),
                relay_port: 30007,
                private: false,
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
        };
        record.save().expect("save");

        let state = TunnelState::default();
        let used = used_relay_ports(&state, "dev").expect("ports");
        assert!(used.contains(&30007), "{used:?}");
        assert!(
            used_relay_ports(&state, "prod").expect("ports").is_empty(),
            "another account's run is not this account's port"
        );
    }

    #[test]
    fn a_dead_pid_is_reported_as_not_alive() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-dead01".to_string(),
            account_name: "dev".to_string(),
            pid: 999_999,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
        };
        record.save().expect("save");

        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1);
        assert!(!found[0].1, "pid 999999 should not be running");
    }

    #[test]
    fn an_old_record_without_ttl_secs_still_parses() {
        let old = r#"{"name":"gt-old","account_name":"dev","pid":42,"started_at":1700000000,
"spec":{"mode":"access","target":{"kind":"instance","instance":"my-vm","zone":"z"},"remote_port":5432,"local_port":15000}}"#;
        let record: EphemeralRecord = serde_json::from_str(old).expect("parse");
        assert_eq!(record.ttl_secs, None);
        assert_eq!(record.expires_at(), None);
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("ttl_secs"), "{json}");
    }

    #[test]
    fn a_record_with_a_ttl_expires_at_start_plus_ttl() {
        let record = EphemeralRecord {
            name: "gt-ttl".to_string(),
            account_name: "dev".to_string(),
            pid: 1,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: Some(90),
        };
        assert_eq!(record.expires_at(), Some(1_700_000_090));
    }

    #[test]
    fn live_runs_skips_dead_records_and_other_accounts() {
        let _temp = TempConfigDir::new();
        let mk = |name: &str, account: &str, pid: u32| EphemeralRecord {
            name: name.to_string(),
            account_name: account.to_string(),
            pid,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
        };
        mk("alive", "dev", std::process::id()).save().unwrap();
        mk("dead", "dev", 999_999).save().unwrap();
        mk("other", "prod", std::process::id()).save().unwrap();
        let names: Vec<String> = live_runs("dev")
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["alive"]);
    }

    #[test]
    fn a_ttl_reads_seconds_minutes_and_hours() {
        assert_eq!(parse_ttl("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_ttl("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_ttl("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_ttl(" 5s ").unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn a_bare_ttl_is_minutes() {
        assert_eq!(parse_ttl("45").unwrap(), Duration::from_secs(45 * 60));
    }

    #[test]
    fn a_ttl_of_zero_or_nonsense_is_refused() {
        for bad in ["0", "0s", "0m", "", "m", "-5m", "1.5h", "10d", "abc", "5 m"] {
            assert!(parse_ttl(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_ttl_past_a_day_is_refused_and_a_day_is_not() {
        assert!(parse_ttl("24h").is_ok());
        assert!(parse_ttl("1440m").is_ok());
        assert!(parse_ttl("86401s").is_err());
        assert!(parse_ttl("25h").is_err());
        assert!(parse_ttl("99999999999999999999h").is_err(), "overflow");
    }

    #[test]
    fn a_generated_name_is_gt_plus_six_characters() {
        for _ in 0..20 {
            let name = generated_name();
            assert_eq!(name.len(), 9, "{name}");
            assert!(name.starts_with("gt-"), "{name}");
            assert!(
                name[3..]
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "{name}"
            );
            crate::state::validate_tunnel_name(&name).expect("a valid tunnel name");
        }
    }

    #[test]
    fn generated_names_differ() {
        let a = generated_name();
        let b = generated_name();
        assert_ne!(a, b);
    }
    // A record whose process died may still own a relay route, so its port
    // stays reserved until teardown or doctor confirms the route is gone
    #[test]
    fn a_dead_records_relay_port_stays_reserved() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-dead02".to_string(),
            account_name: "dev".to_string(),
            pid: 999_999,
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "gt-dead02.dev.example.com".to_string(),
                relay_port: 30_123,
                private: false,
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
        };
        record.save().expect("save");

        let used = ephemeral_ports("dev", true).expect("ports");
        assert!(used.contains(&30_123), "{used:?}");
    }

    #[test]
    fn another_accounts_ports_are_not_reserved() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-other1".to_string(),
            account_name: "prod".to_string(),
            pid: std::process::id(),
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "prod.example.com".to_string(),
                hostname: "gt-other1.prod.example.com".to_string(),
                relay_port: 30_124,
                private: false,
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
        };
        record.save().expect("save");

        let used = ephemeral_ports("dev", true).expect("ports");
        assert!(!used.contains(&30_124), "{used:?}");
    }
}
