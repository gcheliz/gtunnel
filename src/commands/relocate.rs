use anyhow::{bail, Context, Result};

use super::{find_tunnel, resolve};
use crate::daemon;
use crate::expose::routes;
use crate::gcp::GcpClient;
use crate::state::{validate_tunnel_name, PersistentTunnel, TunnelSpec, TunnelState, TunnelStatus};

/// Refuse a new name before anything is touched. A name is unique per
/// account, not globally, so only this account's tunnels can collide.
fn check_rename(state: &TunnelState, account: &str, old: &str, new: &str) -> Result<()> {
    validate_tunnel_name(new)?;
    if state.find_for_account(old, account).is_none() {
        bail!("Tunnel '{old}' not found for account '{account}'");
    }
    if state.find_for_account(new, account).is_some() {
        bail!("Tunnel '{new}' already exists on account '{account}'");
    }
    Ok(())
}

/// The whole of a rename that touches no daemon. A public tunnel's hostname is
/// derived from its name, so it moves with it; the backend and named port are
/// derived from the name at use and need no stored change.
fn rename_record(tunnel: &mut PersistentTunnel, new: &str) {
    tunnel.name = new.to_string();
    if let TunnelSpec::Expose {
        domain, hostname, ..
    } = &mut tunnel.spec
    {
        *hostname = format!("{new}.{domain}");
    }
}

fn rename_in_state(
    state: &mut TunnelState,
    account: &str,
    old: &str,
    new: &str,
) -> Result<PersistentTunnel> {
    check_rename(state, account, old, new)?;
    let mut tunnel = state
        .remove_for_account(old, account)
        .with_context(|| format!("Tunnel '{old}' not found for account '{account}'"))?;
    rename_record(&mut tunnel, new);
    state.add(tunnel.clone())?;
    Ok(tunnel)
}

/// Rename a tunnel and move everything keyed to its name. The order is the
/// point: the old service is gone before the record changes, so a failure
/// leaves either the old tunnel whole or a record `start` can reinstall, and
/// never a job running under a name that no record owns.
pub async fn cmd_rename(account: Option<&str>, old: &str, new: &str) -> Result<()> {
    let (config, acct) = resolve(account)?;
    let mut state = TunnelState::load()?;
    let before = find_tunnel(&state, old, &acct.name)?;
    check_rename(&state, &acct.name, old, new)?;

    let old_label = daemon::platform_label(&acct.name, old);
    let was_running = daemon::status(&old_label).await == TunnelStatus::Running;

    // The old route goes first: it is keyed to the old name and nothing else
    // will ever remove it
    let client = GcpClient::for_account(&acct);
    if before.is_expose() {
        let expose = acct
            .expose
            .clone()
            .context("This account has no relay, but the tunnel is an Expose tunnel")?;
        routes::remove_route(&client, &acct, &expose, &before).await?;
    }

    let _ = daemon::stop(&old_label).await;
    daemon::uninstall(&old_label).await?;

    let renamed = rename_in_state(&mut state, &acct.name, old, new)?;
    state.save()?;
    // Nothing owns the old log now, and the name may be reused
    daemon::remove_log(&before.log_path()?)?;

    // install writes the login copy when auto-start is on, which restores it
    let spec = daemon::service_spec(&config, &renamed)?;
    daemon::install(&spec).await?;

    if renamed.is_expose() {
        routes::ensure_route(&client, &acct, &renamed).await?;
    }
    if was_running {
        daemon::start(&spec.label).await?;
    }
    println!("Renamed {old} to {new}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelState};

    fn access(name: &str, account: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: account.to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "my-vm".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        }
    }

    fn state_of(tunnels: Vec<PersistentTunnel>) -> TunnelState {
        let mut state = TunnelState::default();
        for t in tunnels {
            state.add(t).expect("fixture adds");
        }
        state
    }

    #[test]
    fn renaming_to_a_name_the_account_holds_is_refused() {
        let state = state_of(vec![access("db", "dev"), access("cache", "dev")]);
        let err = check_rename(&state, "dev", "db", "cache").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn renaming_to_a_name_only_another_account_holds_is_allowed() {
        let state = state_of(vec![access("db", "dev"), access("cache", "prod")]);
        assert!(check_rename(&state, "dev", "db", "cache").is_ok());
    }

    #[test]
    fn renaming_to_an_invalid_name_is_refused() {
        let state = state_of(vec![access("db", "dev")]);
        let err = check_rename(&state, "dev", "db", "My_Db").unwrap_err();
        assert!(err.to_string().contains("Invalid tunnel name"), "{err}");
    }

    #[test]
    fn a_rename_moves_the_record_and_leaves_no_old_name() {
        let mut state = state_of(vec![access("db", "dev"), access("other", "prod")]);
        let moved = rename_in_state(&mut state, "dev", "db", "pg").expect("renames");
        assert_eq!(moved.name, "pg");
        assert!(state.find_for_account("db", "dev").is_none());
        assert!(state.find_for_account("pg", "dev").is_some());
        assert_eq!(state.tunnels.len(), 2);
        assert!(state.find_for_account("other", "prod").is_some());
    }

    #[test]
    fn renaming_an_expose_tunnel_rewrites_its_hostname() {
        let mut t = PersistentTunnel {
            name: "api".to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "api.dev.example.com".to_string(),
                relay_port: 30001,
                private: false,
            },
        };
        rename_record(&mut t, "web");
        assert_eq!(t.name, "web");
        match &t.spec {
            TunnelSpec::Expose { hostname, .. } => assert_eq!(hostname, "web.dev.example.com"),
            other => panic!("spec changed kind: {other:?}"),
        }
    }
}
