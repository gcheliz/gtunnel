use crate::error::GtunnelError;
use anyhow::{bail, Context, Result};
use std::path::Path;

use super::{find_tunnel, resolve};
use crate::config::{Account, Config};
use crate::daemon::{self, ServiceSpec};
use crate::expose::naming::hostname_for;
use crate::expose::routes;
use crate::gcp::{compute, GcpClient};
use crate::state::{
    clash_message, validate_tunnel_name, AccessTarget, PersistentTunnel, TunnelSpec, TunnelState,
    TunnelStatus,
};

/// Refuse a rename or a move before anything is touched. A name is unique per
/// account, not globally, so only the destination account's tunnels can
/// collide; a move is a rename whose account changes too.
fn check_relocate(state: &TunnelState, from: &str, to: &str, old: &str, new: &str) -> Result<()> {
    validate_tunnel_name(new)?;
    let Some(tunnel) = state.find_for_account(old, from) else {
        return Err(GtunnelError::not_found(format!(
            "Tunnel '{old}' not found for account '{from}'"
        ))
        .into());
    };
    if from != to {
        if tunnel.is_expose() {
            // A public tunnel's backend, host rule, named port and DNS record
            // live in the source project; rebuilding them in another needs a
            // relay and a domain that may not be there
            bail!(
                "'{old}' is an Expose tunnel and cannot move between accounts; \
                 run `gtunnel delete {old}` on '{from}', then `gtunnel expose` on '{to}'"
            );
        }
        if let TunnelSpec::Access {
            target: AccessTarget::DestGroup { group, .. },
            ..
        } = &tunnel.spec
        {
            // The group is a resource of the source project, so the same
            // argv in the destination would name something that is not there
            bail!(
                "'{old}' reaches its host through the destination group '{group}' of '{from}'; \
                 create the group in '{to}', then `gtunnel delete {old}` and `gtunnel add` it there"
            );
        }
    }
    if state.find_for_account(new, to).is_some() {
        let hint = if from != to {
            "; pass `--rename <new>` to land under another name"
        } else {
            ""
        };
        bail!("Tunnel '{new}' already exists on account '{to}'{hint}");
    }
    // The moving tunnel is still in `state`, and a rename inside one account
    // never holds another account's label, but a move can: drop it from the
    // holders so keeping its own joined name is not a clash
    if let Some(other) = state
        .label_holders(to, new)
        .into_iter()
        .find(|t| !(t.name == old && t.account_name == from))
    {
        bail!("{}", clash_message(to, new, other));
    }
    Ok(())
}

/// The destination's answer to "does the instance exist", read strictly. `add`
/// may shrug at a lookup that cannot run because it destroys nothing; a move has
/// already committed to tearing a working service down, and the ordinary reason
/// for the lookup to fail is a destination account that is not logged in.
fn destination_verdict<T>(
    found: Result<Option<T>>,
    account: &str,
    project: &str,
    zone: &str,
    instance: &str,
) -> Result<()> {
    match found {
        Ok(Some(_)) => Ok(()),
        // The zone is the source tunnel's; a move cannot say otherwise
        Ok(None) => bail!(
            "Instance '{instance}' not found in zone '{zone}' of project '{project}' \
             (account '{account}'); nothing was changed. If it lives in another zone, \
             `gtunnel delete` the tunnel and `gtunnel add` it on '{account}'"
        ),
        Err(e) => Err(e.context(format!(
            "Could not check instance '{instance}' on account '{account}'; nothing was changed"
        ))),
    }
}

/// The start command resolves the account from its own flag, so a tunnel on a
/// non-default account has to be named on it.
fn account_flag(moving: bool, account: &str) -> String {
    if moving {
        format!(" --account {account}")
    } else {
        String::new()
    }
}

/// A move that stays on its account is a rename, which has its own command.
fn check_accounts_differ(from: &str, to: &str, name: &str) -> Result<()> {
    if from == to {
        bail!(
            "Tunnel '{name}' is already on account '{to}'; use `gtunnel rename` to change its name"
        );
    }
    Ok(())
}

/// The name a move lands under: the one the caller chose when the destination
/// holds the old one, otherwise the old name unchanged.
fn target_name<'a>(name: &'a str, rename_to: Option<&'a str>) -> &'a str {
    rename_to.unwrap_or(name)
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

fn relocate_in_state(
    state: &mut TunnelState,
    from: &str,
    to: &str,
    old: &str,
    new: &str,
) -> Result<PersistentTunnel> {
    check_relocate(state, from, to, old, new)?;
    let mut tunnel = state.remove_for_account(old, from).ok_or_else(|| {
        GtunnelError::not_found(format!("Tunnel '{old}' not found for account '{from}'"))
    })?;
    rename_record(&mut tunnel, new);
    // The local port stays: ports are machine-wide, so it is free here too
    tunnel.account_name = to.to_string();
    state.add(tunnel.clone())?;
    Ok(tunnel)
}

/// Everything `run_relocate` does to the outside world, so the order of those
/// effects can be asserted without a daemon or a project to run them against.
trait RenameOps {
    fn label(&self, name: &str) -> String;
    /// Whether what the tunnel points at exists in this ops' account, asked
    /// only of the destination of a move
    async fn verify_target(&self, tunnel: &PersistentTunnel) -> Result<()>;
    async fn is_running(&self, label: &str) -> bool;
    fn spec_for(&self, tunnel: &PersistentTunnel) -> Result<ServiceSpec>;
    async fn remove_route(&self, tunnel: &PersistentTunnel) -> Result<()>;
    async fn stop(&self, label: &str);
    async fn uninstall(&self, label: &str) -> Result<()>;
    // `apply` is the relocation itself, so the live implementation can replay it
    // on a freshly loaded file under the lock instead of overwriting whatever
    // another gtunnel saved since `state` was read
    fn save(
        &self,
        state: &TunnelState,
        apply: &dyn Fn(&mut TunnelState) -> Result<()>,
    ) -> Result<()>;
    async fn install(&self, spec: &ServiceSpec) -> Result<()>;
    async fn ensure_route(&self, tunnel: &PersistentTunnel) -> Result<()>;
    async fn start(&self, label: &str) -> Result<()>;
    fn remove_log(&self, path: &Path) -> Result<()>;
}

/// Rename a tunnel, and with `from != to` move it to another account, taking
/// everything keyed to its name and account with it. `src` is the account the
/// tunnel leaves and `dst` the one it lands in; a rename passes the same ops
/// twice.
///
/// Everything that can be computed or refused is done before the first
/// irreversible step, so a bad name, a missing gcloud or a broken relay config
/// leaves the tunnel untouched. From the route removal onward a failure is not
/// undone: after `remove_route` the old tunnel has no route, after `uninstall`
/// it has no service, and after `save` the record carries the new name. The
/// new tunnel can always be brought up with `gtunnel start <new>`, which
/// reinstalls the service and rebuilds the route. Cleanup that nothing depends
/// on, the old log, runs last.
async fn run_relocate(
    src: &impl RenameOps,
    dst: &impl RenameOps,
    state: &mut TunnelState,
    from: &str,
    to: &str,
    old: &str,
    new: &str,
) -> Result<()> {
    check_relocate(state, from, to, old, new)?;
    let before = state.find_for_account(old, from).cloned().ok_or_else(|| {
        GtunnelError::not_found(format!("Tunnel '{old}' not found for account '{from}'"))
    })?;
    let old_log = before.log_path()?;
    let mut renamed = before.clone();
    rename_record(&mut renamed, new);
    renamed.account_name = to.to_string();
    let moving = from != to;
    // The one check that asks a remote project a question, so it goes before
    // everything else: a typo in the destination must cost nothing
    if moving {
        dst.verify_target(&renamed).await?;
    }
    // Pure, and it fails on the things most likely to be wrong: install it
    // later with this spec, but find out now
    let spec = dst.spec_for(&renamed)?;

    let old_label = src.label(old);
    let was_running = src.is_running(&old_label).await;

    // The old route goes first: it is keyed to the old name and nothing else
    // will ever remove it
    if before.is_expose() {
        src.remove_route(&before).await?;
    }
    src.stop(&old_label).await;
    src.uninstall(&old_label).await?;

    let apply = |s: &mut TunnelState| relocate_in_state(s, from, to, old, new).map(|_| ());
    apply(state)?;
    // Past the uninstall the old service is gone; a failed save leaves the
    // record where it was, and `start` rebuilds the service from that record
    dst.save(state, &apply).map_err(|e| {
        e.context(format!(
            "'{old}' was stopped and uninstalled but the record was not saved, so it is \
             still on account '{from}'; run `gtunnel start {old}{}` to bring it back",
            account_flag(moving, from)
        ))
    })?;

    // install writes the login copy when auto-start is on, which restores it
    // The record is already on the destination, so a plain `start` would look
    // for it on the default account and find nothing
    dst.install(&spec).await.map_err(|e| {
        e.context(format!(
            "'{new}' is now on account '{to}' but its service was not installed; \
             run `gtunnel start {new}{}` to retry",
            account_flag(moving, to)
        ))
    })?;
    let route = if renamed.is_expose() {
        dst.ensure_route(&renamed).await
    } else {
        Ok(())
    };
    // A tunnel that was running is brought back even when its route could not
    // be rebuilt: stopped helps nobody, and the error below says what is wrong
    let start = if was_running {
        dst.start(&spec.label).await
    } else {
        Ok(())
    };
    // Cleanup, not a precondition, so it neither aborts nor waits on the
    // outcome above: the old log is orphaned either way, and a reused name
    // would inherit it
    if let Err(e) = src.remove_log(&old_log) {
        eprintln!("Warning: could not remove the old log: {e:#}");
    }
    let verb = if moving { "moved" } else { "renamed" };
    let done = if was_running {
        format!("{verb}, installed and restarted")
    } else {
        format!("{verb} and installed")
    };
    // The start command resolves the account from its own flag, so a moved
    // tunnel has to be named on the account it now lives on
    let retry = format!(
        "run `gtunnel start {new}{}` to retry",
        account_flag(moving, to)
    );
    match (route, start) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(route), Ok(())) => Err(route.context(format!(
            "'{new}' was {done}, but its route was not rebuilt; {retry}"
        ))),
        (Ok(()), Err(start)) => Err(start.context(format!(
            "'{new}' was {verb} and installed, but it could not be started; {retry}"
        ))),
        (Err(route), Err(start)) => bail!(
            "'{new}' was {verb} and installed, but its route was not rebuilt ({route:#}) \
             and it could not be started ({start:#}); {retry}"
        ),
    }
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
    async fn verify_target(&self, tunnel: &PersistentTunnel) -> Result<()> {
        match &tunnel.spec {
            TunnelSpec::Access {
                target:
                    AccessTarget::Instance { instance, zone }
                    | AccessTarget::Jump { instance, zone, .. },
                ..
            } => {
                let found =
                    compute::get_instance(&self.client, &self.acct.project, zone, instance).await;
                destination_verdict(found, &self.acct.name, &self.acct.project, zone, instance)
            }
            // Refused before this is reached: a group is not portable, and an
            // Expose tunnel does not move
            _ => Ok(()),
        }
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
    fn save(&self, _: &TunnelState, apply: &dyn Fn(&mut TunnelState) -> Result<()>) -> Result<()> {
        TunnelState::update(apply)
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

pub async fn cmd_move(
    name: &str,
    from: Option<&str>,
    to: &str,
    rename_to: Option<&str>,
) -> Result<()> {
    // Two accounts, so two ops: the source stops and cleans up the old
    // service, the destination verifies, installs and starts the new one
    let (config, src_acct) = resolve(from)?;
    let dst_acct = config.get_account(Some(to))?.clone();
    check_accounts_differ(&src_acct.name, &dst_acct.name, name)?;
    // main authenticated the source only; the destination is asked a question
    // before the first write, and an unauthenticated one cannot answer it
    crate::gcp::gcloud::ensure_logged_in(&dst_acct).await?;
    let mut state = TunnelState::load()?;
    find_tunnel(&state, name, &src_acct.name)?;
    let new = target_name(name, rename_to);
    let src = LiveOps {
        client: GcpClient::for_account(&src_acct),
        config: config.clone(),
        acct: src_acct.clone(),
    };
    let dst = LiveOps {
        client: GcpClient::for_account(&dst_acct),
        config,
        acct: dst_acct.clone(),
    };
    // The old name is about to stop on purpose; left in the ledger as Up, the
    // watchdog's next pass would announce a crash that was a move
    crate::lifecycle::forget(&src_acct.name, name);
    run_relocate(
        &src,
        &dst,
        &mut state,
        &src_acct.name,
        &dst_acct.name,
        name,
        new,
    )
    .await?;
    println!("Moved {name} from {} to {}", src_acct.name, dst_acct.name);
    if new != name {
        println!("It is now called {new}");
    }
    Ok(())
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
    // Same reason as in `cmd_move`: the old name stops on purpose
    crate::lifecycle::forget(&acct.name, old);
    run_relocate(&ops, &ops, &mut state, &acct.name, &acct.name, old, new).await?;
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
    fn a_move_onto_another_accounts_joined_name_is_refused() {
        // `api` on prod-eu already holds prod-eu-api
        let state = state_of(vec![access("api", "prod-eu"), access("x", "dev")]);
        let err = check_relocate(&state, "dev", "prod", "x", "eu-api").unwrap_err();
        assert!(
            err.to_string()
                .contains("tunnel 'api' on account 'prod-eu'"),
            "{err}"
        );
    }

    #[test]
    fn a_move_that_keeps_its_own_joined_name_is_allowed() {
        // the only holder of prod-eu-api is the tunnel being moved
        let state = state_of(vec![access("eu-api", "prod")]);
        assert!(check_relocate(&state, "prod", "prod-eu", "eu-api", "api").is_ok());
    }

    #[test]
    fn renaming_to_a_name_the_account_holds_is_refused() {
        let state = state_of(vec![access("db", "dev"), access("cache", "dev")]);
        let err = check_relocate(&state, "dev", "dev", "db", "cache").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn renaming_to_a_name_only_another_account_holds_is_allowed() {
        let state = state_of(vec![access("db", "dev"), access("cache", "prod")]);
        assert!(check_relocate(&state, "dev", "dev", "db", "cache").is_ok());
    }

    #[test]
    fn renaming_to_an_invalid_name_is_refused() {
        let state = state_of(vec![access("db", "dev")]);
        let err = check_relocate(&state, "dev", "dev", "db", "My_Db").unwrap_err();
        assert!(err.to_string().contains("Invalid tunnel name"), "{err}");
    }

    #[test]
    fn a_rename_moves_the_record_and_leaves_no_old_name() {
        let mut state = state_of(vec![access("db", "dev"), access("other", "prod")]);
        let moved = relocate_in_state(&mut state, "dev", "dev", "db", "pg").expect("renames");
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
    use std::rc::Rc;

    /// Records every effect in order; a named step can be made to fail.
    struct Recorder {
        calls: Rc<RefCell<Vec<String>>>,
        /// Prefixes every recorded step and label, so two recorders sharing
        /// one log tell the source's effects from the destination's
        tag: &'static str,
        fail: Vec<&'static str>,
        running: bool,
        specs_built: RefCell<u32>,
        installed: RefCell<Option<ServiceSpec>>,
    }

    impl Recorder {
        fn new(fail: &'static str, running: bool) -> Self {
            Self {
                calls: Rc::new(RefCell::new(Vec::new())),
                tag: "",
                fail: vec![fail],
                running,
                specs_built: RefCell::new(0),
                installed: RefCell::new(None),
            }
        }
        /// A second recorder writing to the same log under its own tag
        fn sharing(&self, tag: &'static str, fail: &'static str, running: bool) -> Self {
            Self {
                calls: Rc::clone(&self.calls),
                tag,
                fail: vec![fail],
                running,
                specs_built: RefCell::new(0),
                installed: RefCell::new(None),
            }
        }
        fn tagged(mut self, tag: &'static str) -> Self {
            self.tag = tag;
            self
        }
        fn and_failing(mut self, step: &'static str) -> Self {
            self.fail.push(step);
            self
        }
        fn step(&self, name: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("{}{name}", self.tag));
            if self.fail.contains(&name) {
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
            format!("{}label-{name}", self.tag)
        }
        async fn verify_target(&self, _: &PersistentTunnel) -> Result<()> {
            self.step("verify")
        }
        async fn is_running(&self, _: &str) -> bool {
            self.running
        }
        fn spec_for(&self, t: &PersistentTunnel) -> Result<ServiceSpec> {
            self.step("spec")?;
            *self.specs_built.borrow_mut() += 1;
            Ok(ServiceSpec {
                label: self.label(&t.name),
                program: PathBuf::from("gcloud"),
                // Tells one built spec from another
                args: vec![format!("spec-{}", self.specs_built.borrow())],
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
        fn save(&self, _: &TunnelState, _: &dyn Fn(&mut TunnelState) -> Result<()>) -> Result<()> {
            self.step("save")
        }
        async fn install(&self, spec: &ServiceSpec) -> Result<()> {
            *self.installed.borrow_mut() = Some(spec.clone());
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
        let result = futures_block(run_relocate(
            ops, ops, &mut state, "dev", "dev", "old", "new",
        ));
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

    #[test]
    fn a_start_failure_says_the_rename_already_happened() {
        let ops = Recorder::new("start", true);
        let (result, state) = run(&ops, access("old", "dev"));
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("renamed"), "{msg}");
        assert!(msg.contains("gtunnel start new"), "{msg}");
        assert!(msg.contains("start failed"), "{msg}");
        assert!(state.find_for_account("new", "dev").is_some());
    }

    #[test]
    fn a_route_failure_is_not_lost_when_the_start_fails_too() {
        let ops = Recorder::new("ensure_route", true).and_failing("start");
        let (result, _) = run(&ops, public("old"));
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("ensure_route failed"), "{msg}");
        assert!(msg.contains("start failed"), "{msg}");
        assert!(msg.contains("gtunnel start new"), "{msg}");
    }

    #[test]
    fn the_old_log_is_removed_even_when_the_route_could_not_be_rebuilt() {
        let ops = Recorder::new("ensure_route", false);
        let (result, _) = run(&ops, public("old"));
        assert!(result.is_err());
        assert_eq!(ops.calls().last().map(String::as_str), Some("remove_log"));
    }

    #[test]
    fn the_spec_built_before_the_first_effect_is_the_one_installed() {
        let ops = Recorder::new("none", false);
        let (result, _) = run(&ops, public("old"));
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(*ops.specs_built.borrow(), 1, "the spec is built once");
        let installed = ops.installed.borrow().clone().expect("installed");
        assert_eq!(installed.args, vec!["spec-1"]);
        assert_eq!(installed.label, "label-new");
    }

    fn moved(
        src: &Recorder,
        dst: &Recorder,
        state: &mut TunnelState,
        name: &str,
        new: &str,
    ) -> Result<()> {
        futures_block(run_relocate(src, dst, state, "dev", "prod", name, new))
    }

    #[test]
    fn moving_an_expose_tunnel_is_refused_with_the_path_to_take() {
        let state = state_of(vec![public("api")]);
        let err = check_relocate(&state, "dev", "prod", "api", "api").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("delete"), "{msg}");
        assert!(msg.contains("expose"), "{msg}");
    }

    #[test]
    fn moving_onto_a_taken_name_without_a_rename_is_refused() {
        let state = state_of(vec![access("db", "dev"), access("db", "prod")]);
        let err = check_relocate(&state, "dev", "prod", "db", "db").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(err.to_string().contains("prod"), "{err}");
    }

    #[test]
    fn moving_onto_a_taken_name_with_a_rename_lands_under_the_new_name() {
        let mut state = state_of(vec![access("db", "dev"), access("db", "prod")]);
        let src = Recorder::new("none", false).tagged("src:");
        let dst = src.sharing("dst:", "none", false);
        moved(&src, &dst, &mut state, "db", "pg").expect("moves");
        assert!(state.find_for_account("db", "dev").is_none());
        let landed = state.find_for_account("pg", "prod").expect("lands as pg");
        assert_eq!(landed.account_name, "prod");
        assert!(
            state.find_for_account("db", "prod").is_some(),
            "the destination's own db is untouched"
        );
        assert_eq!(state.tunnels.len(), 2);
    }

    #[test]
    fn a_move_verifies_the_instance_exists_in_the_destination_first() {
        let mut state = state_of(vec![access("db", "dev")]);
        let src = Recorder::new("none", true).tagged("src:");
        let dst = src.sharing("dst:", "verify", true);
        let result = moved(&src, &dst, &mut state, "db", "db");
        assert!(result.is_err());
        assert_eq!(src.calls(), vec!["dst:verify"], "nothing else ran");
        assert!(state.find_for_account("db", "dev").is_some());
        assert!(state.find_for_account("db", "prod").is_none());
    }

    #[test]
    fn a_move_to_the_account_the_tunnel_is_already_on_is_refused() {
        let err = check_accounts_differ("dev", "dev", "db").unwrap_err();
        assert!(err.to_string().contains("already on"), "{err}");
        assert!(check_accounts_differ("dev", "prod", "db").is_ok());
    }

    #[test]
    fn a_destination_group_tunnel_is_refused_because_its_group_is_in_the_old_project() {
        let mut t = access("db", "dev");
        t.spec = TunnelSpec::Access {
            target: AccessTarget::DestGroup {
                host: "db.internal".to_string(),
                region: "europe-west1".to_string(),
                group: "my-group".to_string(),
                network: "default".to_string(),
            },
            remote_port: 5432,
            local_port: 15432,
        };
        let state = state_of(vec![t]);
        let err = check_relocate(&state, "dev", "prod", "db", "db").unwrap_err();
        assert!(err.to_string().contains("destination group"), "{err}");
    }

    #[test]
    fn a_move_stops_the_source_service_and_installs_the_destination_one() {
        let mut state = state_of(vec![access("db", "dev")]);
        let src = Recorder::new("none", true).tagged("src:");
        let dst = src.sharing("dst:", "none", false);
        moved(&src, &dst, &mut state, "db", "db").expect("moves");
        assert_eq!(
            src.calls(),
            vec![
                "dst:verify",
                "dst:spec",
                "src:stop",
                "src:uninstall",
                "dst:save",
                "dst:install",
                "dst:start",
                "src:remove_log",
            ]
        );
        let installed = dst.installed.borrow().clone().expect("installed");
        assert_eq!(installed.label, "dst:label-db");
        assert!(state.find_for_account("db", "prod").is_some());
        assert!(state.find_for_account("db", "dev").is_none());
    }

    #[test]
    fn a_failed_move_names_the_account_to_retry_on() {
        let mut state = state_of(vec![access("db", "dev")]);
        let src = Recorder::new("none", true).tagged("src:");
        let dst = src.sharing("dst:", "start", true);
        let msg = format!(
            "{:#}",
            moved(&src, &dst, &mut state, "db", "pg").unwrap_err()
        );
        assert!(msg.contains("moved"), "{msg}");
        assert!(msg.contains("gtunnel start pg --account prod"), "{msg}");
        assert!(state.find_for_account("pg", "prod").is_some());
    }

    #[test]
    fn a_move_target_name_is_the_rename_or_the_old_name() {
        assert_eq!(target_name("db", None), "db");
        assert_eq!(target_name("db", Some("pg")), "pg");
    }

    #[test]
    fn a_destination_lookup_that_cannot_run_is_fatal_and_says_nothing_changed() {
        let err = destination_verdict::<()>(
            Err(anyhow::anyhow!("not logged in")),
            "prod",
            "p",
            "z",
            "vm",
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("prod"), "{msg}");
        assert!(msg.contains("nothing was changed"), "{msg}");
        assert!(msg.contains("not logged in"), "{msg}");
    }

    #[test]
    fn an_instance_missing_in_the_destination_zone_points_at_delete_and_add() {
        let err =
            destination_verdict::<()>(Ok(None), "prod", "p", "europe-west1-b", "vm").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("europe-west1-b"), "{msg}");
        assert!(msg.contains("delete") && msg.contains("add"), "{msg}");
        assert!(destination_verdict(Ok(Some(())), "prod", "p", "z", "vm").is_ok());
    }

    #[test]
    fn a_save_that_fails_after_the_uninstall_says_where_the_tunnel_still_is() {
        let mut state = state_of(vec![access("db", "dev")]);
        let src = Recorder::new("none", true).tagged("src:");
        let dst = src.sharing("dst:", "save", true);
        let msg = format!(
            "{:#}",
            moved(&src, &dst, &mut state, "db", "pg").unwrap_err()
        );
        assert!(msg.contains("save failed"), "{msg}");
        assert!(msg.contains("still on account 'dev'"), "{msg}");
        assert!(msg.contains("gtunnel start db --account dev"), "{msg}");
    }

    #[test]
    fn an_install_that_fails_after_the_save_says_the_tunnel_is_on_the_destination() {
        let mut state = state_of(vec![access("db", "dev")]);
        let src = Recorder::new("none", true).tagged("src:");
        let dst = src.sharing("dst:", "install", true);
        let msg = format!(
            "{:#}",
            moved(&src, &dst, &mut state, "db", "pg").unwrap_err()
        );
        assert!(msg.contains("install failed"), "{msg}");
        assert!(msg.contains("now on account 'prod'"), "{msg}");
        assert!(msg.contains("gtunnel start pg --account prod"), "{msg}");
    }

    #[test]
    fn a_taken_name_on_a_move_names_the_rename_flag() {
        let state = state_of(vec![access("db", "dev"), access("db", "prod")]);
        let err = check_relocate(&state, "dev", "prod", "db", "db").unwrap_err();
        assert!(err.to_string().contains("--rename"), "{err}");
    }

    #[test]
    fn a_move_keeps_auto_start_and_the_local_port() {
        let mut t = access("db", "dev");
        t.auto_start = true;
        let mut state = state_of(vec![t]);
        let moved = relocate_in_state(&mut state, "dev", "prod", "db", "db").expect("moves");
        assert!(moved.auto_start);
        match moved.spec {
            TunnelSpec::Access { local_port, .. } => assert_eq!(local_port, 15432),
            other => panic!("spec changed kind: {other:?}"),
        }
    }
}
