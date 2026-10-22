use anyhow::{Context, Result};
use std::collections::HashSet;

use super::app::{Op, OpResult, TunnelEntry, TunnelKind};
use crate::commands::{find_tunnel, resolve};
use crate::config::load_config;
use crate::daemon;
use crate::discovery::{external_tunnels, ExternalTunnel};
use crate::ephemeral::list_ephemeral;
use crate::expose::routes;
use crate::gcp::GcpClient;
use crate::probe::{collect_metrics, http_probe, tcp_probe, HealthStatus};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelState, TunnelStatus};
use std::time::Duration;

const TCP_HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

// Everything the list shows for one account, read fresh
pub async fn load_entries(account: &str) -> Result<Vec<TunnelEntry>> {
    let state = TunnelState::load()?;
    let mut entries = Vec::new();
    let mut managed_pids: HashSet<u32> = HashSet::new();

    for tunnel in state.tunnels_for_account(account) {
        let label = daemon::platform_label(account, &tunnel.name);
        let status = daemon::status(&label).await;
        if let Some(pid) = daemon::runtime_info(&label).await.pid {
            managed_pids.insert(pid);
        }
        entries.push(TunnelEntry::new(
            tunnel.clone(),
            status,
            TunnelKind::Managed,
        ));
    }

    for (record, alive) in list_ephemeral().unwrap_or_default() {
        if record.account_name != account {
            continue;
        }
        managed_pids.insert(record.pid);
        entries.push(TunnelEntry::new(
            PersistentTunnel {
                name: record.name.clone(),
                account_name: record.account_name.clone(),
                auto_start: false,
                spec: record.spec.clone(),
            },
            if alive {
                TunnelStatus::Running
            } else {
                TunnelStatus::Error
            },
            TunnelKind::Ephemeral,
        ));
    }

    for external in external_tunnels(&managed_pids) {
        let mut entry = TunnelEntry::new(
            external_as_tunnel(account, &external),
            TunnelStatus::Running,
            TunnelKind::External,
        );
        entry.external = Some(external);
        entries.push(entry);
    }

    Ok(entries)
}

// An unmanaged gcloud process has no record, so the list builds one to show it
fn external_as_tunnel(account: &str, external: &ExternalTunnel) -> PersistentTunnel {
    PersistentTunnel {
        name: format!("pid-{}", external.pid),
        account_name: account.to_string(),
        auto_start: false,
        spec: TunnelSpec::Access {
            target: AccessTarget::Instance {
                instance: external.target.clone(),
                zone: external.zone.clone().unwrap_or_default(),
            },
            remote_port: external.remote_port,
            local_port: external.local_port.unwrap_or(0),
        },
    }
}

pub fn read_logs(tunnel: &PersistentTunnel, lines: usize) -> Vec<String> {
    let Ok(path) = tunnel.log_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

pub async fn probe_health(tunnel: &PersistentTunnel) -> HealthStatus {
    match &tunnel.spec {
        TunnelSpec::Access { local_port, .. } => {
            match tcp_probe(&format!("127.0.0.1:{local_port}"), TCP_HEALTH_TIMEOUT).await {
                Some(_) => HealthStatus::Healthy,
                None => HealthStatus::Unhealthy,
            }
        }
        TunnelSpec::Expose { hostname, .. } => http_probe(&format!("https://{hostname}")).await,
    }
}

pub async fn collect(
    account: &str,
    name: &str,
    include_lb: bool,
) -> Option<crate::probe::TunnelMetrics> {
    let config = load_config().ok()?;
    let state = TunnelState::load().ok()?;
    let tunnel = state.find_for_account(name, account)?;
    Some(collect_metrics(&config, tunnel, include_lb).await)
}

// Runs on a tokio task; the UI thread never waits on gcloud or the API
pub async fn execute(account: &str, op: Op) -> OpResult {
    match run(account, &op).await {
        Ok(message) => OpResult {
            message,
            ok: true,
            reload: true,
        },
        Err(e) => OpResult {
            message: format!("{e:#}"),
            ok: false,
            reload: matches!(op, Op::Delete(_) | Op::Import(_)),
        },
    }
}

async fn run(account: &str, op: &Op) -> Result<String> {
    match op {
        Op::Start(name) => start(account, name).await,
        Op::Stop(name) => {
            daemon::stop(&daemon::platform_label(account, name)).await?;
            Ok(format!("{name} stopped"))
        }
        Op::Restart(name) => {
            let _ = daemon::stop(&daemon::platform_label(account, name)).await;
            start(account, name).await
        }
        Op::Delete(name) => delete(account, name).await,
        Op::ToggleAutoStart(name) => toggle_auto_start(account, name).await,
        Op::Import(name) => import(account, name).await,
        Op::Create(_) | Op::Edit(_) => {
            anyhow::bail!("creating and editing tunnels is done from the dialogs")
        }
    }
}

async fn start(account: &str, name: &str) -> Result<String> {
    let (config, acct) = resolve(Some(account))?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    let spec = daemon::service_spec(&config, &tunnel)?;
    daemon::install(&spec).await?;
    daemon::start(&spec.label).await?;
    Ok(format!("{name} starting"))
}

async fn delete(account: &str, name: &str) -> Result<String> {
    let (_, acct) = resolve(Some(account))?;
    let mut state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;

    // The relay route goes first: a backend left behind keeps serving a 503
    if tunnel.is_expose() {
        let expose = acct
            .expose
            .clone()
            .context("This account has no relay, but the tunnel is an Expose tunnel")?;
        let client = GcpClient::for_account(&acct);
        routes::remove_route(&client, &acct, &expose, &tunnel).await?;
    }

    let label = daemon::platform_label(&acct.name, name);
    let _ = daemon::stop(&label).await;
    daemon::uninstall(&label).await?;
    state.remove_for_account(name, &acct.name);
    state.save()?;
    Ok(format!("deleted {name}"))
}

async fn toggle_auto_start(account: &str, name: &str) -> Result<String> {
    let (config, acct) = resolve(Some(account))?;
    let mut state = TunnelState::load()?;
    let tunnel = state
        .find_for_account_mut(name, &acct.name)
        .with_context(|| format!("No tunnel named '{name}' in account '{}'", acct.name))?;
    tunnel.auto_start = !tunnel.auto_start;
    let now_on = tunnel.auto_start;
    let spec = daemon::service_spec(&config, &tunnel.clone())?;
    state.save()?;
    daemon::set_auto_start(&spec).await?;
    Ok(format!(
        "{name} auto-start {}",
        if now_on { "on" } else { "off" }
    ))
}

// An ephemeral run keeps its port until it exits; importing records it so the
// next start is gtunnel's
async fn import(account: &str, name: &str) -> Result<String> {
    let (config, acct) = resolve(Some(account))?;
    let (record, _) = list_ephemeral()?
        .into_iter()
        .find(|(r, _)| r.name == name && r.account_name == acct.name)
        .with_context(|| format!("No run named '{name}'"))?;

    let mut state = TunnelState::load()?;
    let tunnel = PersistentTunnel {
        name: record.name.clone(),
        account_name: acct.name.clone(),
        auto_start: false,
        spec: record.spec.clone(),
    };
    state.add(tunnel.clone())?;
    state.save()?;
    daemon::install(&daemon::service_spec(&config, &tunnel)?).await?;
    record.remove()?;
    Ok(format!("{name} is now managed"))
}
