use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::config::Config;
use crate::state::{PersistentTunnel, TunnelSpec};
use crate::tunnel;

pub mod launchd;
pub mod refresh;
pub mod systemd;

// What a service manager needs to run one tunnel
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub log_path: PathBuf,
    pub env: Vec<(String, String)>,
    pub auto_start: bool,
    // What the log's opening line names, so a backend can write it without
    // knowing anything about tunnels
    pub description: String,
}

// Runtime facts the service manager reports back
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub pid: Option<u32>,
    pub restarts: u32,
}

/// The restart budget is read by the supervisor, which a service manager
/// starts with none of the installing shell's variables. Whatever
/// `GTUNNEL_RESTART_*` is set when the service is written is written into it,
/// the way the log level rides on the command line, so the override a person
/// exported is the one the supervisor sees.
const RESTART_VARS: [&str; 5] = [
    "GTUNNEL_RESTART_MAX_RESTARTS",
    "GTUNNEL_RESTART_WINDOW",
    "GTUNNEL_RESTART_MIN_UPTIME",
    "GTUNNEL_RESTART_BASE_BACKOFF",
    "GTUNNEL_RESTART_MAX_BACKOFF",
];

/// The gcloud, proxy and CA variables a supervised gcloud needs and a service
/// manager does not hand it.
///
/// launchd and `systemd --user` start a job with an empty environment, while
/// the login check, `doctor` and the REST client run in the user's shell. A
/// person who points `CLOUDSDK_CONFIG` at a second gcloud home, or sits behind
/// a proxy that re-signs TLS, saw `start` pass its login check and the tunnel
/// then fail as `auth expired`: the two were not looking at the same gcloud.
/// Read when the definition is written, like `GTUNNEL_RESTART_*`, and part of
/// what the post-upgrade refresh compares, so a changed value is rewritten.
/// Lower-case proxy forms are listed because Python's `requests` and curl read
/// them as well as the upper-case ones.
pub const FORWARDED_ENV_VARS: [&str; 12] = [
    "CLOUDSDK_CONFIG",
    "CLOUDSDK_PYTHON",
    "CLOUDSDK_PYTHON_SITEPACKAGES",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CLOUDSDK_CORE_CUSTOM_CA_CERTS_FILE",
];

/// The environment entry that says which gtunnel wrote a definition.
///
/// The refresh cannot tell an older definition from a newer one by reading its
/// arguments: a downgrade, or a second install on `PATH`, looks exactly like
/// an upgrade to a binary that treats every difference as "mine is right", and
/// two versions then rewrite and restart every tunnel back and forth. The
/// stamp gives the comparison a direction. It lives in the environment, where
/// the refresh already ignores `GTUNNEL_*` entries when it compares shapes, so
/// a version bump alone never makes a definition stale.
pub const DEFINITION_VERSION_VAR: &str = "GTUNNEL_DEFINITION_VERSION";

/// The version this binary stamps into what it writes
pub const DEFINITION_VERSION: &str = env!("CARGO_PKG_VERSION");

// A daemon does not inherit an interactive shell's PATH, so gcloud's directory
// is written into the unit. Without it the daemon dies at startup every time.
fn daemon_env_with(
    gcloud: &std::path::Path,
    get: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut path = String::new();
    if let Some(dir) = gcloud.parent() {
        path.push_str(&dir.to_string_lossy());
        path.push(':');
    }
    path.push_str("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    let mut env = vec![
        ("CLOUDSDK_CORE_DISABLE_PROMPTS".to_string(), "1".to_string()),
        ("PATH".to_string(), path),
        // gcloud is Python, and Python block-buffers a stdout that is not a
        // terminal. The service redirects stdout into the tunnel's log, so
        // without this a line is held until 4 KiB have accumulated or the
        // process exits, and a working tunnel reads as a silent one.
        ("PYTHONUNBUFFERED".to_string(), "1".to_string()),
        (
            DEFINITION_VERSION_VAR.to_string(),
            DEFINITION_VERSION.to_string(),
        ),
    ];
    for name in RESTART_VARS.into_iter().chain(FORWARDED_ENV_VARS) {
        if let Some(value) = get(name).filter(|v| !v.trim().is_empty()) {
            env.push((name.to_string(), value));
        }
    }
    env
}

/// The value an installed definition, in either format, holds for `name`.
/// `doctor` compares it with the shell's; reading it back through the same
/// escaping the writers use is what keeps a value with `&` or a space equal
/// to itself.
pub fn forwarded_in(text: &str, name: &str) -> Option<String> {
    launchd::plist_env_value(text, name).or_else(|| systemd::unit_env_value(text, name))
}

/// How many file descriptors a tunnel's service may hold open.
///
/// A launchd job inherits a soft `RLIMIT_NOFILE` of 256 (`launchctl limit
/// maxfiles`), and a forwarding tunnel spends one descriptor per connected
/// client. Left alone that is a ceiling on concurrent clients that nobody
/// chose: past it ssh's `accept()` returns EMFILE and the listener refuses
/// every new connection while the process stays alive, which is the one
/// failure launchd cannot see, because `KeepAlive` only fires on exit.
///
/// 4096 is far more than a tunnel needs and well under the kernel's own
/// `kern.maxfilesperproc` of 61440, so it is a raise that cannot be refused.
/// Only the soft limit is set: the hard limit is root's to raise, and a
/// plist asking for it fails to load rather than being ignored.
pub const TUNNEL_MAX_FILES: u64 = 4096;

/// The exit a supervisor ends with when gcloud died of expired credentials, on
/// a service manager that can be told not to restart it. 78 is `EX_CONFIG`:
/// the fault is in the environment, and only the user can fix it.
pub const AUTH_EXPIRED_EXIT: i32 = 78;

/// What the supervisor exits with after expired credentials. launchd has no
/// per-status exclusion: `KeepAlive` is `SuccessfulExit: false`, which
/// restarts on every non-zero exit, so the only exit it leaves alone is 0.
/// systemd names this code in `RestartPreventExitStatus` instead.
pub fn auth_expired_exit(launchd: bool) -> i32 {
    if launchd {
        0
    } else {
        AUTH_EXPIRED_EXIT
    }
}

/// Where a supervisor records that it stopped for expired credentials. Beside
/// the ready file, keyed by the same label.
pub fn auth_marker_path(label: &str) -> Result<PathBuf> {
    Ok(auth_marker_in(&crate::config::config_dir()?, label))
}

pub fn auth_marker_in(config_dir: &std::path::Path, label: &str) -> PathBuf {
    config_dir.join("run").join(format!("{label}.auth"))
}

/// What a supervisor stopped for, read from its marker. A marker that exists
/// always means stopped, so one that cannot be read is an expired login, which
/// is also what the markers written before slugs existed (`1`) said.
pub fn read_auth_marker(path: &std::path::Path) -> Option<crate::failure::Failure> {
    path.exists().then(|| {
        crate::failure::Failure::from_marker(&std::fs::read_to_string(path).unwrap_or_default())
    })
}

/// A deleted tunnel leaves nothing that a tunnel of the same name, added
/// next, could inherit.
pub fn remove_auth_marker_in(config_dir: &std::path::Path, label: &str) {
    let _ = std::fs::remove_file(auth_marker_in(config_dir, label));
}

/// The file a supervisor writes once its tunnel is listening. Named here so
/// the watchdog reads the one the installer chose.
pub fn ready_file_in(config_dir: &std::path::Path, label: &str) -> PathBuf {
    config_dir.join("run").join(format!("{label}.ready"))
}

/// Where a supervisor records that it stopped because its tunnel spent the
/// restart budget. Same directory and label as the auth marker, so one sweep
/// of `run/` finds both; the file holds the last exit reason for `list`.
pub fn crash_marker_path(label: &str) -> Result<PathBuf> {
    Ok(crash_marker_in(&crate::config::config_dir()?, label))
}

pub fn crash_marker_in(config_dir: &std::path::Path, label: &str) -> PathBuf {
    config_dir.join("run").join(format!("{label}.crash"))
}

/// The failure times a budget counts, kept beside its marker because the
/// service manager starts a new supervisor per attempt and the count has to
/// outlive each one. The marker's last segment is always `crash`, so swapping
/// it never eats part of a tunnel name.
pub fn restart_history_beside(marker: &std::path::Path) -> PathBuf {
    marker.with_extension("restarts")
}

/// When the current run of `remote port forwarding failed` exits began. Its own
/// file, because those exits are not counted in the history.
pub fn busy_beside(marker: &std::path::Path) -> PathBuf {
    marker.with_extension("busy")
}

/// How many runs in a row died of the network, beside the history. Kept apart
/// from it because these exits never spend the budget: an hour offline is a
/// long count here and an empty history there.
pub fn net_failures_beside(marker: &std::path::Path) -> PathBuf {
    marker.with_extension("netfails")
}

/// Forget a spent budget: the marker that says why the job is down and the
/// count that produced it. `start` and `delete` both want exactly this.
pub fn clear_crash_state(marker: &std::path::Path) {
    let _ = std::fs::remove_file(marker);
    let _ = std::fs::remove_file(restart_history_beside(marker));
    let _ = std::fs::remove_file(busy_beside(marker));
    let _ = std::fs::remove_file(net_failures_beside(marker));
}

/// Whether this tunnel's supervisor stopped on purpose for a spent budget.
pub fn crash_marked_in(config_dir: &std::path::Path, label: &str) -> bool {
    crash_marker_in(config_dir, label).exists()
}

/// The last exit reason a spent budget recorded, or `None` when the tunnel is
/// not parked. A marker with nothing readable in it is still a marker.
pub fn crash_reason_in(config_dir: &std::path::Path, label: &str) -> Option<String> {
    let text = std::fs::read_to_string(crash_marker_in(config_dir, label)).ok()?;
    let line = text.trim();
    Some(if line.is_empty() { "unknown" } else { line }.to_string())
}

/// The watchdog's `watch.interval` in whole seconds, as the unit it is
/// installed with spells it. Fixed at install: a change needs `watch install`.
pub(crate) fn watch_interval_secs() -> u64 {
    crate::settings::current().watch_interval.as_secs()
}

/// The watchdog is one job for the whole machine, not one per tunnel, so its
/// label carries no account or tunnel segment.
/// Each service manager owns its own spelling; this picks the host's.
pub fn watchdog_label() -> &'static str {
    if cfg!(target_os = "macos") {
        launchd::WATCHDOG_LABEL
    } else {
        systemd::WATCHDOG_LABEL
    }
}

// Both bounds hold at build time: above launchd's 256 default so the limit is
// a raise, and under macOS's `kern.maxfilesperproc` of 61440 so it cannot be
// refused. A later edit that breaks either fails the build, not a test run.
const _: () = assert!(TUNNEL_MAX_FILES > 256);
const _: () = assert!(TUNNEL_MAX_FILES <= 61440);

/// The definition `add`, `start` and `restart` write, from the live shell:
/// this is the one place the shell's gcloud, proxy and CA values are taken.
/// The refresh rewrites with [`keep_installed_forwarded`] on top, so a command
/// typed in another shell cannot change what a running tunnel was set up with.
pub fn service_spec(cfg: &Config, t: &PersistentTunnel) -> Result<ServiceSpec> {
    service_spec_with(cfg, t, &|name| std::env::var(name).ok())
}

/// Puts the forwarded values `installed` already holds into `spec` in place
/// of the ones the current shell supplied, and drops the ones it lacks. The
/// refresh rewrites a definition for a reason that has nothing to do with the
/// proxy, and must not carry the refreshing shell's into a job that was set up
/// with others. `gtunnel restart` is how a new value is taken.
pub fn keep_installed_forwarded(spec: &mut ServiceSpec, installed: &str) {
    spec.env
        .retain(|(k, _)| !FORWARDED_ENV_VARS.contains(&k.as_str()));
    for name in FORWARDED_ENV_VARS {
        if let Some(value) = forwarded_in(installed, name) {
            spec.env.push((name.to_string(), value));
        }
    }
}

fn service_spec_with(
    cfg: &Config,
    t: &PersistentTunnel,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<ServiceSpec> {
    let acct = cfg.get_account(Some(&t.account_name))?;
    // Before gcloud is looked for: a relative path would be written into a
    // service file, where it resolves against nothing
    acct.credential_file_for_use()?;
    let gcloud = crate::gcp::gcloud::require_gcloud()?;
    // From the config in hand, not from startup: this argv is about to be
    // written into a service file that outlives the command writing it, and
    // the TUI reinstalls one hours into a session, after the config page may
    // have changed the level.
    let level = crate::loglevel::effective(cfg.log_level);
    let gcloud_args = match &t.spec {
        TunnelSpec::Access { .. } => tunnel::access_argv(acct, t, level)?,
        TunnelSpec::Expose { .. } => {
            let ex = acct.expose.as_ref().with_context(|| {
                format!(
                    "Account '{}' has no relay. Run `gtunnel relay up`",
                    acct.name
                )
            })?;
            tunnel::expose_argv(acct, ex, t, level)?
        }
    };
    let program = crate::update::exe_path()?;
    let label = platform_label(&t.account_name, &t.name);
    // The level the gcloud argv above was built for, so the supervisor decides
    // about connection lines from the same answer instead of re-reading a
    // config that may have changed since this file was written
    let mut args = vec![
        "__supervise".to_string(),
        "--log-level".to_string(),
        level.to_string(),
    ];
    // Only an access tunnel has a local port to probe and to wait for
    if let TunnelSpec::Access {
        local_port, target, ..
    } = &t.spec
    {
        // A jump tunnel's stderr also carries the IAP transport ssh rides on,
        // which is one long connection and not a client
        args.push("--conns".to_string());
        args.push(
            match target {
                crate::state::AccessTarget::Jump { .. } => "ssh",
                _ => "iap",
            }
            .to_string(),
        );
        args.push("--port".to_string());
        args.push(local_port.to_string());
        args.push("--ready-file".to_string());
        args.push(
            ready_file_in(&crate::config::config_dir()?, &label)
                .to_string_lossy()
                .to_string(),
        );
    }
    // Every tunnel can lose its login, an expose tunnel as much as an access one
    args.push("--auth-marker".to_string());
    args.push(auth_marker_path(&label)?.to_string_lossy().to_string());
    args.push("--crash-marker".to_string());
    args.push(crash_marker_path(&label)?.to_string_lossy().to_string());
    args.push("--".to_string());
    args.push(gcloud.to_string_lossy().to_string());
    args.extend(gcloud_args);

    Ok(ServiceSpec {
        label,
        program,
        args,
        log_path: t.log_path()?,
        env: daemon_env_with(&gcloud, get),
        auto_start: t.auto_start,
        description: format!("{} -> {}", t.name, t.display_target()),
    })
}

#[cfg(target_os = "macos")]
pub fn platform_label(account: &str, name: &str) -> String {
    launchd::label_for(account, name)
}

/// The text of the service file gtunnel installed for `label`, or `None` when
/// there is none. Existence of the file is what "installed" means: a service
/// manager can be asked whether a job is loaded, but a stopped tunnel is
/// still installed and still carries the argv it was installed with.
pub fn installed_service_text(label: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    let path = launchd::plist_path(label).ok()?;
    #[cfg(not(target_os = "macos"))]
    let path = systemd::unit_path(label).ok()?;
    std::fs::read_to_string(path).ok()
}

/// The text of the watchdog's definition as installed, or `None` when it was
/// never installed. A unit watchdog is two files and the interval lives in the
/// timer, so both are read and joined: the refresh compares the whole of what
/// `watch install` wrote.
pub fn installed_watchdog_text() -> Option<String> {
    let label = watchdog_label();
    #[cfg(target_os = "macos")]
    return installed_service_text(label);
    #[cfg(not(target_os = "macos"))]
    {
        let service = installed_service_text(&format!("{label}.service"))?;
        let timer = installed_service_text(&format!("{label}.timer")).unwrap_or_default();
        Some(format!("{service}\n{timer}"))
    }
}

/// What `watch install` would write for `program`, in the shape
/// [`installed_watchdog_text`] reads back.
pub fn render_watchdog(program: &std::path::Path) -> Result<String> {
    let log = watchdog_log()?;
    let interval = watch_interval_secs();
    #[cfg(target_os = "macos")]
    return Ok(launchd::render_watchdog_plist(program, &log, interval));
    #[cfg(not(target_os = "macos"))]
    {
        let (service, timer) = systemd::render_watchdog_units(program, &log, interval);
        Ok(format!("{service}\n{timer}"))
    }
}

/// Where a refresh records that it rewrote a definition and has not yet
/// restarted the job. Beside the other run markers, keyed by the same label,
/// so one sweep of `run/` finds all of them.
pub fn restart_pending_in(config_dir: &std::path::Path, label: &str) -> PathBuf {
    config_dir
        .join("run")
        .join(format!("{label}.restart-pending"))
}

/// Whether `path` is inside `dir`, compared by component so `/tmp/cfg-ab` is
/// not inside `/tmp/cfg-a`. A `..` anywhere refuses: it would pass the prefix
/// test and then resolve somewhere else.
pub fn is_within(path: &std::path::Path, dir: &std::path::Path) -> bool {
    path.starts_with(dir)
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

#[cfg(not(target_os = "macos"))]
pub fn platform_label(account: &str, name: &str) -> String {
    systemd::unit_for(account, name)
}

// The platform backend every command calls; consumers land in Task 10
#[allow(unused_imports)]
#[cfg(target_os = "macos")]
pub use launchd::{
    install, install_watchdog, restart, runtime_info, set_auto_start, start, status,
    status_checked, stop, uninstall, uninstall_watchdog, watchdog_installed, watchdog_log,
    write_definition,
};

#[allow(unused_imports)]
#[cfg(not(target_os = "macos"))]
pub use systemd::{
    install, install_watchdog, restart, runtime_info, set_auto_start, start, status,
    status_checked, stop, uninstall, uninstall_watchdog, watchdog_installed, watchdog_log,
    write_definition,
};

/// Read the answer of a service manager call, telling "the manager said no"
/// from "the manager never answered".
///
/// A failed exit is `launchctl print` or `systemctl show` reporting that the
/// job is not loaded, and that is a real status. A spawn error or a timeout
/// is no answer at all, and `None` is how the watchdog avoids reading a slow
/// or wedged manager as a tunnel that stopped.
pub(crate) fn checked_status(
    out: Result<std::process::Output>,
    parse: impl Fn(&str) -> crate::state::TunnelStatus,
) -> Option<crate::state::TunnelStatus> {
    let out = out.ok()?;
    if out.status.success() {
        Some(parse(&String::from_utf8_lossy(&out.stdout)))
    } else {
        Some(crate::state::TunnelStatus::Stopped)
    }
}

#[cfg(test)]
mod checked_status_tests {
    use super::checked_status;
    use crate::state::TunnelStatus;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    fn output(code: i32, stdout: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn parse(_: &str) -> TunnelStatus {
        TunnelStatus::Running
    }

    // `launchctl print` timing out says nothing about the job. Reading it as
    // Stopped reset the breaker and announced a stop that never happened.
    #[test]
    fn a_service_manager_call_that_errored_or_timed_out_is_not_a_status() {
        let timed_out = Err(anyhow::anyhow!("launchctl print timed out after 5s"));
        assert_eq!(checked_status(timed_out, parse), None);
        let missing_binary = Err(anyhow::anyhow!("Failed to run launchctl"));
        assert_eq!(checked_status(missing_binary, parse), None);
    }

    // The manager answering "not loaded" is an answer: the job is stopped
    #[test]
    fn a_service_the_manager_does_not_know_is_stopped() {
        assert_eq!(
            checked_status(Ok(output(113, "")), parse),
            Some(TunnelStatus::Stopped)
        );
    }

    #[test]
    fn a_service_the_manager_describes_is_read_by_the_parser() {
        assert_eq!(
            checked_status(Ok(output(0, "state = running")), parse),
            Some(TunnelStatus::Running)
        );
    }
}

// A tunnel that starts gets a clean log. Truncated in place rather than removed
// and rewritten, so a process still holding the file open in append mode keeps
// writing where the reader is looking.
pub fn reset_log(path: &std::path::Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("Failed to clear {}", path.display()))?;
    Ok(())
}

/// Clear a tunnel's log and name the tunnel on its first line. Install is the
/// one place that truncates, so it is also the one place that has to leave
/// something behind: an empty pane and a tunnel that was never installed looked
/// the same on screen.
pub fn write_log_header(spec: &ServiceSpec) -> Result<()> {
    reset_log(&spec.log_path)?;
    log_event(&spec.log_path, &spec.description)
}

// What a reader may take of a log to show its last lines. A line is rarely
// over 200 bytes even at ssh's DEBUG, so this window holds far more lines
// than any pane shows, and nothing above it could be on screen anyway.
pub const TAIL_WINDOW: u64 = 256 * 1024;

/// A file from `offset` to its end, whatever it opens on. Empty for a log
/// that is not there yet, which is what a tunnel that never started has.
/// Lossy on the decode: an offset can fall inside a multi-byte character,
/// and a log is not worth failing over.
pub fn read_from(path: &std::path::Path, offset: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    if offset > 0 && file.seek(SeekFrom::Start(offset)).is_err() {
        return String::new();
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&raw).into_owned()
}

/// What a follower prints next, and the offset to resume from. The offset is
/// the bytes this call read, never a length measured beforehand: a line
/// appended between the two would sit below the new offset and print twice.
/// A file shorter than `offset` was rewritten, so it is read from the start.
/// A character cut off at the end of what was read is left for the next call,
/// because its other half is in the next write and each half alone would
/// print as U+FFFD.
pub fn read_follow(path: &std::path::Path, offset: u64) -> (String, u64) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return (String::new(), offset);
    };
    let from = match file.metadata() {
        Ok(meta) if meta.len() < offset => 0,
        Ok(_) => offset,
        Err(_) => return (String::new(), offset),
    };
    if from > 0 && file.seek(SeekFrom::Start(from)).is_err() {
        return (String::new(), offset);
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return (String::new(), offset);
    }
    raw.truncate(raw.len() - incomplete_tail(&raw));
    (
        String::from_utf8_lossy(&raw).into_owned(),
        from + raw.len() as u64,
    )
}

// How many trailing bytes start a UTF-8 sequence that has not finished. Only
// the last three can: a lead byte announces at most four, and a byte that is
// invalid anywhere else is not waiting for anything
fn incomplete_tail(raw: &[u8]) -> usize {
    for back in 1..=raw.len().min(3) {
        let byte = raw[raw.len() - back];
        let needed = match byte {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            // A continuation byte: keep looking for its lead
            0x80..=0xBF => continue,
            _ => return 0,
        };
        return if needed > back { back } else { 0 };
    }
    0
}

/// The last `max_bytes` of a file, from a line boundary.
pub fn read_tail(path: &std::path::Path, max_bytes: u64) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return String::new();
    };
    let from = meta.len().saturating_sub(max_bytes);
    let text = read_from(path, from);
    // Only a window that skipped something can open mid-line, and half a
    // line is noise. A whole file would otherwise lose its first line.
    match (from > 0).then(|| text.find('\n')).flatten() {
        Some(nl) => text[nl + 1..].to_string(),
        None => text,
    }
}

/// Past `cap`, rewrite the log keeping its last `keep` bytes from a line
/// boundary, with a first line saying what went. The service's child holds
/// this file open in append mode, so truncating in place is safe: its next
/// write lands at the new end. A line it writes between the read and the
/// truncation is lost, which is the price of bounding a file whose main
/// writer is somebody else's process.
///
/// Only a gtunnel process calls this, so a tunnel nobody looks at is bounded
/// the next time one does. `docs/troubleshooting.md` says so.
pub fn trim_log(path: &std::path::Path, cap: u64, keep: u64) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("Failed to open {}", path.display())),
    };
    let size = file
        .metadata()
        .with_context(|| format!("Failed to stat {}", path.display()))?
        .len();
    if size <= cap {
        return Ok(());
    }
    file.seek(SeekFrom::Start(size.saturating_sub(keep)))
        .with_context(|| format!("Failed to seek {}", path.display()))?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    // Lossy on purpose: the window opens at a byte offset, so it can open
    // inside a multi-byte character, and a log is not worth failing over
    let mut tail = String::from_utf8_lossy(&raw).into_owned();
    // That same offset almost always opens mid-line, and half a line is noise
    if let Some(nl) = tail.find('\n') {
        tail = tail[nl + 1..].to_string();
    }
    let kib = cap / 1024;
    let body = format!("gtunnel: earlier lines dropped here, the log passed {kib} KiB\n{tail}");
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("Failed to rewind {}", path.display()))?;
    file.write_all(body.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;
    file.set_len(body.len() as u64)
        .with_context(|| format!("Failed to truncate {}", path.display()))
}

/// Append one gtunnel line to a tunnel's log. The daemon's stdout is redirected
/// into the same file, so both writers append and neither rewrites the other;
/// one `write_all` of one whole line keeps a gtunnel line from landing inside a
/// gcloud one. A `--via` tunnel prints nothing while it is healthy, so without
/// these lines its log is indistinguishable from a tunnel that never ran.
pub fn log_event(path: &std::path::Path, message: &str) -> Result<()> {
    use std::io::Write;
    // A full disk is a worse failure than a short history, and a log this
    // talkative gets there. A trim that fails changes nothing and is not
    // worth refusing the line over.
    let limits = crate::settings::current();
    let _ = trim_log(path, limits.logs_max_size, limits.logs_keep_size);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    // A name carrying a newline would otherwise forge a second line
    let message: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let stamp = humantime::format_rfc3339_seconds(std::time::SystemTime::now());
    let line = format!("{stamp} gtunnel: {message}\n");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?
        .write_all(line.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Delete a tunnel's log. A tunnel that no longer exists owns nothing, and a
/// name reused later would otherwise open on the previous tunnel's lines.
pub fn remove_log(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

#[cfg(test)]
#[allow(clippy::print_stdout, clippy::print_stderr)]
mod tests {
    use std::io::Write;

    #[test]
    fn starting_a_tunnel_forgets_the_budget_the_network_count_and_the_busy_streak() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.crash");
        let files = [
            marker.clone(),
            super::restart_history_beside(&marker),
            super::net_failures_beside(&marker),
            super::busy_beside(&marker),
        ];
        for f in &files {
            std::fs::write(f, "1\n").expect("write");
        }
        super::clear_crash_state(&marker);
        for f in &files {
            assert!(!f.exists(), "{} survived", f.display());
        }
    }

    fn append(path: &std::path::Path, bytes: &[u8]) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    // The offset is what was read, not the length measured before reading, so
    // a line appended in between is neither skipped nor printed twice
    #[test]
    fn a_followed_log_prints_each_line_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        append(&path, b"one\n");
        let (text, offset) = read_follow(&path, 0);
        assert_eq!((text.as_str(), offset), ("one\n", 4));

        append(&path, b"two\n");
        append(&path, b"three\n");
        let (text, offset) = read_follow(&path, offset);
        assert_eq!((text.as_str(), offset), ("two\nthree\n", 14));

        let (text, offset) = read_follow(&path, offset);
        assert_eq!(
            (text.as_str(), offset),
            ("", 14),
            "nothing is printed again"
        );
    }

    #[test]
    fn a_character_split_across_two_reads_prints_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        // U+00E9 is two bytes: the first lands before the second
        append(&path, b"caf\xC3");
        let (text, offset) = read_follow(&path, 0);
        assert_eq!((text.as_str(), offset), ("caf", 3), "the half waits");

        append(&path, b"\xA9\n");
        let (text, offset) = read_follow(&path, offset);
        assert_eq!((text.as_str(), offset), ("\u{e9}\n", 6));
    }

    #[test]
    fn a_truncated_log_is_followed_from_its_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        append(&path, b"a long first line\n");
        let (_, offset) = read_follow(&path, 0);
        std::fs::write(&path, "new\n").unwrap();
        let (text, offset) = read_follow(&path, offset);
        assert_eq!((text.as_str(), offset), ("new\n", 4));
    }

    #[test]
    fn a_byte_that_is_not_utf8_is_replaced_not_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        append(&path, b"a\xFFb\n");
        let (text, offset) = read_follow(&path, 0);
        assert_eq!((text.as_str(), offset), ("a\u{fffd}b\n", 4));
    }

    use super::*;
    use crate::state::TunnelStatus;

    const WATCHDOG_INTERVAL: u64 = 30;

    // The level in a service file has to come from the config being installed
    // from, not from whatever the config said when the process started. A TUI
    // session runs for hours and reinstalls a service after its config page
    // has been edited; before this, that edit was written into `config.toml`
    // and then ignored by the very next restart.
    #[test]
    fn a_service_spec_takes_its_level_from_the_config_it_is_handed() {
        if crate::gcp::gcloud::require_gcloud().is_err() {
            eprintln!("no gcloud on this machine; skipping");
            return;
        }
        let tunnel = crate::state::PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        };
        for level in [
            crate::loglevel::LogLevel::Quiet,
            crate::loglevel::LogLevel::Info,
            crate::loglevel::LogLevel::Debug,
        ] {
            let cfg = Config {
                selected_account: "dev".to_string(),
                log_level: level,
                accounts: vec![crate::config::Account {
                    name: "dev".to_string(),
                    gcloud_configuration: "work".to_string(),
                    project: "proj-1".to_string(),
                    default_zone: "europe-west1-b".to_string(),
                    impersonate_service_account: None,
                    credential_file: None,
                    expose: None,
                }],
                ..Config::default()
            };
            let got = service_spec(&cfg, &tunnel).expect("spec");
            let want = format!("--verbosity={}", level.gcloud_verbosity());
            assert!(got.args.contains(&want), "{level}: {:?}", got.args);
        }
    }

    // The service argv is built from the same gcloud argv, so the flag has to
    // reach the plist or unit without any environment plumbing
    #[test]
    fn a_service_spec_carries_the_credential_file_flag() {
        if crate::gcp::gcloud::require_gcloud().is_err() {
            eprintln!("no gcloud on this machine; skipping");
            return;
        }
        let tunnel = crate::state::PersistentTunnel {
            name: "db".to_string(),
            account_name: "ci".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        };
        let cfg = Config {
            selected_account: "ci".to_string(),
            accounts: vec![crate::config::Account {
                name: "ci".to_string(),
                gcloud_configuration: "ci".to_string(),
                project: "example-dev-1a2b".to_string(),
                default_zone: "europe-west1-b".to_string(),
                impersonate_service_account: None,
                credential_file: Some("/secrets/ci-key.json".into()),
                expose: None,
            }],
            ..Config::default()
        };
        let got = service_spec(&cfg, &tunnel).expect("spec");
        assert!(
            got.args
                .contains(&"--credential-file-override=/secrets/ci-key.json".to_string()),
            "{:?}",
            got.args
        );
    }

    // Refused where the file is used, for the one account, before gcloud is even
    // looked for: the other accounts of the same config keep installing
    #[test]
    fn a_service_spec_refuses_a_relative_credential_file_naming_the_account() {
        let tunnel = crate::state::PersistentTunnel {
            name: "db".to_string(),
            account_name: "ci".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        };
        let cfg = Config {
            selected_account: "ci".to_string(),
            accounts: vec![crate::config::Account {
                name: "ci".to_string(),
                gcloud_configuration: "ci".to_string(),
                project: "example-dev-1a2b".to_string(),
                default_zone: "europe-west1-b".to_string(),
                impersonate_service_account: None,
                credential_file: Some("~/ci-key.json".into()),
                expose: None,
            }],
            ..Config::default()
        };
        let err = service_spec(&cfg, &tunnel).expect_err("relative");
        let text = format!("{err:#}");
        assert!(text.contains("'ci'") && text.contains("absolute"), "{text}");
        assert_eq!(
            crate::error::kind_of(&err),
            crate::error::ErrorKind::Usage,
            "{text}"
        );
    }

    // The file belongs to one account: a tunnel of another account, in the same
    // config, must not inherit the key
    #[test]
    fn a_service_spec_of_another_account_lacks_the_credential_file_flag() {
        if crate::gcp::gcloud::require_gcloud().is_err() {
            eprintln!("no gcloud on this machine; skipping");
            return;
        }
        let tunnel = crate::state::PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        };
        let account = |name: &str, file: Option<&str>| crate::config::Account {
            name: name.to_string(),
            gcloud_configuration: name.to_string(),
            project: "example-dev-1a2b".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: file.map(Into::into),
            expose: None,
        };
        let cfg = Config {
            selected_account: "ci".to_string(),
            accounts: vec![
                account("ci", Some("/secrets/ci-key.json")),
                account("dev", None),
            ],
            ..Config::default()
        };
        let got = service_spec(&cfg, &tunnel).expect("spec");
        assert!(
            !got.args
                .iter()
                .any(|a| a.starts_with("--credential-file-override")),
            "{:?}",
            got.args
        );
    }

    // The supervisor decides about connection lines and its ready file from
    // these flags; an access tunnel carries its port, an expose tunnel has none
    #[test]
    fn a_service_spec_hands_the_supervisor_its_level_port_and_ready_file() {
        if crate::gcp::gcloud::require_gcloud().is_err() {
            eprintln!("no gcloud on this machine; skipping");
            return;
        }
        let tunnel = crate::state::PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        };
        let cfg = Config {
            selected_account: "dev".to_string(),
            log_level: crate::loglevel::LogLevel::Debug,
            accounts: vec![crate::config::Account {
                name: "dev".to_string(),
                gcloud_configuration: "work".to_string(),
                project: "proj-1".to_string(),
                default_zone: "europe-west1-b".to_string(),
                impersonate_service_account: None,
                credential_file: None,
                expose: None,
            }],
            ..Config::default()
        };
        let got = service_spec(&cfg, &tunnel).expect("spec");
        let a = &got.args;
        assert_eq!(a[0], "__supervise");
        let flag = |name: &str| a.iter().position(|x| x == name).map(|i| a[i + 1].clone());
        assert_eq!(flag("--log-level").as_deref(), Some("debug"));
        assert_eq!(flag("--port").as_deref(), Some("15432"));
        assert_eq!(flag("--conns").as_deref(), Some("iap"));
        let ready = flag("--ready-file").expect("ready file");
        assert!(
            ready.ends_with(&format!("run/{}.ready", got.label)),
            "{ready}"
        );
        let dashes = a.iter().position(|x| x == "--").expect("separator");
        assert!(dashes > a.iter().position(|x| x == "--ready-file").unwrap());

        let jump = crate::state::PersistentTunnel {
            spec: TunnelSpec::Access {
                target: crate::state::AccessTarget::Jump {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                    host: "db.internal".to_string(),
                },
                remote_port: 5432,
                local_port: 15433,
            },
            ..tunnel
        };
        let got = service_spec(&cfg, &jump).expect("jump spec");
        let i = got.args.iter().position(|x| x == "--conns").expect("conns");
        assert_eq!(got.args[i + 1], "ssh");
    }

    fn spec() -> ServiceSpec {
        ServiceSpec {
            label: "com.gtunnel.dev.db".to_string(),
            program: std::path::PathBuf::from("/opt/homebrew/bin/gcloud"),
            args: vec![
                "compute".to_string(),
                "start-iap-tunnel".to_string(),
                "my instance".to_string(),
                "5432".to_string(),
                "--local-host-port=localhost:15432".to_string(),
            ],
            log_path: std::path::PathBuf::from("/tmp/gtunnel/logs/dev-db.log"),
            env: vec![
                ("CLOUDSDK_CORE_DISABLE_PROMPTS".to_string(), "1".to_string()),
                (
                    "PATH".to_string(),
                    "/opt/homebrew/bin:/usr/bin:/bin".to_string(),
                ),
            ],
            auto_start: false,
            description: "db -> postgres-1:5432 (europe-west1-b)".to_string(),
        }
    }

    // A launchd job inherits a soft RLIMIT_NOFILE of 256 unless its plist
    // raises it, and a forwarding tunnel spends one descriptor per client.
    // That made 256 a ceiling on concurrent clients that nobody chose and
    // nothing documented: past it ssh's accept() returns EMFILE and the
    // listener refuses every new connection while staying alive, so launchd,
    // which only restarts on exit, never notices.
    #[test]
    fn a_tunnel_is_not_capped_at_launchds_256_descriptors() {
        let plist = launchd::render_plist(&spec());
        assert!(plist.contains("<key>SoftResourceLimits</key>"), "{plist}");
        assert!(plist.contains("<key>NumberOfFiles</key>"), "{plist}");
        assert!(
            plist.contains(&format!("<integer>{TUNNEL_MAX_FILES}</integer>")),
            "{plist}"
        );
    }

    // Only the soft limit: raising the hard limit is root's to do, and a
    // plist that asks for it fails to load rather than being ignored.
    #[test]
    fn a_tunnel_does_not_ask_for_a_hard_limit_it_cannot_have() {
        let plist = launchd::render_plist(&spec());
        assert!(!plist.contains("HardResourceLimits"), "{plist}");
    }

    #[test]
    fn deleting_a_tunnel_removes_its_auth_marker_so_a_rename_starts_clean() {
        let dir = tempfile::tempdir().unwrap();
        let label = "com.gtunnel.dev.db";
        let marker = auth_marker_in(dir.path(), label);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "1\n").unwrap();
        let other = auth_marker_in(dir.path(), "com.gtunnel.dev.cache");
        std::fs::write(&other, "1\n").unwrap();
        remove_auth_marker_in(dir.path(), label);
        assert!(!marker.exists());
        assert!(other.exists(), "only this tunnel's marker goes");
        // Nothing to remove is not an error
        remove_auth_marker_in(dir.path(), label);
    }

    #[test]
    fn a_unit_does_not_restart_after_expired_credentials() {
        let unit = systemd::render_unit(&spec());
        assert!(
            unit.contains(&format!("RestartPreventExitStatus={AUTH_EXPIRED_EXIT}")),
            "{unit}"
        );
    }

    #[test]
    fn the_exit_for_expired_credentials_depends_on_how_the_manager_restarts() {
        // launchd restarts on any non-zero exit, so only 0 stops it
        assert_eq!(auth_expired_exit(true), 0);
        assert_eq!(auth_expired_exit(false), AUTH_EXPIRED_EXIT);
    }

    #[test]
    fn a_tunnel_unit_is_not_capped_at_the_default_descriptors() {
        let unit = systemd::render_unit(&spec());
        assert!(
            unit.contains(&format!("LimitNOFILE={TUNNEL_MAX_FILES}")),
            "{unit}"
        );
    }

    // The watchdog is not a tunnel. It wakes, looks at every tunnel, and
    // exits; the interval is what makes it a watchdog at all
    #[test]
    fn the_watchdog_runs_every_thirty_seconds_and_at_load() {
        let plist = launchd::render_watchdog_plist(
            std::path::Path::new("/usr/local/bin/gtunnel"),
            std::path::Path::new("/tmp/watchdog.log"),
            WATCHDOG_INTERVAL,
        );
        assert!(
            plist.contains(&format!(
                "<key>StartInterval</key>\n    <integer>{WATCHDOG_INTERVAL}</integer>"
            )),
            "{plist}"
        );
        assert!(plist.contains("<key>RunAtLoad</key>"), "{plist}");
    }

    // `watch.interval` is the one value the unit carries, so the unit must
    // take it from the caller and not from a constant
    #[test]
    fn the_watchdog_units_carry_the_interval_they_are_given() {
        let program = std::path::Path::new("/usr/local/bin/gtunnel");
        let log = std::path::Path::new("/tmp/watchdog.log");
        let plist = launchd::render_watchdog_plist(program, log, 45);
        assert!(plist.contains("<integer>45</integer>"), "{plist}");
        let (_, timer) = systemd::render_watchdog_units(program, log, 45);
        assert!(timer.contains("OnBootSec=45s"), "{timer}");
        assert!(timer.contains("OnUnitActiveSec=45s"), "{timer}");
    }

    #[test]
    fn the_watchdog_runs_the_gtunnel_binary_with_watch_once() {
        let plist = launchd::render_watchdog_plist(
            std::path::Path::new("/usr/local/bin/gtunnel"),
            std::path::Path::new("/tmp/watchdog.log"),
            WATCHDOG_INTERVAL,
        );
        assert!(
            plist.contains("<string>/usr/local/bin/gtunnel</string>"),
            "{plist}"
        );
        assert!(plist.contains("<string>watch</string>"), "{plist}");
        assert!(plist.contains("<string>--once</string>"), "{plist}");
        assert!(plist.contains("com.gtunnel.watchdog"), "{plist}");
    }

    // A KeepAlive job that exits immediately is a job launchd runs in a
    // tight loop. The watchdog exits every time, on purpose
    #[test]
    fn the_watchdog_is_an_interval_job_rather_than_a_keepalive_one() {
        let plist = launchd::render_watchdog_plist(
            std::path::Path::new("/usr/local/bin/gtunnel"),
            std::path::Path::new("/tmp/watchdog.log"),
            WATCHDOG_INTERVAL,
        );
        assert!(!plist.contains("KeepAlive"), "{plist}");
    }

    #[test]
    fn the_watchdog_timer_fires_on_the_same_interval() {
        let (service, timer) = systemd::render_watchdog_units(
            std::path::Path::new("/usr/local/bin/gtunnel"),
            std::path::Path::new("/tmp/watchdog.log"),
            WATCHDOG_INTERVAL,
        );
        assert!(service.contains("Type=oneshot"), "{service}");
        assert!(service.contains("watch --once"), "{service}");
        assert!(
            timer.contains(&format!("OnUnitActiveSec={WATCHDOG_INTERVAL}s")),
            "{timer}"
        );
        assert!(timer.contains("[Timer]"), "{timer}");
    }

    // Each renderer writes a file for its own service manager, so the label it
    // embeds is that manager's, never the one the host happens to use. Reading
    // it from `watchdog_label` made the plist carry `gtunnel-watchdog` when the
    // tests ran on Linux, and the timer point at `com.gtunnel.watchdog.service`
    // when they ran on macOS - a unit naming a service that does not exist
    #[test]
    fn each_renderer_carries_its_own_service_managers_label() {
        let program = std::path::Path::new("/usr/local/bin/gtunnel");
        let log = std::path::Path::new("/tmp/watchdog.log");

        let plist = launchd::render_watchdog_plist(program, log, WATCHDOG_INTERVAL);
        assert!(
            plist.contains("<string>com.gtunnel.watchdog</string>"),
            "{plist}"
        );

        let (_, timer) = systemd::render_watchdog_units(program, log, WATCHDOG_INTERVAL);
        assert!(timer.contains("Unit=gtunnel-watchdog.service"), "{timer}");
    }

    // Review Focus 1: a daemon that cannot find gcloud must fail loudly, not
    // silently look healthy, so PATH and the restart policy are part of the unit
    #[test]
    fn plist_carries_label_args_path_and_restart_policy() {
        let plist = launchd::render_plist(&spec());
        assert!(
            plist.contains("<string>com.gtunnel.dev.db</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/opt/homebrew/bin/gcloud</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>start-iap-tunnel</string>"),
            "{plist}"
        );
        assert!(plist.contains("<string>my instance</string>"), "{plist}");
        assert!(plist.contains("<key>PATH</key>"), "{plist}");
        assert!(
            plist.contains("<string>/opt/homebrew/bin:/usr/bin:/bin</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<key>CLOUDSDK_CORE_DISABLE_PROMPTS</key>"),
            "{plist}"
        );
        assert!(plist.contains("<key>SuccessfulExit</key>"), "{plist}");
        assert!(plist.contains("<false/>"), "{plist}");
        assert!(plist.contains("<key>ThrottleInterval</key>"), "{plist}");
        assert!(
            plist.contains("<string>/tmp/gtunnel/logs/dev-db.log</string>"),
            "{plist}"
        );
    }

    // RunAtLoad no longer carries auto-start: KeepAlive overrides it, and the
    // plist is loaded only when the tunnel is meant to run
    #[test]
    fn a_loaded_plist_always_runs() {
        let mut s = spec();
        assert!(launchd::render_plist(&s).contains("<key>RunAtLoad</key>\n    <true/>"));
        s.auto_start = true;
        assert!(launchd::render_plist(&s).contains("<key>RunAtLoad</key>\n    <true/>"));
    }

    // KeepAlive{SuccessfulExit:false} treats a SIGTERM death as a failure, so
    // `launchctl kill` only bought ~10s before launchd brought the tunnel back
    // while `gtunnel stop` had already printed stopped
    #[test]
    fn stopping_unloads_the_job_so_keepalive_cannot_respawn_it() {
        assert_eq!(
            launchd::stop_args("gui/501", "com.gtunnel.dev.db"),
            vec!["bootout", "gui/501/com.gtunnel.dev.db"]
        );
    }

    #[test]
    fn starting_rebootstraps_a_job_that_stop_unloaded() {
        let (load, run) = launchd::start_args(
            "gui/501",
            "com.gtunnel.dev.db",
            "/Users/me/Library/LaunchAgents/com.gtunnel.dev.db.plist",
        );
        assert_eq!(
            load,
            vec![
                "bootstrap",
                "gui/501",
                "/Users/me/Library/LaunchAgents/com.gtunnel.dev.db.plist"
            ]
        );
        assert_eq!(run, vec!["kickstart", "gui/501/com.gtunnel.dev.db"]);
    }

    // Toggling auto-start is a file operation on the LaunchAgents copy. It used
    // to go through install(), which bounced a running tunnel and left the TUI
    // reporting a failed kickstart
    #[test]
    fn auto_start_on_writes_the_launch_agents_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = dir.path().join("LaunchAgents/com.gtunnel.dev.db.plist");
        let mut s = spec();
        s.auto_start = true;
        launchd::sync_agent_copy(&agent, &s).expect("sync");
        assert_eq!(
            std::fs::read_to_string(&agent).expect("agent copy"),
            launchd::render_plist(&s)
        );
    }

    #[test]
    fn auto_start_off_removes_the_launch_agents_copy_and_tolerates_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = dir.path().join("com.gtunnel.dev.db.plist");
        std::fs::write(&agent, "stale").expect("seed");
        let s = spec();
        launchd::sync_agent_copy(&agent, &s).expect("remove");
        assert!(!agent.exists());
        launchd::sync_agent_copy(&agent, &s).expect("already absent");
    }

    // The bootstrap that preceded a failed kickstart is the cause; kickstart
    // only says the job is not loaded
    #[test]
    fn a_failed_kickstart_names_the_bootstrap_that_preceded_it() {
        let msg = launchd::start_failure(
            "Bootstrap failed: 5: Input/output error",
            "Could not find service \"com.gtunnel.dev.db\" in domain for port",
        );
        assert!(
            msg.contains("launchctl kickstart failed: Could not find service"),
            "{msg}"
        );
        assert!(
            msg.contains("launchctl bootstrap failed: Bootstrap failed: 5"),
            "{msg}"
        );
    }

    #[test]
    fn a_silent_bootstrap_adds_nothing_to_a_kickstart_failure() {
        assert_eq!(
            launchd::start_failure("", "boom"),
            "launchctl kickstart failed: boom"
        );
    }

    // With KeepAlive set, launchd runs a job the moment it is loaded, whatever
    // RunAtLoad or Disabled say, and it loads everything in LaunchAgents at
    // login. So a plist kept there is a tunnel that always runs: the loaded
    // definition gtunnel starts and stops lives in its own directory, and the
    // LaunchAgents copy exists only when auto-start is on.
    #[test]
    fn only_an_auto_start_tunnel_puts_a_plist_in_launch_agents() {
        let canonical = launchd::plist_path("com.gtunnel.dev.db").expect("path");
        assert!(
            !canonical.starts_with(launchd::agents_dir().expect("agents")),
            "{}",
            canonical.display()
        );
        let agent = launchd::agent_path("com.gtunnel.dev.db").expect("path");
        assert!(
            agent.starts_with(launchd::agents_dir().expect("agents")),
            "{}",
            agent.display()
        );
        assert_eq!(canonical.file_name(), agent.file_name());
    }

    #[test]
    fn plist_escapes_xml_in_values() {
        let mut s = spec();
        s.args.push("a&b<c>".to_string());
        let plist = launchd::render_plist(&s);
        assert!(
            plist.contains("<string>a&amp;b&lt;c&gt;</string>"),
            "{plist}"
        );
        assert!(!plist.contains("a&b<c>"), "{plist}");
    }

    #[test]
    fn unit_quotes_arguments_and_sets_environment() {
        let unit = systemd::render_unit(&spec());
        assert!(
            unit.contains(
                "ExecStart=/opt/homebrew/bin/gcloud compute start-iap-tunnel \"my instance\" 5432"
            ),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=CLOUDSDK_CORE_DISABLE_PROMPTS=1"),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=PATH=/opt/homebrew/bin:/usr/bin:/bin"),
            "{unit}"
        );
        assert!(unit.contains("Restart=on-failure"), "{unit}");
        assert!(unit.contains("RestartSec=5"), "{unit}");
        assert!(
            unit.contains("StandardOutput=append:/tmp/gtunnel/logs/dev-db.log"),
            "{unit}"
        );
        assert!(
            unit.contains("StandardError=append:/tmp/gtunnel/logs/dev-db.log"),
            "{unit}"
        );
        assert!(unit.contains("WantedBy=default.target"), "{unit}");
    }

    // The fixture above carries a hand-written env, so it can only test the
    // renderer. What a tunnel actually runs with comes from `daemon_env`, and
    // that is what this pins.
    #[test]
    fn the_child_does_not_block_buffer_its_output_into_the_log() {
        let env = daemon_env_with(std::path::Path::new("/opt/homebrew/bin/gcloud"), &|_| None);
        assert!(
            env.iter().any(|(k, v)| k == "PYTHONUNBUFFERED" && v == "1"),
            "gcloud is Python and its stdout block-buffers into a redirected \
             file, so a tunnel's lines arrive 4 KiB at a time or at exit\n{env:?}"
        );
    }

    #[test]
    fn a_restart_variable_set_at_install_time_reaches_the_service() {
        let get = |name: &str| match name {
            "GTUNNEL_RESTART_WINDOW" => Some("20m".to_string()),
            "GTUNNEL_RESTART_MAX_RESTARTS" => Some("0".to_string()),
            "GTUNNEL_RESTART_MIN_UPTIME" => Some("  ".to_string()),
            "GTUNNEL_LOG_LEVEL" => Some("debug".to_string()),
            _ => None,
        };
        let env = daemon_env_with(std::path::Path::new("/opt/homebrew/bin/gcloud"), &get);
        let has = |k: &str, v: &str| env.iter().any(|(ek, ev)| ek == k && ev == v);
        assert!(has("GTUNNEL_RESTART_WINDOW", "20m"), "{env:?}");
        assert!(has("GTUNNEL_RESTART_MAX_RESTARTS", "0"), "{env:?}");
        assert!(
            !env.iter().any(|(k, _)| k == "GTUNNEL_RESTART_MIN_UPTIME"),
            "an empty value is unset"
        );
        assert!(
            !env.iter().any(|(k, _)| k == "GTUNNEL_LOG_LEVEL"),
            "only the restart variables travel"
        );
    }

    // Names only, never values: a proxy URL can carry a password.
    const FORWARDED: [&str; 12] = [
        "CLOUDSDK_CONFIG",
        "CLOUDSDK_PYTHON",
        "CLOUDSDK_PYTHON_SITEPACKAGES",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "NO_PROXY",
        "https_proxy",
        "http_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "REQUESTS_CA_BUNDLE",
        "CLOUDSDK_CORE_CUSTOM_CA_CERTS_FILE",
    ];

    #[test]
    fn the_gcloud_proxy_and_ca_environment_of_the_shell_reaches_the_service() {
        let gcloud = std::path::Path::new("/opt/homebrew/bin/gcloud");
        for name in FORWARDED {
            let value = format!("/value/for/{name}");
            let want = value.clone();
            let get = move |n: &str| (n == name).then(|| value.clone());
            let env = daemon_env_with(gcloud, &get);
            assert!(
                env.iter().any(|(k, v)| k == name && *v == want),
                "{name} is read from the shell when the definition is written\n{env:?}"
            );
        }
        assert_eq!(
            FORWARDED_ENV_VARS.len(),
            FORWARDED.len(),
            "no variable travels unlisted"
        );
    }

    #[test]
    fn a_forwarded_variable_that_is_empty_or_blank_is_left_out() {
        let gcloud = std::path::Path::new("/opt/homebrew/bin/gcloud");
        let get = |n: &str| match n {
            "HTTPS_PROXY" => Some(String::new()),
            "CLOUDSDK_CONFIG" => Some("   ".to_string()),
            _ => None,
        };
        let env = daemon_env_with(gcloud, &get);
        assert!(
            !env.iter()
                .any(|(k, _)| k == "HTTPS_PROXY" || k == "CLOUDSDK_CONFIG"),
            "{env:?}"
        );
    }

    #[test]
    fn a_plist_and_a_unit_rendered_with_cloudsdk_config_carry_it() {
        let gcloud = std::path::Path::new("/opt/homebrew/bin/gcloud");
        let get = |n: &str| match n {
            "CLOUDSDK_CONFIG" => Some("/home/you/second gcloud".to_string()),
            "HTTPS_PROXY" => Some("http://proxy.example:3128/?a=1&b=2%40x".to_string()),
            _ => None,
        };
        let mut s = spec();
        s.env = daemon_env_with(gcloud, &get);

        let plist = launchd::render_plist(&s);
        assert!(plist.contains("<key>CLOUDSDK_CONFIG</key>"), "{plist}");
        assert!(
            plist.contains("<string>/home/you/second gcloud</string>"),
            "{plist}"
        );
        assert!(plist.contains("a=1&amp;b=2%40x"), "{plist}");

        // A space would end the assignment, and a bare percent is a
        // specifier, so a proxy URL with an encoded password would not
        // arrive as written.
        let unit = systemd::render_unit(&s);
        assert!(
            unit.contains("Environment=\"CLOUDSDK_CONFIG=/home/you/second gcloud\"\n"),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=HTTPS_PROXY=http://proxy.example:3128/?a=1&b=2%%40x\n"),
            "{unit}"
        );
    }

    #[test]
    fn what_a_definition_forwards_reads_back_from_both_formats() {
        let gcloud = std::path::Path::new("/opt/homebrew/bin/gcloud");
        let get = |n: &str| match n {
            "CLOUDSDK_CONFIG" => Some("/home/you/second gcloud".to_string()),
            "HTTPS_PROXY" => Some("http://proxy.example:3128/?a=1&b=2%40x".to_string()),
            _ => None,
        };
        let mut s = spec();
        s.env = daemon_env_with(gcloud, &get);
        for text in [launchd::render_plist(&s), systemd::render_unit(&s)] {
            assert_eq!(
                forwarded_in(&text, "CLOUDSDK_CONFIG").as_deref(),
                Some("/home/you/second gcloud"),
                "{text}"
            );
            assert_eq!(
                forwarded_in(&text, "HTTPS_PROXY").as_deref(),
                Some("http://proxy.example:3128/?a=1&b=2%40x"),
                "{text}"
            );
            assert_eq!(forwarded_in(&text, "NO_PROXY"), None, "{text}");
        }
    }

    // `start` and `add` build the definition through `service_spec`, which
    // reads the live shell; the refresh does not. This holds the first half.
    #[test]
    fn a_service_spec_takes_the_shell_value_it_is_given() {
        if crate::gcp::gcloud::require_gcloud().is_err() {
            eprintln!("no gcloud on this machine; skipping");
            return;
        }
        let tunnel = crate::state::PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        };
        let cfg = Config {
            selected_account: "dev".to_string(),
            accounts: vec![crate::config::Account {
                name: "dev".to_string(),
                gcloud_configuration: "work".to_string(),
                project: "example-dev-1a2b".to_string(),
                default_zone: "europe-west1-b".to_string(),
                impersonate_service_account: None,
                credential_file: None,
                expose: None,
            }],
            ..Config::default()
        };
        let get = |n: &str| (n == "HTTPS_PROXY").then(|| "http://proxy.example:3128".to_string());
        let got = service_spec_with(&cfg, &tunnel, &get).expect("spec");
        assert!(
            got.env
                .iter()
                .any(|(k, v)| k == "HTTPS_PROXY" && v == "http://proxy.example:3128"),
            "{:?}",
            got.env
        );
    }

    #[test]
    fn launchctl_print_reports_running_crashed_and_stopped() {
        let running = r#"
	state = running
	pid = 4242
	runs = 1
"#;
        let (status, info) = launchd::parse_launchctl_print(running);
        assert_eq!(status, TunnelStatus::Running);
        assert_eq!(info.pid, Some(4242));
        assert_eq!(info.restarts, 0);

        let crashed = r#"
	state = not running
	runs = 4
	last exit code = 1
"#;
        let (status, info) = launchd::parse_launchctl_print(crashed);
        assert_eq!(status, TunnelStatus::Error);
        assert_eq!(info.pid, None);
        assert_eq!(info.restarts, 3);

        let stopped = r#"
	state = not running
	runs = 2
	last exit code = 0
"#;
        let (status, _) = launchd::parse_launchctl_print(stopped);
        assert_eq!(status, TunnelStatus::Stopped);

        let never_run = "	state = not running\n";
        let (status, info) = launchd::parse_launchctl_print(never_run);
        assert_eq!(status, TunnelStatus::Stopped);
        assert_eq!(info.restarts, 0);
    }

    // `launchctl print` names a sysexits code after the number. The whole value
    // failed to parse as an integer, so a tunnel that could not start read as
    // Stopped.
    #[test]
    fn a_named_launchd_exit_code_reads_as_an_error() {
        let named = "\tstate = not running\n\truns = 3\n\tlast exit code = 78: EX_CONFIG\n";
        let (status, info) = launchd::parse_launchctl_print(named);
        assert_eq!(status, TunnelStatus::Error);
        assert_eq!(info.restarts, 2);
    }

    #[test]
    fn any_number_then_text_exit_code_yields_the_number() {
        for code in [1, 2, 64, 70, 78, 126, 127, 255, -1] {
            for text in ["EX_CONFIG", "EX_USAGE", "some words here", "", "a: b"] {
                let text = format!("\tstate = not running\n\tlast exit code = {code}: {text}\n");
                let (status, _) = launchd::parse_launchctl_print(&text);
                assert_eq!(status, TunnelStatus::Error, "{text:?}");
            }
        }
        let (status, _) =
            launchd::parse_launchctl_print("\tstate = not running\n\tlast exit code = 0: x\n");
        assert_eq!(
            status,
            TunnelStatus::Stopped,
            "0 with a name is still clean"
        );
    }

    // Real `launchctl print` nests endpoint blocks whose own "state = active"
    // lines must not be mistaken for the job's state
    #[test]
    fn launchctl_print_ignores_nested_endpoint_state() {
        let real = "com.gtunnel.dev.db = {\n\tstate = running\n\truns = 1\n\tpid = 96316\n\tlast exit code = (never exited)\n\tendpoints = {\n\t\t\"x\" = {\n\t\t\tstate = active\n\t\t}\n\t}\n}\n";
        let (status, info) = launchd::parse_launchctl_print(real);
        assert_eq!(status, TunnelStatus::Running);
        assert_eq!(info.pid, Some(96316));
    }

    #[test]
    fn systemctl_show_reports_active_failed_and_inactive() {
        let active = "ActiveState=active\nMainPID=4242\nNRestarts=2\n";
        let (status, info) = systemd::parse_systemctl_show(active);
        assert_eq!(status, TunnelStatus::Running);
        assert_eq!(info.pid, Some(4242));
        assert_eq!(info.restarts, 2);

        let failed = "ActiveState=failed\nMainPID=0\nNRestarts=5\n";
        let (status, info) = systemd::parse_systemctl_show(failed);
        assert_eq!(status, TunnelStatus::Error);
        assert_eq!(info.pid, None);
        assert_eq!(info.restarts, 5);

        let inactive = "ActiveState=inactive\nMainPID=0\nNRestarts=0\n";
        let (status, _) = systemd::parse_systemctl_show(inactive);
        assert_eq!(status, TunnelStatus::Stopped);
    }

    #[test]
    fn stopping_a_unit_that_is_not_loaded_is_already_stopped() {
        assert!(systemd::stop_failure_is_already_stopped(
            "Failed to stop gtunnel-dev-db.service: Unit gtunnel-dev-db.service not loaded.\n"
        ));
        assert!(!systemd::stop_failure_is_already_stopped(
            "Failed to stop gtunnel-dev-db.service: Access denied\n"
        ));
    }

    // The joined "<account>-<name>" label cannot be split back apart
    // ("prod-eu-api" is ambiguous), so both parts stay separate
    #[test]
    fn service_labels_follow_the_platform_convention() {
        assert_eq!(launchd::label_for("dev", "db"), "com.gtunnel.dev.db");
        assert_eq!(
            launchd::label_for("prod-eu", "api"),
            "com.gtunnel.prod-eu.api"
        );
        assert_eq!(systemd::unit_for("dev", "db"), "gtunnel-dev-db.service");
        assert_eq!(
            systemd::unit_for("prod-eu", "api"),
            "gtunnel-prod-eu-api.service"
        );
    }
    #[test]
    fn the_log_a_plist_writes_to_is_read_back_unescaped() {
        let mut s = spec();
        s.log_path = std::path::PathBuf::from("/tmp/a&b/logs/dev-db.log");
        let plist = launchd::render_plist(&s);
        assert_eq!(
            launchd::plist_log_path(&plist),
            Some(std::path::PathBuf::from("/tmp/a&b/logs/dev-db.log"))
        );
        assert_eq!(launchd::plist_log_path("not a plist"), None);
    }

    #[test]
    fn the_log_a_unit_appends_to_is_read_back() {
        let unit = systemd::render_unit(&spec());
        assert_eq!(
            systemd::unit_log_path(&unit),
            Some(std::path::PathBuf::from("/tmp/gtunnel/logs/dev-db.log"))
        );
        assert_eq!(systemd::unit_log_path("[Service]\nType=simple\n"), None);
    }

    // The shape of `launchctl print gui/<uid>/<label>`: the `path` line names the
    // plist the loaded job came from, which is what says whose job it is
    fn print_loaded_from(path: &str) -> String {
        format!(
            "gui/501/com.gtunnel.dev.db = {{\n\tactive count = 1\n\tpath = {path}\n\ttype = LaunchAgent\n\tstate = running\n\n\tprogram = /opt/homebrew/bin/gcloud\n}}\n"
        )
    }

    #[test]
    fn a_job_loaded_from_this_config_dir_is_ours_to_boot_out() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        let print = print_loaded_from("/tmp/cfg-a/agents/com.gtunnel.dev.db.plist");
        assert!(launchd::loaded_job_is_ours(&print, cfg, None));
    }

    #[test]
    fn a_job_loaded_from_another_config_dir_is_not_ours() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        let print = print_loaded_from("/tmp/cfg-b/agents/com.gtunnel.dev.db.plist");
        assert!(!launchd::loaded_job_is_ours(&print, cfg, None));
    }

    #[test]
    fn a_job_loaded_from_launchagents_is_ours_only_when_that_copy_passed_ownership() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        let agent =
            std::path::Path::new("/Users/you/Library/LaunchAgents/com.gtunnel.dev.db.plist");
        let print = print_loaded_from(&agent.to_string_lossy());
        assert!(launchd::loaded_job_is_ours(&print, cfg, Some(agent)));
        assert!(!launchd::loaded_job_is_ours(&print, cfg, None));
    }

    #[test]
    fn a_job_loaded_from_the_config_dir_but_outside_its_agents_dir_is_not_ours() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        for path in [
            "/tmp/cfg-a/com.gtunnel.dev.db.plist",
            "/tmp/cfg-a/Library/LaunchAgents/com.gtunnel.dev.db.plist",
        ] {
            let print = print_loaded_from(path);
            assert!(!launchd::loaded_job_is_ours(&print, cfg, None), "{path}");
        }
    }

    #[test]
    fn a_label_that_is_not_loaded_has_nothing_to_boot_out() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        assert_eq!(
            launchd::boot_out_decision(None, cfg, None),
            launchd::BootOut::NotLoaded
        );
    }

    #[test]
    fn a_loaded_job_of_this_config_is_booted_out() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        let print = print_loaded_from("/tmp/cfg-a/agents/com.gtunnel.dev.db.plist");
        assert_eq!(
            launchd::boot_out_decision(Some(&print), cfg, None),
            launchd::BootOut::Bootout
        );
    }

    #[test]
    fn a_loaded_job_of_another_config_is_foreign_and_names_its_path() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        let path = "/tmp/cfg-b/agents/com.gtunnel.dev.db.plist";
        let print = print_loaded_from(path);
        assert_eq!(
            launchd::boot_out_decision(Some(&print), cfg, None),
            launchd::BootOut::Foreign(path.to_string())
        );
    }

    #[test]
    fn a_loaded_job_that_names_no_plist_is_foreign() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        assert!(matches!(
            launchd::boot_out_decision(Some("state = running\n"), cfg, None),
            launchd::BootOut::Foreign(_)
        ));
    }

    #[test]
    fn only_a_missing_service_is_not_loaded() {
        assert!(launchd::print_failure_is_not_loaded(
            "Could not find service \"com.gtunnel.dev.db\" in domain for user gui: 501\n"
        ));
        assert!(!launchd::print_failure_is_not_loaded(
            "Operation not permitted\n"
        ));
    }

    #[test]
    fn a_print_with_no_path_line_is_not_ours() {
        let cfg = std::path::Path::new("/tmp/cfg-a");
        assert!(!launchd::loaded_job_is_ours("state = running\n", cfg, None));
        assert!(!launchd::loaded_job_is_ours("", cfg, None));
    }

    // A tunnel that starts again starts its log again: the previous process's
    // gcloud tracebacks are not this one's, and a log that only ever grows
    // buries the line that matters under the ones that no longer do
    #[test]
    fn a_log_reset_empties_an_existing_log_and_creates_a_missing_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("dev-db.log");
        std::fs::write(&log, "ERROR: [243] Connection reset by peer\n").expect("write");

        reset_log(&log).expect("reset an existing log");
        assert_eq!(std::fs::read_to_string(&log).expect("read"), "");

        let fresh = dir.path().join("dev-new.log");
        reset_log(&fresh).expect("reset a log that does not exist yet");
        assert!(fresh.exists(), "a missing log is created, not an error");
    }

    // Truncation happens in place so a process already holding the file open in
    // append mode keeps writing to the same inode instead of into a deleted file
    #[test]
    fn a_log_reset_keeps_the_same_file_an_open_process_is_writing_to() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("dev-db.log");
        let mut held = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .expect("open");
        held.write_all(b"old line\n").expect("write");

        reset_log(&log).expect("reset");
        held.write_all(b"new line\n")
            .expect("the held handle still writes");

        let after = std::fs::read_to_string(&log).expect("read");
        assert!(after.contains("new line"), "{after:?}");
        assert!(!after.contains("old line"), "{after:?}");
    }

    // A deleted tunnel keeps nothing: its log outlived it, so a name reused
    // later opened on the previous tunnel's lines
    #[test]
    fn removing_a_log_leaves_no_file_and_forgives_a_missing_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("dev-db.log");
        std::fs::write(&log, "a line\n").expect("write");

        remove_log(&log).expect("remove an existing log");
        assert!(!log.exists());
        remove_log(&log).expect("a log that is already gone is not an error");
    }

    #[test]
    fn an_event_appends_one_line_and_creates_the_directory_it_needs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("nested/dev-db.log");
        log_event(&log, "starting dev-db -> 10.0.0.1:443").expect("first event");
        log_event(&log, "stopped by gtunnel").expect("second event");
        let text = std::fs::read_to_string(&log).expect("read");
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(
            text.lines()
                .next()
                .unwrap()
                .ends_with("gtunnel: starting dev-db -> 10.0.0.1:443"),
            "{text}"
        );
        assert!(text.ends_with('\n'), "{text}");
    }

    // The daemon appends too, so an event must never rewrite what is already there
    #[test]
    fn an_event_keeps_the_daemon_lines_already_in_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("dev-db.log");
        std::fs::write(&log, "Testing if tunnel connection works.\n").expect("seed");
        log_event(&log, "stopped by gtunnel").expect("event");
        let text = std::fs::read_to_string(&log).expect("read");
        assert!(
            text.starts_with("Testing if tunnel connection works.\n"),
            "{text}"
        );
        assert_eq!(text.lines().count(), 2, "{text}");
    }

    // An edit or a plain install truncates the log; without a header the pane
    // goes blank again and the tunnel looks like it was never there
    #[test]
    fn installing_leaves_the_tunnel_named_in_its_own_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = spec();
        s.log_path = dir.path().join("dev-db.log");
        s.description = "dev-db -> 10.0.0.1:443".to_string();
        std::fs::write(&s.log_path, "stale line from the last run\n").expect("seed");

        write_log_header(&s).expect("header");

        let text = std::fs::read_to_string(&s.log_path).expect("read");
        assert!(!text.contains("stale"), "{text}");
        assert!(text.contains("dev-db -> 10.0.0.1:443"), "{text}");
    }

    // Review Focus 3: a target carrying a newline must not forge a second line
    #[test]
    fn an_event_writes_one_line_however_the_message_is_shaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("dev-db.log");
        log_event(&log, "starting dev-db\nforged: line").expect("event");
        let text = std::fs::read_to_string(&log).expect("read");
        assert_eq!(text.lines().count(), 1, "{text}");
    }

    // These logs are now talkative - gcloud at --verbosity=info and ssh at
    // DEBUG - and nothing else bounds them. A tunnel left up for weeks, or
    // one restarting in a loop, would otherwise write until the disk said no.
    #[test]
    fn a_log_past_its_cap_keeps_its_tail_and_says_what_it_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dev-db.log");
        let mut body = String::new();
        for i in 0..2000 {
            body.push_str(&format!("line {i} aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"));
        }
        std::fs::write(&path, &body).expect("write");

        trim_log(&path, 4096, 1024).expect("trim");

        let after = std::fs::read_to_string(&path).expect("read");
        assert!(
            after.len() <= 1024 + 128,
            "the cap is the point; kept {} bytes",
            after.len()
        );
        assert!(
            after.ends_with("line 1999 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"),
            "the tail is the half worth keeping\n{after}"
        );
        assert!(
            after
                .lines()
                .next()
                .expect("a first line")
                .contains("earlier lines dropped"),
            "a reader who greps for something old must learn it was dropped, \
             not read silence\n{after}"
        );
        let first_kept = after.lines().nth(1).expect("a kept line");
        assert!(
            first_kept.starts_with("line "),
            "a window opening mid-line must drop that line, not keep half of it: \
             {first_kept:?}"
        );
    }

    // Below the cap there is nothing to do, and rewriting the file would race
    // the child's own appends for no reason
    #[test]
    fn a_log_under_its_cap_is_left_exactly_as_it_was() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dev-db.log");
        std::fs::write(&path, "one\ntwo\n").expect("write");
        trim_log(&path, 4096, 1024).expect("trim");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "one\ntwo\n");
    }

    // The pane reloads every 5 seconds and keeps 200 lines of a file that is
    // now allowed to reach 8 MiB. Reading the whole of it to show the end of
    // it costs the whole of it, every time.
    #[test]
    fn the_log_pane_reads_the_end_of_the_file_rather_than_all_of_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dev-db.log");
        let mut body = String::new();
        for i in 0..500 {
            body.push_str(&format!("line {i} bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"));
        }
        std::fs::write(&path, &body).expect("write");

        let tail = read_tail(&path, 512);

        assert!(tail.len() <= 512, "read {} bytes", tail.len());
        assert!(
            tail.ends_with("line 499 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"),
            "the end of the file is the part on screen\n{tail}"
        );
        assert!(
            tail.lines().next().expect("a line").starts_with("line "),
            "a window opening mid-line must drop that line, not show half of it"
        );
    }

    // A file smaller than the window is the common case, and it must come
    // back whole rather than with its first line eaten
    #[test]
    fn a_log_shorter_than_the_window_keeps_its_first_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dev-db.log");
        std::fs::write(&path, "one\ntwo\n").expect("write");
        assert_eq!(read_tail(&path, 512), "one\ntwo\n");
    }
}

// Live launchd lifecycle check, run by hand on macOS:
//   cargo test --features _live_daemon daemon::live -- --ignored --nocapture
#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::print_stdout, clippy::print_stderr)]
mod live {
    use super::*;
    use crate::state::TunnelStatus;

    #[tokio::test]
    #[ignore = "touches the real launchd domain"]
    async fn install_start_status_stop_uninstall() {
        let _temp = crate::config::testing::TempConfigDir::new();
        let dir = _temp.path();
        let spec = ServiceSpec {
            label: "com.gtunnel.test.sleep".to_string(),
            program: std::path::PathBuf::from("/bin/sleep"),
            args: vec!["600".to_string()],
            log_path: dir.join("logs/test-sleep.log"),
            env: vec![("PATH".to_string(), "/usr/bin:/bin".to_string())],
            auto_start: false,
            description: "sleep -> nothing at all".to_string(),
        };

        launchd::install(&spec).await.expect("install");
        println!(
            "plist written: {}",
            std::fs::read_to_string(
                dirs::home_dir()
                    .unwrap()
                    .join("Library/LaunchAgents/com.gtunnel.test.sleep.plist")
            )
            .unwrap_or_else(|e| format!("READ ERROR {e}"))
        );
        // SAFETY: getuid(2) takes no arguments, reads no caller memory and cannot fail.
        let uid = unsafe { libc::getuid() };
        let mut print = tokio::process::Command::new("launchctl");
        print.args(["print", &format!("gui/{uid}/com.gtunnel.test.sleep")]);
        let raw =
            crate::proc::output_within(print, crate::proc::service_bound(), "launchctl print")
                .await
                .unwrap();
        println!(
            "print rc={:?} stderr={}",
            raw.status.code(),
            String::from_utf8_lossy(&raw.stderr)
        );
        launchd::start(&spec.label).await.expect("start");
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let running = launchd::status(&spec.label).await;
        let info = launchd::runtime_info(&spec.label).await;
        println!("after start: status={running:?} info={info:?}");
        assert_eq!(running, TunnelStatus::Running);
        assert!(info.pid.is_some(), "a running job should report a pid");

        launchd::stop(&spec.label).await.expect("stop");
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        println!(
            "after stop: status={:?}",
            launchd::status(&spec.label).await
        );

        launchd::uninstall(&spec.label).await.expect("uninstall");
        assert_eq!(
            launchd::status(&spec.label).await,
            TunnelStatus::Stopped,
            "an uninstalled job should not be running"
        );
        println!("after uninstall: gone");
    }
}
