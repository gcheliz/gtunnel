use crate::error::GtunnelError;
use anyhow::{bail, Context, Result};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config::{config_dir, ExposeConfig};
use crate::expose::routes;
use crate::gcp::{gcloud, GcpClient};
use crate::state::{
    allocate_port, check_local_port, local_port_is_free, AccessTarget, PersistentTunnel,
    TunnelSpec, TunnelState, ACCESS_PORT_BASE, ACCESS_PORT_SPAN, RELAY_PORT_BASE, RELAY_PORT_SPAN,
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
    /// Who `pid` is: the start token `crate::proc::start_token` read just after
    /// the spawn. A pid alone is a number the kernel hands out again; pid and
    /// start together name one process. Absent in records 0.1.42 and older
    /// wrote, which are judged by the pid alone as they always were. Present
    /// means the pid was spawned with `process_group(0)` and leads its group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<String>,
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

    /// Whether this run's process is still the one that was recorded. A start
    /// token that cannot be read reads as dead: a process we cannot identify
    /// is never signalled.
    pub fn is_alive(&self) -> bool {
        if !pid_is_alive(self.pid) {
            return false;
        }
        match &self.pid_start {
            Some(token) => crate::proc::start_token(self.pid).as_deref() == Some(token),
            None => true,
        }
    }

    /// Ask the run to stop. A record with a start token leads its own process
    /// group (it was spawned with `process_group(0)`), so the group is signalled
    /// and gcloud's children hear it too. An older record is no group leader, so
    /// signalling `-pid` would hit nothing or someone else's group: the pid
    /// alone is signalled, as it always was. False, with nothing sent, when the
    /// run is not ours any more.
    pub fn terminate(&self) -> bool {
        if !self.is_alive() {
            return false;
        }
        let Some(pid) = signalable(self.pid) else {
            return false;
        };
        if self.pid_start.is_some() {
            crate::proc::signal_group(pid, libc::SIGTERM);
        } else {
            // SAFETY: kill(2) takes two integers and no pointer. `signalable`
            // rejected 0 and negative values, so this names one process and
            // never a group. A pid that has exited since is a harmless ESRCH.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        true
    }

    /// Poll until the run is gone, up to `budget`. True means it exited; false
    /// means it outlived the budget and the caller decides what that is worth.
    pub async fn wait_for_exit(&self, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if !self.is_alive() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
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

// Signal 0 asks the kernel whether the process exists without disturbing it.
// A bare pid can be a recycled one, so a run record is judged by its start
// token as well; this alone serves only callers with nothing more than a pid,
// like the config page's `tunnels.edit-<pid>.toml` copies, where a wrong
// "alive" merely keeps a stale copy
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
        let alive = record.is_alive();
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
        return Err(GtunnelError::usage(invalid()).into());
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

/// `expires_at` is whole seconds, so a one-second tick ends a run at most a
/// second late after any suspend, for one clock read a second.
const TTL_TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// Polls the wall clock instead of sleeping the whole TTL. `tokio::time::sleep`
/// runs on the monotonic clock, which does not advance while the machine is
/// suspended on macOS or Linux: a laptop closed for an hour would still owe the
/// full TTL on wake, while `expires_at`, which the TUI shows, had passed. With
/// no TTL the future never completes, so the select treats it like any other
/// run that waits for a signal.
async fn ttl_elapsed(expires_at: Option<u64>, tick: std::time::Duration, now: impl Fn() -> u64) {
    let Some(at) = expires_at else {
        return std::future::pending().await;
    };
    loop {
        if now() >= at {
            return;
        }
        tokio::time::sleep(tick).await;
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
    used.extend(run_relay_ports(account)?);
    Ok(used)
}

/// Relay ports live on one account's relay VM, so another account's runs do not
/// compete for them.
fn run_relay_ports(account: &str) -> Result<std::collections::HashSet<u16>> {
    let mut used = std::collections::HashSet::new();
    for (record, _alive) in list_ephemeral()? {
        if record.account_name != account {
            continue;
        }
        if let TunnelSpec::Expose { relay_port, .. } = &record.spec {
            used.insert(*relay_port);
        }
    }
    Ok(used)
}

/// Local ports are shared by the whole machine, whichever account's run holds
/// them, so every record counts. A dead record still counts: its port is
/// released only when the record goes.
fn run_local_ports() -> Result<std::collections::HashSet<u16>> {
    let mut used = std::collections::HashSet::new();
    for (record, _alive) in list_ephemeral()? {
        if let TunnelSpec::Access { local_port, .. } = &record.spec {
            used.insert(*local_port);
        }
    }
    Ok(used)
}

/// The refusal both `gtunnel run` and the MCP launch give. A second run of a
/// name would overwrite the first one's record file, leaving a process nothing
/// can see or stop, and the first to exit would delete the other's record; a
/// managed tunnel of that name would share its log and its public hostname.
pub fn check_run_name(state: &TunnelState, account: &str, name: &str) -> Result<()> {
    let live = list_ephemeral()?
        .into_iter()
        .any(|(r, alive)| alive && r.account_name == account && r.name == name);
    if live {
        return Err(GtunnelError::usage(format!(
            "A run named '{name}' is already live for account '{account}'"
        ))
        .into());
    }
    if state.find_for_account(name, account).is_some() {
        return Err(GtunnelError::usage(format!(
            "Tunnel '{name}' already exists on account '{account}'"
        ))
        .into());
    }
    Ok(())
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

    let child = Command::new(gcloud_path)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, as the supervisor does: gcloud is a launcher, and a
        // signal to the group is the only way to reach ssh and the IAP proxy
        .process_group(0)
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
    let pid_start = crate::proc::start_token(pid);
    // Where a token can be read, none means the process is already gone, and a
    // record without one would be judged by the pid alone: the weaker rule
    if pid_start.is_none() && cfg!(any(target_os = "macos", target_os = "linux")) {
        crate::proc::signal_group(pid as i32, libc::SIGKILL);
        bail!(
            "{} exited before it could be recorded",
            gcloud_path.display()
        );
    }
    let record = EphemeralRecord {
        name: name.to_string(),
        account_name: acct.name.clone(),
        pid,
        spec,
        started_at: now_secs(),
        ttl_secs: ttl.map(|t| t.as_secs()),
        pid_start,
    };
    if let Err(e) = record.save() {
        // Nothing has connected yet, so there is no grace to give
        crate::proc::signal_group(pid as i32, libc::SIGKILL);
        return Err(e);
    }
    Ok((child, record))
}

/// What a run was asked to be, before a port is chosen for it.
enum RunShape {
    Access {
        target: AccessTarget,
        remote_port: u16,
        local_port: Option<u16>,
    },
    Public {
        local_target: String,
        domain: String,
    },
}

/// Decide the name and the ports, spawn gcloud and save the record as one step
/// under `tunnels.lock`. The ports a run takes are read back by the next
/// claimant from its record, so the choice and the save must not be separated
/// by another claimant's read: two launches could otherwise pick one port, and
/// a name could be claimed twice. Nothing in here awaits (decision 16).
fn claim_and_start(
    gcloud_path: &std::path::Path,
    acct: &crate::config::Account,
    expose: &Option<crate::config::ExposeConfig>,
    name: &str,
    shape: RunShape,
    ttl: Option<std::time::Duration>,
) -> Result<(tokio::process::Child, EphemeralRecord, PersistentTunnel)> {
    TunnelState::with_lock(|state| {
        check_run_name(state, &acct.name, name)?;
        let spec = match shape {
            RunShape::Access {
                target,
                remote_port,
                local_port,
            } => {
                let mut used = state.used_local_ports();
                used.extend(run_local_ports()?);
                let local_port = match local_port {
                    Some(port) => {
                        check_local_port(port, &used, local_port_is_free)?;
                        port
                    }
                    None => allocate_port(
                        name,
                        ACCESS_PORT_BASE,
                        ACCESS_PORT_SPAN,
                        &used,
                        local_port_is_free,
                    )?,
                };
                TunnelSpec::Access {
                    target,
                    remote_port,
                    local_port,
                }
            }
            RunShape::Public {
                local_target,
                domain,
            } => {
                let used = used_relay_ports(state, &acct.name)?;
                let relay_port =
                    allocate_port(name, RELAY_PORT_BASE, RELAY_PORT_SPAN, &used, |_| true)?;
                TunnelSpec::Expose {
                    local_target,
                    hostname: format!("{name}.{domain}"),
                    domain,
                    relay_port,
                    private: false,
                }
            }
        };
        let tunnel = PersistentTunnel {
            name: name.to_string(),
            account_name: acct.name.clone(),
            auto_start: false,
            spec: spec.clone(),
        };
        let (child, record) = start_child(gcloud_path, acct, expose, &tunnel, spec, name, ttl)?;
        Ok((child, record, tunnel))
    })
}

/// Undo a started run whose route could not be made: the group first, so
/// nothing holds the port, then the route and the record.
async fn abort_started(
    child: &mut tokio::process::Child,
    client: &GcpClient,
    acct: &crate::config::Account,
    expose: Option<&ExposeConfig>,
    tunnel: &PersistentTunnel,
    record: &EphemeralRecord,
) {
    if let Err(e) =
        crate::proc::stop_group(child, signalable(record.pid), crate::proc::STOP_GRACE).await
    {
        eprintln!("Failed to stop gcloud: {e:#}");
    }
    teardown(client, acct, expose, tunnel, record).await;
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
    let gcloud_path = gcloud::require_gcloud()?;

    // Everything that can be refused without looking at other runs is refused
    // here; the name and the ports are decided under the lock, in
    // `claim_and_start`
    let (shape, expose_config) = if public {
        let expose = acct.expose.clone().with_context(|| {
            format!(
                "No relay for account '{}'. Run `gtunnel relay up`",
                acct.name
            )
        })?;
        let domain = expose.domain(domain)?.clone();
        (
            RunShape::Public {
                local_target: target.clone(),
                domain: domain.domain.clone(),
            },
            Some(expose),
        )
    } else {
        let (host, remote_port) = crate::commands::access::parse_access_target(&target)?;
        crate::state::validate_instance_name(&host)?;
        if let Some(zone) = zone {
            crate::state::validate_zone(zone)?;
        }
        (
            RunShape::Access {
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

    let client = GcpClient::for_account(&acct);
    let (mut child, record, tunnel) =
        claim_and_start(&gcloud_path, &acct, &expose_config, &name, shape, ttl)?;
    // The route is created after gcloud starts, outside the lock (decision
    // 16): its reverse forward is up for the few seconds before the hostname
    // resolves, which serves nothing
    if let Some(expose) = &expose_config
        && let Err(e) = routes::add_route(&client, &acct, expose, &tunnel).await
    {
        abort_started(&mut child, &client, &acct, Some(expose), &tunnel, &record).await;
        return Err(e);
    }

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

    // Taken before the select: its `child.wait()` arm reaps gcloud, after which
    // `child.id()` is None and the group would have no name
    let pgid = signalable(record.pid);
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
        _ = ttl_elapsed(record.expires_at(), TTL_TICK, now_secs) => {
            println!("TTL reached, stopping");
        }
    }

    // Whichever arm won, the group goes the way the supervisor's does. When
    // gcloud exited by itself its ssh child may still hold the port, and the
    // sweep inside `stop_group` is what ends it.
    if let Err(e) = crate::proc::stop_group(&mut child, pgid, crate::proc::STOP_GRACE).await {
        eprintln!("Failed to stop gcloud: {e:#}");
    }
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
            pid_start: None,
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
            pid_start: None,
        };
        record.save().expect("save");
        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1, "an old record with pid 0 still parses");
        assert!(!found[0].1, "pid 0 must not read as a live run");
    }

    // Orphaned so the kernel reaps it: a child of this test would linger as a
    // zombie and still answer kill(pid, 0)
    fn orphan_sleep() -> u32 {
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
        pid
    }

    fn kill_pid(pid: u32) {
        // SAFETY: kill(2) takes two integers and no pointer. The pid is a sleep
        // this test spawned and still holds, so it is not a recycled pid.
        unsafe { libc::kill(signalable(pid).expect("a real pid"), libc::SIGKILL) };
    }

    fn record(name: &str) -> EphemeralRecord {
        EphemeralRecord {
            name: name.to_string(),
            account_name: "dev".to_string(),
            pid: 1,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
        }
    }

    // Importing a run SIGTERMs it, and the dying process removes its relay
    // route on the way out. Rebuilding the route before it is gone loses the
    // race, so the import has to wait for the pid first.
    #[tokio::test]
    async fn wait_for_exit_waits_for_a_live_run_and_reports_a_dead_one() {
        let pid = orphan_sleep();
        let run = EphemeralRecord {
            pid,
            pid_start: crate::proc::start_token(pid),
            ..record("gt-wait")
        };
        assert!(
            !run.wait_for_exit(Duration::from_millis(300)).await,
            "a live pid must not be reported as exited"
        );
        // The sleep is no group leader, so `terminate` (which signals the group
        // of a tokened record) would reach nothing: signal the pid itself
        // SAFETY: kill(2) takes two integers and no pointer. The pid is the
        // sleep spawned above, which is still running, so it is not recycled.
        unsafe { libc::kill(signalable(pid).expect("a real pid"), libc::SIGTERM) };
        assert!(
            run.wait_for_exit(Duration::from_secs(5)).await,
            "a killed pid must be reported as exited"
        );
    }

    #[test]
    fn a_record_whose_start_differs_reads_as_dead_and_terminate_sends_nothing() {
        let _temp = TempConfigDir::new();
        let pid = orphan_sleep();
        let run = EphemeralRecord {
            pid,
            pid_start: Some("0.000000".into()),
            ..record("gt-reuse")
        };
        run.save().unwrap();
        assert!(
            !list_ephemeral().unwrap()[0].1,
            "a recycled pid is not our run"
        );
        assert!(!run.terminate(), "nothing is sent to a stranger");
        assert!(pid_is_alive(pid), "the stranger is untouched");
        kill_pid(pid);
    }

    #[test]
    fn a_record_with_its_own_start_reads_as_alive() {
        let pid = orphan_sleep();
        let run = EphemeralRecord {
            pid,
            pid_start: crate::proc::start_token(pid),
            ..record("gt-live")
        };
        assert!(run.is_alive());
        kill_pid(pid);
    }

    #[tokio::test]
    async fn an_old_record_is_judged_by_its_pid_and_terminate_signals_the_pid_alone() {
        let pid = orphan_sleep(); // not a group leader of its own
        let old = format!(
            r#"{{"name":"gt-old","account_name":"dev","pid":{pid},"started_at":1700000000,
"spec":{{"mode":"access","target":{{"kind":"instance","instance":"my-vm","zone":"z"}},"remote_port":5432,"local_port":15000}}}}"#
        );
        let run: EphemeralRecord = serde_json::from_str(&old).expect("0.1.42 record parses");
        assert_eq!(run.pid_start, None);
        assert!(run.is_alive());
        assert!(!serde_json::to_string(&run).unwrap().contains("pid_start"));
        assert!(run.terminate());
        assert!(run.wait_for_exit(Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn terminate_signals_the_whole_group_of_a_new_record() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild");
        let mut leader = Command::new("sh")
            .args([
                "-c",
                &format!("sleep 30 & echo $! > {}; wait", pidfile.display()),
            ])
            .process_group(0)
            .spawn()
            .expect("spawn");
        let pid = leader.id().expect("pid");
        let run = EphemeralRecord {
            pid,
            pid_start: crate::proc::start_token(pid),
            ..record("gt-group")
        };
        let mut grandchild = 0;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(p) = text.trim().parse::<i32>()
            {
                grandchild = p;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(grandchild > 0, "the grandchild never reported its pid");
        assert!(run.terminate());
        let status = tokio::time::timeout(Duration::from_secs(5), leader.wait())
            .await
            .expect("the leader exits")
            .unwrap();
        assert!(!status.success(), "ended by SIGTERM: {status:?}");
        let mut gone = false;
        for _ in 0..40 {
            // SAFETY: signal 0 sends nothing; the pid is positive
            if unsafe { libc::kill(grandchild, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(gone, "pid {grandchild} survived the group SIGTERM");
    }

    // Suspend stops the monotonic clock, not the wall clock: an hour asleep is an
    // hour on `now` and nothing on tokio's timer
    #[tokio::test]
    async fn the_ttl_fires_on_the_wall_clock_when_the_monotonic_clock_is_behind() {
        let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000));
        let c = clock.clone();
        let fut = tokio::spawn(ttl_elapsed(
            Some(1_000 + 3_600),
            Duration::from_millis(10),
            move || c.load(std::sync::atomic::Ordering::SeqCst),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !fut.is_finished(),
            "an hour has not passed on the wall clock"
        );
        clock.store(1_000 + 3_601, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(Duration::from_millis(500), fut)
            .await
            .expect("fired")
            .unwrap();
    }

    #[tokio::test]
    async fn a_run_without_a_ttl_never_times_out() {
        let r = tokio::time::timeout(
            Duration::from_millis(100),
            ttl_elapsed(None, Duration::from_millis(10), || u64::MAX),
        )
        .await;
        assert!(r.is_err());
    }

    fn dev_account() -> crate::config::Account {
        crate::config::Account {
            name: "dev".to_string(),
            gcloud_configuration: "work".to_string(),
            project: "example-dev-1a2b".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            expose: None,
        }
    }

    // A stand-in for gcloud: the script runs as the spawned process, so its
    // pid is the one the record names
    fn fake_gcloud(dir: &std::path::Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("gcloud");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn a_spawned_run_records_its_start_and_leads_its_own_group() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let script = fake_gcloud(dir.path(), "exec sleep 30");
        let acct = dev_account();
        let tunnel = record("gt-grp").as_tunnel();
        let (mut child, run) =
            start_child(&script, &acct, &None, &tunnel, spec(), "gt-grp", None).expect("start");
        assert!(run.pid_start.is_some());
        assert_eq!(run.pid_start, crate::proc::start_token(run.pid));
        // SAFETY: getpgid takes an integer
        assert_eq!(unsafe { libc::getpgid(run.pid as i32) }, run.pid as i32);
        crate::proc::stop_group(&mut child, Some(run.pid as i32), Duration::from_millis(600))
            .await
            .expect("stop");
        run.remove().unwrap();
    }

    fn prod_account() -> crate::config::Account {
        crate::config::Account {
            name: "prod".to_string(),
            ..dev_account()
        }
    }

    fn access(local_port: Option<u16>) -> RunShape {
        RunShape::Access {
            target: AccessTarget::Instance {
                instance: "my-vm".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 5432,
            local_port,
        }
    }

    fn local_port(run: &EphemeralRecord) -> u16 {
        match &run.spec {
            TunnelSpec::Access { local_port, .. } => *local_port,
            TunnelSpec::Expose { .. } => panic!("an access run"),
        }
    }

    async fn stop(child: &mut tokio::process::Child, run: &EphemeralRecord) {
        crate::proc::stop_group(child, signalable(run.pid), Duration::from_millis(600))
            .await
            .expect("stop");
    }

    #[tokio::test]
    async fn check_run_name_refuses_a_live_run_with_launchs_message_and_a_managed_tunnel() {
        let _temp = TempConfigDir::new();
        let live = EphemeralRecord {
            pid: std::process::id(),
            pid_start: crate::proc::start_token(std::process::id()),
            ..record("db")
        };
        live.save().unwrap();
        let err = check_run_name(&TunnelState::default(), "dev", "db").unwrap_err();
        assert_eq!(
            err.to_string(),
            "A run named 'db' is already live for account 'dev'"
        );
        assert_eq!(crate::error::kind_of(&err), crate::error::ErrorKind::Usage);
        check_run_name(&TunnelState::default(), "prod", "db").expect("another account");

        let mut state = TunnelState::default();
        state.add(record("k8s").as_tunnel()).unwrap();
        assert_eq!(
            check_run_name(&state, "dev", "k8s")
                .unwrap_err()
                .to_string(),
            "Tunnel 'k8s' already exists on account 'dev'"
        );
        // A dead record does not block
        EphemeralRecord {
            pid: 999_999,
            ..record("old")
        }
        .save()
        .unwrap();
        check_run_name(&TunnelState::default(), "dev", "old").expect("stale record");
    }

    #[test]
    fn a_local_run_port_of_another_account_is_reserved() {
        let _temp = TempConfigDir::new();
        EphemeralRecord {
            account_name: "prod".to_string(),
            ..record("gt-other")
        }
        .save()
        .unwrap();
        assert!(run_local_ports().unwrap().contains(&15432));
    }

    #[tokio::test]
    async fn two_claims_of_one_name_in_two_accounts_get_different_ports() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let gcloud = fake_gcloud(dir.path(), "exec sleep 30");
        // The same name starts the search at the same port in both
        let (mut c1, a, _) =
            claim_and_start(&gcloud, &dev_account(), &None, "db", access(None), None).unwrap();
        let (mut c2, b, _) =
            claim_and_start(&gcloud, &prod_account(), &None, "db", access(None), None).unwrap();
        assert_ne!(local_port(&a), local_port(&b));
        stop(&mut c1, &a).await;
        stop(&mut c2, &b).await;
    }

    #[tokio::test]
    async fn a_second_claim_of_a_live_name_is_refused_and_spawns_nothing() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let gcloud = fake_gcloud(dir.path(), "exec sleep 30");
        let marked = tempfile::tempdir().unwrap();
        let marker = marked.path().join("ran");
        let marking = fake_gcloud(
            marked.path(),
            &format!("touch {}; exec sleep 30", marker.display()),
        );
        let (mut child, first, _) =
            claim_and_start(&gcloud, &dev_account(), &None, "db", access(None), None).unwrap();
        let err =
            claim_and_start(&marking, &dev_account(), &None, "db", access(None), None).unwrap_err();
        assert!(err.to_string().contains("already live"), "{err}");
        assert!(!marker.exists(), "no second gcloud");
        let stored: EphemeralRecord =
            serde_json::from_str(&std::fs::read_to_string(first.path().unwrap()).unwrap()).unwrap();
        assert_eq!(stored, first, "the first record is untouched");
        stop(&mut child, &first).await;
    }

    #[tokio::test]
    async fn an_explicit_port_another_accounts_tunnel_owns_is_refused() {
        let _temp = TempConfigDir::new();
        let owner = PersistentTunnel {
            account_name: "prod".to_string(),
            ..record("k8s").as_tunnel()
        };
        TunnelState::update(|state| state.add(owner)).unwrap();
        let marked = tempfile::tempdir().unwrap();
        let marker = marked.path().join("ran");
        let marking = fake_gcloud(
            marked.path(),
            &format!("touch {}; exec sleep 30", marker.display()),
        );
        let err = claim_and_start(
            &marking,
            &dev_account(),
            &None,
            "db",
            access(Some(15432)),
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("already used by another gtunnel tunnel"),
            "{err}"
        );
        assert!(!marker.exists());
        assert!(list_ephemeral().unwrap().is_empty());
    }

    #[test]
    fn claim_and_start_runs_under_the_tunnels_lock() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let gcloud = fake_gcloud(dir.path(), "exec sleep 30");
        let held = crate::fsutil::lock_exclusive(
            &crate::state::tunnels_lock_path().unwrap(),
            crate::fsutil::LOCK_DEADLINE,
        )
        .unwrap();
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let (mut child, run, _) =
                    claim_and_start(&gcloud, &dev_account(), &None, "db", access(None), None)?;
                stop(&mut child, &run).await;
                Ok::<_, anyhow::Error>(run)
            })
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            list_ephemeral().unwrap().is_empty(),
            "nothing recorded while the lock is held"
        );
        drop(held);
        let run = handle
            .join()
            .unwrap()
            .expect("claimed once the lock is free");
        assert_eq!(list_ephemeral().unwrap()[0].0, run);
    }

    #[tokio::test]
    async fn abort_started_stops_the_group_and_removes_the_record() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let gcloud = fake_gcloud(dir.path(), "exec sleep 30");
        let acct = dev_account();
        let (mut child, run, tunnel) =
            claim_and_start(&gcloud, &acct, &None, "db", access(None), None).unwrap();
        abort_started(
            &mut child,
            &GcpClient::for_account(&acct),
            &acct,
            None,
            &tunnel,
            &run,
        )
        .await;
        assert!(!run.is_alive());
        assert!(list_ephemeral().unwrap().is_empty());
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
            pid_start: None,
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
            pid_start: None,
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
            pid_start: None,
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
            pid_start: None,
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
            pid_start: None,
        };
        record.save().expect("save");

        let used = run_relay_ports("dev").expect("ports");
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
            pid_start: None,
        };
        record.save().expect("save");

        let used = run_relay_ports("dev").expect("ports");
        assert!(!used.contains(&30_124), "{used:?}");
    }
}
