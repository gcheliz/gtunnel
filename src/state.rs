use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::config;

// Access tunnels listen here; one local port per tunnel
pub const ACCESS_PORT_BASE: u16 = 15000;
pub const ACCESS_PORT_SPAN: u16 = 1000;
// Expose tunnels forward to a port on the relay instance; 30999 is the default backend
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

// What an Access tunnel points at: a port of a compute instance, a host reached
// through an IAP destination group, or a host only the instance can reach,
// forwarded by ssh on the instance with IAP carrying the ssh connection
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
    Jump {
        instance: String,
        zone: String,
        host: String,
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

impl TunnelSpec {
    /// The port this machine listens on, which only an access tunnel has: an
    /// expose tunnel listens on the relay. On the spec, not the tunnel, because
    /// a record and a bare spec both reach for it and a tunnel can delegate.
    pub fn local_port(&self) -> Option<u16> {
        match self {
            TunnelSpec::Access { local_port, .. } => Some(*local_port),
            TunnelSpec::Expose { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistentTunnel {
    pub name: String,
    pub account_name: String,
    #[serde(default)]
    pub auto_start: bool,
    pub spec: TunnelSpec,
}

/// The one place the account and the name are joined. Both names may hold
/// dashes, so `prod` + `eu-api` and `prod-eu` + `api` join to the same string;
/// every caller that needs the joined name goes through here so the clash
/// check and the log and service names cannot disagree about it.
pub fn label_of(account: &str, name: &str) -> String {
    format!("{account}-{name}")
}

/// The one text of the label clash, so `add`, the early reads and relocate
/// refuse in the same words.
pub(crate) fn clash_message(account: &str, name: &str, other: &PersistentTunnel) -> String {
    format!(
        "Tunnel '{name}' on account '{account}' would share its log and service with \
         tunnel '{}' on account '{}' (both are '{}'); choose another name",
        other.name,
        other.account_name,
        other.label()
    )
}

impl PersistentTunnel {
    // Account-qualified base name; daemon backends derive their service label from it
    pub fn label(&self) -> String {
        label_of(&self.account_name, &self.name)
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
                AccessTarget::Jump { instance, host, .. } => {
                    format!("{host}:{remote_port} via {instance}")
                }
                AccessTarget::DestGroup { host, group, .. } => {
                    format!("{host}:{remote_port} via dest-group {group}")
                }
            },
            TunnelSpec::Expose { local_target, .. } => local_target.clone(),
        }
    }

    // What kind of tunnel this is, in one word of at most six characters, so
    // the CLI's list and the TUI's MODE column name the same thing the same
    // way. An expose behind IAP is "gated" rather than "public" because the
    // difference is who may reach it, which is the first thing to know about
    // a published URL.
    pub fn display_kind(&self) -> &'static str {
        match &self.spec {
            TunnelSpec::Expose { private: true, .. } => "gated",
            TunnelSpec::Expose { .. } => "public",
            TunnelSpec::Access { target, .. } => match target {
                AccessTarget::Instance { .. } => "access",
                AccessTarget::DestGroup { .. } => "group",
                AccessTarget::Jump { .. } => "jump",
            },
        }
    }

    pub fn is_expose(&self) -> bool {
        matches!(self.spec, TunnelSpec::Expose { .. })
    }
}

/// How long a reservation may stand before `doctor` calls it stale. A route
/// takes seconds to build, so ten minutes is a command that died, not one that
/// is slow.
pub const RESERVATION_STALE_SECS: u64 = 600;

/// A relay port and name held for an `expose` whose route is still being built.
/// It lives beside `tunnels`, not in it, so status, list, the TUI, MCP and
/// discovery never see a tunnel that has no service and no route yet; only the
/// port allocator, the name check and `doctor` read it. `reserved_at` leads the
/// struct because TOML wants plain values before the tunnel's tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub reserved_at: u64,
    pub tunnel: PersistentTunnel,
}

impl Reservation {
    pub fn is_stale(&self, now: u64) -> bool {
        now.saturating_sub(self.reserved_at) >= RESERVATION_STALE_SECS
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelState {
    #[serde(default)]
    pub tunnels: Vec<PersistentTunnel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reservations: Vec<Reservation>,
}

impl TunnelState {
    pub fn load() -> Result<Self> {
        let path = tunnels_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str(&known_text(&text))
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = tunnels_path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some(found) = schema_version_of(&existing) {
            crate::config::refuse_newer_version(&path, found, TUNNELS_SCHEMA_VERSION)?;
        }
        let body = toml::to_string_pretty(self).context("Failed to serialize tunnel state")?;
        // The version leads because a scalar sits above the tables, and a
        // reader looks there first. It is not a field: every struct literal of
        // a state would have to carry it
        let mut text = format!("schema_version = {TUNNELS_SCHEMA_VERSION}\n{body}");
        // Entries of a kind this version cannot read were set aside on load
        // and are re-read from the file here, under the same lock, so they go
        // back exactly as they came
        for entry in unsupported_in(&existing) {
            text.push('\n');
            text.push_str(&entry.raw);
        }
        crate::fsutil::write_atomic(&path, &text)
    }

    // The only way to change the file: two gtunnels that each load, edit and
    // save would otherwise drop one another's tunnel. The closure must not
    // await or start a daemon, since the lock is held until it returns.
    pub fn update<T>(f: impl FnOnce(&mut TunnelState) -> Result<T>) -> Result<T> {
        let _lock = lock_tunnels()?;
        let mut state = Self::load()?;
        let value = f(&mut state)?;
        state.save()?;
        Ok(value)
    }

    /// Read the state under the same lock `update` takes, and save nothing. A
    /// claim that other readers of the state count (a run's port or name) has
    /// to be decided and recorded before the next claimant reads, so the
    /// claim's own record is written inside `f`, not after it. The same rule as
    /// `update` applies: `f` must not await or start a daemon.
    pub fn with_lock<T>(f: impl FnOnce(&TunnelState) -> Result<T>) -> Result<T> {
        let _lock =
            crate::fsutil::lock_exclusive(&tunnels_lock_path()?, crate::fsutil::lock_deadline())?;
        f(&Self::load()?)
    }

    pub fn add(&mut self, tunnel: PersistentTunnel) -> Result<()> {
        validate_tunnel_name(&tunnel.name)?;
        if let TunnelSpec::Access { target, .. } = &tunnel.spec {
            validate_target(target)?;
        }
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
        self.check_label(&tunnel.account_name, &tunnel.name)?;
        self.tunnels.push(tunnel);
        Ok(())
    }

    /// Tunnels on other accounts whose joined name equals this one's. They would
    /// write the same log file and, on Linux, the same systemd unit. The launchd
    /// label is `com.gtunnel.<account>.<name>`, so macOS cannot collide here.
    pub fn label_holders(&self, account: &str, name: &str) -> Vec<&PersistentTunnel> {
        let label = label_of(account, name);
        self.tunnels
            .iter()
            .filter(|t| t.account_name != account && t.label() == label)
            .collect()
    }

    /// The refusal `add` and the early reads before gcloud share. It names the
    /// first holder only because one name is enough to act on.
    pub fn check_label(&self, account: &str, name: &str) -> Result<()> {
        if let Some(other) = self.label_holders(account, name).first() {
            bail!("{}", clash_message(account, name, other));
        }
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

    /// Every tunnel in an account, by name. The file keeps insertion order,
    /// which makes one account read differently from the next for no reason a
    /// person can see; a name is the one key both accounts share. Because a
    /// name leads with its platform — `k8s`, `mongodb`, `redis-session` — the
    /// sort groups platforms without a table of them to maintain.
    pub fn tunnels_for_account(&self, account: &str) -> Vec<&PersistentTunnel> {
        let mut found: Vec<&PersistentTunnel> = self
            .tunnels
            .iter()
            .filter(|t| t.account_name == account)
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    /// Choose a relay port and hold it, in one step under the caller's lock, so
    /// two `expose` commands cannot pick the same one before either has built
    /// its route. `taken` is what the caller knows beyond this file (live runs);
    /// the saved tunnels and the other reservations are counted here.
    pub fn reserve_relay_port(
        &mut self,
        name: &str,
        account: &str,
        taken: &HashSet<u16>,
        now: u64,
        build: impl FnOnce(u16) -> PersistentTunnel,
    ) -> Result<PersistentTunnel> {
        if self.find_for_account(name, account).is_some() || self.find_reservation(name, account) {
            return Err(crate::error::GtunnelError::usage(format!(
                "Tunnel '{name}' already exists for account '{account}'"
            ))
            .into());
        }
        self.check_label(account, name)?;
        let mut used = self.used_relay_ports(account);
        used.extend(taken);
        let port = allocate_port(name, RELAY_PORT_BASE, RELAY_PORT_SPAN, &used, |_| true)?;
        let tunnel = build(port);
        self.reservations.push(Reservation {
            reserved_at: now,
            tunnel: tunnel.clone(),
        });
        Ok(tunnel)
    }

    fn find_reservation(&self, name: &str, account: &str) -> bool {
        self.reservations
            .iter()
            .any(|r| r.tunnel.name == name && r.tunnel.account_name == account)
    }

    /// Drop a reservation, for a route that failed or a stale one `doctor` is
    /// clearing. Says whether one was there.
    pub fn release_reservation(&mut self, name: &str, account: &str) -> bool {
        let before = self.reservations.len();
        self.reservations
            .retain(|r| !(r.tunnel.name == name && r.tunnel.account_name == account));
        self.reservations.len() != before
    }

    /// Turn a reservation into the saved tunnel once its route exists. A
    /// missing reservation is an error: `doctor` cleared it as stale, and the
    /// port may since have gone to another tunnel.
    pub fn commit_reservation(&mut self, name: &str, account: &str) -> Result<()> {
        let idx = self
            .reservations
            .iter()
            .position(|r| r.tunnel.name == name && r.tunnel.account_name == account)
            .ok_or_else(|| {
                anyhow!("The reservation for '{name}' was cleared while the route was created")
            })?;
        let held = self.reservations.remove(idx);
        self.add(held.tunnel)
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
            .filter_map(|t| t.spec.local_port())
            .collect()
    }

    // Relay ports live on one account's relay instance; a reservation holds its
    // port the same as a saved tunnel does
    pub fn used_relay_ports(&self, account: &str) -> HashSet<u16> {
        self.tunnels
            .iter()
            .chain(self.reservations.iter().map(|r| &r.tunnel))
            .filter(|t| t.account_name == account)
            .filter_map(|t| match &t.spec {
                TunnelSpec::Expose { relay_port, .. } => Some(*relay_port),
                _ => None,
            })
            .collect()
    }
}

/// The version of the layout `tunnels.toml` is written in. A file without the
/// key predates it and reads as 0, so it is readable and is stamped by the next
/// save. Raise it only for a change an older binary would misread: a new kind
/// of tunnel does not need it, because an unknown kind is kept aside.
pub const TUNNELS_SCHEMA_VERSION: u32 = 1;

fn schema_version_of(text: &str) -> Option<u32> {
    toml::from_str::<toml::Table>(text)
        .ok()?
        .get("schema_version")?
        .as_integer()
        .and_then(|v| u32::try_from(v).ok())
}

/// A saved tunnel this version cannot represent: its mode or target kind was
/// added by a newer gtunnel. It is listed and never edited, so `raw` is the
/// entry's own text, which `save` writes back unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedTunnel {
    pub name: String,
    pub account_name: String,
    pub kind: String,
    pub raw: String,
}

const KNOWN_MODES: [&str; 2] = ["access", "expose"];
const KNOWN_TARGETS: [&str; 3] = ["instance", "dest_group", "jump"];

// Only a well-formed entry whose discriminator is a string this version does
// not list counts. Anything else wrong with an entry is a real error and is
// left for the strict parse to name by line
fn unknown_kind(entry: &toml_edit::Table) -> Option<String> {
    let spec = entry.get("spec")?.as_table_like()?;
    let mode = spec.get("mode")?.as_str()?;
    if !KNOWN_MODES.contains(&mode) {
        return Some(mode.to_string());
    }
    let kind = spec.get("target")?.as_table_like()?.get("kind")?.as_str()?;
    (!KNOWN_TARGETS.contains(&kind)).then(|| kind.to_string())
}

fn split_unsupported(text: &str) -> Option<(toml_edit::DocumentMut, Vec<UnsupportedTunnel>)> {
    let mut doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    let array = doc.get_mut("tunnels")?.as_array_of_tables_mut()?;
    let mut found = Vec::new();
    let mut index = 0;
    while index < array.len() {
        let Some(entry) = array.get(index) else {
            break;
        };
        let Some(kind) = unknown_kind(entry) else {
            index += 1;
            continue;
        };
        let text_of = |key: &str| {
            entry
                .get(key)
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let mut alone = toml_edit::ArrayOfTables::new();
        alone.push(entry.clone());
        let mut holder = toml_edit::DocumentMut::new();
        holder.insert("tunnels", toml_edit::Item::ArrayOfTables(alone));
        found.push(UnsupportedTunnel {
            name: text_of("name"),
            account_name: text_of("account_name"),
            kind,
            raw: format!("{}\n", holder.to_string().trim_matches('\n')),
        });
        array.remove(index);
    }
    Some((doc, found))
}

/// The file with the unsupported entries taken out, which is what the structs
/// parse. Text that is not TOML is returned as it is so the strict parse
/// reports it.
fn known_text(text: &str) -> String {
    match split_unsupported(text) {
        Some((doc, found)) if !found.is_empty() => doc.to_string(),
        _ => text.to_string(),
    }
}

/// Keys of `tunnels.toml` that this version drops, for `doctor` and for the
/// config page. `schema_version` is the file's own marker and not a struct
/// field, and an entry of a newer kind is kept aside rather than parsed, so
/// neither counts as a key someone mistyped.
pub fn unknown_tunnel_keys(text: &str) -> Result<Vec<String>> {
    let keys = config::unknown_keys::<TunnelState>(&known_text(text))?;
    Ok(keys.into_iter().filter(|k| k != "schema_version").collect())
}

fn unsupported_in(text: &str) -> Vec<UnsupportedTunnel> {
    split_unsupported(text).map(|(_, f)| f).unwrap_or_default()
}

/// The entries `list` shows as `unsupported by this version`. A file that is
/// not there has none.
pub fn unsupported_tunnels() -> Result<Vec<UnsupportedTunnel>> {
    let path = tunnels_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    Ok(unsupported_in(&text))
}

pub fn tunnels_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("tunnels.toml"))
}

pub fn tunnels_lock_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("tunnels.lock"))
}

// The lock every writer of tunnels.toml holds from its read to its write; the
// TUI's config page takes it around its final rename for the same reason
pub fn lock_tunnels() -> Result<crate::fsutil::Lock> {
    crate::fsutil::lock_exclusive(&tunnels_lock_path()?, crate::fsutil::lock_deadline())
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

// gcloud reads any argv element that starts with a dash as one of its own
// options, and argv never goes through a shell that could quote it away. An
// instance, zone or host that came from `add`, a hand-edited tunnels.toml or an
// agent's tool call is therefore checked before it is placed in a command line:
// the shapes below are what GCE and DNS allow anyway, so no real value is lost.

// GCE instance names: `[a-z]([-a-z0-9]{0,61}[a-z0-9])?`
pub fn validate_instance_name(instance: &str) -> Result<()> {
    let bytes = instance.as_bytes();
    let ok = !instance.is_empty()
        && instance.len() <= 63
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1] != b'-'
        && instance
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!(
            "Invalid instance '{instance}': use up to 63 characters, lowercase letters, digits \
             and dashes, starting with a letter and ending with a letter or digit"
        );
    }
    Ok(())
}

// Zones are checked for their alphabet, not their `region-letter` shape, so a
// zone GCP adds later is not refused by a pattern written today
pub fn validate_zone(zone: &str) -> Result<()> {
    let bytes = zone.as_bytes();
    let ok = !zone.is_empty()
        && zone.len() <= 63
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1] != b'-'
        && zone
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!(
            "Invalid zone '{zone}': expected a zone such as europe-west1-b (lowercase letters, \
             digits and dashes, starting with a letter)"
        );
    }
    Ok(())
}

// A hostname or an IP literal; the colon is for IPv6
pub fn validate_host(host: &str) -> Result<()> {
    let ok = !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'));
    if !ok {
        bail!(
            "Invalid host '{host}': use a hostname or an IP address (letters, digits, dots, \
             dashes and colons, not starting with a dash)"
        );
    }
    Ok(())
}

// Every field of a target that reaches gcloud's argv
pub fn validate_target(target: &AccessTarget) -> Result<()> {
    match target {
        AccessTarget::Instance { instance, zone } => {
            validate_instance_name(instance)?;
            validate_zone(zone)?;
        }
        AccessTarget::Jump {
            instance,
            zone,
            host,
        } => {
            validate_instance_name(instance)?;
            validate_zone(zone)?;
            validate_host(host)?;
        }
        AccessTarget::DestGroup { host, .. } => validate_host(host)?,
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

/// The one place an explicit local port is vetted, for `add` and for `run`.
/// Port 0 is refused first because `local_port_is_free(0)` binds an ephemeral
/// port and succeeds, so a saved 0 would pass every later check and then mean
/// a different port each start. `used` is the caller's to build: `add` counts
/// saved tunnels, `run` counts those and every live run too.
pub fn check_local_port(
    port: u16,
    used: &HashSet<u16>,
    is_free: impl Fn(u16) -> bool,
) -> Result<()> {
    if port == 0 {
        return Err(
            crate::error::GtunnelError::usage("Local port must be between 1 and 65535").into(),
        );
    }
    if used.contains(&port) {
        bail!("Local port {port} is already used by another gtunnel tunnel");
    }
    if !is_free(port) {
        bail!("Local port {port} is already in use on this machine");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn check_local_port_refuses_zero_a_saved_port_and_a_bound_one() {
        let used: HashSet<u16> = [15432].into_iter().collect();
        let zero = check_local_port(0, &used, |_| true).unwrap_err();
        assert_eq!(zero.to_string(), "Local port must be between 1 and 65535");
        assert_eq!(crate::error::kind_of(&zero), crate::error::ErrorKind::Usage);
        assert!(check_local_port(15432, &used, |_| true)
            .unwrap_err()
            .to_string()
            .contains("already used by another gtunnel tunnel"));
        assert!(check_local_port(15433, &used, |_| false)
            .unwrap_err()
            .to_string()
            .contains("already in use on this machine"));
        check_local_port(15433, &used, |_| true).expect("free and unsaved");
    }

    fn access_instance(name: &str, local: u16) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: local,
            },
        }
    }

    #[test]
    fn a_tunnel_whose_joined_name_another_account_holds_is_refused() {
        let mut state = TunnelState::default();
        let mut api = access_instance("api", 15432);
        api.account_name = "prod-eu".to_string();
        state.add(api).unwrap();

        let mut eu_api = access_instance("eu-api", 15433);
        eu_api.account_name = "prod".to_string();
        let err = state.add(eu_api).expect_err("same joined name");
        assert_eq!(
            err.to_string(),
            "Tunnel 'eu-api' on account 'prod' would share its log and service with \
             tunnel 'api' on account 'prod-eu' (both are 'prod-eu-api'); choose another name"
        );
        assert_eq!(state.tunnels.len(), 1);
    }

    #[test]
    fn the_clash_is_refused_in_either_order() {
        let mut state = TunnelState::default();
        let mut eu_api = access_instance("eu-api", 15433);
        eu_api.account_name = "prod".to_string();
        state.add(eu_api).unwrap();
        let mut api = access_instance("api", 15432);
        api.account_name = "prod-eu".to_string();
        assert!(state.add(api).is_err());
    }

    #[test]
    fn names_in_one_account_never_clash() {
        let mut state = TunnelState::default();
        state.add(access_instance("eu-api", 15432)).unwrap();
        state.add(access_instance("api", 15433)).unwrap();
        assert!(state.label_holders("dev", "eu-api").is_empty());
    }

    #[test]
    fn concurrent_updates_do_not_lose_tunnels() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        let handles: Vec<_> = (0..2u16)
            .map(|w| {
                std::thread::spawn(move || {
                    for i in 0..25u16 {
                        TunnelState::update(|s| {
                            s.add(access_instance(&format!("t{w}-{i}"), 20000 + w * 1000 + i))
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(TunnelState::load().unwrap().tunnels.len(), 50);
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

    fn reserve(state: &mut TunnelState, name: &str, now: u64) -> PersistentTunnel {
        state
            .reserve_relay_port(name, "dev", &HashSet::new(), now, |p| expose(name, p))
            .expect("reserved")
    }

    fn relay_port_of(t: &PersistentTunnel) -> u16 {
        match &t.spec {
            TunnelSpec::Expose { relay_port, .. } => *relay_port,
            _ => panic!("not an expose tunnel"),
        }
    }

    // Two names whose hash starts them at the same port: the second has to
    // see the first's claim or both walk away with it
    #[test]
    fn two_reservations_never_get_the_same_port() {
        let first = "web";
        let start = name_hash(first) % RELAY_PORT_SPAN as u32;
        let second = (0..)
            .map(|n| format!("api{n}"))
            .find(|n| name_hash(n) % RELAY_PORT_SPAN as u32 == start)
            .expect("a colliding name");
        let mut state = TunnelState::default();
        let a = reserve(&mut state, first, 0);
        let b = reserve(&mut state, &second, 0);
        assert_ne!(relay_port_of(&a), relay_port_of(&b));
    }

    #[test]
    fn a_reserved_name_is_refused_to_a_second_claimant() {
        let mut state = TunnelState::default();
        reserve(&mut state, "web", 0);
        let again =
            state.reserve_relay_port("web", "dev", &HashSet::new(), 0, |p| expose("web", p));
        assert!(again.is_err());
    }

    #[test]
    fn a_reservation_is_not_a_tunnel_until_it_is_committed() {
        let mut state = TunnelState::default();
        let held = reserve(&mut state, "web", 0);
        assert!(state.tunnels_for_account("dev").is_empty());
        assert!(state
            .used_relay_ports("dev")
            .contains(&relay_port_of(&held)));

        state.commit_reservation("web", "dev").expect("committed");
        assert_eq!(state.tunnels, vec![held]);
        assert!(state.reservations.is_empty());
        assert!(state.commit_reservation("web", "dev").is_err());
    }

    #[test]
    fn a_released_reservation_frees_its_port() {
        let mut state = TunnelState::default();
        let held = reserve(&mut state, "web", 0);
        assert!(state.release_reservation("web", "dev"));
        assert!(!state.release_reservation("web", "dev"));
        assert!(state.used_relay_ports("dev").is_empty());
        assert_eq!(
            relay_port_of(&reserve(&mut state, "web", 0)),
            relay_port_of(&held)
        );
    }

    #[test]
    fn reservations_round_trip_and_an_empty_list_leaves_the_file_unchanged() {
        let mut state = TunnelState::default();
        state.tunnels.push(expose("kept", 30001));
        assert!(!toml::to_string_pretty(&state)
            .expect("serialize")
            .contains("reservations"));

        reserve(&mut state, "web", 42);
        let text = toml::to_string_pretty(&state).expect("serialize");
        let back: TunnelState = toml::from_str(&text).expect("parse");
        assert_eq!(back, state);
    }

    #[test]
    fn a_reservation_is_stale_only_after_the_window() {
        let mut state = TunnelState::default();
        reserve(&mut state, "web", 1000);
        let r = &state.reservations[0];
        assert!(!r.is_stale(1000 + RESERVATION_STALE_SECS - 1));
        assert!(r.is_stale(1000 + RESERVATION_STALE_SECS));
        assert!(!r.is_stale(0), "a clock that went back is not stale");
    }

    fn access_jump(name: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Jump {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                    host: "10.0.0.10".to_string(),
                },
                remote_port: 443,
                local_port: 8443,
            },
        }
    }

    // Every kind a tunnel can be gets its own word, and each word fits the
    // six cells the TUI's MODE column is drawn with
    #[test]
    fn display_kind_distinguishes_every_spec_shape() {
        assert_eq!(access_instance("db", 15432).display_kind(), "access");
        assert_eq!(access_dest_group("pg").display_kind(), "group");
        assert_eq!(access_jump("k8s").display_kind(), "jump");
        assert_eq!(expose("api", 30123).display_kind(), "public");

        let mut gated = expose("admin", 30124);
        if let TunnelSpec::Expose { private, .. } = &mut gated.spec {
            *private = true;
        }
        assert_eq!(gated.display_kind(), "gated");
    }

    #[test]
    fn every_display_kind_fits_the_mode_column() {
        for t in [
            access_instance("db", 15432),
            access_dest_group("pg"),
            access_jump("k8s"),
            expose("api", 30123),
        ] {
            assert!(
                t.display_kind().len() <= 6,
                "{} is wider than the MODE column",
                t.display_kind()
            );
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
            ..Default::default()
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
        assert_eq!(db.display_target(), "bastion:5432 (europe-west1-b)");
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
    fn instance_names_follow_gce_and_never_lead_with_a_dash() {
        for ok in ["bastion", "my-instance", "a", "db1", "postgres-1"] {
            assert!(validate_instance_name(ok).is_ok(), "{ok}");
        }
        assert!(validate_instance_name(&"a".repeat(63)).is_ok());
        for bad in [
            "",
            "--command=id",
            "-db",
            "My-DB",
            "my db",
            "my_db",
            "1db",
            "db-",
            "db;id",
        ] {
            assert!(validate_instance_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_instance_name(&"a".repeat(64)).is_err());
        let err = validate_instance_name("--command=id").unwrap_err();
        assert!(
            err.to_string().contains("Invalid instance '--command=id'"),
            "{err}"
        );
    }

    #[test]
    fn zones_are_lowercase_labels_that_never_lead_with_a_dash() {
        for ok in ["europe-west1-b", "us-central1-a", "z"] {
            assert!(validate_zone(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-x",
            "--zone=a",
            "Europe-west1-b",
            "europe west1-b",
            "a-",
        ] {
            assert!(validate_zone(bad).is_err(), "{bad:?}");
        }
        assert!(validate_zone(&"a".repeat(64)).is_err());
        let err = validate_zone("-x").unwrap_err();
        assert!(err.to_string().contains("Invalid zone '-x'"), "{err}");
    }

    #[test]
    fn hosts_are_names_or_ip_literals_that_never_lead_with_a_dash() {
        for ok in [
            "10.0.0.10",
            "db.internal",
            "cache.example.internal",
            "my-host",
            "::1",
            "fd00::10",
            "Redis.Example.Internal",
        ] {
            assert!(validate_host(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-oProxyCommand=id",
            "-host",
            "a b",
            "a\tb",
            "a/b",
            "a;id",
            "a=b",
        ] {
            assert!(validate_host(bad).is_err(), "{bad:?}");
        }
        assert!(validate_host(&"a".repeat(254)).is_err());
        let err = validate_host("-oProxyCommand=id").unwrap_err();
        assert!(
            err.to_string().contains("Invalid host '-oProxyCommand=id'"),
            "{err}"
        );
    }

    #[test]
    fn a_target_is_checked_field_by_field() {
        let jump = |instance: &str, zone: &str, host: &str| AccessTarget::Jump {
            instance: instance.to_string(),
            zone: zone.to_string(),
            host: host.to_string(),
        };
        assert!(validate_target(&jump("bastion", "europe-west1-b", "10.0.0.10")).is_ok());
        assert!(validate_target(&jump("--x", "europe-west1-b", "10.0.0.10")).is_err());
        assert!(validate_target(&jump("bastion", "-x", "10.0.0.10")).is_err());
        assert!(validate_target(&jump("bastion", "europe-west1-b", "-x")).is_err());
        let group = |host: &str| AccessTarget::DestGroup {
            host: host.to_string(),
            region: "europe-west1".to_string(),
            group: "prod".to_string(),
            network: "default".to_string(),
        };
        assert!(validate_target(&group("10.0.0.5")).is_ok());
        assert!(validate_target(&group("-x")).is_err());
    }

    #[test]
    fn a_tunnel_with_a_flag_for_an_instance_is_not_added() {
        let mut state = TunnelState::default();
        let mut t = access_instance("db", 15432);
        t.spec = TunnelSpec::Access {
            target: AccessTarget::Instance {
                instance: "--command=id".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 22,
            local_port: 15432,
        };
        assert!(state.add(t).is_err());
        assert!(state.tunnels.is_empty());
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

    // Both accounts hold the same platforms under the same names, so a list
    // sorted by name groups by platform on its own: every `redis-*` lands
    // together, and dev reads like prod with the missing rows left out.
    #[test]
    fn an_account_lists_its_tunnels_by_name_whatever_order_the_file_holds() {
        let mut state = TunnelState::default();
        for (account, name) in [
            ("prod", "redis-storage"),
            ("dev", "mongodb"),
            ("prod", "k8s"),
            ("dev", "redis-session"),
            ("prod", "mongodb"),
            ("dev", "k8s"),
            ("prod", "redis-cache"),
        ] {
            let mut t = access_instance(name, 15432);
            t.account_name = account.to_string();
            state.add(t).unwrap();
        }

        let names = |account| -> Vec<String> {
            state
                .tunnels_for_account(account)
                .iter()
                .map(|t| t.name.clone())
                .collect()
        };
        assert_eq!(names("dev"), ["k8s", "mongodb", "redis-session"]);
        assert_eq!(
            names("prod"),
            ["k8s", "mongodb", "redis-cache", "redis-storage"]
        );

        // The shared names appear in the same relative order in both accounts.
        let shared: Vec<String> = names("prod")
            .into_iter()
            .filter(|n| names("dev").contains(n))
            .collect();
        assert_eq!(shared, ["k8s", "mongodb"]);
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
        // Releasing an ephemeral port offers it back to the whole machine, so
        // another test binding port 0 in a sibling thread can take this one
        // before the check runs. The predicate is a bind attempt and nothing
        // more: retry briefly so a lost race reads as a slow pass rather than
        // a failure, and only give up once it is clear nothing is coming back.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !local_port_is_free(port) {
            assert!(
                std::time::Instant::now() < deadline,
                "port {port} never came back: something else holds it"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    const KNOWN_AND_UNKNOWN: &str = r#"schema_version = 1

[[tunnels]]
name = "api"
account_name = "dev"
auto_start = false

[tunnels.spec]
mode = "access"
remote_port = 5432
local_port = 15432

[tunnels.spec.target]
kind = "instance"
instance = "bastion"
zone = "europe-west1-b"

# a kind this version has never heard of
[[tunnels]]
name = "mesh"
account_name = "dev"
auto_start = true

[tunnels.spec]
mode = "mesh_relay"
peers = ["a", "b"]

[tunnels.spec.options]
ttl = 30
"#;

    fn write_tunnels(text: &str) {
        std::fs::write(tunnels_path().unwrap(), text).expect("write fixture");
    }

    #[test]
    fn a_tunnel_of_an_unknown_kind_loads_the_others_and_survives_a_save_byte_for_byte() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        write_tunnels(KNOWN_AND_UNKNOWN);

        let mut state = TunnelState::load().expect("one unknown kind must not fail the file");
        assert_eq!(state.tunnels.len(), 1);
        assert_eq!(state.tunnels[0].name, "api");

        let listed = unsupported_tunnels().expect("scan");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "mesh");
        assert_eq!(listed[0].account_name, "dev");
        assert_eq!(listed[0].kind, "mesh_relay");

        state.tunnels[0].auto_start = true;
        state.add(access_instance("db", 15433)).expect("add");
        state.save().expect("save");

        let after = std::fs::read_to_string(tunnels_path().unwrap()).unwrap();
        let entry = &KNOWN_AND_UNKNOWN[KNOWN_AND_UNKNOWN
            .find("[[tunnels]]\nname = \"mesh\"")
            .unwrap()..];
        assert!(after.contains(entry), "the unknown entry changed:\n{after}");
        assert_eq!(TunnelState::load().unwrap().tunnels.len(), 2);

        // A second save must not grow the file with blank lines
        TunnelState::load().unwrap().save().expect("save again");
        let again = std::fs::read_to_string(tunnels_path().unwrap()).unwrap();
        assert_eq!(again, after, "saving is idempotent");
    }

    #[test]
    fn a_known_kind_with_a_bad_field_still_fails_the_file() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        write_tunnels(
            "[[tunnels]]\nname = \"x\"\naccount_name = \"dev\"\n[tunnels.spec]\nmode = \"access\"\nremote_port = \"no\"\nlocal_port = 1\n[tunnels.spec.target]\nkind = \"instance\"\ninstance = \"b\"\nzone = \"z\"\n",
        );
        assert!(TunnelState::load().is_err());
    }

    #[test]
    fn a_saved_file_says_which_schema_it_follows() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        TunnelState::default().save().expect("save");
        let text = std::fs::read_to_string(tunnels_path().unwrap()).unwrap();
        assert!(
            text.starts_with(&format!("schema_version = {TUNNELS_SCHEMA_VERSION}\n")),
            "{text}"
        );
        TunnelState::load().expect("and reads back");
    }

    #[test]
    fn a_tunnels_file_from_a_newer_gtunnel_is_read_but_never_written() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        let newer = TUNNELS_SCHEMA_VERSION + 1;
        let text = format!(
            "schema_version = {newer}\n\n{}",
            &KNOWN_AND_UNKNOWN[KNOWN_AND_UNKNOWN.find("[[tunnels]]").unwrap()..]
        );
        write_tunnels(&text);

        let mut state = TunnelState::load().expect("a newer file is still read");
        state.tunnels[0].auto_start = true;
        let err = state.save().expect_err("a write must refuse");
        assert!(
            format!("{err:#}").contains(&format!(
                "written by a newer gtunnel (v{newer}); upgrade this one"
            )),
            "{err:#}"
        );
        assert!(TunnelState::update(|_| Ok(())).is_err());
        assert_eq!(
            std::fs::read_to_string(tunnels_path().unwrap()).unwrap(),
            text
        );
    }

    #[test]
    fn an_older_file_without_a_schema_version_is_read_and_stamped_on_the_next_save() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        write_tunnels(&KNOWN_AND_UNKNOWN["schema_version = 1\n\n".len()..]);
        TunnelState::update(|_| Ok(())).expect("update");
        let text = std::fs::read_to_string(tunnels_path().unwrap()).unwrap();
        assert!(text.starts_with("schema_version = 1\n"), "{text}");
    }
}
