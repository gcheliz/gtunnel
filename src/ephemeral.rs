use crate::error::GtunnelError;
use anyhow::{bail, Context, Result};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config::config_dir;
use crate::expose::routes;
use crate::gcp::{gcloud, GcpClient};
use crate::lifecycle::now_secs;
use crate::state::{
    allocate_port, check_local_port, local_port_is_free, AccessTarget, PersistentTunnel,
    TunnelSpec, TunnelState, ACCESS_PORT_BASE, ACCESS_PORT_SPAN, RELAY_PORT_BASE, RELAY_PORT_SPAN,
};
use crate::tunnel::{access_argv, diagnose_log_line, expose_argv};

// A tunnel that lives only as long as the foreground process that started it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EphemeralRecord {
    pub name: String,
    pub account_name: String,
    pub pid: u32,
    pub spec: TunnelSpec,
    pub started_at: u64,
    /// Seconds the run was told to live. Absent for a run with no `--ttl`, and
    /// for every record an older version wrote, so those still parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    /// Who `pid` is: the start token `crate::proc::start_token` read just after
    /// the spawn. A pid alone is a number the kernel hands out again; pid and
    /// start together name one process. Absent in records 0.1.42 and older
    /// wrote, which are judged by the pid alone as they always were. Present
    /// means the pid was spawned with `process_group(0)` and leads its group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<String>,
    /// The `gtunnel run` process that owns this record: the one that tears the
    /// route down and removes the file. `pid` is gcloud, its child, which dies
    /// first and so says nothing about whether that teardown has finished.
    /// Absent in records an older version wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    /// The owner's start token, with the same meaning as `pid_start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_start: Option<String>,
}

pub fn run_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("run");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir)
}

impl EphemeralRecord {
    /// The run as a tunnel, so the TUI, `list` and the MCP server render it
    /// with the same code as a declared one instead of each rebuilding it.
    pub fn as_tunnel(&self) -> PersistentTunnel {
        PersistentTunnel {
            name: self.name.clone(),
            account_name: self.account_name.clone(),
            auto_start: false,
            spec: self.spec.clone(),
        }
    }

    /// Unix seconds at which the TTL ends the run; None when it has no TTL.
    pub fn expires_at(&self) -> Option<u64> {
        self.ttl_secs.map(|ttl| self.started_at + ttl)
    }

    pub fn path(&self) -> Result<PathBuf> {
        record_path(&self.account_name, &self.name)
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path()?;
        let json = serde_json::to_string_pretty(self).context("Failed to encode the run record")?;
        crate::fsutil::write_atomic(&path, &json)
    }

    /// Whether this run's process is still the one that was recorded. One we
    /// cannot identify is not alive here, because the callers signal what this
    /// says is alive; those that wait for an exit ask `liveness` and keep
    /// waiting on `Unknown`.
    pub fn is_alive(&self) -> bool {
        self.liveness() == Liveness::Alive
    }

    /// The recorded process as `Liveness`: gone only when the system says so.
    pub fn liveness(&self) -> Liveness {
        liveness_of(self.pid, self.pid_start.as_deref())
    }

    /// Whether the `gtunnel run` that owns this record is still there. None for
    /// a record that names no owner, which no one can call orphaned on this
    /// evidence. An owner that could not be looked up reads as there: false
    /// means the system said it is gone, since the answer decides whether its
    /// run is reaped and whether `reset` deletes the directory it writes into.
    pub fn owner_alive(&self) -> Option<bool> {
        let pid = self.owner_pid?;
        Some(liveness_of(pid, self.owner_start.as_deref()) != Liveness::Gone)
    }

    /// Why this run should be stopped by someone other than its owner, or None
    /// when it is the owner's to end. A dead owner never will; a run past its
    /// TTL with a live owner gets `EXPIRY_GRACE` first, because the owner's own
    /// one-second tick is about to end it and two teardowns would race.
    pub fn orphan_reason(&self, now: u64) -> Option<&'static str> {
        let owner = self.owner_alive();
        if owner == Some(false) {
            return Some("its process is gone");
        }
        let grace = if owner == Some(true) { EXPIRY_GRACE } else { 0 };
        match self.expires_at() {
            Some(at) if now >= at + grace => Some("it is past its TTL"),
            _ => None,
        }
    }

    /// Wait until an imported run has let go of everything: gcloud first, then
    /// the process that tears its route down. The owner is the one to wait for,
    /// because gcloud dies first and the owner goes on to remove the backend,
    /// host rule and DNS record. A record that names no owner cannot say when
    /// that is done, so it gets `settle` after gcloud exits instead.
    pub async fn wait_for_handover(
        &self,
        budget: std::time::Duration,
        settle: std::time::Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + budget;
        if !self.wait_for_exit(budget).await {
            return false;
        }
        if self.owner_pid.is_none() {
            tokio::time::sleep(settle).await;
            return true;
        }
        loop {
            if self.owner_alive() == Some(false) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Ask the run to stop. A record with a start token leads its own process
    /// group (it was spawned with `process_group(0)`), so the group is signalled
    /// and gcloud's children hear it too. An older record is no group leader, so
    /// signalling `-pid` would hit nothing or someone else's group: the pid
    /// alone is signalled, as it always was. False, with nothing sent, when the
    /// run is not ours any more.
    pub fn terminate(&self) -> bool {
        if !self.is_alive() {
            return false;
        }
        let Some(pid) = signalable(self.pid) else {
            return false;
        };
        if self.pid_start.is_some() {
            crate::proc::signal_group(pid, libc::SIGTERM);
        } else {
            // SAFETY: kill(2) takes two integers and no pointer. `signalable`
            // rejected 0 and negative values, so this names one process and
            // never a group. A pid that has exited since is a harmless ESRCH.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        true
    }

    /// Poll until the run is gone, up to `budget`. True means it exited; false
    /// means it outlived the budget and the caller decides what that is worth.
    pub async fn wait_for_exit(&self, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if self.liveness() == Liveness::Gone {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Never creates the run directory, unlike `path`: this is the last thing
    /// a run does, and `reset` may have deleted the directory by then. A
    /// directory made here outlives the reset that was meant to remove it.
    pub fn remove(&self) -> Result<()> {
        let path = config_dir()?.join("run").join(format!(
            "{}.json",
            crate::state::label_of(&self.account_name, &self.name)
        ));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("Failed to remove {}", path.display())),
        }
    }
}

/// What the system says about a recorded process, judged with its start token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The recorded process, running.
    Alive,
    /// Not there, exited and waiting to be reaped, or a different process under
    /// a recycled pid.
    Gone,
    /// The lookup failed, so nothing is known.
    Unknown,
}

fn liveness_of(pid: u32, start: Option<&str>) -> Liveness {
    if !pid_is_alive(pid) {
        return Liveness::Gone;
    }
    judge(crate::proc::presence(pid), start)
}

fn judge(found: crate::proc::Presence, start: Option<&str>) -> Liveness {
    use crate::proc::Presence;
    match (found, start) {
        (Presence::Gone, _) => Liveness::Gone,
        (Presence::Unknown, _) => Liveness::Unknown,
        (Presence::Alive(now), Some(then)) if now != then => Liveness::Gone,
        (Presence::Alive(_), _) => Liveness::Alive,
    }
}

/// How long a run past its TTL keeps its live owner's right to end it itself.
const EXPIRY_GRACE: u64 = 30;

/// The only way a recorded pid becomes an argument to `kill(2)`. A pid is a
/// number read back from a file, and kill gives three of its values a meaning
/// that is not "this process": 0 signals our own process group, a negative
/// value signals a group, and -1 signals everything we may signal. A record
/// that holds 0 (a spawn that lost its child id) or a value past `i32::MAX`
/// (which wraps negative) must therefore read as "no process", never as a
/// broadcast.
pub fn signalable(pid: u32) -> Option<libc::pid_t> {
    i32::try_from(pid).ok().filter(|p| *p > 0)
}

// Signal 0 asks the kernel whether the process exists without disturbing it.
// A bare pid can be a recycled one, so a run record is judged by its start
// token as well; this alone serves only callers with nothing more than a pid,
// like the config page's `tunnels.edit-<pid>.toml` copies, where a wrong
// "alive" merely keeps a stale copy
pub fn pid_is_alive(pid: u32) -> bool {
    let Some(pid) = signalable(pid) else {
        return false;
    };
    // SAFETY: kill(2) with signal 0 takes two integers and no pointer, and sends
    // nothing: it only reports whether the pid exists and may be signalled, so
    // there is no memory or process state to corrupt. `signalable` guarantees
    // the pid is positive, so it names one process and never a group.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A file in the run directory that no record could be read from: written by
/// a gtunnel whose records this one cannot parse, or damaged. It is not a run
/// this version can list, stop or reap, but the process behind it may be
/// alive and holding its ports, so the ports it names are still counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableRecord {
    pub file: String,
    pub reason: String,
    pub account: Option<String>,
    pub local_port: Option<u16>,
    pub relay_port: Option<u16>,
}

// The ports are fished out of the JSON by name, without the struct: the shape
// that failed to parse is the one thing that cannot be relied on, but a port
// number under `spec` is what every version has called it
fn unreadable(file: String, reason: String, text: &str) -> UnreadableRecord {
    let value: Option<serde_json::Value> = serde_json::from_str(text).ok();
    let port = |key: &str| {
        value
            .as_ref()
            .and_then(|v| v.get("spec")?.get(key)?.as_u64())
            .and_then(|p| u16::try_from(p).ok())
    };
    UnreadableRecord {
        file,
        reason,
        account: value
            .as_ref()
            .and_then(|v| v.get("account_name")?.as_str().map(str::to_string)),
        local_port: port("local_port"),
        relay_port: port("relay_port"),
    }
}

type Scan = (Vec<(EphemeralRecord, bool)>, Vec<UnreadableRecord>);

fn scan_runs() -> Result<Scan> {
    let dir = run_dir()?;
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                skipped.push(unreadable(file, e.to_string(), ""));
                continue;
            }
        };
        match serde_json::from_str::<EphemeralRecord>(&text) {
            Ok(record) => {
                let alive = record.is_alive();
                out.push((record, alive));
            }
            Err(e) => skipped.push(unreadable(file, e.to_string(), &text)),
        }
    }
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    skipped.sort_by(|a, b| a.file.cmp(&b.file));
    Ok((out, skipped))
}

pub fn list_ephemeral() -> Result<Vec<(EphemeralRecord, bool)>> {
    Ok(scan_runs()?.0)
}

/// What `list` and `doctor` say about the files `list_ephemeral` cannot read,
/// so a run that vanished from the list is never a silent one.
pub fn unreadable_records() -> Result<Vec<UnreadableRecord>> {
    Ok(scan_runs()?.1)
}

/// The longest a run may be told to live. A TTL is a safety net for a tunnel
/// nobody is watching, so an unbounded one defeats its purpose; a day is
/// already longer than any session this is meant for.
pub const MAX_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// `90s`, `30m` or `2h`; a bare integer is minutes, because the MCP tool and
/// the people asking for "an hour or so" both think in minutes. Zero is
/// refused: a run that ends at once is a mistake, not a request.
pub fn parse_ttl(text: &str) -> Result<std::time::Duration> {
    let text = text.trim();
    let (digits, unit) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1u64),
        Some('m') => (&text[..text.len() - 1], 60),
        Some('h') => (&text[..text.len() - 1], 3600),
        _ => (text, 60),
    };
    let invalid = || format!("Invalid TTL '{text}': use 90s, 30m or 2h (a bare number is minutes)");
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(GtunnelError::usage(invalid()).into());
    }
    let amount: u64 = digits.parse().with_context(invalid)?;
    let secs = amount
        .checked_mul(unit)
        .with_context(|| format!("TTL '{text}' is too large"))?;
    if secs == 0 {
        bail!("TTL must be longer than zero");
    }
    let ttl = std::time::Duration::from_secs(secs);
    if ttl > MAX_TTL {
        bail!("TTL '{text}' is longer than the 24h maximum");
    }
    Ok(ttl)
}

/// `expires_at` is whole seconds, so a one-second tick ends a run at most a
/// second late after any suspend, for one clock read a second.
const TTL_TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// Polls the wall clock instead of sleeping the whole TTL. `tokio::time::sleep`
/// runs on the monotonic clock, which does not advance while the machine is
/// suspended on macOS or Linux: a laptop closed for an hour would still owe the
/// full TTL on wake, while `expires_at`, which the TUI shows, had passed. With
/// no TTL the future never completes, so the select treats it like any other
/// run that waits for a signal.
async fn ttl_elapsed(expires_at: Option<u64>, tick: std::time::Duration, now: impl Fn() -> u64) {
    let Some(at) = expires_at else {
        return std::future::pending().await;
    };
    loop {
        if now() >= at {
            return;
        }
        tokio::time::sleep(tick).await;
    }
}

/// The runs of one account whose process is still there. A dead record is a
/// run that is gone but may still own a route, which is `doctor`'s business and
/// not something to list as a tunnel you can connect to.
pub fn live_runs(account: &str) -> Result<Vec<EphemeralRecord>> {
    Ok(list_ephemeral()?
        .into_iter()
        .filter(|(record, alive)| *alive && record.account_name == account)
        .map(|(record, _)| record)
        .collect())
}

/// The refusal `add`, `expose`, `rename` and `move` give for a name a live run
/// holds: `check_run_name` stops a run taking a saved name, and this is the
/// other direction. Two rows of one name share a log, and the TUI's `d` and
/// `A` look a row up by name, so the saved twin would take the run's keys.
/// It reads the run directory, so it sits in the commands and not in
/// `TunnelState::add`, which stays a pure function of the state.
pub fn check_name_free_of_runs(account: &str, name: &str) -> Result<()> {
    if live_runs(account)?.iter().any(|r| r.name == name) {
        return Err(GtunnelError::usage(format!(
            "A run named '{name}' is live for account '{account}'; stop it or choose another name"
        ))
        .into());
    }
    Ok(())
}

const NAME_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

// An unnamed `run` still needs a name for the log line and the record
pub fn generated_name() -> String {
    let mut rng = rand::rng();
    let suffix: String = (0..6)
        .map(|_| NAME_ALPHABET[rng.random_range(0..NAME_ALPHABET.len())] as char)
        .collect();
    format!("gt-{suffix}")
}

// Every record counts, alive or not: a record is removed only once its relay
// route is gone, so a dead one's port must not be handed to a new tunnel that
// would then share the old hostname's named port
// Relay ports are bound on the relay instance, so nothing local can probe them:
// state and the live runs are the only record, and `expose` has to read both
pub fn used_relay_ports(
    state: &TunnelState,
    account: &str,
) -> Result<std::collections::HashSet<u16>> {
    let mut used = state.used_relay_ports(account);
    used.extend(run_relay_ports(account)?);
    Ok(used)
}

/// Relay ports live on one account's relay instance, so another account's runs do not
/// compete for them.
fn run_relay_ports(account: &str) -> Result<std::collections::HashSet<u16>> {
    let mut used = std::collections::HashSet::new();
    let (records, skipped) = scan_runs()?;
    for (record, _alive) in records {
        if record.account_name != account {
            continue;
        }
        if let TunnelSpec::Expose { relay_port, .. } = &record.spec {
            used.insert(*relay_port);
        }
    }
    // A record that names no account may be this one's, so it counts
    used.extend(
        skipped
            .iter()
            .filter(|r| r.account.as_deref().is_none_or(|a| a == account))
            .filter_map(|r| r.relay_port),
    );
    Ok(used)
}

/// Local ports are shared by the whole machine, whichever account's run holds
/// them, so every record counts. A dead record still counts: its port is
/// released only when the record goes.
fn run_local_ports() -> Result<std::collections::HashSet<u16>> {
    let mut used = std::collections::HashSet::new();
    let (records, skipped) = scan_runs()?;
    for (record, _alive) in records {
        if let TunnelSpec::Access { local_port, .. } = &record.spec {
            used.insert(*local_port);
        }
    }
    used.extend(skipped.iter().filter_map(|r| r.local_port));
    Ok(used)
}

/// The local ports held by runs whose process is still there, with the run's
/// name for the message. `add` reads this and not `run_local_ports`: a dead
/// record holds nothing a bind probe cannot see, and refusing a port for a
/// crashed run's leftover would name a run that is not running.
pub fn live_run_local_ports() -> Result<std::collections::HashMap<u16, String>> {
    let mut used = std::collections::HashMap::new();
    for (record, alive) in list_ephemeral()? {
        if let (true, TunnelSpec::Access { local_port, .. }) = (alive, &record.spec) {
            used.insert(*local_port, record.name.clone());
        }
    }
    Ok(used)
}

/// The record file of a run. The name is built from `state::label_of`, the same
/// join the log and service names use, so a fix can never delete a path the
/// writer did not use.
pub fn record_path(account: &str, name: &str) -> Result<PathBuf> {
    Ok(run_dir()?.join(format!("{}.json", crate::state::label_of(account, name))))
}

/// The refusal both `gtunnel run` and the MCP launch give. A second run of a
/// name would overwrite the first one's record file, leaving a process nothing
/// can see or stop, and the first to exit would delete the other's record; a
/// managed tunnel of that name would share its log and its public hostname.
pub fn check_run_name(state: &TunnelState, account: &str, name: &str) -> Result<()> {
    let mut live = false;
    let label = crate::state::label_of(account, name);
    for (r, alive) in list_ephemeral()? {
        if !alive {
            continue;
        }
        if r.account_name == account && r.name == name {
            live = true;
        } else if r.account_name != account
            && crate::state::label_of(&r.account_name, &r.name) == label
        {
            // Two accounts whose joined strings meet write one record and one log
            return Err(GtunnelError::usage(format!(
                "A run named '{name}' on account '{account}' would share its record and log \
                 with run '{}' on account '{}' (both are '{label}'); choose another name",
                r.name, r.account_name
            ))
            .into());
        }
    }
    if live {
        return Err(GtunnelError::usage(format!(
            "A run named '{name}' is already live for account '{account}'"
        ))
        .into());
    }
    if state.find_for_account(name, account).is_some() {
        return Err(GtunnelError::usage(format!(
            "Tunnel '{name}' already exists on account '{account}'"
        ))
        .into());
    }
    Ok(())
}

/// Ctrl+C, SIGTERM and SIGHUP all have to reach teardown: a route left behind
/// keeps resolving for everyone. The streams are registered once, before
/// gcloud starts, and kept for the whole run: a signal that arrives while the
/// route is being created has to take the same exit as one that arrives later,
/// and until a handler exists the default action ends gtunnel and leaves gcloud,
/// which is in its own process group, running.
struct StopSignals {
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}

impl StopSignals {
    fn register() -> Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        let stream = |kind| signal(kind).context("Failed to listen for stop signals");
        Ok(Self {
            int: stream(SignalKind::interrupt())?,
            term: stream(SignalKind::terminate())?,
            hup: stream(SignalKind::hangup())?,
        })
    }

    async fn next(&mut self) {
        tokio::select! {
            _ = self.int.recv() => {}
            _ = self.term.recv() => {}
            _ = self.hup.recv() => {}
        }
    }
}

/// Run `work` unless a stop signal comes first. None means the signal won and
/// `work` was dropped half done, which the caller undoes with its teardown.
async fn unless_stopped<T>(
    signals: &mut StopSignals,
    work: impl std::future::Future<Output = T>,
) -> Option<T> {
    // Work first: a result that is already there is never thrown away for a
    // signal that arrived in the same instant
    tokio::select! {
        biased;
        out = work => Some(out),
        _ = signals.next() => None,
    }
}

/// The exit status of a run that stopped Ctrl+C-style twice: 128 plus SIGINT,
/// what a shell reports for a process the user interrupted.
const ABANDONED_EXIT: i32 = 130;

/// Run the route teardown unless a stop signal comes first. The first signal
/// started the teardown and was read by whoever raced it; one that arrives now
/// is the person asking again, and the teardown can stall for as long as the
/// GCP operations take, up to the operation timeout. Abandoning leaves the
/// record in place, which is what lets the sweep find the route and remove it.
async fn finish_teardown(
    console: &Console,
    signals: &mut StopSignals,
    url: &str,
    work: impl std::future::Future<Output = ()>,
) -> Result<()> {
    if unless_stopped(signals, work).await.is_some() {
        return Ok(());
    }
    console.warn(format_args!(
        "Route for {url} left in place; `gtunnel doctor --fix` or the watchdog sweep removes it"
    ));
    Err(crate::error::Exited(ABANDONED_EXIT).into())
}

/// The status a run exits with when gcloud ended it. gcloud's own code says
/// what failed; a signal death has none, and an exit 0 still means the tunnel
/// is gone, so a script chained with `&&` must not go on.
fn gcloud_exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().filter(|code| *code != 0).unwrap_or(1)
}

/// Print gcloud's output a line at a time until both streams are closed. Bytes
/// are read raw and decoded lossily, as `logfilter::pump` does: a Latin-1 ssh
/// banner is a line to show, and a read that errs on one stream closes that
/// stream and not the run. The caller treats the end of this as nothing: only
/// gcloud exiting, a signal or the TTL ends a run.
async fn pump_lines<A, B>(stdout: Option<A>, stderr: Option<B>, mut on_line: impl FnMut(String))
where
    A: tokio::io::AsyncRead + Unpin,
    B: tokio::io::AsyncRead + Unpin,
{
    let mut out = stdout.map(BufReader::new);
    let mut err = stderr.map(BufReader::new);
    // Kept across loop turns: a read cancelled by the other stream winning the
    // select has already moved its bytes here, and the next call appends
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    loop {
        let (n, is_out) = match (&mut out, &mut err) {
            (Some(o), Some(e)) => tokio::select! {
                n = o.read_until(b'\n', &mut out_buf) => (n, true),
                n = e.read_until(b'\n', &mut err_buf) => (n, false),
            },
            (Some(o), None) => (o.read_until(b'\n', &mut out_buf).await, true),
            (None, Some(e)) => (e.read_until(b'\n', &mut err_buf).await, false),
            (None, None) => return,
        };
        if !matches!(n, Ok(n) if n > 0) {
            // EOF or an error: this stream has nothing more to say
            if is_out {
                out = None;
            } else {
                err = None;
            }
        }
        let buf = if is_out { &mut out_buf } else { &mut err_buf };
        if !buf.is_empty() {
            let text = String::from_utf8_lossy(buf);
            on_line(text.trim_end_matches(['\n', '\r']).to_string());
            buf.clear();
        }
    }
}

/// What the run prints once it is up. A reader that went away (`| head -1`
/// closes the pipe, a closed terminal answers EIO) must cost the run its
/// output and nothing else: `println!` panics on a failed write, and the panic
/// hook turns that into a silent exit that leaves gcloud forwarding with no one
/// to enforce the TTL. A failed write clears the flag and the run carries on
/// quietly until a signal or the TTL ends it.
struct Console {
    out: std::cell::Cell<bool>,
    err: std::cell::Cell<bool>,
}

impl Console {
    fn new() -> Self {
        Self {
            out: std::cell::Cell::new(true),
            err: std::cell::Cell::new(true),
        }
    }

    fn say(&self, line: std::fmt::Arguments) {
        use std::io::Write;
        if self.out.get() && writeln!(std::io::stdout().lock(), "{line}").is_err() {
            self.out.set(false);
        }
    }

    fn warn(&self, line: std::fmt::Arguments) {
        use std::io::Write;
        if self.err.get() && writeln!(std::io::stderr().lock(), "{line}").is_err() {
            self.err.set(false);
        }
    }
}

/// Holds gcloud for the life of the run so an unwind still stops it. Release
/// builds abort on panic and never run this, so it backs up the signal paths
/// and does not replace them. The record is left in place: for a public run it
/// is what lets the next sweep find the route that needs tearing down.
struct ChildGuard {
    child: tokio::process::Child,
    pgid: Option<i32>,
    armed: bool,
}

impl ChildGuard {
    fn new(child: tokio::process::Child, pgid: Option<i32>) -> Self {
        Self {
            child,
            pgid,
            armed: true,
        }
    }

    /// Teardown has run, so there is nothing left for `Drop` to do.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(pgid) = self.pgid {
            crate::proc::signal_group(pgid, libc::SIGTERM);
        }
        let _ = self.child.start_kill();
    }
}

// Spawn gcloud and write the run record; both have to succeed together
fn start_child(
    gcloud_path: &std::path::Path,
    acct: &crate::config::Account,
    expose_config: &Option<crate::config::ExposeConfig>,
    tunnel: &PersistentTunnel,
    spec: TunnelSpec,
    name: &str,
    ttl: Option<std::time::Duration>,
) -> Result<(tokio::process::Child, EphemeralRecord)> {
    let level = crate::loglevel::current();
    let argv = match expose_config {
        Some(expose) => expose_argv(acct, expose, tunnel, level)?,
        None => access_argv(acct, tunnel, level)?,
    };

    let mut command = Command::new(gcloud_path);
    command
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, as the supervisor does: gcloud is a launcher, and a
        // signal to the group is the only way to reach ssh and the IAP proxy
        .process_group(0);
    // If this process dies without a chance to clean up, gcloud hears SIGTERM
    // from the kernel instead of forwarding with no one to enforce the TTL
    #[cfg(target_os = "linux")]
    {
        // SAFETY: the closure runs in the forked child before exec and makes one
        // prctl(2) call that takes integers and no pointer, which is
        // async-signal-safe; nothing in it allocates or takes a lock.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    let child = command
        .spawn()
        .with_context(|| format!("Failed to run {}", gcloud_path.display()))?;

    // No id means the child was already reaped, so there is nothing to record
    // and a recorded 0 would later read as a process group
    let Some(pid) = child.id() else {
        bail!(
            "{} exited before it could be recorded",
            gcloud_path.display()
        );
    };
    let pid_start = crate::proc::start_token(pid);
    // Where a token can be read, none means the process is already gone, and a
    // record without one would be judged by the pid alone: the weaker rule
    if pid_start.is_none() && cfg!(any(target_os = "macos", target_os = "linux")) {
        crate::proc::signal_group(pid as i32, libc::SIGKILL);
        bail!(
            "{} exited before it could be recorded",
            gcloud_path.display()
        );
    }
    let record = EphemeralRecord {
        name: name.to_string(),
        account_name: acct.name.clone(),
        pid,
        spec,
        started_at: now_secs(),
        ttl_secs: ttl.map(|t| t.as_secs()),
        pid_start,
        owner_pid: Some(std::process::id()),
        owner_start: crate::proc::start_token(std::process::id()),
    };
    if let Err(e) = record.save() {
        // Nothing has connected yet, so there is no grace to give
        crate::proc::signal_group(pid as i32, libc::SIGKILL);
        return Err(e);
    }
    Ok((child, record))
}

/// What a run was asked to be, before a port is chosen for it.
enum RunShape {
    Access {
        target: AccessTarget,
        remote_port: u16,
        local_port: Option<u16>,
    },
    Public {
        local_target: String,
        domain: String,
    },
}

/// Decide the name and the ports, spawn gcloud and save the record as one step
/// under `tunnels.lock`. The ports a run takes are read back by the next
/// claimant from its record, so the choice and the save must not be separated
/// by another claimant's read: two launches could otherwise pick one port, and
/// a name could be claimed twice. Nothing in here awaits (decision 16).
fn claim_and_start(
    gcloud_path: &std::path::Path,
    acct: &crate::config::Account,
    expose: &Option<crate::config::ExposeConfig>,
    name: &str,
    shape: RunShape,
    ttl: Option<std::time::Duration>,
) -> Result<(tokio::process::Child, EphemeralRecord, PersistentTunnel)> {
    TunnelState::with_lock(|state| {
        check_run_name(state, &acct.name, name)?;
        let spec = match shape {
            RunShape::Access {
                target,
                remote_port,
                local_port,
            } => {
                let mut used = state.used_local_ports();
                used.extend(run_local_ports()?);
                let local_port = match local_port {
                    Some(port) => {
                        check_local_port(port, &used, local_port_is_free)?;
                        port
                    }
                    None => allocate_port(
                        name,
                        ACCESS_PORT_BASE,
                        ACCESS_PORT_SPAN,
                        &used,
                        local_port_is_free,
                    )?,
                };
                TunnelSpec::Access {
                    target,
                    remote_port,
                    local_port,
                }
            }
            RunShape::Public {
                local_target,
                domain,
            } => {
                let used = used_relay_ports(state, &acct.name)?;
                let relay_port =
                    allocate_port(name, RELAY_PORT_BASE, RELAY_PORT_SPAN, &used, |_| true)?;
                TunnelSpec::Expose {
                    local_target,
                    hostname: format!("{name}.{domain}"),
                    domain,
                    relay_port,
                    private: false,
                }
            }
        };
        let tunnel = PersistentTunnel {
            name: name.to_string(),
            account_name: acct.name.clone(),
            auto_start: false,
            spec: spec.clone(),
        };
        let (child, record) = start_child(gcloud_path, acct, expose, &tunnel, spec, name, ttl)?;
        Ok((child, record, tunnel))
    })
}

/// Stop a started run's gcloud group first, so nothing holds the port while
/// the route and the record are undone.
async fn stop_started(guard: &mut ChildGuard, console: &Console, record: &EphemeralRecord) {
    if let Err(e) = crate::proc::stop_group(
        &mut guard.child,
        signalable(record.pid),
        crate::proc::STOP_GRACE,
    )
    .await
    {
        console.warn(format_args!("Failed to stop gcloud: {e:#}"));
    }
    guard.disarm();
}

#[allow(clippy::print_stdout, clippy::print_stderr)]
pub async fn cmd_run(
    account: Option<&str>,
    args: &[String],
    zone: Option<&str>,
    local_port: Option<u16>,
    public: bool,
    domain: Option<&str>,
    ttl: Option<std::time::Duration>,
) -> Result<()> {
    let (name, target) = match args {
        [target] => (generated_name(), target.clone()),
        [name, target] => (name.clone(), target.clone()),
        _ => bail!("Usage: gtunnel run [NAME] TARGET"),
    };
    crate::state::validate_tunnel_name(&name)?;

    let (_, acct) = crate::commands::resolve(account)?;
    let gcloud_path = gcloud::require_gcloud()?;

    // Everything that can be refused without looking at other runs is refused
    // here; the name and the ports are decided under the lock, in
    // `claim_and_start`
    let (shape, expose_config) = if public {
        let expose = acct.expose.clone().ok_or_else(|| {
            GtunnelError::usage(format!(
                "No relay for account '{}'. Run `gtunnel relay up`",
                acct.name
            ))
        })?;
        // Before the route: the argv that would refuse it is built after the
        // route exists
        crate::tunnel::normalize_local_target(&target)?;
        let domain = expose.domain(domain)?.clone();
        (
            RunShape::Public {
                local_target: target.clone(),
                domain: domain.domain.clone(),
            },
            Some(expose),
        )
    } else {
        let (host, remote_port) = crate::commands::access::parse_access_target(&target)?;
        crate::state::validate_instance_name(&host)?;
        let found = crate::commands::resolve_instance_zone_or_ask(
            &GcpClient::for_account(&acct),
            &acct,
            zone,
            &host,
        )
        .await?;
        if let Some(w) = &found.warning {
            eprintln!("Warning: {w}");
        }
        let zone = found.zone;
        (
            RunShape::Access {
                target: AccessTarget::Instance {
                    instance: host,
                    zone,
                },
                remote_port,
                local_port,
            },
            None,
        )
    };

    let client = GcpClient::for_account(&acct);
    let console = Console::new();
    // Before gcloud exists: from here on every exit is ours to make
    let mut signals = StopSignals::register()?;
    let (child, record, tunnel) =
        claim_and_start(&gcloud_path, &acct, &expose_config, &name, shape, ttl)?;
    // Taken before the select: its `child.wait()` arm reaps gcloud, after which
    // `child.id()` is None and the group would have no name
    let pgid = signalable(record.pid);
    let mut guard = ChildGuard::new(child, pgid);
    // The route is created after gcloud starts, outside the lock (decision
    // 16): its reverse forward is up for the few seconds before the hostname
    // resolves, which serves nothing
    if let Some(expose) = &expose_config {
        let routed = unless_stopped(
            &mut signals,
            routes::add_route(&client, &acct, expose, &tunnel),
        )
        .await;
        let failure = match routed {
            Some(Ok(())) => None,
            Some(Err(e)) => Some(Err(e)),
            // Ctrl+C is a request to stop, not a failure
            None => Some(Ok(())),
        };
        if let Some(outcome) = failure {
            stop_started(&mut guard, &console, &record).await;
            finish_teardown(
                &console,
                &mut signals,
                &tunnel.display_url(),
                teardown(&console, &client, &acct, Some(expose), &tunnel, &record),
            )
            .await?;
            return outcome;
        }
    }

    console.say(format_args!(
        "{}  {} -> {}",
        name,
        tunnel.display_url(),
        tunnel.display_target()
    ));
    match ttl {
        Some(ttl) => console.say(format_args!(
            "Stops by itself in {}, or Ctrl+C",
            humantime::format_duration(ttl)
        )),
        None => console.say(format_args!("Ctrl+C to stop")),
    }

    let stdout = guard.child.stdout.take();
    let stderr = guard.child.stderr.take();
    // The pump ending changes nothing: gcloud may close a stream and keep
    // forwarding, so it waits here for the arms below to decide
    let credential_file = acct.credential_file.is_some();
    let pump = async {
        pump_lines(stdout, stderr, |line| {
            match diagnose_log_line(&line, credential_file) {
                Some(hint) => console.say(format_args!("{line}\n  {hint}")),
                None => console.say(format_args!("{line}")),
            }
        })
        .await;
        std::future::pending::<()>().await;
    };

    // Set only when gcloud ended the run without being asked
    let mut gcloud_failed = None;
    tokio::select! {
        _ = pump => {}
        status = guard.child.wait() => {
            gcloud_failed = Some(match status {
                Ok(status) => {
                    console.say(format_args!("gcloud exited with {status}"));
                    gcloud_exit_code(status)
                }
                Err(e) => {
                    console.warn(format_args!("Failed to wait for gcloud: {e}"));
                    1
                }
            });
        }
        _ = signals.next() => {
            console.say(format_args!(""));
        }
        // The same exit as Ctrl+C, so the route and the record go the same way
        _ = ttl_elapsed(record.expires_at(), TTL_TICK, now_secs) => {
            console.say(format_args!("TTL reached, stopping"));
        }
    }

    // Whichever arm won, the group goes the way the supervisor's does. When
    // gcloud exited by itself its ssh child may still hold the port, and the
    // sweep inside `stop_group` is what ends it.
    if let Err(e) = crate::proc::stop_group(&mut guard.child, pgid, crate::proc::STOP_GRACE).await {
        console.warn(format_args!("Failed to stop gcloud: {e:#}"));
    }
    guard.disarm();
    finish_teardown(
        &console,
        &mut signals,
        &tunnel.display_url(),
        teardown(
            &console,
            &client,
            &acct,
            expose_config.as_ref(),
            &tunnel,
            &record,
        ),
    )
    .await?;
    match gcloud_failed {
        Some(code) => Err(crate::error::Exited(code).into()),
        None => Ok(()),
    }
}

// Every removal is reported as it happened: a DNS record that is still there
// must never be announced as deleted
#[allow(clippy::print_stdout, clippy::print_stderr)]
async fn teardown(
    console: &Console,
    client: &GcpClient,
    acct: &crate::config::Account,
    expose: Option<&crate::config::ExposeConfig>,
    tunnel: &PersistentTunnel,
    record: &EphemeralRecord,
) {
    if let Some(expose) = expose {
        console.say(format_args!("Removing the route..."));
        match routes::remove_route(client, acct, expose, tunnel).await {
            Ok(removal) => {
                if removal.dns {
                    console.say(format_args!(
                        "Removed DNS record for {}",
                        tunnel.display_url()
                    ));
                }
                if removal.backend {
                    console.say(format_args!("Removed backend service"));
                }
                if removal.host_rule {
                    console.say(format_args!("Removed the load balancer host rule"));
                }
            }
            Err(e) => {
                console.warn(format_args!("Failed to remove the relay route: {e:#}"));
                console.warn(format_args!("Run `gtunnel doctor --fix` to clean it up"));
            }
        }
    }
    if let Err(e) = record.remove() {
        console.warn(format_args!("Failed to remove the run record: {e:#}"));
    }
}

/// How long a reaped run's gcloud gets to exit on SIGTERM before its group is
/// killed: the grace a stopping run gives its own.
const REAP_GRACE: std::time::Duration = crate::proc::STOP_GRACE;

/// Stop the runs whose owner is gone or whose TTL has passed, for `accounts`:
/// the process that would have ended them has died, so the next watchdog pass or
/// `list` does it. One line per run, for the caller to print where it can: `list
/// --json` must keep stdout clean.
pub async fn reap_orphans(accounts: &[crate::config::Account]) -> Vec<String> {
    let records: Vec<EphemeralRecord> = list_ephemeral()
        .unwrap_or_default()
        .into_iter()
        .map(|(record, _)| record)
        .filter(|r| accounts.iter().any(|a| a.name == r.account_name))
        .collect();
    reap_with(records, now_secs(), async |record| {
        let Some(acct) = accounts.iter().find(|a| a.name == record.account_name) else {
            bail!("account '{}' is gone", record.account_name);
        };
        let Some(expose) = &acct.expose else {
            bail!("account '{}' has no relay configured", acct.name);
        };
        let client = GcpClient::for_account(acct);
        routes::remove_route(&client, acct, expose, &record.as_tunnel())
            .await
            .map(|_| ())
    })
    .await
}

/// The reaper without GCP: `remove_route` is called for a public run and its
/// failure keeps the record, so the next pass tries the route again instead of
/// forgetting it. A private run has no route and goes with its process.
async fn reap_with(
    records: Vec<EphemeralRecord>,
    now: u64,
    remove_route: impl AsyncFn(&EphemeralRecord) -> Result<()>,
) -> Vec<String> {
    let mut report = Vec::new();
    for record in records {
        let Some(reason) = record.orphan_reason(now) else {
            continue;
        };
        stop_orphan(&record).await;
        if record.as_tunnel().is_expose()
            && let Err(e) = remove_route(&record).await
        {
            report.push(format!(
                "Stopped run {} ({reason}) but could not remove its relay route: {e:#}. \
                 Run `gtunnel doctor --fix` to clean it up",
                record.name
            ));
            continue;
        }
        match record.remove() {
            Ok(()) => report.push(format!("Stopped run {} ({reason})", record.name)),
            Err(e) => report.push(format!(
                "Failed to remove the record of {}: {e:#}",
                record.name
            )),
        }
    }
    report
}

/// SIGTERM to the group, a bounded wait, then SIGKILL to what is left. The
/// group is killed only while the leader we recorded is still there: once it
/// has gone, its pid may belong to a stranger.
async fn stop_orphan(record: &EphemeralRecord) {
    if !record.terminate() {
        stop_leaderless_group(record).await;
        return;
    }
    if !record.wait_for_exit(REAP_GRACE).await
        && let Some(pid) = signalable(record.pid)
    {
        crate::proc::signal_group(pid, libc::SIGKILL);
        return;
    }
    // The leader exited on TERM; an ssh that ignored it may still hold the port
    if let Some(pid) = signalable(record.pid)
        && record.pid_start.is_some()
        && crate::proc::group_alive(pid)
    {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        crate::proc::signal_group(pid, libc::SIGKILL);
    }
}

/// A leader that is already gone (on Linux it dies with its owner) can leave
/// its own children holding the port. While any of them lives the kernel will
/// not hand the group id out as a pid, so a group with members and no process
/// of that pid is still this run's; a pid that is taken may be a stranger's.
async fn stop_leaderless_group(record: &EphemeralRecord) {
    let Some(pid) = signalable(record.pid) else {
        return;
    };
    if record.pid_start.is_none() {
        return;
    }
    // SAFETY: signal 0 sends nothing, and `signalable` rejected 0 and negative
    // values, so this asks about one process and never a group.
    let pid_free = unsafe { libc::kill(pid, 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    if !pid_free || !crate::proc::group_alive(pid) {
        return;
    }
    crate::proc::signal_group(pid, libc::SIGTERM);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    if crate::proc::group_alive(pid) {
        crate::proc::signal_group(pid, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::testing::TempConfigDir;
    use crate::state::AccessTarget;
    use std::time::Duration;

    fn spec() -> TunnelSpec {
        TunnelSpec::Access {
            target: AccessTarget::Instance {
                instance: "bastion".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 5432,
            local_port: 15432,
        }
    }

    #[test]
    fn a_record_round_trips_through_the_run_directory() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-abc123".to_string(),
            account_name: "dev".to_string(),
            pid: std::process::id(),
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        record.save().expect("save");

        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, record);
        assert!(found[0].1, "the current process is alive");

        record.remove().expect("remove");
        assert!(list_ephemeral().expect("list").is_empty());
    }

    // The run's teardown removes its record last, and `reset` may have deleted
    // the directory by then; creating it again is what left a config behind
    #[test]
    fn a_run_removing_its_record_does_not_recreate_a_deleted_config_directory() {
        let temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-abc123".to_string(),
            account_name: "dev".to_string(),
            pid: std::process::id(),
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        record.save().expect("save");
        std::fs::remove_dir_all(temp.path()).unwrap();

        record
            .remove()
            .expect("a record in a missing directory is already gone");
        assert!(
            !temp.path().exists(),
            "remove recreated the config directory"
        );
    }

    #[test]
    fn a_lookup_that_failed_is_unknown_never_gone() {
        use crate::proc::Presence;
        assert_eq!(judge(Presence::Unknown, Some("t")), Liveness::Unknown);
        assert_eq!(judge(Presence::Unknown, None), Liveness::Unknown);
        assert_eq!(judge(Presence::Gone, Some("t")), Liveness::Gone);
        assert_eq!(judge(Presence::Gone, None), Liveness::Gone);
        assert_eq!(
            judge(Presence::Alive("t".into()), Some("t")),
            Liveness::Alive
        );
        assert_eq!(
            judge(Presence::Alive("u".into()), Some("t")),
            Liveness::Gone
        );
        assert_eq!(judge(Presence::Alive("u".into()), None), Liveness::Alive);
    }

    #[test]
    fn a_pid_that_would_signal_a_group_is_never_signalable() {
        assert_eq!(signalable(0), None, "kill(0) signals our own process group");
        assert_eq!(
            signalable(u32::MAX),
            None,
            "u32::MAX wraps to -1, which signals every process we may signal"
        );
        assert_eq!(signalable(i32::MAX as u32 + 1), None, "negative: a group");
        assert_eq!(signalable(1234), Some(1234));
    }

    #[test]
    fn a_record_with_pid_zero_is_not_running() {
        assert!(!pid_is_alive(0), "kill(0, 0) succeeds for our own group");
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-zero00".to_string(),
            account_name: "dev".to_string(),
            pid: 0,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        record.save().expect("save");
        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1, "an old record with pid 0 still parses");
        assert!(!found[0].1, "pid 0 must not read as a live run");
    }

    // Orphaned so the kernel reaps it: a child of this test would linger as a
    // zombie and still answer kill(pid, 0)
    fn orphan_sleep() -> u32 {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("/bin/sleep 30 >/dev/null 2>&1 & echo $!")
            .output()
            .expect("spawn");
        let pid: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("pid");
        assert!(pid_is_alive(pid), "the sleep should be running");
        pid
    }

    fn kill_pid(pid: u32) {
        // SAFETY: kill(2) takes two integers and no pointer. The pid is a sleep
        // this test spawned and still holds, so it is not a recycled pid.
        unsafe { libc::kill(signalable(pid).expect("a real pid"), libc::SIGKILL) };
    }

    fn record(name: &str) -> EphemeralRecord {
        EphemeralRecord {
            name: name.to_string(),
            account_name: "dev".to_string(),
            pid: 1,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        }
    }

    // Importing a run SIGTERMs it, and the dying process removes its relay
    // route on the way out. Rebuilding the route before it is gone loses the
    // race, so the import has to wait for the pid first.
    #[tokio::test]
    async fn wait_for_exit_waits_for_a_live_run_and_reports_a_dead_one() {
        let pid = orphan_sleep();
        let run = EphemeralRecord {
            pid,
            pid_start: crate::proc::start_token(pid),
            owner_pid: None,
            owner_start: None,
            ..record("gt-wait")
        };
        assert!(
            !run.wait_for_exit(Duration::from_millis(300)).await,
            "a live pid must not be reported as exited"
        );
        // The sleep is no group leader, so `terminate` (which signals the group
        // of a tokened record) would reach nothing: signal the pid itself
        // SAFETY: kill(2) takes two integers and no pointer. The pid is the
        // sleep spawned above, which is still running, so it is not recycled.
        unsafe { libc::kill(signalable(pid).expect("a real pid"), libc::SIGTERM) };
        assert!(
            run.wait_for_exit(Duration::from_secs(5)).await,
            "a killed pid must be reported as exited"
        );
    }

    #[test]
    fn a_record_whose_start_differs_reads_as_dead_and_terminate_sends_nothing() {
        let _temp = TempConfigDir::new();
        let pid = orphan_sleep();
        let run = EphemeralRecord {
            pid,
            pid_start: Some("0.000000".into()),
            owner_pid: None,
            owner_start: None,
            ..record("gt-reuse")
        };
        run.save().unwrap();
        assert!(
            !list_ephemeral().unwrap()[0].1,
            "a recycled pid is not our run"
        );
        assert!(!run.terminate(), "nothing is sent to a stranger");
        assert!(pid_is_alive(pid), "the stranger is untouched");
        kill_pid(pid);
    }

    #[test]
    fn a_record_with_its_own_start_reads_as_alive() {
        let pid = orphan_sleep();
        let run = EphemeralRecord {
            pid,
            pid_start: crate::proc::start_token(pid),
            owner_pid: None,
            owner_start: None,
            ..record("gt-live")
        };
        assert!(run.is_alive());
        kill_pid(pid);
    }

    #[tokio::test]
    async fn an_old_record_is_judged_by_its_pid_and_terminate_signals_the_pid_alone() {
        let pid = orphan_sleep(); // not a group leader of its own
        let old = format!(
            r#"{{"name":"gt-old","account_name":"dev","pid":{pid},"started_at":1700000000,
"spec":{{"mode":"access","target":{{"kind":"instance","instance":"bastion","zone":"z"}},"remote_port":5432,"local_port":15000}}}}"#
        );
        let run: EphemeralRecord = serde_json::from_str(&old).expect("0.1.42 record parses");
        assert_eq!(run.pid_start, None);
        assert!(run.is_alive());
        assert!(!serde_json::to_string(&run).unwrap().contains("pid_start"));
        assert!(run.terminate());
        assert!(run.wait_for_exit(Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn terminate_signals_the_whole_group_of_a_new_record() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild");
        let mut leader = Command::new("sh")
            .args([
                "-c",
                &format!("sleep 30 & echo $! > {}; wait", pidfile.display()),
            ])
            .process_group(0)
            .spawn()
            .expect("spawn");
        let pid = leader.id().expect("pid");
        let run = EphemeralRecord {
            pid,
            pid_start: crate::proc::start_token(pid),
            owner_pid: None,
            owner_start: None,
            ..record("gt-group")
        };
        let mut grandchild = 0;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(p) = text.trim().parse::<i32>()
            {
                grandchild = p;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(grandchild > 0, "the grandchild never reported its pid");
        assert!(run.terminate());
        // The leader would wait 30 s for its sleep, so exiting within 5 s is the
        // signal. Its status proves nothing: a bare `wait` returns 0 whatever
        // ended the children, so a shell that outlives the instant exits 0
        tokio::time::timeout(Duration::from_secs(5), leader.wait())
            .await
            .expect("the leader exits")
            .unwrap();
        let mut gone = false;
        for _ in 0..40 {
            // SAFETY: signal 0 sends nothing; the pid is positive
            if unsafe { libc::kill(grandchild, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(gone, "pid {grandchild} survived the group SIGTERM");
    }

    // Suspend stops the monotonic clock, not the wall clock: an hour asleep is an
    // hour on `now` and nothing on tokio's timer
    #[tokio::test]
    async fn the_ttl_fires_on_the_wall_clock_when_the_monotonic_clock_is_behind() {
        let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000));
        let c = clock.clone();
        let fut = tokio::spawn(ttl_elapsed(
            Some(1_000 + 3_600),
            Duration::from_millis(10),
            move || c.load(std::sync::atomic::Ordering::SeqCst),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !fut.is_finished(),
            "an hour has not passed on the wall clock"
        );
        clock.store(1_000 + 3_601, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(Duration::from_millis(500), fut)
            .await
            .expect("fired")
            .unwrap();
    }

    #[tokio::test]
    async fn a_run_without_a_ttl_never_times_out() {
        let r = tokio::time::timeout(
            Duration::from_millis(100),
            ttl_elapsed(None, Duration::from_millis(10), || u64::MAX),
        )
        .await;
        assert!(r.is_err());
    }

    fn dev_account() -> crate::config::Account {
        crate::config::Account {
            name: "dev".to_string(),
            gcloud_configuration: "work".to_string(),
            project: "example-dev-1a2b".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: None,
            expose: None,
        }
    }

    // A stand-in for gcloud: the script runs as the spawned process, so its
    // pid is the one the record names
    fn fake_gcloud(dir: &std::path::Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("gcloud");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn a_spawned_run_records_its_start_and_leads_its_own_group() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let script = fake_gcloud(dir.path(), "exec sleep 30");
        let acct = dev_account();
        let tunnel = record("gt-grp").as_tunnel();
        let (mut child, run) =
            start_child(&script, &acct, &None, &tunnel, spec(), "gt-grp", None).expect("start");
        assert!(run.pid_start.is_some());
        assert_eq!(run.pid_start, crate::proc::start_token(run.pid));
        // SAFETY: getpgid takes an integer
        assert_eq!(unsafe { libc::getpgid(run.pid as i32) }, run.pid as i32);
        crate::proc::stop_group(&mut child, Some(run.pid as i32), Duration::from_millis(600))
            .await
            .expect("stop");
        run.remove().unwrap();
    }

    fn prod_account() -> crate::config::Account {
        crate::config::Account {
            name: "prod".to_string(),
            ..dev_account()
        }
    }

    fn access(local_port: Option<u16>) -> RunShape {
        RunShape::Access {
            target: AccessTarget::Instance {
                instance: "bastion".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 5432,
            local_port,
        }
    }

    fn local_port(run: &EphemeralRecord) -> u16 {
        match &run.spec {
            TunnelSpec::Access { local_port, .. } => *local_port,
            TunnelSpec::Expose { .. } => panic!("an access run"),
        }
    }

    async fn stop(child: &mut tokio::process::Child, run: &EphemeralRecord) {
        crate::proc::stop_group(child, signalable(run.pid), Duration::from_millis(600))
            .await
            .expect("stop");
    }

    #[test]
    fn a_saved_name_is_refused_while_a_live_run_holds_it_on_that_account() {
        let _temp = TempConfigDir::new();
        let live = EphemeralRecord {
            pid: std::process::id(),
            pid_start: crate::proc::start_token(std::process::id()),
            ..record("db")
        };
        live.save().unwrap();
        let err = check_name_free_of_runs("dev", "db").unwrap_err();
        assert_eq!(
            err.to_string(),
            "A run named 'db' is live for account 'dev'; stop it or choose another name"
        );
        assert_eq!(crate::error::kind_of(&err), crate::error::ErrorKind::Usage);
        check_name_free_of_runs("prod", "db").expect("another account");
        check_name_free_of_runs("dev", "cache").expect("another name");
    }

    #[tokio::test]
    async fn check_run_name_refuses_a_live_run_with_launchs_message_and_a_managed_tunnel() {
        let _temp = TempConfigDir::new();
        let live = EphemeralRecord {
            pid: std::process::id(),
            pid_start: crate::proc::start_token(std::process::id()),
            owner_pid: None,
            owner_start: None,
            ..record("db")
        };
        live.save().unwrap();
        let err = check_run_name(&TunnelState::default(), "dev", "db").unwrap_err();
        assert_eq!(
            err.to_string(),
            "A run named 'db' is already live for account 'dev'"
        );
        assert_eq!(crate::error::kind_of(&err), crate::error::ErrorKind::Usage);
        check_run_name(&TunnelState::default(), "prod", "db").expect("another account");

        let mut state = TunnelState::default();
        state.add(record("k8s").as_tunnel()).unwrap();
        assert_eq!(
            check_run_name(&state, "dev", "k8s")
                .unwrap_err()
                .to_string(),
            "Tunnel 'k8s' already exists on account 'dev'"
        );
        // A dead record does not block
        EphemeralRecord {
            pid: 999_999,
            ..record("old")
        }
        .save()
        .unwrap();
        check_run_name(&TunnelState::default(), "dev", "old").expect("stale record");
    }

    #[test]
    fn a_run_whose_joined_name_another_accounts_live_run_holds_is_refused() {
        let _temp = TempConfigDir::new();
        EphemeralRecord {
            pid: std::process::id(),
            pid_start: crate::proc::start_token(std::process::id()),
            owner_pid: None,
            owner_start: None,
            account_name: "prod-eu".to_string(),
            ..record("api")
        }
        .save()
        .unwrap();
        let err = check_run_name(&TunnelState::default(), "prod", "eu-api").unwrap_err();
        assert_eq!(
            err.to_string(),
            "A run named 'eu-api' on account 'prod' would share its record and log with \
             run 'api' on account 'prod-eu' (both are 'prod-eu-api'); choose another name"
        );
        assert_eq!(crate::error::kind_of(&err), crate::error::ErrorKind::Usage);
    }

    #[test]
    fn a_dead_run_of_another_account_does_not_block_the_joined_name() {
        let _temp = TempConfigDir::new();
        EphemeralRecord {
            pid: 999_999,
            account_name: "prod-eu".to_string(),
            ..record("api")
        }
        .save()
        .unwrap();
        check_run_name(&TunnelState::default(), "prod", "eu-api").expect("dead record");
    }

    #[test]
    fn a_local_run_port_of_another_account_is_reserved() {
        let _temp = TempConfigDir::new();
        EphemeralRecord {
            account_name: "prod".to_string(),
            ..record("gt-other")
        }
        .save()
        .unwrap();
        assert!(run_local_ports().unwrap().contains(&15432));
    }

    #[tokio::test]
    async fn two_claims_of_one_name_in_two_accounts_get_different_ports() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let gcloud = fake_gcloud(dir.path(), "exec sleep 30");
        // The same name starts the search at the same port in both
        let (mut c1, a, _) =
            claim_and_start(&gcloud, &dev_account(), &None, "db", access(None), None).unwrap();
        let (mut c2, b, _) =
            claim_and_start(&gcloud, &prod_account(), &None, "db", access(None), None).unwrap();
        assert_ne!(local_port(&a), local_port(&b));
        stop(&mut c1, &a).await;
        stop(&mut c2, &b).await;
    }

    #[tokio::test]
    async fn a_second_claim_of_a_live_name_is_refused_and_spawns_nothing() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let gcloud = fake_gcloud(dir.path(), "exec sleep 30");
        let marked = tempfile::tempdir().unwrap();
        let marker = marked.path().join("ran");
        let marking = fake_gcloud(
            marked.path(),
            &format!("touch {}; exec sleep 30", marker.display()),
        );
        let (mut child, first, _) =
            claim_and_start(&gcloud, &dev_account(), &None, "db", access(None), None).unwrap();
        let err =
            claim_and_start(&marking, &dev_account(), &None, "db", access(None), None).unwrap_err();
        assert!(err.to_string().contains("already live"), "{err}");
        assert!(!marker.exists(), "no second gcloud");
        let stored: EphemeralRecord =
            serde_json::from_str(&std::fs::read_to_string(first.path().unwrap()).unwrap()).unwrap();
        assert_eq!(stored, first, "the first record is untouched");
        stop(&mut child, &first).await;
    }

    #[tokio::test]
    async fn an_explicit_port_another_accounts_tunnel_owns_is_refused() {
        let _temp = TempConfigDir::new();
        let owner = PersistentTunnel {
            account_name: "prod".to_string(),
            ..record("k8s").as_tunnel()
        };
        TunnelState::update(|state| state.add(owner)).unwrap();
        let marked = tempfile::tempdir().unwrap();
        let marker = marked.path().join("ran");
        let marking = fake_gcloud(
            marked.path(),
            &format!("touch {}; exec sleep 30", marker.display()),
        );
        let err = claim_and_start(
            &marking,
            &dev_account(),
            &None,
            "db",
            access(Some(15432)),
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("already used by another gtunnel tunnel"),
            "{err}"
        );
        assert!(!marker.exists());
        assert!(list_ephemeral().unwrap().is_empty());
    }

    #[test]
    fn claim_and_start_runs_under_the_tunnels_lock() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let gcloud = fake_gcloud(dir.path(), "exec sleep 30");
        let held = crate::fsutil::lock_exclusive(
            &crate::state::tunnels_lock_path().unwrap(),
            crate::fsutil::lock_deadline(),
        )
        .unwrap();
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let (mut child, run, _) =
                    claim_and_start(&gcloud, &dev_account(), &None, "db", access(None), None)?;
                stop(&mut child, &run).await;
                Ok::<_, anyhow::Error>(run)
            })
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            list_ephemeral().unwrap().is_empty(),
            "nothing recorded while the lock is held"
        );
        drop(held);
        let run = handle
            .join()
            .unwrap()
            .expect("claimed once the lock is free");
        assert_eq!(list_ephemeral().unwrap()[0].0, run);
    }

    #[tokio::test]
    async fn stop_started_then_teardown_stops_the_group_and_removes_the_record() {
        let _temp = TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        let gcloud = fake_gcloud(dir.path(), "exec sleep 30");
        let acct = dev_account();
        let (child, run, tunnel) =
            claim_and_start(&gcloud, &acct, &None, "db", access(None), None).unwrap();
        let mut guard = ChildGuard::new(child, signalable(run.pid));
        let console = Console::new();
        stop_started(&mut guard, &console, &run).await;
        teardown(
            &console,
            &GcpClient::for_account(&acct),
            &acct,
            None,
            &tunnel,
            &run,
        )
        .await;
        assert!(!run.is_alive());
        assert!(list_ephemeral().unwrap().is_empty());
    }

    // `expose` read tunnels.toml alone, so it handed out a relay port a live
    // `gtunnel run --public` was already bound to on the relay instance
    #[test]
    fn used_relay_ports_counts_a_live_run() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-live".to_string(),
            account_name: "dev".to_string(),
            pid: std::process::id(),
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "gt-live.dev.example.com".to_string(),
                relay_port: 30007,
                private: false,
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        record.save().expect("save");

        let state = TunnelState::default();
        let used = used_relay_ports(&state, "dev").expect("ports");
        assert!(used.contains(&30007), "{used:?}");
        assert!(
            used_relay_ports(&state, "prod").expect("ports").is_empty(),
            "another account's run is not this account's port"
        );
    }

    #[test]
    fn a_dead_pid_is_reported_as_not_alive() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-dead01".to_string(),
            account_name: "dev".to_string(),
            pid: 999_999,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        record.save().expect("save");

        let found = list_ephemeral().expect("list");
        assert_eq!(found.len(), 1);
        assert!(!found[0].1, "pid 999999 should not be running");
    }

    #[test]
    fn an_old_record_without_ttl_secs_still_parses() {
        let old = r#"{"name":"gt-old","account_name":"dev","pid":42,"started_at":1700000000,
"spec":{"mode":"access","target":{"kind":"instance","instance":"bastion","zone":"z"},"remote_port":5432,"local_port":15000}}"#;
        let record: EphemeralRecord = serde_json::from_str(old).expect("parse");
        assert_eq!(record.ttl_secs, None);
        assert_eq!(record.expires_at(), None);
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("ttl_secs"), "{json}");
    }

    #[test]
    fn a_record_with_a_ttl_expires_at_start_plus_ttl() {
        let record = EphemeralRecord {
            name: "gt-ttl".to_string(),
            account_name: "dev".to_string(),
            pid: 1,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: Some(90),
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        assert_eq!(record.expires_at(), Some(1_700_000_090));
    }

    #[test]
    fn live_runs_skips_dead_records_and_other_accounts() {
        let _temp = TempConfigDir::new();
        let mk = |name: &str, account: &str, pid: u32| EphemeralRecord {
            name: name.to_string(),
            account_name: account.to_string(),
            pid,
            spec: spec(),
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        mk("alive", "dev", std::process::id()).save().unwrap();
        mk("dead", "dev", 999_999).save().unwrap();
        mk("other", "prod", std::process::id()).save().unwrap();
        let names: Vec<String> = live_runs("dev")
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["alive"]);
    }

    #[test]
    fn a_ttl_reads_seconds_minutes_and_hours() {
        assert_eq!(parse_ttl("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_ttl("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_ttl("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_ttl(" 5s ").unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn a_bare_ttl_is_minutes() {
        assert_eq!(parse_ttl("45").unwrap(), Duration::from_secs(45 * 60));
    }

    #[test]
    fn a_ttl_of_zero_or_nonsense_is_refused() {
        for bad in ["0", "0s", "0m", "", "m", "-5m", "1.5h", "10d", "abc", "5 m"] {
            assert!(parse_ttl(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_ttl_past_a_day_is_refused_and_a_day_is_not() {
        assert!(parse_ttl("24h").is_ok());
        assert!(parse_ttl("1440m").is_ok());
        assert!(parse_ttl("86401s").is_err());
        assert!(parse_ttl("25h").is_err());
        assert!(parse_ttl("99999999999999999999h").is_err(), "overflow");
    }

    #[test]
    fn a_generated_name_is_gt_plus_six_characters() {
        for _ in 0..20 {
            let name = generated_name();
            assert_eq!(name.len(), 9, "{name}");
            assert!(name.starts_with("gt-"), "{name}");
            assert!(
                name[3..]
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "{name}"
            );
            crate::state::validate_tunnel_name(&name).expect("a valid tunnel name");
        }
    }

    #[test]
    fn generated_names_differ() {
        let a = generated_name();
        let b = generated_name();
        assert_ne!(a, b);
    }
    // A record whose process died may still own a relay route, so its port
    // stays reserved until teardown or doctor confirms the route is gone
    #[test]
    fn a_dead_records_relay_port_stays_reserved() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-dead02".to_string(),
            account_name: "dev".to_string(),
            pid: 999_999,
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "gt-dead02.dev.example.com".to_string(),
                relay_port: 30_123,
                private: false,
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        record.save().expect("save");

        let used = run_relay_ports("dev").expect("ports");
        assert!(used.contains(&30_123), "{used:?}");
    }

    #[test]
    fn another_accounts_ports_are_not_reserved() {
        let _temp = TempConfigDir::new();
        let record = EphemeralRecord {
            name: "gt-other1".to_string(),
            account_name: "prod".to_string(),
            pid: std::process::id(),
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "prod.example.com".to_string(),
                hostname: "gt-other1.prod.example.com".to_string(),
                relay_port: 30_124,
                private: false,
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        };
        record.save().expect("save");

        let used = run_relay_ports("dev").expect("ports");
        assert!(!used.contains(&30_124), "{used:?}");
    }

    // ---- owner: who tears a run down, as opposed to the gcloud it spawned

    fn owned_by_us(run: EphemeralRecord) -> EphemeralRecord {
        EphemeralRecord {
            owner_pid: Some(std::process::id()),
            owner_start: crate::proc::start_token(std::process::id()),
            ..run
        }
    }

    // The pid is real and alive, the start token is not its own: a recycled pid
    fn owned_by_a_stranger(run: EphemeralRecord) -> EphemeralRecord {
        EphemeralRecord {
            owner_pid: Some(std::process::id()),
            owner_start: Some("0.000000".into()),
            ..run
        }
    }

    fn public_run(name: &str) -> EphemeralRecord {
        EphemeralRecord {
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: format!("{name}.dev.example.com"),
                relay_port: 30_200,
                private: false,
            },
            ..record(name)
        }
    }

    // A real gcloud stand-in: a group leader, reaped by a thread the moment it
    // dies so a zombie never answers kill(pid, 0)
    fn group_leader_sleep() -> (u32, Option<String>) {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let token = crate::proc::start_token(pid);
        std::thread::spawn(move || child.wait());
        (pid, token)
    }

    #[test]
    fn a_record_written_before_the_owner_fields_still_loads() {
        // Exactly what 0.2.39 wrote: this version's record minus the new fields
        let mut value = serde_json::to_value(record("gt-old001")).unwrap();
        value.as_object_mut().unwrap().remove("owner_pid");
        value.as_object_mut().unwrap().remove("owner_start");
        let run: EphemeralRecord = serde_json::from_value(value).unwrap();
        assert_eq!(run.owner_pid, None);
        assert_eq!(run.owner_alive(), None, "no owner named, none judged");
        let written = serde_json::to_string(&run).unwrap();
        assert!(!written.contains("owner_pid"), "{written}");
    }

    #[test]
    fn the_owner_is_judged_by_its_pid_and_its_start() {
        assert_eq!(owned_by_us(record("gt-own1")).owner_alive(), Some(true));
        assert_eq!(
            owned_by_a_stranger(record("gt-own2")).owner_alive(),
            Some(false),
            "a live pid with another start is not our owner"
        );
        let gone = EphemeralRecord {
            owner_pid: Some(0),
            ..record("gt-own3")
        };
        assert_eq!(gone.owner_alive(), Some(false));
    }

    #[tokio::test]
    async fn the_handover_waits_for_the_owner_not_for_gcloud() {
        // gcloud is already gone; the owner, which still has a route to remove,
        // is a live sleep
        let gcloud = orphan_sleep();
        kill_pid(gcloud);
        let owner = orphan_sleep();
        let run = EphemeralRecord {
            pid: gcloud,
            pid_start: None,
            owner_pid: Some(owner),
            owner_start: crate::proc::start_token(owner),
            ..record("gt-handover")
        };
        assert!(
            !run.wait_for_handover(Duration::from_millis(400), Duration::ZERO)
                .await,
            "gcloud being gone is not the handover while the owner lives"
        );
        kill_pid(owner);
        assert!(
            run.wait_for_handover(Duration::from_secs(5), Duration::ZERO)
                .await
        );
    }

    #[tokio::test]
    async fn a_record_without_an_owner_settles_after_gcloud_exits() {
        let gcloud = orphan_sleep();
        kill_pid(gcloud);
        let run = EphemeralRecord {
            pid: gcloud,
            ..record("gt-settle")
        };
        let began = std::time::Instant::now();
        assert!(
            run.wait_for_handover(Duration::from_secs(5), Duration::from_millis(300))
                .await
        );
        assert!(
            began.elapsed() >= Duration::from_millis(300),
            "an owner-less record gets the settle delay: {:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_run_is_an_orphan_when_its_owner_is_gone_or_its_ttl_has_passed() {
        let now = 1_700_000_100;
        let mut live = owned_by_us(record("gt-live1"));
        assert_eq!(live.orphan_reason(now), None, "a live owner, no TTL");

        live.ttl_secs = Some(80);
        assert_eq!(live.orphan_reason(now), None, "inside the owner's grace");
        assert!(
            live.orphan_reason(now + EXPIRY_GRACE).is_some(),
            "a live owner that has not ended a run past its TTL"
        );

        let dead = owned_by_a_stranger(record("gt-dead1"));
        assert_eq!(dead.orphan_reason(now), Some("its process is gone"));

        let old = EphemeralRecord {
            ttl_secs: Some(50),
            ..record("gt-old1")
        };
        assert_eq!(old.orphan_reason(now), Some("it is past its TTL"));
        assert_eq!(
            record("gt-old2").orphan_reason(now),
            None,
            "no owner and no TTL is never judged"
        );
    }

    #[test]
    fn the_stop_signals_are_raced_against_route_creation() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut signals = StopSignals::register().expect("register");
            // SAFETY: raise(3) sends a signal to this process; SIGHUP has a
            // handler registered just above, so it does not end the test run.
            unsafe { libc::raise(libc::SIGHUP) };
            let stopped = unless_stopped(&mut signals, std::future::pending::<()>()).await;
            assert!(
                stopped.is_none(),
                "a signal ends a route that never returns"
            );
            let done = unless_stopped(&mut signals, async { 7 }).await;
            assert_eq!(done, Some(7), "work that finishes is not cut short");
        });
    }

    #[tokio::test]
    async fn a_byte_that_is_not_utf8_is_printed_lossily_and_ends_nothing() {
        let out: &[u8] = b"first\n\xff broken\nlast without a newline";
        let mut lines = Vec::new();
        pump_lines(Some(out), None::<&[u8]>, |line| lines.push(line)).await;
        assert_eq!(
            lines,
            ["first", "\u{fffd} broken", "last without a newline"]
        );
    }

    #[tokio::test]
    async fn one_stream_closing_keeps_the_other_read() {
        use tokio::io::AsyncWriteExt;
        let (out_tx, out_rx) = tokio::io::duplex(64);
        let (mut err_tx, err_rx) = tokio::io::duplex(64);
        drop(out_tx);
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            err_tx.write_all(b"late\n").await.unwrap();
        });
        let mut lines = Vec::new();
        pump_lines(Some(out_rx), Some(err_rx), |line| lines.push(line)).await;
        writer.await.unwrap();
        assert_eq!(lines, ["late"], "stdout closing first did not end stderr");
    }

    #[test]
    fn the_exit_a_run_makes_for_gcloud_is_its_code_and_never_zero() {
        use std::os::unix::process::ExitStatusExt;
        let exited = |code: i32| std::process::ExitStatus::from_raw(code << 8);
        assert_eq!(gcloud_exit_code(exited(7)), 7);
        assert_eq!(gcloud_exit_code(exited(1)), 1);
        assert_eq!(
            gcloud_exit_code(exited(0)),
            1,
            "a tunnel that ended on its own is a failure"
        );
        assert_eq!(
            gcloud_exit_code(std::process::ExitStatus::from_raw(9)),
            1,
            "a signal death has no code"
        );
    }

    #[test]
    fn a_second_stop_signal_abandons_the_route_teardown_and_exits_130() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let console = Console::new();
            let mut signals = StopSignals::register().expect("register");
            // SAFETY: raise(3) sends a signal to this process; SIGHUP has a
            // handler registered just above, so it does not end the test run.
            // Not SIGINT: the supervisor's tests in this process stop their
            // child on it, and a raised one ends them early.
            unsafe { libc::raise(libc::SIGHUP) };
            let err = finish_teardown(
                &console,
                &mut signals,
                "https://x.example.com",
                std::future::pending::<()>(),
            )
            .await
            .expect_err("a stalled teardown is abandoned");
            assert_eq!(
                err.downcast_ref::<crate::error::Exited>().map(|e| e.0),
                Some(130)
            );
            finish_teardown(&console, &mut signals, "https://x.example.com", async {})
                .await
                .expect("a teardown that finishes is not cut short");
        });
    }

    #[test]
    fn a_closed_stdout_stops_the_printing_and_never_panics() {
        let console = Console::new();
        console.out.set(false);
        console.say(format_args!("dropped"));
        assert!(!console.out.get());
        console.warn(format_args!("still open"));
        assert!(console.err.get());
    }

    #[tokio::test]
    async fn reaping_stops_the_gcloud_of_a_dead_owners_run_and_removes_its_record() {
        let _temp = TempConfigDir::new();
        let (pid, token) = group_leader_sleep();
        let run = owned_by_a_stranger(EphemeralRecord {
            pid,
            pid_start: token,
            ..record("gt-reap1")
        });
        run.save().unwrap();
        assert!(run.is_alive());

        let report = reap_with(vec![run.clone()], now_secs(), async |_| {
            panic!("a private run has no route")
        })
        .await;

        assert!(
            run.wait_for_exit(Duration::from_secs(5)).await,
            "gcloud is gone"
        );
        assert!(list_ephemeral().unwrap().is_empty(), "record removed");
        assert_eq!(report, vec!["Stopped run gt-reap1 (its process is gone)"]);
    }

    #[tokio::test]
    async fn a_run_past_its_ttl_whose_owner_is_gone_is_stopped_by_one_reap() {
        let _temp = TempConfigDir::new();
        let (pid, token) = group_leader_sleep();
        let run = EphemeralRecord {
            pid,
            pid_start: token,
            ttl_secs: Some(1),
            started_at: now_secs() - 100,
            ..record("gt-reap2")
        };
        run.save().unwrap();

        reap_with(vec![run.clone()], now_secs(), async |_| Ok(())).await;

        assert!(!run.is_alive());
        assert!(list_ephemeral().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_live_owners_run_is_left_alone() {
        let _temp = TempConfigDir::new();
        let run = owned_by_us(record("gt-keep1"));
        run.save().unwrap();
        let report = reap_with(vec![run], now_secs(), async |_| panic!("nothing to reap")).await;
        assert!(report.is_empty());
        assert_eq!(list_ephemeral().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reaping_a_public_run_removes_its_route_and_keeps_the_record_when_that_fails() {
        let _temp = TempConfigDir::new();
        let calls = std::cell::RefCell::new(Vec::new());
        let run = owned_by_a_stranger(public_run("gt-pub1"));
        run.save().unwrap();

        let report = reap_with(vec![run.clone()], now_secs(), async |r| {
            calls.borrow_mut().push(r.name.clone());
            bail!("the relay is unreachable")
        })
        .await;
        assert_eq!(*calls.borrow(), vec!["gt-pub1"], "the route is torn down");
        assert_eq!(list_ephemeral().unwrap().len(), 1, "kept for the next pass");
        assert!(report[0].contains("doctor --fix"), "{report:?}");

        reap_with(vec![run], now_secs(), async |r| {
            calls.borrow_mut().push(r.name.clone());
            Ok(())
        })
        .await;
        assert_eq!(calls.borrow().len(), 2);
        assert!(
            list_ephemeral().unwrap().is_empty(),
            "gone once the route is"
        );
    }

    fn write_run_file(name: &str, text: &str) {
        std::fs::write(run_dir().unwrap().join(name), text).expect("write fixture");
    }

    // A record a newer gtunnel wrote in a shape this one cannot parse: skipping
    // it silently left the run unlisted, unreaped, and its port free to hand out
    #[test]
    fn an_unparseable_run_record_is_reported_and_its_port_is_not_allocated() {
        let _temp = TempConfigDir::new();
        write_run_file(
            "dev-mesh.json",
            r#"{"name":"mesh","account_name":"dev","pid":1,"started_at":1,
                "spec":{"mode":"mesh_relay","local_port":15999,"relay_port":16000}}"#,
        );

        assert!(list_ephemeral().unwrap().is_empty(), "it is not a record");
        let found = unreadable_records().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, "dev-mesh.json");
        assert_eq!(found[0].local_port, Some(15999));
        assert_eq!(found[0].relay_port, Some(16000));

        assert!(run_local_ports().unwrap().contains(&15999));
        assert!(run_relay_ports("dev").unwrap().contains(&16000));
        assert!(
            !run_relay_ports("prod").unwrap().contains(&16000),
            "a relay port belongs to the record's own account"
        );

        let mut used = std::collections::HashSet::new();
        used.extend(run_local_ports().unwrap());
        let port = allocate_port("mesh", 15999, 1, &used, |_| true);
        assert!(port.is_err(), "the only port in the range is taken");
    }

    #[test]
    fn a_run_file_that_is_not_json_is_reported_with_no_port_to_hold() {
        let _temp = TempConfigDir::new();
        write_run_file("dev-broken.json", "{ not json");
        let found = unreadable_records().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].local_port, None);
        assert!(!found[0].reason.is_empty());
        assert!(run_local_ports().unwrap().is_empty());
    }

    #[test]
    fn a_readable_run_record_is_not_reported_as_unreadable() {
        let _temp = TempConfigDir::new();
        record("gt-fine").save().unwrap();
        assert!(unreadable_records().unwrap().is_empty());
    }
}
