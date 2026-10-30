// The mirror of the waiver in systemd.rs: off macOS nothing reaches this
// module but its own tests, and `clippy -D warnings` reads the binary target,
// where a test is not a caller
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{watch_interval_secs, RuntimeInfo, ServiceSpec};
use crate::state::TunnelStatus;

pub fn label_for(account: &str, name: &str) -> String {
    format!("com.gtunnel.{account}.{name}")
}

// Everything launchd finds here is loaded at login, and a KeepAlive job runs
// the moment it is loaded, so a plist here IS auto-start
pub fn agents_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine the home directory")?;
    Ok(home.join("Library/LaunchAgents"))
}

// The definition gtunnel loads and unloads itself, out of launchd's reach
pub fn plist_path(label: &str) -> Result<PathBuf> {
    Ok(crate::config::config_dir()?
        .join("agents")
        .join(format!("{label}.plist")))
}

pub fn agent_path(label: &str) -> Result<PathBuf> {
    Ok(agents_dir()?.join(format!("{label}.plist")))
}

fn xml_unescape(s: &str) -> String {
    // `&amp;` last, or `&amp;lt;` would come back as `<`
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The string a plist's `<key>name</key>` is followed by
pub fn plist_env_value(text: &str, name: &str) -> Option<String> {
    let key = format!("<key>{}</key>", xml_escape(name));
    let mut lines = text.lines().map(str::trim);
    lines.find(|l| *l == key)?;
    let value = lines.next()?;
    Some(xml_unescape(
        value.strip_prefix("<string>")?.strip_suffix("</string>")?,
    ))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The file the plist sends the job's output to, which is how a plist says
/// whose it is: gtunnel writes it as `<config_dir>/logs/<label>.log`, so two
/// config directories never share one.
pub fn plist_log_path(text: &str) -> Option<PathBuf> {
    let after = text.split("<key>StandardOutPath</key>").nth(1)?;
    let value = after.split("<string>").nth(1)?.split("</string>").next()?;
    // The inverse of `xml_escape`, with `&amp;` last so `&amp;lt;` stays `&lt;`
    Some(PathBuf::from(
        value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    ))
}

/// Whether the job launchd has loaded under a label came from this config dir.
///
/// The `gui/<uid>` domain is shared by every config directory on the machine,
/// so a label proves nothing: a throwaway config and the real one can both
/// name `com.gtunnel.dev.db`. The `path` line of `launchctl print` says which
/// plist the loaded job came from; only that answers whose job it is.
/// `owned_agent` is the LaunchAgents copy, passed only when its own contents
/// already tied it to this config dir.
pub fn loaded_job_is_ours(
    print: &str,
    config_dir: &std::path::Path,
    owned_agent: Option<&std::path::Path>,
) -> bool {
    let Some(path) = print
        .lines()
        .find_map(|line| line.trim().strip_prefix("path = "))
    else {
        return false;
    };
    let path = std::path::Path::new(path.trim());
    // `agents/` and nothing wider: a config dir can be `$HOME`, which holds
    // `Library/LaunchAgents` and every other config dir's copies
    super::is_within(path, &config_dir.join("agents")) || owned_agent == Some(path)
}

/// What to do about the job launchd has loaded under an orphan's label.
#[derive(Debug, PartialEq, Eq)]
pub enum BootOut {
    Bootout,
    NotLoaded,
    /// Loaded from a plist that is not this config's; carries that path (or a
    /// note that `launchctl print` named none) for the error message.
    Foreign(String),
}

/// `print` is `launchctl print`'s output, or None when the label is not loaded.
///
/// Kept apart from the call so that skipping a boot-out is a value the caller
/// must handle: a skipped boot-out that reads as success would delete the
/// plist and leave the job running with nothing for the next scan to find.
pub fn boot_out_decision(
    print: Option<&str>,
    config_dir: &std::path::Path,
    owned_agent: Option<&std::path::Path>,
) -> BootOut {
    let Some(print) = print else {
        return BootOut::NotLoaded;
    };
    if loaded_job_is_ours(print, config_dir, owned_agent) {
        return BootOut::Bootout;
    }
    let path = print
        .lines()
        .find_map(|line| line.trim().strip_prefix("path = "))
        .map_or_else(
            || "(no path reported)".to_string(),
            |p| p.trim().to_string(),
        );
    BootOut::Foreign(path)
}

/// `launchctl print` of a label nothing loaded. Any other failure is not
/// evidence that the job is absent.
pub fn print_failure_is_not_loaded(stderr: &str) -> bool {
    stderr.contains("Could not find service") || stderr.contains("No such process")
}

/// Remove an orphan without reaching a job that belongs to another config dir.
///
/// Unlike [`uninstall`], the boot-out is conditional and so is the deletion of
/// the LaunchAgents copy; the config dir's own plist is always this config's.
pub async fn uninstall_orphan(label: &str, config_dir: &std::path::Path) -> Result<()> {
    let agent = agent_path(label)?;
    let logs = config_dir.join("logs");
    let owned_agent = std::fs::read_to_string(&agent)
        .ok()
        .filter(|text| plist_log_path(text).is_some_and(|log| super::is_within(&log, &logs)))
        .map(|_| agent.clone());
    let target = format!("{}/{}", domain_target(), label);
    let out = launchctl(&["print", &target]).await?;
    let print = if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else if print_failure_is_not_loaded(&String::from_utf8_lossy(&out.stderr)) {
        None
    } else {
        anyhow::bail!(
            "launchctl print {target} failed, so whether it is loaded is unknown: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    };
    match boot_out_decision(print.as_deref(), config_dir, owned_agent.as_deref()) {
        BootOut::Bootout => {
            let _ = launchctl(&["bootout", &target]).await;
        }
        BootOut::NotLoaded => {}
        // Files stay, so the next scan still sees the orphan
        BootOut::Foreign(path) => anyhow::bail!(
            "{label} is loaded from {path}, which is not this config directory's, so it was left running and its files kept"
        ),
    }
    for path in std::iter::once(plist_path(label)?).chain(owned_agent) {
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

/// Where the watchdog writes what it did. One file for the machine, beside
/// the tunnels' own logs.
pub fn watchdog_log() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?
        .join("logs")
        .join("watchdog.log"))
}

/// Install the watchdog agent and start it.
///
/// Written to both the config directory, which is gtunnel's own record, and
/// `~/Library/LaunchAgents`, which is what makes it come back after a
/// reboot. A watchdog that does not survive a login is a watchdog nobody can
/// rely on.
pub async fn install_watchdog(program: &std::path::Path) -> Result<()> {
    crate::state::ensure_logs_dir()?;
    let label = super::watchdog_label();
    let text = render_watchdog_plist(program, &watchdog_log()?, watch_interval_secs());
    for path in [plist_path(label)?, agent_path(label)?] {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
        crate::fsutil::write_atomic(&path, &text)?;
    }
    // Reload rather than bootstrap over a running one, so reinstalling picks
    // up a new binary path
    let _ = launchctl(&["bootout", &format!("{}/{label}", domain_target())]).await;
    let out = launchctl(&[
        "bootstrap",
        &domain_target(),
        &plist_path(label)?.to_string_lossy(),
    ])
    .await?;
    if !out.status.success() {
        anyhow::bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub async fn uninstall_watchdog() -> Result<()> {
    uninstall(super::watchdog_label()).await
}

/// Whether the agent is loaded right now, which is the question a person
/// means by "is the watchdog running": the file existing is not enough.
pub async fn watchdog_installed() -> bool {
    print_service(super::watchdog_label()).await.is_some()
}

/// launchd's name for the watchdog job. It belongs to launchd rather than to
/// the host: this renderer writes a plist whichever platform compiles it, and
/// a plist carrying systemd's label is a plist for nothing.
pub const WATCHDOG_LABEL: &str = "com.gtunnel.watchdog";

/// The watchdog's plist. Deliberately unlike a tunnel's: no `KeepAlive`,
/// because this job is supposed to exit every time, and `StartInterval`
/// instead, because waking on a timer is the whole of what it does. A
/// `KeepAlive` job that exits immediately is a job launchd respawns in a
/// tight loop.
pub fn render_watchdog_plist(
    program: &std::path::Path,
    log: &std::path::Path,
    interval_secs: u64,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
        <string>watch</string>
        <string>--once</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
        <key>{stamp_var}</key>
        <string>{stamp}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>StartInterval</key>
    <integer>{interval}</integer>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        label = WATCHDOG_LABEL,
        stamp_var = super::DEFINITION_VERSION_VAR,
        stamp = super::DEFINITION_VERSION,
        program = xml_escape(&program.to_string_lossy()),
        interval = interval_secs,
        log = xml_escape(&log.to_string_lossy()),
    )
}

pub fn render_plist(spec: &ServiceSpec) -> String {
    let mut args = String::new();
    args.push_str(&format!(
        "        <string>{}</string>\n",
        xml_escape(&spec.program.to_string_lossy())
    ));
    for arg in &spec.args {
        args.push_str(&format!("        <string>{}</string>\n", xml_escape(arg)));
    }

    let mut env = String::new();
    for (key, value) in &spec.env {
        env.push_str(&format!(
            "        <key>{}</key>\n        <string>{}</string>\n",
            xml_escape(key),
            xml_escape(value)
        ));
    }

    let log = xml_escape(&spec.log_path.to_string_lossy());

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>EnvironmentVariables</key>
    <dict>
{env}    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>SoftResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key>
        <integer>{max_files}</integer>
    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        label = xml_escape(&spec.label),
        max_files = super::TUNNEL_MAX_FILES,
    )
}

// `launchctl print` output for one service
pub fn parse_launchctl_print(text: &str) -> (TunnelStatus, RuntimeInfo) {
    let mut pid = None;
    let mut runs = 0u32;
    let mut last_exit: Option<i32> = None;
    let mut running = false;

    for line in text.lines() {
        // Real `launchctl print` nests service-endpoint blocks that carry their
        // own `state = active` lines. Only top-level keys (one leading tab)
        // describe the job itself.
        let depth = line.len() - line.trim_start_matches('\t').len();
        if depth > 1 {
            continue;
        }
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("state = ") {
            running = rest.trim() == "running";
        } else if let Some(rest) = line.strip_prefix("pid = ") {
            pid = rest.trim().parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix("runs = ") {
            runs = rest.trim().parse::<u32>().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("last exit code = ") {
            // `78: EX_CONFIG` names the code; only the number before the colon is it
            last_exit = rest.split(':').next().and_then(|n| n.trim().parse().ok());
        }
    }

    let status = if running {
        TunnelStatus::Running
    } else if matches!(last_exit, Some(code) if code != 0) {
        TunnelStatus::Error
    } else {
        TunnelStatus::Stopped
    };
    let info = RuntimeInfo {
        pid: if running { pid } else { None },
        restarts: runs.saturating_sub(1),
    };
    (status, info)
}

async fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = Command::new("launchctl");
    cmd.args(args).stdin(Stdio::null());
    let what = format!("launchctl {}", args.first().unwrap_or(&""));
    crate::proc::output_within(cmd, crate::proc::service_bound(), &what)
        .await
        .context("Failed to run launchctl")
}

fn domain_target() -> String {
    // SAFETY: getuid(2) takes no arguments, reads no caller memory and cannot fail.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

fn write_plist(path: &std::path::Path, spec: &ServiceSpec) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    crate::fsutil::write_atomic(path, &render_plist(spec))
}

// Writing the definition is not the same as asking the tunnel to run: a loaded
// KeepAlive job always runs, so a job that was not loaded stays unloaded and
// `start` is what loads it.
pub async fn install(spec: &ServiceSpec) -> Result<()> {
    crate::state::ensure_logs_dir()?;
    // The tunnel about to run owns its log; the last run's errors are not its own
    super::write_log_header(spec)?;
    write_plist(&plist_path(&spec.label)?, spec)?;
    sync_agent_copy(&agent_path(&spec.label)?, spec)?;

    // A running tunnel is reloaded so the new argv takes effect
    if status(&spec.label).await == TunnelStatus::Running {
        stop(&spec.label).await?;
        start(&spec.label).await?;
    }
    Ok(())
}

/// The definition and its login copy, and nothing else: no log reset, no
/// launchctl. A refresh of an old install must not wipe the log that may hold
/// the reason the tunnel died, nor start a job the user stopped.
pub async fn write_definition(spec: &ServiceSpec) -> Result<()> {
    write_plist(&plist_path(&spec.label)?, spec)?;
    sync_agent_copy(&agent_path(&spec.label)?, spec)
}

/// launchd has no reload for a loaded job's argv: unloading and loading it is
/// the only way the new definition is read.
pub async fn restart(label: &str) -> Result<()> {
    stop(label).await?;
    start(label).await
}

pub async fn uninstall(label: &str) -> Result<()> {
    let _ = launchctl(&["bootout", &format!("{}/{}", domain_target(), label)]).await;
    for path in [plist_path(label)?, agent_path(label)?] {
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

// `start` loads the plist before kicking it, because `stop` unloads the job
pub fn start_args(domain: &str, label: &str, plist: &str) -> (Vec<String>, Vec<String>) {
    (
        vec![
            "bootstrap".to_string(),
            domain.to_string(),
            plist.to_string(),
        ],
        vec!["kickstart".to_string(), format!("{domain}/{label}")],
    )
}

// Unloading is the only stop that holds: `launchctl kill` leaves the job
// loaded, and KeepAlive{SuccessfulExit:false} reads a SIGTERM death as a
// failure, so launchd restarted the tunnel one ThrottleInterval later
pub fn stop_args(domain: &str, label: &str) -> Vec<String> {
    vec!["bootout".to_string(), format!("{domain}/{label}")]
}

pub async fn start(label: &str) -> Result<()> {
    crate::daemon::note_asked(label);
    let path = plist_path(label)?;
    let (load, run) = start_args(&domain_target(), label, &path.to_string_lossy());
    // Already loaded is the common case and bootstrap says so; kickstart decides
    let loaded = launchctl(&load.iter().map(String::as_str).collect::<Vec<_>>()).await;
    let out = launchctl(&run.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    if !out.status.success() {
        let bootstrap = match &loaded {
            Ok(o) => String::from_utf8_lossy(&o.stderr).into_owned(),
            Err(e) => format!("{e:#}"),
        };
        anyhow::bail!(
            "{}",
            start_failure(&bootstrap, &String::from_utf8_lossy(&out.stderr))
        );
    }
    Ok(())
}

// Kickstart only says the job is not loaded; the bootstrap before it is why
pub fn start_failure(bootstrap_stderr: &str, kickstart_stderr: &str) -> String {
    let kickstart = format!("launchctl kickstart failed: {}", kickstart_stderr.trim());
    match bootstrap_stderr.trim() {
        "" => kickstart,
        b => format!("{kickstart}; launchctl bootstrap failed: {b}"),
    }
}

pub async fn stop(label: &str) -> Result<()> {
    let args = stop_args(&domain_target(), label);
    let out = launchctl(&args.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Not loaded is already stopped
        if !stderr.contains("No such process") && !stderr.contains("not find") {
            anyhow::bail!("launchctl bootout failed: {}", stderr.trim());
        }
    }
    Ok(())
}

// Auto-start is the LaunchAgents copy: written when on, removed when off
pub fn sync_agent_copy(agent: &std::path::Path, spec: &ServiceSpec) -> Result<()> {
    if spec.auto_start {
        write_plist(agent, spec)
    } else if agent.exists() {
        std::fs::remove_file(agent).with_context(|| format!("Failed to remove {}", agent.display()))
    } else {
        Ok(())
    }
}

// Touches only the LaunchAgents copy, never whether the tunnel is running now
pub async fn set_auto_start(spec: &ServiceSpec) -> Result<()> {
    sync_agent_copy(&agent_path(&spec.label)?, spec)
}

async fn print_service(label: &str) -> Option<String> {
    let out = launchctl(&["print", &format!("{}/{}", domain_target(), label)])
        .await
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

pub async fn status(label: &str) -> TunnelStatus {
    match print_service(label).await {
        Some(text) => parse_launchctl_print(&text).0,
        None => TunnelStatus::Stopped,
    }
}

/// `status`, but a call that errored or timed out is `None` rather than
/// `Stopped`. Only the watchdog needs the difference: it must not act on a
/// manager that did not answer.
pub async fn status_checked(label: &str) -> Option<TunnelStatus> {
    let out = launchctl(&["print", &format!("{}/{}", domain_target(), label)]).await;
    super::checked_status(out, |text| parse_launchctl_print(text).0)
}

pub async fn runtime_info(label: &str) -> RuntimeInfo {
    match print_service(label).await {
        Some(text) => parse_launchctl_print(&text).1,
        None => RuntimeInfo::default(),
    }
}
