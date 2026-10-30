use anyhow::{bail, Context, Result};
use std::collections::HashSet;

use super::app::{
    AccountEdit, EditTunnel, MoveTunnel, NewAccount, NewTunnel, Op, OpResult, RenameTunnel,
    TunnelEntry, TunnelKind, TunnelMode,
};
use crate::commands::access::{
    delete_tunnel, restart_tunnel, start_tunnel, stop_by_name, stop_tunnel, Started,
};
use crate::commands::{account, relocate};
use crate::commands::{find_tunnel, resolve};
use crate::config::load_config;
use crate::daemon;
use crate::discovery::ExternalTunnel;
use crate::ephemeral::list_ephemeral;
use crate::expose::routes;
use crate::gcp::GcpClient;
use crate::probe::{
    classify_probe, collect_metrics, collect_unmanaged, http_probe, probe_tunnel, HealthStatus,
    Trail, TCP_TIMEOUT,
};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelState, TunnelStatus};
use std::time::Duration;

// The health dial and the metrics dial are one probe to the TUI, so they wait
// the same time: a tunnel that answers in 3 s must not be healthy by one round
// and unhealthy by the other
const TCP_HEALTH_TIMEOUT: Duration = TCP_TIMEOUT;

// How long an imported run is given to let go of its ports and its route
const IMPORT_HANDOVER: Duration = Duration::from_secs(10);

// Everything the list shows for one account, read fresh
pub async fn load_entries(account: &str) -> Result<(Vec<TunnelEntry>, HashSet<u16>)> {
    let state = TunnelState::load()?;
    let mut entries = Vec::new();
    for tunnel in state.tunnels_for_account(account) {
        let label = daemon::platform_label(account, &tunnel.name);
        let status = daemon::status(&label).await;
        let mut entry = TunnelEntry::new(tunnel.clone(), status, TunnelKind::Managed);
        // Only a running tunnel is probed, and this one stopped itself so
        // the log would go quiet: without this the reason reads as `stopped`
        if status != TunnelStatus::Running
            && crate::commands::access::stopped_for_credentials(tunnel)
        {
            entry.health = HealthStatus::AuthExpired;
        } else if status != TunnelStatus::Running
            && crate::commands::access::stopped_for_budget(tunnel).is_some()
        {
            // The loop is the finding, and the supervisor ended it: the
            // existing crash-loop word and its "check the log" action say so
            entry.health = HealthStatus::CrashLooping;
        }
        entries.push(entry);
    }

    // An error, not an empty list: a run directory that cannot be read would
    // otherwise make every live run vanish from the screen with no word said.
    // The refresh fails into the status line and the rows already shown stay.
    for (record, alive) in list_ephemeral()? {
        if record.account_name != account {
            continue;
        }
        let mut entry = TunnelEntry::new(
            record.as_tunnel(),
            if alive {
                TunnelStatus::Running
            } else {
                TunnelStatus::Error
            },
            TunnelKind::Ephemeral,
        );
        entry.expires_at = record.expires_at();
        entries.push(entry);
    }

    let mut taken: HashSet<String> = entries.iter().map(|e| e.tunnel.name.clone()).collect();
    // Every account's tunnels and every run record, so another account's
    // tunnel is not listed a second time as a stranger
    let strangers = crate::discovery::classify(&crate::discovery::snapshot(), &state).await;
    for external in strangers {
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

    Ok((entries, used_ports(&state)))
}

/// The ports the add dialog must not suggest: saved tunnels and the live runs
/// of every account, the two things `gtunnel add` refuses a port for.
// A run record that cannot be read is treated as no runs: the list is drawn
// on every refresh and must not fail over a hint, and `add` checks again
fn used_ports(state: &TunnelState) -> HashSet<u16> {
    let mut used = state.used_local_ports();
    used.extend(
        crate::ephemeral::live_run_local_ports()
            .unwrap_or_default()
            .into_keys(),
    );
    used
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

// Every project the account can see, for the edit dialog's picker. The account
// is the one being edited, which need not be the one on screen.
pub async fn load_projects(account: &str) -> Result<Vec<super::app::ProjectChoice>> {
    let (_, acct) = resolve(Some(account))?;
    let client = GcpClient::for_account(&acct);
    Ok(crate::gcp::projects::list_projects(&client)
        .await?
        .into_iter()
        .map(|p| super::app::ProjectChoice {
            project_id: p.project_id,
            name: p.name,
        })
        .collect())
}

// The zones of the project the edit dialog chose, for its zone picker, sorted
// so a region's zones sit together
pub async fn load_zones(account: &str, project: &str) -> Result<Vec<String>> {
    let (_, acct) = resolve(Some(account))?;
    let client = GcpClient::for_account(&acct);
    let mut zones = crate::gcp::compute::list_zones(&client, project).await?;
    zones.sort();
    Ok(zones)
}

// Every gcloud configuration on this machine, for the add dialog's picker
pub async fn load_configurations() -> Result<Vec<super::app::ConfigurationChoice>> {
    Ok(crate::gcp::gcloud::list_configurations()
        .await?
        .into_iter()
        .map(|c| super::app::ConfigurationChoice {
            name: c.name,
            project: c.project,
            is_active: c.is_active,
        })
        .collect())
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

pub async fn probe_health(tunnel: &PersistentTunnel) -> HealthStatus {
    probe_health_at(tunnel, &Trail::of(tunnel)).await
}

async fn probe_health_at(tunnel: &PersistentTunnel, trail: &Trail) -> HealthStatus {
    match &tunnel.spec {
        TunnelSpec::Access { local_port, .. } => {
            // A tunnel started with --listen-on-stdin reveals no local port, so
            // there is nothing to dial: the port is unknown, not shut
            if *local_port == 0 {
                return HealthStatus::Unknown;
            }
            let addr = format!("127.0.0.1:{local_port}");
            let answer = probe_tunnel(&addr, TCP_HEALTH_TIMEOUT, &trail.log).await;
            classify_probe(answer, &trail.log, trail.marker.as_deref())
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

// `peaks` is what the sampler saw on each local port since the last round, so
// a round that happens to fall between two short requests still reports them
pub async fn collect(
    target: &MetricTarget,
    include_lb: bool,
    peaks: Option<&std::collections::HashMap<u16, u64>>,
) -> Option<crate::probe::TunnelMetrics> {
    let config = load_config().ok()?;
    Some(match target.pid {
        Some(pid) => collect_unmanaged(&config, &target.tunnel, pid, peaks).await,
        None => collect_metrics(&config, &target.tunnel, include_lb, peaks).await,
    })
}

// An op that writes config.toml, so the accounts the TUI holds are stale
// whether it succeeded or not: a half-made account is still an account
fn touches_accounts(op: &Op) -> bool {
    matches!(
        op,
        Op::AccountAdd(_) | Op::AccountEdit(_) | Op::AccountRemove(_) | Op::AccountSelect(_)
    )
}

// Runs on a tokio task; the UI thread never waits on gcloud or the API
pub async fn execute(account: &str, op: Op) -> OpResult {
    match run(account, &op).await {
        Ok(message) => OpResult {
            message,
            ok: true,
            reload: true,
            from_create: matches!(op, Op::Create(_)),
            from_account: touches_accounts(&op),
            name: pending_row(&op),
        },
        Err(e) => OpResult {
            message: format!("{e:#}"),
            ok: false,
            // A failed create may already have saved its record: `add` saves
            // before it installs and starts, so the list must be re-read or
            // the name check cannot see the half-made tunnel.
            reload: matches!(op, Op::Delete(_) | Op::Import(_) | Op::Create(_)),
            from_create: matches!(op, Op::Create(_)),
            from_account: touches_accounts(&op),
            name: pending_row(&op),
        },
    }
}

// The ops the key handler marks pending: the row they acted on has to leave
// that state when the op returns, whether it worked or not
fn pending_row(op: &Op) -> Option<String> {
    match op {
        Op::Start(n)
        | Op::Stop(n)
        | Op::StopRun(n)
        | Op::Restart(n)
        | Op::Delete(n)
        | Op::ToggleAutoStart(n) => Some(n.clone()),
        _ => None,
    }
}

async fn run(account: &str, op: &Op) -> Result<String> {
    match op {
        Op::Start(name) => start_tunnel(Some(account), name)
            .await
            .map(|s| running_line(&s)),
        Op::Stop(name) => {
            stop_tunnel(Some(account), name).await?;
            Ok(format!("{name} stopped"))
        }
        Op::StopRun(name) => {
            stop_by_name(Some(account), name).await?;
            Ok(format!("{name} stopped"))
        }
        Op::Restart(name) => restart_tunnel(Some(account), name)
            .await
            .map(|s| running_line(&s)),
        Op::Delete(name) => {
            delete_tunnel(Some(account), name).await?;
            Ok(format!("deleted {name}"))
        }
        Op::ToggleAutoStart(name) => toggle_auto_start(account, name).await,
        Op::Import(name) => import(account, name).await,
        Op::ClearLog(name) => clear_log(account, name),
        Op::Create(new) => create(new).await,
        Op::Edit(edit) => edit_tunnel(edit).await,
        Op::Rename(rename) => rename_tunnel(rename).await,
        Op::Move(moving) => move_tunnel(moving).await,
        Op::AccountAdd(new) => add_account(new).await,
        Op::AccountEdit(edit) => edit_account(edit),
        Op::AccountRemove(name) => remove_account(name),
        Op::AccountSelect(name) => select_account(name),
    }
}

// Both of these are the cores of the CLI's own commands, for the reason
// `create` calls `add_tunnel`: which names are free, what moves with a label and
// which tunnels may not leave their account at all are answered in one place,
// and the dialog is a second way to ask rather than a second set of rules. The
// cores, not the `cmd_` wrappers: a println under the alternate screen lands on
// the drawn page, so what the CLI would print goes into the status instead.
async fn rename_tunnel(rename: &RenameTunnel) -> Result<String> {
    let done =
        relocate::rename_tunnel(Some(&rename.account), &rename.name, &rename.new_name).await?;
    Ok(status_line(&done.message_with_warnings()))
}

// `interactive` is false: the terminal is in raw mode and nothing can answer a
// login prompt, so an expired destination login fails with the command to run
async fn move_tunnel(moving: &MoveTunnel) -> Result<String> {
    let done = relocate::move_tunnel(
        &moving.name,
        Some(&moving.from),
        &moving.to,
        moving.rename_to.as_deref(),
        false,
    )
    .await?;
    Ok(status_line(&done.message_with_warnings()))
}

// The status is one line, and the core's message is one per fact
fn status_line(message: &str) -> String {
    message.lines().collect::<Vec<_>>().join(". ")
}

// The four below are the cores of the CLI's own commands, for the reason
// `create` calls `cmd_add`: what an account may be called, which zone is real
// and whether an account may go at all are answered in one place, and the page
// is a second way to ask rather than a second set of rules. The cores, not the
// `cmd_` wrappers: a println under the alternate screen lands on top of the
// drawn page, so what the CLI would print goes into the status instead.
async fn add_account(new: &NewAccount) -> Result<String> {
    account::account_add(
        &new.configuration,
        &new.name,
        account::AddOptions::default(),
    )
    .await?;
    Ok(format!("added account {}", new.name))
}

fn edit_account(edit: &AccountEdit) -> Result<String> {
    let edited = account::account_edit(
        &edit.name,
        edit.project.as_deref(),
        edit.zone.as_deref(),
        None,
    )?;
    Ok(match account::stale_line(&edited.stale) {
        Some(line) => format!("{} updated. {line}", edit.name),
        None => format!("{} updated", edit.name),
    })
}

// The page has already asked the question `--yes` would answer. It does not
// answer the refusal underneath: an account that still holds tunnels is
// refused here all the same.
fn remove_account(name: &str) -> Result<String> {
    account::account_remove(name)?;
    Ok(format!("removed account {name}"))
}

fn select_account(name: &str) -> Result<String> {
    account::account_select(name)?;
    Ok(format!("selected account {name}"))
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
            let added = crate::commands::access::add_tunnel(
                Some(&new.account),
                crate::commands::access::AddRequest {
                    name: &new.name,
                    target: &target,
                    via: via.as_deref(),
                    zone: Some(&new.zone),
                    local_port: Some(new.local_port),
                    dest_group: None,
                    region: None,
                    network: None,
                    start: true,
                },
            )
            .await?;
            if !added.warnings.is_empty() {
                return Ok(format!(
                    "created {}. {}",
                    new.name,
                    added.warnings.join(". ")
                ));
            }
        }
        TunnelMode::Public => {
            // The cores, not `cmd_expose`: it prints, and so does `cmd_start`
            let req = crate::commands::expose::ExposeRequest {
                name: &new.name,
                target: &new.local_target,
                domain: Some(&new.domain),
                private: false,
            };
            let exposed = crate::commands::expose::expose_tunnel(Some(&new.account), &req).await?;
            // The route and record exist by now, so say so if the start fails
            start_tunnel(Some(&new.account), &new.name)
                .await
                .with_context(|| {
                    format!(
                        "{} was created at {} but not started",
                        new.name, exposed.url
                    )
                })?;
            if exposed.certificate_state != "ACTIVE" {
                return Ok(format!(
                    "created {}. certificate {}: HTTPS works once it is ACTIVE",
                    new.name, exposed.certificate_state
                ));
            }
        }
    }
    Ok(format!("created {}", new.name))
}

async fn edit_tunnel(edit: &EditTunnel) -> Result<String> {
    let (config, acct) = resolve(Some(&edit.account))?;
    let updated = TunnelState::update(|state| {
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

        Ok(tunnel.clone())
    })?;

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

// What the status line says once the core has seen the tunnel running; a tunnel
// that failed comes back as the core's error, whose text `execute` shows
fn running_line(started: &Started) -> String {
    format!("{} running on {}", started.name, started.url)
}

async fn toggle_auto_start(account: &str, name: &str) -> Result<String> {
    let (config, acct) = resolve(Some(account))?;
    let (now_on, spec) = TunnelState::update(|state| {
        let tunnel = state
            .find_for_account_mut(name, &acct.name)
            .with_context(|| format!("No tunnel named '{name}' in account '{}'", acct.name))?;
        tunnel.auto_start = !tunnel.auto_start;
        let spec = daemon::service_spec(&config, &tunnel.clone())?;
        Ok((tunnel.auto_start, spec))
    })?;
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

    let tunnel = PersistentTunnel {
        name: record.name.clone(),
        account_name: acct.name.clone(),
        auto_start: false,
        spec: record.spec.clone(),
    };
    TunnelState::update(|state| state.add(tunnel.clone()))?;
    daemon::install(&daemon::service_spec(&config, &tunnel)?).await?;
    // The run holds the local port and, for a public tunnel, the relay port;
    // the managed service cannot take over until it lets go
    // A run that already exited is not signalled, and the wait below treats it
    // the same as a clean exit
    record.terminate();
    let exited = record.wait_for_exit(IMPORT_HANDOVER).await;
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

    // The refresh fails into the status line; an empty list would make every
    // live run vanish without a word
    #[test]
    fn an_unreadable_run_directory_fails_the_refresh() {
        let dir = crate::config::testing::TempConfigDir::new();
        std::fs::write(dir.path().join("run"), "not a directory").unwrap();
        let loaded = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(load_entries("dev"));
        let error = loaded.expect_err("the run directory cannot be read");
        assert!(format!("{error:#}").contains("run"), "{error:#}");
    }

    // A `cmd_` account command prints its confirmation, and under the
    // alternate screen that line lands on the drawn page and stays there
    #[test]
    fn the_accounts_page_calls_the_cores_that_print_nothing() {
        let source = include_str!("ops.rs");
        let needle = concat!("account::", "cmd_");
        let calls: Vec<&str> = source.lines().filter(|l| l.contains(needle)).collect();
        assert!(calls.is_empty(), "printing commands called: {calls:?}");
    }

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

    #[test]
    fn the_ports_the_add_dialog_avoids_include_a_live_runs() {
        let _dir = crate::config::testing::TempConfigDir::new();
        crate::ephemeral::EphemeralRecord {
            name: "scratch".to_string(),
            account_name: "prod".to_string(),
            pid: std::process::id(),
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 17777,
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
        }
        .save()
        .unwrap();
        assert!(used_ports(&TunnelState::default()).contains(&17777));
    }

    // The TUI takes a row's health from the metrics probe when one ran, so the
    // two must not be able to disagree about the same outcome
    #[tokio::test]
    async fn the_metrics_probe_and_the_health_probe_give_the_same_verdict() {
        use tokio::io::AsyncWriteExt;
        let config = crate::config::Config::default();
        // Answers with a banner
        let answering = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let answering_port = answering.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = answering.accept().await {
                let _ = sock.write_all(b"SSH-2.0-OpenSSH_10.0p2\r\n").await;
            }
        });
        // Accepts and never says anything: a stalled tunnel
        let stalled = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let stalled_port = stalled.local_addr().expect("addr").port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = stalled.accept().await {
                held.push(sock);
            }
        });
        // Bound and dropped: the connection is refused
        let refused_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr").port()
        };
        // Closes without a byte after writing a refusal to the tunnel's log:
        // the case only the log can tell from a server that speaks last
        let log = std::env::temp_dir().join(format!("gtunnel-verdict-{}.log", std::process::id()));
        std::fs::write(&log, "").expect("write log");
        let refusing = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let refusing_port = refusing.local_addr().expect("addr").port();
        let writer = log.clone();
        tokio::spawn(async move {
            use std::io::Write;
            while let Ok((sock, _)) = refusing.accept().await {
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&writer)
                    .expect("open log");
                writeln!(f, "Error while connecting [4033: 'not authorized'].").expect("append");
                drop(sock);
            }
        });

        // Every probe reads a temp log and a temp marker, never the user's
        // config directory
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let quiet = dir.join(format!("gtunnel-quiet-{pid}.log"));
        std::fs::write(&quiet, "").expect("write log");
        // What a supervisor leaves after stopping the job for expired credentials
        let marker = dir.join(format!("gtunnel-auth-marker-{pid}"));
        std::fs::write(&marker, "").expect("write marker");
        let trail = |log: &std::path::Path, marker: Option<&std::path::Path>| Trail {
            log: log.to_path_buf(),
            marker: marker.map(std::path::Path::to_path_buf),
        };
        for (what, port, probe, expected) in [
            (
                "answered",
                answering_port,
                trail(&quiet, None),
                HealthStatus::Healthy,
            ),
            (
                "refused",
                refused_port,
                trail(&quiet, None),
                HealthStatus::Unhealthy,
            ),
            (
                "timed out",
                stalled_port,
                trail(&quiet, None),
                HealthStatus::Unhealthy,
            ),
            (
                "closed after a refusal",
                refusing_port,
                trail(&log, None),
                HealthStatus::Unhealthy,
            ),
            (
                "refused with a marker",
                refused_port,
                trail(&quiet, Some(&marker)),
                HealthStatus::AuthExpired,
            ),
            (
                "timed out with a marker",
                stalled_port,
                trail(&quiet, Some(&marker)),
                HealthStatus::AuthExpired,
            ),
            (
                "answered despite a marker",
                answering_port,
                trail(&quiet, Some(&marker)),
                HealthStatus::Healthy,
            ),
        ] {
            let tunnel = access("verdict", port);
            let from_health = probe_health_at(&tunnel, &probe).await;
            let from_metrics = crate::probe::collect_unmanaged_at(
                &config,
                &tunnel,
                std::process::id(),
                None,
                &probe,
            )
            .await
            .health;
            assert_eq!(from_health, expected, "{what}: the health probe");
            assert_eq!(from_metrics, Some(from_health), "{what}: the metrics probe");
        }
    }
}
