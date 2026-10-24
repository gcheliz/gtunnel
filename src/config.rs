use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    #[serde(default)]
    pub accounts: Vec<Account>,
}

impl Config {
    // Resolve an account by name, falling back to the selected one
    pub fn get_account(&self, name: Option<&str>) -> Result<&Account> {
        let wanted = name.unwrap_or(&self.selected_account);
        self.accounts
            .iter()
            .find(|a| a.name == wanted)
            .ok_or_else(|| anyhow!("Account '{wanted}' not found"))
    }

    pub fn get_account_mut(&mut self, name: Option<&str>) -> Result<&mut Account> {
        let wanted = name.unwrap_or(&self.selected_account).to_string();
        self.accounts
            .iter_mut()
            .find(|a| a.name == wanted)
            .ok_or_else(|| anyhow!("Account '{wanted}' not found"))
    }

    pub fn add_account(&mut self, account: Account) -> Result<()> {
        validate_account_name(&account.name)?;
        if self.accounts.iter().any(|a| a.name == account.name) {
            bail!("Account '{}' already exists", account.name);
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
            .ok_or_else(|| anyhow!("Account '{name}' not found"))?;
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
            bail!("Account '{name}' not found");
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
    Ok(())
}

pub fn config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("GTUNNEL_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::config_dir().context("Could not determine the config directory")?;
    Ok(base.join("gtunnel"))
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
    std::fs::write(&path, text).with_context(|| format!("Failed to write {}", path.display()))?;
    set_owner_only(&path)?;
    Ok(())
}

// The config holds project and account identity, so keep it readable only by its owner
#[cfg(unix)]
fn set_owner_only(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

// Tests across several modules point GTUNNEL_CONFIG_DIR at a temp directory.
// The variable is process-wide, so they share one lock and one guard type.
#[cfg(test)]
pub mod testing {
    static CONFIG_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
