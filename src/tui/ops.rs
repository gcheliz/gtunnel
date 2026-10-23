use anyhow::{Context, Result};
use std::collections::HashSet;

use super::app::{EditTunnel, NewTunnel, Op, OpResult, TunnelEntry, TunnelKind, TunnelMode};
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

// Every instance the account can reach, for the picker
pub async fn load_instances(account: &str) -> Result<Vec<super::app::InstanceChoice>> {
    let (_, acct) = resolve(Some(account))?;
    let client = GcpClient::for_account(&acct);
    let mut out: Vec<super::app::InstanceChoice> =
        crate::gcp::compute::list_instances(&client, &acct.project)
            .await?
            .into_iter()
            .map(|i| super::app::InstanceChoice {
                name: i.name,
                zone: i.zone,
            })
            .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
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
        Op::Create(new) => create(new).await,
        Op::Edit(edit) => edit_tunnel(edit).await,
    }
}

// The CLI commands are the only place that knows how to validate a target and
// build a relay route, so the TUI calls them rather than keeping a second copy
async fn create(new: &NewTunnel) -> Result<String> {
    match new.mode {
        TunnelMode::Access => {
            crate::commands::access::cmd_add(
                Some(&new.account),
                &new.name,
                &format!("{}:{}", new.instance, new.remote_port),
                Some(&new.zone),
                Some(new.local_port),
                None,
                None,
                None,
                true,
            )
            .await?
        }
        TunnelMode::Public => {
            crate::commands::expose::cmd_expose(
                Some(&new.account),
                &new.name,
                &new.local_target,
                Some(&new.domain),
                false,
                true,
            )
            .await?
        }
    }
    Ok(format!("created {}", new.name))
}

async fn edit_tunnel(edit: &EditTunnel) -> Result<String> {
    let (config, acct) = resolve(Some(&edit.account))?;
    let mut state = TunnelState::load()?;
    let tunnel = state
        .find_for_account_mut(&edit.name, &acct.name)
        .with_context(|| format!("No tunnel named '{}'", edit.name))?;

    match &mut tunnel.spec {
        TunnelSpec::Access {
            remote_port,
            local_port,
            ..
        } => {
            let (remote, local) = edit
                .value
                .split_once(':')
                .context("Write the new ports as <remote>:<local>")?;
            *remote_port = remote.trim().parse().context("Remote port is not a port")?;
            *local_port = local.trim().parse().context("Local port is not a port")?;
        }
        TunnelSpec::Expose {
            local_target,
            private,
            ..
        } => match edit.value.trim() {
            "private" => *private = true,
            "public" => *private = false,
            other => *local_target = other.to_string(),
        },
    }

    let updated = tunnel.clone();
    state.save()?;

    // A public tunnel's access rule lives on the backend, not in tunnels.toml
    if updated.is_expose() {
        let client = GcpClient::for_account(&acct);
        routes::ensure_route(&client, &acct, &updated).await?;
    }

    let spec = daemon::service_spec(&config, &updated)?;
    let was_running = daemon::status(&spec.label).await == TunnelStatus::Running;
    daemon::install(&spec).await?;
    if was_running {
        daemon::start(&spec.label).await?;
    }
    Ok(format!("{} updated", edit.name))
}

async fn start(account: &str, name: &str) -> Result<String> {
    let (config, acct) = resolve(Some(account))?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    if tunnel.is_expose() {
        let client = GcpClient::for_account(&acct);
        routes::ensure_route(&client, &acct, &tunnel).await?;
    }
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
    // The run holds the local port and, for a public tunnel, the relay port;
    // the managed service cannot take over until it lets go
    #[cfg(unix)]
    unsafe {
        libc::kill(record.pid as libc::pid_t, libc::SIGTERM);
    }
    record.remove()?;
    Ok(format!("{name} is now managed"))
}
