use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const REPO: &str = "gcheliz/gtunnel";
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

fn parse_version(v: &str) -> (u64, u64, u64) {
    let parts: Vec<u64> = v
        .trim_start_matches('v')
        .split('.')
        .filter_map(|p| p.parse().ok())
        .collect();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

fn is_newer(current: &str, latest: &str) -> bool {
    parse_version(latest) > parse_version(current)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateCache {
    latest_version: String,
    checked_at: u64,
}

fn cache_path() -> Option<PathBuf> {
    Some(crate::config::config_dir().ok()?.join("update-check.json"))
}

fn read_cache() -> Option<UpdateCache> {
    serde_json::from_str(&std::fs::read_to_string(cache_path()?).ok()?).ok()
}

fn write_cache(cache: &UpdateCache) -> Result<()> {
    let path = cache_path().context("Could not determine the cache path")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(cache)?)?;
    Ok(())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn user_agent() -> String {
    format!("gtunnel/{}", env!("CARGO_PKG_VERSION"))
}

async fn fetch_latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = reqwest::Client::new()
        .get(&url)
        .header("User-Agent", user_agent())
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("Failed to reach the GitHub API")?;
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    parse_latest_tag(status, &body)
}

// Split from the request so every answer GitHub can give has a message the
// user can act on
fn parse_latest_tag(status: u16, body: &serde_json::Value) -> Result<String> {
    if status == 404 {
        anyhow::bail!("{REPO} has no published releases yet, so there is nothing to update to");
    }
    if status != 200 {
        let message = body["message"].as_str().unwrap_or("no message");
        anyhow::bail!("The GitHub API answered {status}: {message}");
    }
    let tag = body["tag_name"]
        .as_str()
        .context("The latest release has no tag_name")?;
    Ok(tag.strip_prefix('v').unwrap_or(tag).to_string())
}

// The Rust target triple, which is also what the release assets are named after
pub fn platform_target() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Some("aarch64-apple-darwin");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Some("x86_64-apple-darwin");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some("x86_64-unknown-linux-musl");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some("aarch64-unknown-linux-musl");
    #[allow(unreachable_code)]
    None
}

pub fn asset_name(target: &str) -> String {
    format!("gtunnel-{target}.tar.gz")
}

enum InstallMethod {
    Homebrew,
    Cargo,
    Binary(PathBuf),
}

fn detect_install_method() -> InstallMethod {
    let exe = std::env::current_exe().unwrap_or_default();
    let path = exe.to_string_lossy();
    if path.contains("/Cellar/") || path.contains("/homebrew/") {
        InstallMethod::Homebrew
    } else if path.contains("/.cargo/bin/") {
        InstallMethod::Cargo
    } else {
        InstallMethod::Binary(exe)
    }
}

pub async fn cmd_update(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("Checking for updates...");
    let latest = fetch_latest_version().await?;

    let _ = write_cache(&UpdateCache {
        latest_version: latest.clone(),
        checked_at: now_secs(),
    });

    if !is_newer(current, &latest) {
        eprintln!("gtunnel v{current} is already the latest version.");
        return Ok(());
    }
    eprintln!("Update available: v{current} -> v{latest}");
    if check_only {
        eprintln!("\nRun `gtunnel update` to install.");
        return Ok(());
    }

    match detect_install_method() {
        InstallMethod::Homebrew => {
            eprintln!("\ngtunnel was installed with Homebrew. Run:");
            eprintln!("  brew upgrade gtunnel");
        }
        InstallMethod::Cargo => {
            eprintln!("\ngtunnel was installed with cargo. Run:");
            eprintln!("  cargo install gtunnel");
        }
        InstallMethod::Binary(exe) => perform_update(&exe, &latest).await?,
    }
    Ok(())
}

// Reads the cache only: a CLI command must never wait on the network to print
// its own output
pub fn maybe_print_update_hint() {
    let current = env!("CARGO_PKG_VERSION");
    if let Some(cache) = read_cache() {
        if is_newer(current, &cache.latest_version) {
            eprintln!(
                "\ngtunnel v{} is available (current: v{current}). Run `gtunnel update`.",
                cache.latest_version
            );
            return;
        }
        if now_secs().saturating_sub(cache.checked_at) < CHECK_INTERVAL_SECS {
            return;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .args(["update", "--check"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

// "<hex>  <filename>", as shasum and sha256sum both write it
pub fn parse_sha256_file(text: &str) -> Option<String> {
    text.split_whitespace().next().map(|s| s.to_string())
}

fn sha256_of(path: &Path) -> Result<String> {
    for (program, args) in [("shasum", vec!["-a", "256"]), ("sha256sum", vec![])] {
        let mut command = std::process::Command::new(program);
        command.args(&args).arg(path);
        if let Ok(out) = command.output() {
            if out.status.success() {
                return parse_sha256_file(&String::from_utf8_lossy(&out.stdout))
                    .context("Could not read the checksum output");
            }
        }
    }
    anyhow::bail!("Neither shasum nor sha256sum is on PATH")
}

async fn get(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::Client::new()
        .get(url)
        .header("User-Agent", user_agent())
        .send()
        .await
        .with_context(|| format!("Failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("Failed to download {url}"))?;
    Ok(response.bytes().await?.to_vec())
}

async fn perform_update(exe_path: &Path, version: &str) -> Result<()> {
    let target = platform_target().context("This platform has no self-update build")?;
    let asset = asset_name(target);
    let base = format!("https://github.com/{REPO}/releases/download/v{version}");

    eprintln!("Downloading {asset}...");
    let tmp = std::env::temp_dir().join(format!("gtunnel-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let _cleanup = TempDirGuard(tmp.clone());

    let archive = tmp.join(&asset);
    std::fs::write(&archive, &get(&format!("{base}/{asset}")).await?)?;

    // A self-updater overwrites the binary it is running from, so the download
    // is checked against the published digest before it is trusted
    let published = parse_sha256_file(&String::from_utf8_lossy(
        &get(&format!("{base}/{asset}.sha256")).await?,
    ))
    .context("The release has no usable .sha256 file")?;
    let actual = sha256_of(&archive)?;
    if !actual.eq_ignore_ascii_case(&published) {
        anyhow::bail!("Checksum mismatch for {asset}: expected {published}, got {actual}");
    }

    let status = std::process::Command::new("tar")
        .args([
            "xzf",
            &archive.to_string_lossy(),
            "-C",
            &tmp.to_string_lossy(),
        ])
        .status()
        .context("Failed to run tar")?;
    if !status.success() {
        anyhow::bail!("tar could not extract {asset}");
    }

    let new_bin = tmp.join("gtunnel");
    if !new_bin.exists() {
        anyhow::bail!("{asset} does not contain a gtunnel binary");
    }
    replace_binary(&new_bin, exe_path)?;
    eprintln!("Updated gtunnel to v{version}");
    Ok(())
}

fn replace_binary(new_bin: &Path, exe_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new_bin, std::fs::Permissions::from_mode(0o755))?;
        if std::fs::rename(new_bin, exe_path).is_err() {
            std::fs::copy(new_bin, exe_path)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (new_bin, exe_path);
        anyhow::bail!("Self-update works on macOS and Linux only")
    }
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert_eq!(parse_version("1.2.3"), (1, 2, 3));
        assert_eq!(parse_version("v0.1.0"), (0, 1, 0));
        assert_eq!(parse_version("10.0.1"), (10, 0, 1));
        // "10" sorts before "9" as text, and that is the bug this guards
        assert!(is_newer("0.9.0", "0.10.0"));
        assert!(is_newer("1.2.3", "1.2.4"));
        assert!(!is_newer("1.2.3", "1.2.3"));
        assert!(!is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn this_platform_has_a_release_asset() {
        let target = platform_target().expect("a target for this host");
        assert_eq!(asset_name(target), format!("gtunnel-{target}.tar.gz"));
    }

    // A project whose first release is not published yet answers 404, and the
    // user must read that as "nothing to update to", not as a broken tool
    #[test]
    fn a_repository_without_releases_says_so() {
        let err = parse_latest_tag(404, &serde_json::json!({"message": "Not Found"}))
            .expect_err("404 is not a version");
        let text = format!("{err:#}");
        assert!(text.contains("no published releases"), "{text}");
        assert!(text.contains(REPO), "{text}");

        let err = parse_latest_tag(500, &serde_json::json!({})).expect_err("500 is not a version");
        assert!(format!("{err:#}").contains("500"), "{err:#}");

        assert_eq!(
            parse_latest_tag(200, &serde_json::json!({"tag_name": "v1.4.0"})).unwrap(),
            "1.4.0"
        );
    }

    #[test]
    fn the_checksum_line_is_read_from_the_published_file() {
        assert_eq!(
            parse_sha256_file("abc123  gtunnel-aarch64-apple-darwin.tar.gz\n"),
            Some("abc123".to_string())
        );
        assert_eq!(parse_sha256_file("   \n"), None);
    }
}
