use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{is_not_found, GcpClient, Operation};

// IAP forwards arrive from this range; a firewall rule that does not cover it
// is why a tunnel fails with 4003
pub const IAP_RANGE: &str = "35.235.240.0/20";
// Google's load balancer health checkers
pub const LB_HEALTH_CHECK_RANGES: [&str; 2] = ["130.211.0.0/22", "35.191.0.0/16"];

fn last_segment(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub name: String,
    pub zone: String,
    pub status: String,
    pub network: Option<String>,
    pub tags: Vec<String>,
    pub internal_ip: Option<String>,
    pub labels: std::collections::BTreeMap<String, String>,
    // `setMetadata` replaces the whole list, so a change to one key has to be
    // sent with every other key as it was read
    pub metadata: Vec<(String, String)>,
    pub metadata_fingerprint: Option<String>,
}

impl Instance {
    pub fn startup_script(&self) -> Option<&str> {
        self.metadata
            .iter()
            .find(|(k, _)| k == "startup-script")
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Deserialize)]
struct RawInstance {
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    zone: String,
    #[serde(default)]
    tags: RawTags,
    #[serde(default)]
    labels: std::collections::BTreeMap<String, String>,
    #[serde(default, rename = "networkInterfaces")]
    network_interfaces: Vec<RawNic>,
    #[serde(default)]
    metadata: RawMetadata,
}

#[derive(Deserialize, Default)]
struct RawMetadata {
    #[serde(default)]
    fingerprint: Option<String>,
    #[serde(default)]
    items: Vec<RawMetadataItem>,
}

#[derive(Deserialize)]
struct RawMetadataItem {
    key: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize, Default)]
struct RawTags {
    #[serde(default)]
    items: Vec<String>,
}

#[derive(Deserialize)]
struct RawNic {
    #[serde(default)]
    network: Option<String>,
    #[serde(default, rename = "networkIP")]
    network_ip: Option<String>,
}

impl RawInstance {
    fn into_instance(self, zone_hint: &str) -> Instance {
        let zone = if self.zone.is_empty() {
            zone_hint.to_string()
        } else {
            last_segment(&self.zone).to_string()
        };
        let nic = self.network_interfaces.into_iter().next();
        Instance {
            name: self.name,
            zone,
            status: self.status,
            network: nic
                .as_ref()
                .and_then(|n| n.network.as_deref().map(|u| last_segment(u).to_string())),
            tags: self.tags.items,
            internal_ip: nic.and_then(|n| n.network_ip),
            labels: self.labels,
            metadata: self
                .metadata
                .items
                .into_iter()
                .map(|i| (i.key, i.value))
                .collect(),
            metadata_fingerprint: self.metadata.fingerprint,
        }
    }
}

#[derive(Deserialize)]
struct AggregatedInstances {
    #[serde(default)]
    items: std::collections::BTreeMap<String, AggregatedScope>,
}

#[derive(Deserialize)]
struct AggregatedScope {
    #[serde(default)]
    instances: Vec<RawInstance>,
}

pub async fn list_instances(client: &GcpClient, project: &str) -> Result<Vec<Instance>> {
    let url = format!(
        "{}/projects/{project}/aggregated/instances",
        client.endpoints.compute
    );
    let raw: AggregatedInstances = client.get(&url).await?;
    let mut out = Vec::new();
    for (scope, bucket) in raw.items {
        let zone_hint = scope.strip_prefix("zones/").unwrap_or(&scope).to_string();
        for instance in bucket.instances {
            out.push(instance.into_instance(&zone_hint));
        }
    }
    // The aggregated list is grouped by zone, so sort by name: a picker row's
    // position should not depend on which zone the instance happens to be in
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub async fn get_instance(
    client: &GcpClient,
    project: &str,
    zone: &str,
    name: &str,
) -> Result<Option<Instance>> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instances/{name}",
        client.endpoints.compute
    );
    match client.get::<RawInstance>(&url).await {
        Ok(raw) => Ok(Some(raw.into_instance(zone))),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Every instance of the project called `name`, with the zone each one is in.
/// The compute API answers "which zone holds X" with one filtered aggregated
/// list, so a caller that has only a name never has to ask the person for a
/// zone. Zero, one or several come back: names are unique per zone, not per
/// project, and deciding what several means is the resolver's job.
pub async fn find_instances(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Vec<Instance>> {
    let filter = format!("name=\"{name}\"");
    let url = format!(
        "{}/projects/{project}/aggregated/instances?filter={}",
        client.endpoints.compute,
        query_encode(&filter)
    );
    let raw: AggregatedInstances = client.get(&url).await?;
    let mut out = Vec::new();
    for (scope, bucket) in raw.items {
        let zone_hint = scope.strip_prefix("zones/").unwrap_or(&scope).to_string();
        out.extend(
            bucket
                .instances
                .into_iter()
                .filter(|i| i.name == name)
                .map(|i| i.into_instance(&zone_hint)),
        );
    }
    out.sort_by(|a, b| a.zone.cmp(&b.zone));
    Ok(out)
}

// Only unreserved characters pass; `=` and `"` in a filter must not be read as
// query syntax
fn query_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Deserialize)]
struct NamedList {
    #[serde(default)]
    items: Vec<NamedItem>,
}

#[derive(Deserialize)]
struct NamedItem {
    name: String,
}

pub async fn list_zones(client: &GcpClient, project: &str) -> Result<Vec<String>> {
    let url = format!("{}/projects/{project}/zones", client.endpoints.compute);
    let raw: NamedList = client.get(&url).await?;
    Ok(raw.items.into_iter().map(|z| z.name).collect())
}

// Kept as one constant so creation and reconciliation compare against the same
// text: a script that differs by a byte is a relay that is out of date
pub const RELAY_STARTUP_SCRIPT: &str = "#!/bin/bash\n\
    set -e\n\
    grep -q '^GatewayPorts clientspecified' /etc/ssh/sshd_config || \
    echo 'GatewayPorts clientspecified' >> /etc/ssh/sshd_config\n\
    systemctl restart ssh || systemctl restart sshd\n";

// What a reconcile did, so `relay up` can say it instead of printing "exists"
// for a resource it just rewrote
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciled {
    Created,
    Updated,
    Unchanged,
}

// The relay instance: no external IP, SSH reachable only through IAP
pub async fn insert_relay_instance(
    client: &GcpClient,
    project: &str,
    zone: &str,
    name: &str,
    network: &str,
    tag: &str,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instances",
        client.endpoints.compute
    );
    let body = json!({
        "name": name,
        "machineType": format!("zones/{zone}/machineTypes/e2-micro"),
        "disks": [{
            "boot": true,
            "autoDelete": true,
            "initializeParams": {
                "sourceImage": "projects/debian-cloud/global/images/family/debian-12",
                "diskSizeGb": "10"
            }
        }],
        "networkInterfaces": [{"network": format!("global/networks/{network}")}],
        "tags": {"items": [tag]},
        "metadata": {"items": [{"key": "startup-script", "value": RELAY_STARTUP_SCRIPT}]},
        "scheduling": {"automaticRestart": true, "onHostMaintenance": "MIGRATE"}
    });
    client.post(&url, &body).await
}

async fn set_instance_metadata(
    client: &GcpClient,
    project: &str,
    zone: &str,
    name: &str,
    fingerprint: Option<&str>,
    items: &[(String, String)],
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instances/{name}/setMetadata",
        client.endpoints.compute
    );
    let mut body = json!({
        "items": items.iter().map(|(k, v)| json!({"key": k, "value": v})).collect::<Vec<_>>()
    });
    if let Some(fp) = fingerprint {
        body["fingerprint"] = json!(fp);
    }
    client.post(&url, &body).await
}

// Only the startup script is compared: it is the one thing gtunnel puts on the
// VM, and rewriting it is how sshd changes without recreating the instance.
// The caller re-runs it, because metadata alone takes effect at next boot.
pub async fn reconcile_relay_instance(
    client: &GcpClient,
    project: &str,
    zone: &str,
    have: &Instance,
) -> Result<Reconciled> {
    if have.startup_script() == Some(RELAY_STARTUP_SCRIPT) {
        return Ok(Reconciled::Unchanged);
    }
    let mut items: Vec<(String, String)> = have
        .metadata
        .iter()
        .filter(|(k, _)| k != "startup-script")
        .cloned()
        .collect();
    items.push((
        "startup-script".to_string(),
        RELAY_STARTUP_SCRIPT.to_string(),
    ));
    let op = set_instance_metadata(
        client,
        project,
        zone,
        &have.name,
        have.metadata_fingerprint.as_deref(),
        &items,
    )
    .await?;
    client.wait_operation(&op).await?;
    Ok(Reconciled::Updated)
}

pub async fn delete_instance(
    client: &GcpClient,
    project: &str,
    zone: &str,
    name: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instances/{name}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallAllow {
    #[serde(rename = "IPProtocol")]
    pub ip_protocol: String,
    #[serde(default)]
    pub ports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Firewall {
    pub name: String,
    #[serde(default)]
    pub network: String,
    #[serde(default = "ingress")]
    pub direction: String,
    #[serde(default, rename = "sourceRanges")]
    pub source_ranges: Vec<String>,
    #[serde(default)]
    pub allowed: Vec<FirewallAllow>,
    #[serde(default, rename = "targetTags")]
    pub target_tags: Vec<String>,
    #[serde(default)]
    pub disabled: bool,
}

fn ingress() -> String {
    "INGRESS".to_string()
}

#[derive(Deserialize)]
struct FirewallList {
    #[serde(default)]
    items: Vec<Firewall>,
}

pub async fn list_firewalls(client: &GcpClient, project: &str) -> Result<Vec<Firewall>> {
    let url = format!(
        "{}/projects/{project}/global/firewalls",
        client.endpoints.compute
    );
    let raw: FirewallList = client.get(&url).await?;
    Ok(raw.items)
}

pub async fn get_firewall(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<Firewall>> {
    let url = format!(
        "{}/projects/{project}/global/firewalls/{name}",
        client.endpoints.compute
    );
    match client.get::<Firewall>(&url).await {
        Ok(fw) => Ok(Some(fw)),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

pub async fn insert_firewall(
    client: &GcpClient,
    project: &str,
    fw: &Firewall,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/global/firewalls",
        client.endpoints.compute
    );
    client.post(&url, fw).await
}

pub async fn delete_firewall(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/global/firewalls/{name}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

// Whether some enabled ingress rule lets IAP reach this port on these tags
pub fn firewall_allows_iap(
    rules: &[Firewall],
    network: &str,
    instance_tags: &[String],
    port: u16,
) -> bool {
    rules.iter().any(|fw| {
        rule_reaches_instance(fw, network, instance_tags)
            && fw.allowed.iter().any(|a| {
                allows_tcp(a)
                    && (a.ports.is_empty() || a.ports.iter().any(|p| port_spec_covers(p, port)))
            })
    })
}

// Whether some enabled ingress rule lets IAP reach this instance on any TCP
// port. The picker asks it before the port is known, so it shares the rule
// predicate with `firewall_allows_iap` and cannot drift from it.
pub fn firewall_admits_iap(rules: &[Firewall], network: &str, instance_tags: &[String]) -> bool {
    rules.iter().any(|fw| {
        rule_reaches_instance(fw, network, instance_tags) && fw.allowed.iter().any(allows_tcp)
    })
}

// Everything about a rule except which ports it opens
fn rule_reaches_instance(fw: &Firewall, network: &str, instance_tags: &[String]) -> bool {
    !fw.disabled
        && fw.direction.eq_ignore_ascii_case("INGRESS")
        && last_segment(&fw.network) == network
        && fw.source_ranges.iter().any(|r| range_covers_iap(r))
        && (fw.target_tags.is_empty() || fw.target_tags.iter().any(|t| instance_tags.contains(t)))
}

fn allows_tcp(a: &FirewallAllow) -> bool {
    a.ip_protocol.eq_ignore_ascii_case("tcp") || a.ip_protocol.eq_ignore_ascii_case("all")
}

// Whether a source range contains the IAP forwarding range
fn range_covers_iap(range: &str) -> bool {
    if range == IAP_RANGE || range == "0.0.0.0/0" {
        return true;
    }
    let Some((addr, bits)) = range.split_once('/') else {
        return false;
    };
    let Ok(prefix) = bits.parse::<u32>() else {
        return false;
    };
    // A range covers IAP only if it is no more specific than /20 and shares
    // the same network prefix
    if prefix > 20 {
        return false;
    }
    let Ok(base) = addr.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    let iap: std::net::Ipv4Addr = "35.235.240.0".parse().expect("constant");
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(base) & mask) == (u32::from(iap) & mask)
}

fn port_spec_covers(spec: &str, port: u16) -> bool {
    match spec.split_once('-') {
        Some((lo, hi)) => match (lo.trim().parse::<u16>(), hi.trim().parse::<u16>()) {
            (Ok(lo), Ok(hi)) => (lo..=hi).contains(&port),
            _ => false,
        },
        None => spec
            .trim()
            .parse::<u16>()
            .map(|p| p == port)
            .unwrap_or(false),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedPort {
    pub name: String,
    pub port: u16,
}

pub fn merge_named_port(ports: &[NamedPort], name: &str, port: u16) -> Vec<NamedPort> {
    let mut out: Vec<NamedPort> = ports.iter().filter(|p| p.name != name).cloned().collect();
    out.push(NamedPort {
        name: name.to_string(),
        port,
    });
    out
}

pub fn remove_named_port(ports: &[NamedPort], name: &str) -> Vec<NamedPort> {
    ports.iter().filter(|p| p.name != name).cloned().collect()
}

#[derive(Deserialize)]
struct InstanceGroup {
    #[serde(default, rename = "namedPorts")]
    named_ports: Vec<NamedPort>,
    #[serde(default)]
    fingerprint: Option<String>,
}

pub async fn get_named_ports(
    client: &GcpClient,
    project: &str,
    zone: &str,
    group: &str,
) -> Result<(Vec<NamedPort>, Option<String>)> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instanceGroups/{group}",
        client.endpoints.compute
    );
    let ig: InstanceGroup = client.get(&url).await?;
    Ok((ig.named_ports, ig.fingerprint))
}

pub async fn set_named_ports(
    client: &GcpClient,
    project: &str,
    zone: &str,
    group: &str,
    ports: &[NamedPort],
    fingerprint: Option<&str>,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instanceGroups/{group}/setNamedPorts",
        client.endpoints.compute
    );
    let mut body = json!({"namedPorts": ports});
    if let Some(fp) = fingerprint {
        body["fingerprint"] = json!(fp);
    }
    client.post(&url, &body).await
}

pub async fn get_instance_group(
    client: &GcpClient,
    project: &str,
    zone: &str,
    group: &str,
) -> Result<Option<String>> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instanceGroups/{group}",
        client.endpoints.compute
    );
    match client.get::<serde_json::Value>(&url).await {
        Ok(v) => Ok(Some(
            v.get("selfLink")
                .and_then(|s| s.as_str())
                .unwrap_or(&url)
                .to_string(),
        )),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

pub async fn insert_instance_group(
    client: &GcpClient,
    project: &str,
    zone: &str,
    group: &str,
    network: &str,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instanceGroups",
        client.endpoints.compute
    );
    let body = json!({"name": group, "network": format!("global/networks/{network}")});
    client.post(&url, &body).await
}

pub async fn add_instance_to_group(
    client: &GcpClient,
    project: &str,
    zone: &str,
    group: &str,
    instance: &str,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instanceGroups/{group}/addInstances",
        client.endpoints.compute
    );
    let body = json!({
        "instances": [{"instance": format!("zones/{zone}/instances/{instance}")}]
    });
    client.post(&url, &body).await
}

pub async fn delete_instance_group(
    client: &GcpClient,
    project: &str,
    zone: &str,
    group: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/zones/{zone}/instanceGroups/{group}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

// Every gtunnel-owned field of the health check, as one body: creation sends
// it whole and a PATCH sends it whole, so the two cannot drift apart.
// Interval 5 and healthy threshold 1 make a reconnected backend read healthy
// in about 5 s, not 20 s, which is the window in which a single relay 502s.
fn health_check_fields() -> serde_json::Value {
    json!({
        "type": "TCP",
        "tcpHealthCheck": {"portSpecification": "USE_SERVING_PORT"},
        "checkIntervalSec": 5,
        "timeoutSec": 5,
        "healthyThreshold": 1,
        "unhealthyThreshold": 3
    })
}

// Compared field by field, not as a whole: GCP adds fields of its own (ids,
// links, per-protocol defaults) that are not ours to fight over
fn health_check_matches(have: &serde_json::Value) -> bool {
    health_check_fields().as_object().is_some_and(|fields| {
        fields.iter().all(|(key, value)| match key.as_str() {
            "tcpHealthCheck" => {
                have["tcpHealthCheck"]["portSpecification"] == value["portSpecification"]
            }
            _ => have.get(key) == Some(value),
        })
    })
}

// Read-only twin of `reconcile_health_check` for `doctor`: None when the
// health check does not exist
pub async fn health_check_is_current(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<bool>> {
    let url = format!(
        "{}/projects/{project}/global/healthChecks/{name}",
        client.endpoints.compute
    );
    match client.get::<serde_json::Value>(&url).await {
        Ok(have) => Ok(Some(health_check_matches(&have))),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

// One health check serves every backend: USE_SERVING_PORT follows each
// backend's own named port
pub async fn reconcile_health_check(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Reconciled> {
    let base = format!(
        "{}/projects/{project}/global/healthChecks",
        client.endpoints.compute
    );
    let want = health_check_fields();
    let have = match client
        .get::<serde_json::Value>(&format!("{base}/{name}"))
        .await
    {
        Ok(have) => have,
        Err(e) if is_not_found(&e) => {
            let mut body = want;
            body["name"] = json!(name);
            let op: Operation = client.post(&base, &body).await?;
            client.wait_operation(&op).await?;
            return Ok(Reconciled::Created);
        }
        Err(e) => return Err(e),
    };
    if health_check_matches(&have) {
        return Ok(Reconciled::Unchanged);
    }
    let op: Operation = client.patch(&format!("{base}/{name}"), &want).await?;
    client.wait_operation(&op).await?;
    Ok(Reconciled::Updated)
}

pub async fn delete_health_check(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/global/healthChecks/{name}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendService {
    pub name: String,
    pub port_name: Option<String>,
    pub iap_enabled: bool,
    // gtunnel writes its owner tag here; anything else is not ours to delete
    pub description: Option<String>,
    pub timeout_sec: Option<u32>,
    pub draining_timeout_sec: Option<u32>,
}

// The backend fields gtunnel owns. `iap` follows the tunnel; the other two are
// the same for every tunnel, so they live here and not at each call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSettings {
    pub timeout_sec: u32,
    pub draining_sec: u32,
    pub iap: bool,
}

impl BackendSettings {
    pub fn new(iap: bool) -> Self {
        BackendSettings {
            timeout_sec: BACKEND_TIMEOUT_SEC,
            draining_sec: BACKEND_DRAINING_SEC,
            iap,
        }
    }
}

// Long enough for a websocket or a slow upload; the load balancer's own
// default of 30 s cuts both
pub const BACKEND_TIMEOUT_SEC: u32 = 3600;
pub const BACKEND_DRAINING_SEC: u32 = 30;

#[derive(Deserialize)]
struct RawBackendService {
    name: String,
    #[serde(default, rename = "portName")]
    port_name: Option<String>,
    #[serde(default)]
    iap: Option<RawIap>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "timeoutSec")]
    timeout_sec: Option<u32>,
    #[serde(default, rename = "connectionDraining")]
    connection_draining: Option<RawDraining>,
}

#[derive(Deserialize)]
struct RawDraining {
    #[serde(default, rename = "drainingTimeoutSec")]
    draining_timeout_sec: Option<u32>,
}

#[derive(Deserialize)]
struct RawIap {
    #[serde(default)]
    enabled: bool,
}

impl From<RawBackendService> for BackendService {
    fn from(r: RawBackendService) -> Self {
        BackendService {
            name: r.name,
            port_name: r.port_name,
            iap_enabled: r.iap.map(|i| i.enabled).unwrap_or(false),
            description: r.description,
            timeout_sec: r.timeout_sec,
            draining_timeout_sec: r.connection_draining.and_then(|d| d.draining_timeout_sec),
        }
    }
}

#[derive(Deserialize)]
struct BackendServiceList {
    #[serde(default)]
    items: Vec<RawBackendService>,
}

pub async fn list_backend_services(
    client: &GcpClient,
    project: &str,
) -> Result<Vec<BackendService>> {
    let url = format!(
        "{}/projects/{project}/global/backendServices",
        client.endpoints.compute
    );
    let raw: BackendServiceList = client.get(&url).await?;
    Ok(raw.items.into_iter().map(Into::into).collect())
}

pub async fn get_backend_service(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<BackendService>> {
    let url = format!(
        "{}/projects/{project}/global/backendServices/{name}",
        client.endpoints.compute
    );
    match client.get::<RawBackendService>(&url).await {
        Ok(raw) => Ok(Some(raw.into())),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_backend_service(
    client: &GcpClient,
    project: &str,
    name: &str,
    port_name: &str,
    instance_group: &str,
    health_check: &str,
    settings: &BackendSettings,
    owner: &str,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/global/backendServices",
        client.endpoints.compute
    );
    let mut body = json!({
        "name": name,
        "description": owner,
        "loadBalancingScheme": "EXTERNAL_MANAGED",
        "protocol": "HTTP",
        "portName": port_name,
        "healthChecks": [health_check],
        "backends": [{"group": instance_group, "balancingMode": "UTILIZATION"}],
        "timeoutSec": settings.timeout_sec,
        "connectionDraining": {"drainingTimeoutSec": settings.draining_sec}
    });
    if settings.iap {
        body["iap"] = json!({"enabled": true});
    }
    client.post(&url, &body).await
}

// Compares the fields gtunnel owns and patches only the ones that differ, so a
// backend made by an older version converges without being recreated. IAP is
// among them because a tunnel can move between public and private.
pub async fn reconcile_backend_service(
    client: &GcpClient,
    project: &str,
    name: &str,
    want: &BackendSettings,
    have: &BackendService,
) -> Result<Reconciled> {
    let mut patch = serde_json::Map::new();
    if have.timeout_sec != Some(want.timeout_sec) {
        patch.insert("timeoutSec".to_string(), json!(want.timeout_sec));
    }
    if have.draining_timeout_sec != Some(want.draining_sec) {
        patch.insert(
            "connectionDraining".to_string(),
            json!({"drainingTimeoutSec": want.draining_sec}),
        );
    }
    if have.iap_enabled != want.iap {
        patch.insert("iap".to_string(), json!({"enabled": want.iap}));
    }
    if patch.is_empty() {
        return Ok(Reconciled::Unchanged);
    }
    let url = format!(
        "{}/projects/{project}/global/backendServices/{name}",
        client.endpoints.compute
    );
    let op: Operation = client
        .patch(&url, &serde_json::Value::Object(patch))
        .await?;
    client.wait_operation(&op).await?;
    Ok(Reconciled::Updated)
}

// A backend service that sends traffic to one instance group, read apart from
// `BackendService` because only the relay teardown needs the group link and
// the list there answers a different question: who still holds this group
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupHolder {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Deserialize)]
struct RawHolder {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    backends: Vec<RawBackendRef>,
}

#[derive(Deserialize)]
struct RawBackendRef {
    #[serde(default)]
    group: String,
}

#[derive(Deserialize)]
struct RawHolderList {
    #[serde(default)]
    items: Vec<RawHolder>,
}

// TODO: page this through get_all once list calls paginate (#375)
// Matched on the path tail: the link is a full URL whose host is the API
// endpoint, which differs between production and the test fake
pub async fn backend_services_on_group(
    client: &GcpClient,
    project: &str,
    zone: &str,
    group: &str,
) -> Result<Vec<GroupHolder>> {
    let url = format!(
        "{}/projects/{project}/global/backendServices",
        client.endpoints.compute
    );
    let raw: RawHolderList = client.get(&url).await?;
    let tail = format!("/zones/{zone}/instanceGroups/{group}");
    Ok(raw
        .items
        .into_iter()
        .filter(|b| b.backends.iter().any(|r| r.group.ends_with(&tail)))
        .map(|b| GroupHolder {
            name: b.name,
            description: b.description,
        })
        .collect())
}

pub async fn delete_backend_service(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/global/backendServices/{name}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

// The url map is edited read-modify-write under a fingerprint, so it stays raw
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlMap(pub serde_json::Value);

// A path matcher name derived from the host; GCP names must be DNS labels
fn matcher_name(host: &str) -> String {
    let slug: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_lowercase();
    let truncated: String = slug.chars().take(60).collect();
    format!("pm-{}", truncated.trim_end_matches('-'))
}

pub fn with_host_rule(map: &UrlMap, host: &str, backend_url: &str) -> UrlMap {
    let mut value = map.0.clone();
    let name = matcher_name(host);

    let rules = value
        .get("hostRules")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut rules: Vec<serde_json::Value> = rules
        .into_iter()
        .filter(|r| {
            !r.get("hosts")
                .and_then(|h| h.as_array())
                .map(|hosts| hosts.iter().any(|h| h.as_str() == Some(host)))
                .unwrap_or(false)
        })
        .collect();
    rules.push(json!({"hosts": [host], "pathMatcher": name}));

    let matchers = value
        .get("pathMatchers")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let mut matchers: Vec<serde_json::Value> = matchers
        .into_iter()
        .filter(|m| m.get("name").and_then(|n| n.as_str()) != Some(name.as_str()))
        .collect();
    matchers.push(json!({"name": name, "defaultService": backend_url}));

    value["hostRules"] = json!(rules);
    value["pathMatchers"] = json!(matchers);
    UrlMap(value)
}

pub fn without_host_rule(map: &UrlMap, host: &str) -> UrlMap {
    let mut value = map.0.clone();
    let name = matcher_name(host);

    if let Some(rules) = value.get("hostRules").and_then(|r| r.as_array()).cloned() {
        let kept: Vec<serde_json::Value> = rules
            .into_iter()
            .filter(|r| {
                !r.get("hosts")
                    .and_then(|h| h.as_array())
                    .map(|hosts| hosts.iter().any(|h| h.as_str() == Some(host)))
                    .unwrap_or(false)
            })
            .collect();
        value["hostRules"] = json!(kept);
    }
    if let Some(matchers) = value
        .get("pathMatchers")
        .and_then(|m| m.as_array())
        .cloned()
    {
        let kept: Vec<serde_json::Value> = matchers
            .into_iter()
            .filter(|m| m.get("name").and_then(|n| n.as_str()) != Some(name.as_str()))
            .collect();
        value["pathMatchers"] = json!(kept);
    }
    UrlMap(value)
}

pub async fn get_url_map(client: &GcpClient, project: &str, name: &str) -> Result<Option<UrlMap>> {
    let url = format!(
        "{}/projects/{project}/global/urlMaps/{name}",
        client.endpoints.compute
    );
    match client.get::<serde_json::Value>(&url).await {
        Ok(v) => Ok(Some(UrlMap(v))),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

pub async fn insert_url_map(
    client: &GcpClient,
    project: &str,
    name: &str,
    default_service: &str,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/global/urlMaps",
        client.endpoints.compute
    );
    let body = json!({"name": name, "defaultService": default_service});
    client.post(&url, &body).await
}

pub async fn update_url_map(
    client: &GcpClient,
    project: &str,
    name: &str,
    map: &UrlMap,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/global/urlMaps/{name}",
        client.endpoints.compute
    );
    client.put(&url, &map.0).await
}

pub async fn delete_url_map(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/global/urlMaps/{name}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

pub async fn get_global_address(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<String>> {
    let url = format!(
        "{}/projects/{project}/global/addresses/{name}",
        client.endpoints.compute
    );
    match client.get::<serde_json::Value>(&url).await {
        Ok(v) => Ok(v.get("address").and_then(|a| a.as_str()).map(String::from)),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

pub async fn reserve_global_address(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<String> {
    if let Some(ip) = get_global_address(client, project, name).await? {
        return Ok(ip);
    }
    let url = format!(
        "{}/projects/{project}/global/addresses",
        client.endpoints.compute
    );
    let body = json!({"name": name, "ipVersion": "IPV4"});
    let op: Operation = client.post(&url, &body).await?;
    client.wait_operation(&op).await?;
    get_global_address(client, project, name)
        .await?
        .context("Reserved address has no IP")
}

pub async fn delete_global_address(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/global/addresses/{name}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

pub async fn get_target_https_proxy(client: &GcpClient, project: &str, name: &str) -> Result<bool> {
    let url = format!(
        "{}/projects/{project}/global/targetHttpsProxies/{name}",
        client.endpoints.compute
    );
    match client.get::<serde_json::Value>(&url).await {
        Ok(_) => Ok(true),
        Err(e) if is_not_found(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

pub async fn insert_target_https_proxy(
    client: &GcpClient,
    project: &str,
    name: &str,
    url_map: &str,
    cert_map: &str,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/global/targetHttpsProxies",
        client.endpoints.compute
    );
    let body = json!({"name": name, "urlMap": url_map, "certificateMap": cert_map});
    client.post(&url, &body).await
}

pub async fn delete_target_https_proxy(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/global/targetHttpsProxies/{name}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

pub async fn insert_global_forwarding_rule(
    client: &GcpClient,
    project: &str,
    name: &str,
    ip: &str,
    proxy: &str,
) -> Result<Operation> {
    let url = format!(
        "{}/projects/{project}/global/forwardingRules",
        client.endpoints.compute
    );
    let body = json!({
        "name": name,
        "IPAddress": ip,
        "IPProtocol": "TCP",
        "portRange": "443",
        "target": proxy,
        "loadBalancingScheme": "EXTERNAL_MANAGED"
    });
    client.post(&url, &body).await
}

pub async fn get_global_forwarding_rule(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<bool> {
    let url = format!(
        "{}/projects/{project}/global/forwardingRules/{name}",
        client.endpoints.compute
    );
    match client.get::<serde_json::Value>(&url).await {
        Ok(_) => Ok(true),
        Err(e) if is_not_found(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

pub async fn delete_global_forwarding_rule(
    client: &GcpClient,
    project: &str,
    name: &str,
) -> Result<Option<Operation>> {
    let url = format!(
        "{}/projects/{project}/global/forwardingRules/{name}",
        client.endpoints.compute
    );
    client.delete(&url).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fw(name: &str, network: &str, ranges: &[&str], ports: &[&str], tags: &[&str]) -> Firewall {
        Firewall {
            name: name.to_string(),
            network: format!(
                "https://www.googleapis.com/compute/v1/projects/p/global/networks/{network}"
            ),
            direction: "INGRESS".to_string(),
            source_ranges: ranges.iter().map(|s| s.to_string()).collect(),
            allowed: vec![FirewallAllow {
                ip_protocol: "tcp".to_string(),
                ports: ports.iter().map(|s| s.to_string()).collect(),
            }],
            target_tags: tags.iter().map(|s| s.to_string()).collect(),
            disabled: false,
        }
    }

    // Review Focus 3: a missing IAP firewall rule is the most common reason a
    // tunnel fails with 4003, so the check has to be exact
    #[test]
    fn firewall_allows_iap_accepts_a_matching_rule() {
        let rules = vec![fw("a", "default", &["35.235.240.0/20"], &["5432"], &[])];
        assert!(firewall_allows_iap(&rules, "default", &[], 5432));
    }

    #[test]
    fn firewall_allows_iap_accepts_a_wider_range_and_no_ports() {
        let rules = vec![fw("a", "default", &["0.0.0.0/0"], &[], &[])];
        assert!(firewall_allows_iap(&rules, "default", &[], 5432));
    }

    #[test]
    fn firewall_allows_iap_accepts_a_port_range() {
        let rules = vec![fw(
            "a",
            "default",
            &["35.235.240.0/20"],
            &["8000-8080"],
            &[],
        )];
        assert!(firewall_allows_iap(&rules, "default", &[], 8080));
        assert!(!firewall_allows_iap(&rules, "default", &[], 8081));
    }

    #[test]
    fn firewall_allows_iap_rejects_disabled_rules() {
        let mut rules = vec![fw("a", "default", &["35.235.240.0/20"], &["5432"], &[])];
        rules[0].disabled = true;
        assert!(!firewall_allows_iap(&rules, "default", &[], 5432));
    }

    #[test]
    fn firewall_allows_iap_rejects_another_network() {
        let rules = vec![fw("a", "other", &["35.235.240.0/20"], &["5432"], &[])];
        assert!(!firewall_allows_iap(&rules, "default", &[], 5432));
    }

    #[test]
    fn firewall_allows_iap_rejects_egress_and_wrong_protocol() {
        let mut egress = fw("a", "default", &["35.235.240.0/20"], &["5432"], &[]);
        egress.direction = "EGRESS".to_string();
        assert!(!firewall_allows_iap(&[egress], "default", &[], 5432));

        let mut udp = fw("a", "default", &["35.235.240.0/20"], &["5432"], &[]);
        udp.allowed[0].ip_protocol = "udp".to_string();
        assert!(!firewall_allows_iap(&[udp], "default", &[], 5432));
    }

    #[test]
    fn firewall_target_tags_must_intersect_the_instance_tags() {
        let rules = vec![fw(
            "a",
            "default",
            &["35.235.240.0/20"],
            &["5432"],
            &["web"],
        )];
        assert!(!firewall_allows_iap(
            &rules,
            "default",
            &["db".to_string()],
            5432
        ));
        assert!(firewall_allows_iap(
            &rules,
            "default",
            &["web".to_string()],
            5432
        ));
        // An untagged rule applies to every instance in the network
        let untagged = vec![fw("a", "default", &["35.235.240.0/20"], &["5432"], &[])];
        assert!(firewall_allows_iap(
            &untagged,
            "default",
            &["db".to_string()],
            5432
        ));
    }

    #[test]
    fn firewall_accepts_the_all_protocol() {
        let mut all = fw("a", "default", &["35.235.240.0/20"], &[], &[]);
        all.allowed[0].ip_protocol = "all".to_string();
        assert!(firewall_allows_iap(&[all], "default", &[], 5432));
    }

    fn empty_url_map() -> UrlMap {
        UrlMap(serde_json::json!({
            "name": "gt-dev-urlmap",
            "defaultService": "https://example/backendServices/gt-dev-default",
            "fingerprint": "abc123"
        }))
    }

    #[test]
    fn with_host_rule_adds_a_host_and_path_matcher() {
        let map = with_host_rule(
            &empty_url_map(),
            "api.example.com",
            "https://example/bs/gt-dev-api",
        );
        let rules = map.0["hostRules"].as_array().expect("hostRules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["hosts"][0], "api.example.com");
        let matcher_name = rules[0]["pathMatcher"].as_str().expect("pathMatcher");
        let matchers = map.0["pathMatchers"].as_array().expect("pathMatchers");
        assert_eq!(matchers.len(), 1);
        assert_eq!(matchers[0]["name"], matcher_name);
        assert_eq!(
            matchers[0]["defaultService"],
            "https://example/bs/gt-dev-api"
        );
    }

    #[test]
    fn with_host_rule_is_idempotent_and_repoints_the_backend() {
        let one = with_host_rule(
            &empty_url_map(),
            "api.example.com",
            "https://example/bs/old",
        );
        let two = with_host_rule(&one, "api.example.com", "https://example/bs/new");
        assert_eq!(two.0["hostRules"].as_array().expect("hostRules").len(), 1);
        let matchers = two.0["pathMatchers"].as_array().expect("pathMatchers");
        assert_eq!(matchers.len(), 1);
        assert_eq!(matchers[0]["defaultService"], "https://example/bs/new");
    }

    #[test]
    fn without_host_rule_removes_only_that_host() {
        let map = with_host_rule(
            &empty_url_map(),
            "api.example.com",
            "https://example/bs/api",
        );
        let map = with_host_rule(&map, "web.example.com", "https://example/bs/web");
        let pruned = without_host_rule(&map, "api.example.com");

        let rules = pruned.0["hostRules"].as_array().expect("hostRules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["hosts"][0], "web.example.com");
        let matchers = pruned.0["pathMatchers"].as_array().expect("pathMatchers");
        assert_eq!(matchers.len(), 1);
        assert_eq!(matchers[0]["defaultService"], "https://example/bs/web");
    }

    #[test]
    fn without_host_rule_on_an_absent_host_changes_nothing() {
        let map = with_host_rule(
            &empty_url_map(),
            "api.example.com",
            "https://example/bs/api",
        );
        let pruned = without_host_rule(&map, "nope.example.com");
        assert_eq!(pruned.0["hostRules"], map.0["hostRules"]);
    }

    #[test]
    fn merge_named_port_replaces_by_name() {
        let ports = vec![
            NamedPort {
                name: "gt-default".to_string(),
                port: 30999,
            },
            NamedPort {
                name: "gt-api".to_string(),
                port: 30100,
            },
        ];
        let merged = merge_named_port(&ports, "gt-api", 30123);
        assert_eq!(merged.len(), 2);
        let api = merged.iter().find(|p| p.name == "gt-api").expect("gt-api");
        assert_eq!(api.port, 30123);
    }

    #[test]
    fn merge_named_port_appends_a_new_name() {
        let ports = vec![NamedPort {
            name: "gt-default".to_string(),
            port: 30999,
        }];
        let merged = merge_named_port(&ports, "gt-api", 30123);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn remove_named_port_drops_only_that_name() {
        let ports = vec![
            NamedPort {
                name: "gt-default".to_string(),
                port: 30999,
            },
            NamedPort {
                name: "gt-api".to_string(),
                port: 30123,
            },
        ];
        let pruned = remove_named_port(&ports, "gt-api");
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].name, "gt-default");
    }

    fn client(server: &MockServer) -> crate::gcp::GcpClient {
        crate::gcp::GcpClient::new(
            crate::gcp::TokenSource::Static("tok".to_string()),
            crate::gcp::Endpoints::single(&server.uri()),
        )
    }

    #[tokio::test]
    async fn list_instances_flattens_the_aggregated_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/aggregated/instances"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "items": {
                "zones/europe-west1-b": {
                  "instances": [
                    {"name":"bastion","status":"RUNNING","tags":{"items":["db"]},
                     "networkInterfaces":[{"network":"https://x/networks/default","networkIP":"10.0.0.5"}],
                     "zone":"https://x/projects/p/zones/europe-west1-b"},
                    {"name":"other-instance","status":"TERMINATED","tags":{},
                     "networkInterfaces":[{"network":"https://x/networks/default","networkIP":"10.0.0.6"}],
                     "zone":"https://x/projects/p/zones/europe-west1-b"}
                  ]
                },
                "zones/us-central1-a": {"warning": {"code": "NO_RESULTS_ON_PAGE"}}
              }
            })))
            .mount(&server)
            .await;

        let got = list_instances(&client(&server), "p").await.expect("list");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "bastion");
        assert_eq!(got[0].zone, "europe-west1-b");
        assert_eq!(got[0].status, "RUNNING");
        assert_eq!(got[0].internal_ip.as_deref(), Some("10.0.0.5"));
        assert_eq!(got[0].tags, vec!["db".to_string()]);
        assert_eq!(got[1].tags, Vec::<String>::new());
    }

    #[tokio::test]
    async fn list_instances_keeps_the_labels() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/aggregated/instances"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "items": {
                "zones/europe-west1-b": {
                  "instances": [
                    {"name":"gke-node-1","status":"RUNNING",
                     "labels":{"goog-gke-node":"","env":"dev"},
                     "zone":"https://x/projects/p/zones/europe-west1-b"},
                    {"name":"bastion","status":"RUNNING",
                     "zone":"https://x/projects/p/zones/europe-west1-b"}
                  ]
                }
              }
            })))
            .mount(&server)
            .await;

        let got = list_instances(&client(&server), "p").await.expect("list");
        let bastion = &got[0];
        let node = &got[1];
        assert_eq!(bastion.name, "bastion");
        assert!(bastion.labels.is_empty());
        assert_eq!(
            node.labels.get("goog-gke-node").map(String::as_str),
            Some("")
        );
        assert_eq!(node.labels.get("env").map(String::as_str), Some("dev"));
    }

    #[test]
    fn firewall_admits_iap_on_any_tcp_port() {
        let rules = vec![fw("a", "default", &["35.235.240.0/20"], &["5432"], &[])];
        assert!(firewall_admits_iap(&rules, "default", &[]));
        let no_ports = vec![fw("a", "default", &["35.235.240.0/20"], &[], &[])];
        assert!(firewall_admits_iap(&no_ports, "default", &[]));
    }

    #[test]
    fn firewall_admits_iap_rejects_what_allows_iap_rejects() {
        let rule = || fw("a", "default", &["35.235.240.0/20"], &["22"], &["web"]);
        let web = ["web".to_string()];
        assert!(firewall_admits_iap(&[rule()], "default", &web));
        assert!(!firewall_admits_iap(&[rule()], "other", &web));
        assert!(!firewall_admits_iap(
            &[rule()],
            "default",
            &["db".to_string()]
        ));
        let mut disabled = rule();
        disabled.disabled = true;
        assert!(!firewall_admits_iap(&[disabled], "default", &web));
        let mut egress = rule();
        egress.direction = "EGRESS".to_string();
        assert!(!firewall_admits_iap(&[egress], "default", &web));
        let mut udp = rule();
        udp.allowed[0].ip_protocol = "udp".to_string();
        assert!(!firewall_admits_iap(&[udp], "default", &web));
        let elsewhere = fw("a", "default", &["192.168.0.0/16"], &["22"], &[]);
        assert!(!firewall_admits_iap(&[elsewhere], "default", &[]));
        assert!(!firewall_admits_iap(&[], "default", &[]));
    }

    // The aggregated list walks one bucket per zone, so without a sort an
    // instance's position in the picker depends on which zone it lives in
    #[tokio::test]
    async fn list_instances_sorts_by_name_across_zones() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/aggregated/instances"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "items": {
                "zones/us-central1-a": {
                  "instances": [
                    {"name":"zeta","status":"RUNNING","tags":{},
                     "networkInterfaces":[],
                     "zone":"https://x/projects/p/zones/us-central1-a"},
                    {"name":"alpha","status":"RUNNING","tags":{},
                     "networkInterfaces":[],
                     "zone":"https://x/projects/p/zones/us-central1-a"}
                  ]
                },
                "zones/europe-west1-b": {
                  "instances": [
                    {"name":"middle","status":"RUNNING","tags":{},
                     "networkInterfaces":[],
                     "zone":"https://x/projects/p/zones/europe-west1-b"}
                  ]
                }
              }
            })))
            .mount(&server)
            .await;

        let got = list_instances(&client(&server), "p").await.expect("list");
        let names: Vec<&str> = got.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zeta"]);
    }

    #[tokio::test]
    async fn find_instances_asks_for_the_name_and_returns_each_zone() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/aggregated/instances"))
            .and(query_param("filter", "name=\"bastion\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
              "items": {
                "zones/us-central1-a": {"instances": [{"name":"bastion","zone":"https://x/projects/p/zones/us-central1-a"}]},
                "zones/europe-west1-b": {"instances": [{"name":"bastion","zone":"https://x/projects/p/zones/europe-west1-b"}]},
                "zones/asia-east1-a": {"warning": {"code": "NO_RESULTS_ON_PAGE"}}
              }
            })))
            .mount(&server)
            .await;

        let got = find_instances(&client(&server), "p", "bastion")
            .await
            .expect("find");
        let zones: Vec<&str> = got.iter().map(|i| i.zone.as_str()).collect();
        assert_eq!(zones, vec!["europe-west1-b", "us-central1-a"]);
    }

    #[test]
    fn a_filter_is_encoded_so_its_quotes_are_not_query_syntax() {
        assert_eq!(query_encode("name=\"a-b\""), "name%3D%22a-b%22");
    }

    #[tokio::test]
    async fn list_zones_returns_short_names() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/zones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"name": "europe-west1-b"}, {"name": "us-central1-a"}]
            })))
            .mount(&server)
            .await;
        let zones = list_zones(&client(&server), "p").await.expect("zones");
        assert_eq!(zones, vec!["europe-west1-b", "us-central1-a"]);
    }

    #[tokio::test]
    async fn get_instance_maps_404_to_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let got = get_instance(&client(&server), "p", "europe-west1-b", "nope")
            .await
            .expect("call");
        assert!(got.is_none());
    }

    fn done_op() -> serde_json::Value {
        serde_json::json!({"name": "op-1", "status": "DONE"})
    }

    async fn sent(server: &MockServer, verb: &str) -> Vec<wiremock::Request> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.method.as_str().eq_ignore_ascii_case(verb))
            .collect()
    }

    fn health_check_body(interval: u32, healthy: u32) -> serde_json::Value {
        serde_json::json!({
            "name": "gt-dev-hc",
            "type": "TCP",
            "tcpHealthCheck": {"portSpecification": "USE_SERVING_PORT"},
            "checkIntervalSec": interval,
            "timeoutSec": 5,
            "healthyThreshold": healthy,
            "unhealthyThreshold": 3
        })
    }

    // A health check made by an older gtunnel kept its 10 s interval for ever
    // because relay up only checked that the name existed
    #[tokio::test]
    async fn a_health_check_with_old_values_gets_one_patch_with_the_new_ones() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(health_check_body(10, 2)))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;

        let got = reconcile_health_check(&client(&server), "p", "gt-dev-hc")
            .await
            .expect("reconcile");

        assert_eq!(got, Reconciled::Updated);
        let patches = sent(&server, "PATCH").await;
        assert_eq!(patches.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&patches[0].body).expect("json");
        assert_eq!(body["checkIntervalSec"], 5);
        assert_eq!(body["timeoutSec"], 5);
        assert_eq!(body["healthyThreshold"], 1);
        assert_eq!(body["unhealthyThreshold"], 3);
        assert!(sent(&server, "POST").await.is_empty());
    }

    #[tokio::test]
    async fn a_health_check_that_matches_sends_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(health_check_body(5, 1)))
            .mount(&server)
            .await;

        let got = reconcile_health_check(&client(&server), "p", "gt-dev-hc")
            .await
            .expect("reconcile");

        assert_eq!(got, Reconciled::Unchanged);
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            1
        );
    }

    #[tokio::test]
    async fn a_missing_health_check_is_created_with_the_new_values() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;

        let got = reconcile_health_check(&client(&server), "p", "gt-dev-hc")
            .await
            .expect("reconcile");

        assert_eq!(got, Reconciled::Created);
        let posts = sent(&server, "POST").await;
        let body: serde_json::Value = serde_json::from_slice(&posts[0].body).expect("json");
        assert_eq!(body["checkIntervalSec"], 5);
        assert_eq!(body["healthyThreshold"], 1);
    }

    fn backend(timeout: Option<u32>, draining: Option<u32>, iap: bool) -> BackendService {
        BackendService {
            name: "gt-dev-api".to_string(),
            port_name: Some("gt-api".to_string()),
            iap_enabled: iap,
            description: None,
            timeout_sec: timeout,
            draining_timeout_sec: draining,
        }
    }

    #[tokio::test]
    async fn a_backend_service_patch_carries_only_the_fields_that_differ() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        let want = BackendSettings::new(false);

        let got = reconcile_backend_service(
            &client(&server),
            "p",
            "gt-dev-api",
            &want,
            &backend(Some(want.timeout_sec), None, false),
        )
        .await
        .expect("reconcile");

        assert_eq!(got, Reconciled::Updated);
        let patches = sent(&server, "PATCH").await;
        assert_eq!(patches.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&patches[0].body).expect("json");
        assert_eq!(
            body,
            serde_json::json!({"connectionDraining": {"drainingTimeoutSec": want.draining_sec}})
        );
    }

    #[tokio::test]
    async fn a_backend_service_that_matches_sends_nothing() {
        let server = MockServer::start().await;
        let want = BackendSettings::new(true);

        let got = reconcile_backend_service(
            &client(&server),
            "p",
            "gt-dev-api",
            &want,
            &backend(Some(want.timeout_sec), Some(want.draining_sec), true),
        )
        .await
        .expect("reconcile");

        assert_eq!(got, Reconciled::Unchanged);
        assert!(server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty());
    }

    #[test]
    fn the_backend_defaults_are_the_ones_the_issue_agreed() {
        let want = BackendSettings::new(false);
        assert_eq!((want.timeout_sec, want.draining_sec), (3600, 30));
    }

    fn instance(script: Option<&str>) -> Instance {
        let mut metadata = vec![("ssh-keys".to_string(), "kept".to_string())];
        if let Some(script) = script {
            metadata.push(("startup-script".to_string(), script.to_string()));
        }
        Instance {
            name: "gt-dev-relay".to_string(),
            zone: "europe-west1-b".to_string(),
            status: "RUNNING".to_string(),
            network: None,
            tags: vec![],
            internal_ip: None,
            labels: Default::default(),
            metadata,
            metadata_fingerprint: Some("fp-meta".to_string()),
        }
    }

    // The startup script is the only way to change sshd without recreating the
    // VM, so a relay whose script differs must be told the new one
    #[tokio::test]
    async fn a_relay_whose_startup_script_differs_gets_set_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;

        let got = reconcile_relay_instance(
            &client(&server),
            "p",
            "europe-west1-b",
            &instance(Some("#!/bin/bash\necho old\n")),
        )
        .await
        .expect("reconcile");

        assert_eq!(got, Reconciled::Updated);
        let posts = sent(&server, "POST").await;
        assert_eq!(posts.len(), 1);
        assert!(posts[0]
            .url
            .path()
            .ends_with("/instances/gt-dev-relay/setMetadata"));
        let body: serde_json::Value = serde_json::from_slice(&posts[0].body).expect("json");
        assert_eq!(body["fingerprint"], "fp-meta");
        let items = body["items"].as_array().expect("items");
        let script = items
            .iter()
            .find(|i| i["key"] == "startup-script")
            .expect("script");
        assert_eq!(script["value"], RELAY_STARTUP_SCRIPT);
        assert!(
            items
                .iter()
                .any(|i| i["key"] == "ssh-keys" && i["value"] == "kept"),
            "metadata gtunnel does not own must survive: {items:?}"
        );
    }

    #[tokio::test]
    async fn a_relay_with_the_current_startup_script_sends_nothing() {
        let server = MockServer::start().await;

        let got = reconcile_relay_instance(
            &client(&server),
            "p",
            "europe-west1-b",
            &instance(Some(RELAY_STARTUP_SCRIPT)),
        )
        .await
        .expect("reconcile");

        assert_eq!(got, Reconciled::Unchanged);
        assert!(server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty());
    }

    #[tokio::test]
    async fn a_relay_with_no_startup_script_at_all_is_updated_too() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;

        let got =
            reconcile_relay_instance(&client(&server), "p", "europe-west1-b", &instance(None))
                .await
                .expect("reconcile");

        assert_eq!(got, Reconciled::Updated);
    }

    #[tokio::test]
    async fn get_instance_reads_the_startup_script_and_the_metadata_fingerprint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-relay",
                "status": "RUNNING",
                "metadata": {
                    "fingerprint": "fp-9",
                    "items": [
                        {"key": "startup-script", "value": "#!/bin/bash\n"},
                        {"key": "other", "value": "x"}
                    ]
                }
            })))
            .mount(&server)
            .await;

        let got = get_instance(&client(&server), "p", "europe-west1-b", "gt-dev-relay")
            .await
            .expect("call")
            .expect("instance");

        assert_eq!(got.metadata_fingerprint.as_deref(), Some("fp-9"));
        assert_eq!(got.startup_script(), Some("#!/bin/bash\n"));
        assert_eq!(got.metadata.len(), 2);
    }
}
