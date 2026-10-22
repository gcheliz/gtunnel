use anyhow::{bail, Context, Result};

use super::{confirm, prompt};
use crate::config::{config_dir, load_config, save_config, validate_account_name, Account};
use crate::gcp::{compute, gcloud, GcpClient};

// First run: turn a gcloud configuration into a gtunnel account
pub async fn cmd_init() -> Result<()> {
    gcloud::require_gcloud()?;
    let configurations = gcloud::list_configurations().await?;
    if configurations.is_empty() {
        bail!("No gcloud configurations found. Run `gcloud init` first");
    }

    println!("gcloud configurations:");
    for (index, c) in configurations.iter().enumerate() {
        let active = if c.is_active { " (active)" } else { "" };
        println!(
            "  {}. {}{active}  project={}  account={}",
            index + 1,
            c.name,
            c.project.as_deref().unwrap_or("-"),
            c.account.as_deref().unwrap_or("-")
        );
    }

    let active = configurations
        .iter()
        .position(|c| c.is_active)
        .map(|i| (i + 1).to_string());
    let choice = prompt("Configuration number", active.as_deref())?;
    let index: usize = choice
        .parse::<usize>()
        .ok()
        .filter(|n| *n >= 1 && *n <= configurations.len())
        .with_context(|| format!("'{choice}' is not one of the listed numbers"))?;
    let chosen = &configurations[index - 1];

    let mut config = load_config().unwrap_or_default();
    let name = prompt("Account name", Some(&chosen.name))?;
    validate_account_name(&name)?;
    if config.accounts.iter().any(|a| a.name == name) {
        bail!("Account '{name}' already exists. Pick another name");
    }

    let project = prompt("Project", chosen.project.as_deref())?;
    if project.is_empty() {
        bail!("A project is required");
    }
    let impersonate = prompt("Service account to impersonate (optional)", Some(""))?;

    let mut account = Account {
        name: name.clone(),
        gcloud_configuration: chosen.name.clone(),
        project,
        default_zone: String::new(),
        impersonate_service_account: (!impersonate.is_empty()).then_some(impersonate),
        expose: None,
    };

    let client = GcpClient::for_account(&account);
    let zones = compute::list_zones(&client, &account.project)
        .await
        .unwrap_or_default();
    let zone = prompt("Default zone", chosen.zone.as_deref())?;
    if zone.is_empty() {
        bail!("A default zone is required");
    }
    if !zones.is_empty() && !zones.contains(&zone) {
        bail!(
            "Zone '{zone}' does not exist in project '{}'",
            account.project
        );
    }
    account.default_zone = zone;

    config.add_account(account)?;
    save_config(&config)?;
    println!("Account '{name}' saved to {}", config_dir()?.display());
    Ok(())
}

pub fn cmd_account_list() -> Result<()> {
    let config = load_config()?;
    if config.accounts.is_empty() {
        println!("No accounts. Run `gtunnel init`");
        return Ok(());
    }
    for account in &config.accounts {
        let marker = if account.name == config.selected_account {
            "*"
        } else {
            " "
        };
        println!(
            "{marker} {:<16} project={:<24} zone={}",
            account.name, account.project, account.default_zone
        );
    }
    Ok(())
}

pub fn cmd_account_select(name: &str) -> Result<()> {
    let mut config = load_config()?;
    config.select_account(name)?;
    save_config(&config)?;
    println!("Selected account '{name}'");
    Ok(())
}

pub fn cmd_account_remove(name: &str, assume_yes: bool) -> Result<()> {
    let mut config = load_config()?;
    let state = crate::state::TunnelState::load()?;
    let live = state.tunnels_for_account(name).len();
    if live > 0 {
        println!("Account '{name}' still has {live} tunnel(s); they stay in tunnels.toml");
    }
    if !confirm(&format!("Remove account '{name}'?"), assume_yes)? {
        println!("Cancelled");
        return Ok(());
    }
    config.remove_account(name)?;
    save_config(&config)?;
    println!("Removed account '{name}'");
    Ok(())
}
