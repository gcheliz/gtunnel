use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::config;

// Access tunnels listen here; one local port per tunnel
pub const ACCESS_PORT_BASE: u16 = 15000;
pub const ACCESS_PORT_SPAN: u16 = 1000;
// Expose tunnels forward to a port on the relay VM; 30999 is the default backend
pub const RELAY_PORT_BASE: u16 = 30000;
pub const RELAY_PORT_SPAN: u16 = 999;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Running,
    Stopped,
    Error,
}

impl TunnelStatus {
    pub fn symbol(&self) -> &'static str {
        match self {
            TunnelStatus::Running => "\u{25cf}",
            TunnelStatus::Stopped => "\u{25cb}",
            TunnelStatus::Error => "\u{2717}",
        }
    }
}

// What an Access tunnel points at: a compute instance, or a host reached
// through an IAP destination group
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccessTarget {
    Instance {
        instance: String,
        zone: String,
    },
    DestGroup {
        host: String,
        region: String,
        group: String,
        network: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TunnelSpec {
    // Forward a remote port to localhost over IAP
    Access {
        target: AccessTarget,
        remote_port: u16,
        local_port: u16,
    },
    // Publish a local service publicly through the relay
    Expose {
        local_target: String,
        domain: String,
        hostname: String,
        relay_port: u16,
        private: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistentTunnel {
    pub name: String,
    pub account_name: String,
    #[serde(default)]
    pub auto_start: bool,
    pub spec: TunnelSpec,
}

impl PersistentTunnel {
    // Account-qualified base name; daemon backends derive their service label from it
    pub fn label(&self) -> String {
        format!("{}-{}", self.account_name, self.name)
    }

    pub fn log_path(&self) -> Result<PathBuf> {
        Ok(config::config_dir()?
            .join("logs")
            .join(format!("{}.log", self.label())))
    }

    // What the user connects to
    pub fn display_url(&self) -> String {
        match &self.spec {
            TunnelSpec::Access { local_port, .. } => format!("localhost:{local_port}"),
            TunnelSpec::Expose { hostname, .. } => format!("https://{hostname}"),
        }
    }

    // What sits on the other end
    pub fn display_target(&self) -> String {
        match &self.spec {
            TunnelSpec::Access {
                target,
                remote_port,
                ..
            } => match target {
                AccessTarget::Instance { instance, zone } => {
                    format!("{instance}:{remote_port} ({zone})")
                }
                AccessTarget::DestGroup { host, group, .. } => {
                    format!("{host}:{remote_port} via dest-group {group}")
                }
            },
            TunnelSpec::Expose { local_target, .. } => local_target.clone(),
        }
    }

    pub fn is_expose(&self) -> bool {
        matches!(self.spec, TunnelSpec::Expose { .. })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelState {
    #[serde(default)]
    pub tunnels: Vec<PersistentTunnel>,
}

impl TunnelState {
    pub fn load() -> Result<Self> {
        let path = tunnels_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("Failed to parse {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = tunnels_path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self).context("Failed to serialize tunnel state")?;
        std::fs::write(&path, text).with_context(|| format!("Failed to write {}", path.display()))
    }

    pub fn add(&mut self, tunnel: PersistentTunnel) -> Result<()> {
        validate_tunnel_name(&tunnel.name)?;
        if self
            .find_for_account(&tunnel.name, &tunnel.account_name)
            .is_some()
        {
            bail!(
                "Tunnel '{}' already exists on account '{}'",
                tunnel.name,
                tunnel.account_name
            );
        }
        self.tunnels.push(tunnel);
        Ok(())
    }

    pub fn find_for_account(&self, name: &str, account: &str) -> Option<&PersistentTunnel> {
        self.tunnels
            .iter()
            .find(|t| t.name == name && t.account_name == account)
    }

    pub fn find_for_account_mut(
        &mut self,
        name: &str,
        account: &str,
    ) -> Option<&mut PersistentTunnel> {
        self.tunnels
            .iter_mut()
            .find(|t| t.name == name && t.account_name == account)
    }

    pub fn tunnels_for_account(&self, account: &str) -> Vec<&PersistentTunnel> {
        self.tunnels
            .iter()
            .filter(|t| t.account_name == account)
            .collect()
    }

    pub fn remove_for_account(&mut self, name: &str, account: &str) -> Option<PersistentTunnel> {
        let idx = self
            .tunnels
            .iter()
            .position(|t| t.name == name && t.account_name == account)?;
        Some(self.tunnels.remove(idx))
    }

    // Local ports are machine-wide, so they are not scoped to an account
    pub fn used_local_ports(&self) -> HashSet<u16> {
        self.tunnels
            .iter()
            .filter_map(|t| match &t.spec {
                TunnelSpec::Access { local_port, .. } => Some(*local_port),
                _ => None,
            })
            .collect()
    }

    // Relay ports live on one account's relay VM
    pub fn used_relay_ports(&self, account: &str) -> HashSet<u16> {
        self.tunnels
            .iter()
            .filter(|t| t.account_name == account)
            .filter_map(|t| match &t.spec {
                TunnelSpec::Expose { relay_port, .. } => Some(*relay_port),
                _ => None,
            })
            .collect()
    }
}

pub fn tunnels_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("tunnels.toml"))
}

pub fn ensure_logs_dir() -> Result<PathBuf> {
    let dir = config::config_dir()?.join("logs");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir)
}

// Tunnel names become DNS labels and GCP resource name components
pub fn validate_tunnel_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let ok = !name.is_empty()
        && name.len() <= 40
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!(
            "Invalid tunnel name '{name}': use up to 40 characters, lowercase letters, digits and \
             dashes, starting with a letter and ending with a letter or digit"
        );
    }
    Ok(())
}

// ytunnel's name hash, kept so a given name lands on the same port every time
pub fn name_hash(name: &str) -> u32 {
    name.bytes()
        .fold(0u32, |acc, b| acc.wrapping_add(b as u32).wrapping_mul(31))
}

// Pick a port for a tunnel: start from the name's hash so it is stable, then
// probe forward with wraparound past ports that are taken in state or on the host
pub fn allocate_port(
    name: &str,
    base: u16,
    span: u16,
    used: &HashSet<u16>,
    is_free: impl Fn(u16) -> bool,
) -> Result<u16> {
    if span == 0 {
        bail!("Empty port range at {base}");
    }
    let start = (name_hash(name) % span as u32) as u16;
    for offset in 0..span {
        let port = base + (start + offset) % span;
        if !used.contains(&port) && is_free(port) {
            return Ok(port);
        }
    }
    Err(anyhow!("No free port in {}-{}", base, base + span - 1))
}

// Whether nothing on this machine is listening on the port
pub fn local_port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn access_instance(name: &str, local: u16) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "my-vm".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: local,
            },
        }
    }

    fn access_dest_group(name: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::DestGroup {
                    host: "10.0.0.5".to_string(),
                    region: "europe-west1".to_string(),
                    group: "prod".to_string(),
                    network: "default".to_string(),
                },
                remote_port: 5432,
                local_port: 15001,
            },
        }
    }

    fn expose(name: &str, relay_port: u16) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: true,
            spec: TunnelSpec::Expose {
                local_target: "localhost:3000".to_string(),
                domain: "example.com".to_string(),
                hostname: format!("{name}.example.com"),
                relay_port,
                private: false,
            },
        }
    }

    #[test]
    fn state_round_trips_all_three_spec_shapes() {
        let state = TunnelState {
            tunnels: vec![
                access_instance("db", 15432),
                access_dest_group("pg"),
                expose("api", 30123),
            ],
        };
        let text = toml::to_string_pretty(&state).expect("serialize");
        assert!(text.contains(r#"mode = "access""#), "got:\n{text}");
        assert!(text.contains(r#"mode = "expose""#), "got:\n{text}");
        assert!(text.contains(r#"kind = "dest_group""#), "got:\n{text}");
        assert!(text.contains(r#"kind = "instance""#), "got:\n{text}");

        let back: TunnelState = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.tunnels.len(), 3);
        assert_eq!(back.tunnels, state.tunnels);
    }

    #[test]
    fn display_helpers_describe_each_mode() {
        let db = access_instance("db", 15432);
        assert_eq!(db.display_url(), "localhost:15432");
        assert_eq!(db.display_target(), "my-vm:5432 (europe-west1-b)");
        assert_eq!(db.label(), "dev-db");

        let pg = access_dest_group("pg");
        assert_eq!(pg.display_target(), "10.0.0.5:5432 via dest-group prod");

        let api = expose("api", 30123);
        assert_eq!(api.display_url(), "https://api.example.com");
        assert_eq!(api.display_target(), "localhost:3000");
    }

    #[test]
    fn allocate_port_is_stable_for_a_name() {
        let used = HashSet::new();
        let a = allocate_port("db", 15000, 1000, &used, |_| true).unwrap();
        let b = allocate_port("db", 15000, 1000, &used, |_| true).unwrap();
        assert_eq!(a, b);
        assert!((15000..16000).contains(&a), "got {a}");
    }

    #[test]
    fn allocate_port_skips_ports_already_in_state() {
        let first = allocate_port("db", 15000, 1000, &HashSet::new(), |_| true).unwrap();
        let used: HashSet<u16> = [first].into_iter().collect();
        let second = allocate_port("db", 15000, 1000, &used, |_| true).unwrap();
        assert_ne!(second, first);
    }

    // Review Focus 2: a port free in state can still be taken by another process
    #[test]
    fn allocate_port_skips_ports_another_process_holds() {
        let first = allocate_port("db", 15000, 1000, &HashSet::new(), |_| true).unwrap();
        let busy = first;
        let second = allocate_port("db", 15000, 1000, &HashSet::new(), move |p| p != busy).unwrap();
        assert_ne!(second, busy);
    }

    #[test]
    fn allocate_port_wraps_past_the_end_of_the_range() {
        // Force the hash to land on the last port of a 2-port range
        let base = 15000u16;
        let span = 2u16;
        let start = base + (name_hash("db") % span as u32) as u16;
        let used: HashSet<u16> = [start].into_iter().collect();
        let got = allocate_port("db", base, span, &used, |_| true).unwrap();
        assert!(got >= base && got < base + span, "got {got}");
        assert_ne!(got, start);
    }

    #[test]
    fn allocate_port_errors_when_the_range_is_full() {
        let used: HashSet<u16> = (15000..15002).collect();
        let err = allocate_port("db", 15000, 2, &used, |_| true).expect_err("should fail");
        assert!(err.to_string().contains("15000-15001"), "got: {err}");
    }

    #[test]
    fn tunnel_names_are_validated() {
        assert!(validate_tunnel_name("db-prod").is_ok());
        assert!(validate_tunnel_name("a").is_ok());
        assert!(validate_tunnel_name("My_App").is_err());
        assert!(validate_tunnel_name("-a").is_err());
        assert!(validate_tunnel_name("a-").is_err());
        assert!(validate_tunnel_name("").is_err());
        assert!(validate_tunnel_name(&"a".repeat(40)).is_ok());
        assert!(validate_tunnel_name(&"a".repeat(41)).is_err());
    }

    #[test]
    fn duplicate_tunnel_names_per_account_are_rejected() {
        let mut state = TunnelState::default();
        state.add(access_instance("db", 15432)).unwrap();
        let err = state
            .add(access_instance("db", 15433))
            .expect_err("should fail");
        assert!(err.to_string().contains("already exists"), "got: {err}");

        // The same name under a different account is fine
        let mut other = access_instance("db", 15434);
        other.account_name = "prod".to_string();
        state.add(other).expect("different account");
    }

    #[test]
    fn lookups_and_removal_are_scoped_to_an_account() {
        let mut state = TunnelState::default();
        state.add(access_instance("db", 15432)).unwrap();
        let mut other = access_instance("db", 15433);
        other.account_name = "prod".to_string();
        state.add(other).unwrap();

        assert_eq!(state.tunnels_for_account("dev").len(), 1);
        assert!(state.find_for_account("db", "dev").is_some());
        assert!(state.find_for_account("db", "staging").is_none());

        let removed = state.remove_for_account("db", "dev").expect("removed");
        assert_eq!(removed.account_name, "dev");
        assert_eq!(state.tunnels.len(), 1);
        assert_eq!(state.tunnels[0].account_name, "prod");
    }

    #[test]
    fn used_ports_are_collected_per_kind_and_account() {
        let mut state = TunnelState::default();
        state.add(access_instance("db", 15432)).unwrap();
        state.add(expose("api", 30123)).unwrap();
        let mut other = expose("web", 30200);
        other.account_name = "prod".to_string();
        state.add(other).unwrap();

        assert_eq!(state.used_local_ports(), [15432].into_iter().collect());
        assert_eq!(
            state.used_relay_ports("dev"),
            [30123].into_iter().collect::<HashSet<u16>>()
        );
        assert_eq!(
            state.used_relay_ports("prod"),
            [30200].into_iter().collect::<HashSet<u16>>()
        );
    }

    #[test]
    fn local_port_is_free_reports_a_bound_port_as_taken() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        assert!(!local_port_is_free(port));
        drop(listener);
        assert!(local_port_is_free(port));
    }
}
