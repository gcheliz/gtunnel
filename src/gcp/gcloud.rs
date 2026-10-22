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
    args.extend(crate::tunnel::gcloud_common(acct));
    let stdout = run_gcloud(&args)
        .await
        .context("Could not get a gcloud access token. Run `gcloud auth login` and try again.")?;
    Ok(stdout.trim().to_string())
}
