use anyhow::{bail, Result};
use std::future::Future;

use crate::config::{Account, ExposeConfig};
use crate::gcp::{compute, dns, is_precondition_failed, GcpClient};
use crate::state::{PersistentTunnel, TunnelSpec};

use super::naming::Names;

// How many times a read-modify-write against a fingerprinted resource is retried
pub const FINGERPRINT_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteRemoval {
    pub dns: bool,
    pub host_rule: bool,
    pub backend: bool,
    pub named_port: bool,
}

// Url maps and instance groups are updated read-modify-write under a
// fingerprint; a concurrent change returns 412 and the read has to be redone
pub async fn retry_on_precondition<F, Fut, T>(attempts: u32, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut last: Option<anyhow::Error> = None;
    for _ in 0..attempts.max(1) {
        match f().await {
            Ok(value) => return Ok(value),
            Err(e) if is_precondition_failed(&e) => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    match last {
        Some(e) => {
            bail!("Gave up after {attempts} attempts: the resource fingerprint kept changing ({e})")
        }
        None => bail!("Gave up after {attempts} attempts"),
    }
}

// Where an Expose tunnel's route lives
pub struct RouteTarget {
    pub names: Names,
    pub backend: String,
    pub named_port: String,
    pub hostname: String,
    pub domain: String,
    pub relay_port: u16,
    pub private: bool,
}

pub fn route_target(acct: &Account, t: &PersistentTunnel) -> Result<RouteTarget> {
    let TunnelSpec::Expose {
        domain,
        hostname,
        relay_port,
        private,
        ..
    } = &t.spec
    else {
        bail!("Tunnel '{}' is not an Expose tunnel", t.name);
    };
    let names = Names::new(&acct.name);
    Ok(RouteTarget {
        backend: names.backend(&t.name),
        named_port: names.named_port(&t.name),
        hostname: hostname.clone(),
        domain: domain.clone(),
        relay_port: *relay_port,
        private: *private,
        names,
    })
}

// Removal runs in reverse and stops at the first real failure: a DNS record
// reported as deleted while it still resolves is worse than a partial teardown
pub async fn remove_route(
    client: &GcpClient,
    acct: &Account,
    ex: &ExposeConfig,
    t: &PersistentTunnel,
) -> Result<RouteRemoval> {
    let target = route_target(acct, t)?;
    let project = &acct.project;
    let mut removal = RouteRemoval::default();

    let domain = ex.domain(Some(&target.domain))?;
    let record = format!("{}.", target.hostname);
    let existing = dns::list_record_sets(client, project, &domain.managed_zone).await?;
    if existing.iter().any(|r| r.name == record && r.rtype == "A") {
        dns::delete_record_set(client, project, &domain.managed_zone, &record, "A").await?;
        removal.dns = true;
    }

    let names = &target.names;
    let host = target.hostname.clone();
    removal.host_rule = retry_on_precondition(FINGERPRINT_ATTEMPTS, || async {
        let Some(map) = compute::get_url_map(client, project, &names.url_map).await? else {
            return Ok(false);
        };
        let updated = compute::without_host_rule(&map, &host);
        if updated == map {
            return Ok(false);
        }
        let op = compute::update_url_map(client, project, &names.url_map, &updated).await?;
        client.wait_operation(&op).await?;
        Ok(true)
    })
    .await?;

    if let Some(op) = compute::delete_backend_service(client, project, &target.backend).await? {
        client.wait_operation(&op).await?;
        removal.backend = true;
    }

    let zone = &ex.relay_zone;
    let group = &names.instance_group;
    let port_name = target.named_port.clone();
    removal.named_port = retry_on_precondition(FINGERPRINT_ATTEMPTS, || async {
        let (ports, fingerprint) = compute::get_named_ports(client, project, zone, group).await?;
        if !ports.iter().any(|p| p.name == port_name) {
            return Ok(false);
        }
        let kept = compute::remove_named_port(&ports, &port_name);
        let op =
            compute::set_named_ports(client, project, zone, group, &kept, fingerprint.as_deref())
                .await?;
        client.wait_operation(&op).await?;
        Ok(true)
    })
    .await?;

    Ok(removal)
}
