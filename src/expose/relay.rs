use anyhow::{bail, Context, Result};

use crate::config::{Account, Config, DomainConfig, ExposeConfig};
use crate::daemon;
use crate::gcp::compute::{Firewall, FirewallAllow, NamedPort, IAP_RANGE, LB_HEALTH_CHECK_RANGES};
use crate::gcp::{certmanager, compute, dns, is_not_found, GcpClient};
use crate::state::TunnelState;

use super::naming::Names;
use super::routes;

// The relay VM carries this tag, and both firewall rules target it
pub const RELAY_TAG: &str = "gt-relay";
// The default backend's port has no listener, so an unknown host gets a 503
pub const DEFAULT_RELAY_PORT: u16 = 30_999;
pub const RELAY_PORT_RANGE: &str = "30000-30999";

pub struct RelayUpOptions {
    pub zone: String,
    pub network: String,
    pub managed_zone: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayStatus {
    pub vm: Option<String>,
    pub ip: Option<String>,
    pub certs: Vec<(String, String)>,
    pub forwarding_rule: bool,
}

// One line per step, so `relay up` shows what it created and what it found
fn step(created: bool, what: &str) {
    if created {
        println!("  ✓ {what}");
    } else {
        println!("  · {what} (exists)");
    }
}

fn iap_ssh_firewall(names: &Names, network: &str) -> Firewall {
    Firewall {
        name: names.fw_iap_ssh.clone(),
        network: format!("global/networks/{network}"),
        direction: "INGRESS".to_string(),
        source_ranges: vec![IAP_RANGE.to_string()],
        allowed: vec![FirewallAllow {
            ip_protocol: "tcp".to_string(),
            ports: vec!["22".to_string()],
        }],
        target_tags: vec![RELAY_TAG.to_string()],
        disabled: false,
    }
}

fn lb_health_check_firewall(names: &Names, network: &str) -> Firewall {
    Firewall {
        name: names.fw_lb_hc.clone(),
        network: format!("global/networks/{network}"),
        direction: "INGRESS".to_string(),
        source_ranges: LB_HEALTH_CHECK_RANGES
            .iter()
            .map(|r| r.to_string())
            .collect(),
        allowed: vec![FirewallAllow {
            ip_protocol: "tcp".to_string(),
            ports: vec![RELAY_PORT_RANGE.to_string()],
        }],
        target_tags: vec![RELAY_TAG.to_string()],
        disabled: false,
    }
}

async fn ensure_firewall(client: &GcpClient, project: &str, fw: &Firewall) -> Result<()> {
    if compute::get_firewall(client, project, &fw.name)
        .await?
        .is_some()
    {
        step(false, &format!("firewall {}", fw.name));
        return Ok(());
    }
    let op = compute::insert_firewall(client, project, fw).await?;
    client.wait_operation(&op).await?;
    step(true, &format!("firewall {}", fw.name));
    Ok(())
}

// The domain a managed zone serves, without the trailing dot Cloud DNS uses
async fn domain_of_zone(client: &GcpClient, project: &str, managed_zone: &str) -> Result<String> {
    let zones = dns::list_managed_zones(client, project).await?;
    let zone = zones
        .iter()
        .find(|z| z.name == managed_zone)
        .with_context(|| format!("Managed zone '{managed_zone}' not found in project {project}"))?;
    Ok(zone.dns_name.trim_end_matches('.').to_string())
}

// The DNS authorization's CNAME has to resolve before the certificate can be
// issued, so relay up writes it into the managed zone itself
async fn ensure_domain(
    client: &GcpClient,
    project: &str,
    names: &Names,
    domain: &str,
    managed_zone: &str,
) -> Result<DomainConfig> {
    let auth_name = names.dns_auth(domain);
    let auth = certmanager::ensure_dns_authorization(client, project, &auth_name, domain).await?;
    step(true, &format!("dns authorization {auth_name}"));

    let records = dns::list_record_sets(client, project, managed_zone).await?;
    let present = records
        .iter()
        .any(|r| r.name == auth.cname_name && r.rtype == "CNAME");
    if !present {
        dns::create_record_set(
            client,
            project,
            managed_zone,
            &dns::RecordSet {
                name: auth.cname_name.clone(),
                rtype: "CNAME".to_string(),
                ttl: 300,
                rrdatas: vec![auth.cname_data.clone()],
            },
        )
        .await?;
    }
    step(!present, &format!("validation CNAME {}", auth.cname_name));

    let cert = names.certificate(domain);
    certmanager::ensure_certificate(
        client,
        project,
        &cert,
        &[format!("*.{domain}"), domain.to_string()],
        std::slice::from_ref(&auth.name),
    )
    .await?;
    step(true, &format!("certificate {cert}"));

    for wildcard in [true, false] {
        let entry = names.cert_map_entry(domain, wildcard);
        let hostname = if wildcard {
            format!("*.{domain}")
        } else {
            domain.to_string()
        };
        certmanager::ensure_certificate_map_entry(
            client,
            project,
            &names.cert_map,
            &entry,
            &hostname,
            &cert,
        )
        .await?;
        step(true, &format!("certificate map entry {entry}"));
    }

    Ok(DomainConfig {
        domain: domain.to_string(),
        managed_zone: managed_zone.to_string(),
    })
}

pub async fn relay_up(
    client: &GcpClient,
    acct: &Account,
    opts: &RelayUpOptions,
) -> Result<ExposeConfig> {
    let names = Names::new(&acct.name);
    let project = &acct.project;
    let zone = &opts.zone;
    let network = &opts.network;

    let domain = domain_of_zone(client, project, &opts.managed_zone).await?;

    ensure_firewall(client, project, &iap_ssh_firewall(&names, network)).await?;
    ensure_firewall(client, project, &lb_health_check_firewall(&names, network)).await?;

    let existing_vm = compute::get_instance(client, project, zone, &names.relay_vm).await?;
    if existing_vm.is_none() {
        let op = compute::insert_relay_instance(
            client,
            project,
            zone,
            &names.relay_vm,
            network,
            RELAY_TAG,
        )
        .await?;
        client.wait_operation(&op).await?;
    }
    step(
        existing_vm.is_none(),
        &format!("relay vm {}", names.relay_vm),
    );

    let group_url =
        compute::get_instance_group(client, project, zone, &names.instance_group).await?;
    let group_url = match group_url {
        Some(url) => {
            step(false, &format!("instance group {}", names.instance_group));
            url
        }
        None => {
            let op = compute::insert_instance_group(
                client,
                project,
                zone,
                &names.instance_group,
                network,
            )
            .await?;
            client.wait_operation(&op).await?;
            let op = compute::add_instance_to_group(
                client,
                project,
                zone,
                &names.instance_group,
                &names.relay_vm,
            )
            .await?;
            client.wait_operation(&op).await?;
            step(true, &format!("instance group {}", names.instance_group));
            compute::get_instance_group(client, project, zone, &names.instance_group)
                .await?
                .context("Instance group disappeared right after it was created")?
        }
    };

    let default_port = names.default_named_port();
    let (ports, fingerprint) =
        compute::get_named_ports(client, project, zone, &names.instance_group).await?;
    let has_default = ports
        .iter()
        .any(|p| p.name == default_port && p.port == DEFAULT_RELAY_PORT);
    if !has_default {
        let merged: Vec<NamedPort> =
            compute::merge_named_port(&ports, &default_port, DEFAULT_RELAY_PORT);
        let op = compute::set_named_ports(
            client,
            project,
            zone,
            &names.instance_group,
            &merged,
            fingerprint.as_deref(),
        )
        .await?;
        client.wait_operation(&op).await?;
    }
    step(!has_default, &format!("named port {default_port}"));

    compute::ensure_health_check(client, project, &names.health_check).await?;
    step(true, &format!("health check {}", names.health_check));

    let existing_backend =
        compute::get_backend_service(client, project, &names.default_backend).await?;
    if existing_backend.is_none() {
        let op = compute::insert_backend_service(
            client,
            project,
            &names.default_backend,
            &default_port,
            &group_url,
            &format!(
                "projects/{project}/global/healthChecks/{}",
                names.health_check
            ),
            false,
            &names.owner(),
        )
        .await?;
        client.wait_operation(&op).await?;
    }
    step(
        existing_backend.is_none(),
        &format!("default backend {}", names.default_backend),
    );

    let had_address = compute::get_global_address(client, project, &names.address)
        .await?
        .is_some();
    let ip = compute::reserve_global_address(client, project, &names.address).await?;
    step(!had_address, &format!("address {} ({ip})", names.address));

    certmanager::ensure_certificate_map(client, project, &names.cert_map).await?;
    step(true, &format!("certificate map {}", names.cert_map));

    let domain_config = ensure_domain(client, project, &names, &domain, &opts.managed_zone).await?;

    let existing_map = compute::get_url_map(client, project, &names.url_map).await?;
    if existing_map.is_none() {
        let op = compute::insert_url_map(
            client,
            project,
            &names.url_map,
            &format!(
                "projects/{project}/global/backendServices/{}",
                names.default_backend
            ),
        )
        .await?;
        client.wait_operation(&op).await?;
    }
    step(
        existing_map.is_none(),
        &format!("url map {}", names.url_map),
    );

    let had_proxy = compute::get_target_https_proxy(client, project, &names.https_proxy).await?;
    if !had_proxy {
        let op = compute::insert_target_https_proxy(
            client,
            project,
            &names.https_proxy,
            &format!("projects/{project}/global/urlMaps/{}", names.url_map),
            &format!(
                "projects/{project}/locations/global/certificateMaps/{}",
                names.cert_map
            ),
        )
        .await?;
        client.wait_operation(&op).await?;
    }
    step(!had_proxy, &format!("https proxy {}", names.https_proxy));

    let had_rule =
        compute::get_global_forwarding_rule(client, project, &names.forwarding_rule).await?;
    if !had_rule {
        let op = compute::insert_global_forwarding_rule(
            client,
            project,
            &names.forwarding_rule,
            &ip,
            &format!(
                "projects/{project}/global/targetHttpsProxies/{}",
                names.https_proxy
            ),
        )
        .await?;
        client.wait_operation(&op).await?;
    }
    step(
        !had_rule,
        &format!("forwarding rule {}", names.forwarding_rule),
    );

    Ok(ExposeConfig {
        relay_instance: names.relay_vm.clone(),
        relay_zone: zone.clone(),
        network: network.clone(),
        default_domain: domain,
        domains: vec![domain_config],
    })
}

// Register a second domain against an existing relay
pub async fn add_domain(
    client: &GcpClient,
    acct: &Account,
    managed_zone: &str,
) -> Result<DomainConfig> {
    let names = Names::new(&acct.name);
    let project = &acct.project;
    let domain = domain_of_zone(client, project, managed_zone).await?;
    certmanager::ensure_certificate_map(client, project, &names.cert_map).await?;
    ensure_domain(client, project, &names, &domain, managed_zone).await
}

pub async fn relay_status(client: &GcpClient, acct: &Account) -> Result<RelayStatus> {
    let names = Names::new(&acct.name);
    let project = &acct.project;
    let Some(expose) = acct.expose.as_ref() else {
        return Ok(RelayStatus::default());
    };

    let vm = compute::get_instance(client, project, &expose.relay_zone, &names.relay_vm)
        .await?
        .map(|i| i.status);
    let ip = compute::get_global_address(client, project, &names.address).await?;
    let forwarding_rule =
        compute::get_global_forwarding_rule(client, project, &names.forwarding_rule).await?;

    let mut certs = Vec::new();
    for domain in &expose.domains {
        let state =
            certmanager::certificate_state(client, project, &names.certificate(&domain.domain))
                .await?;
        certs.push((domain.domain.clone(), state));
    }

    Ok(RelayStatus {
        vm,
        ip,
        certs,
        forwarding_rule,
    })
}

// Deleting an operation-backed resource that is already gone is the desired
// end state; any other failure stops the teardown
async fn drop_resource(
    client: &GcpClient,
    what: &str,
    op: Result<Option<crate::gcp::Operation>>,
) -> Result<()> {
    match op {
        Ok(Some(op)) => {
            client.wait_operation(&op).await?;
            step(true, &format!("deleted {what}"));
            Ok(())
        }
        Ok(None) => {
            step(false, &format!("{what} already gone"));
            Ok(())
        }
        Err(e) if is_not_found(&e) => {
            step(false, &format!("{what} already gone"));
            Ok(())
        }
        Err(e) => Err(e),
    }
}

// Public runs that are still alive for this account. A run owns its route
// until the process exits, so `relay down` has to count it as a tenant.
pub fn live_public_runs(
    records: &[(crate::ephemeral::EphemeralRecord, bool)],
    account: &str,
) -> Vec<String> {
    records
        .iter()
        .filter(|(r, alive)| {
            *alive
                && r.account_name == account
                && matches!(r.spec, crate::state::TunnelSpec::Expose { .. })
        })
        .map(|(r, _)| r.name.clone())
        .collect()
}

pub async fn relay_down(
    client: &GcpClient,
    cfg: &Config,
    acct: &Account,
    state: &mut TunnelState,
    force: bool,
) -> Result<()> {
    let names = Names::new(&acct.name);
    let project = &acct.project;
    let expose = acct
        .expose
        .clone()
        .context("This account has no relay. Run `gtunnel relay up` first")?;

    let live: Vec<String> = state
        .tunnels_for_account(&acct.name)
        .into_iter()
        .filter(|t| t.is_expose())
        .map(|t| t.name.clone())
        .collect();
    // A `gtunnel run --public` holds a route too, and it is not gtunnel's to
    // delete: that process owns it until it exits
    let runs = live_public_runs(&crate::ephemeral::list_ephemeral()?, &acct.name);
    if (!live.is_empty() || !runs.is_empty()) && !force {
        let mut serving = live.clone();
        serving.extend(runs.iter().map(|r| format!("{r} (gtunnel run)")));
        bail!(
            "Relay still serves: {}. Delete them or pass --force",
            serving.join(", ")
        );
    }
    for name in &runs {
        step(
            false,
            &format!("{name} is a live `gtunnel run` and will lose its URL"),
        );
    }

    for name in &live {
        let tunnel = state
            .find_for_account(name, &acct.name)
            .cloned()
            .context("Tunnel vanished while the relay was being deleted")?;
        routes::remove_route(client, acct, &expose, &tunnel).await?;
        let label = daemon::service_spec(cfg, &tunnel)?.label;
        let _ = daemon::stop(&label).await;
        daemon::uninstall(&label).await?;
        state.remove_for_account(name, &acct.name);
        step(true, &format!("removed tunnel {name}"));
    }
    if !live.is_empty() {
        state.save()?;
    }

    drop_resource(
        client,
        &names.forwarding_rule,
        compute::delete_global_forwarding_rule(client, project, &names.forwarding_rule).await,
    )
    .await?;
    drop_resource(
        client,
        &names.https_proxy,
        compute::delete_target_https_proxy(client, project, &names.https_proxy).await,
    )
    .await?;
    drop_resource(
        client,
        &names.address,
        compute::delete_global_address(client, project, &names.address).await,
    )
    .await?;
    drop_resource(
        client,
        &names.url_map,
        compute::delete_url_map(client, project, &names.url_map).await,
    )
    .await?;
    drop_resource(
        client,
        &names.default_backend,
        compute::delete_backend_service(client, project, &names.default_backend).await,
    )
    .await?;
    drop_resource(
        client,
        &names.health_check,
        compute::delete_health_check(client, project, &names.health_check).await,
    )
    .await?;

    for domain in &expose.domains {
        for wildcard in [true, false] {
            certmanager::delete_certificate_map_entry(
                client,
                project,
                &names.cert_map,
                &names.cert_map_entry(&domain.domain, wildcard),
            )
            .await?;
        }
        certmanager::delete_certificate(client, project, &names.certificate(&domain.domain))
            .await?;
        let auth = names.dns_auth(&domain.domain);
        certmanager::delete_dns_authorization(client, project, &auth).await?;
        step(true, &format!("deleted certificate for {}", domain.domain));
    }
    certmanager::delete_certificate_map(client, project, &names.cert_map).await?;
    step(true, &format!("deleted certificate map {}", names.cert_map));

    drop_resource(
        client,
        &names.instance_group,
        compute::delete_instance_group(client, project, &expose.relay_zone, &names.instance_group)
            .await,
    )
    .await?;
    drop_resource(
        client,
        &names.relay_vm,
        compute::delete_instance(client, project, &expose.relay_zone, &names.relay_vm).await,
    )
    .await?;
    drop_resource(
        client,
        &names.fw_lb_hc,
        compute::delete_firewall(client, project, &names.fw_lb_hc).await,
    )
    .await?;
    drop_resource(
        client,
        &names.fw_iap_ssh,
        compute::delete_firewall(client, project, &names.fw_iap_ssh).await,
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Account, Config, ExposeConfig};
    use crate::gcp::{Endpoints, GcpClient, TokenSource};
    use crate::state::{PersistentTunnel, TunnelSpec, TunnelState};
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn run_record(name: &str, account: &str, public: bool) -> crate::ephemeral::EphemeralRecord {
        crate::ephemeral::EphemeralRecord {
            name: name.to_string(),
            account_name: account.to_string(),
            pid: 1234,
            spec: if public {
                TunnelSpec::Expose {
                    local_target: "3000".to_string(),
                    domain: "dev.example.com".to_string(),
                    hostname: format!("{name}.dev.example.com"),
                    relay_port: 30001,
                    private: false,
                }
            } else {
                TunnelSpec::Access {
                    target: crate::state::AccessTarget::Instance {
                        instance: "vm".to_string(),
                        zone: "europe-west1-b".to_string(),
                    },
                    remote_port: 5432,
                    local_port: 15432,
                }
            },
            started_at: 1_700_000_000,
        }
    }

    // `relay down` counted only tunnels.toml, so a `gtunnel run --public` had
    // the load balancer pulled out from under it without a word
    #[test]
    fn a_live_public_run_counts_as_a_relay_tenant() {
        let records = vec![
            (run_record("gt-live", "dev", true), true),
            (run_record("gt-dead", "dev", true), false),
            (run_record("gt-access", "dev", false), true),
            (run_record("gt-other", "prod", true), true),
        ];
        assert_eq!(
            live_public_runs(&records, "dev"),
            vec!["gt-live".to_string()]
        );
    }

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
            expose: Some(ExposeConfig {
                relay_instance: "gt-dev-relay".to_string(),
                relay_zone: "europe-west1-b".to_string(),
                network: "default".to_string(),
                default_domain: "dev.example.com".to_string(),
                domains: vec![],
            }),
        }
    }

    // Everything the relay checks for already exists, except the firewalls
    fn existing_body() -> serde_json::Value {
        serde_json::json!({
            "name": "existing",
            "status": "RUNNING",
            "address": "34.1.2.3",
            "selfLink": "https://example.test/self",
            "fingerprint": "fp-1",
            "namedPorts": [{"name": "gt-default", "port": 30999}],
            "managed": {"state": "ACTIVE"},
            "dnsResourceRecord": {
                "name": "_acme-challenge.dev.example.com.",
                "type": "CNAME",
                "data": "abc.authorize.certificatemanager.goog."
            },
            "managedZones": [{"name": "example", "dnsName": "dev.example.com."}],
            "rrsets": [{
                "name": "_acme-challenge.dev.example.com.",
                "type": "CNAME",
                "ttl": 300,
                "rrdatas": ["abc.authorize.certificatemanager.goog."]
            }]
        })
    }

    async fn relay_server() -> MockServer {
        let server = MockServer::start().await;
        // Firewalls are the one thing missing, so relay up has to create them
        Mock::given(method("GET"))
            .and(path_regex(r".*/global/firewalls/.*"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": 404, "message": "not found"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(existing_body()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "op-1",
                "status": "DONE"
            })))
            .mount(&server)
            .await;
        server
    }

    async fn requests(server: &MockServer, verb: &str, needle: &str) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str().eq_ignore_ascii_case(verb))
            .filter(|r| r.url.path().contains(needle))
            .count()
    }

    #[tokio::test]
    async fn relay_up_skips_the_vm_that_exists_and_creates_both_firewalls() {
        let server = relay_server().await;
        let opts = RelayUpOptions {
            zone: "europe-west1-b".to_string(),
            network: "default".to_string(),
            managed_zone: "example".to_string(),
        };
        let cfg = relay_up(&client(&server), &account(), &opts)
            .await
            .expect("relay up");

        assert_eq!(cfg.relay_instance, "gt-dev-relay");
        assert_eq!(cfg.default_domain, "dev.example.com");
        assert_eq!(requests(&server, "POST", "/instances").await, 0);
        assert_eq!(requests(&server, "POST", "/global/firewalls").await, 2);
    }

    #[tokio::test]
    async fn relay_up_is_idempotent_for_the_resources_that_exist() {
        let server = relay_server().await;
        let opts = RelayUpOptions {
            zone: "europe-west1-b".to_string(),
            network: "default".to_string(),
            managed_zone: "example".to_string(),
        };
        relay_up(&client(&server), &account(), &opts)
            .await
            .expect("relay up");

        for resource in [
            "/urlMaps",
            "/addresses",
            "/backendServices",
            "/healthChecks",
        ] {
            assert_eq!(
                requests(&server, "POST", resource).await,
                0,
                "{resource} was created although it exists"
            );
        }
    }

    // Review Focus 5: tearing the relay down under live tunnels would break
    // them silently, so it refuses and names them
    #[tokio::test]
    async fn relay_down_refuses_while_expose_tunnels_exist() {
        let server = relay_server().await;
        let mut config = Config {
            selected_account: "dev".to_string(),
            accounts: vec![account()],
        };
        config.accounts[0].expose.as_mut().expect("expose").domains = vec![];
        let mut state = TunnelState::default();
        state
            .add(PersistentTunnel {
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
            })
            .expect("add");

        let err = relay_down(&client(&server), &config, &account(), &mut state, false)
            .await
            .expect_err("should refuse");
        let message = err.to_string();
        assert!(message.contains("api"), "{message}");
        assert_eq!(requests(&server, "DELETE", "/").await, 0);
    }
}
