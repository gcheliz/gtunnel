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
}
