use anyhow::{bail, Context, Result};
use std::future::Future;

use crate::config::{Account, ExposeConfig};
use crate::gcp::{compute, dns, is_precondition_failed, GcpClient};
use crate::state::{PersistentTunnel, TunnelSpec};

use super::naming::Names;

/// How many times a read-modify-write against a fingerprinted resource is
/// tried: `gcp.fingerprint_attempts`.
pub fn fingerprint_attempts() -> u32 {
    crate::settings::current().gcp_fingerprint_attempts
}

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
    removal.host_rule = retry_on_precondition(fingerprint_attempts(), || async {
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
    removal.named_port = retry_on_precondition(fingerprint_attempts(), || async {
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

// Everything one Expose tunnel needs on the relay: its named port, its backend
// service, its host rule and its DNS record
pub async fn add_route(
    client: &GcpClient,
    acct: &Account,
    ex: &ExposeConfig,
    t: &PersistentTunnel,
) -> Result<()> {
    let target = route_target(acct, t)?;
    let project = &acct.project;
    let names = &target.names;
    let zone = &ex.relay_zone;
    let group = &names.instance_group;
    let port_name = target.named_port.clone();
    let relay_port = target.relay_port;

    retry_on_precondition(fingerprint_attempts(), || async {
        let (ports, fingerprint) = compute::get_named_ports(client, project, zone, group).await?;
        if ports
            .iter()
            .any(|p| p.name == port_name && p.port == relay_port)
        {
            return Ok(());
        }
        let merged = compute::merge_named_port(&ports, &port_name, relay_port);
        let op = compute::set_named_ports(
            client,
            project,
            zone,
            group,
            &merged,
            fingerprint.as_deref(),
        )
        .await?;
        client.wait_operation(&op).await
    })
    .await?;

    let group_url = compute::get_instance_group(client, project, zone, group)
        .await?
        .with_context(|| format!("Instance group '{group}' is missing. Run `gtunnel relay up`"))?;
    match compute::get_backend_service(client, project, &target.backend).await? {
        None => {
            let op = compute::insert_backend_service(
                client,
                project,
                &target.backend,
                &target.named_port,
                &group_url,
                &format!(
                    "projects/{project}/global/healthChecks/{}",
                    names.health_check
                ),
                &compute::BackendSettings::configured(target.private),
                &names.owner(),
            )
            .await?;
            client.wait_operation(&op).await?;
        }
        // A backend made by an older gtunnel keeps its old settings until
        // something converges it, and its access rule is the one that matters
        Some(existing) => {
            compute::reconcile_backend_service(
                client,
                project,
                &target.backend,
                &compute::BackendSettings::configured(target.private),
                &existing,
            )
            .await?;
        }
    }

    let backend_url = format!(
        "projects/{project}/global/backendServices/{}",
        target.backend
    );
    let host = target.hostname.clone();
    retry_on_precondition(fingerprint_attempts(), || async {
        let map = compute::get_url_map(client, project, &names.url_map)
            .await?
            .with_context(|| {
                format!(
                    "Url map '{}' is missing. Run `gtunnel relay up`",
                    names.url_map
                )
            })?;
        let updated = compute::with_host_rule(&map, &host, &backend_url);
        if updated == map {
            return Ok(());
        }
        let op = compute::update_url_map(client, project, &names.url_map, &updated).await?;
        client.wait_operation(&op).await
    })
    .await?;

    let domain = ex.domain(Some(&target.domain))?;
    let record = format!("{}.", target.hostname);
    let existing = dns::list_record_sets(client, project, &domain.managed_zone).await?;
    if !existing.iter().any(|r| r.name == record && r.rtype == "A") {
        let ip = compute::get_global_address(client, project, &names.address)
            .await?
            .with_context(|| {
                format!(
                    "Address '{}' is missing. Run `gtunnel relay up`",
                    names.address
                )
            })?;
        dns::create_record_set(
            client,
            project,
            &domain.managed_zone,
            &dns::RecordSet {
                name: record,
                rtype: "A".to_string(),
                ttl: 60,
                rrdatas: vec![ip],
            },
        )
        .await?;
    }

    Ok(())
}

// start, restart and edit call this so a route deleted outside gtunnel comes
// back. An Access tunnel has no route, so it reaches GCP at all only for a
// public one.
pub async fn ensure_route(client: &GcpClient, acct: &Account, t: &PersistentTunnel) -> Result<()> {
    if !t.is_expose() {
        return Ok(());
    }
    let ex = acct
        .expose
        .clone()
        .context("This account has no relay, but the tunnel is an Expose tunnel")?;
    add_route(client, acct, &ex, t).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Account, DomainConfig};
    use crate::gcp::{Endpoints, GcpClient, TokenSource};
    use crate::state::{PersistentTunnel, TunnelSpec};
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> GcpClient {
        GcpClient::new(
            TokenSource::Static("tok".to_string()),
            Endpoints::single(&server.uri()),
        )
    }

    fn account() -> Account {
        Account {
            name: "dev".to_string(),
            gcloud_configuration: "dev".to_string(),
            project: "p".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: None,
            expose: Some(expose()),
        }
    }

    fn expose() -> ExposeConfig {
        ExposeConfig {
            relay_instance: "gt-dev-relay".to_string(),
            relay_zone: "europe-west1-b".to_string(),
            network: "default".to_string(),
            default_domain: "dev.example.com".to_string(),
            domains: vec![DomainConfig {
                domain: "dev.example.com".to_string(),
                managed_zone: "example".to_string(),
            }],
        }
    }

    fn tunnel() -> PersistentTunnel {
        PersistentTunnel {
            name: "api".to_string(),
            account_name: "dev".to_string(),
            auto_start: true,
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "api.dev.example.com".to_string(),
                relay_port: 30001,
                private: false,
            },
        }
    }

    fn url_map_body() -> serde_json::Value {
        serde_json::json!({
            "name": "gt-dev-urlmap",
            "fingerprint": "fp-1",
            "defaultService": "projects/p/global/backendServices/gt-dev-default",
            "hostRules": [],
            "pathMatchers": []
        })
    }

    fn done_op() -> serde_json::Value {
        serde_json::json!({"name": "op-1", "status": "DONE"})
    }

    fn precondition_failed() -> ResponseTemplate {
        ResponseTemplate::new(412).set_body_json(serde_json::json!({
            "error": {"code": 412, "message": "fingerprint mismatch"}
        }))
    }

    async fn count(server: &MockServer, verb: &str, needle: &str) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str().eq_ignore_ascii_case(verb))
            .filter(|r| r.url.path().contains(needle))
            .count()
    }

    // Common mocks: the instance group, the backend service and DNS all behave
    async fn base_mocks(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path_regex(r".*/urlMaps/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(url_map_body()))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/instanceGroups/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-ig",
                "fingerprint": "fp-ig",
                "namedPorts": [{"name": "gt-default", "port": 30999}]
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/backendServices/.*"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": 404, "message": "not found"}
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/addresses/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-ip",
                "address": "34.1.2.3"
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/rrsets.*"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"rrsets": []})),
            )
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(server)
            .await;
    }

    // The comment on ensure_route promised start and restart repaired a route
    // deleted outside gtunnel, but nothing on either path called it
    #[tokio::test]
    async fn ensure_route_recreates_a_dns_record_deleted_outside_gtunnel() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        base_mocks(&server).await;

        ensure_route(&client(&server), &account(), &tunnel())
            .await
            .expect("ensure route");

        assert_eq!(count(&server, "POST", "rrsets").await, 1);
    }

    // An Access tunnel has no relay route, so starting one must not call GCP
    #[tokio::test]
    async fn ensure_route_leaves_an_access_tunnel_alone() {
        let server = MockServer::start().await;
        let mut t = tunnel();
        t.spec = TunnelSpec::Access {
            target: crate::state::AccessTarget::Instance {
                instance: "bastion".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 5432,
            local_port: 15432,
        };

        ensure_route(&client(&server), &account(), &t)
            .await
            .expect("ensure route");

        assert!(server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty());
    }

    // Review Focus 4: two gtunnel runs editing the url map at once get a 412,
    // and the loser has to re-read the fingerprint instead of failing
    #[tokio::test]
    async fn add_route_retries_the_url_map_after_a_fingerprint_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(precondition_failed())
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        base_mocks(&server).await;

        add_route(&client(&server), &account(), &expose(), &tunnel())
            .await
            .expect("add route");

        assert_eq!(count(&server, "GET", "/urlMaps/").await, 2);
        assert_eq!(count(&server, "PUT", "/urlMaps/").await, 2);
    }

    #[tokio::test]
    async fn add_route_gives_up_after_five_fingerprint_mismatches() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(precondition_failed())
            .mount(&server)
            .await;
        base_mocks(&server).await;

        let err = add_route(&client(&server), &account(), &expose(), &tunnel())
            .await
            .expect_err("should give up");
        assert!(err.to_string().contains("fingerprint"), "{err}");
        assert_eq!(count(&server, "PUT", "/urlMaps/").await, 5);
    }

    #[tokio::test]
    async fn add_route_creates_the_dns_record_and_the_backend() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        base_mocks(&server).await;

        add_route(&client(&server), &account(), &expose(), &tunnel())
            .await
            .expect("add route");

        assert_eq!(count(&server, "POST", "/rrsets").await, 1);
        assert_eq!(count(&server, "POST", "/backendServices").await, 1);
        assert_eq!(count(&server, "POST", "/setNamedPorts").await, 1);
    }

    // ytunnel v1.0 lesson: a DNS delete that failed must never be reported as
    // done, and the resources behind it must stay put
    #[tokio::test]
    async fn remove_route_stops_when_the_dns_delete_fails() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/rrsets.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "rrsets": [{
                    "name": "api.dev.example.com.",
                    "type": "A",
                    "ttl": 60,
                    "rrdatas": ["34.1.2.3"]
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": {"code": 500, "message": "backend error"}
            })))
            .mount(&server)
            .await;
        base_mocks(&server).await;

        let err = remove_route(&client(&server), &account(), &expose(), &tunnel())
            .await
            .expect_err("should fail");
        let message = format!("{err:#}");
        assert!(
            message.contains("500") || message.contains("backend error"),
            "{message}"
        );
        assert_eq!(count(&server, "DELETE", "/backendServices").await, 0);
    }
    // A tunnel re-added with --private must not keep serving publicly because
    // its backend service already existed without IAP
    #[tokio::test]
    async fn add_route_turns_iap_on_for_a_backend_that_predates_private() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/backendServices/gt-dev-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-api",
                "portName": "gt-api",
                "iap": {"enabled": false}
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        base_mocks(&server).await;

        let mut private = tunnel();
        if let TunnelSpec::Expose { private: p, .. } = &mut private.spec {
            *p = true;
        }
        add_route(&client(&server), &account(), &expose(), &private)
            .await
            .expect("add route");

        assert_eq!(
            count(&server, "PATCH", "/backendServices/gt-dev-api").await,
            1
        );
        assert_eq!(count(&server, "POST", "/backendServices").await, 0);
    }

    #[tokio::test]
    async fn add_route_leaves_a_backend_whose_iap_already_matches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/backendServices/gt-dev-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-api",
                "portName": "gt-api",
                "iap": {"enabled": false},
                "timeoutSec": 3600,
                "connectionDraining": {"drainingTimeoutSec": 30}
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        base_mocks(&server).await;

        add_route(&client(&server), &account(), &expose(), &tunnel())
            .await
            .expect("add route");

        assert_eq!(count(&server, "PATCH", "/backendServices").await, 0);
    }

    // A backend made before the load balancer settings were owned has no
    // timeoutSec; ensure_route has to give it one, once
    #[tokio::test]
    async fn ensure_route_patches_a_backend_without_timeout_and_then_leaves_it_alone() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/backendServices/gt-dev-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-api",
                "portName": "gt-api",
                "iap": {"enabled": false}
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/backendServices/gt-dev-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-api",
                "portName": "gt-api",
                "iap": {"enabled": false},
                "timeoutSec": 3600,
                "connectionDraining": {"drainingTimeoutSec": 30}
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200).set_body_json(done_op()))
            .mount(&server)
            .await;
        base_mocks(&server).await;

        ensure_route(&client(&server), &account(), &tunnel())
            .await
            .expect("first call");
        assert_eq!(count(&server, "PATCH", "/backendServices/").await, 1);
        let patch = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|r| r.method.as_str() == "PATCH")
            .expect("patch");
        let body: serde_json::Value = serde_json::from_slice(&patch.body).expect("json");
        assert_eq!(body["timeoutSec"], 3600);

        ensure_route(&client(&server), &account(), &tunnel())
            .await
            .expect("second call");
        assert_eq!(
            count(&server, "PATCH", "/backendServices/").await,
            1,
            "a converged backend must not be patched again"
        );
    }
}
