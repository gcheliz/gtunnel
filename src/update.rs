use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const REPO: &str = "gcheliz/gtunnel";
// brew installs from this repository's formula, not from the release, so it is
// the source of truth for what a Homebrew user can actually upgrade to
const TAP_REPO: &str = "gcheliz/homebrew-tap";

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
    // What the tap formula offers, when the last Homebrew check could read it.
    // None is "not read": a binary install never asks, a failed read leaves it
    // empty, and a cache from before this field has none
    #[serde(default)]
    tap_version: Option<String>,
}

fn cache_path() -> Option<PathBuf> {
    Some(crate::config::config_dir().ok()?.join("update-check.json"))
}

fn read_cache() -> Option<UpdateCache> {
    serde_json::from_str(&std::fs::read_to_string(cache_path()?).ok()?).ok()
}

fn write_cache(cache: &UpdateCache) -> Result<()> {
    let path = cache_path().context("Could not determine the cache path")?;
    // Never creates the directory: a check that outlives `reset` writes after
    // the config is gone, and a cache is not worth bringing it back for
    crate::fsutil::write_atomic(&path, &serde_json::to_string(cache)?)?;
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
            tap_version: None,
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

// The formula carries no `version` line: brew reads it from the url, so this
// does too. All four urls are rewritten in one commit, so the first is enough
fn parse_formula_version(formula: &str) -> Option<String> {
    let rest = formula.split("/releases/download/v").nth(1)?;
    let version: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let valid = version.split('.').count() == 3
        && version
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    valid.then_some(version)
}

async fn fetch_tap_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{TAP_REPO}/contents/Formula/gtunnel.rb");
    let response = crate::http::client_builder()
        .build()
        .context("Failed to build the HTTP client")?
        .get(&url)
        .header("User-Agent", user_agent())
        .header("Accept", "application/vnd.github.raw+json")
        .send()
        .await
        .context("Failed to reach the GitHub API")?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if status != 200 {
        anyhow::bail!("The GitHub API answered {status} for the tap formula");
    }
    parse_formula_version(&body).context("The tap formula names no release version")
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

// Says what a Homebrew user can do now, which is not always what the release
// says: until the tap job writes the formula, brew answers "already up to date"
#[allow(clippy::print_stderr)]
fn report_homebrew(current: &str, release: &str, tap: Option<&str>, check_only: bool) {
    match notice_for(current, release, tap, &InstallMethod::Homebrew) {
        Some(notice) if check_only => {
            eprintln!("Update available: v{current} -> v{}", notice.latest);
            eprintln!("\nRun `{}` to install.", notice.command);
        }
        Some(notice) => {
            eprintln!("Update available: v{current} -> v{}", notice.latest);
            eprintln!(
                "\ngtunnel was installed with Homebrew. Run:\n  {}",
                notice.command
            );
        }
        None => match tap {
            Some(tap) if is_newer(current, release) => eprintln!(
                "gtunnel v{release} is released, but the Homebrew tap still offers v{tap}. \
                 brew will offer it once the tap catches up."
            ),
            _ => eprintln!("gtunnel v{current} is already the latest version."),
        },
    }
}

fn detect_install_method() -> Result<InstallMethod> {
    Ok(detect_install_method_from_path(&exe_path()?))
}

// Homebrew is decided by where the file really lives, not by the name it was
// started under: on an Intel Mac brew's entry point is a symlink at
// /usr/local/bin/gtunnel into /usr/local/Cellar, so the unresolved path says
// nothing. The Binary variant carries the canonical file for the same reason:
// the swap and the `.old` backup must land on it, or an update replaces the
// symlink and the next `brew link` fails with "already exists"
fn detect_install_method_from_path(exe: &Path) -> InstallMethod {
    let real = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    if real.to_string_lossy().contains("/Cellar/gtunnel/") {
        InstallMethod::Homebrew
    } else {
        InstallMethod::Binary(real)
    }
}

/// The path a service, a watchdog or a spawned child should run this binary by.
///
/// `current_exe()` is not one answer: macOS returns the path the process was
/// started by, and Linux returns `/proc/self/exe`, which is resolved and gains
/// a ` (deleted)` suffix once an update has replaced the file. Written into a
/// unit as it stands, that is a program that cannot start. So the marker is
/// stripped, and a stable path that reaches the same file wins over a versioned
/// Cellar path that `brew upgrade` is about to delete.
pub fn exe_path() -> Result<PathBuf> {
    let exe = absolute_exe(std::env::current_exe().context("Cannot find the gtunnel binary")?)?;
    Ok(stable_path(&exe, &stable_candidates()))
}

// A relative path would resolve against whatever directory the unit or the
// child starts in, so it is refused here rather than baked into a plist
fn absolute_exe(raw: PathBuf) -> Result<PathBuf> {
    let exe = strip_deleted(&raw);
    if !exe.is_absolute() {
        anyhow::bail!("{} is not an absolute path to gtunnel", exe.display());
    }
    Ok(exe)
}

fn strip_deleted(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_suffix(" (deleted)") {
        Some(stripped) => PathBuf::from(stripped),
        None => path.to_path_buf(),
    }
}

// Most stable first: `opt/gtunnel` survives every upgrade, `bin` is a link
// brew rewrites, a PATH entry is whatever the shell found
fn stable_candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(prefix) = std::env::var_os("HOMEBREW_PREFIX") {
        found.push(PathBuf::from(prefix).join("opt/gtunnel/bin/gtunnel"));
    }
    for prefix in ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"] {
        found.push(PathBuf::from(prefix).join("opt/gtunnel/bin/gtunnel"));
    }
    for prefix in ["/opt/homebrew", "/usr/local"] {
        found.push(PathBuf::from(prefix).join("bin/gtunnel"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        found.extend(std::env::split_paths(&path).map(|dir| dir.join("gtunnel")));
    }
    found.retain(|p| p.is_absolute());
    found
}

// Only a candidate that is the same file qualifies: another gtunnel earlier on
// PATH must not be adopted just because it is there
fn stable_path(exe: &Path, candidates: &[PathBuf]) -> PathBuf {
    let Ok(real) = std::fs::canonicalize(exe) else {
        return exe.to_path_buf();
    };
    candidates
        .iter()
        .find(|c| std::fs::canonicalize(c).is_ok_and(|r| r == real))
        .cloned()
        .unwrap_or_else(|| exe.to_path_buf())
}

#[allow(clippy::print_stdout, clippy::print_stderr)]
pub async fn cmd_update(check_only: bool, rollback: bool) -> Result<()> {
    if rollback {
        // No network: the way back must work when the new version is why
        // nothing else does
        return cmd_rollback();
    }
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("Checking for updates...");
    let latest = match fetch_latest_version().await {
        Ok(latest) => latest,
        Err(e) => {
            record_failure(now_secs());
            return Err(e);
        }
    };

    let homebrew = matches!(detect_install_method(), Ok(InstallMethod::Homebrew));
    let tap_version = if homebrew {
        fetch_tap_version().await.ok()
    } else {
        None
    };

    let _ = write_cache(&UpdateCache {
        latest_version: latest.clone(),
        checked_at: now_secs(),
        failed_at: None,
        tap_version: tap_version.clone(),
    });

    if homebrew {
        report_homebrew(current, &latest, tap_version.as_deref(), check_only);
        return Ok(());
    }

    if !is_newer(current, &latest) {
        eprintln!("gtunnel v{current} is already the latest version.");
        return Ok(());
    }
    eprintln!("Update available: v{current} -> v{latest}");
    let InstallMethod::Binary(exe) = detect_install_method()? else {
        unreachable!("Homebrew returned above")
    };
    if check_only {
        eprintln!("\nRun `gtunnel update` to install.");
        return Ok(());
    }
    perform_update(&exe, &latest).await
}

/// What the TUI and the CLI both say when a newer release is cached. One type
/// so the two surfaces cannot name different commands for the same install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    pub latest: String,
    pub command: &'static str,
}

// Pure so the install method and the versions are arguments, not the test
// runner's own path and Cargo.toml. A Homebrew install is judged by the tap,
// because brew cannot upgrade to a release the formula does not name yet. With
// no tap reading the release is the best guess, and `brew update` first is the
// command that works whether or not the tap moved
fn notice_for(
    current: &str,
    release: &str,
    tap: Option<&str>,
    method: &InstallMethod,
) -> Option<UpdateNotice> {
    let (latest, command) = match (method, tap) {
        (InstallMethod::Homebrew, Some(tap)) => (tap, install_command(method)),
        (InstallMethod::Homebrew, None) => (release, BREW_REFRESH_COMMAND),
        (InstallMethod::Binary(_), _) => (release, install_command(method)),
    };
    if !is_newer(current, latest) {
        return None;
    }
    Some(UpdateNotice {
        latest: latest.trim_start_matches('v').to_string(),
        command,
    })
}

const BREW_REFRESH_COMMAND: &str = "brew update && brew upgrade gtunnel";

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
        cache.tap_version.as_deref(),
        &detect_install_method().ok()?,
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
        let child = std::process::Command::new(exe)
            .args(["update", "--check"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        // The TUI outlives the check, so an unreaped child is a zombie for the
        // whole session; the thread only waits, it never reads the result
        if let Ok(mut child) = child {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

// Reads the cache only: a CLI command must never wait on the network to print
// its own output
#[allow(clippy::print_stdout, clippy::print_stderr)]
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

// Hashed in process and in memory: a file written first and hashed afterwards
// can be swapped in between, and a `shasum` found on PATH is one more program
// the digest of a self-replacing binary would depend on
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

// Far above a real release (about 10 MB) and far below what could exhaust
// memory: the body is held whole so it can be hashed before a byte is written
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;

fn check_declared_len(declared: Option<u64>, limit: u64) -> Result<()> {
    match declared {
        Some(len) if len > limit => {
            anyhow::bail!("The download is {len} bytes, over the {limit} byte limit")
        }
        _ => Ok(()),
    }
}

// The declared length can be absent or false, so the bytes are counted as they
// arrive and the chunk that crosses the limit is never kept
fn append_capped(buf: &mut Vec<u8>, chunk: &[u8], limit: u64) -> Result<()> {
    if buf.len() as u64 + chunk.len() as u64 > limit {
        anyhow::bail!("The download is over the {limit} byte limit");
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

async fn get(url: &str) -> Result<Vec<u8>> {
    let mut response = crate::http::client_builder()
        .build()
        .context("Failed to build the HTTP client")?
        .get(url)
        .header("User-Agent", user_agent())
        .send()
        .await
        .with_context(|| format!("Failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("Failed to download {url}"))?;
    check_declared_len(response.content_length(), MAX_DOWNLOAD)
        .with_context(|| format!("Refused {url}"))?;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("Failed to download {url}"))?
    {
        append_capped(&mut body, &chunk, MAX_DOWNLOAD).with_context(|| format!("Refused {url}"))?;
    }
    Ok(body)
}

#[allow(clippy::print_stdout, clippy::print_stderr)]
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
    let actual = sha256_hex(&bytes);
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
    install_staged(&new_bin, exe_path, version, SMOKE_TIMEOUT)?;
    eprintln!("Updated gtunnel to v{version}");
    eprintln!(
        "The previous version is kept as {}; `gtunnel update --rollback` restores it.",
        backup_path(exe_path).display()
    );
    Ok(())
}

// Beside the installed binary, so the restore is a rename on one volume
fn backup_path(exe_path: &Path) -> PathBuf {
    let mut name = exe_path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe_path.with_file_name(name)
}

const SMOKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

// A checksum proves the file is the one published, not that it runs here: a
// wrong architecture or a missing library passes it and then takes down every
// tunnel at the next restart. Running it is the only proof, and the answer has
// to name the version asked for, so a stale asset cannot pass as the new one
fn smoke_test(bin: &Path, version: &str, timeout: std::time::Duration) -> Result<()> {
    use std::io::Read;
    let mut child = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("The downloaded binary could not be started")?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "The downloaded binary did not answer --version within {}s",
                timeout.as_secs_f32()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    if !status.success() {
        anyhow::bail!("The downloaded binary failed `--version` ({status})");
    }
    if !out.contains(version) {
        anyhow::bail!(
            "The downloaded binary reports `{}` for --version, expected v{version}",
            out.trim()
        );
    }
    Ok(())
}

// Smoke test, then keep the old binary, then swap. A refusal at any step
// leaves the installed binary exactly as it was
fn install_staged(
    new_bin: &Path,
    exe_path: &Path,
    version: &str,
    timeout: std::time::Duration,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new_bin, std::fs::Permissions::from_mode(0o755))?;
    }
    smoke_test(new_bin, version, timeout)?;
    keep_previous(exe_path)?;
    replace_binary(new_bin, exe_path)
}

// A copy renamed into place, not a rename of the installed file: the running
// binary stays where it is until the new one replaces it, so a failure here
// never leaves the install path empty
fn keep_previous(exe_path: &Path) -> Result<()> {
    let backup = backup_path(exe_path);
    let staged = exe_path.with_file_name(format!(".gtunnel-update-{}.old", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    let kept = std::fs::copy(exe_path, &staged).and_then(|_| std::fs::rename(&staged, &backup));
    if let Err(e) = kept {
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("Could not keep {}", backup.display()));
    }
    Ok(())
}

// Renames the backup over the installed binary, so it is spent: a second
// rollback is refused rather than swapping the broken version back in
fn rollback_binary(exe_path: &Path) -> Result<()> {
    let backup = backup_path(exe_path);
    if !backup.exists() {
        anyhow::bail!(
            "There is no previous version to restore: {} does not exist",
            backup.display()
        );
    }
    std::fs::rename(&backup, exe_path)
        .with_context(|| format!("Failed to restore {}", backup.display()))
}

#[allow(clippy::print_stderr)]
fn cmd_rollback() -> Result<()> {
    match detect_install_method()? {
        InstallMethod::Homebrew => {
            anyhow::bail!("gtunnel was installed with Homebrew, which keeps no previous binary here. Run:\n  brew upgrade gtunnel")
        }
        InstallMethod::Binary(exe) => {
            rollback_binary(&exe)?;
            eprintln!("Restored the previous version at {}", exe.display());
            Ok(())
        }
    }
}

fn replace_binary(new_bin: &Path, exe_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new_bin, std::fs::Permissions::from_mode(0o755))?;
        if std::fs::rename(new_bin, exe_path).is_ok() {
            return Ok(());
        }
        copy_beside_then_swap(new_bin, exe_path)
    }
    #[cfg(not(unix))]
    {
        let _ = (new_bin, exe_path);
        anyhow::bail!("Self-update works on macOS and Linux only")
    }
}

// The download sits on another volume. Copying onto exe_path would truncate
// the inode the kernel has mapped and signed, and a rewritten signed binary
// is killed at its next launch on macOS. Stage the copy beside the target,
// same volume, and swap the directory entry instead
#[cfg(unix)]
fn copy_beside_then_swap(new_bin: &Path, exe_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = exe_path
        .parent()
        .context("The executable has no directory")?;
    let staged = dir.join(format!(".gtunnel-update-{}.new", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    let swapped = std::fs::copy(new_bin, &staged)
        .and_then(|_| std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)))
        .and_then(|_| std::fs::rename(&staged, exe_path));
    if let Err(e) = swapped {
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("Failed to replace {}", exe_path.display()));
    }
    Ok(())
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

    #[cfg(unix)]
    #[test]
    fn replacing_across_volumes_gives_the_target_a_new_inode_and_never_writes_the_old_one() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("gtunnel");
        let new_bin = dir.path().join("download");
        std::fs::write(&exe, "old binary").unwrap();
        std::fs::write(&new_bin, "new binary").unwrap();
        // A second link keeps the old inode reachable after the swap
        let old_link = dir.path().join("old-link");
        std::fs::hard_link(&exe, &old_link).unwrap();
        let old_inode = std::fs::metadata(&exe).unwrap().ino();

        copy_beside_then_swap(&new_bin, &exe).unwrap();

        let meta = std::fs::metadata(&exe).unwrap();
        assert_ne!(meta.ino(), old_inode);
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new binary");
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_to_string(&old_link).unwrap(), "old binary");
        let names = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(names, 3, "a staged copy was left behind");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_swap_returns_the_error_and_leaves_the_target_alone() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("gtunnel");
        std::fs::write(&exe, "old binary").unwrap();
        let missing = dir.path().join("no-such-download");
        assert!(copy_beside_then_swap(&missing, &exe).is_err());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old binary");
    }

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
    fn the_empty_input_and_a_multi_block_input_hash_to_their_known_digests() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn bytes_are_hashed_without_a_file() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // `reset` deletes the directory and the hint after it starts a check that
    // writes the cache; creating the parent brought the directory back
    #[test]
    fn a_cache_write_does_not_recreate_a_deleted_config_directory() {
        let temp = crate::config::testing::TempConfigDir::new();
        std::fs::remove_dir_all(temp.path()).unwrap();
        let _ = write_cache(&UpdateCache {
            latest_version: "9.9.9".to_string(),
            checked_at: 1,
            failed_at: None,
            tap_version: None,
        });
        assert!(
            !temp.path().exists(),
            "the cache write recreated the directory"
        );
    }

    #[test]
    fn a_cargo_bin_path_updates_itself_as_a_binary() {
        let exe = PathBuf::from("/Users/you/.cargo/bin/gtunnel");
        let method = detect_install_method_from_path(&exe);
        assert!(matches!(method, InstallMethod::Binary(_)));
    }

    #[test]
    fn a_homebrew_install_is_sent_to_brew_upgrade() {
        let exe = PathBuf::from("/opt/homebrew/Cellar/gtunnel/0.1.0/bin/gtunnel");
        assert!(matches!(
            detect_install_method_from_path(&exe),
            InstallMethod::Homebrew
        ));
        assert_eq!(
            install_command(&InstallMethod::Homebrew),
            "brew upgrade gtunnel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_intel_homebrew_symlink_into_the_cellar_is_detected_as_homebrew() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("Cellar/gtunnel/0.2.65/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("gtunnel"), "binary").unwrap();
        let link_dir = dir.path().join("bin");
        std::fs::create_dir_all(&link_dir).unwrap();
        let link = link_dir.join("gtunnel");
        std::os::unix::fs::symlink(bin.join("gtunnel"), &link).unwrap();

        assert!(matches!(
            detect_install_method_from_path(&link),
            InstallMethod::Homebrew
        ));
    }

    #[test]
    fn a_trailing_deleted_marker_is_stripped_from_the_path() {
        assert_eq!(
            strip_deleted(Path::new("/usr/local/bin/gtunnel (deleted)")),
            PathBuf::from("/usr/local/bin/gtunnel")
        );
        assert_eq!(
            strip_deleted(Path::new("/usr/local/bin/gtunnel")),
            PathBuf::from("/usr/local/bin/gtunnel")
        );
    }

    #[test]
    fn a_relative_path_is_an_error_not_a_definition() {
        let err = absolute_exe(PathBuf::from("target/debug/gtunnel")).expect_err("relative");
        assert!(
            format!("{err:#}").contains("not an absolute path"),
            "{err:#}"
        );
        let stripped = absolute_exe(PathBuf::from("/usr/bin/gtunnel (deleted)")).unwrap();
        assert_eq!(stripped, PathBuf::from("/usr/bin/gtunnel"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_install_is_updated_through_its_target_and_the_symlink_survives() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        let link_dir = dir.path().join("bin");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::create_dir_all(&link_dir).unwrap();
        let real = stub(&real_dir, "gtunnel", "echo gtunnel 1.0.0");
        let link = link_dir.join("gtunnel");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let new_bin = stub(dir.path(), "staged", "echo gtunnel 2.0.0");

        let InstallMethod::Binary(target) = detect_install_method_from_path(&link) else {
            panic!("a user symlink is not a package manager's");
        };
        install_staged(&new_bin, &target, "2.0.0", SMOKE).unwrap();

        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "the symlink was replaced");
        assert_eq!(std::fs::read_link(&link).unwrap(), real);
        assert!(std::fs::read_to_string(&real).unwrap().contains("2.0.0"));
        assert_eq!(
            backup_path(&target),
            std::fs::canonicalize(&real_dir)
                .unwrap()
                .join("gtunnel.old"),
            "the backup sits beside the file, not beside the symlink"
        );
        assert!(!link_dir.join("gtunnel.old").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_linuxbrew_cellar_path_is_rewritten_to_the_opt_path_that_reaches_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join("Cellar/gtunnel/0.2.65");
        std::fs::create_dir_all(version.join("bin")).unwrap();
        std::fs::write(version.join("bin/gtunnel"), "binary").unwrap();
        let opt = dir.path().join("opt");
        std::fs::create_dir_all(&opt).unwrap();
        std::os::unix::fs::symlink(&version, opt.join("gtunnel")).unwrap();
        let stable = opt.join("gtunnel/bin/gtunnel");
        let versioned = version.join("bin/gtunnel");
        let other = dir.path().join("elsewhere/gtunnel");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "different").unwrap();

        let chosen = stable_path(&versioned, &[other.clone(), stable.clone()]);
        assert_eq!(chosen, stable);

        // A candidate that is a different file is never preferred
        assert_eq!(stable_path(&versioned, &[other]), versioned);
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
            notice_for("0.1.0", "9.9.9", None, &binary),
            Some(UpdateNotice {
                latest: "9.9.9".to_string(),
                command: "gtunnel update"
            })
        );
        assert_eq!(
            notice_for("0.1.0", "9.9.9", Some("9.9.9"), &InstallMethod::Homebrew)
                .expect("a newer version is a notice")
                .command,
            "brew upgrade gtunnel"
        );
    }

    #[test]
    fn the_cli_hint_names_the_command_for_the_install_method() {
        let brew =
            notice_for("0.2.38", "9.9.9", Some("9.9.9"), &InstallMethod::Homebrew).expect("newer");
        assert_eq!(
            hint_line("0.2.38", &brew),
            "gtunnel v9.9.9 is available (current: v0.2.38). Run `brew upgrade gtunnel`."
        );
        let binary = notice_for(
            "0.2.38",
            "9.9.9",
            None,
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
        assert_eq!(
            notice_for("1.2.3", "1.2.3", Some("1.2.3"), &InstallMethod::Homebrew),
            None
        );
        assert_eq!(
            notice_for("1.2.3", "1.2.2", Some("1.2.2"), &InstallMethod::Homebrew),
            None
        );
    }

    fn cache_at(checked_at: u64, failed_at: Option<u64>) -> UpdateCache {
        UpdateCache {
            latest_version: "9.9.9".to_string(),
            checked_at,
            failed_at,
            tap_version: None,
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
            tap_version: None,
        };
        assert!(check_failed(&blank));
        let binary = InstallMethod::Binary(PathBuf::from("/usr/local/bin/gtunnel"));
        assert_eq!(
            notice_for("0.1.0", &blank.latest_version, None, &binary),
            None
        );
    }

    // Copied from the template `bump-tap-formula.py` rewrites: no `version` line,
    // so the version is the one in the url
    const FORMULA: &str = r#"class Gtunnel < Formula
  desc "TUI-first CLI for GCP IAP tunnels and public URLs via a GCP relay"
  homepage "https://github.com/gcheliz/gtunnel"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/gcheliz/gtunnel/releases/download/v0.3.12/gtunnel-macos-arm64-v0.3.12.tar.gz"
      sha256 "b5714c5c9b89d88a5db88b3065f4cbc48205419dc23a432cedb8ff98982d7775"
    end
  end
end
"#;

    #[test]
    fn the_tap_version_is_read_from_the_formula_url() {
        assert_eq!(parse_formula_version(FORMULA).as_deref(), Some("0.3.12"));
        assert_eq!(
            parse_formula_version("class Gtunnel < Formula\nend\n"),
            None
        );
        assert_eq!(
            parse_formula_version(r#"url "https://x/releases/download/vnext/a.tar.gz""#),
            None
        );
    }

    #[test]
    fn a_homebrew_install_waits_for_the_tap() {
        let binary = InstallMethod::Binary(PathBuf::from("/usr/local/bin/gtunnel"));
        // Released as 0.3.13, the formula still says 0.3.12
        assert_eq!(
            notice_for("0.3.12", "0.3.13", Some("0.3.12"), &InstallMethod::Homebrew),
            None
        );
        let notice = notice_for("0.3.12", "0.3.13", Some("0.3.12"), &binary).expect("a release");
        assert_eq!(notice.latest, "0.3.13");
        assert_eq!(notice.command, "gtunnel update");
        // Once the tap has it, brew is told
        let brew = notice_for("0.3.12", "0.3.13", Some("0.3.13"), &InstallMethod::Homebrew)
            .expect("the tap caught up");
        assert_eq!(brew.command, "brew upgrade gtunnel");
    }

    #[test]
    fn an_unreadable_tap_falls_back_to_refreshing_brew() {
        let notice = notice_for("0.3.12", "0.3.13", None, &InstallMethod::Homebrew)
            .expect("the release is the best guess");
        assert_eq!(notice.latest, "0.3.13");
        assert_eq!(notice.command, "brew update && brew upgrade gtunnel");
        assert_eq!(
            notice_for("0.3.13", "0.3.13", None, &InstallMethod::Homebrew),
            None
        );
    }

    #[test]
    fn a_cache_from_before_the_tap_was_read_has_no_tap_version() {
        let old = r#"{"latest_version":"1.2.3","checked_at":1700000000}"#;
        let cache: UpdateCache = serde_json::from_str(old).expect("an old cache parses");
        assert_eq!(cache.tap_version, None);
    }

    // A stub that answers `--version` the way the real binary does, or fails
    #[cfg(unix)]
    fn stub(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const SMOKE: std::time::Duration = std::time::Duration::from_secs(10);

    #[cfg(unix)]
    #[test]
    fn a_staged_binary_that_fails_its_version_check_is_not_swapped_in() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "gtunnel", "echo gtunnel 1.0.0");
        let new_bin = stub(dir.path(), "staged", "exit 3");
        let before = std::fs::read(&exe).unwrap();

        let err = install_staged(&new_bin, &exe, "2.0.0", SMOKE).expect_err("a broken binary");

        assert!(format!("{err:#}").contains("--version"), "{err:#}");
        assert_eq!(std::fs::read(&exe).unwrap(), before);
        assert!(
            !backup_path(&exe).exists(),
            "no backup for a swap that never happened"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_staged_binary_that_reports_another_version_is_not_swapped_in() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "gtunnel", "echo gtunnel 1.0.0");
        let new_bin = stub(dir.path(), "staged", "echo gtunnel 1.5.0");
        assert!(install_staged(&new_bin, &exe, "2.0.0", SMOKE).is_err());
        assert!(std::fs::read_to_string(&exe).unwrap().contains("1.0.0"));
    }

    #[cfg(unix)]
    #[test]
    fn a_staged_binary_that_hangs_on_its_version_check_is_killed_and_refused() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "gtunnel", "echo gtunnel 1.0.0");
        let new_bin = stub(dir.path(), "staged", "exec sleep 30");
        let started = std::time::Instant::now();
        let err = install_staged(
            &new_bin,
            &exe,
            "2.0.0",
            std::time::Duration::from_millis(300),
        )
        .expect_err("a hung binary");
        assert!(format!("{err:#}").contains("did not answer"), "{err:#}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(std::fs::read_to_string(&exe).unwrap().contains("1.0.0"));
    }

    #[cfg(unix)]
    #[test]
    fn a_successful_update_leaves_the_previous_binary_as_gtunnel_old() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "gtunnel", "echo gtunnel 1.0.0");
        let old_contents = std::fs::read(&exe).unwrap();
        let new_bin = stub(dir.path(), "staged", "echo gtunnel 2.0.0");

        install_staged(&new_bin, &exe, "2.0.0", SMOKE).unwrap();

        assert_eq!(backup_path(&exe), dir.path().join("gtunnel.old"));
        assert_eq!(std::fs::read(backup_path(&exe)).unwrap(), old_contents);
        assert!(std::fs::read_to_string(&exe).unwrap().contains("2.0.0"));
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 2, "a staged copy was left behind: {names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn rollback_restores_the_backup_and_is_refused_when_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let exe = stub(dir.path(), "gtunnel", "echo gtunnel 2.0.0");

        let err = rollback_binary(&exe).expect_err("no backup yet");
        assert!(
            format!("{err:#}").contains("no previous version"),
            "{err:#}"
        );
        assert!(std::fs::read_to_string(&exe).unwrap().contains("2.0.0"));

        stub(dir.path(), "gtunnel.old", "echo gtunnel 1.0.0");
        rollback_binary(&exe).unwrap();
        assert!(std::fs::read_to_string(&exe).unwrap().contains("1.0.0"));
        assert!(!backup_path(&exe).exists());
        assert!(rollback_binary(&exe).is_err(), "the backup is spent");
    }

    #[test]
    fn a_download_over_the_limit_is_refused_before_a_byte_is_kept() {
        assert!(check_declared_len(Some(MAX_DOWNLOAD), MAX_DOWNLOAD).is_ok());
        assert!(check_declared_len(Some(MAX_DOWNLOAD + 1), MAX_DOWNLOAD).is_err());
        assert!(check_declared_len(None, MAX_DOWNLOAD).is_ok());

        // A server that sends no length is caught as the chunks arrive
        let mut buf = Vec::new();
        assert!(append_capped(&mut buf, &[0; 6], 10).is_ok());
        assert!(append_capped(&mut buf, &[0; 4], 10).is_ok());
        assert!(append_capped(&mut buf, &[0; 1], 10).is_err());
        assert_eq!(buf.len(), 10);
        assert_eq!(MAX_DOWNLOAD, 64 * 1024 * 1024);
    }
}
