use anyhow::{bail, Context, Result};

use crate::config::{Account, Config, DomainConfig, ExposeConfig};
use crate::daemon;
use crate::gcp::compute::{
    Firewall, FirewallAllow, NamedPort, Reconciled, IAP_RANGE, LB_HEALTH_CHECK_RANGES,
};
use crate::gcp::{certmanager, compute, dns, is_not_found, GcpClient};
use crate::state::TunnelState;

use super::naming::Names;
use super::routes;

// The relay instance carries this tag, and both firewall rules target it
pub const RELAY_TAG: &str = "gt-relay";
// The default backend's port has no listener, so an unknown host gets a 503
pub const DEFAULT_RELAY_PORT: u16 = 30_999;
pub const RELAY_PORT_RANGE: &str = "30000-30999";

pub struct RelayUpOptions {
    pub zone: String,
    pub network: String,
    pub managed_zone: String,
    pub machine_type: String,
    // Rebuild the VM from the current image family. The only way to patch a
    // relay that has no route to the package mirrors.
    pub recreate: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayStatus {
    pub vm: Option<String>,
    pub ip: Option<String>,
    pub certs: Vec<(String, String)>,
    pub forwarding_rule: bool,
    pub machine_type: Option<String>,
}

// A relay is created at one size and left there. A resize needs a stop, and a
// stop drops every exposed URL until the instance is back, so `relay up`
// reports the difference and leaves the choice, and the downtime, to the
// person. None when the relay did not say what it is: no ground to call it wrong.
pub fn machine_type_mismatch(
    have: Option<&str>,
    want: &str,
    project: &str,
    zone: &str,
    vm: &str,
) -> Option<String> {
    let have = have?;
    if have == want {
        return None;
    }
    Some(format!(
        "relay instance {vm} is {have} but relay.machine_type is {want}; it is left as it is, \
         because changing the type needs a stop. Run `gtunnel relay down`, then `gtunnel relay \
         up`, or stop the instance and run: gcloud compute instances set-machine-type {vm} \
         --machine-type={want} --zone={zone} --project={project}"
    ))
}

// One line per step, so `relay up` shows what it created and what it found
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn step(created: bool, what: &str) {
    if created {
        println!("  ✓ {what}");
    } else {
        println!("  · {what} (exists)");
    }
}

#[allow(clippy::print_stdout, clippy::print_stderr)]
fn step_note(note: &str) {
    println!("  ! {note}");
}

// Like `step`, for a resource that is converged and not only created: a
// resource `relay up` rewrote has to say so, or "exists" reads as untouched
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn step_reconciled(change: Reconciled, what: &str) {
    match change {
        Reconciled::Created => println!("  ✓ {what}"),
        Reconciled::Updated => println!("  ~ {what} (updated)"),
        Reconciled::Unchanged => println!("  · {what} (exists)"),
    }
}

// Metadata alone reaches the VM at its next boot, and a relay can run for
// months, so the script is also run now
const STARTUP_RERUN_COMMAND: &str = "sudo google_metadata_script_runner startup";
// The first ssh to a VM can generate and propagate a key before it connects
const STARTUP_RERUN_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

pub fn startup_rerun_argv(acct: &Account, zone: &str, vm: &str) -> Result<Vec<String>> {
    let mut argv = crate::tunnel::iap_ssh_prefix(vm, zone)?;
    argv.push(format!("--command={STARTUP_RERUN_COMMAND}"));
    argv.extend(crate::tunnel::gcloud_common(
        acct,
        crate::loglevel::current(),
    )?);
    Ok(argv)
}

// The metadata is already written when this runs, so a second `relay up` would
// find it matching and never retry: the error carries the command to run by hand
async fn rerun_startup_script(acct: &Account, zone: &str, vm: &str) -> Result<()> {
    let argv = startup_rerun_argv(acct, zone, vm)?;
    let gcloud = crate::gcp::gcloud::require_gcloud()?;
    let mut cmd = tokio::process::Command::new(&gcloud);
    cmd.args(&argv).stdin(std::process::Stdio::null());
    let manual = format!("gcloud {}", argv.join(" "));
    let out = crate::proc::output_within(cmd, STARTUP_RERUN_BOUND, &manual)
        .await
        .with_context(|| {
            format!("The relay's startup script was updated but could not be re-run. Run: {manual}")
        })?;
    if !out.status.success() {
        bail!(
            "The relay's startup script was updated but re-running it failed: {}. Run: {manual}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

// Waits for the startup script's last line, reading only what the serial
// console printed after `start`: an earlier boot's marker is still in the
// buffer. Ok(false) is a script that never finished inside `bound`, which is
// worth saying and not worth failing the command over.
async fn wait_relay_ready(
    client: &GcpClient,
    project: &str,
    zone: &str,
    vm: &str,
    start: u64,
    bound: std::time::Duration,
) -> Result<bool> {
    let began = std::time::Instant::now();
    let mut from = start;
    loop {
        let chunk = compute::serial_output(client, project, zone, vm, from).await?;
        if chunk.contents.contains(compute::RELAY_READY_MARKER) {
            return Ok(true);
        }
        from = chunk.next.max(from);
        if began.elapsed() >= bound {
            return Ok(false);
        }
        tokio::time::sleep(client.poll_interval()).await;
    }
}

// Where the serial console is now, for a relay whose script is about to be
// replaced and re-run. Failing to read it only costs the wait.
async fn serial_offset(client: &GcpClient, project: &str, zone: &str, vm: &str) -> Option<u64> {
    compute::serial_output(client, project, zone, vm, 0)
        .await
        .ok()
        .map(|c| c.next)
}

// The script installs nftables at most, so three minutes is already a script
// that will not finish; waiting out the whole operation timeout only delays the
// warning.
const RELAY_READY_BOUND: std::time::Duration = std::time::Duration::from_secs(180);

#[allow(clippy::print_stdout, clippy::print_stderr)]
async fn await_relay_ready(client: &GcpClient, project: &str, zone: &str, vm: &str, start: u64) {
    let bound = client.operation_timeout().min(RELAY_READY_BOUND);
    match wait_relay_ready(client, project, zone, vm, start, bound).await {
        Ok(true) => println!("  ✓ relay sshd ready"),
        Ok(false) => eprintln!(
            "  ! the relay's startup script did not report done within {}s; a tunnel started \
             now may bind on loopback only. Read the serial console: gcloud compute \
             instances get-serial-port-output {vm} --zone={zone}",
            bound.as_secs()
        ),
        Err(e) => eprintln!("  ! could not read the relay's serial console ({e:#}); not waiting"),
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

    let present = dns::find_record_set(client, project, managed_zone, &auth.cname_name, "CNAME")
        .await?
        .is_some();
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
    let _relay = super::lock::route_guard(&acct.name)?;
    let names = Names::new(&acct.name);
    let project = &acct.project;
    let zone = &opts.zone;
    let network = &opts.network;

    let domain = domain_of_zone(client, project, &opts.managed_zone).await?;

    ensure_firewall(client, project, &iap_ssh_firewall(&names, network)).await?;
    ensure_firewall(client, project, &lb_health_check_firewall(&names, network)).await?;

    let existing_vm = compute::get_instance(client, project, zone, &names.relay_vm).await?;
    let mut rebuilt = false;
    let vm_change = match &existing_vm {
        None => {
            let op = compute::insert_relay_instance(
                client,
                project,
                zone,
                &names.relay_vm,
                network,
                RELAY_TAG,
                &opts.machine_type,
            )
            .await?;
            client.wait_operation(&op).await?;
            await_relay_ready(client, project, zone, &names.relay_vm, 0).await;
            Reconciled::Created
        }
        Some(_) if opts.recreate => {
            // Deleting first frees the name; the instance group drops a
            // deleted member by itself, so it is joined again below
            let gone = compute::delete_instance(client, project, zone, &names.relay_vm).await;
            drop_resource(client, &format!("relay instance {}", names.relay_vm), gone).await?;
            let op = compute::insert_relay_instance(
                client,
                project,
                zone,
                &names.relay_vm,
                network,
                RELAY_TAG,
                &opts.machine_type,
            )
            .await?;
            client.wait_operation(&op).await?;
            await_relay_ready(client, project, zone, &names.relay_vm, 0).await;
            rebuilt = true;
            Reconciled::Created
        }
        Some(vm) => {
            if let Some(note) = machine_type_mismatch(
                vm.machine_type.as_deref(),
                &opts.machine_type,
                project,
                zone,
                &names.relay_vm,
            ) {
                step_note(&note);
            }
            let rewriting = vm.status == "RUNNING"
                && vm.startup_script() != Some(compute::RELAY_STARTUP_SCRIPT);
            let offset = if rewriting {
                serial_offset(client, project, zone, &names.relay_vm).await
            } else {
                None
            };
            let change = compute::reconcile_relay_instance(client, project, zone, vm).await?;
            // A stopped VM runs the new script at its next boot anyway, and
            // ssh to it would only fail
            if change == Reconciled::Updated && vm.status == "RUNNING" {
                rerun_startup_script(acct, zone, &names.relay_vm).await?;
                if let Some(start) = offset {
                    await_relay_ready(client, project, zone, &names.relay_vm, start).await;
                }
            }
            change
        }
    };
    step_reconciled(vm_change, &format!("relay instance {}", names.relay_vm));

    let group_url =
        compute::get_instance_group(client, project, zone, &names.instance_group).await?;
    let group_url = match group_url {
        Some(url) => {
            if rebuilt {
                let op = compute::add_instance_to_group(
                    client,
                    project,
                    zone,
                    &names.instance_group,
                    &names.relay_vm,
                )
                .await?;
                client.wait_operation(&op).await?;
            }
            step(rebuilt, &format!("instance group {}", names.instance_group));
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

    let hc_change = compute::reconcile_health_check(client, project, &names.health_check).await?;
    step_reconciled(hc_change, &format!("health check {}", names.health_check));

    let backend_settings = compute::BackendSettings::configured(false);
    let backend_change =
        match compute::get_backend_service(client, project, &names.default_backend).await? {
            None => {
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
                    &backend_settings,
                    &names.owner(),
                )
                .await?;
                client.wait_operation(&op).await?;
                Reconciled::Created
            }
            Some(existing) => {
                compute::reconcile_backend_service(
                    client,
                    project,
                    &names.default_backend,
                    &backend_settings,
                    &existing,
                )
                .await?
            }
        };
    step_reconciled(
        backend_change,
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

    let instance =
        compute::get_instance(client, project, &expose.relay_zone, &names.relay_vm).await?;
    let machine_type = instance.as_ref().and_then(|i| i.machine_type.clone());
    let vm = instance.map(|i| i.status);
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
        machine_type,
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
    // Held to the last delete: a route built meanwhile would point at a group
    // that is about to go
    let _relay = super::lock::teardown_guard(&acct.name)?;

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
        // One removal per tunnel, each under the lock on its own: the
        // route and service work above awaits, so no lock spans the loop
        TunnelState::update(|s| {
            s.remove_for_account(name, &acct.name);
            Ok(())
        })?;
        state.remove_for_account(name, &acct.name);
        step(true, &format!("removed tunnel {name}"));
    }

    // The group cannot be deleted while any backend service points at it, and
    // that is only found out at the end, with the front end already gone. Ask
    // now, while stopping still leaves a relay that works.
    let holders: Vec<compute::GroupHolder> = compute::backend_services_on_group(
        client,
        project,
        &expose.relay_zone,
        &names.instance_group,
    )
    .await?
    .into_iter()
    .filter(|h| h.name != names.default_backend)
    .collect();
    let owner = names.owner();
    let (orphans, foreign): (Vec<_>, Vec<_>) = holders
        .into_iter()
        .partition(|h| h.description.as_deref() == Some(owner.as_str()));
    if let Some(first) = foreign.first() {
        bail!(
            "Backend service {} points at the relay's instance group and gtunnel did not create it. \
             Delete it or point it elsewhere, then run `relay down` again",
            first.name
        );
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
    // Gone with the url map above, which held the host rules that named them
    for orphan in &orphans {
        drop_resource(
            client,
            &orphan.name,
            compute::delete_backend_service(client, project, &orphan.name).await,
        )
        .await?;
    }
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
    use crate::gcp::GcpClient;
    use crate::state::{PersistentTunnel, TunnelSpec, TunnelState};
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // The VM refuses a peer by itself, so no rule gtunnel adds to the VPC may
    // name an internal source: that would be the operator's decision (design
    // decision 8), not gtunnel's
    #[test]
    fn no_firewall_rule_gtunnel_creates_admits_a_vpc_source() {
        let names = Names::new("dev");
        let iap = iap_ssh_firewall(&names, "default");
        assert_eq!(iap.source_ranges, vec![IAP_RANGE.to_string()]);
        assert_eq!(iap.allowed[0].ports, vec!["22".to_string()]);
        let lb = lb_health_check_firewall(&names, "default");
        assert_eq!(lb.source_ranges, vec!["130.211.0.0/22", "35.191.0.0/16"]);
        assert_eq!(lb.allowed[0].ports, vec![RELAY_PORT_RANGE.to_string()]);
        assert!(!lb
            .source_ranges
            .iter()
            .any(|r| r.starts_with("10.") || r == "0.0.0.0/0"));
    }

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
                        instance: "bastion".to_string(),
                        zone: "europe-west1-b".to_string(),
                    },
                    remote_port: 5432,
                    local_port: 15432,
                }
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
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

    fn account() -> Account {
        Account {
            name: "dev".to_string(),
            gcloud_configuration: "dev".to_string(),
            project: "p".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: None,
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
        relay_mocks(&server).await;
        server
    }

    // Mocks match in the order they are mounted, so a test that wants one
    // resource to look old mounts its own mock before calling this
    async fn relay_mocks(server: &MockServer) {
        // Firewalls are the one thing missing, so relay up has to create them
        Mock::given(method("GET"))
            .and(path_regex(r".*/global/firewalls/.*"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": 404, "message": "not found"}
            })))
            .mount(server)
            .await;
        // The resources gtunnel converges already look the way gtunnel wants
        // them, so a relay up that sends anything to them is a regression
        Mock::given(method("GET"))
            .and(path_regex(r".*/instances/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-relay",
                "status": "RUNNING",
                "machineType": "zones/europe-west1-b/machineTypes/e2-micro",
                "metadata": {
                    "fingerprint": "fp-meta",
                    "items": [{
                        "key": "startup-script",
                        "value": compute::RELAY_STARTUP_SCRIPT
                    }]
                }
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/healthChecks/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(health_check_body(5, 1)))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/backendServices/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-default",
                "iap": {"enabled": false},
                "timeoutSec": 3600,
                "connectionDraining": {"drainingTimeoutSec": 30}
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(existing_body()))
            .mount(server)
            .await;
        for verb in ["POST", "PATCH"] {
            Mock::given(method(verb))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "name": "op-1",
                    "status": "DONE"
                })))
                .mount(server)
                .await;
        }
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

    fn options() -> RelayUpOptions {
        RelayUpOptions {
            zone: "europe-west1-b".to_string(),
            network: "default".to_string(),
            managed_zone: "example".to_string(),
            machine_type: "e2-micro".to_string(),
            recreate: false,
        }
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

    async fn serial_server(bodies: Vec<serde_json::Value>) -> MockServer {
        let server = MockServer::start().await;
        for body in bodies {
            Mock::given(method("GET"))
                .and(path_regex(r".*/serialPort"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .up_to_n_times(1)
                .mount(&server)
                .await;
        }
        server
    }

    // `relay up` returns when the instance insert finishes, which is before
    // the startup script has run: an `expose` straight after would bind on
    // loopback only, and the load balancer would never reach it
    #[tokio::test]
    async fn relay_up_waits_for_the_startup_script_to_print_its_marker() {
        let server = serial_server(vec![
            serde_json::json!({"contents": "booting\n", "next": 8}),
            serde_json::json!({"contents": "startup-script: gtunnel-relay-ready\n", "next": 45}),
        ])
        .await;
        let ready = wait_relay_ready(
            &GcpClient::for_mock(&server),
            "p",
            "z",
            "vm",
            0,
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("serial");
        assert!(ready);
        assert_eq!(requests(&server, "GET", "/serialPort").await, 2);
    }

    #[tokio::test]
    async fn a_marker_from_an_earlier_boot_does_not_count() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/serialPort"))
            .and(wiremock::matchers::query_param("start", "500"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"contents": "", "next": 500})),
            )
            .mount(&server)
            .await;
        let ready = wait_relay_ready(
            &GcpClient::for_mock(&server),
            "p",
            "z",
            "vm",
            500,
            std::time::Duration::from_millis(100),
        )
        .await
        .expect("serial");
        assert!(!ready, "only output after the offset is read");
    }

    #[tokio::test]
    async fn relay_up_skips_the_vm_that_exists_and_creates_both_firewalls() {
        let server = relay_server().await;
        let opts = RelayUpOptions {
            zone: "europe-west1-b".to_string(),
            network: "default".to_string(),
            managed_zone: "example".to_string(),
            machine_type: "e2-micro".to_string(),
            recreate: false,
        };
        let cfg = relay_up(&GcpClient::for_mock(&server), &account(), &opts)
            .await
            .expect("relay up");

        assert_eq!(cfg.relay_instance, "gt-dev-relay");
        assert_eq!(cfg.default_domain, "dev.example.com");
        assert_eq!(requests(&server, "POST", "/instances").await, 0);
        assert_eq!(requests(&server, "POST", "/global/firewalls").await, 2);
    }

    #[tokio::test]
    async fn relay_up_recreate_deletes_and_reinserts_the_instance() {
        let server = MockServer::start().await;
        // The rebuilt VM is waited on through its serial console
        Mock::given(method("GET"))
            .and(path_regex(r".*/serialPort"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "contents": "startup-script: gtunnel-relay-ready\n",
                "next": 40
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(r".*/instances/gt-dev-relay"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "op-1",
                "status": "DONE"
            })))
            .mount(&server)
            .await;
        relay_mocks(&server).await;
        let opts = RelayUpOptions {
            recreate: true,
            ..options()
        };

        relay_up(&GcpClient::for_mock(&server), &account(), &opts)
            .await
            .expect("relay up --recreate");

        let log = server.received_requests().await.unwrap_or_default();
        let position = |verb: &str, needle: &str| {
            log.iter().position(|r| {
                r.method.as_str().eq_ignore_ascii_case(verb) && r.url.path().contains(needle)
            })
        };
        assert_eq!(
            requests(&server, "DELETE", "/instances/gt-dev-relay").await,
            1
        );
        assert_eq!(requests(&server, "POST", "/instances").await, 1);
        assert!(
            position("DELETE", "/instances/").expect("delete")
                < position("POST", "/instances").expect("insert"),
            "the name is free only after the delete"
        );
        // Nothing the recreate is not about is deleted
        assert_eq!(requests(&server, "DELETE", "/instanceGroups").await, 0);
        assert_eq!(requests(&server, "DELETE", "/forwardingRules").await, 0);
        assert_eq!(requests(&server, "DELETE", "/sslCertificates").await, 0);
        assert_eq!(requests(&server, "POST", "/addInstances").await, 1);
    }

    #[tokio::test]
    async fn relay_up_without_recreate_never_deletes_the_instance() {
        let server = relay_server().await;

        relay_up(&GcpClient::for_mock(&server), &account(), &options())
            .await
            .expect("relay up");

        assert_eq!(requests(&server, "DELETE", "/instances").await, 0);
        assert_eq!(requests(&server, "POST", "/addInstances").await, 0);
    }

    // Idempotence is "no request when everything matches": creating nothing is
    // not enough, because an update is a request too
    #[tokio::test]
    async fn relay_up_sends_no_write_to_a_resource_that_already_matches() {
        let server = relay_server().await;
        relay_up(&GcpClient::for_mock(&server), &account(), &options())
            .await
            .expect("relay up");

        for resource in [
            "/urlMaps",
            "/addresses",
            "/backendServices",
            "/healthChecks",
        ] {
            for verb in ["POST", "PATCH", "PUT"] {
                assert_eq!(
                    requests(&server, verb, resource).await,
                    0,
                    "{verb} {resource} was sent although it matches"
                );
            }
        }
        assert_eq!(requests(&server, "POST", "/setMetadata").await, 0);
    }

    // Before this, a fix to the health check reached only relays created after
    // it, and `relay down` refuses while a tunnel holds a route
    #[tokio::test]
    async fn relay_up_patches_an_existing_health_check_that_has_old_values() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/healthChecks/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(health_check_body(10, 2)))
            .mount(&server)
            .await;
        relay_mocks(&server).await;

        relay_up(&GcpClient::for_mock(&server), &account(), &options())
            .await
            .expect("relay up");

        assert_eq!(requests(&server, "PATCH", "/healthChecks/").await, 1);
        assert_eq!(requests(&server, "POST", "/healthChecks").await, 0);
        let patch = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|r| r.method.as_str() == "PATCH" && r.url.path().contains("/healthChecks/"))
            .expect("patch");
        let body: serde_json::Value = serde_json::from_slice(&patch.body).expect("json");
        assert_eq!(body["checkIntervalSec"], 5);
        assert_eq!(body["healthyThreshold"], 1);
        assert_eq!(body["unhealthyThreshold"], 3);
    }

    // The default backend is a backend service like any other
    #[tokio::test]
    async fn relay_up_patches_a_default_backend_that_lacks_the_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/backendServices/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-default",
                "iap": {"enabled": false}
            })))
            .mount(&server)
            .await;
        relay_mocks(&server).await;

        relay_up(&GcpClient::for_mock(&server), &account(), &options())
            .await
            .expect("relay up");

        assert_eq!(requests(&server, "PATCH", "/backendServices/").await, 1);
    }

    #[test]
    fn the_startup_script_is_rerun_over_iap_ssh_as_root() {
        let mut acct = account();
        acct.impersonate_service_account = Some("sa@example.com".to_string());
        let argv = startup_rerun_argv(&acct, "europe-west1-b", "gt-dev-relay").expect("argv");
        assert_eq!(&argv[..3], ["compute", "ssh", "gt-dev-relay"]);
        for want in [
            "--zone=europe-west1-b",
            "--tunnel-through-iap",
            "--quiet",
            "--command=sudo google_metadata_script_runner startup",
            "--project=p",
            "--impersonate-service-account=sa@example.com",
        ] {
            assert!(argv.iter().any(|a| a == want), "{want} missing in {argv:?}");
        }
    }

    #[test]
    fn the_startup_rerun_refuses_an_instance_or_zone_that_would_read_as_a_flag() {
        let acct = account();
        for (zone, vm) in [
            ("europe-west1-b", "--project=other"),
            ("--zone=x", "gt-dev-relay"),
            ("europe-west1-b", "bad name"),
        ] {
            let err = startup_rerun_argv(&acct, zone, vm)
                .expect_err("a name that is not a name must not reach gcloud");
            assert!(
                format!("{err:#}").contains("Invalid"),
                "{zone} {vm}: {err:#}"
            );
        }
    }

    // Review Focus 5: tearing the relay down under live tunnels would break
    // them silently, so it refuses and names them
    #[tokio::test]
    async fn relay_down_refuses_while_expose_tunnels_exist() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        let server = relay_server().await;
        let mut config = Config {
            selected_account: "dev".to_string(),
            accounts: vec![account()],
            ..Default::default()
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

        let err = relay_down(
            &GcpClient::for_mock(&server),
            &config,
            &account(),
            &mut state,
            false,
        )
        .await
        .expect_err("should refuse");
        let message = err.to_string();
        assert!(message.contains("api"), "{message}");
        assert_eq!(requests(&server, "DELETE", "/").await, 0);
    }

    const GROUP_PATH: &str = "/projects/p/zones/europe-west1-b/instanceGroups/gt-dev-ig";

    fn backend_on_group(
        name: &str,
        description: Option<&str>,
        server: &MockServer,
    ) -> serde_json::Value {
        let mut b = serde_json::json!({
            "name": name,
            "backends": [{"group": format!("{}{GROUP_PATH}", server.uri())}],
        });
        if let Some(d) = description {
            b["description"] = d.into();
        }
        b
    }

    async fn list_backends(server: &MockServer, items: Vec<serde_json::Value>) {
        Mock::given(method("GET"))
            .and(path_regex(r".*/global/backendServices$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "items": items })),
            )
            .mount(server)
            .await;
    }

    // GCP refuses to delete a group a backend service still points at, so the
    // fake does too: the group delete fails until the backend delete was seen
    async fn group_guarded_deletes(server: &MockServer, backend: &'static str) {
        let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        Mock::given(method("DELETE"))
            .respond_with(move |req: &wiremock::Request| {
                use std::sync::atomic::Ordering;
                let p = req.url.path();
                if p.ends_with(backend) {
                    gone.store(true, Ordering::SeqCst);
                }
                if p.contains("/instanceGroups/") && !gone.load(Ordering::SeqCst) {
                    return ResponseTemplate::new(400).set_body_json(serde_json::json!({
                        "error": {"code": 400, "message": format!(
                            "The instance_group resource is already being used by 'projects/p/global/backendServices/{backend}'")}
                    }));
                }
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"name": "op-1", "status": "DONE"}))
            })
            .mount(server)
            .await;
    }

    fn dev_config() -> Config {
        Config {
            selected_account: "dev".to_string(),
            accounts: vec![account()],
            ..Default::default()
        }
    }

    fn deleted(reqs: &[wiremock::Request]) -> Vec<String> {
        reqs.iter()
            .filter(|r| r.method.as_str() == "DELETE")
            .map(|r| r.url.path().to_string())
            .collect()
    }

    // A `run --public` killed before its cleanup leaves a backend service the
    // local state never listed; the group cannot go while it points there
    #[tokio::test]
    async fn relay_down_deletes_an_orphaned_backend_before_the_group() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        // Mocks match in mount order, so the specific ones go first
        let server = MockServer::start().await;
        group_guarded_deletes(&server, "gt-dev-orphan").await;
        list_backends(
            &server,
            vec![backend_on_group(
                "gt-dev-orphan",
                Some("gtunnel:dev"),
                &server,
            )],
        )
        .await;
        relay_mocks(&server).await;

        let mut state = TunnelState::default();
        relay_down(
            &GcpClient::for_mock(&server),
            &dev_config(),
            &account(),
            &mut state,
            false,
        )
        .await
        .expect("relay down");

        let paths = deleted(&server.received_requests().await.unwrap_or_default());
        let at = |needle: &str| paths.iter().position(|p| p.contains(needle));
        let orphan = at("/backendServices/gt-dev-orphan").expect("orphan deleted");
        let group = at("/instanceGroups/gt-dev-ig").expect("group deleted");
        assert!(orphan < group, "{paths:?}");
        assert!(at("/instances/gt-dev-relay").is_some(), "{paths:?}");
    }

    // A route that is mid-build holds the lock; the teardown waits it out
    // instead of deleting the group under it
    #[tokio::test]
    async fn relay_down_waits_for_a_route_that_is_being_built() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        let dir = tempfile::tempdir().unwrap();
        super::super::lock::TEST_DIR.with(|d| *d.borrow_mut() = Some(dir.path().to_path_buf()));
        let route = super::super::lock::route_guard("dev").unwrap();
        let server = relay_server().await;
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(route);
        });
        let started = std::time::Instant::now();

        let mut state = TunnelState::default();
        relay_down(
            &GcpClient::for_mock(&server),
            &dev_config(),
            &account(),
            &mut state,
            false,
        )
        .await
        .expect("relay down");

        release.join().unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(250));
        assert!(requests(&server, "DELETE", "/instances/").await > 0);
    }

    // And a `relay up` started during the teardown is refused, not raced
    #[tokio::test]
    async fn relay_up_refuses_while_the_relay_is_being_torn_down() {
        let dir = tempfile::tempdir().unwrap();
        super::super::lock::TEST_DIR.with(|d| *d.borrow_mut() = Some(dir.path().to_path_buf()));
        let _teardown = super::super::lock::teardown_guard("dev").unwrap();
        let server = relay_server().await;

        let err = relay_up(&GcpClient::for_mock(&server), &account(), &options())
            .await
            .expect_err("should refuse");
        assert!(err.to_string().contains("being torn down"), "{err}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    // Not gtunnel's to delete, so the teardown stops and names it, and stops
    // before the first delete: the relay is still a working relay
    #[tokio::test]
    async fn relay_down_stops_before_any_delete_on_a_backend_it_did_not_create() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        let server = MockServer::start().await;
        group_guarded_deletes(&server, "gt-dev-handmade").await;
        list_backends(
            &server,
            vec![
                backend_on_group("gt-dev-handmade", None, &server),
                // Another group's backend is no reason to stop
                serde_json::json!({
                    "name": "unrelated",
                    "backends": [{"group": format!("{}/projects/p/zones/europe-west1-b/instanceGroups/other", server.uri())}],
                }),
            ],
        )
        .await;
        relay_mocks(&server).await;

        let mut state = TunnelState::default();
        let err = relay_down(
            &GcpClient::for_mock(&server),
            &dev_config(),
            &account(),
            &mut state,
            false,
        )
        .await
        .expect_err("should stop");

        let message = err.to_string();
        assert!(message.contains("gt-dev-handmade"), "{message}");
        assert!(!message.contains("unrelated"), "{message}");
        let paths = deleted(&server.received_requests().await.unwrap_or_default());
        assert!(paths.is_empty(), "{paths:?}");
    }

    // The foreign backend sits on page 2 of the list: reading only page 1
    // would let the teardown delete the front end and then fail at the group
    #[tokio::test]
    async fn relay_down_stops_before_any_delete_on_a_foreign_backend_on_page_two() {
        use wiremock::matchers::query_param;
        let _cfg = crate::config::testing::TempConfigDir::new();
        let server = MockServer::start().await;
        group_guarded_deletes(&server, "gt-dev-handmade").await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/global/backendServices$"))
            .and(query_param("pageToken", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [backend_on_group("gt-dev-handmade", None, &server)]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/global/backendServices$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"name": "unrelated", "backends": []}],
                "nextPageToken": "page-2"
            })))
            .mount(&server)
            .await;
        relay_mocks(&server).await;

        let mut state = TunnelState::default();
        let err = relay_down(
            &GcpClient::for_mock(&server),
            &dev_config(),
            &account(),
            &mut state,
            false,
        )
        .await
        .expect_err("should stop");

        let message = err.to_string();
        assert!(message.contains("gt-dev-handmade"), "{message}");
        let paths = deleted(&server.received_requests().await.unwrap_or_default());
        assert!(paths.is_empty(), "{paths:?}");
    }

    // The teardown reads the run records of the config dir it is given, so a
    // test that owns the dir sees its own records and nobody else's
    #[tokio::test]
    async fn relay_down_sees_only_the_run_records_of_its_own_config_dir() {
        let _cfg = crate::config::testing::TempConfigDir::new();
        let server = relay_server().await;
        let mut state = TunnelState::default();

        // An empty dir: nothing serves, so the teardown goes through
        relay_down(
            &GcpClient::for_mock(&server),
            &dev_config(),
            &account(),
            &mut state,
            false,
        )
        .await
        .expect("an empty run directory must not block relay down");

        // One live public run of the same account, saved here: it is named
        crate::ephemeral::EphemeralRecord {
            name: "gt-own".to_string(),
            account_name: "dev".to_string(),
            pid: std::process::id(),
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "gt-own.dev.example.com".to_string(),
                relay_port: 30008,
                private: false,
            },
            started_at: 1_700_000_000,
            ttl_secs: None,
            pid_start: None,
            owner_pid: None,
            owner_start: None,
        }
        .save()
        .expect("save");
        let err = relay_down(
            &GcpClient::for_mock(&server),
            &dev_config(),
            &account(),
            &mut state,
            false,
        )
        .await
        .expect_err("a live run refuses");
        let message = err.to_string();
        assert!(message.contains("gt-own (gtunnel run)"), "{message}");
        assert!(!message.contains("gt-live"), "{message}");
    }

    // The relay of a running tunnel is not ours to stop: a resize needs a stop,
    // and a stop drops every exposed URL until the instance is back
    async fn relay_of_type(machine_type: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r".*/instances/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-relay",
                "status": "RUNNING",
                "machineType": format!(
                    "https://example.test/compute/v1/projects/p/zones/europe-west1-b/machineTypes/{machine_type}"
                ),
                "metadata": {
                    "fingerprint": "fp-meta",
                    "items": [{
                        "key": "startup-script",
                        "value": compute::RELAY_STARTUP_SCRIPT
                    }]
                }
            })))
            .mount(&server)
            .await;
        relay_mocks(&server).await;
        server
    }

    #[tokio::test]
    async fn relay_up_does_not_resize_a_relay_whose_machine_type_differs() {
        let server = relay_of_type("e2-small").await;
        relay_up(&GcpClient::for_mock(&server), &account(), &options())
            .await
            .expect("relay up");

        for needle in ["/stop", "/setMachineType", "/start", "/delete"] {
            assert_eq!(
                requests(&server, "POST", needle).await,
                0,
                "POST {needle} was sent to resize a running relay"
            );
        }
        assert_eq!(requests(&server, "POST", "/instances").await, 0);
        assert_eq!(requests(&server, "PATCH", "/instances").await, 0);
    }

    #[test]
    fn a_relay_of_another_machine_type_is_reported_with_the_commands_that_fix_it() {
        let note = machine_type_mismatch(
            Some("e2-small"),
            "e2-micro",
            "example-dev-1a2b",
            "europe-west1-b",
            "gt-dev-relay",
        )
        .expect("a mismatch is reported");
        assert!(note.contains("e2-small"), "{note}");
        assert!(note.contains("relay.machine_type"), "{note}");
        assert!(note.contains("e2-micro"), "{note}");
        assert!(note.contains("gtunnel relay down"), "{note}");
        assert!(note.contains("gtunnel relay up"), "{note}");
        assert!(
            note.contains(
                "gcloud compute instances set-machine-type gt-dev-relay --machine-type=e2-micro \
                 --zone=europe-west1-b --project=example-dev-1a2b"
            ),
            "{note}"
        );
        assert!(note.contains("stop"), "{note}");
    }

    #[test]
    fn a_relay_of_the_wanted_or_an_unknown_machine_type_is_not_reported() {
        let same = machine_type_mismatch(Some("e2-micro"), "e2-micro", "p", "z", "vm");
        assert_eq!(same, None);
        // A relay that did not say what it is gives no ground to call it wrong
        assert_eq!(
            machine_type_mismatch(None, "e2-micro", "p", "z", "vm"),
            None
        );
    }

    #[tokio::test]
    async fn relay_status_reports_the_machine_type_the_relay_runs() {
        let server = relay_of_type("e2-small").await;
        let status = relay_status(&GcpClient::for_mock(&server), &account())
            .await
            .expect("status");
        assert_eq!(status.machine_type.as_deref(), Some("e2-small"));
    }
}
