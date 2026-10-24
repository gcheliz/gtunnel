<p align="center">
  <img src="docs/logo.png" alt="gtunnel" width="420">
</p>

<p align="center">
  <a href="CHANGELOG.md"><img src="https://img.shields.io/badge/version-0.1.0-blue.svg" alt="Version 0.1.0"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-orange.svg" alt="Rust 1.88 or newer"></a>
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/edition-2024-orange.svg" alt="Rust 2024 edition"></a>
  <a href="SECURITY.md"><img src="https://img.shields.io/badge/security-policy-green.svg" alt="Security policy"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg" alt="Platform: macOS and Linux">
</p>

<p align="center">
  <sub>Every push runs <a href=".github/workflows/ci.yml">CI</a>: rustfmt, clippy with <code>-D warnings</code>, the test suite, <code>cargo audit --deny warnings</code>, a build on the declared 1.88 floor, and zizmor over the workflows.</sub>
</p>

![The gtunnel TUI: tunnel list, details, logs and metrics](docs/demo-tui.gif)

A TUI-first manager for GCP Identity-Aware Proxy tunnels. One binary, two modes:
a private port on `localhost` that reaches a VM inside a VPC, and a public HTTPS
URL in front of a service running on your laptop.

No inbound firewall rules, no public IP on the target, no agent on the VM.

## Features

| Feature | What it does |
|---|---|
| Access tunnels | `localhost:15432` reaches a VM port through IAP. Creates nothing in GCP |
| Jump tunnels | `--via <instance>` reaches a host only that instance can reach, such as a private GKE control plane |
| Public tunnels | `https://api.example.com` reaches `localhost:3000` through a relay VM and a global HTTPS load balancer |
| TUI dashboard | Live status, latency, connection counts, load-balancer traffic and per-tunnel logs |
| Managed services | launchd on macOS, systemd `--user` on Linux. Restart on failure, optional auto-start at login |
| Multiple accounts | One gcloud configuration, project and zone per account; `;` cycles them in the TUI |

[docs/architecture.md](docs/architecture.md) has the topology for both modes, the jump and
service-lifecycle diagrams, the account model, and every resource name.

## Prerequisites

macOS or Linux, the Google Cloud SDK on `PATH`, an authenticated gcloud configuration, and
`roles/iap.tunnelResourceAccessor` on the instance, destination group or project. IAP also
needs a firewall rule letting `35.235.240.0/20` reach the port you are tunnelling to;
`gtunnel doctor` prints the exact command. Public mode additionally needs a Cloud DNS
managed zone and the IAM in [docs/troubleshooting.md](docs/troubleshooting.md#iam).

gtunnel stores no credential: every token comes from `gcloud auth print-access-token`.

## Installation

`cargo install --path .`, or download `gtunnel-<target>.tar.gz` from Releases, verify the
`.sha256` and extract. Targets per release: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`. `gtunnel update` replaces a
downloaded binary in place after verifying the published SHA-256, and prints the right
upgrade command instead when Homebrew or cargo installed it.

## Quick start

### A private port to a VM

```
gtunnel init                    # read a gcloud configuration into a gtunnel account
gtunnel add db my-vm:5432 -s    # localhost:15432 -> my-vm:5432, started now
psql -h localhost -p 15432
```

![gtunnel add, list, start, stop and delete](docs/demo-access.gif)

### A public URL to a local service

The relay is a one-off per account:

```
gtunnel domains add my-dns-zone  # register a Cloud DNS managed zone
gtunnel relay up                 # relay VM, load balancer, certificate
gtunnel expose web 3000 -s       # https://web.example.com -> localhost:3000
```

`relay up` creates an e2-micro VM, a global load balancer and a managed certificate:
roughly USD 25/month per account, nothing per tunnel. `relay down` removes all of it.

## The TUI

`gtunnel` with no subcommand opens the TUI; `gtunnel demo` opens it filled with synthetic
data and touches no GCP project and no config file. `?` opens all 31 bindings as a page,
`;` switches account and project together, `:` opens the accounts page to add, edit, select
and remove them, and the footer offers only what the selected tunnel can do. `e` chooses
between editing the target and ports, renaming, and moving the tunnel to another account.
Metrics refresh every 5s, health every 30s, load-balancer request counts
every 60s. Below 121 columns the list stacks above Details, Logs and Metrics.

Every binding and pane: [docs/usage.md](docs/usage.md#the-tui). Why each one looks the way
it does: [docs/design.md](docs/design.md).

## Commands

Beyond the five above: `run` holds an ephemeral tunnel in the foreground and tears its
route down on Ctrl+C, `logs -f` follows a tunnel's log, and `doctor --fix` removes
leftovers. `rename` moves a tunnel's service and log with its name, and `move` hands an
Access tunnel to another account, verifying the destination before it writes anything.
`start`, `stop`, `restart`, `autostart`, `list`, `delete`, `zones`, `account`,
`reset`, `demo` and `update` do what they say. `--account <name>` works on every command.
Each one, its output, and which of them write to GCP:
[docs/usage.md](docs/usage.md#command-map).

## Configuration

`~/Library/Application Support/gtunnel` on macOS, `~/.config/gtunnel` on Linux;
`GTUNNEL_CONFIG_DIR` overrides it. It holds `config.toml` (accounts, relay and domains),
`tunnels.toml`, the service definitions and one log per tunnel — the full table is in
[docs/architecture.md](docs/architecture.md#files). Local ports come from 15000-15999 and
are derived from the tunnel name, so the same name gets the same port on every machine
unless it is taken; relay ports come from 30000-30998.

## Troubleshooting

Start with `gtunnel doctor`: it reports everything it can detect and prints the exact
command for what it cannot fix itself. `4003` in a log means IAP cannot reach the target
port, so allow `35.235.240.0/20` to it; `4033` means you lack
`roles/iap.tunnelResourceAccessor`; a public tunnel returning 503 has lost its backend,
host rule or DNS record, and `gtunnel restart <name>` re-creates them.

[docs/troubleshooting.md](docs/troubleshooting.md) has the full error table, the IAP
firewall and IAM requirements, the service commands for both platforms, and the cost
breakdown.

## Documentation

| Document | Contents |
|---|---|
| [docs/usage.md](docs/usage.md) | Every command, with its output |
| [docs/architecture.md](docs/architecture.md) | Topology, process model, resource names |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Error table, firewall and IAM, services, cost |
| [docs/design.md](docs/design.md) | How the TUI displays information: hierarchy, density, colour |
| [CHANGELOG.md](CHANGELOG.md) | Release history |
| [SECURITY.md](SECURITY.md) | Reporting a vulnerability |

The recordings above are generated from the committed
[VHS](https://github.com/charmbracelet/vhs) tapes in [docs/tapes](docs/tapes), with
`vhs docs/tapes/<name>.tape`.

## Safety

`gtunnel doctor --fix` removes only resources tagged `gtunnel:<account>` that no live
tunnel claims, and re-checks that claim immediately before each removal. A Cloud DNS
record carries no description, so gtunnel treats one as its own only while it still holds
a tagged backend, host rule or named port for the same tunnel. `gtunnel reset` removes
local state only; cloud resources go with `gtunnel relay down`.

## License

MIT. See [LICENSE](LICENSE).
