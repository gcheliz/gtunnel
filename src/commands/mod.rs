pub mod access;
pub mod account;
pub mod expose;
pub mod maint;

use anyhow::{Context, Result};
use std::io::Write;

use crate::config::{load_config, Account, Config};
use crate::daemon;
use crate::state::{PersistentTunnel, TunnelSpec, TunnelState, TunnelStatus};

// Column widths shared by the header and every row
const NAME_WIDTH: usize = 16;
const MODE_WIDTH: usize = 7;
const URL_WIDTH: usize = 32;

pub fn status_word(status: TunnelStatus) -> &'static str {
    match status {
        TunnelStatus::Running => "running",
        TunnelStatus::Stopped => "stopped",
        TunnelStatus::Error => "error",
    }
}

pub fn list_header() -> String {
    format!(
        "{:<10} {:<NAME_WIDTH$} {:<MODE_WIDTH$} {:<URL_WIDTH$} {}",
        "STATUS", "NAME", "MODE", "URL", "TARGET"
    )
}

pub fn format_list_row(status: TunnelStatus, t: &PersistentTunnel) -> String {
    let mode = match t.spec {
        TunnelSpec::Access { .. } => "access",
        TunnelSpec::Expose { .. } => "expose",
    };
    format!(
        "{} {:<8} {:<NAME_WIDTH$} {:<MODE_WIDTH$} {:<URL_WIDTH$} {}",
        status.symbol(),
        status_word(status),
        t.name,
        mode,
        t.display_url(),
        t.display_target()
    )
    .trim_end()
    .to_string()
}

// Read one line, falling back to the default when the user just presses enter
pub fn prompt(label: &str, default: Option<&str>) -> Result<String> {
    match default {
        Some(d) => print!("{label} [{d}]: "),
        None => print!("{label}: "),
    }
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("Failed to read from stdin")?;
    let value = line.trim().to_string();
    if value.is_empty() {
        return match default {
            Some(d) => Ok(d.to_string()),
            None => Ok(String::new()),
        };
    }
    Ok(value)
}

pub fn confirm(question: &str, assume_yes: bool) -> Result<bool> {
    if assume_yes {
        return Ok(true);
    }
    let answer = prompt(&format!("{question} [y/N]"), Some("n"))?;
    Ok(matches!(answer.to_lowercase().as_str(), "y" | "yes"))
}

// The config plus the account this invocation acts on
pub fn resolve(account: Option<&str>) -> Result<(Config, Account)> {
    let config = load_config()?;
    let acct = config.get_account(account)?.clone();
    Ok((config, acct))
}

// Find a tunnel of this account by name, with a message that names the account
pub fn find_tunnel(state: &TunnelState, name: &str, account: &str) -> Result<PersistentTunnel> {
    state
        .find_for_account(name, account)
        .cloned()
        .with_context(|| format!("Tunnel '{name}' not found for account '{account}'"))
}

pub async fn tunnel_status(t: &PersistentTunnel) -> TunnelStatus {
    daemon::status(&daemon::platform_label(&t.account_name, &t.name)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};

    fn access() -> PersistentTunnel {
        PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: true,
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

    fn expose() -> PersistentTunnel {
        PersistentTunnel {
            name: "api".to_string(),
            account_name: "dev".to_string(),
            auto_start: true,
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "api.dev.example.com".to_string(),
                relay_port: 30001,
                private: false,
            },
        }
    }

    #[test]
    fn list_row_holds_status_name_mode_url_and_target() {
        let row = format_list_row(TunnelStatus::Running, &access());
        let columns: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(columns[0], TunnelStatus::Running.symbol());
        assert_eq!(columns[1], "running");
        assert_eq!(columns[2], "db");
        assert_eq!(columns[3], "access");
        assert_eq!(columns[4], "localhost:15432");
        assert_eq!(columns[5], "my-vm:5432");
    }

    #[test]
    fn list_row_marks_an_expose_tunnel() {
        let row = format_list_row(TunnelStatus::Stopped, &expose());
        assert!(row.contains("expose"), "{row}");
        assert!(row.contains("https://api.dev.example.com"), "{row}");
    }

    #[test]
    fn the_list_header_lines_up_with_a_row() {
        let header = list_header();
        let row = format_list_row(TunnelStatus::Running, &access());
        assert_eq!(
            header.split_whitespace().count(),
            5,
            "header columns: {header}"
        );
        assert!(row.len() >= header.len() - 8, "{row}");
    }
}
