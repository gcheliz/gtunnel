use anyhow::{bail, Context, Result};
use std::time::Duration;

use super::{find_tunnel, resolve, status_word, tunnel_status, ListLayout, ListRow};
use crate::config::Account;
use crate::daemon;
use crate::expose::routes;
use crate::gcp::{compute, GcpClient};
use crate::lifecycle::{self, Cause, State};
use crate::state::{
    allocate_port, local_port_is_free, validate_host, validate_instance_name, validate_tunnel_name,
    validate_zone, AccessTarget, PersistentTunnel, TunnelSpec, TunnelState, TunnelStatus,
    ACCESS_PORT_BASE, ACCESS_PORT_SPAN,
};
use crate::tunnel::diagnose_log_line;

// A tunnel with nothing to listen for, a public one, has no readiness signal
// to wait on, so it is given this long before its status is believed
const EXPOSE_SETTLE: Duration = Duration::from_secs(3);
// 20s because an ssh jump tunnel took 7s to listen in a real log, and a
// deadline that close to the slowest start seen would fail a healthy one
const START_DEADLINE: Duration = Duration::from_secs(20);
const READY_POLL: Duration = Duration::from_millis(100);
// A service that crashed is read from the manager, which is a subprocess, so
// it is asked a few times a second and not on every poll
const STATUS_EVERY: u32 = 5;
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

/// What `add` needs, named so the CLI, the MCP server and the TUI build the same
/// request instead of passing ten positional arguments in the same order.
pub struct AddRequest<'a> {
    pub name: &'a str,
    pub target: &'a str,
    pub via: Option<&'a str>,
    pub zone: Option<&'a str>,
    pub local_port: Option<u16>,
    pub dest_group: Option<&'a str>,
    pub region: Option<&'a str>,
    pub network: Option<&'a str>,
    pub start: bool,
}

impl AddRequest<'_> {
    /// The checks that need no account and no gcloud, in one place so the CLI,
    /// the TUI and the MCP server refuse the same values with the same words,
    /// and so a bad instance is refused before `verify_instance` hands it to
    /// the API. The MCP server maps this error to Invalid params.
    pub fn validate(&self) -> Result<()> {
        validate_tunnel_name(self.name)?;
        let (host, _) = parse_access_target(self.target)?;
        if let Some(zone) = self.zone {
            validate_zone(zone)?;
        }
        match (self.via, self.dest_group) {
            (Some(instance), _) => {
                validate_instance_name(instance)?;
                validate_host(&host)
            }
            (None, Some(_)) => validate_host(&host),
            (None, None) => validate_instance_name(&host),
        }
    }
}

pub struct Added {
    pub name: String,
    pub display_url: String,
    pub display_target: String,
    pub started: Option<Started>,
}

// The record and the service, without the start: the CLI prints the added line
// between the two, so a start that fails still leaves it on screen
async fn save_tunnel(account: Option<&str>, req: &AddRequest<'_>) -> Result<Added> {
    let AddRequest {
        name,
        target,
        via,
        zone,
        local_port,
        dest_group,
        region,
        network,
        ..
    } = *req;
    req.validate()?;
    let (config, acct) = resolve(account)?;
    // A plain read to fail fast, before any gcloud call; `add` inside the
    // locked update below is the check that holds
    if TunnelState::load()?
        .find_for_account(name, &acct.name)
        .is_some()
    {
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

    // The port choice reads the saved ports, so it belongs under the same lock
    // as the save; the gcloud checks above are outside it
    let tunnel = TunnelState::update(|state| {
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
        Ok(tunnel)
    })?;

    let spec = daemon::service_spec(&config, &tunnel)?;
    daemon::install(&spec).await?;
    Ok(Added {
        name: tunnel.name.clone(),
        display_url: tunnel.display_url(),
        display_target: tunnel.display_target(),
        started: None,
    })
}

/// The record is saved before the start is tried and stays when it fails, so a
/// failed `start: true` is an error about a tunnel that now exists, not about
/// one that was never made.
pub async fn add_tunnel(account: Option<&str>, req: AddRequest<'_>) -> Result<Added> {
    let mut added = save_tunnel(account, &req).await?;
    if req.start {
        added.started = Some(start_tunnel(account, req.name).await?);
    }
    Ok(added)
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
    let req = AddRequest {
        name,
        target,
        via,
        zone,
        local_port,
        dest_group,
        region,
        network,
        start,
    };
    let added = save_tunnel(account, &req).await?;
    println!(
        "{} {}  {} -> {}",
        TunnelStatus::Stopped.symbol(),
        added.name,
        added.display_url,
        added.display_target
    );
    if start {
        print_started(&start_tunnel(account, name).await.map_err(print_failed)?);
    }
    Ok(())
}

// A typo in an instance name otherwise surfaces as a tunnel that never starts
pub(crate) async fn verify_instance(acct: &Account, zone: &str, instance: &str) -> Result<()> {
    let client = GcpClient::for_account(acct);
    verify_instance_with_client(&client, acct, zone, instance).await
}

pub(crate) async fn verify_instance_with_client(
    client: &GcpClient,
    acct: &Account,
    zone: &str,
    instance: &str,
) -> Result<()> {
    match compute::get_instance(client, &acct.project, zone, instance).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => bail!(
            "Instance '{instance}' not found in zone '{zone}' of project '{}'. Run `gtunnel zones` to check available zones",
            acct.project
        ),
        // A lookup that cannot run is not proof the instance is missing
        Err(e) => {
            eprintln!("Warning: could not verify instance '{instance}': {e:#}");
            Ok(())
        }
    }
}

pub struct Started {
    pub name: String,
    pub url: String,
}

/// A tunnel that did not come up. The text is what a person reads on the CLI
/// and in the TUI status line, and what an MCP client gets as the tool's error,
/// so it carries the hint and the way to the log itself.
#[derive(Debug)]
pub struct StartFailed {
    pub name: String,
    pub hint: Option<String>,
}

impl std::fmt::Display for StartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed to start", self.name)?;
        if let Some(hint) = &self.hint {
            write!(f, "\n  {hint}")?;
        }
        write!(f, "\n  Logs: gtunnel logs {}", self.name)
    }
}

impl std::error::Error for StartFailed {}

/// A failed start the CLI has already put on screen. `main` exits 1 on it
/// without printing again; anything else that reaches it, such as the TUI
/// creating a public tunnel through `cmd_expose`, still reads the text.
#[derive(Debug)]
pub struct Printed(pub StartFailed);

impl std::fmt::Display for Printed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Printed {}

// The CLI keeps the lines a failed start always printed, glyph included, on
// stdout where a script reading `gtunnel start` already looks for them; only
// the exit code changes
fn print_failed(e: anyhow::Error) -> anyhow::Error {
    match e.downcast::<StartFailed>() {
        Ok(failed) => {
            println!("{} {failed}", TunnelStatus::Error.symbol());
            Printed(failed).into()
        }
        Err(other) => other,
    }
}

/// How long a start waits for readiness before it fails.
/// `GTUNNEL_START_DEADLINE_MS` overrides it, as `GTUNNEL_SERVICE_BOUND_MS` does
/// for the service manager's bound, so a test or a slow link can move it.
pub fn start_deadline() -> Duration {
    deadline_from(std::env::var("GTUNNEL_START_DEADLINE_MS").ok().as_deref())
}

fn deadline_from(value: Option<&str>) -> Duration {
    value
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(START_DEADLINE)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    // The service manager reports the job as failed
    Failed,
    TimedOut,
}

/// Waits for the tunnel to be listening, not for a fixed time to pass.
///
/// The supervisor writes `ready_file` when the tunnel's own log says it is
/// listening. `port` is the second witness and is passed only when that file
/// cannot be relied on: at log level `quiet` ssh prints no listener line, so a
/// jump tunnel never gets one. It is a bare connect and not `probe::tcp_probe`,
/// which registers into the probe ledger and does a round trip through the far
/// end; readiness asks only whether something is listening here.
pub async fn wait_ready(
    label: &str,
    ready_file: Option<&std::path::Path>,
    port: Option<u16>,
    deadline: Duration,
) -> Readiness {
    let started = std::time::Instant::now();
    let mut polls: u32 = 0;
    loop {
        if ready_file.is_some_and(|p| p.exists()) {
            return Readiness::Ready;
        }
        if let Some(port) = port
            && listening(port).await
        {
            return Readiness::Ready;
        }
        if polls.is_multiple_of(STATUS_EVERY) && daemon::status(label).await == TunnelStatus::Error
        {
            return Readiness::Failed;
        }
        polls = polls.wrapping_add(1);
        if started.elapsed() >= deadline {
            return Readiness::TimedOut;
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

async fn listening(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(200),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await,
        Ok(Ok(_))
    )
}

// The ready file `service_spec` handed the supervisor, read back from the
// argv it built so the two cannot name different paths
fn ready_file_of(spec: &daemon::ServiceSpec) -> Option<std::path::PathBuf> {
    let at = spec.args.iter().position(|a| a == "--ready-file")?;
    spec.args.get(at + 1).map(std::path::PathBuf::from)
}

fn is_quiet(spec: &daemon::ServiceSpec) -> bool {
    spec.args
        .windows(2)
        .any(|w| w[0] == "--log-level" && w[1] == "quiet")
}

/// Returns once the tunnel is listening, the job has failed, or the deadline
/// passes: an `Ok` means the tunnel was ready when looked at, not that the
/// request was sent.
pub async fn start_tunnel(account: Option<&str>, name: &str) -> Result<Started> {
    let (_, acct) = resolve(account)?;
    // Recorded before anything runs, so the watchdog that looks while the job
    // is still loading finds a start in flight and not a tunnel that died
    lifecycle::record_user(&acct.name, name, State::Starting);
    match start_inner(account, name).await {
        Ok(started) => {
            lifecycle::record_user(&acct.name, name, State::Up);
            Ok(started)
        }
        Err(err) => {
            // The one edge here that is not the user's: they asked for a
            // start and it did not happen, which is worth a notification
            // when it was the TUI that asked and nobody is reading stderr
            let edge = lifecycle::transition(&acct.name, name, State::Failed, Cause::Observed);
            lifecycle::announce(edge.as_slice());
            Err(err)
        }
    }
}

async fn start_inner(account: Option<&str>, name: &str) -> Result<Started> {
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
    // A tunnel that started is started whether or not its log took the line
    let _ = daemon::log_event(
        &spec.log_path,
        &format!("starting {name} on {}", tunnel.display_url()),
    );
    // A file left by the previous run would read as ready before this one
    // has listened. The supervisor clears it too, but not before launchd has
    // started it, and this is the one place that is certain to run first.
    let ready_file = ready_file_of(&spec);
    if let Some(path) = &ready_file {
        let _ = std::fs::remove_file(path);
    }
    daemon::start(&spec.label).await?;

    let deadline = start_deadline();
    let readiness = match (&ready_file, &tunnel.spec) {
        (Some(path), TunnelSpec::Access { local_port, .. }) => {
            let port = (is_quiet(&spec) && *local_port != 0).then_some(*local_port);
            wait_ready(&spec.label, Some(path), port, deadline).await
        }
        _ => {
            tokio::time::sleep(EXPOSE_SETTLE).await;
            if daemon::status(&spec.label).await == TunnelStatus::Error {
                Readiness::Failed
            } else {
                Readiness::Ready
            }
        }
    };
    if readiness != Readiness::Ready {
        let hint = match (last_hint(&tunnel)?, readiness) {
            (Some(hint), _) => Some(hint),
            (None, Readiness::TimedOut) => Some(format!(
                "not listening after {}s",
                deadline.as_secs().max(1)
            )),
            (None, _) => None,
        };
        return Err(StartFailed {
            name: name.to_string(),
            hint,
        }
        .into());
    }
    Ok(Started {
        name: name.to_string(),
        url: tunnel.display_url(),
    })
}

fn print_started(started: &Started) {
    println!(
        "{} {}  {}",
        TunnelStatus::Running.symbol(),
        started.name,
        started.url
    );
}

pub async fn cmd_start(account: Option<&str>, name: &str) -> Result<()> {
    print_started(&start_tunnel(account, name).await.map_err(print_failed)?);
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

pub async fn stop_tunnel(account: Option<&str>, name: &str) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    // Before the stop, not after: a pass that looks in between would see a
    // running tunnel go away that nobody had said was meant to
    lifecycle::record_user(&acct.name, name, State::Stopped);
    daemon::stop(&daemon::platform_label(&acct.name, &tunnel.name)).await?;
    if let Ok(log) = tunnel.log_path() {
        let _ = daemon::log_event(&log, "stopped by gtunnel");
    }
    Ok(())
}

pub async fn cmd_stop(account: Option<&str>, name: &str) -> Result<()> {
    stop_tunnel(account, name).await?;
    println!("{} {name} stopped", TunnelStatus::Stopped.symbol());
    Ok(())
}

// What `gtunnel autostart` should do: nothing when it was only asked, and
// nothing when the record already says what was asked for
fn auto_start_change(current: bool, wanted: Option<bool>) -> Option<bool> {
    wanted.filter(|w| *w != current)
}

// The record is the side the user edits, so it changes first and the service
// manager is brought to it
fn set_record_auto_start(account: &str, name: &str, on: bool) -> Result<PersistentTunnel> {
    TunnelState::update(|state| {
        let tunnel = state
            .find_for_account_mut(name, account)
            .with_context(|| format!("Tunnel '{name}' not found"))?;
        tunnel.auto_start = on;
        Ok(tunnel.clone())
    })
}

pub async fn cmd_auto_start(account: Option<&str>, name: &str, wanted: Option<bool>) -> Result<()> {
    let (config, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;

    let Some(on) = auto_start_change(tunnel.auto_start, wanted) else {
        println!(
            "{name} auto-start is {}",
            if tunnel.auto_start { "on" } else { "off" }
        );
        return Ok(());
    };

    let updated = set_record_auto_start(&acct.name, name, on)?;
    let spec = daemon::service_spec(&config, &updated)?;
    daemon::set_auto_start(&spec).await?;
    println!("{name} auto-start is {}", if on { "on" } else { "off" });
    Ok(())
}

pub async fn restart_tunnel(account: Option<&str>, name: &str) -> Result<Started> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    lifecycle::record_user(&acct.name, name, State::Stopped);
    let _ = daemon::stop(&daemon::platform_label(&acct.name, &tunnel.name)).await;
    start_tunnel(account, name).await
}

pub async fn cmd_restart(account: Option<&str>, name: &str) -> Result<()> {
    print_started(&restart_tunnel(account, name).await.map_err(print_failed)?);
    Ok(())
}

pub async fn delete_tunnel(account: Option<&str>, name: &str) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
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
    // Gone from the ledger first: the stop below is the user's, and a row left
    // behind would announce the deleted tunnel as a crash
    lifecycle::forget(&acct.name, name);
    // A stop that failed for another reason is tolerated, as before, but one that
    // timed out leaves the job possibly running: removing the record would
    // orphan it
    if let Err(e) = daemon::stop(&label).await
        && crate::proc::is_timeout(&e)
    {
        return Err(e.context(format!(
            "Could not stop {name}; it was not deleted, and its job may still be running"
        )));
    }
    daemon::uninstall(&label).await?;
    // The log goes with the tunnel: nothing is left to own it
    daemon::remove_log(&log_path)?;
    // Only the record is removed under the lock; the daemon work is done
    TunnelState::update(|state| {
        state.remove_for_account(name, &acct.name);
        Ok(())
    })
}

pub async fn cmd_delete(account: Option<&str>, name: &str) -> Result<()> {
    delete_tunnel(account, name).await?;
    println!("Deleted {name}");
    Ok(())
}

pub async fn cmd_logs(account: Option<&str>, name: &str, follow: bool, lines: usize) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnel = find_tunnel(&state, name, &acct.name)?;
    let path = tunnel.log_path()?;

    // The end of the file, not all of it: these logs are talkative and the
    // lines asked for are at the end
    let text = daemon::read_tail(&path, daemon::TAIL_WINDOW);
    let tail: Vec<&str> = text.lines().rev().take(lines).collect();
    for line in tail.into_iter().rev() {
        println!("{line}");
    }
    if !follow {
        return Ok(());
    }

    // Poll the file's length rather than holding a handle, so a daemon that
    // rewrites the log is still followed
    let mut offset = std::fs::metadata(&path).map_or(0, |m| m.len());
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
        // Only what arrived since the last look, so following a large log
        // does not re-read it on every tick
        let fresh = daemon::read_from(&path, offset);
        print!("{fresh}");
        use std::io::Write;
        std::io::stdout().flush().ok();
        offset = meta.len();
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ListRowJson {
    pub status: String,
    pub name: String,
    pub mode: String,
    pub url: String,
    pub target: String,
    pub account: String,
    pub auto_start: bool,
    pub unmanaged: bool,
    /// A `gtunnel run`, which ends by itself. False on a declared tunnel, so an
    /// agent reads one schema for every row; `mode` still says what it is.
    pub run: bool,
    /// Unix seconds at which the run's TTL ends it; absent without a TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

pub async fn cmd_list(account: Option<&str>, json: bool) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let state = TunnelState::load()?;
    let tunnels = state.tunnels_for_account(&acct.name);
    let external = external_rows().await;
    let runs = crate::ephemeral::live_runs(&acct.name).unwrap_or_default();

    if json {
        let mut json_rows = Vec::new();
        for tunnel in tunnels {
            let status = tunnel_status(tunnel).await;
            json_rows.push(ListRowJson {
                status: status_word(status).to_string(),
                name: tunnel.name.clone(),
                mode: tunnel.display_kind().to_string(),
                url: tunnel.display_url(),
                target: tunnel.display_target(),
                account: tunnel.account_name.clone(),
                auto_start: tunnel.auto_start,
                unmanaged: false,
                run: false,
                expires_at: None,
            });
        }
        for run in &runs {
            let t = run.as_tunnel();
            json_rows.push(ListRowJson {
                status: "running".to_string(),
                name: t.name.clone(),
                mode: t.display_kind().to_string(),
                url: t.display_url(),
                target: t.display_target(),
                account: t.account_name.clone(),
                auto_start: false,
                unmanaged: false,
                run: true,
                expires_at: run.expires_at(),
            });
        }
        for ext in external {
            json_rows.push(ListRowJson {
                status: status_word(ext.status).to_string(),
                name: ext.name,
                mode: ext.mode.to_string(),
                url: ext.url,
                target: ext.target,
                account: acct.name.clone(),
                auto_start: false,
                unmanaged: true,
                run: false,
                expires_at: None,
            });
        }
        println!("{}", serde_json::to_string_pretty(&json_rows)?);
        return Ok(());
    }

    if tunnels.is_empty() && runs.is_empty() {
        println!("No tunnels for account '{}'", acct.name);
        return Ok(());
    }
    // Every row is collected first: the columns cannot be sized until the
    // unmanaged tunnels below have been discovered too.
    let mut rows = Vec::new();
    for tunnel in tunnels {
        let status = tunnel_status(tunnel).await;
        rows.push(ListRow::from_tunnel(status, tunnel));
    }
    rows.extend(runs.iter().map(ListRow::from_run));
    rows.extend(external);

    let layout = ListLayout::fit(&rows);
    println!("{}", layout.header());
    for row in &rows {
        println!("{}", row.render(&layout));
    }
    Ok(())
}

// IAP tunnels started outside gtunnel hold ports and confuse `doctor`, so the
// list names them instead of pretending they are not there
async fn external_rows() -> Vec<ListRow> {
    let Ok(state) = crate::state::TunnelState::load() else {
        return Vec::new();
    };
    crate::discovery::classify(&crate::discovery::snapshot(), &state)
        .await
        .into_iter()
        .map(|external| ListRow {
            status: TunnelStatus::Running,
            name: format!("pid {}", external.pid),
            mode: "access",
            url: match external.local_port {
                Some(port) => format!("localhost:{port}"),
                None => "-".to_string(),
            },
            target: match &external.zone {
                Some(zone) => format!("{}:{} ({zone})", external.target, external.remote_port),
                None => format!("{}:{}", external.target, external.remote_port),
            },
            note: Some("  [not managed by gtunnel]".to_string()),
        })
        .collect()
}

pub async fn cmd_zones(account: Option<&str>, default: Option<&str>) -> Result<()> {
    let (_, acct) = resolve(account)?;
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
    crate::config::update_config(|config| {
        config.get_account_mut(Some(&acct.name))?.default_zone = zone.to_string();
        Ok(())
    })?;
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

    #[tokio::test]
    async fn wait_ready_returns_as_soon_as_the_ready_file_appears() {
        let dir = std::env::temp_dir().join(format!("gtunnel-ready-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.ready");
        let _ = std::fs::remove_file(&file);
        let writer = file.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::fs::write(writer, "").unwrap();
        });
        let began = std::time::Instant::now();
        let got = wait_ready(
            "com.gtunnel.test.ready-file",
            Some(&file),
            None,
            Duration::from_secs(10),
        )
        .await;
        let took = began.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, Readiness::Ready);
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert!(took < Duration::from_secs(1), "{took:?}");
    }

    #[tokio::test]
    async fn wait_ready_accepts_a_listening_port_when_no_file_will_come() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let got = wait_ready(
            "com.gtunnel.test.port",
            None,
            Some(port),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(got, Readiness::Ready);
    }

    #[tokio::test]
    async fn wait_ready_fails_at_the_deadline_with_the_hint() {
        // The deadline an operator would set, read the way `start` reads it
        let deadline = deadline_from(Some("200"));
        assert_eq!(deadline, Duration::from_millis(200));
        let dir = std::env::temp_dir().join(format!("gtunnel-never-{}", std::process::id()));
        let began = std::time::Instant::now();
        let got = wait_ready(
            "com.gtunnel.test.never",
            Some(&dir.join("never.ready")),
            None,
            deadline,
        )
        .await;
        assert_eq!(got, Readiness::TimedOut);
        assert!(began.elapsed() < Duration::from_secs(2));
        // `start_tunnel` turns that into the hint the log gives, or this one
        let hint = format!("not listening after {}s", deadline.as_secs().max(1));
        let failed = StartFailed {
            name: "db".into(),
            hint: Some(hint),
        };
        assert!(failed.to_string().contains("not listening after 1s"));
        assert!(failed.to_string().contains("Logs: gtunnel logs db"));
    }

    #[test]
    fn the_start_deadline_defaults_to_twenty_seconds() {
        assert_eq!(deadline_from(None), Duration::from_secs(20));
        assert_eq!(deadline_from(Some("soon")), Duration::from_secs(20));
    }

    #[test]
    fn a_failed_start_renders_the_hint_and_the_logs_line() {
        let e = StartFailed {
            name: "db".into(),
            hint: Some("IAP refused the connection".into()),
        };
        assert_eq!(
            e.to_string(),
            "db failed to start\n  IAP refused the connection\n  Logs: gtunnel logs db"
        );
    }

    #[test]
    fn a_failed_start_without_a_hint_has_two_lines() {
        let e = StartFailed {
            name: "db".into(),
            hint: None,
        };
        assert_eq!(e.to_string(), "db failed to start\n  Logs: gtunnel logs db");
    }

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

    // `gtunnel autostart db` with no flag asks a question. It must not answer it
    // by changing anything, least of all by loading a job into launchd.
    #[test]
    fn asking_without_a_flag_changes_nothing() {
        assert_eq!(auto_start_change(true, None), None);
        assert_eq!(auto_start_change(false, None), None);
        assert_eq!(auto_start_change(false, Some(false)), None);
        assert_eq!(auto_start_change(true, Some(true)), None);
        assert_eq!(auto_start_change(false, Some(true)), Some(true));
        assert_eq!(auto_start_change(true, Some(false)), Some(false));
    }

    #[test]
    fn turning_auto_start_off_clears_the_record() {
        let _temp = crate::config::testing::TempConfigDir::new();
        let mut state = TunnelState::default();
        state.tunnels.push(PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: true,
            spec: TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "postgres-1".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        });
        state.save().expect("save");

        let updated = set_record_auto_start("dev", "db", false).expect("update");
        assert!(!updated.auto_start);
        let reloaded = TunnelState::load().expect("load");
        assert!(
            !reloaded
                .find_for_account("db", "dev")
                .expect("tunnel")
                .auto_start,
            "the record on disk is what the next doctor run reads"
        );
    }
    #[tokio::test]
    async fn verify_instance_missing_suggests_zones() {
        use crate::gcp::{Endpoints, GcpClient, TokenSource};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/compute/v1/projects/proj-1/zones/europe-west1-b/instances/missing-vm",
            ))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = GcpClient::new(
            TokenSource::Static("tok".to_string()),
            Endpoints::single(&server.uri()),
        );
        let acct = Account {
            name: "dev".to_string(),
            gcloud_configuration: "dev".to_string(),
            project: "proj-1".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            expose: None,
        };
        let err = verify_instance_with_client(&client, &acct, "europe-west1-b", "missing-vm")
            .await
            .expect_err("should bail");
        let msg = format!("{err:#}");
        assert!(msg.contains(
            "Instance 'missing-vm' not found in zone 'europe-west1-b' of project 'proj-1'"
        ));
        assert!(msg.contains("Run `gtunnel zones` to check available zones"));
    }
}
