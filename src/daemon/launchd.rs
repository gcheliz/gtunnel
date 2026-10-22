use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{RuntimeInfo, ServiceSpec};
use crate::state::TunnelStatus;

pub fn label_for(account: &str, name: &str) -> String {
    format!("com.gtunnel.{account}.{name}")
}

fn agents_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine the home directory")?;
    Ok(home.join("Library/LaunchAgents"))
}

fn plist_path(label: &str) -> Result<PathBuf> {
    Ok(agents_dir()?.join(format!("{label}.plist")))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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

    let run_at_load = if spec.auto_start { "true" } else { "false" };
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
    <{run_at_load}/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
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
            last_exit = rest.trim().parse::<i32>().ok();
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
    Command::new("launchctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .context("Failed to run launchctl")
}

fn domain_target() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

pub async fn install(spec: &ServiceSpec) -> Result<()> {
    crate::state::ensure_logs_dir()?;
    let dir = agents_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = plist_path(&spec.label)?;
    std::fs::write(&path, render_plist(spec))
        .with_context(|| format!("Failed to write {}", path.display()))?;
    // Replace any already-loaded definition so the new argv takes effect
    let _ = launchctl(&["bootout", &format!("{}/{}", domain_target(), spec.label)]).await;
    let out = launchctl(&["bootstrap", &domain_target(), &path.to_string_lossy()]).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("launchctl bootstrap failed: {}", stderr.trim());
    }
    Ok(())
}

pub async fn uninstall(label: &str) -> Result<()> {
    let _ = launchctl(&["bootout", &format!("{}/{}", domain_target(), label)]).await;
    let path = plist_path(label)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
    }
    Ok(())
}

pub async fn start(label: &str) -> Result<()> {
    let out = launchctl(&["kickstart", &format!("{}/{}", domain_target(), label)]).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("launchctl kickstart failed: {}", stderr.trim());
    }
    Ok(())
}

pub async fn stop(label: &str) -> Result<()> {
    // `kill` stops the job without unloading it, so `start` can bring it back
    let out = launchctl(&["kill", "SIGTERM", &format!("{}/{}", domain_target(), label)]).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.contains("No such process") {
            anyhow::bail!("launchctl kill failed: {}", stderr.trim());
        }
    }
    Ok(())
}

pub async fn set_auto_start(spec: &ServiceSpec) -> Result<()> {
    // RunAtLoad lives in the plist, so rewriting and reloading is the change
    install(spec).await
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

pub async fn runtime_info(label: &str) -> RuntimeInfo {
    match print_service(label).await {
        Some(text) => parse_launchctl_print(&text).1,
        None => RuntimeInfo::default(),
    }
}

pub async fn is_running(label: &str) -> bool {
    status(label).await == TunnelStatus::Running
}
