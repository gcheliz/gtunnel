use anyhow::Result;
use std::collections::HashSet;

use crate::commands::{confirm, resolve};
use crate::config::{Account, Config};
use crate::ephemeral::{list_ephemeral, EphemeralRecord};
use crate::expose::naming::Names;
use crate::expose::routes;
use crate::gcp::compute::{BackendService, NamedPort, UrlMap};
use crate::gcp::dns::RecordSet;
use crate::gcp::{certmanager, compute, dns, gcloud, iap, GcpClient};
use crate::probe::Holder;
use crate::state::{local_port_is_free, AccessTarget, PersistentTunnel, TunnelSpec, TunnelState};

pub const TUNNEL_ROLE: &str = "roles/iap.tunnelResourceAccessor";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    GcloudMissing,
    GcloudAuth {
        account: String,
    },
    NumpyMissing,
    StaleState {
        tunnel: String,
    },
    // A service on this machine that no record in tunnels.toml owns: the mirror of
    // StaleState. Machine-wide, so it carries a label and no account.
    OrphanService {
        label: String,
    },
    MissingInstance {
        tunnel: String,
    },
    FirewallMissing {
        tunnel: String,
        command: String,
    },
    IamMissing {
        tunnel: String,
        role: &'static str,
    },
    PortConflict {
        tunnel: String,
        port: u16,
    },
    // tunnels.toml says one thing about starting at login, the service manager
    // another. The record is what the user edits, so the record wins the fix.
    AutoStartDrift {
        tunnel: String,
        record: bool,
        installed: bool,
    },
    DeadEphemeral {
        name: String,
    },
    RelayMissing {
        account: String,
    },
    CertNotActive {
        domain: String,
        state: String,
    },
    OrphanDns {
        zone: String,
        fqdn: String,
    },
    UnknownDns {
        zone: String,
        fqdn: String,
    },
    OrphanBackend {
        name: String,
    },
    OrphanHostRule {
        host: String,
    },
    OrphanNamedPort {
        name: String,
    },
    // One job for the whole machine, so it carries no account or tunnel
    WatchdogMissing,
    // An account made before `run` and `run-*` were reserved. It keeps
    // working; only its log files can collide with a run's, so the person
    // decides when to rename it
    ReservedAccountName {
        account: String,
    },
    // The tunnel's own log says it ran out of descriptors, with whoever is
    // holding its connections now. None when lsof is not there to ask
    DescriptorsExhausted {
        tunnel: String,
        holder: Option<Holder>,
    },
    // A key in one of the two hand-edited files that the structs drop on load.
    // Report only: which of the file's keys was meant is the person's call,
    // and gtunnel rewriting a file they edit would lose their layout.
    UnknownKey {
        file: String,
        path: String,
    },
}

impl Finding {
    // gtunnel removes only what it created; everything else is explained so the
    // person decides
    pub fn fixable(&self) -> bool {
        matches!(
            self,
            Finding::StaleState { .. }
                | Finding::OrphanService { .. }
                | Finding::AutoStartDrift { .. }
                | Finding::DeadEphemeral { .. }
                | Finding::OrphanDns { .. }
                | Finding::OrphanBackend { .. }
                | Finding::OrphanHostRule { .. }
                | Finding::OrphanNamedPort { .. }
                | Finding::WatchdogMissing
        )
    }

    pub fn describe(&self) -> String {
        match self {
            Finding::GcloudMissing => {
                "gcloud is not on PATH. Install the Google Cloud SDK".to_string()
            }
            Finding::GcloudAuth { account } => format!(
                "gcloud has no usable credentials for account '{account}'. Run `gcloud auth login`"
            ),
            Finding::NumpyMissing => {
                "gcloud's Python is missing numpy, which makes IAP tunnels slow. Run \
                 `gcloud components reinstall`"
                    .to_string()
            }
            Finding::StaleState { tunnel } => {
                format!("Tunnel '{tunnel}' is in tunnels.toml but has no service installed")
            }
            Finding::OrphanService { label } => format!(
                "Service {label} is installed but no tunnel in tunnels.toml owns it, so it may \
                 still hold a local port at login"
            ),
            Finding::MissingInstance { tunnel } => {
                format!("Tunnel '{tunnel}' points at an instance that no longer exists")
            }
            Finding::FirewallMissing { tunnel, command } => {
                format!("Tunnel '{tunnel}' has no firewall rule for IAP:\n    {command}")
            }
            Finding::IamMissing { tunnel, role } => {
                format!("Tunnel '{tunnel}' needs {role} on the instance or project")
            }
            Finding::PortConflict { tunnel, port } => {
                format!("Tunnel '{tunnel}' wants local port {port}, which is already in use")
            }
            Finding::AutoStartDrift {
                tunnel,
                record,
                installed,
            } => {
                if *installed {
                    format!(
                        "Tunnel '{tunnel}' starts at login, but tunnels.toml has auto-start off. \
                         Run `gtunnel autostart {tunnel} --off` to match the record, or \
                         `--on` to keep it"
                    )
                } else {
                    let _ = record;
                    format!(
                        "Tunnel '{tunnel}' has auto-start on in tunnels.toml but nothing starts it \
                         at login. Run `gtunnel autostart {tunnel} --on` to install it"
                    )
                }
            }
            Finding::DeadEphemeral { name } => {
                format!("Run record '{name}' belongs to a process that is gone")
            }
            Finding::RelayMissing { account } => format!(
                "Account '{account}' has Expose tunnels but no relay. Run `gtunnel relay up`"
            ),
            Finding::CertNotActive { domain, state } => {
                format!("Certificate for {domain} is {state}, so HTTPS does not work yet")
            }
            Finding::OrphanDns { zone, fqdn } => {
                format!("DNS record {fqdn} in zone {zone} points at the relay but has no tunnel")
            }
            Finding::UnknownDns { zone, fqdn } => format!(
                "DNS record {fqdn} in zone {zone} points at the relay but gtunnel did not \
                 create it. Remove it yourself if it is stale: `gcloud dns record-sets delete \
                 {fqdn} --type=A --zone={zone}`"
            ),
            Finding::OrphanBackend { name } => {
                format!("Backend service {name} has no tunnel")
            }
            Finding::OrphanHostRule { host } => {
                format!("Load balancer host rule for {host} has no tunnel")
            }
            Finding::OrphanNamedPort { name } => {
                format!("Named port {name} on the relay has no tunnel")
            }
            Finding::DescriptorsExhausted { tunnel, holder } => {
                let who = match holder {
                    // The line in the log may be hours old; the count is
                    // from a listing taken just now, so say which is which
                    // rather than letting a bystander read as the cause
                    Some(h) => format!(
                        "{} (pid {}) is holding {} of its connections right now",
                        h.command, h.pid, h.count
                    ),
                    None => "the process holding its connections could not be identified, \
                             which is what `lsof` being absent looks like"
                        .to_string(),
                };
                format!(
                    "Tunnel '{tunnel}' ran out of file descriptors: {who}. Deal with that, \
                     then `gtunnel restart {tunnel}`"
                )
            }
            Finding::ReservedAccountName { account } => format!(
                "Account '{account}' has a reserved name: run logs are written as \
                 run-<account>-<name>.log, so its tunnel logs can share a file with a run. \
                 Rename the account: add one under a new name, `gtunnel move` its tunnels, then \
                 `gtunnel account remove {account}`"
            ),
            Finding::UnknownKey { file, path } => format!(
                "{file} has a key gtunnel ignores: {path}. Edit the file and remove or move it"
            ),
            Finding::WatchdogMissing => format!(
                "No watchdog is installed, so a tunnel that wedges stays wedged until someone \
                 notices. Run `gtunnel watch install` to check every {}s",
                crate::daemon::WATCHDOG_INTERVAL
            ),
        }
    }
}

// The answer `Too many open files` sends a person here for. The log is the
// trigger rather than the connection count, because the count a tunnel wedges
// at is whatever ceiling its unit was installed with, and an old unit's is 256.
pub fn out_of_descriptors(log: &str) -> bool {
    log.contains("Too many open files")
}

pub fn descriptors_exhausted(tunnel: &str, log: &str, holders: &[Holder]) -> Option<Finding> {
    out_of_descriptors(log).then(|| Finding::DescriptorsExhausted {
        tunnel: tunnel.to_string(),
        holder: holders.first().cloned(),
    })
}

/// Accounts the name rule would refuse today. Pure, because it reads only the
/// config and needs neither gcloud nor the network.
pub fn reserved_accounts(config: &Config) -> Vec<String> {
    config
        .accounts
        .iter()
        .filter(|a| crate::config::is_reserved_account_name(&a.name))
        .map(|a| a.name.clone())
        .collect()
}

/// Keys the two config files carry that gtunnel drops on load.
///
/// Reads the files itself rather than taking the parsed structs, because the
/// structs are what lost the keys. A missing file is nothing to report, and so
/// is one that does not parse: loading already stops on that with the line.
pub fn unknown_key_findings(dir: &std::path::Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut scan = |file: &str, keys: Option<Vec<String>>| {
        out.extend(
            keys.unwrap_or_default()
                .into_iter()
                .map(|path| Finding::UnknownKey {
                    file: file.to_string(),
                    path,
                }),
        );
    };
    let read = |file: &str| std::fs::read_to_string(dir.join(file)).ok();
    scan(
        "config.toml",
        read("config.toml").and_then(|t| crate::config::unknown_keys::<Config>(&t).ok()),
    );
    scan(
        "tunnels.toml",
        read("tunnels.toml").and_then(|t| crate::config::unknown_keys::<TunnelState>(&t).ok()),
    );
    out
}

// The watchdog is machine-wide, so doctor reports it once rather than once per
// account. It is only worth reporting when there is a tunnel for it to watch:
// a fresh install with nothing in tunnels.toml is not missing anything.
pub fn watchdog_finding(installed: bool, tunnels: usize) -> Option<Finding> {
    (!installed && tunnels > 0).then_some(Finding::WatchdogMissing)
}

/// Services installed on this machine that no record in `tunnels.toml` owns.
///
/// The comparison is against the labels the records produce, never against a
/// label taken apart: an account or a tunnel name may itself hold a dot or a
/// dash, so splitting a label back into the two is a guess. The watchdog is
/// one job for the machine and owns no record, so it is never an orphan.
pub fn orphan_services(installed: &[String], expected: &HashSet<String>) -> Vec<String> {
    let watchdog = crate::daemon::watchdog_label();
    let watchdog_unit = format!("{watchdog}.service");
    let mut orphans: Vec<String> = installed
        .iter()
        .filter(|label| {
            !expected.contains(*label) && *label != watchdog && **label != watchdog_unit
        })
        .cloned()
        .collect();
    // The two macOS directories hold the same definition twice
    orphans.sort();
    orphans.dedup();
    orphans
}

// The label a definition file stands for, or None for a file that is not a
// gtunnel job. The prefix is what keeps another tool's plist from being read
// as an orphan and then removed.
#[cfg(target_os = "macos")]
fn label_of_file(file_name: &str) -> Option<String> {
    let label = file_name.strip_suffix(".plist")?;
    label.starts_with("com.gtunnel.").then(|| label.to_string())
}

#[cfg(not(target_os = "macos"))]
fn label_of_file(file_name: &str) -> Option<String> {
    (file_name.starts_with("gtunnel-") && file_name.ends_with(".service"))
        .then(|| file_name.to_string())
}

// Only these directories are ever listed. Both macOS ones are shared with every
// other config dir on the machine (LaunchAgents is keyed to the home, not to
// GTUNNEL_CONFIG_DIR), so a file found here is judged by `owned_by` first.
fn service_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Ok(dir) = crate::daemon::launchd::agents_dir() {
            dirs.push(dir);
        }
        if let Ok(dir) = crate::config::config_dir() {
            dirs.push(dir.join("agents"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".config/systemd/user"));
    }
    dirs
}

/// Whether a service definition belongs to the config dir `config_dir`.
///
/// `tunnels.toml` only speaks for its own config dir, but the service manager's
/// directory holds every config dir's services. A service logs to
/// `<config_dir>/logs/`, so its log path is the one thing in the file that
/// names its owner. A file whose log path cannot be read belongs to nobody:
/// guessing wrong here deletes a service someone else is running.
pub fn owned_by(text: &str, config_dir: &std::path::Path) -> bool {
    #[cfg(target_os = "macos")]
    let log = crate::daemon::launchd::plist_log_path(text);
    #[cfg(not(target_os = "macos"))]
    let log = crate::daemon::systemd::unit_log_path(text);
    log.is_some_and(|log| crate::daemon::is_within(&log, &config_dir.join("logs")))
}

/// The thin caller over [`orphan_services`]: lists `dirs` and builds the
/// expected labels from every record in every account. A directory that cannot
/// be read holds nothing to report. A file inside `config_dir` is this
/// config's by location; any other is a candidate only if `owned_by` says so.
pub fn find_orphan_services(
    dirs: &[std::path::PathBuf],
    tunnels: &[crate::state::PersistentTunnel],
    config_dir: &std::path::Path,
) -> Vec<String> {
    let expected: HashSet<String> = tunnels
        .iter()
        .map(|t| crate::daemon::platform_label(&t.account_name, &t.name))
        .collect();
    let mut installed = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        // Exactly `agents/`, never the config dir as a whole: it can be `$HOME`,
        // which contains the shared LaunchAgents directory
        let ours_by_location = dir == &config_dir.join("agents");
        for entry in entries.flatten() {
            let Some(label) = entry
                .file_name()
                .into_string()
                .ok()
                .and_then(|name| label_of_file(&name))
            else {
                continue;
            };
            if ours_by_location
                || std::fs::read_to_string(entry.path())
                    .is_ok_and(|text| owned_by(&text, config_dir))
            {
                installed.push(label);
            }
        }
    }
    orphan_services(&installed, &expected)
}

// The exact command that makes an Access tunnel's port reachable through IAP
pub fn firewall_fix_command(acct: &Account, network: &str, port: u16) -> String {
    format!(
        "gcloud compute firewall-rules create gt-allow-iap-{port} --project={} \
         --network={network} --direction=INGRESS --allow=tcp:{port} \
         --source-ranges=35.235.240.0/20",
        acct.project
    )
}

fn host_rules(map: &UrlMap) -> Vec<String> {
    map.0
        .get("hostRules")
        .and_then(|r| r.as_array())
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| rule.get("hosts").and_then(|h| h.as_array()))
                .flatten()
                .filter_map(|host| host.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// A running `gtunnel run` publishes a hostname that tunnels.toml never sees, so
// its route is owned until the process is gone
pub fn ephemeral_ownership(
    records: &[(EphemeralRecord, bool)],
    account: &str,
) -> (HashSet<String>, HashSet<String>) {
    let mut hostnames = HashSet::new();
    let mut names = HashSet::new();
    for (record, alive) in records {
        if !alive || record.account_name != account {
            continue;
        }
        if let TunnelSpec::Expose { hostname, .. } = &record.spec {
            hostnames.insert(hostname.clone());
            names.insert(record.name.clone());
        }
    }
    (hostnames, names)
}

// Everything that owns a route right now: tunnels.toml plus the live runs.
// Both are re-read at fix time, not at scan time.
pub fn live_ownership(
    tunnels: &[PersistentTunnel],
    records: &[(EphemeralRecord, bool)],
    account: &str,
) -> (HashSet<String>, HashSet<String>) {
    let (mut hostnames, mut names) = ephemeral_ownership(records, account);
    for t in tunnels {
        if t.account_name != account {
            continue;
        }
        if let TunnelSpec::Expose { hostname, .. } = &t.spec {
            hostnames.insert(hostname.clone());
            names.insert(t.name.clone());
        }
    }
    (hostnames, names)
}

// The fix pass runs after a confirmation prompt, so `gtunnel run` or
// `gtunnel expose` can claim a route between the scan and the delete
pub fn claimed_since_scan(
    finding: &Finding,
    hostnames: &HashSet<String>,
    tunnels: &HashSet<String>,
    names: &Names,
) -> bool {
    match finding {
        Finding::OrphanDns { fqdn, .. } => hostnames.contains(fqdn.trim_end_matches('.')),
        Finding::OrphanHostRule { host } => hostnames.contains(host),
        Finding::OrphanBackend { name } => tunnels.iter().any(|t| names.backend(t) == *name),
        Finding::OrphanNamedPort { name } => tunnels.iter().any(|t| names.named_port(t) == *name),
        _ => false,
    }
}

// Relay resources whose tunnel is gone. Only resources gtunnel named are
// considered, so a hand-made record or backend is never touched
#[allow(clippy::too_many_arguments)]
pub fn find_expose_orphans(
    state_hostnames: &HashSet<String>,
    state_names: &HashSet<String>,
    relay_ip: &str,
    records: &[(String, RecordSet)],
    backends: &[BackendService],
    url_map: &UrlMap,
    named_ports: &[NamedPort],
    names: &Names,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Account names share dash-separated prefixes, so the name alone cannot say
    // whose backend this is; only the owner tag gtunnel wrote can
    let owner = names.owner();
    let backend_prefix = format!("gt-{}-", names.account);

    // A Cloud DNS record carries no description, so pointing at the relay IP
    // proves nothing on its own. A record is gtunnel's only when gtunnel still
    // holds a resource it did tag for the same tunnel.
    let mut owned_labels: HashSet<&str> = HashSet::new();
    for backend in backends {
        if backend.description.as_deref() == Some(owner.as_str())
            && let Some(tunnel) = backend.name.strip_prefix(&backend_prefix)
        {
            owned_labels.insert(tunnel);
        }
    }
    for port in named_ports {
        if let Some(tunnel) = port.name.strip_prefix("gt-") {
            owned_labels.insert(tunnel);
        }
    }
    let owned_hosts: HashSet<String> = host_rules(url_map).into_iter().collect();

    for (zone, record) in records {
        if record.rtype != "A" || record.rrdatas != [relay_ip.to_string()] {
            continue;
        }
        let hostname = record.name.trim_end_matches('.');
        if state_hostnames.contains(hostname) {
            continue;
        }
        let label = hostname.split('.').next().unwrap_or_default();
        if owned_hosts.contains(hostname) || owned_labels.contains(label) {
            findings.push(Finding::OrphanDns {
                zone: zone.clone(),
                fqdn: record.name.clone(),
            });
        } else {
            findings.push(Finding::UnknownDns {
                zone: zone.clone(),
                fqdn: record.name.clone(),
            });
        }
    }

    for backend in backends {
        if backend.description.as_deref() != Some(owner.as_str()) {
            continue;
        }
        let Some(tunnel) = backend.name.strip_prefix(&backend_prefix) else {
            continue;
        };
        if backend.name == names.default_backend || state_names.contains(tunnel) {
            continue;
        }
        findings.push(Finding::OrphanBackend {
            name: backend.name.clone(),
        });
    }

    for host in host_rules(url_map) {
        if !state_hostnames.contains(&host) {
            findings.push(Finding::OrphanHostRule { host });
        }
    }

    let default_port = names.default_named_port();
    for port in named_ports {
        let Some(tunnel) = port.name.strip_prefix("gt-") else {
            continue;
        };
        if port.name == default_port || state_names.contains(tunnel) {
            continue;
        }
        findings.push(Finding::OrphanNamedPort {
            name: port.name.clone(),
        });
    }

    findings
}

// Everything gtunnel can see about one account, without changing anything
// `gcloud info` prints one value per line, and an unset location is blank
pub fn python_location(stdout: &str) -> Option<String> {
    let line = stdout.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

// Without numpy gcloud falls back to a pure-Python socket loop, and an IAP
// tunnel runs several times slower. A probe that cannot run reports nothing:
// a finding nobody can act on is worse than silence
async fn numpy_is_missing() -> bool {
    let Some(gcloud_path) = gcloud::find_gcloud() else {
        return false;
    };
    let Ok(info) = tokio::process::Command::new(&gcloud_path)
        .args(["info", "--format=value(basic.python_location)"])
        .output()
        .await
    else {
        return false;
    };
    if !info.status.success() {
        return false;
    }
    let Some(python) = python_location(&String::from_utf8_lossy(&info.stdout)) else {
        return false;
    };
    match tokio::process::Command::new(python)
        .args(["-c", "import numpy"])
        .output()
        .await
    {
        Ok(out) => !out.status.success(),
        Err(_) => false,
    }
}

pub async fn run_doctor(
    cfg: &Config,
    state: &TunnelState,
    accounts: &[&Account],
) -> Vec<(String, Finding)> {
    let mut out = Vec::new();

    if gcloud::find_gcloud().is_none() {
        out.push((String::new(), Finding::GcloudMissing));
    } else if numpy_is_missing().await {
        out.push((String::new(), Finding::NumpyMissing));
    }

    if let Some(finding) = watchdog_finding(
        crate::daemon::watchdog_installed().await,
        state.tunnels.len(),
    ) {
        out.push((String::new(), finding));
    }

    for account in reserved_accounts(cfg) {
        out.push((account.clone(), Finding::ReservedAccountName { account }));
    }

    // The files are shared by every account, so these carry none
    if let Ok(config_dir) = crate::config::config_dir() {
        for finding in unknown_key_findings(&config_dir) {
            out.push((String::new(), finding));
        }
    }

    // Machine-wide like the watchdog: a service has no account the scan could
    // file it under, since the record that would name it is the thing missing
    if let Ok(config_dir) = crate::config::config_dir() {
        for label in find_orphan_services(&service_dirs(), &state.tunnels, &config_dir) {
            out.push((String::new(), Finding::OrphanService { label }));
        }
    }

    for (record, alive) in list_ephemeral().unwrap_or_default() {
        if !alive {
            out.push((
                record.account_name.clone(),
                Finding::DeadEphemeral { name: record.name },
            ));
        }
    }

    for acct in accounts {
        out.extend(
            account_findings(cfg, state, acct)
                .await
                .into_iter()
                .map(|f| (acct.name.clone(), f)),
        );
    }
    out
}

async fn account_findings(_cfg: &Config, state: &TunnelState, acct: &Account) -> Vec<Finding> {
    let mut findings = Vec::new();
    let client = GcpClient::for_account(acct);

    // Before the token, because a tunnel out of descriptors is a local fact
    // and the expired credentials that often accompany it must not hide it
    for tunnel in state.tunnels_for_account(&acct.name) {
        let Ok(log_path) = tunnel.log_path() else {
            continue;
        };
        let log = crate::daemon::read_tail(&log_path, crate::daemon::TAIL_WINDOW);
        // The lsof listing is only asked for once the log has earned it
        if !out_of_descriptors(&log) {
            continue;
        }
        let port = match &tunnel.spec {
            TunnelSpec::Access { local_port, .. } => *local_port,
            TunnelSpec::Expose { .. } => 0,
        };
        findings.extend(descriptors_exhausted(
            &tunnel.name,
            &log,
            &crate::probe::holders_for_port(port),
        ));
    }

    if gcloud::print_access_token(acct).await.is_err() {
        findings.push(Finding::GcloudAuth {
            account: acct.name.clone(),
        });
        // Without a token nothing else can be checked against GCP
        return findings;
    }

    let tunnels = state.tunnels_for_account(&acct.name);
    let firewalls = compute::list_firewalls(&client, &acct.project)
        .await
        .unwrap_or_default();

    for tunnel in &tunnels {
        let label = crate::daemon::platform_label(&acct.name, &tunnel.name);
        if crate::daemon::status(&label).await == crate::state::TunnelStatus::Stopped
            && !service_installed(&label)
        {
            findings.push(Finding::StaleState {
                tunnel: tunnel.name.clone(),
            });
        }

        // A tunnel the user set to start at login, or set not to, against what
        // the service manager will actually do at the next login
        let at_login = starts_at_login(&label);
        if at_login != tunnel.auto_start {
            findings.push(Finding::AutoStartDrift {
                tunnel: tunnel.name.clone(),
                record: tunnel.auto_start,
                installed: at_login,
            });
        }

        match &tunnel.spec {
            TunnelSpec::Access {
                target,
                remote_port,
                local_port,
            } => {
                if !local_port_is_free(*local_port)
                    && crate::daemon::status(&label).await != crate::state::TunnelStatus::Running
                {
                    findings.push(Finding::PortConflict {
                        tunnel: tunnel.name.clone(),
                        port: *local_port,
                    });
                }
                if let AccessTarget::Instance { instance, zone } = target {
                    match compute::get_instance(&client, &acct.project, zone, instance).await {
                        Ok(None) => findings.push(Finding::MissingInstance {
                            tunnel: tunnel.name.clone(),
                        }),
                        Ok(Some(found)) => {
                            let network = found.network.clone().unwrap_or_else(|| "default".into());
                            if !compute::firewall_allows_iap(
                                &firewalls,
                                &network,
                                &found.tags,
                                *remote_port,
                            ) {
                                findings.push(Finding::FirewallMissing {
                                    tunnel: tunnel.name.clone(),
                                    command: firewall_fix_command(acct, &network, *remote_port),
                                });
                            }
                            if let Ok(false) =
                                iap::can_tunnel_instance(&client, &acct.project, zone, instance)
                                    .await
                            {
                                findings.push(Finding::IamMissing {
                                    tunnel: tunnel.name.clone(),
                                    role: TUNNEL_ROLE,
                                });
                            }
                        }
                        Err(_) => {}
                    }
                } else if let AccessTarget::DestGroup { region, group, .. } = target {
                    // Without this arm a destination-group tunnel got no IAM check
                    // at all, and a missing role looked healthy
                    if let Ok(false) =
                        iap::can_tunnel_dest_group(&client, &acct.project, region, group).await
                    {
                        findings.push(Finding::IamMissing {
                            tunnel: tunnel.name.clone(),
                            role: TUNNEL_ROLE,
                        });
                    }
                }
            }
            TunnelSpec::Expose { .. } => {
                if acct.expose.is_none() {
                    findings.push(Finding::RelayMissing {
                        account: acct.name.clone(),
                    });
                }
            }
        }
    }

    if let Some(expose) = &acct.expose {
        findings.extend(relay_findings(&client, acct, expose, &tunnels).await);
    }

    findings
}

// On macOS the definition gtunnel loads and unloads lives in <config>/agents;
// LaunchAgents holds a copy only while auto-start is on, so asking it whether a
// service exists reported every stopped tunnel as stale state to be deleted.
#[cfg(target_os = "macos")]
fn service_installed(label: &str) -> bool {
    match crate::daemon::launchd::plist_path(label) {
        Ok(path) => path.exists(),
        Err(_) => true,
    }
}

#[cfg(not(target_os = "macos"))]
fn service_installed(label: &str) -> bool {
    let Some(home) = dirs::home_dir() else {
        return true;
    };
    home.join(".config/systemd/user").join(label).exists()
}

// Whether something will start this tunnel at login, which on macOS is the
// LaunchAgents copy and on Linux the systemd `enable` symlink
fn starts_at_login(label: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::daemon::launchd::agent_path(label)
            .map(|p| p.exists())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::home_dir()
            .map(|home| {
                home.join(".config/systemd/user/default.target.wants")
                    .join(label)
                    .exists()
            })
            .unwrap_or(false)
    }
}

async fn relay_findings(
    client: &GcpClient,
    acct: &Account,
    expose: &crate::config::ExposeConfig,
    tunnels: &[&crate::state::PersistentTunnel],
) -> Vec<Finding> {
    let names = Names::new(&acct.name);
    let mut findings = Vec::new();

    for domain in &expose.domains {
        if let Ok(state) = certmanager::certificate_state(
            client,
            &acct.project,
            &names.certificate(&domain.domain),
        )
        .await
            && state != "ACTIVE"
        {
            findings.push(Finding::CertNotActive {
                domain: domain.domain.clone(),
                state,
            });
        }
    }

    let Ok(Some(relay_ip)) =
        compute::get_global_address(client, &acct.project, &names.address).await
    else {
        return findings;
    };

    let state_hostnames: HashSet<String> = tunnels
        .iter()
        .filter_map(|t| match &t.spec {
            TunnelSpec::Expose { hostname, .. } => Some(hostname.clone()),
            _ => None,
        })
        .collect();
    let mut state_names: HashSet<String> = tunnels
        .iter()
        .filter(|t| t.is_expose())
        .map(|t| t.name.clone())
        .collect();

    let (ephemeral_hostnames, ephemeral_names) =
        ephemeral_ownership(&list_ephemeral().unwrap_or_default(), &acct.name);
    let mut state_hostnames = state_hostnames;
    state_hostnames.extend(ephemeral_hostnames);
    state_names.extend(ephemeral_names);

    let mut records = Vec::new();
    for domain in &expose.domains {
        if let Ok(found) = dns::list_record_sets(client, &acct.project, &domain.managed_zone).await
        {
            records.extend(
                found
                    .into_iter()
                    .map(|record| (domain.managed_zone.clone(), record)),
            );
        }
    }
    let backends = compute::list_backend_services(client, &acct.project)
        .await
        .unwrap_or_default();
    let url_map = compute::get_url_map(client, &acct.project, &names.url_map)
        .await
        .ok()
        .flatten()
        .unwrap_or(UrlMap(serde_json::json!({})));
    let (named_ports, _) = compute::get_named_ports(
        client,
        &acct.project,
        &expose.relay_zone,
        &names.instance_group,
    )
    .await
    .unwrap_or_default();

    findings.extend(find_expose_orphans(
        &state_hostnames,
        &state_names,
        &relay_ip,
        &records,
        &backends,
        &url_map,
        &named_ports,
        &names,
    ));
    findings
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FindingJson {
    pub account: String,
    pub description: String,
    pub fixable: bool,
}

pub async fn cmd_doctor(
    account: Option<&str>,
    fix: bool,
    assume_yes: bool,
    all: bool,
    json: bool,
) -> Result<()> {
    let (config, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let accounts: Vec<&Account> = if all {
        config.accounts.iter().collect()
    } else {
        vec![config
            .accounts
            .iter()
            .find(|a| a.name == acct.name)
            .unwrap_or(&acct)]
    };

    let findings = run_doctor(&config, &state, &accounts).await;
    if json {
        let json_findings: Vec<FindingJson> = findings
            .iter()
            .map(|(account_name, finding)| FindingJson {
                account: account_name.clone(),
                description: finding.describe(),
                fixable: finding.fixable(),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_findings)?);
        return Ok(());
    }

    if findings.is_empty() {
        println!("Nothing to report");
        return Ok(());
    }

    let mut current = None;
    for (account_name, finding) in &findings {
        if current.as_deref() != Some(account_name.as_str()) {
            if account_name.is_empty() {
                println!("gtunnel:");
            } else {
                println!("account {account_name}:");
            }
            current = Some(account_name.clone());
        }
        let marker = if finding.fixable() { "-" } else { "!" };
        println!("  {marker} {}", finding.describe());
    }

    if !fix {
        let fixable = findings.iter().filter(|(_, f)| f.fixable()).count();
        if fixable > 0 {
            println!("\n{fixable} of these can be fixed with `gtunnel doctor --fix`");
        }
        return Ok(());
    }

    let fixable: Vec<(String, Finding)> =
        findings.into_iter().filter(|(_, f)| f.fixable()).collect();
    if fixable.is_empty() {
        println!("\nNothing here is gtunnel's to remove");
        return Ok(());
    }
    if !confirm(
        &format!("\nRemove {} leftover(s)?", fixable.len()),
        assume_yes,
    )? {
        println!("Cancelled");
        return Ok(());
    }

    for (account_name, finding) in fixable {
        // Read per fix: each can await gcloud, and the file is only locked
        // for the one removal that writes it
        let state = TunnelState::load()?;
        match apply_fix(&config, &state, &account_name, &finding).await {
            // Not every fix is a removal: auto-start drift is reconciled, not deleted
            Ok(()) => println!("  fixed: {}", finding.describe()),
            Err(e) => eprintln!("  failed: {} ({e:#})", finding.describe()),
        }
    }
    Ok(())
}

async fn apply_fix(
    config: &Config,
    state: &TunnelState,
    account_name: &str,
    finding: &Finding,
) -> Result<()> {
    let acct = config.get_account(Some(account_name)).ok();
    // Re-read both owners: `gtunnel expose` writes tunnels.toml and `gtunnel
    // run` writes a record, and either can land between the scan and here
    let (hostnames, tunnels) = live_ownership(
        &state.tunnels,
        &list_ephemeral().unwrap_or_default(),
        account_name,
    );
    if claimed_since_scan(finding, &hostnames, &tunnels, &Names::new(account_name)) {
        anyhow::bail!("a tunnel claimed it since the scan");
    }
    match finding {
        Finding::StaleState { tunnel } => TunnelState::update(|state| {
            state.remove_for_account(tunnel, account_name);
            Ok(())
        }),
        // Re-checked against the record set just loaded: a tunnel added since the
        // scan may own this label, and uninstalling it would take that tunnel down
        Finding::OrphanService { label } => {
            let config_dir = crate::config::config_dir()?;
            if !find_orphan_services(&service_dirs(), &state.tunnels, &config_dir).contains(label) {
                anyhow::bail!("a tunnel claimed it since the scan");
            }
            // The launchd domain is shared by every config dir, so the plain
            // uninstall's boot-out by label could stop another config's job
            #[cfg(target_os = "macos")]
            return crate::daemon::launchd::uninstall_orphan(label, &config_dir).await;
            // The unit file passed the ownership rule above, and its path is
            // the only thing systemd keys it by
            #[cfg(not(target_os = "macos"))]
            crate::daemon::uninstall(label).await
        }
        // tunnels.toml is what the user edits, so the record is the truth the
        // service manager is brought back to
        Finding::AutoStartDrift { tunnel, .. } => {
            let t = state
                .find_for_account(tunnel, account_name)
                .ok_or_else(|| anyhow::anyhow!("Tunnel '{tunnel}' is gone"))?
                .clone();
            let spec = crate::daemon::service_spec(config, &t)?;
            crate::daemon::set_auto_start(&spec).await
        }
        Finding::DeadEphemeral { name } => {
            let path = crate::ephemeral::record_path(account_name, name)?;
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            Ok(())
        }
        Finding::OrphanDns { zone, fqdn } => {
            let acct = acct.ok_or_else(|| anyhow::anyhow!("Account '{account_name}' not found"))?;
            let client = GcpClient::for_account(acct);
            dns::delete_record_set(&client, &acct.project, zone, fqdn, "A").await
        }
        Finding::OrphanBackend { name } => {
            let acct = acct.ok_or_else(|| anyhow::anyhow!("Account '{account_name}' not found"))?;
            let client = GcpClient::for_account(acct);
            if let Some(op) = compute::delete_backend_service(&client, &acct.project, name).await? {
                client.wait_operation(&op).await?;
            }
            Ok(())
        }
        Finding::OrphanHostRule { host } => {
            let acct = acct.ok_or_else(|| anyhow::anyhow!("Account '{account_name}' not found"))?;
            let client = GcpClient::for_account(acct);
            let names = Names::new(&acct.name);
            routes::retry_on_precondition(routes::FINGERPRINT_ATTEMPTS, || async {
                let Some(map) =
                    compute::get_url_map(&client, &acct.project, &names.url_map).await?
                else {
                    return Ok(());
                };
                let updated = compute::without_host_rule(&map, host);
                if updated == map {
                    return Ok(());
                }
                let op = compute::update_url_map(&client, &acct.project, &names.url_map, &updated)
                    .await?;
                client.wait_operation(&op).await
            })
            .await
        }
        Finding::OrphanNamedPort { name } => {
            let acct = acct.ok_or_else(|| anyhow::anyhow!("Account '{account_name}' not found"))?;
            let expose = acct
                .expose
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Account '{account_name}' has no relay"))?;
            let client = GcpClient::for_account(acct);
            let names = Names::new(&acct.name);
            routes::retry_on_precondition(routes::FINGERPRINT_ATTEMPTS, || async {
                let (ports, fingerprint) = compute::get_named_ports(
                    &client,
                    &acct.project,
                    &expose.relay_zone,
                    &names.instance_group,
                )
                .await?;
                if !ports.iter().any(|p| p.name == *name) {
                    return Ok(());
                }
                let kept = compute::remove_named_port(&ports, name);
                let op = compute::set_named_ports(
                    &client,
                    &acct.project,
                    &expose.relay_zone,
                    &names.instance_group,
                    &kept,
                    fingerprint.as_deref(),
                )
                .await?;
                client.wait_operation(&op).await
            })
            .await
        }
        // The binary that runs the watchdog is this one: a path baked from
        // `cargo run` or a moved binary would install a job that cannot start
        Finding::WatchdogMissing => {
            crate::daemon::install_watchdog(&std::env::current_exe()?).await
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Account;
    use crate::ephemeral::EphemeralRecord;
    use crate::gcp::compute::{BackendService, NamedPort, UrlMap};
    use crate::gcp::dns::RecordSet;

    const RELAY_IP: &str = "34.1.2.3";

    fn account() -> Account {
        Account {
            name: "dev".to_string(),
            gcloud_configuration: "dev".to_string(),
            project: "proj-1".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: None,
            expose: None,
        }
    }

    fn record(name: &str, ip: &str) -> (String, RecordSet) {
        (
            "example".to_string(),
            RecordSet {
                name: format!("{name}.dev.example.com."),
                rtype: "A".to_string(),
                ttl: 60,
                rrdatas: vec![ip.to_string()],
            },
        )
    }

    fn backend(name: &str) -> BackendService {
        owned_backend(name, Some("gtunnel:dev"))
    }

    fn owned_backend(name: &str, description: Option<&str>) -> BackendService {
        BackendService {
            name: name.to_string(),
            port_name: Some("gt-x".to_string()),
            iap_enabled: false,
            description: description.map(|d| d.to_string()),
        }
    }

    fn expose_record(name: &str, hostname: &str, alive: bool) -> (EphemeralRecord, bool) {
        (
            EphemeralRecord {
                name: name.to_string(),
                account_name: "dev".to_string(),
                pid: 1234,
                spec: TunnelSpec::Expose {
                    local_target: "localhost:3000".to_string(),
                    domain: "dev.example.com".to_string(),
                    hostname: hostname.to_string(),
                    relay_port: 30001,
                    private: false,
                },
                started_at: 0,
                ttl_secs: None,
                pid_start: None,
            },
            alive,
        )
    }

    #[test]
    fn a_running_ephemeral_tunnel_owns_its_route() {
        let records = vec![
            expose_record("gt-live", "gt-live.dev.example.com", true),
            expose_record("gt-dead", "gt-dead.dev.example.com", false),
        ];
        let (hostnames, names) = ephemeral_ownership(&records, "dev");
        assert_eq!(
            hostnames,
            ["gt-live.dev.example.com".to_string()]
                .into_iter()
                .collect::<HashSet<String>>()
        );
        assert_eq!(
            names,
            ["gt-live".to_string()]
                .into_iter()
                .collect::<HashSet<String>>()
        );
        // Another account's running tunnel is not this account's to keep or remove
        assert!(ephemeral_ownership(&records, "prod").0.is_empty());
    }

    #[test]
    fn a_route_claimed_between_the_scan_and_the_fix_is_not_removed() {
        let names = Names::new("dev");
        let records = vec![expose_record("gt-live", "gt-live.dev.example.com", true)];
        let (hostnames, tunnels) = live_ownership(&[], &records, "dev");
        let claimed = |f: &Finding| claimed_since_scan(f, &hostnames, &tunnels, &names);

        assert!(claimed(&Finding::OrphanDns {
            zone: "example".to_string(),
            fqdn: "gt-live.dev.example.com.".to_string(),
        }));
        assert!(claimed(&Finding::OrphanHostRule {
            host: "gt-live.dev.example.com".to_string(),
        }));
        assert!(claimed(&Finding::OrphanBackend {
            name: "gt-dev-gt-live".to_string(),
        }));
        assert!(claimed(&Finding::OrphanNamedPort {
            name: "gt-gt-live".to_string(),
        }));
        assert!(!claimed(&Finding::OrphanBackend {
            name: "gt-dev-old".to_string(),
        }));
    }

    #[test]
    fn a_backend_gtunnel_did_not_create_is_never_an_orphan() {
        let names = Names::new("dev");
        let backends = vec![
            // Another account whose name starts with this one's
            owned_backend("gt-dev-api-web", Some("gtunnel:dev-api")),
            // Someone else's backend that happens to match the naming scheme
            owned_backend("gt-dev-handmade", None),
            owned_backend("gt-dev-old", Some("gtunnel:dev")),
        ];
        let findings = find_expose_orphans(
            &HashSet::new(),
            &HashSet::new(),
            RELAY_IP,
            &[],
            &backends,
            &url_map(&[]),
            &[],
            &names,
        );
        assert_eq!(
            findings,
            vec![Finding::OrphanBackend {
                name: "gt-dev-old".to_string()
            }]
        );
    }

    fn url_map(hosts: &[&str]) -> UrlMap {
        let mut map = UrlMap(serde_json::json!({"name": "gt-dev-urlmap"}));
        for host in hosts {
            map = crate::gcp::compute::with_host_rule(&map, host, "projects/p/x");
        }
        map
    }

    #[test]
    fn expose_orphans_are_exactly_the_resources_state_does_not_know() {
        let names = Names::new("dev");
        let state_hostnames: HashSet<String> =
            ["api.dev.example.com".to_string()].into_iter().collect();
        let state_names: HashSet<String> = ["api".to_string()].into_iter().collect();
        let records = vec![
            record("api", RELAY_IP),
            record("old", RELAY_IP),
            // Another service's record, pointing somewhere else entirely
            record("www", "203.0.113.9"),
        ];
        let backends = vec![
            backend("gt-dev-api"),
            backend("gt-dev-old"),
            backend("gt-dev-default"),
        ];
        let map = url_map(&["api.dev.example.com"]);
        let ports = vec![
            NamedPort {
                name: "gt-api".to_string(),
                port: 30001,
            },
            NamedPort {
                name: "gt-default".to_string(),
                port: 30999,
            },
        ];

        let findings = find_expose_orphans(
            &state_hostnames,
            &state_names,
            RELAY_IP,
            &records,
            &backends,
            &map,
            &ports,
            &names,
        );

        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(findings.iter().any(
            |f| matches!(f, Finding::OrphanDns { fqdn, .. } if fqdn == "old.dev.example.com.")
        ));
        assert!(findings
            .iter()
            .any(|f| matches!(f, Finding::OrphanBackend { name } if name == "gt-dev-old")));
    }

    // Cloud DNS records carry no description, so pointing at the relay IP was
    // the whole test and `doctor --fix` deleted a record someone made by hand
    #[test]
    fn a_hand_made_record_pointing_at_the_relay_is_reported_but_not_deletable() {
        let names = Names::new("dev");
        let findings = find_expose_orphans(
            &HashSet::new(),
            &HashSet::new(),
            RELAY_IP,
            &[record("legacy", RELAY_IP)],
            &[],
            &url_map(&[]),
            &[],
            &names,
        );
        assert_eq!(
            findings,
            vec![Finding::UnknownDns {
                zone: "example".to_string(),
                fqdn: "legacy.dev.example.com.".to_string(),
            }]
        );
        assert!(!findings[0].fixable());
    }

    // The fix pass runs after a prompt, so `gtunnel expose` can claim a route
    // between the scan and the delete just as `gtunnel run` can
    #[test]
    fn a_managed_tunnel_added_between_the_scan_and_the_fix_keeps_its_route() {
        let names = Names::new("dev");
        let hostnames: HashSet<String> = ["api.dev.example.com".to_string()].into_iter().collect();
        let tunnels: HashSet<String> = ["api".to_string()].into_iter().collect();

        assert!(claimed_since_scan(
            &Finding::OrphanBackend {
                name: "gt-dev-api".to_string()
            },
            &hostnames,
            &tunnels,
            &names
        ));
        assert!(claimed_since_scan(
            &Finding::OrphanDns {
                zone: "example".to_string(),
                fqdn: "api.dev.example.com.".to_string(),
            },
            &hostnames,
            &tunnels,
            &names
        ));
        assert!(!claimed_since_scan(
            &Finding::OrphanBackend {
                name: "gt-dev-gone".to_string()
            },
            &hostnames,
            &tunnels,
            &names
        ));
    }

    #[test]
    fn an_orphan_host_rule_and_named_port_are_reported() {
        let names = Names::new("dev");
        let findings = find_expose_orphans(
            &HashSet::new(),
            &HashSet::new(),
            RELAY_IP,
            &[],
            &[],
            &url_map(&["gone.dev.example.com"]),
            &[NamedPort {
                name: "gt-gone".to_string(),
                port: 30002,
            }],
            &names,
        );
        assert!(findings.iter().any(
            |f| matches!(f, Finding::OrphanHostRule { host } if host == "gone.dev.example.com")
        ));
        assert!(findings
            .iter()
            .any(|f| matches!(f, Finding::OrphanNamedPort { name } if name == "gt-gone")));
    }

    #[test]
    fn the_firewall_fix_command_is_exact() {
        assert_eq!(
            firewall_fix_command(&account(), "default", 5432),
            "gcloud compute firewall-rules create gt-allow-iap-5432 --project=proj-1 \
             --network=default --direction=INGRESS --allow=tcp:5432 \
             --source-ranges=35.235.240.0/20"
        );
    }

    // doctor reported "Nothing to report" while four tunnels were set to start at
    // login, because it never compared the record against what launchd holds
    #[test]
    fn auto_start_drift_describes_both_sides() {
        let on_disk = Finding::AutoStartDrift {
            tunnel: "k8s-dev".to_string(),
            record: false,
            installed: true,
        };
        let text = on_disk.describe();
        assert!(text.contains("k8s-dev"), "{text}");
        assert!(text.contains("login"), "{text}");
        assert!(on_disk.fixable());

        let other_way = Finding::AutoStartDrift {
            tunnel: "k8s-dev".to_string(),
            record: true,
            installed: false,
        }
        .describe();
        assert!(other_way.contains("k8s-dev"), "{other_way}");
        assert_ne!(other_way, text, "the two directions must not read alike");
    }

    // The canonical definition moved to <config>/agents when auto-start stopped
    // meaning "loaded": looking in LaunchAgents reported every stopped tunnel
    // with auto-start off as stale, and the fix for stale is to delete it
    #[cfg(target_os = "macos")]
    #[test]
    fn an_installed_service_is_found_by_its_own_definition_not_the_login_copy() {
        let _temp = crate::config::testing::TempConfigDir::new();
        let label = "com.gtunnel.test.doctor-installed";
        assert!(!service_installed(label), "nothing is installed yet");

        let path = crate::daemon::launchd::plist_path(label).expect("path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "<plist/>").expect("write");
        assert!(
            service_installed(label),
            "a definition in {} is an installed service",
            path.display()
        );
    }

    // What gtunnel created, it may remove; what it only found, it explains
    #[test]
    fn only_gtunnels_own_leftovers_are_fixable() {
        assert!(Finding::StaleState {
            tunnel: "db".to_string()
        }
        .fixable());
        assert!(Finding::DeadEphemeral {
            name: "gt-abc123".to_string()
        }
        .fixable());
        assert!(Finding::OrphanDns {
            zone: "example".to_string(),
            fqdn: "old.dev.example.com.".to_string()
        }
        .fixable());
        assert!(!Finding::FirewallMissing {
            tunnel: "db".to_string(),
            command: "gcloud ...".to_string()
        }
        .fixable());
        assert!(!Finding::IamMissing {
            tunnel: "db".to_string(),
            role: "roles/iap.tunnelResourceAccessor"
        }
        .fixable());
        assert!(!Finding::GcloudMissing.fixable());
    }

    #[test]
    fn every_finding_describes_itself_without_panicking() {
        let findings = vec![
            Finding::GcloudMissing,
            Finding::GcloudAuth {
                account: "dev".to_string(),
            },
            Finding::NumpyMissing,
            Finding::StaleState {
                tunnel: "db".to_string(),
            },
            Finding::MissingInstance {
                tunnel: "db".to_string(),
            },
            Finding::PortConflict {
                tunnel: "db".to_string(),
                port: 15432,
            },
            Finding::RelayMissing {
                account: "dev".to_string(),
            },
            Finding::CertNotActive {
                domain: "dev.example.com".to_string(),
                state: "PROVISIONING".to_string(),
            },
        ];
        for finding in findings {
            assert!(!finding.describe().is_empty());
        }
    }
    // The hint `Too many open files` leaves in the log sends the person to
    // `gtunnel doctor`, so doctor has to be able to answer. Who is holding the
    // connections is a local fact: no account, no token, no GCP
    #[test]
    fn doctor_names_whoever_is_holding_a_wedged_tunnels_connections() {
        let holders = [Holder {
            pid: 4471,
            command: "my-client".to_string(),
            count: 3804,
        }];
        assert_eq!(
            descriptors_exhausted("k8s", "debug1: nothing wrong here", &holders),
            None
        );

        let text = descriptors_exhausted("k8s", "accept: Too many open files", &holders)
            .expect("a finding")
            .describe();
        assert!(text.contains("my-client"), "{text}");
        assert!(text.contains("4471"), "{text}");
        assert!(text.contains("3804"), "{text}");

        // Without lsof there is no holder to name, and naming nobody beats
        // naming the wrong process
        let unknown = descriptors_exhausted("k8s", "accept: Too many open files", &[])
            .expect("a finding")
            .describe();
        assert!(unknown.contains("k8s"), "{unknown}");
        assert!(!unknown.contains("my-client"), "{unknown}");
    }

    #[test]
    fn doctor_names_an_account_whose_name_is_now_reserved() {
        let mut cfg = Config::default();
        for name in ["run", "run-eu", "runner", "dev"] {
            let mut a = account();
            a.name = name.to_string();
            // Pushed directly: add_account would refuse the reserved ones
            cfg.accounts.push(a);
        }
        assert_eq!(reserved_accounts(&cfg), vec!["run", "run-eu"]);
        let finding = Finding::ReservedAccountName {
            account: "run".to_string(),
        };
        assert!(!finding.fixable());
        assert!(finding.describe().contains("Rename the account"));
    }

    // The watchdog is what turns the breaker into something that acts while
    // nobody is watching, so doctor has to say when it is absent. It is a
    // decision about two facts, and taking them as arguments is what makes it
    // testable: asking launchd for real would make this a test of the machine
    #[test]
    fn doctor_reports_the_watchdog_as_not_installed_when_it_is_not() {
        assert_eq!(watchdog_finding(false, 2), Some(Finding::WatchdogMissing));
        assert_eq!(watchdog_finding(true, 2), None);
        // No tunnels, nothing to watch: an install with nothing in it yet is
        // not a machine missing its watchdog
        assert_eq!(watchdog_finding(false, 0), None);
    }

    fn expected(labels: &[&str]) -> HashSet<String> {
        labels.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn a_service_no_record_owns_is_an_orphan() {
        let owned = crate::daemon::platform_label("dev", "db");
        let gone = crate::daemon::platform_label("dev", "gone");
        let found = orphan_services(&[owned.clone(), gone.clone()], &expected(&[&owned]));
        assert_eq!(found, vec![gone]);
    }

    #[test]
    fn a_service_is_owned_only_by_a_record_in_its_own_account() {
        // `db` exists in prod only; dev's service for it has no record
        let prod = crate::daemon::platform_label("prod", "db");
        let dev = crate::daemon::platform_label("dev", "db");
        let found = orphan_services(&[prod.clone(), dev.clone()], &expected(&[&prod]));
        assert_eq!(found, vec![dev]);
    }

    #[test]
    fn the_watchdog_is_never_an_orphan() {
        let watchdog = crate::daemon::watchdog_label().to_string();
        let found = orphan_services(&[watchdog], &expected(&[]));
        assert!(found.is_empty(), "the watchdog is not a tunnel: {found:?}");
    }

    #[test]
    fn a_service_listed_in_two_directories_is_reported_once() {
        let gone = crate::daemon::platform_label("dev", "gone");
        let found = orphan_services(&[gone.clone(), gone.clone()], &expected(&[]));
        assert_eq!(found, vec![gone]);
    }

    #[test]
    fn an_orphan_is_fixable_and_describes_itself() {
        let finding = Finding::OrphanService {
            label: "com.gtunnel.dev.gone".to_string(),
        };
        assert!(finding.fixable());
        assert!(finding.describe().contains("com.gtunnel.dev.gone"));
    }

    #[cfg(target_os = "macos")]
    fn persistent(account: &str, name: &str) -> crate::state::PersistentTunnel {
        crate::state::PersistentTunnel {
            name: name.to_string(),
            account_name: account.to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_orphan_plist_in_the_config_dir_is_found_and_the_watchdog_plist_is_not() {
        let temp = crate::config::testing::TempConfigDir::new();
        let agents = temp.path().join("agents");
        std::fs::create_dir_all(&agents).expect("mkdir");
        for label in [
            "com.gtunnel.dev.db",
            "com.gtunnel.dev.gone",
            crate::daemon::launchd::WATCHDOG_LABEL,
        ] {
            std::fs::write(agents.join(format!("{label}.plist")), "<plist/>").expect("write");
        }
        // Not ours: files that merely live beside the others
        std::fs::write(agents.join("com.example.other.plist"), "<plist/>").expect("write");
        std::fs::write(agents.join("notes.txt"), "x").expect("write");

        let found = find_orphan_services(&[agents], &[persistent("dev", "db")], temp.path());
        assert_eq!(found, vec!["com.gtunnel.dev.gone".to_string()]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_directory_that_does_not_exist_holds_no_orphans() {
        let temp = crate::config::testing::TempConfigDir::new();
        let found = find_orphan_services(&[temp.path().join("absent")], &[], temp.path());
        assert!(found.is_empty());
    }

    // The definition the host's service manager would hold for a tunnel whose
    // log is `log`, and the file name it lives under
    fn service_text(label: &str, log: &std::path::Path) -> String {
        let spec = crate::daemon::ServiceSpec {
            label: label.to_string(),
            program: std::path::PathBuf::from("/usr/bin/gcloud"),
            args: vec!["compute".to_string()],
            log_path: log.to_path_buf(),
            env: vec![],
            auto_start: true,
            description: "db".to_string(),
        };
        #[cfg(target_os = "macos")]
        {
            crate::daemon::launchd::render_plist(&spec)
        }
        #[cfg(not(target_os = "macos"))]
        {
            crate::daemon::systemd::render_unit(&spec)
        }
    }

    fn service_file_name(label: &str) -> String {
        if cfg!(target_os = "macos") {
            format!("{label}.plist")
        } else {
            label.to_string()
        }
    }

    #[test]
    fn a_service_logging_into_this_config_dir_belongs_to_it() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        let text = service_text("x", &cfg.join("logs/dev-db.log"));
        assert!(owned_by(&text, cfg));
    }

    #[test]
    fn a_service_logging_into_another_config_dir_does_not() {
        let text = service_text("x", std::path::Path::new("/tmp/cfg-b/logs/dev-db.log"));
        assert!(!owned_by(&text, std::path::Path::new("/tmp/cfg-a")));
        // A prefix of the name is not a parent directory
        let text = service_text("x", std::path::Path::new("/tmp/cfg-ab/logs/dev-db.log"));
        assert!(!owned_by(&text, std::path::Path::new("/tmp/cfg-a")));
    }

    #[test]
    fn a_log_path_that_climbs_out_of_the_config_dir_does_not_belong_to_it() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        let text = service_text("x", &cfg.join("logs/../../cfg-b/logs/dev-db.log"));
        assert!(!owned_by(&text, cfg));
    }

    #[test]
    fn a_service_whose_log_cannot_be_read_belongs_to_nobody() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        assert!(!owned_by("", cfg));
        assert!(!owned_by("not a service definition", cfg));
    }

    #[test]
    fn a_service_installed_for_another_config_dir_is_not_an_orphan() {
        let temp = crate::config::testing::TempConfigDir::new();
        let installed = tempfile::tempdir().expect("tempdir");
        let other = tempfile::tempdir().expect("tempdir");
        let label = crate::daemon::platform_label("prod", "k8s");
        std::fs::write(
            installed.path().join(service_file_name(&label)),
            service_text(&label, &other.path().join("logs/prod-k8s.log")),
        )
        .expect("write");

        // This config has no record of it, and it is still not this config's to judge
        let found = find_orphan_services(&[installed.path().to_path_buf()], &[], temp.path());
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_service_installed_for_this_config_dir_without_a_record_is_an_orphan() {
        let temp = crate::config::testing::TempConfigDir::new();
        let installed = tempfile::tempdir().expect("tempdir");
        let label = crate::daemon::platform_label("dev", "gone");
        std::fs::write(
            installed.path().join(service_file_name(&label)),
            service_text(&label, &temp.path().join("logs/dev-gone.log")),
        )
        .expect("write");

        let found = find_orphan_services(&[installed.path().to_path_buf()], &[], temp.path());
        assert_eq!(found, vec![label]);
    }

    #[test]
    fn a_config_dir_that_contains_the_service_dir_does_not_make_it_ours() {
        // GTUNNEL_CONFIG_DIR=$HOME: LaunchAgents is inside it, and shared
        let home = tempfile::tempdir().expect("tempdir");
        let other = tempfile::tempdir().expect("tempdir");
        let agents = home.path().join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).expect("mkdir");
        let label = crate::daemon::platform_label("prod", "k8s");
        std::fs::write(
            agents.join(service_file_name(&label)),
            service_text(&label, &other.path().join("logs/prod-k8s.log")),
        )
        .expect("write");

        let found = find_orphan_services(&[agents], &[], home.path());
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_relative_config_dir_does_not_make_a_service_dir_ours() {
        let home = tempfile::tempdir().expect("tempdir");
        let other = tempfile::tempdir().expect("tempdir");
        let agents = home.path().join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).expect("mkdir");
        let label = crate::daemon::platform_label("prod", "k8s");
        std::fs::write(
            agents.join(service_file_name(&label)),
            service_text(&label, &other.path().join("logs/prod-k8s.log")),
        )
        .expect("write");

        let relative = std::path::Path::new("Library");
        let found = find_orphan_services(&[agents], &[], relative);
        assert!(found.is_empty(), "{found:?}");
        // A relative path is judged as written: its own logs dir still counts
        let text = service_text(&label, std::path::Path::new("rel-cfg/logs/prod-k8s.log"));
        assert!(owned_by(&text, std::path::Path::new("rel-cfg")));
        assert!(!owned_by(&text, std::path::Path::new("other-cfg")));
    }

    #[test]
    fn an_unparsable_or_unreadable_service_file_is_not_an_orphan() {
        let temp = crate::config::testing::TempConfigDir::new();
        let installed = tempfile::tempdir().expect("tempdir");
        let junk = crate::daemon::platform_label("dev", "junk");
        let binary = crate::daemon::platform_label("dev", "binary");
        std::fs::write(installed.path().join(service_file_name(&junk)), "junk").expect("write");
        std::fs::write(
            installed.path().join(service_file_name(&binary)),
            [0xff, 0xfe, 0x00],
        )
        .expect("write");

        let found = find_orphan_services(&[installed.path().to_path_buf()], &[], temp.path());
        assert!(found.is_empty(), "{found:?}");
    }

    // The two files the person edits by hand. `log_level` typed below the last
    // table is the mistake that sent this check into being
    const CONFIG_WITH_STRAY_KEY: &str = "selected_account = \"dev\"\n\
        [[accounts]]\nname = \"dev\"\ngcloud_configuration = \"default\"\n\
        project = \"example-dev-1a2b\"\ndefault_zone = \"europe-west1-b\"\n\
        log_level = \"debug\"\n";

    const TUNNELS_WITH_STRAY_KEY: &str = "[[tunnels]]\nname = \"db\"\naccount_name = \"dev\"\n\
        [tunnels.spec]\nmode = \"access\"\nremote_port = 5432\nremote_prot = 5432\n\
        local_port = 5432\n[tunnels.spec.target]\nkind = \"instance\"\n\
        instance = \"bastion\"\nzone = \"europe-west1-b\"\n";

    #[test]
    fn doctor_reports_an_unknown_key_in_config_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("config.toml"), CONFIG_WITH_STRAY_KEY).expect("write");
        let findings = unknown_key_findings(dir.path());
        assert_eq!(
            findings,
            vec![Finding::UnknownKey {
                file: "config.toml".to_string(),
                path: "accounts[0].log_level".to_string(),
            }]
        );
        assert_eq!(
            findings[0].describe(),
            "config.toml has a key gtunnel ignores: accounts[0].log_level. \
             Edit the file and remove or move it"
        );
    }

    #[test]
    fn doctor_reports_an_unknown_key_in_tunnels_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("tunnels.toml"), TUNNELS_WITH_STRAY_KEY).expect("write");
        let findings = unknown_key_findings(dir.path());
        assert_eq!(
            findings,
            vec![Finding::UnknownKey {
                file: "tunnels.toml".to_string(),
                path: "tunnels[0].spec.remote_prot".to_string(),
            }]
        );
    }

    #[test]
    fn doctor_says_nothing_for_a_clean_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Nothing at all, a file that does not parse (load reports that), and
        // a file gtunnel wrote itself are all silent
        assert!(unknown_key_findings(dir.path()).is_empty());
        std::fs::write(dir.path().join("config.toml"), "selected_account = ").expect("write");
        assert!(unknown_key_findings(dir.path()).is_empty());
        let mut config = Config::default();
        config.accounts.push(account());
        let text = toml::to_string_pretty(&config).expect("serialize");
        std::fs::write(dir.path().join("config.toml"), text).expect("write");
        assert!(unknown_key_findings(dir.path()).is_empty());
    }

    #[test]
    fn fix_leaves_an_unknown_key_alone() {
        let finding = Finding::UnknownKey {
            file: "config.toml".to_string(),
            path: "log_level".to_string(),
        };
        assert!(!finding.fixable());
    }

    #[test]
    fn an_unknown_key_serializes_like_its_siblings() {
        let finding = Finding::UnknownKey {
            file: "tunnels.toml".to_string(),
            path: "tunnels[0].spec.remote_prot".to_string(),
        };
        let json = serde_json::to_value(FindingJson {
            account: String::new(),
            description: finding.describe(),
            fixable: finding.fixable(),
        })
        .expect("json");
        assert_eq!(json["account"], "");
        assert_eq!(json["fixable"], false);
        assert!(
            json["description"]
                .as_str()
                .expect("a string")
                .contains("tunnels[0].spec.remote_prot"),
            "{json}"
        );
    }
}
