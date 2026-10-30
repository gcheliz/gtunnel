use std::collections::HashSet;

// An IAP tunnel running on this machine that gtunnel did not start
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalTunnel {
    pub pid: u32,
    pub target: String,
    pub remote_port: u16,
    pub local_port: Option<u16>,
    pub zone: Option<String>,
}

// The flags of `gcloud compute start-iap-tunnel` and gcloud's global ones that
// take a value. A flag outside this list is read as a switch, so an unknown one
// can never swallow the instance or the port that follows it.
const VALUE_FLAGS: &[&str] = &[
    "--zone",
    "--project",
    "--configuration",
    "--local-host-port",
    "--region",
    "--network",
    "--dest-group",
    "--account",
    "--billing-project",
    "--impersonate-service-account",
    "--access-token-file",
    "--credential-file-override",
    "--verbosity",
    "--format",
    "--flags-file",
    "--iap-tunnel-url-override",
];

// A flag's value in either spelling gcloud accepts, `--flag=value` or `--flag value`
fn flag_value<'a>(tokens: &[&'a str], flag: &str) -> Option<&'a str> {
    tokens.iter().enumerate().find_map(|(i, token)| {
        if *token == flag {
            tokens.get(i + 1).copied()
        } else {
            token.strip_prefix(flag)?.strip_prefix('=')
        }
    })
}

// What is left once every flag, and the value a spaced flag owns, is skipped
fn positionals<'a>(tokens: &[&'a str]) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut iter = tokens.iter();
    while let Some(token) = iter.next() {
        if VALUE_FLAGS.contains(token) {
            iter.next();
        } else if !token.starts_with('-') {
            found.push(*token);
        }
    }
    found
}

// One process as `ps -axo pid=,ppid=,command=` prints it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
}

// Every process on the machine, read once. Discovery used to ask `ps` twice,
// once for the family tree and once for the commands, and a supervisor that
// started its gcloud child between the two calls was in the second answer and
// not the first: the child looked like a stranger for as long as it took the
// next round to notice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub rows: Vec<ProcessRow>,
}

/// `discovery = false`, `GTUNNEL_DISCOVERY=false` or the older
/// `GTUNNEL_NO_DISCOVERY` makes the machine look like it runs no other
/// process. An integration test isolates its config with `GTUNNEL_CONFIG_DIR`,
/// but `ps` reads the whole user: any gcloud tunnel on the host, a
/// maintainer's own or another worktree's test run, would add `pid N` rows and
/// break exact row counts.
pub fn snapshot() -> Snapshot {
    if !crate::settings::current().discovery {
        return Snapshot::default();
    }
    let mut cmd = std::process::Command::new("ps");
    cmd.args(["-axo", "pid=,ppid=,command="]);
    // A timeout is an empty snapshot, as a machine with no `ps` already is
    let Ok(out) = crate::proc::output_within_blocking(cmd, crate::proc::LOCAL_TOOL_BOUND, "ps")
    else {
        return Snapshot::default();
    };
    if !out.status.success() {
        return Snapshot::default();
    }
    parse_snapshot(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_snapshot(ps: &str) -> Snapshot {
    let rows = ps
        .lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (ppid, command) = rest
                .trim_start()
                .split_once(char::is_whitespace)
                .unwrap_or((rest.trim(), ""));
            Some(ProcessRow {
                pid: pid.parse().ok()?,
                ppid: ppid.parse().ok()?,
                command: command.trim().to_string(),
            })
        })
        .collect();
    Snapshot { rows }
}

// The tunnel a process row is, if it is an IAP listener at all
fn iap_tunnel(row: &ProcessRow) -> Option<ExternalTunnel> {
    let line = &row.command;
    if !line.contains("start-iap-tunnel") {
        return None;
    }
    // The grep that looks for these processes matches itself.
    // A command with --listen-on-stdin is an OpenSSH ProxyCommand pipe, not an external port listener.
    if line.contains("grep") || line.contains("--listen-on-stdin") {
        return None;
    }
    let rest: Vec<&str> = line.split_whitespace().collect();
    let position = rest.iter().position(|t| *t == "start-iap-tunnel")?;
    let positional = positionals(&rest[position + 1..]);
    let [target, port] = positional.as_slice() else {
        return None;
    };
    let remote_port = port.parse::<u16>().ok()?;
    let local_port = flag_value(&rest, "--local-host-port")
        .and_then(|value| value.rsplit(':').next())
        .and_then(|port| port.parse::<u16>().ok());
    let zone = flag_value(&rest, "--zone").map(|z| z.to_string());

    Some(ExternalTunnel {
        pid: row.pid,
        target: target.to_string(),
        remote_port,
        local_port,
        zone,
    })
}

// Which services to ask for a running pid: every account's, not only a selected
// one. A tunnel gtunnel started under another account is still gtunnel's, and a
// stranger is told from a managed process by pid alone, so leaving an account
// out listed its tunnels a second time as `pid-NNNNN`.
pub fn managed_labels(state: &crate::state::TunnelState) -> Vec<String> {
    state
        .tunnels
        .iter()
        .map(|t| crate::daemon::platform_label(&t.account_name, &t.name))
        .collect()
}

// The local ports gtunnel answers for, whoever's account holds them and
// whether or not the process is up yet. A run record counts alive or not: its
// port is claimed from the moment it is written. An Expose spec's local port is
// the service being published, not an IAP listener, so it owns nothing here.
pub fn owned_ports(
    state: &crate::state::TunnelState,
    ephemeral: &[(crate::ephemeral::EphemeralRecord, bool)],
) -> HashSet<u16> {
    let mut ports = state.used_local_ports();
    for (record, _) in ephemeral {
        if let crate::state::TunnelSpec::Access { local_port, .. } = &record.spec {
            ports.insert(*local_port);
        }
    }
    ports
}

// Every IAP tunnel in the snapshot that gtunnel does not manage. A process on
// an owned port is gtunnel's before any pid is asked: launchd reports the
// supervisor's pid before the supervisor has spawned its gcloud child, and a
// test that needs the pid tree to be complete is a test of the clock.
pub fn classify_with_pids(
    snapshot: &Snapshot,
    owned: &HashSet<u16>,
    root_pids: &HashSet<u32>,
) -> Vec<ExternalTunnel> {
    // A service's own transport processes answer to it
    let family = descendants(snapshot, root_pids);
    snapshot
        .rows
        .iter()
        .filter(|row| !family.contains(&row.pid))
        .filter_map(iap_tunnel)
        .filter(|tunnel| tunnel.local_port.is_none_or(|port| !owned.contains(&port)))
        .collect()
}

// Every IAP tunnel on this machine that gtunnel does not manage: the services
// it installed, whatever account owns them, and the ephemeral runs it recorded
pub async fn classify(
    snapshot: &Snapshot,
    state: &crate::state::TunnelState,
) -> Vec<ExternalTunnel> {
    // Unknown, not empty: without the run records no process can be told from
    // a run's own, so every one of them would read as a stranger
    let Ok(ephemeral) = crate::ephemeral::list_ephemeral() else {
        return Vec::new();
    };
    let mut roots = HashSet::new();
    for label in managed_labels(state) {
        if let Some(pid) = crate::daemon::runtime_info(&label).await.pid {
            roots.insert(pid);
        }
    }
    roots.extend(ephemeral.iter().map(|(record, _)| record.pid));
    classify_with_pids(snapshot, &owned_ports(state, &ephemeral), &roots)
}

// Every process below the given roots. A tunnel's transport is the tunnel, not
// a second one: a jump tunnel reaches its target through an ssh child whose own
// ProxyCommand is another gcloud IAP process, and that process answers to the
// service above it.
fn descendants(snapshot: &Snapshot, roots: &HashSet<u32>) -> HashSet<u32> {
    let mut family: HashSet<u32> = roots.clone();
    // The tree is shallow and unordered, so keep sweeping until a pass adds
    // nothing rather than assuming a parent is listed before its child
    loop {
        let before = family.len();
        for row in &snapshot.rows {
            if family.contains(&row.ppid) {
                family.insert(row.pid);
            }
        }
        if family.len() == before {
            return family;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = "\
  501     1 /usr/bin/python3 /opt/homebrew/share/google-cloud-sdk/lib/gcloud.py compute start-iap-tunnel db-1 5432 --local-host-port=localhost:6000 --zone=us-central1-a
  502     1 /usr/bin/python3 /opt/homebrew/share/google-cloud-sdk/lib/gcloud.py compute start-iap-tunnel managed-1 3306 --local-host-port=localhost:15306 --zone=europe-west1-b
  503     1 /usr/bin/grep start-iap-tunnel
  504     1 /bin/zsh
";

    // Tunnels by pid alone, as before ports were known
    fn strangers(ps: &str, managed: &HashSet<u32>) -> Vec<ExternalTunnel> {
        classify_with_pids(&parse_snapshot(ps), &HashSet::new(), managed)
    }

    #[test]
    fn external_tunnels_exclude_the_ones_gtunnel_manages() {
        let managed: HashSet<u32> = [502u32].into_iter().collect();
        let found = strangers(PS, &managed);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, 501);
        assert_eq!(found[0].target, "db-1");
        assert_eq!(found[0].remote_port, 5432);
        assert_eq!(found[0].local_port, Some(6000));
        assert_eq!(found[0].zone.as_deref(), Some("us-central1-a"));
    }

    // The grep that found the processes is not itself a tunnel
    #[test]
    fn the_grep_line_is_not_a_tunnel() {
        let found = strangers(PS, &HashSet::new());
        assert!(found.iter().all(|t| t.pid != 503), "{found:?}");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_tunnel_without_a_local_host_port_still_parses() {
        let ps = "  601     1 gcloud compute start-iap-tunnel db-9 22\n";
        let found = strangers(ps, &HashSet::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].local_port, None);
        assert_eq!(found[0].zone, None);
        assert_eq!(found[0].remote_port, 22);
    }

    #[test]
    fn proxy_commands_with_listen_on_stdin_are_ignored() {
        let ps = "  602     1 python3 /opt/google-cloud-sdk/lib/gcloud.py compute start-iap-tunnel bastion 22 --listen-on-stdin\n";
        let found = strangers(ps, &HashSet::new());
        assert!(
            found.is_empty(),
            "listen-on-stdin proxycommand should not be parsed as an external tunnel: {found:?}"
        );
    }

    fn one(command: &str) -> Vec<ExternalTunnel> {
        strangers(&format!("  700     1 {command}\n"), &HashSet::new())
    }

    #[test]
    fn flags_before_the_positionals_do_not_hide_the_tunnel() {
        let found = one("gcloud compute start-iap-tunnel --zone=europe-west1-b bastion 22");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].target, "bastion");
        assert_eq!(found[0].remote_port, 22);
        assert_eq!(found[0].zone.as_deref(), Some("europe-west1-b"));
    }

    #[test]
    fn a_flag_value_after_a_space_is_read_and_not_taken_for_a_positional() {
        let found = one(
            "gcloud compute start-iap-tunnel bastion 22 --local-host-port localhost:2222 --zone europe-west1-b",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].local_port, Some(2222));
        assert_eq!(found[0].zone.as_deref(), Some("europe-west1-b"));

        let leading = one("gcloud compute start-iap-tunnel --project example-dev-1a2b bastion 22");
        assert_eq!(leading.len(), 1, "{leading:?}");
        assert_eq!(leading[0].target, "bastion");
    }

    // Any placement of the flags, in either spelling, around the two positionals
    // is the same tunnel. The set is small and fixed so a failure names its case.
    #[test]
    fn permuting_the_flags_around_the_positionals_yields_the_same_tunnel() {
        let flags: [&[&str]; 4] = [
            &["--zone=europe-west1-b"],
            &["--local-host-port", "localhost:2222"],
            &["--project", "example-dev-1a2b"],
            &["--quiet"],
        ];
        let expected = one(
            "gcloud compute start-iap-tunnel bastion 22 --zone=europe-west1-b --local-host-port=localhost:2222",
        );
        assert_eq!(expected.len(), 1);

        let n = flags.len();
        // Every order of the four flags, and every split of them into
        // before / between / after the positionals
        let mut orders: Vec<Vec<usize>> = vec![vec![]];
        for _ in 0..n {
            let mut longer = Vec::new();
            for order in &orders {
                for i in (0..n).filter(|i| !order.contains(i)) {
                    let mut next = order.clone();
                    next.push(i);
                    longer.push(next);
                }
            }
            orders = longer;
        }
        assert_eq!(orders.len(), 24);
        for order in &orders {
            for (cut_a, cut_b) in [(0, 0), (0, 2), (2, 2), (1, 3), (4, 4), (0, 4)] {
                let words = |range: std::ops::Range<usize>| -> Vec<&str> {
                    order[range]
                        .iter()
                        .flat_map(|i| flags[*i].iter().copied())
                        .collect()
                };
                let mut argv = vec!["gcloud", "compute", "start-iap-tunnel"];
                argv.extend(words(0..cut_a));
                argv.push("bastion");
                argv.extend(words(cut_a..cut_b));
                argv.push("22");
                argv.extend(words(cut_b..n));
                let found = one(&argv.join(" "));
                assert_eq!(found.len(), 1, "{argv:?}");
                assert_eq!(found[0].target, expected[0].target, "{argv:?}");
                assert_eq!(found[0].remote_port, 22, "{argv:?}");
                assert_eq!(found[0].local_port, Some(2222), "{argv:?}");
                // --zone is among the flags in every case
                assert_eq!(found[0].zone, expected[0].zone, "{argv:?}");
            }
        }
    }

    #[test]
    fn no_processes_means_no_tunnels() {
        assert!(strangers("", &HashSet::new()).is_empty());
    }

    fn tunnel(name: &str, account: &str) -> crate::state::PersistentTunnel {
        crate::state::PersistentTunnel {
            name: name.to_string(),
            account_name: account.to_string(),
            auto_start: false,
            spec: crate::state::TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 22,
                local_port: 8443,
            },
        }
    }

    // A list is account-scoped, the machine is not: a tunnel gtunnel runs for
    // another account owns its pid and its port, and asking only one account's
    // services for a pid left that process looking unmanaged, so the same
    // tunnel appeared twice - once by name, once as `pid-NNNNN`
    #[test]
    fn a_pid_lookup_covers_every_account_not_just_a_selected_one() {
        let state = crate::state::TunnelState {
            tunnels: vec![tunnel("bastion", "dev"), tunnel("bastion", "prod")],
            ..Default::default()
        };
        let labels = managed_labels(&state);
        assert!(
            labels.contains(&crate::daemon::platform_label("prod", "bastion")),
            "another account's service was not asked for its pid: {labels:?}"
        );
        assert_eq!(labels.len(), 2, "every tunnel contributes one lookup");
    }

    // A jump tunnel is a `gcloud compute ssh` wrapper whose ssh child spawns a
    // second gcloud as its ProxyCommand. That grandchild is an IAP tunnel by
    // every pattern `ps` can see, so without the family tree the list showed a
    // managed tunnel's own transport as a stranger beside it
    #[test]
    fn a_managed_pids_descendants_are_managed_too() {
        const TREE: &str = "\
  100     1
  200   100
  300   200
  400     1
  500   400
";
        let found = descendants(&parse_snapshot(TREE), &[100u32].into_iter().collect());
        assert!(found.contains(&200) && found.contains(&300), "{found:?}");
        assert!(
            !found.contains(&400) && !found.contains(&500),
            "another process's family is not ours: {found:?}"
        );
    }

    // A ps snapshot where the gcloud child (pid 702) is present but the
    // supervisor that launchd has not yet reported (pid 701) is not in the
    // managed set.
    const LATE_SUPERVISOR: &str = "\
  701     1 gtunnel __supervise -- gcloud compute start-iap-tunnel bastion 22 --local-host-port=localhost:2222 --zone=europe-west1-b
  702   701 /usr/bin/python3 /opt/homebrew/share/google-cloud-sdk/lib/gcloud.py compute start-iap-tunnel bastion 22 --local-host-port=localhost:2222 --zone=europe-west1-b
  703     1 /usr/bin/python3 /opt/homebrew/share/google-cloud-sdk/lib/gcloud.py compute start-iap-tunnel db-1 5432 --local-host-port=localhost:6000 --zone=us-central1-a
";

    fn state_with(tunnels: &[(&str, &str, u16)]) -> crate::state::TunnelState {
        crate::state::TunnelState {
            tunnels: tunnels
                .iter()
                .map(|(account, name, port)| crate::state::PersistentTunnel {
                    spec: crate::state::TunnelSpec::Access {
                        target: crate::state::AccessTarget::Instance {
                            instance: "bastion".to_string(),
                            zone: "europe-west1-b".to_string(),
                        },
                        remote_port: 22,
                        local_port: *port,
                    },
                    ..tunnel(name, account)
                })
                .collect(),
            ..Default::default()
        }
    }

    fn ephemeral_record(
        account: &str,
        name: &str,
        local_port: u16,
    ) -> Vec<(crate::ephemeral::EphemeralRecord, bool)> {
        vec![(
            crate::ephemeral::EphemeralRecord {
                name: name.to_string(),
                account_name: account.to_string(),
                pid: 9999,
                spec: crate::state::TunnelSpec::Access {
                    target: crate::state::AccessTarget::Instance {
                        instance: "bastion".to_string(),
                        zone: "europe-west1-b".to_string(),
                    },
                    remote_port: 22,
                    local_port,
                },
                started_at: 0,
                ttl_secs: None,
                pid_start: None,
                owner_pid: None,
                owner_start: None,
            },
            true,
        )]
    }

    #[test]
    fn a_tunnel_whose_port_the_state_owns_is_not_a_stranger_even_without_its_pid() {
        let snap = parse_snapshot(LATE_SUPERVISOR);
        let owned = owned_ports(&state_with(&[("dev", "mongodb", 2222)]), &[]);
        let found = classify_with_pids(&snap, &owned, &HashSet::new());
        assert_eq!(found.iter().map(|e| e.pid).collect::<Vec<_>>(), vec![703]);
    }

    #[test]
    fn a_managed_tunnel_of_another_account_is_not_a_stranger() {
        let snap = parse_snapshot(LATE_SUPERVISOR);
        let owned = owned_ports(&state_with(&[("prod", "mongodb", 2222)]), &[]);
        let found = classify_with_pids(&snap, &owned, &HashSet::new());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].target, "db-1");
    }

    #[test]
    fn a_port_owned_by_an_ephemeral_run_is_not_a_stranger() {
        let snap = parse_snapshot(LATE_SUPERVISOR);
        let owned = owned_ports(
            &crate::state::TunnelState::default(),
            &ephemeral_record("dev", "scratch", 2222),
        );
        let found = classify_with_pids(&snap, &owned, &HashSet::new());
        assert_eq!(found.iter().map(|e| e.pid).collect::<Vec<_>>(), vec![703]);
    }

    #[test]
    fn a_stranger_on_an_unowned_port_is_listed_on_first_sight() {
        let snap = parse_snapshot(LATE_SUPERVISOR);
        let owned = owned_ports(&state_with(&[("dev", "mongodb", 2222)]), &[]);
        let found = classify_with_pids(&snap, &owned, &HashSet::new());
        assert_eq!(found.len(), 1, "one round is enough: {found:?}");
        assert_eq!(found[0].local_port, Some(6000));
    }

    #[test]
    fn the_tree_and_the_candidates_come_from_one_snapshot() {
        let snap = parse_snapshot(LATE_SUPERVISOR);
        let managed: HashSet<u32> = [701u32].into_iter().collect();
        let found = classify_with_pids(&snap, &HashSet::new(), &managed);
        assert_eq!(found.iter().map(|e| e.pid).collect::<Vec<_>>(), vec![703]);
    }

    // The relay's local service is not an IAP listener: its port says nothing
    // about which gcloud process is gtunnel's
    #[test]
    fn an_expose_spec_owns_no_iap_port() {
        let mut record = ephemeral_record("dev", "site", 2222);
        record[0].0.spec = crate::state::TunnelSpec::Expose {
            local_target: "localhost:6000".to_string(),
            domain: "dev.example.com".to_string(),
            hostname: "site.dev.example.com".to_string(),
            relay_port: 30001,
            private: false,
        };
        let owned = owned_ports(&crate::state::TunnelState::default(), &record);
        assert!(owned.is_empty(), "{owned:?}");
    }

    // An unreadable run directory says nothing about which tunnels are runs,
    // so listing every IAP process as a stranger would offer to adopt or kill
    // the user's own live runs
    #[test]
    fn an_unreadable_run_directory_lists_nothing_as_unmanaged() {
        let dir = crate::config::testing::TempConfigDir::new();
        // A file where the directory should be: create_dir_all fails on it
        std::fs::write(dir.path().join("run"), "not a directory").unwrap();
        let found = tokio::runtime::Runtime::new().unwrap().block_on(classify(
            &parse_snapshot(PS),
            &crate::state::TunnelState::default(),
        ));
        assert!(found.is_empty(), "{found:?}");
    }
}
