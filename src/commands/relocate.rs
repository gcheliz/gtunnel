use anyhow::{bail, Context, Result};
use std::path::Path;

use super::{find_tunnel, resolve};
use crate::config::{Account, Config};
use crate::daemon::{self, ServiceSpec};
use crate::expose::naming::hostname_for;
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
        *hostname = hostname_for(new, domain);
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

/// Everything `run_rename` does to the outside world, so the order of those
/// effects can be asserted without a daemon or a project to run them against.
trait RenameOps {
    fn label(&self, name: &str) -> String;
    async fn is_running(&self, label: &str) -> bool;
    fn spec_for(&self, tunnel: &PersistentTunnel) -> Result<ServiceSpec>;
    async fn remove_route(&self, tunnel: &PersistentTunnel) -> Result<()>;
    async fn stop(&self, label: &str);
    async fn uninstall(&self, label: &str) -> Result<()>;
    fn save(&self, state: &TunnelState) -> Result<()>;
    async fn install(&self, spec: &ServiceSpec) -> Result<()>;
    async fn ensure_route(&self, tunnel: &PersistentTunnel) -> Result<()>;
    async fn start(&self, label: &str) -> Result<()>;
    fn remove_log(&self, path: &Path) -> Result<()>;
}

/// Rename a tunnel and move everything keyed to its name.
///
/// Everything that can be computed or refused is done before the first
/// irreversible step, so a bad name, a missing gcloud or a broken relay config
/// leaves the tunnel untouched. From the route removal onward a failure is not
/// undone: after `remove_route` the old tunnel has no route, after `uninstall`
/// it has no service, and after `save` the record carries the new name. The
/// new tunnel can always be brought up with `gtunnel start <new>`, which
/// reinstalls the service and rebuilds the route. Cleanup that nothing depends
/// on, the old log, runs last.
async fn run_rename(
    ops: &impl RenameOps,
    state: &mut TunnelState,
    account: &str,
    old: &str,
    new: &str,
) -> Result<()> {
    check_rename(state, account, old, new)?;
    let before = state
        .find_for_account(old, account)
        .cloned()
        .with_context(|| format!("Tunnel '{old}' not found for account '{account}'"))?;
    let old_log = before.log_path()?;
    let mut renamed = before.clone();
    rename_record(&mut renamed, new);
    // Pure, and it fails on the things most likely to be wrong: install it
    // later with this spec, but find out now
    let spec = ops.spec_for(&renamed)?;

    let old_label = ops.label(old);
    let was_running = ops.is_running(&old_label).await;

    // The old route goes first: it is keyed to the old name and nothing else
    // will ever remove it
    if before.is_expose() {
        ops.remove_route(&before).await?;
    }
    ops.stop(&old_label).await;
    ops.uninstall(&old_label).await?;

    rename_in_state(state, account, old, new)?;
    ops.save(state)?;

    // install writes the login copy when auto-start is on, which restores it
    ops.install(&spec).await?;
    let route = if renamed.is_expose() {
        ops.ensure_route(&renamed).await
    } else {
        Ok(())
    };
    // A tunnel that was running is brought back even when its route could not
    // be rebuilt: stopped helps nobody, and the error below says what is wrong
    if was_running {
        ops.start(&spec.label).await?;
    }
    if let Err(e) = route {
        let state_word = if was_running {
            "restarted"
        } else {
            "installed"
        };
        return Err(e.context(format!(
            "'{new}' was renamed and {state_word}, but its route was not rebuilt; \
             run `gtunnel start {new}` to retry"
        )));
    }
    // Cleanup, not a precondition: a log that cannot be removed must not
    // undo a rename that has already happened
    if let Err(e) = ops.remove_log(&old_log) {
        eprintln!("Warning: could not remove the old log: {e:#}");
    }
    Ok(())
}

struct LiveOps {
    config: Config,
    acct: Account,
    client: GcpClient,
}

impl RenameOps for LiveOps {
    fn label(&self, name: &str) -> String {
        daemon::platform_label(&self.acct.name, name)
    }
    async fn is_running(&self, label: &str) -> bool {
        daemon::status(label).await == TunnelStatus::Running
    }
    fn spec_for(&self, tunnel: &PersistentTunnel) -> Result<ServiceSpec> {
        daemon::service_spec(&self.config, tunnel)
    }
    async fn remove_route(&self, tunnel: &PersistentTunnel) -> Result<()> {
        let expose = self
            .acct
            .expose
            .clone()
            .context("This account has no relay, but the tunnel is an Expose tunnel")?;
        routes::remove_route(&self.client, &self.acct, &expose, tunnel)
            .await
            .map(|_| ())
    }
    async fn stop(&self, label: &str) {
        let _ = daemon::stop(label).await;
    }
    async fn uninstall(&self, label: &str) -> Result<()> {
        daemon::uninstall(label).await
    }
    fn save(&self, state: &TunnelState) -> Result<()> {
        state.save()
    }
    async fn install(&self, spec: &ServiceSpec) -> Result<()> {
        daemon::install(spec).await
    }
    async fn ensure_route(&self, tunnel: &PersistentTunnel) -> Result<()> {
        routes::ensure_route(&self.client, &self.acct, tunnel).await
    }
    async fn start(&self, label: &str) -> Result<()> {
        daemon::start(label).await
    }
    fn remove_log(&self, path: &Path) -> Result<()> {
        daemon::remove_log(path)
    }
}

pub async fn cmd_rename(account: Option<&str>, old: &str, new: &str) -> Result<()> {
    let (config, acct) = resolve(account)?;
    let mut state = TunnelState::load()?;
    find_tunnel(&state, old, &acct.name)?;
    let ops = LiveOps {
        client: GcpClient::for_account(&acct),
        config,
        acct: acct.clone(),
    };
    run_rename(&ops, &mut state, &acct.name, old, new).await?;
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

    use std::cell::RefCell;
    use std::path::PathBuf;

    /// Records every effect in order; a named step can be made to fail.
    struct Recorder {
        calls: RefCell<Vec<String>>,
        fail: &'static str,
        running: bool,
    }

    impl Recorder {
        fn new(fail: &'static str, running: bool) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail,
                running,
            }
        }
        fn step(&self, name: &str) -> Result<()> {
            self.calls.borrow_mut().push(name.to_string());
            if self.fail == name {
                bail!("{name} failed");
            }
            Ok(())
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl RenameOps for Recorder {
        fn label(&self, name: &str) -> String {
            format!("label-{name}")
        }
        async fn is_running(&self, _: &str) -> bool {
            self.running
        }
        fn spec_for(&self, t: &PersistentTunnel) -> Result<ServiceSpec> {
            self.step("spec")?;
            Ok(ServiceSpec {
                label: self.label(&t.name),
                program: PathBuf::from("gcloud"),
                args: Vec::new(),
                log_path: PathBuf::from("log"),
                env: Vec::new(),
                auto_start: false,
                description: String::new(),
            })
        }
        async fn remove_route(&self, _: &PersistentTunnel) -> Result<()> {
            self.step("remove_route")
        }
        async fn stop(&self, _: &str) {
            let _ = self.step("stop");
        }
        async fn uninstall(&self, _: &str) -> Result<()> {
            self.step("uninstall")
        }
        fn save(&self, _: &TunnelState) -> Result<()> {
            self.step("save")
        }
        async fn install(&self, _: &ServiceSpec) -> Result<()> {
            self.step("install")
        }
        async fn ensure_route(&self, _: &PersistentTunnel) -> Result<()> {
            self.step("ensure_route")
        }
        async fn start(&self, _: &str) -> Result<()> {
            self.step("start")
        }
        fn remove_log(&self, _: &Path) -> Result<()> {
            self.step("remove_log")
        }
    }

    fn public(name: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: format!("{name}.dev.example.com"),
                relay_port: 30001,
                private: false,
            },
        }
    }

    fn run(ops: &Recorder, t: PersistentTunnel) -> (Result<()>, TunnelState) {
        let mut state = state_of(vec![t]);
        let result = futures_block(run_rename(ops, &mut state, "dev", "old", "new"));
        (result, state)
    }

    fn futures_block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    #[test]
    fn a_spec_that_cannot_be_built_stops_the_rename_before_anything_is_touched() {
        let ops = Recorder::new("spec", true);
        let (result, state) = run(&ops, public("old"));
        assert!(result.is_err());
        assert_eq!(ops.calls(), vec!["spec"], "nothing irreversible ran");
        assert!(state.find_for_account("old", "dev").is_some());
        assert!(state.find_for_account("new", "dev").is_none());
    }

    #[test]
    fn a_log_that_cannot_be_removed_does_not_undo_the_rename() {
        let ops = Recorder::new("remove_log", true);
        let (result, state) = run(&ops, access("old", "dev"));
        assert!(result.is_ok(), "{result:?}");
        let calls = ops.calls();
        assert!(calls.contains(&"install".to_string()), "{calls:?}");
        assert!(calls.contains(&"start".to_string()), "{calls:?}");
        assert_eq!(calls.last().map(String::as_str), Some("remove_log"));
        assert!(state.find_for_account("new", "dev").is_some());
    }

    #[test]
    fn a_route_failure_still_restarts_a_running_tunnel_and_says_so() {
        let ops = Recorder::new("ensure_route", true);
        let (result, _) = run(&ops, public("old"));
        assert!(
            ops.calls().contains(&"start".to_string()),
            "{:?}",
            ops.calls()
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("restarted"), "{msg}");
        assert!(msg.contains("gtunnel start new"), "{msg}");
    }
}
