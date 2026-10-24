use anyhow::{bail, Context, Result};
use std::collections::HashSet;

use super::app::{EditTunnel, NewTunnel, Op, OpResult, TunnelEntry, TunnelKind, TunnelMode};
use crate::commands::{find_tunnel, resolve};
use crate::config::load_config;
use crate::daemon;
use crate::discovery::{external_tunnels, ExternalTunnel};
use crate::ephemeral::list_ephemeral;
use crate::expose::routes;
use crate::gcp::GcpClient;
use crate::probe::{collect_metrics, collect_unmanaged, http_probe, tcp_probe, HealthStatus};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelState, TunnelStatus};
use std::time::Duration;

// How long an imported run is given to let go of its ports and its route
const IMPORT_HANDOVER: Duration = Duration::from_secs(10);
const TCP_HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

// Everything the list shows for one account, read fresh
pub async fn load_entries(account: &str) -> Result<Vec<TunnelEntry>> {
    let state = TunnelState::load()?;
    let mut entries = Vec::new();
    // Every gtunnel pid on the machine, so another account's tunnel is not
    // listed a second time as a stranger
    let managed_pids: HashSet<u32> = crate::discovery::managed_pids().await;

    for tunnel in state.tunnels_for_account(account) {
        let label = daemon::platform_label(account, &tunnel.name);
        let status = daemon::status(&label).await;
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

    let mut taken: HashSet<String> = entries.iter().map(|e| e.tunnel.name.clone()).collect();
    for external in external_tunnels(&managed_pids) {
        let name = external_name(&external, &taken);
        taken.insert(name.clone());
        let mut entry = TunnelEntry::new(
            external_as_tunnel(account, &external, name),
            TunnelStatus::Running,
            TunnelKind::External,
        );
        entry.external = Some(external);
        entries.push(entry);
    }

    Ok(entries)
}

/// What to call a `gcloud` process gtunnel did not start.
///
/// The pid is unique and the target is not, which is why the name used to be
/// `pid-NNNNN` — but the pid is also the one fact the reader cannot act on,
/// because every action on the row is refused. What the row is for is saying
/// which service already holds a port, so the target says it, and the pid
/// comes back only where it has to: two strangers on the same target and port,
/// or a target a name in this account already spells.
fn external_name(external: &ExternalTunnel, taken: &HashSet<String>) -> String {
    let base = format!("{}:{}", external.target, external.remote_port);
    match taken.contains(&base) {
        true => format!("{base}@{}", external.pid),
        false => base,
    }
}

// An unmanaged gcloud process has no record, so the list builds one to show it
fn external_as_tunnel(account: &str, external: &ExternalTunnel, name: String) -> PersistentTunnel {
    PersistentTunnel {
        name,
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
    let text = daemon::read_tail(&path, daemon::TAIL_WINDOW);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

// Empties one tunnel's log without touching the service behind it: the process
// keeps its handle on the file, so the next line it writes lands in the file the
// panel is reading.
fn clear_log(account: &str, name: &str) -> Result<String> {
    let (_, acct) = resolve(Some(account))?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    let path = tunnel.log_path()?;
    daemon::reset_log(&path)?;
    // Otherwise the pane the user just cleared reads like a tunnel with nothing
    // to say, which is the state this whole pass exists to tell apart
    let _ = daemon::log_event(&path, "log cleared");
    Ok(format!("{name} log cleared"))
}

// A stop has nothing left to fail: the line is best effort, and the tunnel is
// stopped either way
fn log_stopped(account: &str, name: &str) {
    let Ok(state) = TunnelState::load() else {
        return;
    };
    let Some(tunnel) = state.find_for_account(name, account) else {
        return;
    };
    if let Ok(path) = tunnel.log_path() {
        let _ = daemon::log_event(&path, "stopped by gtunnel");
    }
}

pub async fn probe_health(tunnel: &PersistentTunnel) -> HealthStatus {
    match &tunnel.spec {
        TunnelSpec::Access { local_port, .. } => {
            // A tunnel started with --listen-on-stdin reveals no local port, so
            // there is nothing to dial: the port is unknown, not shut
            if *local_port == 0 {
                return HealthStatus::Unknown;
            }
            match tcp_probe(&format!("127.0.0.1:{local_port}"), TCP_HEALTH_TIMEOUT).await {
                Some(_) => HealthStatus::Healthy,
                None => HealthStatus::Unhealthy,
            }
        }
        TunnelSpec::Expose { hostname, .. } => http_probe(&format!("https://{hostname}")).await,
    }
}

// One row to measure this round. An unmanaged tunnel has no service to ask, so
// it brings the pid `ps` gave it; a managed one is read from its service.
#[derive(Debug, Clone)]
pub struct MetricTarget {
    pub tunnel: PersistentTunnel,
    pub pid: Option<u32>,
}

// Latency, peak and the chart belong to the port, not to whoever opened it: a
// running tunnel is measured whether or not gtunnel manages it, and a row whose
// port is unknown has nothing to measure.
pub fn metric_targets(entries: &[TunnelEntry]) -> Vec<MetricTarget> {
    entries
        .iter()
        .filter(|e| e.status == TunnelStatus::Running)
        .filter(|e| !matches!(e.tunnel.spec, TunnelSpec::Access { local_port: 0, .. }))
        .map(|e| MetricTarget {
            tunnel: e.tunnel.clone(),
            pid: e.external.as_ref().map(|x| x.pid),
        })
        .collect()
}

pub async fn collect(
    target: &MetricTarget,
    include_lb: bool,
) -> Option<crate::probe::TunnelMetrics> {
    let config = load_config().ok()?;
    Some(match target.pid {
        Some(pid) => collect_unmanaged(&config, &target.tunnel, pid).await,
        None => collect_metrics(&config, &target.tunnel, include_lb).await,
    })
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
            log_stopped(account, name);
            Ok(format!("{name} stopped"))
        }
        Op::Restart(name) => {
            let _ = daemon::stop(&daemon::platform_label(account, name)).await;
            start(account, name).await
        }
        Op::Delete(name) => delete(account, name).await,
        Op::ToggleAutoStart(name) => toggle_auto_start(account, name).await,
        Op::Import(name) => import(account, name).await,
        Op::ClearLog(name) => clear_log(account, name),
        Op::Create(new) => create(new).await,
        Op::Edit(edit) => edit_tunnel(edit).await,
    }
}

// What the draft means to `add`: a tunnel to a host the instance reaches names
// that host as the target and the instance as the way to it, while a tunnel to
// the instance's own port names the instance
pub fn add_arguments(new: &NewTunnel) -> (String, Option<String>) {
    if new.host.trim().is_empty() {
        (format!("{}:{}", new.instance, new.remote_port), None)
    } else {
        (
            format!("{}:{}", new.host.trim(), new.remote_port),
            Some(new.instance.clone()),
        )
    }
}

// The CLI commands are the only place that knows how to validate a target and
// build a relay route, so the TUI calls them rather than keeping a second copy
async fn create(new: &NewTunnel) -> Result<String> {
    match new.mode {
        TunnelMode::Access => {
            let (target, via) = add_arguments(new);
            crate::commands::access::cmd_add(
                Some(&new.account),
                &new.name,
                &target,
                via.as_deref(),
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
    // A tunnel that started is started whether or not its log took the line
    let _ = daemon::log_event(
        &spec.log_path,
        &format!("starting {name} on {}", tunnel.display_url()),
    );
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
    let log_path = tunnel.log_path()?;
    let _ = daemon::stop(&label).await;
    daemon::uninstall(&label).await?;
    // The log goes with the tunnel: nothing is left to own it
    daemon::remove_log(&log_path)?;
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
    if let Some(pid) = crate::ephemeral::signalable(record.pid) {
        // SAFETY: kill(2) takes two integers and no pointer, so it cannot touch
        // our memory. The pid is the one in the run's own record, which this
        // function loaded a moment ago, and `signalable` has rejected 0 and
        // negative values so it names one process and never a group. If the run
        // already exited the call fails with ESRCH, which the wait below treats
        // the same as a clean exit.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    let exited = crate::ephemeral::wait_for_pid_exit(record.pid, IMPORT_HANDOVER).await;
    record.remove()?;

    // The run tears its own route down as it dies, taking the backend, host
    // rule and DNS record the imported tunnel now owns, so the route is
    // rebuilt once the pid is gone and not a moment before
    if tunnel.is_expose() {
        if !exited {
            bail!(
                "Run {} did not exit within {}s, so '{name}' was recorded but its route was left \
                 to the run. Stop that process, then run `gtunnel restart {name}`",
                record.pid,
                IMPORT_HANDOVER.as_secs()
            );
        }
        let client = GcpClient::for_account(&acct);
        routes::ensure_route(&client, &acct, &tunnel).await?;
    }
    Ok(format!("{name} is now managed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(name: &str, local_port: u16) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 22,
                local_port,
            },
        }
    }

    // An unmanaged tunnel started with --listen-on-stdin has no local port in
    // its `ps` line, so there is nothing to dial. That is a port gtunnel does
    // not know, not a tunnel that is down, and the row said down.
    #[tokio::test]
    async fn a_tunnel_with_no_local_port_is_unknown_rather_than_unhealthy() {
        assert_eq!(
            probe_health(&access("pid-72420", 0)).await,
            HealthStatus::Unknown
        );
    }

    // The chart and the latency are a property of the port, not of who started
    // it: an unmanaged tunnel whose port is known is measured like any other,
    // and one whose port is unknown has nothing to measure
    #[test]
    fn metrics_are_collected_for_an_unmanaged_tunnel_that_has_a_port() {
        let mut known = TunnelEntry::new(
            access("pid-1", 5455),
            TunnelStatus::Running,
            TunnelKind::External,
        );
        known.external = Some(ExternalTunnel {
            pid: 1,
            target: "bastion".to_string(),
            remote_port: 22,
            local_port: Some(5455),
            zone: None,
        });
        let unknown = TunnelEntry::new(
            access("pid-2", 0),
            TunnelStatus::Running,
            TunnelKind::External,
        );
        let stopped = TunnelEntry::new(
            access("db", 6000),
            TunnelStatus::Stopped,
            TunnelKind::Managed,
        );
        let running = TunnelEntry::new(
            access("api", 6001),
            TunnelStatus::Running,
            TunnelKind::Managed,
        );

        let targets = metric_targets(&[known, unknown, stopped, running]);
        let names: Vec<&str> = targets.iter().map(|t| t.tunnel.name.as_str()).collect();
        assert_eq!(names, vec!["pid-1", "api"], "{names:?}");
        assert_eq!(targets[0].pid, Some(1), "an unmanaged row carries its pid");
        assert_eq!(
            targets[1].pid, None,
            "a managed row is read from its service"
        );
    }

    fn stranger(pid: u32, target: &str, remote_port: u16) -> ExternalTunnel {
        ExternalTunnel {
            pid,
            target: target.to_string(),
            remote_port,
            local_port: Some(15379),
            zone: None,
        }
    }

    // The pid was the only unique thing about a stranger, but it is also the
    // only thing the reader cannot use: the row refuses every action, so the
    // name's whole job is to say which service already holds the port.
    #[test]
    fn an_unmanaged_tunnel_is_named_for_its_target_rather_than_its_pid() {
        let taken = HashSet::new();
        assert_eq!(
            external_name(&stranger(48210, "cache-1", 6379), &taken),
            "cache-1:6379"
        );
    }

    #[test]
    fn two_unmanaged_tunnels_to_one_target_are_told_apart_by_pid() {
        let mut taken = HashSet::new();
        let first = external_name(&stranger(48210, "cache-1", 6379), &taken);
        taken.insert(first.clone());
        let second = external_name(&stranger(48211, "cache-1", 6379), &taken);
        assert_eq!(first, "cache-1:6379");
        assert_eq!(second, "cache-1:6379@48211");
    }

    // A stranger reaching a target a managed tunnel already names would
    // otherwise shadow it in a list keyed by name
    #[test]
    fn an_unmanaged_name_gives_way_to_a_name_the_account_already_uses() {
        let taken = HashSet::from(["cache-1:6379".to_string()]);
        assert_eq!(
            external_name(&stranger(48210, "cache-1", 6379), &taken),
            "cache-1:6379@48210"
        );
    }
}
