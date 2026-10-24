use anyhow::{bail, Result};

use crate::config::{Account, ExposeConfig};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec};

// Flags every gcloud invocation carries, so a tunnel never depends on which
// gcloud configuration happens to be active in the shell
pub fn gcloud_common(acct: &Account) -> Vec<String> {
    let mut args = vec![
        format!("--configuration={}", acct.gcloud_configuration),
        format!("--project={}", acct.project),
        // The per-connection lines in gcloud's iap_tunnel are log.info, and
        // the default verbosity is warning, so a tunnel carrying traffic
        // logged nothing to say so
        "--verbosity=info".to_string(),
    ];
    if let Some(sa) = &acct.impersonate_service_account {
        args.push(format!("--impersonate-service-account={sa}"));
    }
    args
}

// `gcloud compute start-iap-tunnel` argv for an Access tunnel
pub fn access_argv(acct: &Account, t: &PersistentTunnel) -> Result<Vec<String>> {
    let (target, remote_port, local_port) = match &t.spec {
        TunnelSpec::Access {
            target,
            remote_port,
            local_port,
        } => (target, *remote_port, *local_port),
        TunnelSpec::Expose { .. } => {
            bail!(
                "Tunnel '{}' is an Expose tunnel, not an Access tunnel",
                t.name
            )
        }
    };

    // A jump tunnel is a different gcloud command, not a different flag: IAP
    // carries an ssh connection and ssh makes the forward from the far side
    if let AccessTarget::Jump {
        instance,
        zone,
        host,
    } = target
    {
        let mut argv = vec![
            "compute".to_string(),
            "ssh".to_string(),
            instance.clone(),
            format!("--zone={zone}"),
            "--tunnel-through-iap".to_string(),
            "--quiet".to_string(),
        ];
        argv.extend(gcloud_common(acct));
        argv.push("--".to_string());
        argv.extend(
            [
                "-N",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "ServerAliveCountMax=3",
                // gcloud's --verbosity governs gcloud, not the ssh it execs.
                // At the default level ssh says nothing once connected, so a
                // healthy jump tunnel's log was empty; VERBOSE reports the
                // forward being established and each channel opened on it
                "-o",
                "LogLevel=VERBOSE",
                "-L",
            ]
            .into_iter()
            .map(String::from),
        );
        argv.push(format!("{local_port}:{host}:{remote_port}"));
        return Ok(argv);
    }

    let mut argv = vec!["compute".to_string(), "start-iap-tunnel".to_string()];
    match target {
        // Answered above, before the IAP forward is built
        AccessTarget::Jump { .. } => unreachable!("a jump tunnel is an ssh forward"),
        AccessTarget::Instance { instance, zone } => {
            argv.push(instance.clone());
            argv.push(remote_port.to_string());
            argv.push(format!("--local-host-port=localhost:{local_port}"));
            argv.push(format!("--zone={zone}"));
        }
        AccessTarget::DestGroup {
            host,
            region,
            group,
            network,
        } => {
            argv.push(host.clone());
            argv.push(remote_port.to_string());
            argv.push(format!("--local-host-port=localhost:{local_port}"));
            argv.push(format!("--region={region}"));
            argv.push(format!("--network={network}"));
            argv.push(format!("--dest-group={group}"));
        }
    }
    argv.extend(gcloud_common(acct));
    Ok(argv)
}

// `gcloud compute ssh --tunnel-through-iap` argv that opens the reverse
// forward from the relay VM back to the local service
pub fn expose_argv(acct: &Account, ex: &ExposeConfig, t: &PersistentTunnel) -> Result<Vec<String>> {
    let (local_target, relay_port) = match &t.spec {
        TunnelSpec::Expose {
            local_target,
            relay_port,
            ..
        } => (local_target, *relay_port),
        TunnelSpec::Access { .. } => {
            bail!(
                "Tunnel '{}' is an Access tunnel, not an Expose tunnel",
                t.name
            )
        }
    };
    let normalized = normalize_local_target(local_target)?;

    let mut argv = vec![
        "compute".to_string(),
        "ssh".to_string(),
        ex.relay_instance.clone(),
        format!("--zone={}", ex.relay_zone),
        "--tunnel-through-iap".to_string(),
        "--quiet".to_string(),
    ];
    argv.extend(gcloud_common(acct));
    argv.push("--".to_string());
    argv.extend(
        [
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-R",
        ]
        .into_iter()
        .map(String::from),
    );
    argv.push(format!("0.0.0.0:{relay_port}:{normalized}"));
    Ok(argv)
}

// Turn what a user types for a local service into host:port
pub fn normalize_local_target(target: &str) -> Result<String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        bail!("Empty local target: use a port (3000) or host:port (localhost:3000)");
    }
    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    let without_path = without_scheme
        .split_once('/')
        .map(|(host, _)| host)
        .unwrap_or(without_scheme);

    if let Ok(port) = without_path.parse::<u16>() {
        if port == 0 {
            bail!("Invalid local target '{target}': port 0 is not a service");
        }
        return Ok(format!("localhost:{port}"));
    }
    match without_path.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => {
            Ok(format!("{host}:{port}"))
        }
        _ => bail!(
            "Invalid local target '{target}': use a port (3000) or host:port (localhost:3000)"
        ),
    }
}

// Turn a gcloud or ssh log line into something the user can act on. The log is
// the only place these failures surface, and the raw text names none of the fixes.
pub fn diagnose_log_line(line: &str) -> Option<&'static str> {
    const TABLE: &[(&str, &str)] = &[
        (
            "4003",
            "IAP cannot reach the target port. Allow 35.235.240.0/20 to it in a firewall rule \
             (run: gtunnel doctor).",
        ),
        (
            "4033",
            "Not authorized for IAP tunnelling. Grant roles/iap.tunnelResourceAccessor.",
        ),
        (
            "Reauthentication required",
            "gcloud credentials expired. Run: gcloud auth login",
        ),
        (
            "gcloud auth login",
            "gcloud credentials expired. Run: gcloud auth login",
        ),
        (
            "Address already in use",
            "Local port is already used by another process.",
        ),
        (
            "remote port forwarding failed",
            "Relay port is busy on the relay VM. Restart the tunnel or run gtunnel doctor.",
        ),
        (
            "numpy",
            "Install NumPy for gcloud to speed up IAP tunnels (see docs/troubleshooting.md).",
        ),
    ];
    TABLE
        .iter()
        .find(|(needle, _)| line.contains(needle))
        .map(|(_, hint)| *hint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Account, ExposeConfig};
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec};

    fn account() -> Account {
        Account {
            name: "dev".to_string(),
            gcloud_configuration: "work".to_string(),
            project: "proj-1".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            expose: None,
        }
    }

    fn expose_config() -> ExposeConfig {
        ExposeConfig {
            relay_instance: "gt-dev-relay".to_string(),
            relay_zone: "europe-west1-b".to_string(),
            network: "default".to_string(),
            default_domain: "example.com".to_string(),
            domains: vec![],
        }
    }

    fn instance_tunnel() -> PersistentTunnel {
        PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "my-vm".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        }
    }

    fn dest_group_tunnel() -> PersistentTunnel {
        PersistentTunnel {
            name: "pg".to_string(),
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

    // What k9s needs: the cluster master, reached from the bastion
    fn jump_tunnel() -> PersistentTunnel {
        PersistentTunnel {
            name: "k8s-dev".to_string(),
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

    fn expose_tunnel(local_target: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: "api".to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Expose {
                local_target: local_target.to_string(),
                domain: "example.com".to_string(),
                hostname: "api.example.com".to_string(),
                relay_port: 30123,
                private: false,
            },
        }
    }

    #[test]
    fn access_argv_for_an_instance_with_impersonation() {
        let mut acct = account();
        acct.impersonate_service_account = Some("sa@proj-1.iam.gserviceaccount.com".to_string());
        let argv = access_argv(&acct, &instance_tunnel()).expect("argv");
        assert_eq!(
            argv,
            vec![
                "compute",
                "start-iap-tunnel",
                "my-vm",
                "5432",
                "--local-host-port=localhost:15432",
                "--zone=europe-west1-b",
                "--configuration=work",
                "--project=proj-1",
                "--verbosity=info",
                "--impersonate-service-account=sa@proj-1.iam.gserviceaccount.com",
            ]
        );
    }

    // gcloud's --verbosity governs gcloud, not the ssh it execs, and a jump
    // tunnel is ssh. Without this a healthy --via tunnel still logs nothing.
    #[test]
    fn a_jump_tunnel_asks_ssh_to_say_when_it_forwards() {
        let argv = access_argv(&account(), &jump_tunnel()).expect("argv");
        let i = argv
            .iter()
            .position(|a| a == "LogLevel=VERBOSE")
            .expect("ssh is silent by default, so a healthy --via tunnel logs nothing");
        assert_eq!(argv[i - 1], "-o", "{argv:?}");
    }

    // The lines that say a client connected are log.info in iap_tunnel.py,
    // and gcloud's default verbosity is warning, so a tunnel carrying
    // traffic printed nothing at all.
    #[test]
    fn every_gcloud_command_asks_for_its_connection_lines() {
        for t in [instance_tunnel(), jump_tunnel()] {
            let argv = access_argv(&account(), &t).expect("argv");
            assert!(argv.contains(&"--verbosity=info".to_string()), "{argv:?}");
        }
    }

    #[test]
    fn access_argv_for_a_dest_group_has_no_zone() {
        let argv = access_argv(&account(), &dest_group_tunnel()).expect("argv");
        assert!(argv.contains(&"--dest-group=prod".to_string()), "{argv:?}");
        assert!(
            argv.contains(&"--region=europe-west1".to_string()),
            "{argv:?}"
        );
        assert!(argv.contains(&"--network=default".to_string()), "{argv:?}");
        assert!(
            !argv.iter().any(|a| a.starts_with("--zone=")),
            "dest-group tunnels are regional: {argv:?}"
        );
    }

    #[test]
    fn access_argv_rejects_an_expose_tunnel() {
        let err = access_argv(&account(), &expose_tunnel("3000")).expect_err("should fail");
        assert!(err.to_string().contains("Expose"), "got: {err}");
    }

    #[test]
    fn expose_argv_normalizes_a_bare_port() {
        let argv = expose_argv(&account(), &expose_config(), &expose_tunnel("3000")).expect("argv");
        assert_eq!(
            argv.last().expect("last"),
            "0.0.0.0:30123:localhost:3000",
            "{argv:?}"
        );
        assert_eq!(argv[argv.len() - 2], "-R");
    }

    #[test]
    fn expose_argv_strips_a_scheme() {
        let argv = expose_argv(
            &account(),
            &expose_config(),
            &expose_tunnel("http://localhost:8080"),
        )
        .expect("argv");
        assert_eq!(argv.last().expect("last"), "0.0.0.0:30123:localhost:8080");
    }

    #[test]
    fn expose_argv_runs_ssh_through_iap_with_keepalives() {
        let argv = expose_argv(&account(), &expose_config(), &expose_tunnel("3000")).expect("argv");
        assert_eq!(argv[0], "compute");
        assert_eq!(argv[1], "ssh");
        assert_eq!(argv[2], "gt-dev-relay");
        assert!(
            argv.contains(&"--tunnel-through-iap".to_string()),
            "{argv:?}"
        );
        assert!(
            argv.contains(&"--zone=europe-west1-b".to_string()),
            "{argv:?}"
        );
        assert!(argv.contains(&"--".to_string()), "{argv:?}");
        assert!(argv.contains(&"-N".to_string()), "{argv:?}");
        assert!(
            argv.contains(&"ExitOnForwardFailure=yes".to_string()),
            "{argv:?}"
        );
        assert!(
            argv.contains(&"ServerAliveInterval=30".to_string()),
            "{argv:?}"
        );
    }

    #[test]
    fn expose_argv_rejects_an_access_tunnel() {
        let err =
            expose_argv(&account(), &expose_config(), &instance_tunnel()).expect_err("should fail");
        assert!(err.to_string().contains("Access"), "got: {err}");
    }

    #[test]
    fn normalize_local_target_handles_the_shapes_users_type() {
        assert_eq!(normalize_local_target("3000").unwrap(), "localhost:3000");
        assert_eq!(
            normalize_local_target("localhost:3000").unwrap(),
            "localhost:3000"
        );
        assert_eq!(
            normalize_local_target("http://127.0.0.1:8080").unwrap(),
            "127.0.0.1:8080"
        );
        assert_eq!(
            normalize_local_target("https://localhost:8443/").unwrap(),
            "localhost:8443"
        );
        assert!(normalize_local_target("").is_err());
        assert!(normalize_local_target("localhost").is_err());
    }

    // Review Focus 1 and 3: the log is the only place these failures surface
    #[test]
    fn diagnose_log_line_explains_the_failures_users_hit() {
        let cases = [
            ("ERROR: (gcloud.compute.start-iap-tunnel) Error while connecting [4003: 'failed to connect to backend'].",
             "35.235.240.0/20"),
            ("Error while connecting [4033: 'not authorized'].", "roles/iap.tunnelResourceAccessor"),
            ("Reauthentication required, please rerun gcloud auth login", "gcloud auth login"),
            ("bind: Address already in use", "another process"),
            ("Warning: remote port forwarding failed for listen port 30123", "relay"),
            ("WARNING: Python 3.12 lacks numpy, falling back to a slower implementation", "NumPy"),
        ];
        for (line, expected) in cases {
            let hint = diagnose_log_line(line).unwrap_or_else(|| panic!("no hint for: {line}"));
            assert!(
                hint.contains(expected),
                "line {line:?}\n  hint {hint:?}\n  expected to contain {expected:?}"
            );
        }
    }

    #[test]
    fn diagnose_log_line_is_quiet_about_ordinary_lines() {
        assert!(diagnose_log_line("Testing if tunnel connection works.").is_none());
        assert!(diagnose_log_line("Listening on port [15432].").is_none());
        assert!(diagnose_log_line("").is_none());
    }

    #[test]
    fn parse_configurations_reads_the_gcloud_shape() {
        let json = r#"[
          {"name":"default","is_active":false,
           "properties":{"core":{"project":"other-proj","account":"a@example.com"}}},
          {"name":"work","is_active":true,
           "properties":{"core":{"project":"proj-1","account":"me@example.com"},
                         "compute":{"zone":"europe-west1-b","region":"europe-west1"}}}
        ]"#;
        let configs = crate::gcp::gcloud::parse_configurations(json).expect("parse");
        assert_eq!(configs.len(), 2);
        let active = configs.iter().find(|c| c.is_active).expect("active");
        assert_eq!(active.name, "work");
        assert_eq!(active.project.as_deref(), Some("proj-1"));
        assert_eq!(active.zone.as_deref(), Some("europe-west1-b"));
        assert_eq!(active.account.as_deref(), Some("me@example.com"));
        let inactive = configs.iter().find(|c| !c.is_active).expect("inactive");
        assert_eq!(inactive.zone, None);
    }

    // Real gcloud emits null for every unset property and many extra sections
    #[test]
    fn parse_configurations_reads_real_gcloud_output() {
        let json = include_str!("../tests/fixtures/gcloud_configurations.json");
        let configs = crate::gcp::gcloud::parse_configurations(json).expect("parse");
        assert!(!configs.is_empty(), "fixture should hold a configuration");
        let active = configs
            .iter()
            .find(|c| c.is_active)
            .expect("an active configuration");
        assert_eq!(active.name, "default");
        assert_eq!(active.account.as_deref(), Some("me@example.com"));
    }

    // An IAP TCP forward lands on a port of the instance itself, so a private
    // endpoint behind a bastion is out of its reach: the forward has to be made
    // by ssh on the far side, with IAP carrying the ssh connection
    #[test]
    fn access_argv_for_a_jump_host_forwards_from_the_far_side() {
        let argv = access_argv(&account(), &jump_tunnel()).expect("argv");
        assert_eq!(argv[0], "compute", "{argv:?}");
        assert_eq!(argv[1], "ssh", "{argv:?}");
        assert_eq!(argv[2], "bastion", "{argv:?}");
        assert!(
            argv.contains(&"--tunnel-through-iap".to_string()),
            "{argv:?}"
        );
        assert!(
            argv.contains(&"--zone=europe-west1-b".to_string()),
            "{argv:?}"
        );
        let dash = argv.iter().position(|a| a == "--").expect("an ssh argv");
        assert!(
            argv[dash..].contains(&"-L".to_string())
                && argv.last() == Some(&"8443:10.0.0.10:443".to_string()),
            "the forward is local, from the local port to the host the bastion reaches: {argv:?}"
        );
        assert!(
            argv[dash..].contains(&"ExitOnForwardFailure=yes".to_string()),
            "a forward that cannot bind must fail loudly: {argv:?}"
        );
    }

    #[test]
    fn parse_configurations_tolerates_missing_properties() {
        let configs =
            crate::gcp::gcloud::parse_configurations(r#"[{"name":"bare"}]"#).expect("parse");
        assert_eq!(configs[0].name, "bare");
        assert!(!configs[0].is_active);
        assert_eq!(configs[0].project, None);
    }
}
