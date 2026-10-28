use crate::error::GtunnelError;
use anyhow::{bail, Context, Result};

use super::{confirm, prompt};
use crate::config::{
    config_dir, load_config, update_config, update_or_init_config, validate_account_name,
    validate_credential_file, Account,
};
use crate::gcp::projects::{self, Project};
use crate::gcp::{compute, gcloud, GcpClient};
use crate::state::validate_zone;

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
    require_project(&project)?;
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
        prompt(
            "Default zone (name, or Enter to skip)",
            chosen.zone.as_deref(),
        )?
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
        let answer = prompt(
            "Default zone (number, name, or Enter to skip)",
            chosen.zone.as_deref(),
        )?;
        resolve_zone_choice(&answer, &zones)
    };
    check_zone(&zone, &zones, &account.project)?;
    let zoneless = zone.is_empty();
    account.default_zone = zone;

    update_or_init_config(|config| config.add_account(account))?;
    println!("Account '{name}' saved to {}", config_dir()?.display());
    if zoneless {
        println!("No default zone saved; pass --zone to commands that need one");
    }
    Ok(())
}

// A zone is only a default for commands that reach an instance, and a gcloud
// configuration often has none, so init must not make a person invent one. A
// typed zone is still judged against the list, but an empty list means the
// lookup failed, which is no verdict.
fn check_zone(zone: &str, listed: &[String], project: &str) -> Result<()> {
    if zone.is_empty() || listed.is_empty() || listed.iter().any(|z| z == zone) {
        return Ok(());
    }
    bail!("Zone '{zone}' does not exist in project '{project}'")
}

// The project is what every API call is addressed to, so unlike the zone it
// cannot be skipped
fn require_project(project: &str) -> Result<()> {
    if project.is_empty() {
        bail!("A project is required");
    }
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
    account_select(name)?;
    println!("Selected account '{name}'");
    Ok(())
}

// The cores below print nothing, so the TUI can call them under the alternate
// screen, where a println lands on top of the drawn page (decision 15)
pub fn account_select(name: &str) -> Result<()> {
    update_config(|config| config.select_account(name))
}

// A tunnel record names its account, so removing the account first would leave
// records that no command can reach. `--yes` answers the confirmation below and
// nothing else: it cannot answer what becomes of the tunnels, which is the one
// question a script cannot have settled by passing a flag.
pub fn cmd_account_remove(name: &str, assume_yes: bool) -> Result<()> {
    refuse_if_held(name)?;
    if !confirm(&format!("Remove account '{name}'?"), assume_yes)? {
        println!("Cancelled");
        return Ok(());
    }
    account_remove(name)?;
    println!("Removed account '{name}'");
    Ok(())
}

// Asks nothing: a caller that reaches it has already had the answer
pub fn account_remove(name: &str) -> Result<()> {
    refuse_if_held(name)?;
    update_config(|config| config.remove_account(name))?;
    Ok(())
}

fn refuse_if_held(name: &str) -> Result<()> {
    let state = crate::state::TunnelState::load()?;
    let held = state.tunnels_for_account(name).len();
    if held > 0 {
        bail!(
            "Account '{name}' still has {held} tunnel(s). Delete them with `gtunnel delete`, \
             or move them to another account with `gtunnel move`, then remove the account"
        );
    }
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

// Build an account from flags, with no gcloud call. The configuration name
// need not exist: gcloud still runs with it, but creates nothing and warns
// "Could not open the configuration file" on every call. gtunnel does not
// create it, because a gcloud configuration is the user's to make.
//
// An empty configuration is refused because gcloud reads `--configuration=` as
// "the active one": the tunnel would run as whoever is logged in to the shell,
// which is what pinning a configuration exists to prevent, and a CI script
// with an unset variable reaches it silently.
fn account_from_flags(configuration: &str, name: &str, opts: &AddOptions) -> Result<Account> {
    if configuration.is_empty() {
        return Err(GtunnelError::usage("--configuration cannot be empty").into());
    }
    let project = opts
        .project
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| GtunnelError::usage("--project cannot be empty"))?;
    if let Some(zone) = &opts.zone {
        validate_zone(zone).map_err(|e| GtunnelError::usage(e.to_string()))?;
    }
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
    account_add(configuration, name, opts).await?;
    println!("Account '{name}' saved to {}", config_dir()?.display());
    Ok(())
}

pub async fn account_add(configuration: &str, name: &str, opts: AddOptions) -> Result<()> {
    // Before the branch, so the gcloud path refuses it too: there it would
    // only surface as "no configuration named ''" after a gcloud call
    if configuration.is_empty() {
        return Err(GtunnelError::usage("--configuration cannot be empty").into());
    }
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
            validate_zone(zone).map_err(|e| GtunnelError::usage(e.to_string()))?;
            account.default_zone = zone.clone();
        }
        account
    };
    update_or_init_config(|config| config.add_account(account))
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
    let edited = account_edit(name, project, zone, credential_file)?;
    println!("Account '{name}' updated");
    if let Some(line) = stale_line(&edited.stale) {
        println!("{line}");
    }
    Ok(())
}

/// What an edit left behind: the installed tunnels whose service file still
/// carries a value the edit replaced.
#[derive(Debug, Default)]
pub struct AccountEdited {
    pub stale: Vec<String>,
}

/// The project and credential file an edit replaced, as (old, new). Only these
/// two reach a service file: the zone is not in the argv, because every
/// tunnel carries its own.
#[derive(Debug, Default)]
struct ServiceChange {
    project: Option<(String, String)>,
    credential_file: Option<(Option<String>, Option<String>)>,
}

// The core of `account edit`: it writes the config and reports, and prints
// nothing, so a caller that is not a terminal can use it (decision 15).
pub fn account_edit(
    name: &str,
    project: Option<&str>,
    zone: Option<&str>,
    credential_file: Option<&str>,
) -> Result<AccountEdited> {
    if project.is_none() && zone.is_none() && credential_file.is_none() {
        return Err(GtunnelError::usage(
            "Nothing to change: pass --project, --zone, or both (or --credential-file)",
        )
        .into());
    }
    if project == Some("") || zone == Some("") {
        return Err(GtunnelError::usage("A project or zone cannot be empty").into());
    }
    if let Some(file) = credential_file.filter(|f| !f.is_empty()) {
        validate_credential_file(std::path::Path::new(file))?;
    }
    let change = update_config(|config| {
        let account = config.get_account_mut(Some(name))?;
        let mut change = ServiceChange::default();
        if let Some(project) = project {
            change.project = Some((account.project.clone(), project.to_string()));
            account.project = project.to_string();
        }
        if let Some(zone) = zone {
            account.default_zone = zone.to_string();
        }
        if let Some(file) = credential_file {
            let new = (!file.is_empty()).then(|| std::path::PathBuf::from(file));
            change.credential_file = Some((
                account
                    .credential_file
                    .as_ref()
                    .map(|p| p.display().to_string()),
                new.as_ref().map(|p| p.display().to_string()),
            ));
            account.credential_file = new;
        }
        Ok(change)
    })?;
    let state = crate::state::TunnelState::load()?;
    let tunnels = state.tunnels_for_account(name);
    let stale = stale_tunnels(&tunnels, &change, |label| {
        crate::daemon::installed_service_text(label)
    });
    Ok(AccountEdited { stale })
}

// Names the tunnels whose installed service file still holds a replaced value.
// The test is whether the old value is in the file, and a value set for the
// first time has no old one to find, so there the flag's absence is the
// signal. A file that cannot be read is not installed, so it is not named.
fn stale_tunnels(
    tunnels: &[&crate::state::PersistentTunnel],
    change: &ServiceChange,
    read: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    const FLAG: &str = "--credential-file-override=";
    tunnels
        .iter()
        .filter(|t| {
            let Some(text) = read(&crate::daemon::platform_label(&t.account_name, &t.name)) else {
                return false;
            };
            let project_stale = change
                .project
                .as_ref()
                .is_some_and(|(old, new)| old != new && text.contains(&format!("--project={old}")));
            let credential_stale =
                change
                    .credential_file
                    .as_ref()
                    .is_some_and(|(old, new)| match old {
                        _ if old == new => false,
                        Some(old) => text.contains(&format!("{FLAG}{old}")),
                        None => !text.contains(FLAG),
                    });
            project_stale || credential_stale
        })
        .map(|t| t.name.clone())
        .collect()
}

// One line, so a script reading the output sees one extra line at most. No
// restart happens here: it drops live connections, which is the user's call.
pub fn stale_line(stale: &[String]) -> Option<String> {
    if stale.is_empty() {
        return None;
    }
    let restarts = stale
        .iter()
        .map(|n| format!("`gtunnel restart {n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Installed tunnels still use the old value: {}. Restart to apply: {restarts}",
        stale.join(", ")
    ))
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

    fn exit_of(err: &anyhow::Error) -> i32 {
        crate::error::kind_of(err).exit_code()
    }

    #[test]
    fn add_refuses_an_empty_configuration_as_a_usage_error() {
        let opts = AddOptions {
            project: Some("example-dev-1a2b".into()),
            ..AddOptions::default()
        };
        let err = account_from_flags("", "x", &opts).unwrap_err();
        assert_eq!(exit_of(&err), 2, "{err}");
        assert!(err.to_string().contains("--configuration"), "{err}");
    }

    #[tokio::test]
    async fn add_with_an_empty_configuration_exits_2_and_writes_nothing() {
        let dir = TempConfigDir::new();
        let opts = AddOptions {
            project: Some("p".into()),
            ..AddOptions::default()
        };
        let err = cmd_account_add("", "x", opts).await.unwrap_err();
        assert_eq!(exit_of(&err), 2, "{err}");
        assert!(!dir.path().join("config.toml").exists());
    }

    #[test]
    fn add_refuses_a_zone_that_is_not_one_as_a_usage_error() {
        let opts = AddOptions {
            project: Some("p".into()),
            zone: Some("not a zone".into()),
            ..AddOptions::default()
        };
        let err = account_from_flags("c", "x", &opts).unwrap_err();
        assert_eq!(exit_of(&err), 2, "{err}");
    }

    #[test]
    fn edit_refusals_are_usage_errors() {
        let _dir = TempConfigDir::new();
        save_accounts(&["dev"]);
        for err in [
            cmd_account_edit("dev", None, None, None).unwrap_err(),
            cmd_account_edit("dev", Some(""), None, None).unwrap_err(),
            cmd_account_edit("dev", None, Some(""), None).unwrap_err(),
        ] {
            assert_eq!(exit_of(&err), 2, "{err}");
        }
    }

    // The service file carries the value as it was at install time, so the
    // question is whether the old one is still in it. What is read is a
    // closure, so the test needs no service manager and no home directory.
    #[test]
    fn a_tunnel_whose_service_file_holds_the_old_credential_file_is_stale() {
        let a = tunnel("db", "ci");
        let b = tunnel("cache", "ci");
        let files = |label: &str| {
            let name = crate::daemon::platform_label("ci", "db");
            (label == name).then(|| {
                "--credential-file-override=/secrets/old.json --project=example-dev-1a2b"
                    .to_string()
            })
        };
        let change = ServiceChange {
            project: Some(("example-dev-1a2b".into(), "example-dev-1a2b".into())),
            credential_file: Some((Some("/secrets/old.json".into()), None)),
        };
        assert_eq!(stale_tunnels(&[&a, &b], &change, files), vec!["db"]);
    }

    #[test]
    fn a_credential_file_set_for_the_first_time_makes_an_installed_tunnel_stale() {
        let a = tunnel("db", "ci");
        let files = |_: &str| Some("--project=example-dev-1a2b".to_string());
        let change = ServiceChange {
            project: None,
            credential_file: Some((None, Some("/secrets/new.json".into()))),
        };
        assert_eq!(stale_tunnels(&[&a], &change, files), vec!["db"]);
    }

    #[test]
    fn a_changed_project_makes_the_tunnel_stale_and_an_unchanged_one_does_not() {
        let a = tunnel("db", "ci");
        let files = |_: &str| Some("--project=example-dev-1a2b".to_string());
        let moved = ServiceChange {
            project: Some(("example-dev-1a2b".into(), "example-dev-9z9z".into())),
            credential_file: None,
        };
        assert_eq!(stale_tunnels(&[&a], &moved, files), vec!["db"]);
        let same = ServiceChange {
            project: Some(("example-dev-1a2b".into(), "example-dev-1a2b".into())),
            credential_file: None,
        };
        assert!(stale_tunnels(&[&a], &same, files).is_empty());
    }

    #[test]
    fn an_edit_that_touches_neither_project_nor_credential_names_nothing() {
        let a = tunnel("db", "ci");
        let files = |_: &str| Some("--project=example-dev-1a2b".to_string());
        assert!(stale_tunnels(&[&a], &ServiceChange::default(), files).is_empty());
    }

    #[test]
    fn the_stale_line_names_the_tunnels_and_the_restart_command() {
        let line = stale_line(&["db".to_string(), "cache".to_string()]).expect("a line");
        assert!(line.contains("db, cache"), "{line}");
        assert!(line.contains("gtunnel restart"), "{line}");
        assert_eq!(stale_line(&[]), None);
    }

    // Through the real core, with a plist written where launchd's job for
    // this config directory would be. The other platform keeps its units in
    // the home directory, which a test must not write.
    #[cfg(target_os = "macos")]
    #[test]
    fn editing_the_credential_file_names_the_installed_tunnel_and_nothing_else() {
        let dir = TempConfigDir::new();
        let mut config = Config::default();
        let mut ci = account("ci");
        ci.credential_file = Some("/secrets/old.json".into());
        config.add_account(ci).expect("fixture adds");
        save_config(&config).expect("fixture saves");
        let mut state = TunnelState::default();
        state.add(tunnel("db", "ci")).expect("fixture adds");
        state.add(tunnel("cache", "ci")).expect("fixture adds");
        state.save().expect("fixture saves");

        // nothing installed: nothing to name
        let none = account_edit("ci", None, None, Some("/secrets/new.json")).expect("edits");
        assert!(none.stale.is_empty(), "{:?}", none.stale);

        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("com.gtunnel.ci.db.plist"),
            "<string>--credential-file-override=/secrets/new.json</string>",
        )
        .unwrap();
        let got = account_edit("ci", None, None, Some("/secrets/newer.json")).expect("edits");
        assert_eq!(got.stale, vec!["db"]);
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

    // The edit that repairs a relative path has to be able to load the file that
    // holds it, and `account add` on such a file must not swallow the others
    #[tokio::test]
    async fn a_relative_credential_file_can_be_cleared_and_never_costs_the_other_accounts() {
        let _dir = TempConfigDir::new();
        save_accounts(&["ci", "other"]);
        let mut config = load_config().unwrap();
        config.accounts[0].credential_file = Some("~/ci-key.json".into());
        save_config(&config).unwrap();
        let opts = AddOptions {
            project: Some("example-dev-1a2b".into()),
            ..AddOptions::default()
        };
        cmd_account_add("new", "new", opts).await.expect("adds");
        assert_eq!(load_config().unwrap().accounts.len(), 3);
        cmd_account_edit("ci", None, None, Some("")).expect("clears");
        assert_eq!(
            load_config()
                .unwrap()
                .get_account(Some("ci"))
                .unwrap()
                .credential_file,
            None
        );
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
    fn check_zone_allows_an_empty_zone() {
        check_zone("", &["europe-west1-b".to_string()], "example-dev-1a2b")
            .expect("a skipped zone is not an error");
    }

    #[test]
    fn check_zone_refuses_a_zone_the_project_lacks() {
        let err = check_zone(
            "us-east1-b",
            &["europe-west1-b".to_string()],
            "example-dev-1a2b",
        )
        .expect_err("not in the list");
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn check_zone_trusts_a_missing_list() {
        check_zone("us-east1-b", &[], "example-dev-1a2b").expect("no list, no verdict");
    }

    #[test]
    fn project_stays_required() {
        let err = require_project("").expect_err("empty project");
        assert_eq!(err.to_string(), "A project is required");
        require_project("example-dev-1a2b").expect("a project passes");
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
