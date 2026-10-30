use crate::error::GtunnelError;
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
    let mut cmd = Command::new(&gcloud);
    cmd.args(args).stdin(Stdio::null());
    let what = format!("gcloud {}", args.join(" "));
    let out = crate::proc::output_within(cmd, crate::proc::GCLOUD_BOUND, &what)
        .await
        .with_context(|| format!("Failed to run {}", gcloud.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(GtunnelError::gcloud(format!(
            "gcloud {} failed: {}",
            args.join(" "),
            stderr.trim()
        ))
        .into());
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
    )?);
    // A token that cannot be minted is a credential problem whatever gcloud
    // printed, so this is the one place a gcloud failure is reclassified
    let stdout = run_gcloud(&args).await.map_err(|e| {
        GtunnelError::auth(format!(
            "Could not get a gcloud access token. {}: {e:#}",
            auth_advice(acct, is_interactive())
        ))
    })?;
    Ok(stdout.trim().to_string())
}

// What to do about a missing token depends on where the credentials come from
// and on whether anyone is there to answer a login. A runner with no terminal
// must not be told to open a browser, and a credential-file account must not
// be told to log in at all: that would sign in a person instead of the key.
pub fn auth_advice(acct: &Account, interactive: bool) -> String {
    if let Some(file) = &acct.credential_file {
        return credential_file_advice(&file.display().to_string());
    }
    let cfg = &acct.gcloud_configuration;
    if interactive {
        format!("Run `gcloud auth login --configuration={cfg}` and try again")
    } else {
        format!(
            "No terminal to log in from. On a workstation run `gcloud auth login \
             --configuration={cfg}`; on a CI runner, give the runner a service account with \
             roles/iap.tunnelResourceAccessor, or set credential_file on the account"
        )
    }
}

// Shared with doctor, which has the path as text and no account at hand
pub fn credential_file_advice(path: &str) -> String {
    format!(
        "The credential file {path} gave no token. Check that it exists, is a service-account \
         key or a workload identity federation file, and that its identity is not disabled"
    )
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
// `can_login` is false for an account that names a credential file: a login
// would authenticate a person, never the key, so a failed probe is final.
pub fn next_step(probe_ok: bool, interactive: bool, can_login: bool) -> AuthStep {
    match (probe_ok, interactive && can_login) {
        (true, _) => AuthStep::Proceed,
        (false, true) => AuthStep::Login,
        (false, false) => AuthStep::Fail,
    }
}

// Both ends must be a terminal: the login prints a URL to read and takes a
// verification code typed back, and a redirected stdout hides the first
pub fn is_interactive() -> bool {
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
#[allow(clippy::print_stdout, clippy::print_stderr)]
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

// gcloud's own words for why the token failed, shown by `run` and `doctor`
// alike. The probe error embeds stderr after "failed: ". gcloud leads with an
// INFO line naming the file, which is no reason, so the first ERROR line wins
// and the first non-empty line is the fallback.
pub fn gcloud_reason(msg: &str) -> Option<String> {
    let (_, stderr) = msg.split_once("failed: ")?;
    let mut lines = stderr.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.clone().next();
    lines
        .find(|l| l.starts_with("ERROR"))
        .or(first)
        .map(str::to_string)
}

// Checks credentials against GCP and asks confirmation before opening a browser
// when interactive. Non-interactive sessions fail immediately without prompting.
pub async fn ensure_logged_in_with<F>(acct: &Account, interactive: bool, confirm: F) -> Result<()>
where
    F: FnOnce(&str) -> Result<bool>,
{
    // A relative path is a usage error, not a credential failure: the probe
    // below folds every error into an auth exit and would hide that
    acct.credential_file_for_use()?;
    let probe = print_access_token(acct).await;
    let probe_ok = probe.is_ok();
    match next_step(probe_ok, interactive, acct.credential_file.is_none()) {
        AuthStep::Proceed => return Ok(()),
        AuthStep::Login => {
            if confirm(&acct.name)? {
                // Only reached when `interactive`, which the caller vouches for
                #[allow(clippy::print_stderr)]
                {
                    eprintln!("Starting `gcloud auth login`.");
                }
                // A login that exits 0 proves nothing; only a second probe does
                if run_login(acct).await.is_ok() && print_access_token(acct).await.is_ok() {
                    return Ok(());
                }
            }
        }
        AuthStep::Fail => {}
    }
    let advice = auth_advice(acct, interactive);
    Err(GtunnelError::auth(if acct.credential_file.is_some() {
        let reason = probe
            .err()
            .and_then(|e| gcloud_reason(&format!("{e:#}")))
            .map(|r| format!("; gcloud said: {r}"))
            .unwrap_or_default();
        format!(
            "gtunnel could not get a token for '{}'. {advice}{reason}",
            acct.name
        )
    } else {
        format!(
            "gtunnel needs a logged-in Google account for '{}'. {advice}",
            acct.name
        )
    })
    .into())
}

#[cfg(test)]
pub(crate) mod tests {
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
            credential_file: None,
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
        assert_eq!(next_step(true, true, true), AuthStep::Proceed);
        assert_eq!(next_step(true, false, true), AuthStep::Proceed);
    }

    #[test]
    fn a_failed_probe_on_a_terminal_starts_the_login() {
        assert_eq!(next_step(false, true, true), AuthStep::Login);
    }

    #[test]
    fn a_failed_probe_without_a_terminal_fails() {
        assert_eq!(next_step(false, false, true), AuthStep::Fail);
    }

    #[test]
    fn a_credential_file_account_never_logs_in() {
        assert_eq!(next_step(false, true, false), AuthStep::Fail);
        assert_eq!(next_step(false, true, true), AuthStep::Login);
        assert_eq!(next_step(true, false, false), AuthStep::Proceed);
    }

    #[test]
    fn advice_without_a_terminal_does_not_send_a_runner_to_a_browser() {
        let text = auth_advice(&account(None), false);
        assert!(
            text.contains("CI runner") && text.contains("credential_file"),
            "{text}"
        );
        assert!(text.contains("--configuration=__gtunnel_test_nonexistent__"));
    }

    #[test]
    fn advice_on_a_terminal_names_the_login_for_the_configuration() {
        assert_eq!(
            auth_advice(&account(None), true),
            "Run `gcloud auth login --configuration=__gtunnel_test_nonexistent__` and try again"
        );
    }

    #[test]
    fn advice_for_a_credential_file_names_the_file_not_gcloud_auth_login() {
        let mut acct = account(None);
        acct.credential_file = Some("/secrets/ci-key.json".into());
        for interactive in [true, false] {
            let text = auth_advice(&acct, interactive);
            assert!(
                text.contains("/secrets/ci-key.json") && !text.contains("gcloud auth login"),
                "{text}"
            );
        }
    }

    #[tokio::test]
    async fn a_credential_file_account_fails_without_prompting_even_on_a_terminal() {
        let mut acct = account(None);
        acct.credential_file = Some("/nonexistent/ci-key.json".into());
        let mut prompted = false;
        let err = ensure_logged_in_with(&acct, true, |_| {
            prompted = true;
            Ok(true)
        })
        .await
        .expect_err("a bad key must not fall back to a person's login");
        assert!(
            !prompted,
            "a browser login would sign in a person, not the key"
        );
        assert!(
            err.to_string().contains("/nonexistent/ci-key.json"),
            "{err}"
        );
    }

    #[test]
    fn gcloud_reason_is_the_first_non_empty_line_after_the_failure() {
        let msg = "Could not get a gcloud access token. Check it: gcloud auth \
                   print-access-token failed: \nINFO: Using file\nERROR: File /secrets/ci-key.json was not found.\nmore";
        assert_eq!(
            gcloud_reason(msg).as_deref(),
            Some("ERROR: File /secrets/ci-key.json was not found.")
        );
        assert_eq!(gcloud_reason("no marker here"), None);
    }

    #[tokio::test]
    async fn a_relative_credential_file_is_a_usage_error_not_an_auth_failure() {
        let mut acct = account(None);
        acct.credential_file = Some("~/ci-key.json".into());
        let err = ensure_logged_in_with(&acct, false, |_| Ok(false))
            .await
            .expect_err("relative");
        assert_eq!(
            crate::error::kind_of(&err),
            crate::error::ErrorKind::Usage,
            "{err:#}"
        );
    }

    // `find_gcloud` reads PATH and nothing else, so PATH is the only seam for
    // a stand-in. It is process-wide, hence the lock: two tests swapping it
    // would each see the other's script. The real PATH stays behind the fake
    // so `sh` and friends still resolve.
    static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) struct FakeGcloudOnPath {
        original: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl FakeGcloudOnPath {
        pub(crate) fn new(body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let guard = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().expect("tempdir");
            let script = dir.path().join("gcloud");
            std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let original = std::env::var_os("PATH");
            let mut dirs = vec![dir.path().to_path_buf()];
            dirs.extend(std::env::split_paths(&original.clone().unwrap_or_default()));
            let joined = std::env::join_paths(dirs).expect("join PATH");
            // SAFETY: every test that rewrites PATH holds PATH_LOCK through
            // `_guard`; std serialises its own environment reads against this.
            unsafe { std::env::set_var("PATH", joined) };
            Self {
                original,
                _dir: dir,
                _guard: guard,
            }
        }
    }

    impl Drop for FakeGcloudOnPath {
        fn drop(&mut self) {
            // SAFETY: `_guard` is still held while this body runs.
            unsafe {
                match &self.original {
                    Some(p) => std::env::set_var("PATH", p),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    async fn missing_credential_file_error() -> String {
        let mut acct = account(None);
        acct.credential_file = Some("/nonexistent/ci-key.json".into());
        ensure_logged_in_with(&acct, false, |_| Ok(false))
            .await
            .expect_err("a missing key cannot sign in")
            .to_string()
    }

    // The wording is gcloud's, so it is pinned in a stand-in: asserting it
    // against whatever gcloud a runner happens to have made CI flaky
    #[tokio::test]
    async fn a_missing_credential_file_error_carries_gcloud_s_reason() {
        let _fake = FakeGcloudOnPath::new(
            "echo 'ERROR: (gcloud.auth.print-access-token) File /nonexistent/ci-key.json was not found.' >&2; exit 1",
        );
        let err = missing_credential_file_error().await;
        assert!(err.contains("was not found"), "{err}");
    }

    // Run by hand against the installed gcloud to catch a change in its wording:
    // cargo test -- --ignored the_real_gcloud_words_a_missing_credential_file
    #[tokio::test]
    #[ignore = "needs the real gcloud on PATH; run by hand"]
    async fn the_real_gcloud_words_a_missing_credential_file_as_not_found() {
        assert!(find_gcloud().is_some(), "no gcloud on PATH");
        let err = missing_credential_file_error().await;
        assert!(err.contains("was not found"), "{err}");
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
