use crate::error::GtunnelError;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::loglevel::LogLevel;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose: Option<ExposeConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub selected_account: String,
    // How much every tunnel and every gcloud call says. Global rather than
    // per account, because it answers "how much do I want to see right now",
    // which is a property of the moment and not of a tunnel. It sits above
    // `accounts` because toml writes a value before a table.
    #[serde(default)]
    pub log_level: LogLevel,
    #[serde(default, skip_serializing_if = "NotifyConfig::is_default")]
    pub notifications: NotifyConfig,
    #[serde(default)]
    pub accounts: Vec<Account>,
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
    pub up: bool,
    pub degraded: bool,
    pub recovered: bool,
    pub failed: bool,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            up: false,
            degraded: true,
            recovered: true,
            failed: true,
        }
    }
}

impl NotifyConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
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
    toml::from_str(&text).with_context(|| format!("Failed to parse {}", path.display()))
}

pub fn save_config(config: &Config) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = config_path()?;
    let text = toml::to_string_pretty(config).context("Failed to serialize config")?;
    crate::fsutil::write_atomic(&path, &text)
}

// Same contract as `TunnelState::update`, on its own lock so editing an account
// never waits on a tunnel save. A config that does not exist yet is an error;
// the commands that create one use `update_or_init_config`.
pub fn update_config<T>(f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    locked_config_update(load_config, f)
}

// For `init` and `account add`, which may be writing the first account
pub fn update_or_init_config<T>(f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    locked_config_update(|| Ok(load_config().unwrap_or_default()), f)
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "first caller lands with the editor check")
)]
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "first caller lands with the editor check")
)]
fn missing_from(original: &toml::Value, echoed: &toml::Value, path: &str, out: &mut Vec<String>) {
    match (original, echoed) {
        (toml::Value::Table(o), toml::Value::Table(e)) => {
            for (key, value) in o {
                let here = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
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

// serde_ignored's own Display writes array indices as `.0`; the editor and
// doctor show `[0]`, which reads as an index and not as a key named "0"
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "first caller lands with the editor check")
)]
fn dotted(path: &serde_ignored::Path<'_>) -> String {
    use serde_ignored::Path;
    match path {
        Path::Root => String::new(),
        Path::Seq { parent, index } => format!("{}[{index}]", dotted(parent)),
        Path::Map { parent, key } => match dotted(parent) {
            head if head.is_empty() => key.clone(),
            head => format!("{head}.{key}"),
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
        crate::fsutil::LOCK_DEADLINE,
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
    fn default_notifications_are_not_written_back_to_config_toml() {
        let text = toml::to_string_pretty(&Config::default()).expect("serialize");
        assert!(!text.contains("notifications"), "{text}");
        let custom = Config {
            notifications: NotifyConfig {
                up: true,
                ..NotifyConfig::default()
            },
            ..Config::default()
        };
        let text = toml::to_string_pretty(&custom).expect("serialize");
        assert!(text.contains("[notifications]"), "{text}");
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
}
