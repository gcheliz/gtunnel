# Security Policy

## Supported versions

| Version | Supported |
|---|---|
| Latest 0.2.x release | Yes |
| Anything older | No; upgrade to the latest release |

Only the latest release gets fixes. gtunnel is a single binary with no server
side, so upgrading is replacing the binary: `gtunnel update`, Homebrew, or
cargo, whichever installed it.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository: the Security
tab, then **Report a vulnerability**. That opens a private advisory visible
only to the maintainers, so nothing is disclosed while a fix is being written.

Please do not open a public issue for a vulnerability.

| | |
|---|---|
| First response | within 7 days |
| Assessment and a plan, or a reason it is not a vulnerability | within 14 days |
| Fix released | depends on severity; you will be told which release carries it |
| Credit | offered in the advisory and the changelog unless you ask otherwise |

## What gtunnel holds

Knowing what is and is not at stake usually narrows a report quickly.

| | |
|---|---|
| Credentials | None are stored: every call takes a fresh token from `gcloud auth print-access-token` ([why](docs/decisions.md#1-gtunnel-stores-no-credential)) |
| On disk | `config.toml` and `tunnels.toml` (accounts, projects, zones, tunnel definitions), launchd plists or systemd units, and logs. No secret is written to any of them |
| Network | gtunnel does not implement the transport. An Access tunnel runs `gcloud compute start-iap-tunnel`, a `--via` tunnel `gcloud compute ssh --tunnel-through-iap -- -N -L`, and a public tunnel the same command with `-R` to the relay instance |
| In GCP | Access mode creates nothing. Public mode creates a relay instance, a load balancer, a certificate map, DNS records and firewall rules. `doctor --fix` removes only leftovers it can prove are its own ([Leftovers](docs/troubleshooting.md#leftovers)) |

A tunnel's local port is reachable by anything on the machine that can open a
socket. That is what a forwarded port is, not a defect, but it is worth
knowing on a shared host.

## Dependencies

`cargo deny check` (advisories, licences and sources, per `deny.toml`) runs on every pull request and on every release, and
Dependabot opens updates for the crate and action dependencies.
