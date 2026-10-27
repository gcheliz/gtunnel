use crate::error::GtunnelError;
use anyhow::{bail, Context, Result};
use serde::Serialize;

use super::{confirm, prompt, resolve};
use crate::config::{update_config, DomainConfig};
use crate::daemon;
use crate::expose::naming::{hostname_for, Names};
use crate::expose::{relay, routes};
use crate::gcp::{certmanager, dns, GcpClient};
use crate::state::{
    allocate_port, validate_tunnel_name, PersistentTunnel, TunnelSpec, TunnelState, TunnelStatus,
    RELAY_PORT_BASE, RELAY_PORT_SPAN,
};

// What the relay costs before anyone agrees to create it
pub fn relay_cost_line() -> String {
    "Global forwarding rule (~USD 18/month) and e2-micro VM (~USD 7/month, free tier in some US \
     regions)"
        .to_string()
}

pub async fn cmd_relay_up(
    account: Option<&str>,
    zone: Option<&str>,
    network: &str,
    managed_zone: Option<&str>,
    assume_yes: bool,
) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let client = GcpClient::for_account(&acct);

    let managed_zone = match managed_zone {
        Some(z) => z.to_string(),
        None => pick_managed_zone(&client, &acct.project).await?,
    };
    let zone = zone.unwrap_or(&acct.default_zone).to_string();

    println!("Relay for account '{}' in {zone}:", acct.name);
    println!("  {}", relay_cost_line());
    if !confirm("Create it?", assume_yes)? {
        println!("Cancelled");
        return Ok(());
    }

    let opts = relay::RelayUpOptions {
        zone,
        network: network.to_string(),
        managed_zone,
    };
    let expose = relay::relay_up(&client, &acct, &opts).await?;
    update_config(|config| {
        config.get_account_mut(Some(&acct.name))?.expose = Some(expose.clone());
        Ok(())
    })?;
    println!("Relay ready. Default domain: {}", expose.default_domain);
    Ok(())
}

// One managed zone needs no question; several do
async fn pick_managed_zone(client: &GcpClient, project: &str) -> Result<String> {
    let zones = dns::list_managed_zones(client, project).await?;
    match zones.len() {
        0 => bail!(
            "No Cloud DNS managed zones in project '{project}'. Create one with `gcloud dns managed-zones create`, then run `gtunnel relay up --managed-zone <zone>`"
        ),
        1 => Ok(zones[0].name.clone()),
        _ => {
            println!("Cloud DNS managed zones:");
            for (index, zone) in zones.iter().enumerate() {
                println!("  {}. {}  {}", index + 1, zone.name, zone.dns_name);
            }
            let choice = prompt("Managed zone number", Some("1"))?;
            let index: usize = choice
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1 && *n <= zones.len())
                .with_context(|| format!("'{choice}' is not one of the listed numbers"))?;
            Ok(zones[index - 1].name.clone())
        }
    }
}

pub async fn cmd_relay_down(account: Option<&str>, force: bool, assume_yes: bool) -> Result<()> {
    let (config, acct) = resolve(account)?;
    let client = GcpClient::for_account(&acct);
    let mut state = TunnelState::load()?;

    if !confirm(
        &format!(
            "Delete the relay for account '{}' and everything it owns?",
            acct.name
        ),
        assume_yes,
    )? {
        println!("Cancelled");
        return Ok(());
    }

    relay::relay_down(&client, &config, &acct, &mut state, force).await?;
    update_config(|config| {
        config.get_account_mut(Some(&acct.name))?.expose = None;
        Ok(())
    })?;
    println!("Relay deleted");
    Ok(())
}

pub async fn cmd_relay_status(account: Option<&str>) -> Result<()> {
    let (_, acct) = resolve(account)?;
    if acct.expose.is_none() {
        println!(
            "No relay for account '{}'. Run `gtunnel relay up`",
            acct.name
        );
        return Ok(());
    }
    let client = GcpClient::for_account(&acct);
    let status = relay::relay_status(&client, &acct).await?;

    println!(
        "vm              {}",
        status.vm.as_deref().unwrap_or("missing")
    );
    println!(
        "address         {}",
        status.ip.as_deref().unwrap_or("missing")
    );
    println!(
        "forwarding rule {}",
        if status.forwarding_rule {
            "present"
        } else {
            "missing"
        }
    );
    for (domain, state) in status.certs {
        println!("certificate     {domain}: {state}");
    }
    Ok(())
}

pub async fn cmd_domains(
    account: Option<&str>,
    default: Option<&str>,
    add: Option<&str>,
) -> Result<()> {
    let (_, acct) = resolve(account)?;
    let expose = acct.expose.clone().with_context(|| {
        format!(
            "No relay for account '{}'. Run `gtunnel relay up`",
            acct.name
        )
    })?;

    if let Some(managed_zone) = add {
        let client = GcpClient::for_account(&acct);
        let domain: DomainConfig = relay::add_domain(&client, &acct, managed_zone).await?;
        update_config(|config| {
            let stored = config
                .get_account_mut(Some(&acct.name))?
                .expose
                .as_mut()
                .context("The relay vanished while the domain was being added")?;
            if !stored.domains.iter().any(|d| d.domain == domain.domain) {
                stored.domains.push(domain.clone());
            }
            Ok(())
        })?;
        println!("Domain {} registered", domain.domain);
        return Ok(());
    }

    if let Some(domain) = default {
        if !expose.domains.iter().any(|d| d.domain == domain) {
            bail!("Domain '{domain}' is not registered. Run `gtunnel domains add`");
        }
        update_config(|config| {
            config
                .get_account_mut(Some(&acct.name))?
                .expose
                .as_mut()
                .context("The relay vanished while the default domain was being set")?
                .default_domain = domain.to_string();
            Ok(())
        })?;
        println!("Default domain for '{}' is now {domain}", acct.name);
        return Ok(());
    }

    for registered in &expose.domains {
        let marker = if registered.domain == expose.default_domain {
            "*"
        } else {
            " "
        };
        println!(
            "{marker} {:<32} zone={}",
            registered.domain, registered.managed_zone
        );
    }
    Ok(())
}

// Auto-start is off for a new tunnel: `-s` starts it now, and `A` in the TUI
// is what makes one survive a reboot
fn new_expose_tunnel(
    name: &str,
    account: &str,
    local_target: &str,
    domain: &str,
    relay_port: u16,
    private: bool,
) -> PersistentTunnel {
    PersistentTunnel {
        name: name.to_string(),
        account_name: account.to_string(),
        auto_start: false,
        spec: TunnelSpec::Expose {
            local_target: local_target.to_string(),
            domain: domain.to_string(),
            hostname: hostname_for(name, domain),
            relay_port,
            private,
        },
    }
}

/// What to publish. `private` is the whole of the access decision, so each
/// front end states it itself: the CLI defaults to public, MCP to private.
pub struct ExposeRequest<'a> {
    pub name: &'a str,
    pub target: &'a str,
    pub domain: Option<&'a str>,
    pub private: bool,
}

/// The outcome of an expose, shaped for a machine reader; the CLI prints two
/// lines from it and MCP returns it as structured content.
#[derive(Debug, Serialize)]
pub struct Exposed {
    pub name: String,
    pub account: String,
    pub url: String,
    pub target: String,
    pub domain: String,
    pub public: bool,
    pub certificate_state: String,
}

/// Everything `expose` does except print and start, so a caller that owns
/// stdout (the MCP server) can reuse it without corrupting a frame.
pub async fn expose_tunnel(account: Option<&str>, req: &ExposeRequest<'_>) -> Result<Exposed> {
    let name = req.name;
    validate_tunnel_name(name)?;
    let (config, acct) = resolve(account)?;
    let expose = acct.expose.clone().ok_or_else(|| {
        GtunnelError::usage(format!(
            "No relay for account '{}'. Run `gtunnel relay up`",
            acct.name
        ))
    })?;
    let domain = expose.domain(req.domain)?.clone();

    // Before any GCP call: the argv builder checks it too, but only at start
    crate::tunnel::normalize_local_target(req.target)?;

    // Read to choose a relay port before the route is created; the locked
    // update below re-checks both the name and the port
    let state = TunnelState::load()?;
    if state.find_for_account(name, &acct.name).is_some() {
        return Err(GtunnelError::usage(format!(
            "Tunnel '{name}' already exists for account '{}'",
            acct.name
        ))
        .into());
    }
    state.check_label(&acct.name, name)?;

    let relay_port = allocate_port(
        name,
        RELAY_PORT_BASE,
        RELAY_PORT_SPAN,
        // The port is bound on the relay VM, not here, so nothing local can
        // probe it: tunnels.toml and the live runs are the whole record
        &crate::ephemeral::used_relay_ports(&state, &acct.name)?,
        |_| true,
    )?;

    let tunnel = new_expose_tunnel(
        name,
        &acct.name,
        req.target,
        &domain.domain,
        relay_port,
        req.private,
    );

    let client = GcpClient::for_account(&acct);
    routes::add_route(&client, &acct, &expose, &tunnel).await?;

    TunnelState::update(|state| {
        // The route above awaited, so another gtunnel may have taken the port
        if crate::ephemeral::used_relay_ports(state, &acct.name)?.contains(&relay_port) {
            bail!("Relay port {relay_port} was taken by another tunnel while the route was created; run `gtunnel doctor` to clear the route, then add the tunnel again");
        }
        state.add(tunnel.clone())
    })?;
    let spec = daemon::service_spec(&config, &tunnel)?;
    daemon::install(&spec).await?;

    // A certificate that is still provisioning is the usual reason the first
    // request fails, so say so instead of letting the browser explain it
    let names = Names::new(&acct.name);
    let certificate_state =
        certmanager::certificate_state(&client, &acct.project, &names.certificate(&domain.domain))
            .await
            .unwrap_or_else(|_| "UNKNOWN".to_string());

    Ok(Exposed {
        name: name.to_string(),
        account: acct.name.clone(),
        url: tunnel.display_url(),
        target: tunnel.display_target(),
        domain: domain.domain.clone(),
        public: !req.private,
        certificate_state,
    })
}

pub async fn cmd_expose(
    account: Option<&str>,
    name: &str,
    target: &str,
    domain: Option<&str>,
    private: bool,
    start: bool,
) -> Result<()> {
    let req = ExposeRequest {
        name,
        target,
        domain,
        private,
    };
    let exposed = expose_tunnel(account, &req).await?;

    println!(
        "{} {name}  {} -> {}",
        TunnelStatus::Stopped.symbol(),
        exposed.url,
        exposed.target
    );
    if exposed.certificate_state != "ACTIVE" {
        println!(
            "  certificate {}: HTTPS works once it is ACTIVE (usually under 15 minutes)",
            exposed.certificate_state
        );
    }

    if start {
        super::access::cmd_start(account, name).await?;
    }
    Ok(())
}

#[cfg(test)]
mod new_tunnel_tests {
    use super::*;

    // `expose` without `-s` printed stopped and started the tunnel anyway
    #[test]
    fn a_new_expose_tunnel_does_not_auto_start() {
        let t = new_expose_tunnel("web", "dev", "localhost:3000", "example.com", 30001, false);
        assert!(!t.auto_start);
        assert_eq!(t.display_url(), "https://web.example.com");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{save_config, Account, Config};

    use crate::config::testing::TempConfigDir;

    fn account_without_relay() -> Account {
        Account {
            name: "dev".to_string(),
            gcloud_configuration: "dev".to_string(),
            project: "proj-1".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            expose: None,
        }
    }

    // Exposing before the relay exists has to say what to run, not fail at the
    // first GCP call
    #[tokio::test]
    async fn expose_without_a_relay_points_at_relay_up() {
        let _temp = TempConfigDir::new();
        let mut config = Config::default();
        config
            .add_account(account_without_relay())
            .expect("add account");
        save_config(&config).expect("save");

        let err = cmd_expose(None, "api", "3000", None, false, false)
            .await
            .expect_err("should refuse");
        let message = format!("{err:#}");
        assert!(message.contains("gtunnel relay up"), "{message}");
        assert!(message.contains("dev"), "{message}");
    }

    #[tokio::test]
    async fn the_core_without_a_relay_points_at_relay_up() {
        let _temp = TempConfigDir::new();
        let mut config = Config::default();
        config
            .add_account(account_without_relay())
            .expect("add account");
        save_config(&config).expect("save");

        let req = ExposeRequest {
            name: "api",
            target: "3000",
            domain: None,
            private: true,
        };
        let err = expose_tunnel(None, &req).await.expect_err("should refuse");
        let message = format!("{err:#}");
        assert!(message.contains("gtunnel relay up"), "{message}");
        assert!(message.contains("dev"), "{message}");
    }

    #[test]
    fn the_cost_line_names_both_charges() {
        let line = relay_cost_line();
        assert!(line.contains("forwarding rule"), "{line}");
        assert!(line.contains("e2-micro"), "{line}");
    }
    #[tokio::test]
    async fn pick_managed_zone_empty_suggests_creating_one() {
        use crate::gcp::{Endpoints, GcpClient, TokenSource};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/proj-1/managedZones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "managedZones": []
            })))
            .mount(&server)
            .await;

        let client = GcpClient::new(
            TokenSource::Static("tok".to_string()),
            Endpoints::single(&server.uri()),
        );

        let err = pick_managed_zone(&client, "proj-1")
            .await
            .expect_err("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("No Cloud DNS managed zones in project 'proj-1'"));
        assert!(msg.contains(
            "Create one with `gcloud dns managed-zones create`, then run `gtunnel relay up --managed-zone <zone>`"
        ));
    }
}
