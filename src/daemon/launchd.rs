// The mirror of the waiver in systemd.rs: off macOS nothing reaches this
// module but its own tests, and `clippy -D warnings` reads the binary target,
// where a test is not a caller
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{RuntimeInfo, ServiceSpec};
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
    // SAFETY: getuid(2) takes no arguments, reads no caller memory and cannot fail.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

fn write_plist(path: &std::path::Path, spec: &ServiceSpec) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    std::fs::write(path, render_plist(spec))
        .with_context(|| format!("Failed to write {}", path.display()))
}

// Writing the definition is not the same as asking the tunnel to run: a loaded
// KeepAlive job always runs, so a job that was not loaded stays unloaded and
// `start` is what loads it.
pub async fn install(spec: &ServiceSpec) -> Result<()> {
    crate::state::ensure_logs_dir()?;
    // The tunnel about to run owns its log; the last run's errors are not its own
    super::write_log_header(spec)?;
    write_plist(&plist_path(&spec.label)?, spec)?;

    let agent = agent_path(&spec.label)?;
    if spec.auto_start {
        write_plist(&agent, spec)?;
    } else if agent.exists() {
        std::fs::remove_file(&agent)
            .with_context(|| format!("Failed to remove {}", agent.display()))?;
    }

    // A running tunnel is reloaded so the new argv takes effect
    if status(&spec.label).await == TunnelStatus::Running {
        stop(&spec.label).await?;
        start(&spec.label).await?;
    }
    Ok(())
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
    let path = plist_path(label)?;
    let (load, run) = start_args(&domain_target(), label, &path.to_string_lossy());
    // Already loaded is the common case and bootstrap says so; kickstart decides
    let _ = launchctl(&load.iter().map(String::as_str).collect::<Vec<_>>()).await;
    let out = launchctl(&run.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("launchctl kickstart failed: {}", stderr.trim());
    }
    Ok(())
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

// Auto-start is the LaunchAgents copy, which install adds or removes. It does
// not touch whether the tunnel is running now.
pub async fn set_auto_start(spec: &ServiceSpec) -> Result<()> {
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
