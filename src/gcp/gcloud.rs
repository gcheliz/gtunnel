use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use crate::config::Account;

// A gcloud named configuration, as `gcloud config configurations list` reports it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcloudConfiguration {
    pub name: String,
    pub is_active: bool,
    pub project: Option<String>,
    pub zone: Option<String>,
    pub account: Option<String>,
}

#[derive(Deserialize)]
struct RawConfiguration {
    name: String,
    #[serde(default)]
    is_active: bool,
    #[serde(default)]
    properties: RawProperties,
}

#[derive(Deserialize, Default)]
struct RawProperties {
    #[serde(default)]
    core: RawCore,
    #[serde(default)]
    compute: RawCompute,
}

#[derive(Deserialize, Default)]
struct RawCore {
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    account: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawCompute {
    #[serde(default)]
    zone: Option<String>,
}

pub fn parse_configurations(json: &str) -> Result<Vec<GcloudConfiguration>> {
    let raw: Vec<RawConfiguration> =
        serde_json::from_str(json).context("Failed to parse gcloud configurations")?;
    Ok(raw
        .into_iter()
        .map(|r| GcloudConfiguration {
            name: r.name,
            is_active: r.is_active,
            project: r.properties.core.project,
            zone: r.properties.compute.zone,
            account: r.properties.core.account,
        })
        .collect())
}

// Where gcloud lives. Daemons do not inherit an interactive PATH, so the
// absolute path is what gets written into the service file.
pub fn find_gcloud() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("gcloud");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for fallback in [
        "/opt/homebrew/bin/gcloud",
        "/usr/local/bin/gcloud",
        "/usr/lib/google-cloud-sdk/bin/gcloud",
        "/snap/bin/gcloud",
    ] {
        let candidate = PathBuf::from(fallback);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn require_gcloud() -> Result<PathBuf> {
    find_gcloud().context(
        "gcloud was not found on PATH. Install the Google Cloud SDK: \
         https://cloud.google.com/sdk/docs/install",
    )
}

async fn run_gcloud(args: &[String]) -> Result<String> {
    let gcloud = require_gcloud()?;
    let out = Command::new(&gcloud)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("Failed to run {}", gcloud.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("gcloud {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn list_configurations() -> Result<Vec<GcloudConfiguration>> {
    let stdout = run_gcloud(&[
        "config".to_string(),
        "configurations".to_string(),
        "list".to_string(),
        "--format=json".to_string(),
    ])
    .await?;
    parse_configurations(&stdout)
}

// An OAuth access token for the account's gcloud configuration
pub async fn print_access_token(acct: &Account) -> Result<String> {
    let mut args = vec!["auth".to_string(), "print-access-token".to_string()];
    args.extend(crate::tunnel::gcloud_common(
        acct,
        crate::loglevel::current(),
    ));
    let stdout = run_gcloud(&args)
        .await
        .context("Could not get a gcloud access token. Run `gcloud auth login` and try again.")?;
    Ok(stdout.trim().to_string())
}

// What startup does about credentials, decided apart from the I/O so the
// three outcomes are testable without a terminal or a gcloud
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStep {
    Proceed,
    Login,
    Fail,
}

// A browser login cannot finish under cron, a pipe or CI, so without a
// terminal a failed probe is a failure rather than a prompt nobody can answer
pub fn next_step(probe_ok: bool, interactive: bool) -> AuthStep {
    match (probe_ok, interactive) {
        (true, _) => AuthStep::Proceed,
        (false, true) => AuthStep::Login,
        (false, false) => AuthStep::Fail,
    }
}

// Both ends must be a terminal: the login prints a URL to read and takes a
// verification code typed back, and a redirected stdout hides the first
fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

// No --project and no --impersonate-service-account: `auth login` rejects
// impersonation, and the project is not something a login stores. It fixes
// the user credentials the impersonated token is minted from.
pub fn login_argv(acct: &Account) -> Vec<String> {
    vec![
        "auth".to_string(),
        "login".to_string(),
        format!("--configuration={}", acct.gcloud_configuration),
    ]
}

// Unlike `run_gcloud`, stdio is inherited: the login prompts, prints a URL
// and reads a code, none of which works with stdin nulled and output captured
async fn run_login(acct: &Account) -> Result<()> {
    let gcloud = require_gcloud()?;
    let status = Command::new(&gcloud)
        .args(login_argv(acct))
        .status()
        .await
        .with_context(|| format!("Failed to run {}", gcloud.display()))?;
    if !status.success() {
        bail!("`gcloud auth login` did not complete ({status})");
    }
    Ok(())
}

// Explains why credentials are required and asks for confirmation before
// launching a browser login, preventing unexpected window popups
pub fn confirm_login(account_name: &str) -> Result<bool> {
    eprintln!("gcloud has no usable credentials for account '{account_name}'.");
    eprintln!("gtunnel requires an authenticated GCP account to establish tunnels.");
    crate::commands::confirm("Do you want to log in now?", false)
}

// Runs before the TUI takes the screen, so the login's output lands on a
// normal terminal and nothing prints after raw mode starts
pub async fn ensure_logged_in(acct: &Account) -> Result<()> {
    ensure_logged_in_with(acct, is_interactive(), confirm_login).await
}

// Checks credentials against GCP and asks confirmation before opening a browser
// when interactive. Non-interactive sessions fail immediately without prompting.
pub async fn ensure_logged_in_with<F>(acct: &Account, interactive: bool, confirm: F) -> Result<()>
where
    F: FnOnce(&str) -> Result<bool>,
{
    let probe_ok = print_access_token(acct).await.is_ok();
    match next_step(probe_ok, interactive) {
        AuthStep::Proceed => return Ok(()),
        AuthStep::Login => {
            if confirm(&acct.name)? {
                eprintln!("Starting `gcloud auth login`.");
                // A login that exits 0 proves nothing; only a second probe does
                if run_login(acct).await.is_ok() && print_access_token(acct).await.is_ok() {
                    return Ok(());
                }
            }
        }
        AuthStep::Fail => {}
    }
    bail!(
        "gtunnel needs a logged-in Google account for '{}'. Run `gcloud auth login`",
        acct.name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // A nonexistent configuration ensures print_access_token cannot succeed
    // against a live developer config during unit tests
    fn account(impersonate: Option<&str>) -> Account {
        Account {
            name: "prod".to_string(),
            gcloud_configuration: "__gtunnel_test_nonexistent__".to_string(),
            project: "my-project".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: impersonate.map(str::to_string),
            expose: None,
        }
    }

    #[test]
    fn login_names_the_configuration_and_nothing_else() {
        assert_eq!(
            login_argv(&account(None)),
            vec![
                "auth",
                "login",
                "--configuration=__gtunnel_test_nonexistent__"
            ]
        );
    }

    #[test]
    fn login_never_carries_impersonation_or_a_project() {
        let argv = login_argv(&account(Some("sa@my-project.iam.gserviceaccount.com")));
        assert!(!argv.iter().any(|a| a.contains("impersonate")));
        assert!(!argv.iter().any(|a| a.contains("--project")));
    }

    #[test]
    fn a_passing_probe_proceeds_with_or_without_a_terminal() {
        assert_eq!(next_step(true, true), AuthStep::Proceed);
        assert_eq!(next_step(true, false), AuthStep::Proceed);
    }

    #[test]
    fn a_failed_probe_on_a_terminal_starts_the_login() {
        assert_eq!(next_step(false, true), AuthStep::Login);
    }

    #[test]
    fn a_failed_probe_without_a_terminal_fails() {
        assert_eq!(next_step(false, false), AuthStep::Fail);
    }

    #[tokio::test]
    async fn interactive_login_aborts_if_user_declines_confirmation() {
        let acct = account(None);
        let mut prompted = false;
        let res = ensure_logged_in_with(&acct, true, |name| {
            prompted = true;
            assert_eq!(name, "prod");
            Ok(false)
        })
        .await;

        assert!(
            prompted,
            "User must be prompted when credentials probe fails"
        );
        let err = res.expect_err("ensure_logged_in_with must fail when confirmation is declined");
        assert!(
            err.to_string()
                .contains("gtunnel needs a logged-in Google account for 'prod'"),
            "Unexpected error message: {err}"
        );
    }

    #[tokio::test]
    async fn non_interactive_login_never_prompts_for_confirmation() {
        let acct = account(None);
        let mut prompted = false;
        let res = ensure_logged_in_with(&acct, false, |_| {
            prompted = true;
            Ok(true)
        })
        .await;

        assert!(!prompted, "Non-interactive sessions must never prompt");
        let err = res.expect_err("ensure_logged_in_with must fail without a terminal");
        assert!(
            err.to_string()
                .contains("gtunnel needs a logged-in Google account for 'prod'"),
            "Unexpected error message: {err}"
        );
    }
}
