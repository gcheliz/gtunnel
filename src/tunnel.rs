use crate::error::GtunnelError;
use crate::failure::Failure;
use anyhow::{bail, Result};

use crate::config::{Account, ExposeConfig};
use crate::loglevel::LogLevel;
use crate::state::{
    validate_instance_name, validate_target, validate_zone, AccessTarget, PersistentTunnel,
    TunnelSpec,
};

// Flags every gcloud invocation carries, so a tunnel never depends on which
// gcloud configuration happens to be active in the shell
pub fn gcloud_common(acct: &Account, level: LogLevel) -> Result<Vec<String>> {
    let mut args = vec![
        format!("--configuration={}", acct.gcloud_configuration),
        format!("--project={}", acct.project),
        // The per-connection lines in gcloud's iap_tunnel are log.info, and
        // gcloud's own default is warning, so a tunnel carrying traffic
        // logged nothing to say so. Which level that is now comes from
        // `LogLevel`, which maps it for ssh in the same breath.
        format!("--verbosity={}", level.gcloud_verbosity()),
    ];
    if let Some(sa) = &acct.impersonate_service_account {
        args.push(format!("--impersonate-service-account={sa}"));
    }
    // A hidden global flag, so one place covers the token probe, the tunnel,
    // compute ssh (gcloud hands it to ssh's inner ProxyCommand) and the argv
    // written to a service file. One argv element: the path may hold spaces.
    // Checked here and not at load, so a bad path stops this account's
    // commands with a usage error and leaves every other account usable
    if let Some(file) = acct.credential_file_for_use()? {
        args.push(format!("--credential-file-override={}", file.display()));
    }
    Ok(args)
}

// `gcloud compute start-iap-tunnel` argv for an Access tunnel
pub fn access_argv(acct: &Account, t: &PersistentTunnel, level: LogLevel) -> Result<Vec<String>> {
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

    // Whatever loaded or built this tunnel, nothing reaches argv unchecked
    validate_target(target)?;

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
        argv.extend(gcloud_common(acct, level)?);
        argv.push("--".to_string());
        argv.extend(ssh_forward_options(level, "-L"));
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
    argv.extend(gcloud_common(acct, level)?);
    Ok(argv)
}

// The ssh options both forwarding tunnels carry, ending with the forward flag
// itself so the caller only has to append its spec.
//
// gcloud's --verbosity governs gcloud, not the ssh it execs, so ssh needs the
// level spelled in its own scale. `LogLevel::ssh_log_level` is where that
// translation lives and why `info` reaches ssh as DEBUG.
//
// ConnectTimeout is here because ServerAlive* only starts after authentication:
// through gcloud's ProxyCommand nothing else bounds a banner exchange that
// stalls behind IAP. The watchdog rescues a jump tunnel with a port probe but
// skips expose tunnels, so for those this option is the only bound.
fn ssh_forward_options(level: LogLevel, forward_flag: &str) -> Vec<String> {
    let mut opts: Vec<String> = [
        "-N",
        "-o",
        "ConnectTimeout=30",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    opts.push(format!("LogLevel={}", level.ssh_log_level()));
    opts.push(forward_flag.to_string());
    opts
}

// `gcloud compute ssh --tunnel-through-iap` argv that opens the reverse
// forward from the relay instance back to the local service
pub fn expose_argv(
    acct: &Account,
    ex: &ExposeConfig,
    t: &PersistentTunnel,
    level: LogLevel,
) -> Result<Vec<String>> {
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
    validate_instance_name(&ex.relay_instance)?;
    validate_zone(&ex.relay_zone)?;

    let mut argv = vec![
        "compute".to_string(),
        "ssh".to_string(),
        ex.relay_instance.clone(),
        format!("--zone={}", ex.relay_zone),
        "--tunnel-through-iap".to_string(),
        "--quiet".to_string(),
    ];
    argv.extend(gcloud_common(acct, level)?);
    argv.push("--".to_string());
    argv.extend(ssh_forward_options(level, "-R"));
    argv.push(format!("0.0.0.0:{relay_port}:{normalized}"));
    Ok(argv)
}

// Turn what a user types for a local service into host:port. A bad target is a
// usage refusal, not an internal error, so `expose` and `run --public` can check
// it before the first GCP call and exit 2
pub fn normalize_local_target(target: &str) -> Result<String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err(GtunnelError::usage(
            "Empty local target: use a port (3000) or host:port (localhost:3000)",
        )
        .into());
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
            return Err(GtunnelError::usage(format!(
                "Invalid local target '{target}': port 0 is not a service"
            ))
            .into());
        }
        return Ok(format!("localhost:{port}"));
    }
    match without_path.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => {
            if port.parse::<u16>() == Ok(0) {
                return Err(GtunnelError::usage(format!(
                    "Invalid local target '{target}': port 0 is not a service"
                ))
                .into());
            }
            Ok(format!("{host}:{port}"))
        }
        _ => Err(GtunnelError::usage(format!(
            "Invalid local target '{target}': use a port (3000) or host:port (localhost:3000)"
        ))
        .into()),
    }
}

// Turn a gcloud or ssh log line into something the user can act on. The log is
// the only place these failures surface, and the raw text names none of the fixes.
//
// The credential rows are not listed here: `Failure::from_line` is the one
// table that says what a line means, and this asks it. `credential_file` is
// whether the account signs in with a key, because the hint for an expired
// login is `gcloud auth login`, which would sign in a person instead of the key.
pub fn diagnose_log_line(line: &str, credential_file: bool) -> Option<String> {
    // [4033: before the credential rows: IAP refusing an identity that logged
    // in fine is not a login problem, and no login line may stand in for it
    if let Some(failure) = Failure::from_line(line)
        && failure.is_stopping()
        && !line.contains("[4033:")
    {
        return failure.log_hint(credential_file);
    }
    if let Some(failure) = Failure::from_line(line)
        && failure == Failure::ListenerBusy
    {
        return failure.log_hint(credential_file);
    }
    const TABLE: &[(&str, &str)] = &[
        (
            "[4003:",
            "IAP cannot reach the target port. Allow 35.235.240.0/20 to it in a firewall rule \
             (run: gtunnel doctor).",
        ),
        (
            "[4033:",
            "Not authorized for IAP tunnelling. Grant roles/iap.tunnelResourceAccessor.",
        ),
        (
            "Address already in use",
            "Local port is already used by another process.",
        ),
        (
            "Too many open files",
            "The tunnel ran out of file descriptors: something is holding its connections \
             open. Run: gtunnel doctor to name the process.",
        ),
        (
            crate::failure::NUMPY_WARNING,
            "gcloud is running this tunnel without NumPy, which slows large transfers. Run: \
             gtunnel doctor for the fix.",
        ),
        (
            "[4004:",
            "IAP asked gcloud to reauthenticate; gcloud resumes the session on its own. If it \
             repeats, check the account's credentials (run: gtunnel doctor).",
        ),
        (
            "Unable to reconnect within",
            "gcloud gave up reconnecting to IAP after 15 minutes; clients on this tunnel were \
             cut. See Limits in docs/architecture.md.",
        ),
    ];
    TABLE
        .iter()
        .find(|(needle, _)| line.contains(needle))
        .map(|(_, hint)| (*hint).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Account, ExposeConfig};
    use crate::loglevel::LogLevel;
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec};

    fn account() -> Account {
        Account {
            name: "dev".to_string(),
            gcloud_configuration: "work".to_string(),
            project: "proj-1".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            credential_file: None,
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
                    instance: "bastion".to_string(),
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
    fn access_argv_refuses_a_value_gcloud_would_read_as_a_flag() {
        let mut t = instance_tunnel();
        t.spec = TunnelSpec::Access {
            target: AccessTarget::Instance {
                instance: "--command=id".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 22,
            local_port: 15432,
        };
        let err = access_argv(&account(), &t, LogLevel::Quiet).unwrap_err();
        assert!(err.to_string().contains("--command=id"), "{err}");

        let mut t = jump_tunnel();
        t.spec = TunnelSpec::Access {
            target: AccessTarget::Jump {
                instance: "bastion".to_string(),
                zone: "europe-west1-b".to_string(),
                host: "-oProxyCommand=id".to_string(),
            },
            remote_port: 443,
            local_port: 8443,
        };
        assert!(access_argv(&account(), &t, LogLevel::Quiet).is_err());
    }

    #[test]
    fn expose_argv_refuses_a_relay_that_gcloud_would_read_as_a_flag() {
        let mut ex = expose_config();
        ex.relay_instance = "--command=id".to_string();
        let err = expose_argv(&account(), &ex, &expose_tunnel("3000"), LogLevel::Quiet);
        assert!(err.unwrap_err().to_string().contains("--command=id"));
    }

    // gcloud's scale, asked for by the level rather than written at the
    // call site, so the two tools cannot drift apart
    #[test]
    fn an_access_tunnel_carries_the_levels_verbosity() {
        // Both shapes `access_argv` builds without a jump host. A dest group
        // takes a different branch to the same `gcloud_common`, and a guard
        // that covers only the instance shape would not notice if it stopped.
        for tunnel in [instance_tunnel(), dest_group_tunnel()] {
            for level in [LogLevel::Quiet, LogLevel::Info, LogLevel::Debug] {
                let argv = access_argv(&account(), &tunnel, level).expect("argv");
                let want = format!("--verbosity={}", level.gcloud_verbosity());
                assert!(argv.contains(&want), "{level}: {argv:?}");
                assert_eq!(
                    argv.iter()
                        .filter(|a| a.starts_with("--verbosity="))
                        .count(),
                    1,
                    "one verbosity, not two: {argv:?}"
                );
            }
        }
    }

    // Before the separator gcloud takes the flag and rejects it
    #[test]
    fn a_jump_tunnel_carries_the_levels_ssh_level_after_the_separator() {
        for level in [LogLevel::Quiet, LogLevel::Info, LogLevel::Debug] {
            let argv = access_argv(&account(), &jump_tunnel(), level).expect("argv");
            assert_ssh_level(&argv, level);
        }
    }

    #[test]
    fn a_public_tunnel_carries_the_levels_ssh_level_after_the_separator() {
        for level in [LogLevel::Quiet, LogLevel::Info, LogLevel::Debug] {
            let argv = expose_argv(&account(), &expose_config(), &expose_tunnel("3000"), level)
                .expect("argv");
            assert_ssh_level(&argv, level);
        }
    }

    fn assert_ssh_level(argv: &[String], level: LogLevel) {
        let want = format!("LogLevel={}", level.ssh_log_level());
        let sep = argv
            .iter()
            .position(|a| a == "--")
            .expect("ssh options live after the -- separator");
        let i = argv
            .iter()
            .position(|a| *a == want)
            .unwrap_or_else(|| panic!("{level}: no {want} in {argv:?}"));
        assert_eq!(argv[i - 1], "-o", "{argv:?}");
        assert!(
            i > sep,
            "before the separator gcloud takes the flag and rejects it\n{argv:?}"
        );
    }

    // The guard this whole change exists for: introducing the knob must not
    // move a single byte of what the tunnels run today. Every argv below is
    // what the builders produced before the level was a parameter.
    #[test]
    fn the_default_level_leaves_every_argv_byte_identical_to_today() {
        let d = LogLevel::default();

        assert_eq!(
            access_argv(&account(), &instance_tunnel(), d).expect("instance"),
            vec![
                "compute",
                "start-iap-tunnel",
                "bastion",
                "5432",
                "--local-host-port=localhost:15432",
                "--zone=europe-west1-b",
                "--configuration=work",
                "--project=proj-1",
                "--verbosity=info",
            ]
        );

        assert_eq!(
            access_argv(&account(), &dest_group_tunnel(), d).expect("dest group"),
            vec![
                "compute",
                "start-iap-tunnel",
                "10.0.0.5",
                "5432",
                "--local-host-port=localhost:15001",
                "--region=europe-west1",
                "--network=default",
                "--dest-group=prod",
                "--configuration=work",
                "--project=proj-1",
                "--verbosity=info",
            ]
        );

        assert_eq!(
            access_argv(&account(), &jump_tunnel(), d).expect("jump"),
            vec![
                "compute",
                "ssh",
                "bastion",
                "--zone=europe-west1-b",
                "--tunnel-through-iap",
                "--quiet",
                "--configuration=work",
                "--project=proj-1",
                "--verbosity=info",
                "--",
                "-N",
                "-o",
                "ConnectTimeout=30",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "LogLevel=DEBUG",
                "-L",
                "8443:10.0.0.10:443",
            ]
        );

        assert_eq!(
            expose_argv(&account(), &expose_config(), &expose_tunnel("3000"), d).expect("expose"),
            vec![
                "compute",
                "ssh",
                "gt-dev-relay",
                "--zone=europe-west1-b",
                "--tunnel-through-iap",
                "--quiet",
                "--configuration=work",
                "--project=proj-1",
                "--verbosity=info",
                "--",
                "-N",
                "-o",
                "ConnectTimeout=30",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "LogLevel=DEBUG",
                "-R",
                "0.0.0.0:30123:localhost:3000",
            ]
        );
    }

    #[test]
    fn access_argv_for_an_instance_with_impersonation() {
        let mut acct = account();
        acct.impersonate_service_account = Some("sa@proj-1.iam.gserviceaccount.com".to_string());
        let argv = access_argv(&acct, &instance_tunnel(), LogLevel::Info).expect("argv");
        assert_eq!(
            argv,
            vec![
                "compute",
                "start-iap-tunnel",
                "bastion",
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

    #[test]
    fn gcloud_common_carries_the_credential_file_only_when_set() {
        let mut acct = account();
        assert!(!gcloud_common(&acct, LogLevel::Quiet)
            .expect("args")
            .iter()
            .any(|a| a.starts_with("--credential-file-override")));
        acct.credential_file = Some("/secrets/ci key.json".into());
        let args = gcloud_common(&acct, LogLevel::Quiet).expect("args");
        // One argv element, spaces and all: nothing here goes through a shell
        assert!(args.contains(&"--credential-file-override=/secrets/ci key.json".to_string()));
    }

    #[test]
    fn a_relative_credential_file_stops_the_tunnel_with_a_usage_error() {
        let mut acct = account();
        acct.credential_file = Some("~/ci-key.json".into());
        let err = gcloud_common(&acct, LogLevel::Quiet).expect_err("relative");
        assert_eq!(
            crate::error::kind_of(&err),
            crate::error::ErrorKind::Usage,
            "{err:#}"
        );
        let err = access_argv(&acct, &instance_tunnel(), LogLevel::Info).expect_err("relative");
        assert!(format!("{err:#}").contains("absolute"), "{err:#}");
    }

    #[test]
    fn every_tunnel_kind_carries_the_credential_file_when_set() {
        let mut acct = account();
        acct.credential_file = Some("/secrets/ci-key.json".into());
        acct.expose = Some(expose_config());
        let flag = "--credential-file-override=/secrets/ci-key.json".to_string();
        for t in [instance_tunnel(), dest_group_tunnel(), jump_tunnel()] {
            let argv = access_argv(&acct, &t, LogLevel::Info).expect("argv");
            assert!(argv.contains(&flag), "{}: {argv:?}", t.name);
        }
    }

    // Both tunnel kinds that run `gcloud compute ssh` need it. gcloud's
    // --verbosity governs gcloud, not the ssh it execs.
    //
    // The level is DEBUG, not VERBOSE. ssh's levels run
    // QUIET < FATAL < ERROR < INFO < VERBOSE < DEBUG, and every line worth
    // having here - the forward being established, and one per client
    // connection - is a debug() message. At VERBOSE ssh prints three lines
    // for the whole session and nothing per connection, which is the silence
    // this exists to end.
    #[test]
    fn an_ssh_tunnel_asks_for_the_level_that_reports_forwarding() {
        for argv in [
            access_argv(&account(), &jump_tunnel(), LogLevel::Info).expect("jump"),
            expose_argv(
                &account(),
                &expose_config(),
                &expose_tunnel("3000"),
                LogLevel::Info,
            )
            .expect("expose"),
        ] {
            let sep = argv
                .iter()
                .position(|a| a == "--")
                .expect("ssh options live after the -- separator");
            let i = argv
                .iter()
                .position(|a| a == "LogLevel=DEBUG")
                .unwrap_or_else(|| panic!("ssh says nothing per connection below DEBUG\n{argv:?}"));
            assert_eq!(argv[i - 1], "-o", "{argv:?}");
            assert!(
                i > sep,
                "before the separator gcloud takes the flag and rejects it\n{argv:?}"
            );
        }
    }

    // The lines that say a client connected are log.info in iap_tunnel.py,
    // and gcloud's default verbosity is warning, so a tunnel carrying
    // traffic printed nothing at all.
    #[test]
    fn every_gcloud_command_asks_for_its_connection_lines() {
        let argvs = [
            access_argv(&account(), &instance_tunnel(), LogLevel::Info).expect("instance"),
            access_argv(&account(), &jump_tunnel(), LogLevel::Info).expect("jump"),
            expose_argv(
                &account(),
                &expose_config(),
                &expose_tunnel("3000"),
                LogLevel::Info,
            )
            .expect("expose"),
        ];
        for argv in argvs {
            assert!(argv.contains(&"--verbosity=info".to_string()), "{argv:?}");
        }
    }

    #[test]
    fn access_argv_for_a_dest_group_has_no_zone() {
        let argv = access_argv(&account(), &dest_group_tunnel(), LogLevel::Info).expect("argv");
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
        let err = access_argv(&account(), &expose_tunnel("3000"), LogLevel::Info)
            .expect_err("should fail");
        assert!(err.to_string().contains("Expose"), "got: {err}");
    }

    #[test]
    fn expose_argv_normalizes_a_bare_port() {
        let argv = expose_argv(
            &account(),
            &expose_config(),
            &expose_tunnel("3000"),
            LogLevel::Info,
        )
        .expect("argv");
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
            LogLevel::Info,
        )
        .expect("argv");
        assert_eq!(argv.last().expect("last"), "0.0.0.0:30123:localhost:8080");
    }

    #[test]
    fn expose_argv_runs_ssh_through_iap_with_keepalives() {
        let argv = expose_argv(
            &account(),
            &expose_config(),
            &expose_tunnel("3000"),
            LogLevel::Info,
        )
        .expect("argv");
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
    fn forward_options_bound_the_handshake() {
        // ServerAlive* only starts after authentication, so through a
        // ProxyCommand ConnectTimeout alone bounds a stalled banner exchange.
        let jump = access_argv(&account(), &jump_tunnel(), LogLevel::Info).expect("jump");
        let expose = expose_argv(
            &account(),
            &expose_config(),
            &expose_tunnel("3000"),
            LogLevel::Info,
        )
        .expect("expose");
        for argv in [jump, expose] {
            assert!(argv.contains(&"ConnectTimeout=30".to_string()), "{argv:?}");
        }
    }

    #[test]
    fn expose_argv_rejects_an_access_tunnel() {
        let err = expose_argv(
            &account(),
            &expose_config(),
            &instance_tunnel(),
            LogLevel::Info,
        )
        .expect_err("should fail");
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
        let zero = normalize_local_target("localhost:0")
            .unwrap_err()
            .to_string();
        assert!(zero.contains("port 0 is not a service"), "{zero}");
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
            ("Warning: remote port forwarding failed for listen port 30123", "old session"),
            // What gtunnel's own tunnels actually print: every one of them
            // sets ExitOnForwardFailure=yes, and OpenSSH spells the same
            // failure `Error:` and makes it fatal when that option is on
            ("Error: remote port forwarding failed for listen port 30123", "old session"),
            // What gcloud 590 prints, spelled as its source spells it
            ("WARNING: To increase the performance of the tunnel, consider installing NumPy. For instructions,", "NumPy"),
            ("INFO: [3] Received WebSocket Close message [4004: 'reauthentication required'].", "resumes the session"),
            ("WARNING: [3] Unable to reconnect within [900000] ms", "15 minutes"),
        ];
        for (line, expected) in cases {
            let hint =
                diagnose_log_line(line, false).unwrap_or_else(|| panic!("no hint for: {line}"));
            assert!(
                hint.contains(expected),
                "line {line:?}\n  hint {hint:?}\n  expected to contain {expected:?}"
            );
        }
    }

    // The listener is the victim of this one, not the cause, so the hint has
    // to send the reader somewhere that can name the cause
    #[test]
    fn diagnose_log_line_explains_a_tunnel_out_of_descriptors() {
        let hint = diagnose_log_line("accept: Too many open files", false).expect("a hint");
        assert!(
            hint.contains("descriptor") || hint.contains("connections"),
            "{hint}"
        );
        assert!(hint.contains("gtunnel doctor"), "{hint}");
    }

    #[test]
    fn diagnose_log_line_is_quiet_about_ordinary_lines() {
        assert!(diagnose_log_line("Testing if tunnel connection works.", false).is_none());
        assert!(diagnose_log_line("Listening on port [15432].", false).is_none());
        assert!(diagnose_log_line("", false).is_none());
    }

    // A login would sign in a person, never the key, so a key's account must
    // not be sent to one by any line that earns a hint
    #[test]
    fn a_credential_file_accounts_hint_never_says_to_log_in() {
        let lines = [
            "ERROR: There was a problem refreshing your current auth tokens: Reauthentication required.",
            "Please run: gcloud auth login",
            "invalid_grant: Token has been expired or revoked.",
            "ERROR: Failed to impersonate [runner@example-dev-1a2b.iam.gserviceaccount.com].",
            "invalid_grant: Invalid JWT Signature.",
            "ERROR: The configuration [work] does not exist.",
        ];
        for line in lines {
            let hint = diagnose_log_line(line, true).unwrap_or_else(|| panic!("no hint: {line}"));
            assert!(!hint.contains("auth login"), "{line}\n  {hint}");
        }
        let ordinary = diagnose_log_line("Please run: gcloud auth login", false).expect("a hint");
        assert!(ordinary.contains("gcloud auth login"), "{ordinary}");
    }

    #[test]
    fn a_denied_impersonation_names_the_role_not_a_login() {
        let hint = diagnose_log_line("ERROR: Failed to impersonate [sa@example.invalid].", false)
            .expect("a hint");
        assert!(
            hint.contains("roles/iam.serviceAccountTokenCreator"),
            "{hint}"
        );
        assert!(!hint.contains("auth login"), "{hint}");
    }

    // A port or URL that merely contains the digits is not an IAP close code
    #[test]
    fn diagnose_log_line_matches_close_codes_exactly() {
        for line in [
            "Listening on port [14004].",
            "Listening on port [14003].",
            "Listening on port [14033].",
            "Client closed connection from 127.0.0.1:54004",
            "Reconnecting with URL ['wss://tunnel.cloudproxy.app/v4/reconnect?sid=a4003b']",
        ] {
            assert!(diagnose_log_line(line, false).is_none(), "{line}");
        }
    }

    // gcloud prints this on every call when the configuration was never
    // created; it is noise, not a refusal or an error to explain
    #[test]
    fn a_missing_configuration_warning_is_not_a_refusal_or_a_failure() {
        let line = "WARNING: Could not open the configuration file: [/home/you/.config/gcloud/configurations/config_ci].";
        assert!(diagnose_log_line(line, false).is_none());
        assert!(!crate::probe::is_refusal(line));
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
        let argv = access_argv(&account(), &jump_tunnel(), LogLevel::Info).expect("argv");
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
