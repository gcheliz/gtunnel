// Only `platform_label` reaches this module on macOS, where the rest is
// compiled for its tests and never called
#![cfg_attr(target_os = "macos", allow(dead_code))]

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{RuntimeInfo, ServiceSpec};
use crate::state::TunnelStatus;

pub fn unit_for(account: &str, name: &str) -> String {
    format!("gtunnel-{account}-{name}.service")
}

fn units_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine the home directory")?;
    Ok(home.join(".config/systemd/user"))
}

fn unit_path(label: &str) -> Result<PathBuf> {
    Ok(units_dir()?.join(label))
}

// systemd splits ExecStart on whitespace, so anything containing one is quoted
fn quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

pub fn render_unit(spec: &ServiceSpec) -> String {
    let mut exec = quote(&spec.program.to_string_lossy());
    for arg in &spec.args {
        exec.push(' ');
        exec.push_str(&quote(arg));
    }

    let env: String = spec
        .env
        .iter()
        .map(|(k, v)| format!("Environment={k}={v}\n"))
        .collect();

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
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
"#,
        label = spec.label,
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
    Command::new("systemctl")
        .args(&full)
        .stdin(Stdio::null())
        .output()
        .await
        .context("Failed to run systemctl")
}

async fn daemon_reload() -> Result<()> {
    systemctl(&["daemon-reload"]).await?;
    Ok(())
}

pub async fn install(spec: &ServiceSpec) -> Result<()> {
    crate::state::ensure_logs_dir()?;
    let dir = units_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = unit_path(&spec.label)?;
    std::fs::write(&path, render_unit(spec))
        .with_context(|| format!("Failed to write {}", path.display()))?;
    daemon_reload().await?;
    set_auto_start(spec).await
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
        anyhow::bail!("systemctl stop failed: {}", stderr.trim());
    }
    Ok(())
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

pub async fn is_running(label: &str) -> bool {
    status(label).await == TunnelStatus::Running
}
