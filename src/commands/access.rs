use anyhow::{bail, Context, Result};
use std::time::Duration;

use super::{find_tunnel, format_list_row, list_header, resolve, tunnel_status};
use crate::config::Account;
use crate::daemon;
use crate::expose::routes;
use crate::gcp::{compute, GcpClient};
use crate::state::{
    allocate_port, local_port_is_free, validate_tunnel_name, AccessTarget, PersistentTunnel,
    TunnelSpec, TunnelState, TunnelStatus, ACCESS_PORT_BASE, ACCESS_PORT_SPAN,
};
use crate::tunnel::diagnose_log_line;

// How long a freshly started tunnel is given before its status is believed
const START_SETTLE: Duration = Duration::from_secs(3);
const FOLLOW_INTERVAL: Duration = Duration::from_millis(500);

// Auto-start is off for a new tunnel: `-s` starts it now, and `A` in the TUI
// is what makes one survive a reboot
fn new_access_tunnel(
    name: &str,
    account: &str,
    target: AccessTarget,
    remote_port: u16,
    local_port: u16,
) -> PersistentTunnel {
    PersistentTunnel {
        name: name.to_string(),
        account_name: account.to_string(),
        auto_start: false,
        spec: TunnelSpec::Access {
            target,
            remote_port,
            local_port,
        },
    }
}

// `INSTANCE:PORT`, or `HOST:PORT` when a destination group is used
pub fn parse_access_target(target: &str) -> Result<(String, u16)> {
    let (host, port) = target
        .rsplit_once(':')
        .with_context(|| format!("Invalid target '{target}': expected INSTANCE:PORT"))?;
    let port: u16 = port
        .parse()
        .with_context(|| format!("Invalid target '{target}': expected INSTANCE:PORT"))?;
    if host.is_empty() || port == 0 {
        bail!("Invalid target '{target}': expected INSTANCE:PORT");
    }
    Ok((host.to_string(), port))
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_add(
    account: Option<&str>,
    name: &str,
    target: &str,
    via: Option<&str>,
    zone: Option<&str>,
    local_port: Option<u16>,
    dest_group: Option<&str>,
    region: Option<&str>,
    network: Option<&str>,
    start: bool,
) -> Result<()> {
    validate_tunnel_name(name)?;
    let (config, acct) = resolve(account)?;
    let mut state = TunnelState::load()?;
    if state.find_for_account(name, &acct.name).is_some() {
        bail!("Tunnel '{name}' already exists for account '{}'", acct.name);
    }

    let (host, remote_port) = parse_access_target(target)?;
    let access_target = match (dest_group, via) {
        // The instance is what gtunnel checks exists; the host behind it is
        // reachable from there or it is not, which only the tunnel can tell
        (_, Some(instance)) => {
            let zone = zone.unwrap_or(&acct.default_zone).to_string();
            verify_instance(&acct, &zone, instance).await?;
            AccessTarget::Jump {
                instance: instance.to_string(),
                zone,
                host,
            }
        }
        (Some(group), None) => AccessTarget::DestGroup {
            host,
            region: region
                .context("--region is required with --dest-group")?
                .to_string(),
            group: group.to_string(),
            network: network.unwrap_or("default").to_string(),
        },
        (None, None) => {
            let zone = zone.unwrap_or(&acct.default_zone).to_string();
            verify_instance(&acct, &zone, &host).await?;
            AccessTarget::Instance {
                instance: host,
                zone,
            }
        }
    };

    let local_port = match local_port {
        Some(port) => {
            if state.used_local_ports().contains(&port) {
                bail!("Local port {port} is already used by another gtunnel tunnel");
            }
            if !local_port_is_free(port) {
                bail!("Local port {port} is already in use on this machine");
            }
            port
        }
        None => allocate_port(
            name,
            ACCESS_PORT_BASE,
            ACCESS_PORT_SPAN,
            &state.used_local_ports(),
            local_port_is_free,
        )?,
    };

    let tunnel = new_access_tunnel(name, &acct.name, access_target, remote_port, local_port);
    state.add(tunnel.clone())?;
    state.save()?;

    let spec = daemon::service_spec(&config, &tunnel)?;
    daemon::install(&spec).await?;
    println!(
        "{} {}  {} -> {}",
        TunnelStatus::Stopped.symbol(),
        tunnel.name,
        tunnel.display_url(),
        tunnel.display_target()
    );

    if start {
        cmd_start(account, name).await?;
    }
    Ok(())
}

// A typo in an instance name otherwise surfaces as a tunnel that never starts
async fn verify_instance(acct: &Account, zone: &str, instance: &str) -> Result<()> {
    let client = GcpClient::for_account(acct);
    match compute::get_instance(&client, &acct.project, zone, instance).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => bail!(
            "Instance '{instance}' not found in zone '{zone}' of project '{}'",
            acct.project
        ),
        // A lookup that cannot run is not proof the instance is missing
        Err(e) => {
            eprintln!("Warning: could not verify instance '{instance}': {e:#}");
            Ok(())
        }
    }
}

pub async fn cmd_start(account: Option<&str>, name: &str) -> Result<()> {
    let (config, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;

    // A public tunnel whose backend, host rule or DNS record was removed
    // outside gtunnel would otherwise come up serving a 503
    if tunnel.is_expose() {
        let client = GcpClient::for_account(&acct);
        routes::ensure_route(&client, &acct, &tunnel).await?;
    }

    // A unit written by an older gtunnel is rewritten before it is started
    let spec = daemon::service_spec(&config, &tunnel)?;
    daemon::install(&spec).await?;
    daemon::start(&spec.label).await?;

    tokio::time::sleep(START_SETTLE).await;
    let status = daemon::status(&spec.label).await;
    if status == TunnelStatus::Error {
        println!("{} {name} failed to start", status.symbol());
        if let Some(hint) = last_hint(&tunnel)? {
            println!("  {hint}");
        }
        println!("  Logs: gtunnel logs {name}");
        return Ok(());
    }
    println!(
        "{} {name}  {}",
        TunnelStatus::Running.symbol(),
        tunnel.display_url()
    );
    Ok(())
}

// The newest log line gtunnel recognises, turned into an explanation
fn last_hint(t: &PersistentTunnel) -> Result<Option<String>> {
    let path = t.log_path()?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    Ok(text
        .lines()
        .rev()
        .find_map(|line| diagnose_log_line(line).map(|hint| hint.to_string())))
}

pub async fn cmd_stop(account: Option<&str>, name: &str) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    daemon::stop(&daemon::platform_label(&acct.name, &tunnel.name)).await?;
    println!("{} {name} stopped", TunnelStatus::Stopped.symbol());
    Ok(())
}

pub async fn cmd_restart(account: Option<&str>, name: &str) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    let _ = daemon::stop(&daemon::platform_label(&acct.name, &tunnel.name)).await;
    cmd_start(account, name).await
}

pub async fn cmd_delete(account: Option<&str>, name: &str) -> Result<()> {
    let (_, acct) = resolve(account)?;
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

    let label = daemon::platform_label(&acct.name, &tunnel.name);
    let log_path = tunnel.log_path()?;
    let _ = daemon::stop(&label).await;
    daemon::uninstall(&label).await?;
    // The log goes with the tunnel: nothing is left to own it
    daemon::remove_log(&log_path)?;
    state.remove_for_account(name, &acct.name);
    state.save()?;
    println!("Deleted {name}");
    Ok(())
}

pub async fn cmd_logs(account: Option<&str>, name: &str, follow: bool, lines: usize) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    let path = tunnel.log_path()?;

    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(lines).collect();
    for line in tail.into_iter().rev() {
        println!("{line}");
    }
    if !follow {
        return Ok(());
    }

    // Poll the file's length rather than holding a handle, so a daemon that
    // rewrites the log is still followed
    let mut offset = text.len() as u64;
    loop {
        tokio::time::sleep(FOLLOW_INTERVAL).await;
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() < offset {
            offset = 0;
        }
        if meta.len() == offset {
            continue;
        }
        let whole = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some(fresh) = whole.get(offset as usize..) {
            print!("{fresh}");
            use std::io::Write;
            std::io::stdout().flush().ok();
        }
        offset = whole.len() as u64;
    }
}

pub async fn cmd_list(account: Option<&str>) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnels = state.tunnels_for_account(&acct.name);
    if tunnels.is_empty() {
        println!("No tunnels for account '{}'", acct.name);
        return Ok(());
    }
    println!("{}", list_header());
    for tunnel in tunnels {
        let status = tunnel_status(tunnel).await;
        println!("{}", format_list_row(status, tunnel));
    }
    print_external_tunnels().await;
    Ok(())
}

// IAP tunnels started outside gtunnel hold ports and confuse `doctor`, so the
// list names them instead of pretending they are not there
async fn print_external_tunnels() {
    let managed = crate::discovery::managed_pids().await;
    for external in crate::discovery::external_tunnels(&managed) {
        let url = match external.local_port {
            Some(port) => format!("localhost:{port}"),
            None => "-".to_string(),
        };
        let target = match &external.zone {
            Some(zone) => format!("{}:{} ({zone})", external.target, external.remote_port),
            None => format!("{}:{}", external.target, external.remote_port),
        };
        println!(
            "{} {:<8} {:<16} {:<7} {:<32} {target}  [not managed by gtunnel]",
            TunnelStatus::Running.symbol(),
            "running",
            format!("pid {}", external.pid),
            "access",
            url
        );
    }
}

pub async fn cmd_zones(account: Option<&str>, default: Option<&str>) -> Result<()> {
    let (mut config, acct) = resolve(account)?;
    let client = GcpClient::for_account(&acct);
    let zones = compute::list_zones(&client, &acct.project).await?;

    let Some(zone) = default else {
        for name in zones {
            let marker = if name == acct.default_zone { "*" } else { " " };
            println!("{marker} {name}");
        }
        return Ok(());
    };

    if !zones.iter().any(|z| z == zone) {
        bail!("Zone '{zone}' does not exist in project '{}'", acct.project);
    }
    config.get_account_mut(Some(&acct.name))?.default_zone = zone.to_string();
    crate::config::save_config(&config)?;
    println!("Default zone for '{}' is now {zone}", acct.name);
    Ok(())
}

#[cfg(test)]
mod new_tunnel_tests {
    use super::*;

    // `add` without `-s` printed stopped and started the tunnel anyway: the
    // record said auto_start, so install() bootstrapped a RunAtLoad plist
    #[test]
    fn a_new_access_tunnel_does_not_auto_start() {
        let t = new_access_tunnel(
            "db",
            "dev",
            AccessTarget::Instance {
                instance: "my-vm".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            5432,
            15432,
        );
        assert!(!t.auto_start);
        assert_eq!(t.name, "db");
        assert_eq!(t.account_name, "dev");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_target_splits_host_and_port() {
        assert_eq!(
            parse_access_target("my-vm:5432").expect("parse"),
            ("my-vm".to_string(), 5432)
        );
    }

    #[test]
    fn access_target_needs_a_port() {
        let err = parse_access_target("my-vm").expect_err("should fail");
        assert!(err.to_string().contains("expected INSTANCE:PORT"), "{err}");
    }

    #[test]
    fn access_target_rejects_a_port_that_is_not_a_number() {
        let err = parse_access_target("my-vm:http").expect_err("should fail");
        assert!(err.to_string().contains("expected INSTANCE:PORT"), "{err}");
    }

    #[test]
    fn access_target_accepts_an_ip_host() {
        assert_eq!(
            parse_access_target("10.0.0.5:5432").expect("parse"),
            ("10.0.0.5".to_string(), 5432)
        );
    }
}
