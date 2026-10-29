use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const REPO: &str = "gcheliz/gtunnel";

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
    // When the last check failed, if it did. A cache written before this field
    // existed has none, and reads as "never failed"
    #[serde(default)]
    failed_at: Option<u64>,
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

// Keeps what the last success learned, so a failed check does not erase a
// notice that is still true. With no cache yet the empty version is never
// newer than anything, and `checked_at` 0 makes the failure the newer event
fn record_failure(now: u64) {
    let cache = match read_cache() {
        Some(cache) => UpdateCache {
            failed_at: Some(now),
            ..cache
        },
        None => UpdateCache {
            latest_version: String::new(),
            checked_at: 0,
            failed_at: Some(now),
        },
    };
    let _ = write_cache(&cache);
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
    let response = crate::http::client_builder()
        .build()
        .context("Failed to build the HTTP client")?
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

// What the release asset for this host is called. Not the Rust target triple:
// a triple puts `unknown` in the vendor slot and spells amd64 `x86_64`, which
// is exact but tells a visitor on the releases page nothing about which file
// is theirs. release.yml maps each triple it builds to one of these
pub fn platform_slug() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Some("macos-arm64");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Some("macos-intel");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some("linux-amd64");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some("linux-arm64");
    #[allow(unreachable_code)]
    None
}

pub fn asset_name(target: &str, version: &str) -> String {
    let version = version.strip_prefix('v').unwrap_or(version);
    format!("gtunnel-{target}-v{version}.tar.gz")
}

enum InstallMethod {
    Homebrew,
    Binary(PathBuf),
}

// What to tell someone whose copy a package manager owns, or None when the binary is
// ours to replace.
fn upgrade_hint(method: &InstallMethod) -> Option<String> {
    match method {
        InstallMethod::Homebrew => {
            Some("gtunnel was installed with Homebrew. Run:\n  brew upgrade gtunnel".to_string())
        }
        InstallMethod::Binary(_) => None,
    }
}

fn detect_install_method() -> InstallMethod {
    let exe = std::env::current_exe().unwrap_or_default();
    detect_install_method_from_path(&exe)
}

fn detect_install_method_from_path(exe: &Path) -> InstallMethod {
    let path = exe.to_string_lossy();
    if path.contains("/Cellar/") || path.contains("/homebrew/") {
        InstallMethod::Homebrew
    } else {
        InstallMethod::Binary(exe.to_path_buf())
    }
}

pub async fn cmd_update(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("Checking for updates...");
    let latest = match fetch_latest_version().await {
        Ok(latest) => latest,
        Err(e) => {
            record_failure(now_secs());
            return Err(e);
        }
    };

    let _ = write_cache(&UpdateCache {
        latest_version: latest.clone(),
        checked_at: now_secs(),
        failed_at: None,
    });

    if !is_newer(current, &latest) {
        eprintln!("gtunnel v{current} is already the latest version.");
        return Ok(());
    }
    eprintln!("Update available: v{current} -> v{latest}");
    let method = detect_install_method();
    if check_only {
        eprintln!("\nRun `{}` to install.", install_command(&method));
        return Ok(());
    }

    match upgrade_hint(&method) {
        Some(hint) => eprintln!("\n{hint}"),
        None => {
            let InstallMethod::Binary(exe) = method else {
                unreachable!("only a downloaded binary has no upgrade hint")
            };
            perform_update(&exe, &latest).await?
        }
    }
    Ok(())
}

/// What the TUI and the CLI both say when a newer release is cached. One type
/// so the two surfaces cannot name different commands for the same install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    pub latest: String,
    pub command: &'static str,
}

// Pure so the install method and the versions are arguments, not the test
// runner's own path and Cargo.toml
fn notice_for(current: &str, latest: &str, method: &InstallMethod) -> Option<UpdateNotice> {
    if !is_newer(current, latest) {
        return None;
    }
    Some(UpdateNotice {
        latest: latest.trim_start_matches('v').to_string(),
        command: install_command(method),
    })
}

// What installs a newer version for this copy: Homebrew owns its own files, so
// `gtunnel update` there only redirects to brew
fn install_command(method: &InstallMethod) -> &'static str {
    match method {
        InstallMethod::Homebrew => "brew upgrade gtunnel",
        InstallMethod::Binary(_) => "gtunnel update",
    }
}

/// A file read and nothing more: the TUI calls this on its own thread, where a
/// network wait would freeze the screen.
pub fn available_update() -> Option<UpdateNotice> {
    let cache = read_cache()?;
    notice_for(
        env!("CARGO_PKG_VERSION"),
        &cache.latest_version,
        &detect_install_method(),
    )
}

// A cache dated in the future is not due: a clock that stepped back would
// otherwise respawn the check on every start. A failure counts as a check for
// this: an offline laptop would otherwise ask GitHub on every start
fn check_due(cache: Option<&UpdateCache>, now: u64, interval: std::time::Duration) -> bool {
    match cache {
        Some(cache) => {
            let last = cache.checked_at.max(cache.failed_at.unwrap_or(0));
            now.saturating_sub(last) >= interval.as_secs()
        }
        None => true,
    }
}

// Newer than the last success, so one good check clears the hint
fn check_failed(cache: &UpdateCache) -> bool {
    cache.failed_at.is_some_and(|at| at > cache.checked_at)
}

/// Whether the last check ended in an error and nothing has succeeded since.
/// A file read, like `available_update`, for the same reason.
pub fn last_check_failed() -> bool {
    read_cache().is_some_and(|cache| check_failed(&cache))
}

/// Starts `update --check` detached when the cache is missing or older than
/// `update_check_interval`. It never waits for it: the answer lands in the
/// cache for the next read.
pub fn spawn_check_if_due() {
    let interval = crate::settings::current().update_check_interval;
    if !check_due(read_cache().as_ref(), now_secs(), interval) {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .args(["update", "--check"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

// Reads the cache only: a CLI command must never wait on the network to print
// its own output
pub fn maybe_print_update_hint() {
    if let Some(notice) = available_update() {
        eprintln!("\n{}", hint_line(env!("CARGO_PKG_VERSION"), &notice));
        return;
    }
    spawn_check_if_due();
}

// The command is the notice's, so a Homebrew install is sent to `brew upgrade`
// here as it is in the TUI header, not to an `update` that only redirects
fn hint_line(current: &str, notice: &UpdateNotice) -> String {
    format!(
        "gtunnel v{} is available (current: v{current}). Run `{}`.",
        notice.latest, notice.command
    )
}

// "<hex>  <filename>", as shasum and sha256sum both write it
pub fn parse_sha256_file(text: &str) -> Option<String> {
    text.split_whitespace().next().map(|s| s.to_string())
}

// Hashes the download in memory: a file written first and hashed afterwards
// can be swapped in between
fn sha256_bytes(bytes: &[u8]) -> Result<String> {
    use std::io::Write;
    for (program, args) in [("shasum", vec!["-a", "256"]), ("sha256sum", vec![])] {
        let spawned = std::process::Command::new(program)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else { continue };
        child
            .stdin
            .take()
            .context("Could not write to the checksum program")?
            .write_all(bytes)?;
        let out = child.wait_with_output()?;
        if out.status.success() {
            return parse_sha256_file(&String::from_utf8_lossy(&out.stdout))
                .context("Could not read the checksum output");
        }
    }
    anyhow::bail!("Neither shasum nor sha256sum is on PATH")
}

// Fails if the path already exists, so a pre-created directory is never used
fn create_private_dir(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("Could not create {}", path.display()))
}

// A private directory under `parent`, cleaned up when the guard drops. The
// name is random because the staged binary is about to replace the running one
fn staging_dir(parent: &Path) -> Result<TempDirGuard> {
    use rand::RngExt;
    for _ in 0..8 {
        let suffix: u64 = rand::rng().random();
        let path = parent.join(format!(".gtunnel-update-{suffix:016x}"));
        if create_private_dir(&path).is_ok() {
            return Ok(TempDirGuard(path));
        }
    }
    anyhow::bail!(
        "Could not create a staging directory in {}",
        parent.display()
    )
}

async fn get(url: &str) -> Result<Vec<u8>> {
    let response = crate::http::client_builder()
        .build()
        .context("Failed to build the HTTP client")?
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
    let slug = platform_slug().context("This platform has no self-update build")?;
    let asset = asset_name(slug, version);
    let base = format!("https://github.com/{REPO}/releases/download/v{version}");

    eprintln!("Downloading {asset}...");
    let bytes = get(&format!("{base}/{asset}")).await?;

    // A self-updater overwrites the binary it is running from, so the download
    // is checked against the published digest before it is trusted
    let published = parse_sha256_file(&String::from_utf8_lossy(
        &get(&format!("{base}/{asset}.sha256")).await?,
    ))
    .context("The release has no usable .sha256 file")?;
    let actual = sha256_bytes(&bytes)?;
    if !actual.eq_ignore_ascii_case(&published) {
        anyhow::bail!("Checksum mismatch for {asset}: expected {published}, got {actual}");
    }

    // Staged beside the executable so the final rename is atomic, and so the
    // directory inherits that location's permissions rather than /tmp's
    let guard = match exe_path.parent() {
        Some(dir) => staging_dir(dir).or_else(|_| staging_dir(&std::env::temp_dir()))?,
        None => staging_dir(&std::env::temp_dir())?,
    };
    let tmp = guard.0.clone();

    let archive = tmp.join(&asset);
    std::fs::write(&archive, &bytes)?;

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

    // The asset names are what a visitor reads on the releases page, so they
    // say the platform rather than the Rust triple. They are also what the
    // updater fetches, so release.yml and this list have to agree exactly
    #[test]
    fn this_platform_has_a_release_asset() {
        let slug = platform_slug().expect("a slug for this host");
        assert!(
            ["macos-arm64", "macos-intel", "linux-amd64", "linux-arm64"].contains(&slug),
            "slug not published by release.yml: {slug}"
        );
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            asset_name(slug, version),
            format!("gtunnel-{slug}-v{version}.tar.gz")
        );
    }

    #[test]
    fn the_slug_names_the_host_in_plain_words() {
        let slug = platform_slug().expect("a slug for this host");
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(slug, "macos-arm64");
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        assert_eq!(slug, "macos-intel");
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(slug, "linux-amd64");
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        assert_eq!(slug, "linux-arm64");
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

    // The staging directory holds a binary that is about to replace the one
    // the user runs. On a shared /tmp a predictable name lets someone else
    // create it first and swap the contents after the digest is checked
    #[test]
    fn the_staging_directory_is_private_and_unguessable() {
        let base = std::env::temp_dir().join(format!("gtunnel-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();

        let first = staging_dir(&base).expect("a staging directory");
        let second = staging_dir(&base).expect("a second staging directory");
        assert_ne!(first.0, second.0, "two runs must not collide");
        assert!(first.0.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first.0).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "mode was {:o}", mode & 0o777);
        }

        // An attacker who guessed the name owns it: creating it again must fail
        assert!(create_private_dir(&first.0).is_err());

        let path = first.0.clone();
        drop(first);
        drop(second);
        assert!(!path.exists(), "the guard must clean up");
        let _ = std::fs::remove_dir_all(&base);
    }

    // The digest is checked before any byte reaches the disk, so there is no
    // window between verifying the archive and reading it back
    #[test]
    fn bytes_are_hashed_without_a_file() {
        assert_eq!(
            sha256_bytes(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_cargo_bin_path_updates_itself_as_a_binary() {
        let exe = PathBuf::from("/Users/you/.cargo/bin/gtunnel");
        let method = detect_install_method_from_path(&exe);
        assert!(matches!(method, InstallMethod::Binary(_)));
        assert!(upgrade_hint(&method).is_none());
    }

    #[test]
    fn a_homebrew_install_is_sent_to_brew_upgrade() {
        let exe = PathBuf::from("/opt/homebrew/Cellar/gtunnel/0.1.0/bin/gtunnel");
        assert!(matches!(
            detect_install_method_from_path(&exe),
            InstallMethod::Homebrew
        ));
        let hint = upgrade_hint(&InstallMethod::Homebrew).expect("homebrew carries a hint");
        assert!(hint.contains("brew upgrade gtunnel"));
    }

    #[test]
    fn a_downloaded_binary_updates_itself_rather_than_printing_a_hint() {
        assert!(upgrade_hint(&InstallMethod::Binary(PathBuf::from(
            "/usr/local/bin/gtunnel"
        )))
        .is_none());
    }

    #[test]
    fn the_checksum_line_is_read_from_the_published_file() {
        let version = env!("CARGO_PKG_VERSION");
        let line = format!("abc123  gtunnel-macos-arm64-v{version}.tar.gz\n");
        assert_eq!(parse_sha256_file(&line), Some("abc123".to_string()));
        assert_eq!(parse_sha256_file("   \n"), None);
    }

    #[test]
    fn a_newer_cached_version_is_a_notice_with_the_install_command() {
        let binary = InstallMethod::Binary(PathBuf::from("/usr/local/bin/gtunnel"));
        assert_eq!(
            notice_for("0.1.0", "9.9.9", &binary),
            Some(UpdateNotice {
                latest: "9.9.9".to_string(),
                command: "gtunnel update"
            })
        );
        assert_eq!(
            notice_for("0.1.0", "9.9.9", &InstallMethod::Homebrew)
                .expect("a newer version is a notice")
                .command,
            "brew upgrade gtunnel"
        );
    }

    #[test]
    fn the_cli_hint_names_the_command_for_the_install_method() {
        let brew = notice_for("0.2.38", "9.9.9", &InstallMethod::Homebrew).expect("newer");
        assert_eq!(
            hint_line("0.2.38", &brew),
            "gtunnel v9.9.9 is available (current: v0.2.38). Run `brew upgrade gtunnel`."
        );
        let binary = notice_for(
            "0.2.38",
            "9.9.9",
            &InstallMethod::Binary(PathBuf::from("/x")),
        )
        .expect("newer");
        assert_eq!(
            hint_line("0.2.38", &binary),
            "gtunnel v9.9.9 is available (current: v0.2.38). Run `gtunnel update`."
        );
    }

    #[test]
    fn update_check_names_the_same_command_as_the_hint() {
        assert_eq!(
            install_command(&InstallMethod::Homebrew),
            "brew upgrade gtunnel"
        );
        assert_eq!(
            install_command(&InstallMethod::Binary(PathBuf::from("/x"))),
            "gtunnel update"
        );
    }

    #[test]
    fn the_current_or_an_older_version_is_no_notice() {
        assert_eq!(notice_for("1.2.3", "1.2.3", &InstallMethod::Homebrew), None);
        assert_eq!(notice_for("1.2.3", "1.2.2", &InstallMethod::Homebrew), None);
    }

    fn cache_at(checked_at: u64, failed_at: Option<u64>) -> UpdateCache {
        UpdateCache {
            latest_version: "9.9.9".to_string(),
            checked_at,
            failed_at,
        }
    }

    #[test]
    fn a_missing_or_stale_cache_is_due_and_a_fresh_one_is_not() {
        let hours = |h: u64| std::time::Duration::from_secs(h * 3600);
        let now = 100 * 3600;
        assert!(check_due(None, now, hours(6)));
        assert!(check_due(
            Some(&cache_at(now - 6 * 3600, None)),
            now,
            hours(6)
        ));
        assert!(!check_due(
            Some(&cache_at(now - 6 * 3600 + 1, None)),
            now,
            hours(6)
        ));
        // The same cache is due under a shorter interval and not under a longer one
        let cache = cache_at(now - 3 * 3600, None);
        assert!(check_due(Some(&cache), now, hours(1)));
        assert!(!check_due(Some(&cache), now, hours(24)));
        // A clock that stepped back must not read as a check due forever
        assert!(!check_due(Some(&cache_at(now + 5, None)), now, hours(6)));
    }

    #[test]
    fn a_recent_failure_postpones_the_next_check_like_a_success() {
        let interval = std::time::Duration::from_secs(6 * 3600);
        let now = 100 * 3600;
        let failed = cache_at(now - 24 * 3600, Some(now - 3600));
        assert!(!check_due(Some(&failed), now, interval));
        let long_ago = cache_at(now - 24 * 3600, Some(now - 7 * 3600));
        assert!(check_due(Some(&long_ago), now, interval));
    }

    #[test]
    fn a_failure_newer_than_the_last_success_is_a_failed_check_and_a_success_clears_it() {
        assert!(check_failed(&cache_at(100, Some(200))));
        assert!(!check_failed(&cache_at(300, Some(200))));
        assert!(!check_failed(&cache_at(100, None)));
    }

    #[test]
    fn a_cache_from_before_failures_were_recorded_still_reads() {
        let old = r#"{"latest_version":"1.2.3","checked_at":1700000000}"#;
        let cache: UpdateCache = serde_json::from_str(old).expect("an old cache parses");
        assert_eq!(cache.latest_version, "1.2.3");
        assert_eq!(cache.failed_at, None);
        assert!(!check_failed(&cache));
    }

    #[test]
    fn a_failure_with_no_cache_yet_is_a_failed_check_and_no_notice() {
        let blank = UpdateCache {
            latest_version: String::new(),
            checked_at: 0,
            failed_at: Some(500),
        };
        assert!(check_failed(&blank));
        let binary = InstallMethod::Binary(PathBuf::from("/usr/local/bin/gtunnel"));
        assert_eq!(notice_for("0.1.0", &blank.latest_version, &binary), None);
    }
}
