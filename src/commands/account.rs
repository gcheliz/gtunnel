use crate::error::GtunnelError;
use anyhow::{bail, Context, Result};

use super::{confirm, prompt};
use crate::config::{
    config_dir, load_config, update_config, update_or_init_config, validate_account_name,
    validate_credential_file, Account,
};
use crate::gcp::projects::{self, Project};
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

    // Read for the early duplicate check; the locked update at the end checks again
    let config = load_config().unwrap_or_default();
    let name = prompt("Account name", Some(&chosen.name))?;
    validate_account_name(&name)?;
    if config.accounts.iter().any(|a| a.name == name) {
        return Err(GtunnelError::usage(format!("Account '{name}' already exists")).into());
    }

    // Provisional: the project list is fetched with this account's gcloud
    // configuration, before the project is known
    let mut account = Account {
        name: name.clone(),
        gcloud_configuration: chosen.name.clone(),
        project: String::new(),
        default_zone: String::new(),
        impersonate_service_account: None,
        credential_file: None,
        expose: None,
    };
    let client = GcpClient::for_account(&account);

    let project = match projects::list_projects(&client).await {
        Ok(listed) if !listed.is_empty() => {
            println!("Projects:");
            for (index, p) in listed.iter().enumerate() {
                let default = if chosen.project.as_deref() == Some(p.project_id.as_str()) {
                    " (default)"
                } else {
                    ""
                };
                println!("  {}. {}  {}{default}", index + 1, p.project_id, p.name);
            }
            let answer = prompt("Project (number or id)", chosen.project.as_deref())?;
            resolve_project_choice(&answer, &listed)
        }
        // A failed or empty list never blocks the account: typing still works
        other => {
            match other {
                Err(e) => println!("Could not list projects: {}", short_cause(&e)),
                Ok(_) => println!("Could not list projects: no project is visible to this account"),
            }
            prompt("Project", chosen.project.as_deref())?
        }
    };
    if project.is_empty() {
        bail!("A project is required");
    }
    println!(
        "Every gcloud call of this account acts as this service account; \
         you need roles/iam.serviceAccountTokenCreator on it. Empty: act as yourself."
    );
    let impersonate = prompt("Service account to impersonate (optional)", Some(""))?;
    account.project = project;
    account.impersonate_service_account = (!impersonate.is_empty()).then_some(impersonate);

    let client = GcpClient::for_account(&account);
    let zones = compute::list_zones(&client, &account.project)
        .await
        .unwrap_or_default();
    // Listed as the projects are, so the zone is picked rather than recalled;
    // an empty list (denied or unreachable) leaves the prompt as it was
    let zone = if zones.is_empty() {
        prompt("Default zone", chosen.zone.as_deref())?
    } else {
        println!("Zones:");
        for (index, z) in zones.iter().enumerate() {
            let default = if chosen.zone.as_deref() == Some(z.as_str()) {
                " (default)"
            } else {
                ""
            };
            println!("  {}. {z}{default}", index + 1);
        }
        let answer = prompt("Default zone (number or name)", chosen.zone.as_deref())?;
        resolve_zone_choice(&answer, &zones)
    };
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

    update_or_init_config(|config| config.add_account(account))?;
    println!("Account '{name}' saved to {}", config_dir()?.display());
    Ok(())
}

// A number in range picks that row; anything else is the id as typed, so a
// project the list does not show (or a list that is empty) still works. Reading
// a number as a row is safe because a GCP project id starts with a letter
fn resolve_project_choice(input: &str, projects: &[Project]) -> String {
    let typed = input.trim();
    typed
        .parse::<usize>()
        .ok()
        .filter(|n| *n >= 1 && *n <= projects.len())
        .map(|n| projects[n - 1].project_id.clone())
        .unwrap_or_else(|| typed.to_string())
}

// The same rule as a project: a number in range picks that row, anything else
// is the zone as typed, which the existence check after the prompt then judges.
// A zone name never parses as a number.
fn resolve_zone_choice(input: &str, zones: &[String]) -> String {
    let typed = input.trim();
    typed
        .parse::<usize>()
        .ok()
        .filter(|n| *n >= 1 && *n <= zones.len())
        .map(|n| zones[n - 1].clone())
        .unwrap_or_else(|| typed.to_string())
}

// The root cause only: the full chain names the request URL, which is noise
// on a prompt line
fn short_cause(err: &anyhow::Error) -> String {
    let cause = err.root_cause().to_string();
    cause.lines().next().unwrap_or_default().to_string()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountJson {
    pub name: String,
    pub gcloud_configuration: String,
    pub project: String,
    pub default_zone: String,
    pub selected: bool,
}

pub fn cmd_account_list(json: bool) -> Result<()> {
    let config = load_config()?;
    if json {
        let rows: Vec<AccountJson> = config
            .accounts
            .iter()
            .map(|a| AccountJson {
                name: a.name.clone(),
                gcloud_configuration: a.gcloud_configuration.clone(),
                project: a.project.clone(),
                default_zone: a.default_zone.clone(),
                selected: a.name == config.selected_account,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
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
    update_config(|config| config.select_account(name))?;
    println!("Selected account '{name}'");
    Ok(())
}

// A tunnel record names its account, so removing the account first would leave
// records that no command can reach. `--yes` answers the confirmation below and
// nothing else: it cannot answer what becomes of the tunnels, which is the one
// question a script cannot have settled by passing a flag.
pub fn cmd_account_remove(name: &str, assume_yes: bool) -> Result<()> {
    let state = crate::state::TunnelState::load()?;
    let held = state.tunnels_for_account(name).len();
    if held > 0 {
        bail!(
            "Account '{name}' still has {held} tunnel(s). Delete them with `gtunnel delete`, \
             or move them to another account with `gtunnel move`, then remove the account"
        );
    }
    if !confirm(&format!("Remove account '{name}'?"), assume_yes)? {
        println!("Cancelled");
        return Ok(());
    }
    update_config(|config| config.remove_account(name))?;
    println!("Removed account '{name}'");
    Ok(())
}

// Build an account from a configuration gcloud already knows, taking project and
// zone from it. Separate from the gcloud call so the choices are testable.
fn account_from_configuration(
    configurations: &[gcloud::GcloudConfiguration],
    configuration: &str,
    name: &str,
) -> Result<Account> {
    let found = configurations
        .iter()
        .find(|c| c.name == configuration)
        .with_context(|| format!("No gcloud configuration named '{configuration}'"))?;
    let project = found
        .project
        .clone()
        .filter(|p| !p.is_empty())
        .with_context(|| {
            format!("gcloud configuration '{configuration}' has no project; set one with `gtunnel account edit`")
        })?;
    Ok(Account {
        name: name.to_string(),
        gcloud_configuration: found.name.clone(),
        project,
        default_zone: found.zone.clone().unwrap_or_default(),
        impersonate_service_account: None,
        credential_file: None,
        expose: None,
    })
}

/// What `account add` may take besides a configuration. `project` is the
/// switch: given, the account is built from the flags alone, so it works on a
/// CI runner where no gcloud configuration was ever made.
#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    pub project: Option<String>,
    pub zone: Option<String>,
    pub credential_file: Option<std::path::PathBuf>,
}

// Build an account from flags, with no gcloud call. gcloud creates a named
// configuration on first use, so the name need not exist yet.
fn account_from_flags(configuration: &str, name: &str, opts: &AddOptions) -> Result<Account> {
    let project = opts
        .project
        .clone()
        .filter(|p| !p.is_empty())
        .context("--project cannot be empty")?;
    if let Some(file) = &opts.credential_file {
        validate_credential_file(file)?;
    }
    Ok(Account {
        name: name.to_string(),
        gcloud_configuration: configuration.to_string(),
        project,
        default_zone: opts.zone.clone().unwrap_or_default(),
        impersonate_service_account: None,
        credential_file: opts.credential_file.clone(),
        expose: None,
    })
}

// The non-interactive counterpart of `init`, for callers that already know which
// configuration they mean. It is async because listing configurations shells out
// to gcloud, which `--project` makes unnecessary.
pub async fn cmd_account_add(configuration: &str, name: &str, opts: AddOptions) -> Result<()> {
    let account = if opts.project.is_some() {
        account_from_flags(configuration, name, &opts)?
    } else {
        gcloud::require_gcloud()?;
        let configurations = gcloud::list_configurations().await?;
        let mut account = account_from_configuration(&configurations, configuration, name)?;
        if let Some(file) = &opts.credential_file {
            validate_credential_file(file)?;
            account.credential_file = Some(file.clone());
        }
        if let Some(zone) = &opts.zone {
            account.default_zone = zone.clone();
        }
        account
    };
    update_or_init_config(|config| config.add_account(account))?;
    println!("Account '{name}' saved to {}", config_dir()?.display());
    Ok(())
}

// Only the fields given change; an empty value is refused so an edit cannot
// leave an account without the project or zone every command needs. The
// credential file is the one field that may be emptied: "" clears it.
pub fn cmd_account_edit(
    name: &str,
    project: Option<&str>,
    zone: Option<&str>,
    credential_file: Option<&str>,
) -> Result<()> {
    if project.is_none() && zone.is_none() && credential_file.is_none() {
        bail!("Nothing to change: pass --project, --zone, or both (or --credential-file)");
    }
    if project == Some("") || zone == Some("") {
        bail!("A project or zone cannot be empty");
    }
    if let Some(file) = credential_file.filter(|f| !f.is_empty()) {
        validate_credential_file(std::path::Path::new(file))?;
    }
    update_config(|config| {
        let account = config.get_account_mut(Some(name))?;
        if let Some(project) = project {
            account.project = project.to_string();
        }
        if let Some(zone) = zone {
            account.default_zone = zone.to_string();
        }
        if let Some(file) = credential_file {
            account.credential_file = (!file.is_empty()).then(|| file.into());
        }
        Ok(())
    })?;
    println!("Account '{name}' updated");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::testing::TempConfigDir;
    use crate::config::{save_config, Config};
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelState};

    fn account(name: &str) -> Account {
        Account {
            name: name.to_string(),
            gcloud_configuration: "default".to_string(),
            project: "example-dev-1a2b".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: None,
            expose: None,
        }
    }

    fn tunnel(name: &str, account: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: account.to_string(),
            auto_start: false,
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

    fn save_accounts(names: &[&str]) {
        let mut config = Config::default();
        for n in names {
            config.add_account(account(n)).expect("fixture adds");
        }
        save_config(&config).expect("fixture saves");
    }

    #[test]
    fn removing_an_account_that_still_has_tunnels_is_refused() {
        let _dir = TempConfigDir::new();
        save_accounts(&["dev", "prod"]);
        let mut state = TunnelState::default();
        state.add(tunnel("db", "dev")).expect("fixture adds");
        state.add(tunnel("cache", "dev")).expect("fixture adds");
        state.save().expect("fixture saves");

        // --yes skips the question, not the refusal: a script that passes it
        // has still not said what becomes of the tunnels
        let err = cmd_account_remove("dev", true).unwrap_err().to_string();
        assert!(err.contains("2 tunnel(s)"), "{err}");
        assert!(err.contains("gtunnel move"), "{err}");
        assert!(err.contains("delete"), "{err}");
        let config = load_config().expect("loads");
        assert!(config.get_account(Some("dev")).is_ok(), "the account stays");
    }

    #[test]
    fn removing_an_account_with_no_tunnels_succeeds() {
        let _dir = TempConfigDir::new();
        save_accounts(&["dev", "prod"]);
        cmd_account_remove("prod", true).expect("removes");
        assert!(load_config().unwrap().get_account(Some("prod")).is_err());
    }

    // Characterization: `Config::remove_account` already does this, so the
    // test pins existing behaviour and never went RED
    #[test]
    fn removing_the_selected_account_moves_the_selection() {
        let _dir = TempConfigDir::new();
        save_accounts(&["dev", "prod"]);
        cmd_account_remove("dev", true).expect("removes");
        assert_eq!(load_config().unwrap().selected_account, "prod");
    }

    #[test]
    fn editing_an_account_changes_only_the_fields_given() {
        let _dir = TempConfigDir::new();
        save_accounts(&["dev"]);
        cmd_account_edit("dev", None, Some("europe-west4-a"), None).expect("edits");
        let config = load_config().unwrap();
        let a = config.get_account(Some("dev")).unwrap();
        assert_eq!(a.default_zone, "europe-west4-a");
        assert_eq!(a.project, "example-dev-1a2b");
        cmd_account_edit("dev", Some("example-dev-9z9z"), None, None).expect("edits");
        let config = load_config().unwrap();
        let a = config.get_account(Some("dev")).unwrap();
        assert_eq!(a.project, "example-dev-9z9z");
        assert_eq!(a.default_zone, "europe-west4-a");
    }

    #[test]
    fn editing_with_nothing_to_change_or_an_empty_value_is_refused() {
        let _dir = TempConfigDir::new();
        save_accounts(&["dev"]);
        assert!(cmd_account_edit("dev", None, None, None).is_err());
        assert!(cmd_account_edit("dev", Some(""), None, None).is_err());
        assert!(cmd_account_edit("nope", Some("x"), None, None).is_err());
    }

    #[test]
    fn add_with_a_project_needs_no_existing_configuration() {
        let opts = AddOptions {
            project: Some("example-dev-1a2b".into()),
            zone: Some("europe-west1-b".into()),
            credential_file: None,
        };
        let acct = account_from_flags("ci", "ci", &opts).expect("builds");
        assert_eq!(
            (
                acct.gcloud_configuration.as_str(),
                acct.project.as_str(),
                acct.default_zone.as_str()
            ),
            ("ci", "example-dev-1a2b", "europe-west1-b")
        );
        assert_eq!(acct.credential_file, None);
    }

    #[test]
    fn add_keeps_an_absolute_credential_file() {
        let opts = AddOptions {
            project: Some("example-dev-1a2b".into()),
            zone: None,
            credential_file: Some("/secrets/ci-key.json".into()),
        };
        let acct = account_from_flags("ci", "ci", &opts).expect("builds");
        assert_eq!(acct.credential_file, Some("/secrets/ci-key.json".into()));
        assert_eq!(acct.default_zone, "");
    }

    #[test]
    fn add_refuses_a_relative_credential_file() {
        let opts = AddOptions {
            project: Some("example-dev-1a2b".into()),
            zone: None,
            credential_file: Some("key.json".into()),
        };
        assert!(account_from_flags("ci", "ci", &opts).is_err());
    }

    #[test]
    fn add_refuses_an_empty_project() {
        let opts = AddOptions {
            project: Some(String::new()),
            ..AddOptions::default()
        };
        assert!(account_from_flags("ci", "ci", &opts).is_err());
    }

    #[tokio::test]
    async fn add_with_a_project_saves_without_calling_gcloud() {
        let _dir = TempConfigDir::new();
        let opts = AddOptions {
            project: Some("example-dev-1a2b".into()),
            zone: Some("europe-west1-b".into()),
            credential_file: Some("/secrets/ci-key.json".into()),
        };
        cmd_account_add("ci", "ci", opts).await.expect("adds");
        let config = load_config().unwrap();
        let a = config.get_account(Some("ci")).unwrap();
        assert_eq!(a.project, "example-dev-1a2b");
        assert_eq!(a.credential_file, Some("/secrets/ci-key.json".into()));
    }

    #[test]
    fn editing_sets_and_clears_the_credential_file() {
        let _dir = TempConfigDir::new();
        save_accounts(&["dev"]);
        cmd_account_edit("dev", None, None, Some("/secrets/ci-key.json")).expect("sets");
        let config = load_config().unwrap();
        assert_eq!(
            config.get_account(Some("dev")).unwrap().credential_file,
            Some("/secrets/ci-key.json".into())
        );
        cmd_account_edit("dev", None, None, Some("")).expect("clears");
        let config = load_config().unwrap();
        assert_eq!(
            config.get_account(Some("dev")).unwrap().credential_file,
            None
        );
        assert!(cmd_account_edit("dev", None, None, Some("key.json")).is_err());
    }

    fn configuration(name: &str) -> gcloud::GcloudConfiguration {
        gcloud::GcloudConfiguration {
            name: name.to_string(),
            is_active: false,
            project: Some("example-dev-1a2b".to_string()),
            zone: Some("europe-west1-b".to_string()),
            account: Some("you@example.com".to_string()),
        }
    }

    #[test]
    fn an_account_is_built_from_the_configuration_it_names() {
        let configs = [configuration("work"), configuration("other")];
        let built = account_from_configuration(&configs, "work", "dev").expect("builds");
        assert_eq!(built.name, "dev");
        assert_eq!(built.gcloud_configuration, "work");
        assert_eq!(built.project, "example-dev-1a2b");
        assert_eq!(built.default_zone, "europe-west1-b");
    }

    #[test]
    fn a_missing_configuration_or_one_without_a_project_is_refused() {
        let mut bare = configuration("bare");
        bare.project = None;
        let configs = [bare];
        let err = account_from_configuration(&configs, "nope", "dev").unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
        let err = account_from_configuration(&configs, "bare", "dev").unwrap_err();
        assert!(err.to_string().contains("project"), "{err}");
    }
    fn listed(ids: &[&str]) -> Vec<Project> {
        ids.iter()
            .map(|id| Project {
                project_id: (*id).to_string(),
                name: String::new(),
            })
            .collect()
    }

    #[test]
    fn a_number_picks_the_row() {
        let rows = listed(&["example-dev-1a2b", "example-prod-3c4d"]);
        assert_eq!(resolve_project_choice("2", &rows), "example-prod-3c4d");
    }

    #[test]
    fn a_number_out_of_range_is_taken_as_typed() {
        let rows = listed(&["example-dev-1a2b", "example-prod-3c4d"]);
        assert_eq!(resolve_project_choice("7", &rows), "7");
        assert_eq!(resolve_project_choice("0", &rows), "0");
    }

    #[test]
    fn a_typed_id_is_returned_trimmed() {
        let rows = listed(&["example-prod-3c4d"]);
        assert_eq!(
            resolve_project_choice("  example-dev-1a2b ", &rows),
            "example-dev-1a2b"
        );
    }

    #[test]
    fn an_empty_list_returns_the_typed_input() {
        assert_eq!(resolve_project_choice("2", &[]), "2");
        assert_eq!(resolve_project_choice(" ", &[]), "");
    }

    #[test]
    fn a_zone_number_resolves_to_that_zone_and_a_name_is_kept() {
        let zones = vec!["europe-west1-b".to_string(), "europe-west1-c".to_string()];
        assert_eq!(resolve_zone_choice("2", &zones), "europe-west1-c");
        assert_eq!(
            resolve_zone_choice("europe-west1-b", &zones),
            "europe-west1-b"
        );
        assert_eq!(resolve_zone_choice("9", &zones), "9");
        assert_eq!(
            resolve_zone_choice(" europe-west1-c ", &zones),
            "europe-west1-c"
        );
    }

    #[test]
    fn duplicate_account_name_is_rejected() {
        let mut config = Config::default();
        config.add_account(account("dev")).expect("adds");
        let err = config
            .add_account(account("dev"))
            .expect_err("should reject duplicate");
        assert_eq!(err.to_string(), "Account 'dev' already exists");
    }
}
