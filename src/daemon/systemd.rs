// Only `platform_label` reaches this module on macOS, where the rest is
// compiled for its tests and never called
#![cfg_attr(target_os = "macos", allow(dead_code))]

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{watch_interval_secs, RuntimeInfo, ServiceSpec};
use crate::state::TunnelStatus;

pub fn unit_for(account: &str, name: &str) -> String {
    format!("gtunnel-{account}-{name}.service")
}

fn units_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine the home directory")?;
    Ok(home.join(".config/systemd/user"))
}

pub(super) fn unit_path(label: &str) -> Result<PathBuf> {
    Ok(units_dir()?.join(label))
}

/// The log a unit appends to, which is how a unit says whose it is. See
/// [`super::launchd::plist_log_path`].
pub fn unit_log_path(text: &str) -> Option<PathBuf> {
    text.lines()
        .find_map(|line| line.strip_prefix("StandardOutput=append:"))
        .map(|p| PathBuf::from(p.trim()))
}

// systemd splits ExecStart on whitespace, so anything containing one is quoted
fn quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

pub fn watchdog_log() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?
        .join("logs")
        .join("watchdog.log"))
}

/// Install the watchdog timer and start it. The timer is what is enabled;
/// the service it fires is not enabled on its own, or it would run once at
/// boot and never again.
pub async fn install_watchdog(program: &std::path::Path) -> Result<()> {
    crate::state::ensure_logs_dir()?;
    let label = super::watchdog_label();
    let (service, timer) = render_watchdog_units(program, &watchdog_log()?, watch_interval_secs());
    for (name, text) in [
        (format!("{label}.service"), service),
        (format!("{label}.timer"), timer),
    ] {
        let path = unit_path(&name)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
        crate::fsutil::write_atomic(&path, &text)?;
    }
    let _ = systemctl(&["daemon-reload"]).await;
    let out = systemctl(&["enable", "--now", &format!("{label}.timer")]).await?;
    if !out.status.success() {
        anyhow::bail!(
            "systemctl enable failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub async fn uninstall_watchdog() -> Result<()> {
    let label = super::watchdog_label();
    let _ = systemctl(&["disable", "--now", &format!("{label}.timer")]).await;
    for name in [format!("{label}.service"), format!("{label}.timer")] {
        if let Ok(path) = unit_path(&name)
            && path.exists()
        {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
        }
    }
    let _ = systemctl(&["daemon-reload"]).await;
    Ok(())
}

pub async fn watchdog_installed() -> bool {
    systemctl(&["is-active", &format!("{}.timer", super::watchdog_label())])
        .await
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The watchdog as systemd spells it: a oneshot service that exits, and a
/// timer that fires it again. `OnUnitActiveSec` measures from the end of the
/// last run, so a slow pass delays the next one rather than overlapping it.
/// systemd's name for the watchdog units. See [`super::launchd::WATCHDOG_LABEL`]
/// for why it is a constant here rather than the host's label.
pub const WATCHDOG_LABEL: &str = "gtunnel-watchdog";

pub fn render_watchdog_units(
    program: &std::path::Path,
    log: &std::path::Path,
    interval_secs: u64,
) -> (String, String) {
    let label = WATCHDOG_LABEL;
    let service = format!(
        r#"[Unit]
Description=gtunnel - restart tunnels that are refusing connections

[Service]
Type=oneshot
Environment={stamp_var}={stamp}
ExecStart={program} watch --once
StandardOutput=append:{log}
StandardError=append:{log}
"#,
        program = quote(&program.to_string_lossy()),
        log = log.display(),
        stamp_var = super::DEFINITION_VERSION_VAR,
        stamp = super::DEFINITION_VERSION,
    );
    let timer = format!(
        r#"[Unit]
Description=gtunnel - watchdog timer

[Timer]
OnBootSec={interval}s
OnUnitActiveSec={interval}s
Unit={label}.service

[Install]
WantedBy=timers.target
"#,
        interval = interval_secs,
    );
    (service, timer)
}

/// One `Environment=` line. A value is written as it is unless systemd would
/// read it differently: `%` starts a specifier, so it is doubled (a proxy URL
/// with an encoded password has one), and whitespace, a quote or a backslash
/// needs the whole assignment quoted, or a `CLOUDSDK_CONFIG` under a path
/// with a space ends at the space.
fn env_line(key: &str, value: &str) -> String {
    let value = value.replace('%', "%%");
    if value.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') {
        let inner = format!("{key}={value}")
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        format!("Environment=\"{inner}\"\n")
    } else {
        format!("Environment={key}={value}\n")
    }
}

/// The inverse of [`env_line`] for one variable
pub fn unit_env_value(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix("Environment=")?;
        let assignment = match rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
            Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
            None => rest.to_string(),
        };
        let value = assignment.strip_prefix(name)?.strip_prefix('=')?;
        Some(value.replace("%%", "%"))
    })
}

pub fn render_unit(spec: &ServiceSpec) -> String {
    let mut exec = quote(&spec.program.to_string_lossy());
    for arg in &spec.args {
        exec.push(' ');
        exec.push_str(&quote(arg));
    }

    let env: String = spec.env.iter().map(|(k, v)| env_line(k, v)).collect();

    let log = spec.log_path.display();

    format!(
        r#"[Unit]
Description=gtunnel - {label}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
{env}ExecStart={exec}
Restart=on-failure
RestartSec=5
RestartPreventExitStatus={prevent}
LimitNOFILE={max_files}
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
"#,
        label = spec.label,
        prevent = super::AUTH_EXPIRED_EXIT,
        max_files = super::TUNNEL_MAX_FILES,
    )
}

// `systemctl --user show -p ActiveState,MainPID,NRestarts` output
pub fn parse_systemctl_show(text: &str) -> (TunnelStatus, RuntimeInfo) {
    let mut active = "";
    let mut pid = 0u32;
    let mut restarts = 0u32;

    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("ActiveState=") {
            active = rest.trim();
        } else if let Some(rest) = line.trim().strip_prefix("MainPID=") {
            pid = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.trim().strip_prefix("NRestarts=") {
            restarts = rest.trim().parse().unwrap_or(0);
        }
    }

    let status = match active {
        "active" | "activating" | "reloading" => TunnelStatus::Running,
        "failed" => TunnelStatus::Error,
        _ => TunnelStatus::Stopped,
    };
    let info = RuntimeInfo {
        pid: (pid != 0).then_some(pid),
        restarts,
    };
    (status, info)
}

async fn systemctl(args: &[&str]) -> Result<std::process::Output> {
    let mut full = vec!["--user"];
    full.extend_from_slice(args);
    let mut cmd = Command::new("systemctl");
    cmd.args(&full).stdin(Stdio::null());
    let what = format!("systemctl {}", args.first().unwrap_or(&""));
    crate::proc::output_within(cmd, crate::proc::service_bound(), &what)
        .await
        .context("Failed to run systemctl")
}

async fn daemon_reload() -> Result<()> {
    systemctl(&["daemon-reload"]).await?;
    Ok(())
}

pub async fn install(spec: &ServiceSpec) -> Result<()> {
    crate::state::ensure_logs_dir()?;
    // The tunnel about to run owns its log; the last run's errors are not its own
    super::write_log_header(spec)?;
    let dir = units_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = unit_path(&spec.label)?;
    crate::fsutil::write_atomic(&path, &render_unit(spec))?;
    daemon_reload().await?;
    set_auto_start(spec).await
}

/// The unit file and its enablement, with no log reset and no start; see
/// the launchd twin for why.
pub async fn write_definition(spec: &ServiceSpec) -> Result<()> {
    let dir = units_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = unit_path(&spec.label)?;
    crate::fsutil::write_atomic(&path, &render_unit(spec))?;
    daemon_reload().await?;
    set_auto_start(spec).await
}

pub async fn restart(label: &str) -> Result<()> {
    let out = systemctl(&["restart", label]).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("systemctl restart failed: {}", stderr.trim());
    }
    Ok(())
}

pub async fn uninstall(label: &str) -> Result<()> {
    let _ = systemctl(&["stop", label]).await;
    let _ = systemctl(&["disable", label]).await;
    let path = unit_path(label)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
    }
    daemon_reload().await
}

pub async fn start(label: &str) -> Result<()> {
    let out = systemctl(&["start", label]).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("systemctl start failed: {}", stderr.trim());
    }
    Ok(())
}

pub async fn stop(label: &str) -> Result<()> {
    let out = systemctl(&["stop", label]).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stop_failure_is_already_stopped(&stderr) {
            anyhow::bail!("systemctl stop failed: {}", stderr.trim());
        }
    }
    Ok(())
}

// Not loaded is already stopped, as it is for launchd
pub fn stop_failure_is_already_stopped(stderr: &str) -> bool {
    stderr.contains("not loaded")
}

pub async fn set_auto_start(spec: &ServiceSpec) -> Result<()> {
    let verb = if spec.auto_start { "enable" } else { "disable" };
    let _ = systemctl(&[verb, &spec.label]).await;
    Ok(())
}

async fn show(label: &str) -> Option<String> {
    let out = systemctl(&["show", "-p", "ActiveState,MainPID,NRestarts", label])
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn status(label: &str) -> TunnelStatus {
    match show(label).await {
        Some(text) => parse_systemctl_show(&text).0,
        None => TunnelStatus::Stopped,
    }
}

pub async fn runtime_info(label: &str) -> RuntimeInfo {
    match show(label).await {
        Some(text) => parse_systemctl_show(&text).1,
        None => RuntimeInfo::default(),
    }
}
