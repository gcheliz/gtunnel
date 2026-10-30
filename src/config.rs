use crate::error::GtunnelError;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::loglevel::LogLevel;
use crate::settings;

// A Cloud DNS managed zone registered with the relay, and the domain it serves
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainConfig {
    pub domain: String,
    pub managed_zone: String,
}

// The shared Expose relay for an account, written by `gtunnel relay up`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExposeConfig {
    pub relay_instance: String,
    pub relay_zone: String,
    pub network: String,
    pub default_domain: String,
    #[serde(default)]
    pub domains: Vec<DomainConfig>,
}

impl ExposeConfig {
    // Look up a domain by name, or the default when none is given
    pub fn domain(&self, name: Option<&str>) -> Result<&DomainConfig> {
        let wanted = name.unwrap_or(&self.default_domain);
        self.domains
            .iter()
            .find(|d| d.domain == wanted)
            .ok_or_else(|| {
                anyhow!("Domain '{wanted}' is not registered. Run `gtunnel domains add`")
            })
    }
}

// One gtunnel account: a gcloud configuration plus the project it targets
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub gcloud_configuration: String,
    pub project: String,
    pub default_zone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impersonate_service_account: Option<String>,
    // A service-account key or a workload identity federation file, passed to
    // gcloud per call. gtunnel only holds the path: it never copies, stores or
    // logs the secret, so a key stays where its owner put it; `doctor` reads
    // it only to check its type and mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose: Option<ExposeConfig>,
}

impl Account {
    /// The path to hand to gcloud, or the usage error for a relative one. Every
    /// code path that builds a gcloud argv for this account goes through here.
    pub fn credential_file_for_use(&self) -> Result<Option<&std::path::Path>> {
        let Some(file) = self.credential_file.as_deref() else {
            return Ok(None);
        };
        validate_credential_file(file).with_context(|| format!("In account '{}'", self.name))?;
        Ok(Some(file))
    }
}

/// The version a file written today carries. 2 is the first: it marks the
/// release that switched notifications off by default.
pub const CONFIG_VERSION: u32 = 2;

fn unmarked_config_version() -> u32 {
    0
}

/// What loading did to an older file, kept for the command that is allowed to
/// say so. `load_config` is called from the watchdog and the MCP server as
/// well, so it cannot print; it records, and the dispatcher decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    None,
    Migrated,
    /// Read as migrated in memory, but the file could not be rewritten.
    NotSaved,
}

static MIGRATION: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The migration this process performed, once: a second call reports `None`.
pub fn take_migration() -> MigrationOutcome {
    match MIGRATION.swap(0, std::sync::atomic::Ordering::SeqCst) {
        1 => MigrationOutcome::Migrated,
        2 => MigrationOutcome::NotSaved,
        _ => MigrationOutcome::None,
    }
}

/// Whether to tell the person. `unattended` is the watchdog, the supervisor
/// and the MCP server, whose stderr nobody reads or whose protocol must stay
/// clean; a stderr that is not a terminal is a log or a pipe.
pub fn migration_note_wanted(
    outcome: MigrationOutcome,
    unattended: bool,
    stderr_is_terminal: bool,
) -> bool {
    outcome != MigrationOutcome::None && !unattended && stderr_is_terminal
}

pub const MIGRATION_NOTE: &str =
    "notifications are now off by default; turn them on with N in the TUI or enabled = true in config.toml";

pub const MIGRATION_NOT_SAVED: &str = "could not rewrite config.toml to switch notifications off; \
     they are off for this run, and the next one will try again";

// Every config written before the marker says `enabled = true` because `init`
// wrote it, so the file cannot tell a default from a choice; the marker is
// what makes the flip happen once.
fn migrate_to_current(config: &mut Config) -> bool {
    if config.config_version >= CONFIG_VERSION {
        return false;
    }
    config.notifications.enabled = false;
    config.config_version = CONFIG_VERSION;
    true
}

// Never blocks and never fails the load. Without the lock it skips the write:
// the holder is another writer whose own load migrates, or this very process
// inside `locked_config_update`, which saves the migrated value itself. The
// file is re-read under the lock so a writer that finished first is not
// overwritten with this process's older copy.
fn save_migration(config: &Config) -> bool {
    let Ok(dir) = config_dir() else { return false };
    let Ok(_lock) =
        crate::fsutil::lock_exclusive(&dir.join("config.lock"), std::time::Duration::ZERO)
    else {
        return false;
    };
    let current = std::fs::read_to_string(dir.join("config.toml")).ok();
    let still_old = current
        .and_then(|t| toml::from_str::<Config>(&t).ok())
        .is_some_and(|c| c.config_version < CONFIG_VERSION);
    still_old && save_config(config).is_ok()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    // Marks a file written by a gtunnel that knows notifications are off
    // until chosen. Absent reads as 0, so an older file is told apart from a
    // new one; the struct default is the current version, so `init` writes it.
    // First, because a scalar sits above the tables and a reader looks here
    // before anything else.
    #[serde(default = "unmarked_config_version")]
    pub config_version: u32,
    #[serde(default)]
    pub selected_account: String,
    // How much every tunnel and every gcloud call says. Global rather than
    // per account, because it answers "how much do I want to see right now",
    // which is a property of the moment and not of a tunnel. It sits above
    // `accounts` because toml writes a value before a table.
    #[serde(default)]
    pub log_level: LogLevel,
    // The six values below are kept raw (a duration string, a flag, a command
    // line) and checked by `settings::resolve`, for the reason `RestartConfig`
    // is: the reader of a bad one needs the key's name, not a line number.
    // Like `[restart]` they are always written, so the file shows what there
    // is to tune. Scalars come before the tables, as above.
    #[serde(default = "settings::default_start_deadline")]
    pub start_deadline: String,
    #[serde(default = "settings::default_discovery")]
    pub discovery: bool,
    #[serde(default = "settings::default_service_call_timeout")]
    pub service_call_timeout: String,
    #[serde(default = "settings::default_list_probe_timeout")]
    pub list_probe_timeout: String,
    #[serde(default = "settings::default_update_check_interval")]
    pub update_check_interval: String,
    // Empty is "no editor chosen": TOML has no null, and an absent key would
    // hide the setting from the one place a person looks for it
    #[serde(default)]
    pub editor: String,
    // Always written, like `restart`, so `enabled` sits next to the four edge
    // switches it overrules
    #[serde(default)]
    pub notifications: NotifyConfig,
    // Always written, never skipped when it equals the defaults: it is the
    // part of the file a person opens to tune, and a missing key is
    // invisible. An older file without it still loads, with these values.
    #[serde(default)]
    pub restart: RestartConfig,
    // Always written too, for the same reason: the three watchdog knobs are
    // the file's second-most tuned part and an absent key hides them
    #[serde(default)]
    pub watch: WatchConfig,
    // Written for the same reason: the log's size and rate limits are a
    // decision, and an absent key hides it
    #[serde(default)]
    pub logs: LogsConfig,
    // Written for the same reason: the API timeouts and retry counts are what
    // a slow network or a busy account tunes, and an absent key hides them
    #[serde(default)]
    pub gcp: GcpConfig,
    #[serde(default)]
    pub accounts: Vec<Account>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            selected_account: String::new(),
            log_level: LogLevel::default(),
            start_deadline: settings::default_start_deadline(),
            discovery: settings::default_discovery(),
            service_call_timeout: settings::default_service_call_timeout(),
            list_probe_timeout: settings::default_list_probe_timeout(),
            update_check_interval: settings::default_update_check_interval(),
            editor: String::new(),
            notifications: NotifyConfig::default(),
            restart: RestartConfig::default(),
            watch: WatchConfig::default(),
            logs: LogsConfig::default(),
            gcp: GcpConfig::default(),
            accounts: Vec::new(),
        }
    }
}

/// The watchdog's three knobs as written in `config.toml`, raw for the reason
/// `RestartConfig` is: a bad one is named by key, not by a line number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchConfig {
    pub probe_timeout: String,
    pub degraded_after: i64,
    pub interval: String,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            probe_timeout: settings::default_watch_probe_timeout(),
            degraded_after: settings::default_watch_degraded_after(),
            interval: settings::default_watch_interval(),
        }
    }
}

/// The log's limits as written in `config.toml`, raw like `WatchConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogsConfig {
    pub max_size: String,
    pub keep_size: String,
    pub lines_per_minute: i64,
    pub auth_exit_window: String,
}

impl Default for LogsConfig {
    fn default() -> Self {
        Self {
            max_size: settings::default_logs_max_size(),
            keep_size: settings::default_logs_keep_size(),
            lines_per_minute: settings::default_logs_lines_per_minute(),
            auth_exit_window: settings::default_logs_auth_exit_window(),
        }
    }
}

/// The GCP client's timings and retry counts as written in `config.toml`, raw
/// like `LogsConfig`. `lock_deadline` and `fingerprint_attempts` sit here
/// because the plan files them under `[gcp]`: one is the wait for the file
/// lock every gtunnel writer takes, the other the retries of a fingerprinted
/// update, and neither has a section of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GcpConfig {
    pub token_ttl: String,
    pub operation_timeout: String,
    pub http_timeout: String,
    pub poll_interval: String,
    pub lock_deadline: String,
    pub fingerprint_attempts: i64,
}

impl Default for GcpConfig {
    fn default() -> Self {
        Self {
            token_ttl: settings::default_gcp_token_ttl(),
            operation_timeout: settings::default_gcp_operation_timeout(),
            http_timeout: settings::default_gcp_http_timeout(),
            poll_interval: settings::default_gcp_poll_interval(),
            lock_deadline: settings::default_gcp_lock_deadline(),
            fingerprint_attempts: settings::default_gcp_fingerprint_attempts(),
        }
    }
}

/// The restart budget as written in `config.toml`.
///
/// Kept raw (an integer and duration strings) and checked by
/// `Config::restart_budget`, not by serde: a typed field would answer a
/// negative `max_restarts` with a parse error about a line, and the reader
/// needs the key's name and the range it may take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RestartConfig {
    pub max_restarts: i64,
    pub window: String,
    pub min_uptime: String,
    pub base_backoff: String,
    pub max_backoff: String,
}

impl Default for RestartConfig {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            window: "10m".to_string(),
            min_uptime: "60s".to_string(),
            base_backoff: "10s".to_string(),
            max_backoff: "5m".to_string(),
        }
    }
}

impl RestartConfig {
    /// Precedence is the variable, then the file, then the default, the same
    /// order as the log level. Each key is resolved to one value first and the
    /// cross-key rules run on that, so a variable that makes the file's other
    /// keys inconsistent is caught like any other bad value.
    fn budget(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<crate::restart_budget::Budget> {
        // An exported-but-empty variable is how a shell spells unset. The
        // source is named in every error: the value may come from a profile
        // the reader is not looking at.
        let var = |key: &str| {
            let name = format!("GTUNNEL_RESTART_{}", key.to_uppercase());
            env(&name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .map(|v| (name, v))
        };
        let bad = |source: &str, why: String| -> anyhow::Error {
            GtunnelError::usage(format!("{source}: {why}")).into()
        };
        let max_restarts = match var("max_restarts") {
            Some((name, raw)) => raw
                .parse::<i64>()
                .map_err(|_| bad(&name, format!("'{raw}' is not a whole number")))
                .map(|n| (name, n))?,
            None => ("[restart] max_restarts".to_string(), self.max_restarts),
        };
        let max_restarts = u32::try_from(max_restarts.1).map_err(|_| {
            bad(
                &max_restarts.0,
                format!("{} is out of range, use 0 to {}", max_restarts.1, u32::MAX),
            )
        })?;
        let duration = |key: &str, file: &str| -> Result<std::time::Duration> {
            let (source, raw) =
                var(key).unwrap_or_else(|| (format!("[restart] {key}"), file.to_string()));
            let d = humantime::parse_duration(&raw)
                .map_err(|e| bad(&source, format!("'{raw}' is not a duration ({e})")))?;
            if d.is_zero() {
                return Err(bad(&source, format!("'{raw}' must be longer than zero")));
            }
            Ok(d)
        };
        let window = duration("window", &self.window)?;
        let min_uptime = duration("min_uptime", &self.min_uptime)?;
        let base_backoff = duration("base_backoff", &self.base_backoff)?;
        let max_backoff = duration("max_backoff", &self.max_backoff)?;
        // A failure forgotten before its own wait ends could never add up
        let shown = |d: std::time::Duration| humantime::format_duration(d).to_string();
        if window < base_backoff {
            return Err(bad(
                "[restart] window",
                format!(
                    "{} is shorter than base_backoff {}",
                    shown(window),
                    shown(base_backoff)
                ),
            ));
        }
        if max_backoff < base_backoff {
            return Err(bad(
                "[restart] max_backoff",
                format!(
                    "{} is shorter than base_backoff {}",
                    shown(max_backoff),
                    shown(base_backoff)
                ),
            ));
        }
        Ok(crate::restart_budget::Budget {
            max_restarts,
            window,
            min_uptime,
            base_backoff,
            max_backoff,
        })
    }
}

/// Which lifecycle edges are worth a desktop notification.
///
/// Four switches rather than one per edge: the edges a person thinks in are
/// "it broke", "it is struggling", "it got better" and "it started", and a
/// finer table is a config nobody reads. `up` is off because the person who
/// ran `start` is looking at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    /// The master switch, off until a person turns it on: off, no edge reaches the desktop whatever the four
    /// below say.
    pub enabled: bool,
    pub up: bool,
    pub degraded: bool,
    pub recovered: bool,
    pub failed: bool,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            up: false,
            degraded: true,
            recovered: true,
            failed: true,
        }
    }
}

/// The part of the precedence that cannot change while the process runs: the
/// `--log-level` flag, then `GTUNNEL_LOG_LEVEL`. `None` when neither is set,
/// which leaves `config.toml` to decide - and leaves it free to decide again
/// after someone edits it, which a TUI session does.
pub fn resolve_log_override(flag: Option<LogLevel>, env: Option<&str>) -> Result<Option<LogLevel>> {
    if let Some(level) = flag {
        return Ok(Some(level));
    }
    // An exported-but-empty variable is how a shell spells "unset"
    match env.map(str::trim).filter(|s| !s.is_empty()) {
        // Naming the variable matters: the value comes from a shell profile
        // the user is not looking at, and without it the error says only that
        // some string somewhere is wrong
        Some(raw) => raw
            .parse()
            .map(Some)
            .map_err(|e| GtunnelError::usage(format!("GTUNNEL_LOG_LEVEL: {e}")).into()),
        None => Ok(None),
    }
}

impl Config {
    /// The budget a supervisor stops by, or the first bad `[restart]` key.
    pub fn restart_budget(&self) -> Result<crate::restart_budget::Budget> {
        self.restart_budget_with(&|name| std::env::var(name).ok())
    }

    /// `env` is a parameter so the precedence is tested without touching the
    /// process environment, which `cargo test` shares between threads.
    pub fn restart_budget_with(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<crate::restart_budget::Budget> {
        self.restart.budget(env)
    }

    // Resolve an account by name, falling back to the selected one
    pub fn get_account(&self, name: Option<&str>) -> Result<&Account> {
        let wanted = name.unwrap_or(&self.selected_account);
        self.accounts
            .iter()
            .find(|a| a.name == wanted)
            .ok_or_else(|| GtunnelError::not_found(format!("Account '{wanted}' not found")).into())
    }

    pub fn get_account_mut(&mut self, name: Option<&str>) -> Result<&mut Account> {
        let wanted = name.unwrap_or(&self.selected_account).to_string();
        self.accounts
            .iter_mut()
            .find(|a| a.name == wanted)
            .ok_or_else(|| GtunnelError::not_found(format!("Account '{wanted}' not found")).into())
    }

    pub fn add_account(&mut self, account: Account) -> Result<()> {
        validate_account_name(&account.name)?;
        if self.accounts.iter().any(|a| a.name == account.name) {
            return Err(
                GtunnelError::usage(format!("Account '{}' already exists", account.name)).into(),
            );
        }
        if self.accounts.is_empty() {
            self.selected_account = account.name.clone();
        }
        self.accounts.push(account);
        Ok(())
    }

    pub fn remove_account(&mut self, name: &str) -> Result<Account> {
        let idx = self
            .accounts
            .iter()
            .position(|a| a.name == name)
            .ok_or_else(|| GtunnelError::not_found(format!("Account '{name}' not found")))?;
        let removed = self.accounts.remove(idx);
        if self.selected_account == name {
            self.selected_account = self
                .accounts
                .first()
                .map(|a| a.name.clone())
                .unwrap_or_default();
        }
        Ok(removed)
    }

    pub fn select_account(&mut self, name: &str) -> Result<()> {
        if !self.accounts.iter().any(|a| a.name == name) {
            return Err(GtunnelError::not_found(format!("Account '{name}' not found")).into());
        }
        self.selected_account = name.to_string();
        Ok(())
    }
}

// Account names become part of service labels and GCP resource names
pub fn validate_account_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 16
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!(
            "Invalid account name '{name}': use 1-16 characters, lowercase letters, digits and \
             dashes, starting with a letter"
        );
    }
    if is_reserved_account_name(name) {
        bail!(
            "Account name '{name}' is reserved: run logs are written as run-<account>-<name>.log, \
             so an account named run or run-\u{2026} would share their file names"
        );
    }
    Ok(())
}

/// `run` and `run-*` are the only names under which a declared tunnel's log
/// (`<account>-<name>.log`) can collide with a run's (`run-<account>-<name>.log`).
/// Split out so `doctor` can ask the same question of accounts that predate it.
pub fn is_reserved_account_name(name: &str) -> bool {
    name == "run" || name.starts_with("run-")
}

pub fn config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("GTUNNEL_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let dot_gtunnel = home.join(".gtunnel");
    if dot_gtunnel.exists() {
        return Ok(dot_gtunnel);
    }
    if let Some(base) = dirs::config_dir() {
        let legacy = base.join("gtunnel");
        if legacy.exists() {
            return Ok(legacy);
        }
    }
    Ok(dot_gtunnel)
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        bail!("gtunnel is not initialized. Run `gtunnel init`.");
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut config: Config =
        toml::from_str(&text).with_context(|| format!("Failed to parse {}", path.display()))?;
    if migrate_to_current(&mut config) {
        // A file that a concurrent loader already migrated reads as "not
        // saved" here, which is harmless: the in-memory value is the same
        let outcome = if save_migration(&config) { 1 } else { 2 };
        MIGRATION.store(outcome, std::sync::atomic::Ordering::SeqCst);
    }
    // A bad [restart] key is refused here, with the file named, so it cannot
    // sit unnoticed until a tunnel is failing and the supervisor reads it
    // The file alone: a variable is the shell's, and blaming this path for it
    // would send the reader to the wrong place
    config
        .restart_budget_with(&|_| None)
        .with_context(|| format!("In {}", path.display()))?;
    // The same refusal for the settings keys, and for the same reason
    crate::settings::resolve(&config, &|_| None)
        .with_context(|| format!("In {}", path.display()))?;
    Ok(config)
}

// A service runs with another working directory and no shell, so `~` and a
// relative path would resolve against nothing. Only an absolute path is used,
// and that is checked where it is used (`Account::credential_file_for_use`),
// not at load: one account's bad path must not make the file unloadable, since
// the commands that repair it load the file first.
pub fn validate_credential_file(path: &std::path::Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(GtunnelError::usage(format!(
            "credential_file must be an absolute path: {}",
            path.display()
        ))
        .into());
    }
    Ok(())
}

/// The refusal every writer of a versioned state file gives when the file says
/// a newer gtunnel wrote it. Reading such a file is allowed, because a newer
/// file is mostly a superset; writing it would drop whatever the newer binary
/// added and this one cannot see, with no sign that anything was lost.
pub fn refuse_newer_version(path: &std::path::Path, found: u32, known: u32) -> Result<()> {
    if found > known {
        bail!(
            "{} was written by a newer gtunnel (v{found}); upgrade this one",
            path.display()
        );
    }
    Ok(())
}

/// Keys the structs leave out of a save when they are unset. A key in the file
/// that the new text no longer has is deleted only if it is one of these: any
/// other key the struct did not write is one this version does not know, and
/// stays.
const OPTIONAL_KEYS: [&str; 3] = ["impersonate_service_account", "credential_file", "expose"];

/// Writes `config` into the text already on disk instead of replacing it, so a
/// key or a comment this version does not know survives any write. The struct
/// is serialised first and merged in: a changed value replaces the old one
/// under the old key, so the comment above the key stays with it.
pub(crate) fn merge_into_existing(existing: &str, config: &Config) -> Result<String> {
    let fresh = toml::to_string_pretty(config).context("Failed to serialize config")?;
    let Ok(mut doc) = existing.parse::<toml_edit::DocumentMut>() else {
        // Nothing to preserve in text that is not TOML; `load_config` already
        // refused such a file, so this is a first write or a damaged one
        return Ok(fresh);
    };
    let new: toml_edit::DocumentMut = fresh
        .parse()
        .context("Failed to re-read the serialised config")?;
    merge_table(doc.as_table_mut(), new.as_table());
    Ok(doc.to_string())
}

fn merge_table(old: &mut toml_edit::Table, new: &toml_edit::Table) {
    use toml_edit::Item;
    let gone: Vec<String> = old
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| OPTIONAL_KEYS.contains(&k.as_str()) && !new.contains_key(k))
        .collect();
    for key in gone {
        old.remove(&key);
    }
    for (key, item) in new.iter() {
        match (old.get_mut(key), item) {
            (Some(Item::Table(o)), Item::Table(n)) => merge_table(o, n),
            (Some(Item::ArrayOfTables(o)), Item::ArrayOfTables(n)) => merge_array(o, n),
            (Some(Item::Value(o)), Item::Value(n)) if same_value(o, n) => {}
            (Some(slot), _) => *slot = item.clone(),
            (None, _) => {
                old.insert(key, item.clone());
            }
        }
    }
}

fn same_value(a: &toml_edit::Value, b: &toml_edit::Value) -> bool {
    let bare = |v: &toml_edit::Value| {
        let mut v = v.clone();
        v.decor_mut().clear();
        v.to_string()
    };
    bare(a) == bare(b)
}

// Rows are matched by their identity, not their position: removing the first
// account must not hand its unknown keys to the second
fn merge_array(old: &mut toml_edit::ArrayOfTables, new: &toml_edit::ArrayOfTables) {
    const IDENTITY: [&str; 2] = ["name", "domain"];
    let id_of = |t: &toml_edit::Table| {
        IDENTITY
            .iter()
            .find_map(|k| t.get(k).and_then(|i| i.as_str()).map(str::to_string))
    };
    let mut merged = toml_edit::ArrayOfTables::new();
    for fresh in new.iter() {
        let matched = id_of(fresh).and_then(|id| {
            old.iter()
                .find(|t| id_of(t).as_deref() == Some(id.as_str()))
        });
        let mut row = match matched {
            Some(existing) => existing.clone(),
            None => {
                merged.push(fresh.clone());
                continue;
            }
        };
        merge_table(&mut row, fresh);
        merged.push(row);
    }
    *old = merged;
}

pub fn save_config(config: &Config) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = config_path()?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if let Some(found) = toml::from_str::<toml::Table>(&existing)
        .ok()
        .and_then(|t| t.get("config_version").and_then(|v| v.as_integer()))
        .and_then(|v| u32::try_from(v).ok())
    {
        refuse_newer_version(&path, found, CONFIG_VERSION)?;
    }
    let text = merge_into_existing(&existing, config)?;
    crate::fsutil::write_atomic(&path, &text)
}

/// Flips `notifications.enabled` in `config.toml` and returns the new value.
///
/// Edits the one line instead of serialising the struct back: a hand-edited
/// file keeps its comments, its key order and anything this version does not
/// know, and the toggle is a keypress that must not rewrite a file a person
/// has curated. The result is parsed before it is written, so a spelling this
/// does not understand (`notifications.enabled = true` as a dotted key) is
/// refused rather than silently ignored.
pub fn toggle_notifications() -> Result<bool> {
    let _lock = lock_config()?;
    let path = config_path()?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("Failed to parse {}", path.display()))?;
    refuse_newer_version(&path, config.config_version, CONFIG_VERSION)?;
    // An unmarked file is read as off, as `load_config` reads it
    let now = config.config_version >= CONFIG_VERSION && config.notifications.enabled;
    let edited = with_notifications_enabled(&text, !now, config.config_version);
    let check: Config = toml::from_str::<Config>(&edited)
        .ok()
        .filter(|c| c.notifications.enabled == !now)
        .ok_or_else(|| anyhow!("could not edit notifications.enabled in {}", path.display()))?;
    debug_assert!(check.config_version >= CONFIG_VERSION);
    crate::fsutil::write_atomic(&path, &edited)?;
    Ok(!now)
}

fn with_notifications_enabled(text: &str, on: bool, version: u32) -> String {
    let value = format!("enabled = {on}");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    match lines.iter().position(|l| l.trim() == "[notifications]") {
        Some(header) => {
            let end = lines[header + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with('['))
                .map_or(lines.len(), |n| header + 1 + n);
            let found = (header + 1..end).find(|&i| {
                let t = lines[i].trim_start();
                t.strip_prefix("enabled")
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            });
            match found {
                Some(i) => {
                    let indent: String =
                        lines[i].chars().take_while(|c| c.is_whitespace()).collect();
                    lines[i] = format!("{indent}{value}");
                }
                None => lines.insert(header + 1, value),
            }
        }
        None => {
            lines.push(String::new());
            lines.push("[notifications]".to_string());
            lines.push(value);
        }
    }
    if version < CONFIG_VERSION {
        let marker = format!("config_version = {CONFIG_VERSION}");
        // Only a top-level line counts: a table's own key of that name is not it
        let top = lines
            .iter()
            .take_while(|l| !l.trim_start().starts_with('['))
            .position(|l| l.trim_start().starts_with("config_version"));
        match top {
            Some(i) => lines[i] = marker,
            None => lines.insert(0, marker),
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

// Same contract as `TunnelState::update`, on its own lock so editing an account
// never waits on a tunnel save. A config that does not exist yet is an error;
// the commands that create one use `update_or_init_config`.
pub fn update_config<T>(f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    locked_config_update(load_config, f)
}

// For `init` and `account add`, which may be writing the first account
pub fn update_or_init_config<T>(f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    locked_config_update(
        || {
            // Only a file that is not there is "no config yet". Any other
            // failure means there is a config we cannot read, and saving
            // over it would delete every account it holds.
            if config_path()?.exists() {
                load_config()
            } else {
                Ok(Config::default())
            }
        },
        f,
    )
}

/// The keys a TOML text carries that `T` does not read, as dotted paths with
/// `[i]` for array elements.
///
/// Loading stays lenient on purpose: a file with a stray key must keep working,
/// so the structs do not deny unknown fields. This is the separate, opt-in
/// check that the editor and `doctor` run, and it asks the real structs what
/// they ignore rather than keeping a second list of known keys that would
/// drift from them.
///
/// Two passes, because one is blind where the other sees. `serde_ignored`
/// watches the deserializer, but an internally tagged enum (`TunnelSpec`,
/// `AccessTarget`) is buffered before its variant is chosen and the buffer
/// loses the trail, so a typo inside a tunnel's `spec` goes unreported. The
/// second pass reads the file into `T`, writes `T` back out, and reports what
/// the file had that the copy does not.
pub fn unknown_keys<T: serde::de::DeserializeOwned + Serialize>(text: &str) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let de = toml::Deserializer::parse(text).map_err(|e| anyhow!("{e}"))?;
    let parsed: T = serde_ignored::deserialize(de, |path| found.push(dotted(&path)))
        .map_err(|e| anyhow!("{e}"))?;
    let original: toml::Value = toml::from_str(text).map_err(|e| anyhow!("{e}"))?;
    let echoed = toml::Value::try_from(&parsed).context("Failed to serialize the parsed file")?;
    missing_from(&original, &echoed, "", &mut found);
    Ok(found)
}

// Appends each key of `original` that `echoed` lacks, once. The root is not
// compared: its keys are never inside a buffered enum, so the first pass has
// them all, and a root table omitted on purpose (`[notifications]` at its
// defaults is not written back) would read as unknown here.
fn missing_from(original: &toml::Value, echoed: &toml::Value, path: &str, out: &mut Vec<String>) {
    match (original, echoed) {
        (toml::Value::Table(o), toml::Value::Table(e)) => {
            for (key, value) in o {
                let key_text = segment(key);
                let here = if path.is_empty() {
                    key_text
                } else {
                    format!("{path}.{key_text}")
                };
                match e.get(key) {
                    Some(echo) => missing_from(value, echo, &here, out),
                    None if path.is_empty() => {}
                    None if out.contains(&here) => {}
                    None => out.push(here),
                }
            }
        }
        (toml::Value::Array(o), toml::Value::Array(e)) => {
            for (i, (value, echo)) in o.iter().zip(e).enumerate() {
                missing_from(value, echo, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

// A key that holds a dot, bracket or space is quoted, so `\"remote.port\"` does
// not read as two levels of the path
fn segment(key: &str) -> String {
    if key.contains(['.', '[', ']', ' ']) {
        format!("\"{key}\"")
    } else {
        key.to_string()
    }
}

// serde_ignored's own Display writes array indices as `.0`; the editor and
// doctor show `[0]`, which reads as an index and not as a key named "0"
fn dotted(path: &serde_ignored::Path<'_>) -> String {
    use serde_ignored::Path;
    match path {
        Path::Root => String::new(),
        Path::Seq { parent, index } => format!("{}[{index}]", dotted(parent)),
        Path::Map { parent, key } => match dotted(parent) {
            head if head.is_empty() => segment(key),
            head => format!("{head}.{}", segment(key)),
        },
        Path::Some { parent }
        | Path::NewtypeStruct { parent }
        | Path::NewtypeVariant { parent } => dotted(parent),
    }
}

// Held from the read to the write by every writer of config.toml; the TUI's
// config page takes it around its final rename for the same reason
pub fn lock_config() -> Result<crate::fsutil::Lock> {
    crate::fsutil::lock_exclusive(
        &config_dir()?.join("config.lock"),
        crate::fsutil::lock_deadline(),
    )
}

fn locked_config_update<T>(
    load: impl FnOnce() -> Result<Config>,
    f: impl FnOnce(&mut Config) -> Result<T>,
) -> Result<T> {
    let _lock = lock_config()?;
    let mut config = load()?;
    let value = f(&mut config)?;
    save_config(&config)?;
    Ok(value)
}

// Tests across several modules point GTUNNEL_CONFIG_DIR at a temp directory.
// The variable is process-wide, so they share one lock and one guard type.
#[cfg(test)]
pub mod testing {
    pub(crate) static CONFIG_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub struct TempConfigDir {
        // Held to keep the directory alive for the test's lifetime, and read
        // only by `path`, whose single caller is the launchd lifecycle test
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl TempConfigDir {
        pub fn new() -> Self {
            let guard = CONFIG_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().expect("tempdir");
            // SAFETY: every test that reads or writes this variable holds
            // CONFIG_DIR_LOCK through the guard above, so none of them races this
            // write. A test that never takes the lock and reads the environment
            // concurrently is outside that guarantee, as it was before the edition
            // made the call `unsafe`.
            unsafe { std::env::set_var("GTUNNEL_CONFIG_DIR", dir.path()) };
            Self { dir, _guard: guard }
        }

        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        pub fn path(&self) -> &std::path::Path {
            self.dir.path()
        }
    }

    impl Drop for TempConfigDir {
        fn drop(&mut self) {
            // SAFETY: `_guard` is still held here (fields drop after this body), so
            // the lock covering `set_var` covers this removal too.
            unsafe { std::env::remove_var("GTUNNEL_CONFIG_DIR") };
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::restart_budget::Budget;
    use std::time::Duration;

    fn budget_of(text: &str) -> anyhow::Result<Budget> {
        toml::from_str::<Config>(text)
            .expect("parse")
            .restart_budget()
    }

    #[test]
    fn restart_defaults_match_the_decision() {
        let b = budget_of("selected_account = \"dev\"\n").expect("defaults");
        assert_eq!(b.max_restarts, 5);
        assert_eq!(b.window, Duration::from_secs(600));
        assert_eq!(b.min_uptime, Duration::from_secs(60));
        assert_eq!(b.base_backoff, Duration::from_secs(10));
        assert_eq!(b.max_backoff, Duration::from_secs(300));
        assert_eq!(b, Budget::default(), "the two defaults cannot drift");
    }

    #[test]
    fn restart_keys_override_one_at_a_time() {
        let b = budget_of("[restart]\nmax_restarts = 2\nbase_backoff = \"1s\"\n").expect("parse");
        assert_eq!(b.max_restarts, 2);
        assert_eq!(b.base_backoff, Duration::from_secs(1));
        assert_eq!(
            b.window,
            Duration::from_secs(600),
            "untouched keys keep theirs"
        );
    }

    #[test]
    fn restart_rejects_window_shorter_than_backoff() {
        let err = budget_of("[restart]\nwindow = \"5s\"\nbase_backoff = \"10s\"\n")
            .expect_err("window below backoff");
        let text = format!("{err:#}");
        assert!(text.contains("window"), "{text}");
        assert!(text.contains("base_backoff"), "{text}");
        assert_eq!(crate::error::kind_of(&err).exit_code(), 2);
    }

    #[test]
    fn restart_rejects_a_value_it_cannot_use_naming_the_key() {
        for (text, key) in [
            ("[restart]\nmax_restarts = -1\n", "max_restarts"),
            ("[restart]\nwindow = \"soon\"\n", "window"),
            ("[restart]\nmin_uptime = \"0s\"\n", "min_uptime"),
            ("[restart]\nmax_backoff = \"1s\"\n", "max_backoff"),
        ] {
            let err = budget_of(text).expect_err(text);
            assert!(format!("{err:#}").contains(key), "{key}: {err:#}");
            assert_eq!(crate::error::kind_of(&err).exit_code(), 2, "{key}");
        }
    }

    #[test]
    fn restart_zero_disables() {
        let b = budget_of("[restart]\nmax_restarts = 0\n").expect("zero is valid");
        assert_eq!(b.max_restarts, 0);
    }

    fn with_env(text: &str, vars: &[(&str, &str)]) -> anyhow::Result<Budget> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        toml::from_str::<Config>(text)
            .expect("parse")
            .restart_budget_with(&|name| {
                vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
            })
    }

    #[test]
    fn a_restart_variable_beats_the_config_file_and_the_file_beats_the_default() {
        let file = "[restart]\nmax_restarts = 2\nwindow = \"20m\"\n";
        let b = with_env(
            file,
            &[
                ("GTUNNEL_RESTART_MAX_RESTARTS", "9"),
                ("GTUNNEL_RESTART_MIN_UPTIME", "2m"),
            ],
        )
        .expect("merge");
        assert_eq!(b.max_restarts, 9, "env over file");
        assert_eq!(b.min_uptime, Duration::from_secs(120), "env over default");
        assert_eq!(b.window, Duration::from_secs(1200), "file over default");
        assert_eq!(b.base_backoff, Duration::from_secs(10), "default");
    }

    #[test]
    fn every_restart_key_has_its_own_variable() {
        let b = with_env(
            "",
            &[
                ("GTUNNEL_RESTART_MAX_RESTARTS", "1"),
                ("GTUNNEL_RESTART_WINDOW", "30m"),
                ("GTUNNEL_RESTART_MIN_UPTIME", "5s"),
                ("GTUNNEL_RESTART_BASE_BACKOFF", "2s"),
                ("GTUNNEL_RESTART_MAX_BACKOFF", "8s"),
            ],
        )
        .expect("all five");
        assert_eq!(b.max_restarts, 1);
        assert_eq!(b.window, Duration::from_secs(1800));
        assert_eq!(b.min_uptime, Duration::from_secs(5));
        assert_eq!(b.base_backoff, Duration::from_secs(2));
        assert_eq!(b.max_backoff, Duration::from_secs(8));
    }

    #[test]
    fn an_invalid_restart_variable_is_a_usage_error_naming_it() {
        for (var, value) in [
            ("GTUNNEL_RESTART_MAX_RESTARTS", "lots"),
            ("GTUNNEL_RESTART_MAX_RESTARTS", "-1"),
            ("GTUNNEL_RESTART_WINDOW", "soon"),
            ("GTUNNEL_RESTART_MIN_UPTIME", "0s"),
        ] {
            let err = with_env("", &[(var, value)]).expect_err(var);
            let text = format!("{err:#}");
            assert!(text.starts_with(var), "{text}");
            assert_eq!(crate::error::kind_of(&err).exit_code(), 2, "{text}");
        }
    }

    #[test]
    fn the_merged_result_is_validated_not_each_source() {
        // Fine in the file alone; the variable makes the window too short
        let err = with_env(
            "[restart]\nbase_backoff = \"1s\"\nwindow = \"5s\"\n",
            &[("GTUNNEL_RESTART_BASE_BACKOFF", "30s")],
        )
        .expect_err("window below the variable's backoff");
        assert_eq!(crate::error::kind_of(&err).exit_code(), 2);
    }

    #[test]
    fn a_restart_variable_of_zero_disables_the_budget() {
        let b = with_env("", &[("GTUNNEL_RESTART_MAX_RESTARTS", "0")]).expect("zero");
        assert_eq!(b.max_restarts, 0);
    }

    #[test]
    fn an_empty_restart_variable_is_unset() {
        let b = with_env(
            "[restart]\nmax_restarts = 3\n",
            &[("GTUNNEL_RESTART_MAX_RESTARTS", " ")],
        )
        .expect("empty is how a shell spells unset");
        assert_eq!(b.max_restarts, 3);
    }

    const DEFAULT_RESTART: &str = "[restart]\nmax_restarts = 5\nwindow = \"10m\"\nmin_uptime = \"60s\"\nbase_backoff = \"10s\"\nmax_backoff = \"5m\"\n";

    fn config_text() -> String {
        std::fs::read_to_string(config_path().expect("path")).expect("config.toml")
    }

    // The file is where a person looks for what to tune, so init writes the
    // whole section rather than leaving the defaults to the source
    #[test]
    fn init_on_a_fresh_dir_writes_all_five_restart_defaults() {
        with_temp_dir(|| {
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains(DEFAULT_RESTART), "{text}");
            let at = |needle: &str| text.find(needle).expect(needle);
            assert!(at("[restart]") < at("[[accounts]]"), "{text}");
        });
    }

    #[test]
    fn init_keeps_a_restart_value_already_in_the_file_and_fills_the_rest() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "selected_account = \"dev\"\n\n[restart]\nmax_restarts = 9\nwindow = \"20m\"\n",
            )
            .expect("write");
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains("max_restarts = 9"), "{text}");
            assert!(text.contains("window = \"20m\""), "{text}");
            for filled in [
                "min_uptime = \"60s\"",
                "base_backoff = \"10s\"",
                "max_backoff = \"5m\"",
            ] {
                assert!(text.contains(filled), "{filled}: {text}");
            }
        });
    }

    #[test]
    fn init_twice_changes_nothing_the_second_time() {
        with_temp_dir(|| {
            update_or_init_config(|c| c.add_account(account("dev"))).expect("first");
            let first = config_text();
            update_or_init_config(|_| Ok(())).expect("second");
            assert_eq!(config_text(), first);
        });
    }

    const DEFAULT_SETTINGS: &str = "start_deadline = \"20s\"\ndiscovery = true\nservice_call_timeout = \"10s\"\nlist_probe_timeout = \"500ms\"\nupdate_check_interval = \"6h\"\neditor = \"\"\n\n[notifications]\nenabled = false\n";

    const UNMARKED_ON: &str =
        "selected_account = \"dev\"\n\n[notifications]\nenabled = true\nup = true\n";

    #[test]
    fn an_absent_notifications_section_and_a_fresh_init_both_read_as_off() {
        let config: Config = toml::from_str("selected_account = \"dev\"\n").expect("parse");
        assert!(!config.notifications.enabled);
        with_temp_dir(|| {
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.starts_with("config_version = 2\n"), "{text}");
            assert!(!load_config().expect("loads").notifications.enabled);
        });
    }

    #[test]
    fn an_unmarked_file_with_notifications_on_loads_off_and_is_saved_once() {
        with_temp_dir(|| {
            let path = config_path().expect("path");
            std::fs::write(&path, UNMARKED_ON).expect("write");
            let loaded = load_config().expect("loads");
            assert!(!loaded.notifications.enabled);
            assert!(loaded.notifications.up, "only the master switch moves");
            let saved = config_text();
            assert!(saved.contains("config_version = 2"), "{saved}");
            assert!(saved.contains("enabled = false"), "{saved}");
            let before = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .expect("mtime");
            std::thread::sleep(std::time::Duration::from_millis(30));
            load_config().expect("second load");
            assert_eq!(config_text(), saved);
            let after = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .expect("mtime");
            assert_eq!(before, after, "the second load rewrote the file");
        });
    }

    #[test]
    fn a_version_2_file_with_notifications_on_stays_on_and_is_not_rewritten() {
        with_temp_dir(|| {
            let path = config_path().expect("path");
            let text = format!("config_version = 2\n{UNMARKED_ON}");
            std::fs::write(&path, &text).expect("write");
            let before = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .expect("mtime");
            std::thread::sleep(std::time::Duration::from_millis(30));
            assert!(load_config().expect("loads").notifications.enabled);
            assert_eq!(config_text(), text);
            let after = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .expect("mtime");
            assert_eq!(before, after);
        });
    }

    #[test]
    fn a_migration_that_cannot_save_still_loads_with_notifications_off() {
        with_temp_dir(|| {
            let path = config_path().expect("path");
            std::fs::write(&path, UNMARKED_ON).expect("write");
            // A directory where the sibling temp file goes makes the write fail
            // for every pid and counter, so the file itself stays readable
            let dir = path.parent().expect("dir");
            let mut perms = std::fs::metadata(dir).expect("meta").permissions();
            let was = perms.clone();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
            std::fs::set_permissions(dir, perms).expect("chmod");
            let loaded = load_config();
            std::fs::set_permissions(dir, was).expect("restore");
            let loaded = loaded.expect("a failed save must not fail the load");
            assert!(!loaded.notifications.enabled);
            assert_eq!(config_text(), UNMARKED_ON, "the file is untouched");
        });
    }

    #[test]
    fn toggling_notifications_changes_one_line_and_nothing_else() {
        with_temp_dir(|| {
            let text = "config_version = 2\n# my notes\nselected_account = \"dev\"\nzzz_unknown = 1\n\n[notifications]\n# master\nenabled = false\nup = true\n\n[[accounts]]\nname = \"dev\"\ngcloud_configuration = \"work\"\nproject = \"example-dev-1a2b\"\ndefault_zone = \"europe-west1-b\"\n";
            std::fs::write(config_path().expect("path"), text).expect("write");
            assert!(toggle_notifications().expect("on"));
            assert_eq!(
                config_text(),
                text.replace("enabled = false", "enabled = true")
            );
            assert!(!toggle_notifications().expect("off"));
            assert_eq!(config_text(), text);
        });
    }

    #[test]
    fn toggling_a_file_without_the_section_or_the_marker_adds_both() {
        with_temp_dir(|| {
            std::fs::write(config_path().expect("path"), "selected_account = \"dev\"\n")
                .expect("write");
            assert!(toggle_notifications().expect("on"));
            let loaded = load_config().expect("loads");
            assert!(loaded.notifications.enabled, "the marker keeps it on");
            assert_eq!(loaded.config_version, CONFIG_VERSION);
        });
    }

    #[test]
    fn the_migration_note_is_for_an_interactive_command_only() {
        assert!(migration_note_wanted(
            MigrationOutcome::Migrated,
            false,
            true
        ));
        assert!(!migration_note_wanted(
            MigrationOutcome::Migrated,
            false,
            false
        ));
        assert!(!migration_note_wanted(
            MigrationOutcome::Migrated,
            true,
            true
        ));
        assert!(!migration_note_wanted(MigrationOutcome::None, false, true));
        assert!(migration_note_wanted(
            MigrationOutcome::NotSaved,
            false,
            true
        ));
    }

    #[test]
    fn init_on_a_fresh_dir_writes_every_settings_key_at_its_default() {
        with_temp_dir(|| {
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains(DEFAULT_SETTINGS), "{text}");
            let at = |needle: &str| text.find(needle).expect(needle);
            assert!(at("start_deadline") < at("[notifications]"), "{text}");
            assert!(at("[notifications]") < at("[restart]"), "{text}");
        });
    }

    #[test]
    fn init_keeps_a_settings_value_already_in_the_file_and_fills_the_rest() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "selected_account = \"dev\"\nstart_deadline = \"90s\"\neditor = \"nano\"\n\n\
                 [notifications]\nenabled = false\nup = true\n",
            )
            .expect("write");
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            for kept in [
                "start_deadline = \"90s\"",
                "editor = \"nano\"",
                "enabled = false",
                "up = true",
            ] {
                assert!(text.contains(kept), "{kept}: {text}");
            }
            for filled in [
                "discovery = true",
                "service_call_timeout = \"10s\"",
                "list_probe_timeout = \"500ms\"",
                "update_check_interval = \"6h\"",
            ] {
                assert!(text.contains(filled), "{filled}: {text}");
            }
        });
    }

    #[test]
    fn init_keeps_an_edited_list_probe_wait() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "selected_account = \"dev\"\nlist_probe_timeout = \"2s\"\n",
            )
            .expect("write");
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains("list_probe_timeout = \"2s\""), "{text}");
            assert!(!text.contains("list_probe_timeout = \"500ms\""), "{text}");
        });
    }

    const DEFAULT_WATCH: &str =
        "[watch]\nprobe_timeout = \"3s\"\ndegraded_after = 2\ninterval = \"30s\"\n";

    #[test]
    fn init_on_a_fresh_dir_writes_the_watch_section_at_its_defaults() {
        with_temp_dir(|| {
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains(DEFAULT_WATCH), "{text}");
        });
    }

    #[test]
    fn init_keeps_a_watch_value_already_in_the_file_and_fills_the_rest() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "selected_account = \"dev\"\n\n[watch]\ninterval = \"45s\"\n",
            )
            .expect("write");
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains("interval = \"45s\""), "{text}");
            assert!(text.contains("probe_timeout = \"3s\""), "{text}");
            assert!(text.contains("degraded_after = 2"), "{text}");
            let first = text;
            update_or_init_config(|_| Ok(())).expect("again");
            assert_eq!(config_text(), first, "a second init changes nothing");
        });
    }

    #[test]
    fn load_config_refuses_a_bad_watch_key_with_exit_2_naming_it() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "[watch]\ndegraded_after = 0\n",
            )
            .expect("write");
            let err = load_config().expect_err("zero is not a threshold");
            assert!(
                format!("{err:#}").contains("watch.degraded_after"),
                "{err:#}"
            );
            assert_eq!(crate::error::kind_of(&err).exit_code(), 2);
        });
    }

    const DEFAULT_LOGS: &str = "[logs]\nmax_size = \"8MiB\"\nkeep_size = \"2MiB\"\nlines_per_minute = 60\nauth_exit_window = \"30s\"\n";

    #[test]
    fn init_on_a_fresh_dir_writes_the_logs_section_at_its_defaults() {
        with_temp_dir(|| {
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains(DEFAULT_LOGS), "{text}");
        });
    }

    #[test]
    fn init_keeps_a_logs_value_already_in_the_file_and_fills_the_rest() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "selected_account = \"dev\"\n\n[logs]\nmax_size = \"16MiB\"\n",
            )
            .expect("write");
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains("max_size = \"16MiB\""), "{text}");
            assert!(text.contains("keep_size = \"2MiB\""), "{text}");
            assert!(text.contains("lines_per_minute = 60"), "{text}");
            let first = text;
            update_or_init_config(|_| Ok(())).expect("again");
            assert_eq!(config_text(), first, "a second init changes nothing");
        });
    }

    const DEFAULT_GCP: &str = "[gcp]\ntoken_ttl = \"45m\"\noperation_timeout = \"10m\"\nhttp_timeout = \"30s\"\npoll_interval = \"2s\"\nlock_deadline = \"5s\"\nfingerprint_attempts = 5\n";

    #[test]
    fn init_on_a_fresh_dir_writes_the_gcp_section_at_its_defaults() {
        with_temp_dir(|| {
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains(DEFAULT_GCP), "{text}");
        });
    }

    #[test]
    fn init_keeps_a_gcp_value_already_in_the_file_and_fills_the_rest() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "selected_account = \"dev\"\n\n[gcp]\nhttp_timeout = \"1m\"\n",
            )
            .expect("write");
            update_or_init_config(|c| c.add_account(account("dev"))).expect("init");
            let text = config_text();
            assert!(text.contains("http_timeout = \"1m\""), "{text}");
            assert!(text.contains("token_ttl = \"45m\""), "{text}");
            assert!(text.contains("fingerprint_attempts = 5"), "{text}");
            let first = text;
            update_or_init_config(|_| Ok(())).expect("again");
            assert_eq!(config_text(), first, "a second init changes nothing");
        });
    }

    #[test]
    fn load_config_refuses_a_zero_gcp_timeout_with_exit_2_naming_the_key() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "[gcp]\npoll_interval = \"0s\"\n",
            )
            .expect("write");
            let err = load_config().expect_err("zero interval");
            assert!(format!("{err:#}").contains("gcp.poll_interval"));
            assert_eq!(crate::error::kind_of(&err).exit_code(), 2);
        });
    }

    #[test]
    fn load_config_refuses_a_keep_size_that_is_not_below_max_size_naming_both() {
        with_temp_dir(|| {
            std::fs::write(
                config_path().expect("path"),
                "[logs]\nmax_size = \"1MiB\"\nkeep_size = \"3MiB\"\n",
            )
            .expect("write");
            let err = load_config().expect_err("keep is above max");
            let text = format!("{err:#}");
            assert!(text.contains("logs.keep_size"), "{text}");
            assert!(text.contains("logs.max_size"), "{text}");
            assert_eq!(crate::error::kind_of(&err).exit_code(), 2);
        });
    }

    #[test]
    fn load_config_refuses_a_bad_settings_key_with_exit_2() {
        with_temp_dir(|| {
            let cfg = Config {
                start_deadline: "soon".to_string(),
                ..Config::default()
            };
            save_config(&cfg).expect("save");
            let err = load_config().expect_err("not a duration");
            assert!(format!("{err:#}").contains("start_deadline"), "{err:#}");
            assert_eq!(crate::error::kind_of(&err).exit_code(), 2);
        });
    }

    #[test]
    fn an_older_file_without_the_settings_keys_still_loads_with_the_defaults() {
        with_temp_dir(|| {
            std::fs::write(config_path().expect("path"), "selected_account = \"dev\"\n")
                .expect("write");
            let loaded = load_config().expect("loads");
            assert_eq!(
                loaded,
                Config {
                    selected_account: "dev".to_string(),
                    ..Config::default()
                }
            );
        });
    }

    #[test]
    fn an_older_file_without_the_section_still_loads_with_the_defaults() {
        with_temp_dir(|| {
            std::fs::write(config_path().expect("path"), "selected_account = \"dev\"\n")
                .expect("write");
            let loaded = load_config().expect("loads");
            assert_eq!(loaded.restart, RestartConfig::default());
        });
    }

    #[test]
    fn an_absent_notifications_section_is_the_defaults() {
        let config: Config = toml::from_str("selected_account = \"dev\"\n").expect("parse");
        assert_eq!(config.notifications, NotifyConfig::default());
        assert!(!config.notifications.up, "a start is not worth a banner");
        assert!(config.notifications.degraded);
        assert!(config.notifications.recovered);
        assert!(config.notifications.failed);
    }

    #[test]
    fn a_partial_notifications_section_keeps_the_other_defaults() {
        let config: Config =
            toml::from_str("[notifications]\nup = true\nrecovered = false\n").expect("parse");
        assert!(config.notifications.up);
        assert!(!config.notifications.recovered);
        assert!(config.notifications.failed);
    }

    #[test]
    fn a_relative_credential_file_is_refused() {
        assert!(validate_credential_file(std::path::Path::new("ci-key.json")).is_err());
        assert!(validate_credential_file(std::path::Path::new("~/ci-key.json")).is_err());
        assert!(validate_credential_file(std::path::Path::new("/secrets/ci-key.json")).is_ok());
    }

    // One account's bad path must not stop the file loading: the commands that
    // repair it (`account edit`, `account remove`, `doctor`) load it first
    #[test]
    fn load_config_accepts_a_relative_credential_file() {
        with_temp_dir(|| {
            let mut cfg = Config::default();
            let mut acct = account("ci");
            acct.credential_file = Some("~/ci-key.json".into());
            cfg.accounts.push(acct);
            cfg.accounts.push(account("other"));
            save_config(&cfg).expect("save");
            let loaded = load_config().expect("a relative path still loads");
            assert_eq!(loaded.accounts.len(), 2);
        });
    }

    #[test]
    fn load_config_refuses_a_bad_restart_key_with_exit_2() {
        with_temp_dir(|| {
            let mut cfg = Config::default();
            cfg.restart.window = "1s".to_string();
            save_config(&cfg).expect("save");
            let err = load_config().expect_err("window below base_backoff");
            assert!(format!("{err:#}").contains("window"), "{err:#}");
            assert_eq!(crate::error::kind_of(&err).exit_code(), 2);
        });
    }

    #[test]
    fn the_use_check_names_the_account_and_is_a_usage_error() {
        let mut acct = account("ci");
        acct.credential_file = Some("~/ci-key.json".into());
        let err = acct.credential_file_for_use().expect_err("relative");
        let text = format!("{err:#}");
        assert!(text.contains("'ci'"), "{text}");
        assert!(text.contains("absolute"), "{text}");
        assert_eq!(
            crate::error::kind_of(&err),
            crate::error::ErrorKind::Usage,
            "{text}"
        );
        acct.credential_file = Some("/secrets/ci-key.json".into());
        assert!(acct.credential_file_for_use().expect("ok").is_some());
        acct.credential_file = None;
        assert!(acct.credential_file_for_use().expect("ok").is_none());
    }

    #[test]
    fn update_or_init_refuses_an_unparseable_config_and_leaves_it_untouched() {
        with_temp_dir(|| {
            let path = config_path().expect("path");
            let text = "selected_account = [broken\n";
            std::fs::write(&path, text).expect("write");
            let err = update_or_init_config(|c| {
                c.accounts.push(account("new"));
                Ok(())
            })
            .expect_err("must not overwrite");
            assert!(
                format!("{err:#}").contains(&path.display().to_string()),
                "{err:#}"
            );
            assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
            // A value the enum does not know is the same refusal
            let bad = "log_level = \"loud\"\n";
            std::fs::write(&path, bad).expect("write");
            assert!(update_or_init_config(|_| Ok(())).is_err());
            assert_eq!(std::fs::read_to_string(&path).expect("read"), bad);
        });
    }

    #[test]
    fn update_or_init_keeps_every_account_when_one_has_a_relative_credential_file() {
        with_temp_dir(|| {
            let text = "selected_account = \"ci\"\n\n[[accounts]]\nname = \"ci\"\ngcloud_configuration = \"ci\"\nproject = \"example-dev-1a2b\"\ndefault_zone = \"europe-west1-b\"\ncredential_file = \"~/ci-key.json\"\n";
            std::fs::write(config_path().expect("path"), text).expect("write");
            update_or_init_config(|c| {
                c.accounts.push(account("new"));
                Ok(())
            })
            .expect("a relative path loads, so the update goes through");
            let loaded = load_config().expect("load");
            assert_eq!(loaded.accounts.len(), 2, "the other account survives");
        });
    }

    #[test]
    fn update_or_init_still_creates_a_missing_config() {
        with_temp_dir(|| {
            assert!(!config_path().expect("path").exists());
            update_or_init_config(|c| {
                c.accounts.push(account("first"));
                Ok(())
            })
            .expect("create");
            assert_eq!(load_config().expect("load").accounts[0].name, "first");
        });
    }

    #[test]
    fn an_account_without_credential_file_round_trips_unchanged() {
        let text = "config_version = 2\nselected_account = \"dev\"\nlog_level = \"info\"\nstart_deadline = \"20s\"\ndiscovery = true\nservice_call_timeout = \"10s\"\nlist_probe_timeout = \"500ms\"\nupdate_check_interval = \"6h\"\neditor = \"\"\n\n[notifications]\nenabled = false\nup = false\ndegraded = true\nrecovered = true\nfailed = true\n\n[restart]\nmax_restarts = 5\nwindow = \"10m\"\nmin_uptime = \"60s\"\nbase_backoff = \"10s\"\nmax_backoff = \"5m\"\n\n[watch]\nprobe_timeout = \"3s\"\ndegraded_after = 2\ninterval = \"30s\"\n\n[logs]\nmax_size = \"8MiB\"\nkeep_size = \"2MiB\"\nlines_per_minute = 60\nauth_exit_window = \"30s\"\n\n[gcp]\ntoken_ttl = \"45m\"\noperation_timeout = \"10m\"\nhttp_timeout = \"30s\"\npoll_interval = \"2s\"\nlock_deadline = \"5s\"\nfingerprint_attempts = 5\n\n[[accounts]]\nname = \"dev\"\ngcloud_configuration = \"work\"\nproject = \"example-dev-1a2b\"\ndefault_zone = \"europe-west1-b\"\n";
        let cfg: Config = toml::from_str(text).expect("parse");
        assert_eq!(cfg.accounts[0].credential_file, None);
        let again = toml::to_string_pretty(&cfg).expect("serialize");
        let back: Config = toml::from_str(&again).expect("reparse");
        assert_eq!(back, cfg);
        assert!(!again.contains("credential_file"), "{again}");
        // Text, not struct: an unset field must not appear as a line either
        assert_eq!(again, text);
    }

    // The section is the one place `enabled` can be found, so a file of
    // defaults still carries it
    #[test]
    fn default_notifications_are_written_back_to_config_toml() {
        let text = toml::to_string_pretty(&Config::default()).expect("serialize");
        assert!(
            text.contains("[notifications]\nenabled = false\nup = false\n"),
            "{text}"
        );
    }

    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelState};

    // The motivating mistake: a top-level key typed after the last table, where
    // toml files it under that table and serde drops it without a word
    const MISPLACED_LOG_LEVEL: &str = "selected_account = \"dev\"\n\
        [[accounts]]\nname = \"dev\"\ngcloud_configuration = \"default\"\n\
        project = \"example-dev-1a2b\"\ndefault_zone = \"europe-west1-b\"\n\
        [accounts.expose]\nrelay_instance = \"relay\"\nrelay_zone = \"europe-west1-b\"\n\
        network = \"default\"\ndefault_domain = \"example.com\"\n\
        [[accounts.expose.domains]]\ndomain = \"example.com\"\nmanaged_zone = \"example\"\n\
        log_level = \"debug\"\n";

    #[test]
    fn log_level_below_an_accounts_table_is_unknown() {
        let keys = unknown_keys::<Config>(MISPLACED_LOG_LEVEL).expect("parse");
        assert_eq!(keys, vec!["accounts[0].expose.domains[0].log_level"]);
        let config: Config = toml::from_str(MISPLACED_LOG_LEVEL).expect("still loads");
        assert_eq!(config.log_level, LogLevel::default());
    }

    #[test]
    fn a_typo_in_a_nested_key_is_reported_with_its_path() {
        let text = "[[tunnels]]\nname = \"db\"\naccount_name = \"dev\"\n\
            [tunnels.spec]\nmode = \"access\"\nremote_prot = 5432\nremote_port = 5432\n\
            local_port = 5432\n[tunnels.spec.target]\nkind = \"instance\"\n\
            instance = \"bastion\"\nzone = \"europe-west1-b\"\n";
        let keys = unknown_keys::<TunnelState>(text).expect("parse");
        assert_eq!(keys, vec!["tunnels[0].spec.remote_prot"]);
        assert!(toml::from_str::<TunnelState>(text).is_ok(), "still loads");
    }

    #[test]
    fn a_key_holding_a_dot_is_quoted_in_its_path() {
        let text = "[[tunnels]]\nname = \"db\"\naccount_name = \"dev\"\n\
            [tunnels.spec]\nmode = \"access\"\nremote_port = 5432\nlocal_port = 5432\n\
            \"stray.key\" = 1\n[tunnels.spec.target]\nkind = \"instance\"\n\
            instance = \"bastion\"\nzone = \"europe-west1-b\"\n";
        let keys = unknown_keys::<TunnelState>(text).expect("parse");
        assert_eq!(keys, vec!["tunnels[0].spec.\"stray.key\""]);
    }

    #[test]
    fn a_file_gtunnel_wrote_has_no_unknown_keys() {
        let mut config = Config {
            selected_account: "dev".into(),
            ..Config::default()
        };
        config.accounts.push(account("dev"));
        let text = toml::to_string_pretty(&config).expect("serialize");
        assert!(
            unknown_keys::<Config>(&text).expect("parse").is_empty(),
            "{text}"
        );

        let access = PersistentTunnel {
            name: "db".into(),
            account_name: "dev".into(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "bastion".into(),
                    zone: "europe-west1-b".into(),
                },
                remote_port: 5432,
                local_port: 5432,
            },
        };
        let expose = PersistentTunnel {
            name: "web".into(),
            account_name: "dev".into(),
            auto_start: true,
            spec: TunnelSpec::Expose {
                local_target: "localhost:3000".into(),
                domain: "example.com".into(),
                hostname: "web.example.com".into(),
                relay_port: 9000,
                private: false,
            },
        };
        let state = TunnelState {
            tunnels: vec![access, expose],
            ..Default::default()
        };
        let text = toml::to_string_pretty(&state).expect("serialize");
        assert!(
            unknown_keys::<TunnelState>(&text)
                .expect("parse")
                .is_empty(),
            "{text}"
        );
    }

    #[test]
    fn a_notifications_table_at_its_defaults_is_not_unknown() {
        let text =
            "[notifications]\nup = false\ndegraded = true\nrecovered = true\nfailed = true\n";
        assert!(unknown_keys::<Config>(text).expect("parse").is_empty());
    }

    #[test]
    fn invalid_toml_is_an_error_not_an_empty_list() {
        assert!(unknown_keys::<Config>("selected_account = ").is_err());
    }

    use super::*;

    fn account(name: &str) -> Account {
        Account {
            name: name.to_string(),
            gcloud_configuration: "default".to_string(),
            project: "proj-1".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: None,
            expose: None,
        }
    }

    fn with_temp_dir<T>(f: impl FnOnce() -> T) -> T {
        let _temp = testing::TempConfigDir::new();
        f()
    }

    #[test]
    fn config_round_trips_through_disk() {
        with_temp_dir(|| {
            let mut cfg = Config::default();
            let mut acct = account("dev");
            acct.expose = Some(ExposeConfig {
                relay_instance: "gt-dev-relay".to_string(),
                relay_zone: "europe-west1-b".to_string(),
                network: "default".to_string(),
                default_domain: "example.com".to_string(),
                domains: vec![DomainConfig {
                    domain: "example.com".to_string(),
                    managed_zone: "example-zone".to_string(),
                }],
            });
            cfg.add_account(acct).expect("add");
            save_config(&cfg).expect("save");

            let loaded = load_config().expect("load");
            assert_eq!(loaded, cfg);
            assert_eq!(loaded.selected_account, "dev");
            assert_eq!(
                loaded.accounts[0]
                    .expose
                    .as_ref()
                    .expect("expose")
                    .default_domain,
                "example.com"
            );
        });
    }

    #[test]
    fn load_without_config_names_init() {
        with_temp_dir(|| {
            let err = load_config().expect_err("should fail");
            assert!(err.to_string().contains("gtunnel init"), "got: {err}");
        });
    }

    #[test]
    fn config_dir_prefers_dot_gtunnel_under_home() {
        if let Some(home) = dirs::home_dir() {
            let dot_gtunnel = home.join(".gtunnel");
            if dot_gtunnel.exists() {
                let guard = testing::CONFIG_DIR_LOCK
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let prev = std::env::var_os("GTUNNEL_CONFIG_DIR");
                // SAFETY: CONFIG_DIR_LOCK is held through the guard above.
                unsafe { std::env::remove_var("GTUNNEL_CONFIG_DIR") };
                let dir = config_dir().expect("config_dir");
                if let Some(p) = prev {
                    // SAFETY: CONFIG_DIR_LOCK is held through the guard above.
                    unsafe { std::env::set_var("GTUNNEL_CONFIG_DIR", p) };
                }
                drop(guard);
                assert_eq!(dir, dot_gtunnel);
            }
        }
    }

    #[test]
    fn saved_config_is_owner_only() {
        with_temp_dir(|| {
            let mut cfg = Config::default();
            cfg.add_account(account("dev")).expect("add");
            save_config(&cfg).expect("save");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(config_path().unwrap())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
            }
        });
    }

    #[test]
    fn get_account_defaults_to_selected() {
        let mut cfg = Config::default();
        cfg.add_account(account("dev")).unwrap();
        cfg.add_account(account("prod")).unwrap();
        cfg.select_account("prod").unwrap();
        assert_eq!(cfg.get_account(None).unwrap().name, "prod");
        assert_eq!(cfg.get_account(Some("dev")).unwrap().name, "dev");
    }

    #[test]
    fn get_unknown_account_is_an_error() {
        let cfg = Config::default();
        let err = cfg.get_account(Some("nope")).expect_err("should fail");
        assert_eq!(err.to_string(), "Account 'nope' not found");
    }

    #[test]
    fn removing_the_selected_account_selects_the_first_remaining() {
        let mut cfg = Config::default();
        cfg.add_account(account("dev")).unwrap();
        cfg.add_account(account("prod")).unwrap();
        cfg.select_account("dev").unwrap();
        let removed = cfg.remove_account("dev").expect("remove");
        assert_eq!(removed.name, "dev");
        assert_eq!(cfg.selected_account, "prod");
    }

    #[test]
    fn removing_the_last_account_clears_the_selection() {
        let mut cfg = Config::default();
        cfg.add_account(account("dev")).unwrap();
        cfg.remove_account("dev").unwrap();
        assert_eq!(cfg.selected_account, "");
        assert!(cfg.accounts.is_empty());
    }

    #[test]
    fn duplicate_accounts_are_rejected() {
        let mut cfg = Config::default();
        cfg.add_account(account("dev")).unwrap();
        let err = cfg.add_account(account("dev")).expect_err("should fail");
        assert!(err.to_string().contains("already exists"), "got: {err}");
    }

    #[test]
    fn account_names_are_validated() {
        assert!(validate_account_name("prod-eu").is_ok());
        assert!(validate_account_name("d1").is_ok());
        assert!(validate_account_name("Prod").is_err());
        assert!(validate_account_name("1dev").is_err());
        assert!(validate_account_name("dev_eu").is_err());
        assert!(validate_account_name("").is_err());
        assert!(validate_account_name("abcdefghijklmnopq").is_err());
        assert!(validate_account_name("abcdefghijklmnop").is_ok());
    }

    #[test]
    fn the_run_prefix_is_reserved_for_run_logs() {
        for name in ["run", "run-eu", "run-"] {
            let err = validate_account_name(name).expect_err(name);
            assert!(err.to_string().contains("is reserved"), "got: {err}");
            assert!(err.to_string().contains(name), "got: {err}");
        }
        assert!(validate_account_name("runner").is_ok());
        assert!(validate_account_name("rundeck").is_ok());
        assert!(validate_account_name("prod-run").is_ok());
        let mut cfg = Config::default();
        assert!(cfg.add_account(account("run-eu")).is_err());
    }

    // Validation guards what is added, not what is already on disk: refusing
    // to load would lock the person out of the very `doctor` that names it.
    #[test]
    fn an_existing_account_named_run_still_loads() {
        let text = "selected_account = \"run\"\n\n[[accounts]]\nname = \"run\"\n\
                    gcloud_configuration = \"default\"\nproject = \"proj-1\"\n\
                    default_zone = \"europe-west1-b\"\n";
        let config: Config = toml::from_str(text).expect("parse");
        assert_eq!(config.get_account(None).expect("account").name, "run");
    }

    // The whole precedence, through the two functions `main` actually calls.
    fn resolve_log_level(
        flag: Option<LogLevel>,
        env: Option<&str>,
        cfg: LogLevel,
    ) -> Result<LogLevel> {
        Ok(crate::loglevel::resolve_effective(
            resolve_log_override(flag, env)?,
            cfg,
        ))
    }

    // Precedence exists so a one-off `--log-level debug` can answer "why is
    // this tunnel quiet" without editing a file and putting it back after.
    #[test]
    fn the_flag_beats_the_environment_which_beats_the_file() {
        let from_file = LogLevel::Quiet;
        assert_eq!(
            resolve_log_level(Some(LogLevel::Debug), Some("quiet"), from_file).unwrap(),
            LogLevel::Debug
        );
        assert_eq!(
            resolve_log_level(None, Some("info"), from_file).unwrap(),
            LogLevel::Info
        );
        assert_eq!(
            resolve_log_level(None, None, from_file).unwrap(),
            LogLevel::Quiet
        );
    }

    #[test]
    fn nothing_set_anywhere_is_info() {
        assert_eq!(
            resolve_log_level(None, None, LogLevel::default()).unwrap(),
            LogLevel::Info
        );
    }

    // Every config.toml written before this field existed has to keep loading
    #[test]
    fn a_config_without_the_field_still_loads_and_is_info() {
        let cfg: Config = toml::from_str(
            r#"
            selected_account = "dev"

            [[accounts]]
            name = "dev"
            gcloud_configuration = "default"
            project = "proj-1"
            default_zone = "europe-west1-b"
            "#,
        )
        .expect("a config written before the field existed");
        assert_eq!(cfg.log_level, LogLevel::Info);
    }

    // A typo in a shell profile would otherwise make every command fail with
    // an error that does not say which variable to fix
    #[test]
    fn a_bad_value_in_the_environment_is_an_error_naming_the_variable() {
        let err = resolve_log_level(None, Some("verbose"), LogLevel::Info)
            .expect_err("an unknown level is not a level");
        let err = err.to_string();
        assert!(err.contains("GTUNNEL_LOG_LEVEL"), "{err}");
        assert!(err.contains("verbose"), "{err}");
        assert!(
            err.contains("quiet") && err.contains("info") && err.contains("debug"),
            "{err}"
        );
    }

    // The flag is the way out of a typo in a shell profile, so it must not
    // need that typo fixed first. The value is never read, so it never fails.
    #[test]
    fn an_explicit_flag_overrides_even_an_unreadable_environment() {
        assert_eq!(
            resolve_log_level(Some(LogLevel::Debug), Some("nonsense"), LogLevel::Info).unwrap(),
            LogLevel::Debug
        );
    }

    // An empty variable is how a shell spells "unset" in practice, and
    // failing on it would make `GTUNNEL_LOG_LEVEL= gtunnel list` an error
    #[test]
    fn an_empty_environment_variable_is_the_same_as_an_unset_one() {
        assert_eq!(
            resolve_log_level(None, Some(""), LogLevel::Quiet).unwrap(),
            LogLevel::Quiet
        );
    }

    #[test]
    fn expose_domain_lookup_falls_back_to_default() {
        let ex = ExposeConfig {
            relay_instance: "r".to_string(),
            relay_zone: "z".to_string(),
            network: "default".to_string(),
            default_domain: "example.com".to_string(),
            domains: vec![DomainConfig {
                domain: "example.com".to_string(),
                managed_zone: "mz".to_string(),
            }],
        };
        assert_eq!(ex.domain(None).unwrap().managed_zone, "mz");
        let err = ex.domain(Some("other.com")).expect_err("should fail");
        assert!(
            err.to_string().contains("gtunnel domains add"),
            "got: {err}"
        );
    }

    fn write_config_text(text: &str) {
        let path = config_path().expect("path");
        std::fs::write(path, text).expect("write fixture");
    }

    fn sample_account(name: &str) -> Account {
        Account {
            name: name.to_string(),
            gcloud_configuration: name.to_string(),
            project: "example-dev-1a2b".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: None,
            expose: None,
        }
    }

    #[test]
    fn a_config_from_a_newer_gtunnel_is_read_but_never_written() {
        with_temp_dir(|| {
            let newer = CONFIG_VERSION + 1;
            write_config_text(&format!(
                "config_version = {newer}\nselected_account = \"dev\"\n"
            ));
            let mut cfg = load_config().expect("a newer file is still read");
            assert_eq!(cfg.selected_account, "dev");

            let before = std::fs::read_to_string(config_path().unwrap()).unwrap();
            cfg.selected_account = "prod".to_string();
            let err = save_config(&cfg).expect_err("a write must refuse");
            assert!(
                format!("{err:#}").contains(&format!(
                    "written by a newer gtunnel (v{newer}); upgrade this one"
                )),
                "{err:#}"
            );
            let err = update_config(|c| {
                c.selected_account = "prod".to_string();
                Ok(())
            })
            .expect_err("an update is a write");
            assert!(format!("{err:#}").contains("newer gtunnel"), "{err:#}");
            let err = toggle_notifications().expect_err("a toggle is a write");
            assert!(format!("{err:#}").contains("newer gtunnel"), "{err:#}");
            assert_eq!(
                std::fs::read_to_string(config_path().unwrap()).unwrap(),
                before,
                "the file is untouched"
            );
        });
    }

    #[test]
    fn a_write_keeps_an_unknown_key_and_a_comment() {
        with_temp_dir(|| {
            let mut cfg = Config::default();
            cfg.accounts.push(sample_account("dev"));
            save_config(&cfg).expect("first save");
            let mut text = std::fs::read_to_string(config_path().unwrap()).unwrap();
            text = format!("# my own notes\nfuture_top_level = \"kept\"\n{text}");
            text = text.replacen(
                "[restart]",
                "[restart]\n# tuned by hand\nfuture_restart_key = 7",
                1,
            );
            text = text.replacen(
                "name = \"dev\"",
                "name = \"dev\"\nfuture_account_key = true",
                1,
            );
            write_config_text(&text);

            let mut loaded = load_config().expect("load");
            loaded.selected_account = "dev".to_string();
            loaded.restart.max_restarts = 9;
            loaded.accounts[0].default_zone = "europe-west1-c".to_string();
            save_config(&loaded).expect("second save");

            let after = std::fs::read_to_string(config_path().unwrap()).unwrap();
            for kept in [
                "# my own notes",
                "future_top_level = \"kept\"",
                "# tuned by hand",
                "future_restart_key = 7",
                "future_account_key = true",
            ] {
                assert!(after.contains(kept), "lost {kept:?} in:\n{after}");
            }
            let reread = load_config().expect("reload");
            assert_eq!(reread, loaded, "the known keys are what was saved");
        });
    }

    #[test]
    fn a_removed_account_and_a_cleared_optional_key_leave_the_file() {
        with_temp_dir(|| {
            let mut cfg = Config::default();
            let mut dev = sample_account("dev");
            dev.impersonate_service_account = Some("sa@example.com".to_string());
            cfg.accounts = vec![dev, sample_account("prod")];
            cfg.accounts[1].impersonate_service_account = Some("sa@example.com".to_string());
            save_config(&cfg).expect("save");
            cfg.accounts.remove(0);
            cfg.accounts[0].impersonate_service_account = None;
            save_config(&cfg).expect("save again");
            let after = std::fs::read_to_string(config_path().unwrap()).unwrap();
            assert!(!after.contains("\"dev\""), "{after}");
            assert!(!after.contains("impersonate_service_account"), "{after}");
            assert_eq!(load_config().unwrap(), cfg);
        });
    }
}
