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
}

pub fn run_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("run");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir)
}

impl EphemeralRecord {
    pub fn path(&self) -> Result<PathBuf> {
        Ok(run_dir()?.join(format!("{}-{}.json", self.account_name, self.name)))
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path()?;
        let json = serde_json::to_string_pretty(self).context("Failed to encode the run record")?;
        std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))
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

pub fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 takes two integers and no pointer, and sends
    // nothing: it only reports whether the pid exists and may be signalled, so
    // there is no memory or process state to corrupt.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
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
) -> Result<(tokio::process::Child, EphemeralRecord)> {
    let argv = match expose_config {
        Some(expose) => expose_argv(acct, expose, tunnel)?,
        None => access_argv(acct, tunnel)?,
    };

    let mut child = Command::new(gcloud_path)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run {}", gcloud_path.display()))?;

    let record = EphemeralRecord {
        name: name.to_string(),
        account_name: acct.name.clone(),
        pid: child.id().unwrap_or(0),
        spec,
        started_at: now_secs(),
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

    let started = start_child(&gcloud_path, &acct, &expose_config, &tunnel, spec, &name);
    let (mut child, record) = match started {
        Ok(started) => started,
        Err(e) => {
            // The route is already published; leaving it would serve a
            // hostname with nothing behind it
            if let Some(expose) = &expose_config {
                if let Err(cleanup) = routes::remove_route(&client, &acct, expose, &tunnel).await {
                    eprintln!("Failed to remove the relay route: {cleanup:#}");
                    eprintln!("Run `gtunnel doctor --fix` to clean it up");
                }
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
    println!("Ctrl+C to stop");

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
            if let Ok(status) = status {
                if !status.success() {
                    println!("gcloud exited with {status}");
                }
            }
        }
        _ = stop_signal() => {
            println!();
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
        };
        record.save().expect("save");

        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, record);
        assert!(found[0].1, "the current process is alive");

        record.remove().expect("remove");
        assert!(list_ephemeral().expect("list").is_empty());
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
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
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
        };
        record.save().expect("save");

        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1);
        assert!(!found[0].1, "pid 999999 should not be running");
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
        };
        record.save().expect("save");

        let used = ephemeral_ports("dev", true).expect("ports");
        assert!(!used.contains(&30_124), "{used:?}");
    }
}
