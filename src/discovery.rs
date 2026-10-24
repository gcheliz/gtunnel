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

fn flag_value<'a>(tokens: &[&'a str], flag: &str) -> Option<&'a str> {
    tokens.iter().find_map(|token| token.strip_prefix(flag))
}

// `ps -axo pid=,command=` output, minus the processes gtunnel already knows
pub fn parse_iap_processes(ps: &str, managed_pids: &HashSet<u32>) -> Vec<ExternalTunnel> {
    let mut out = Vec::new();
    for line in ps.lines() {
        if !line.contains("start-iap-tunnel") {
            continue;
        }
        // The grep that looks for these processes matches itself
        if line.contains("grep") {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(pid) = tokens.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if managed_pids.contains(&pid) {
            continue;
        }
        let rest: Vec<&str> = tokens.collect();
        let Some(position) = rest.iter().position(|t| *t == "start-iap-tunnel") else {
            continue;
        };
        let positional: Vec<&str> = rest[position + 1..]
            .iter()
            .take_while(|t| !t.starts_with('-'))
            .copied()
            .collect();
        let [target, port] = positional.as_slice() else {
            continue;
        };
        let Ok(remote_port) = port.parse::<u16>() else {
            continue;
        };
        let local_port = flag_value(&rest, "--local-host-port=")
            .and_then(|value| value.rsplit(':').next())
            .and_then(|port| port.parse::<u16>().ok());
        let zone = flag_value(&rest, "--zone=").map(|z| z.to_string());

        out.push(ExternalTunnel {
            pid,
            target: target.to_string(),
            remote_port,
            local_port,
            zone,
        });
    }
    out
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

// Every pid gtunnel is responsible for: the services it installed, whatever
// account owns them, and the ephemeral runs it recorded.
pub async fn managed_pids() -> HashSet<u32> {
    let mut pids = HashSet::new();
    if let Ok(state) = crate::state::TunnelState::load() {
        for label in managed_labels(&state) {
            if let Some(pid) = crate::daemon::runtime_info(&label).await.pid {
                pids.insert(pid);
            }
        }
    }
    if let Ok(records) = crate::ephemeral::list_ephemeral() {
        pids.extend(records.iter().map(|(record, _)| record.pid));
    }
    // A service's own transport processes answer to it
    descendants(&process_tree(), &pids)
}

// Every process below the given roots, by pid and parent pid as `ps` prints
// them. A tunnel's transport is the tunnel, not a second one: a jump tunnel
// reaches its target through an ssh child whose own ProxyCommand is another
// gcloud IAP process, and that process answers to the service above it.
fn descendants(ps_pid_ppid: &str, roots: &HashSet<u32>) -> HashSet<u32> {
    let pairs: Vec<(u32, u32)> = ps_pid_ppid
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect();

    let mut family: HashSet<u32> = roots.clone();
    // The tree is shallow and unordered, so keep sweeping until a pass adds
    // nothing rather than assuming a parent is listed before its child
    loop {
        let before = family.len();
        for (pid, ppid) in &pairs {
            if family.contains(ppid) {
                family.insert(*pid);
            }
        }
        if family.len() == before {
            return family;
        }
    }
}

fn process_tree() -> String {
    std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid="])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

// Every IAP tunnel on this machine that gtunnel does not manage
pub fn external_tunnels(managed_pids: &HashSet<u32>) -> Vec<ExternalTunnel> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_iap_processes(&String::from_utf8_lossy(&out.stdout), managed_pids)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = "\
  501 /usr/bin/python3 /opt/homebrew/share/google-cloud-sdk/lib/gcloud.py compute start-iap-tunnel db-1 5432 --local-host-port=localhost:6000 --zone=us-central1-a
  502 /usr/bin/python3 /opt/homebrew/share/google-cloud-sdk/lib/gcloud.py compute start-iap-tunnel managed-vm 3306 --local-host-port=localhost:15306 --zone=europe-west1-b
  503 /usr/bin/grep start-iap-tunnel
  504 /bin/zsh
";

    #[test]
    fn external_tunnels_exclude_the_ones_gtunnel_manages() {
        let managed: HashSet<u32> = [502u32].into_iter().collect();
        let found = parse_iap_processes(PS, &managed);
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
        let found = parse_iap_processes(PS, &HashSet::new());
        assert!(found.iter().all(|t| t.pid != 503), "{found:?}");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_tunnel_without_a_local_host_port_still_parses() {
        let ps = "  601 gcloud compute start-iap-tunnel vm-9 22\n";
        let found = parse_iap_processes(ps, &HashSet::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].local_port, None);
        assert_eq!(found[0].zone, None);
        assert_eq!(found[0].remote_port, 22);
    }

    #[test]
    fn no_processes_means_no_tunnels() {
        assert!(parse_iap_processes("", &HashSet::new()).is_empty());
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
        let found = descendants(TREE, &[100u32].into_iter().collect());
        assert!(found.contains(&200) && found.contains(&300), "{found:?}");
        assert!(
            !found.contains(&400) && !found.contains(&500),
            "another process's family is not ours: {found:?}"
        );
    }
}
