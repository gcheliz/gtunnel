<p align="center">
  <img src="docs/logo.png" alt="gtunnel" width="420">
</p>

<p align="center">
  <a href="https://github.com/gcheliz/gtunnel/releases/latest"><img src="https://img.shields.io/github/v/release/gcheliz/gtunnel?color=blue" alt="Latest release"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-orange.svg" alt="Rust 1.88 or newer"></a>
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/edition-2024-orange.svg" alt="Rust 2024 edition"></a>
  <a href="SECURITY.md"><img src="https://img.shields.io/badge/security-policy-green.svg" alt="Security policy"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg" alt="Platform: macOS and Linux">
</p>

<p align="center"><a href="https://gcheliz.github.io/gtunnel/">gcheliz.github.io/gtunnel</a></p>

## What gtunnel is

A TUI-first manager for GCP Identity-Aware Proxy tunnels. One binary, two modes:
a private port on `localhost` that reaches a VM inside a VPC, and a public HTTPS
URL in front of a service running on your laptop. Tunnels are declared once, run as
supervised user services, and are watched from a dashboard that shows whether each
one is actually carrying traffic.

<p align="center">
  <img src="docs/demo-tui.gif" alt="The gtunnel TUI: tunnel list, details, logs and metrics, the edit and add choosers, the accounts page and the key map" width="720">
</p>

| Why | What it means in practice |
|---|---|
| Nothing is exposed | No inbound firewall rule, no public IP on the target, no agent on the VM. IAP authorises each connection against your own Google identity |
| Nothing is stored | gtunnel keeps no credential. Every token comes from `gcloud auth print-access-token` at connect time |
| Access mode creates nothing in GCP | A private port is a local process plus an IAP connection, so it leaves no resource behind and costs nothing |
| Tunnels survive the terminal | launchd on macOS, systemd `--user` on Linux: restart on failure, optional start at login, one log per tunnel |
| The dashboard tells the truth | Live latency, active connections, load-balancer request counts and the tunnel's own log, rather than a process that is merely running |

Everything in the table below ships in the current version. This README has no roadmap
section because it describes only what the binary already does.

![gtunnel topology: a private port inward through IAP, a public URL outward through a relay VM](docs/images/architecture/gtunnel-overview.png)

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

| Requirement | How to get it | How to check |
|---|---|---|
| macOS or Linux | — | `uname -s` |
| Google Cloud SDK on `PATH` | [cloud.google.com/sdk/docs/install](https://cloud.google.com/sdk/docs/install) | `gcloud version` |
| An authenticated configuration | `gcloud init` then `gcloud auth login` | `gcloud config list` |
| `roles/iap.tunnelResourceAccessor` | [cloud.google.com/iap/docs/using-tcp-forwarding](https://cloud.google.com/iap/docs/using-tcp-forwarding#grant-permission) on the instance, destination group or project | `gtunnel doctor` |
| A firewall rule for `35.235.240.0/20` | `gtunnel doctor` prints the exact `gcloud compute firewall-rules create` for the port you are tunnelling to | `gtunnel doctor` |
| Rust 1.88 or newer and a C compiler, for the cargo routes only | [rustup](https://rustup.rs) on macOS and Linux; `xcode-select --install` on macOS, `build-essential` or `gcc` on Linux ([docs/usage.md](docs/usage.md#building-from-source)) | `cargo --version`, `cc --version` |

Public mode additionally needs a [Cloud DNS](https://cloud.google.com/dns/docs/zones)
managed zone you control and the IAM in
[docs/troubleshooting.md](docs/troubleshooting.md#iam).

The guided walkthrough, from a clean machine to a first working tunnel with the output of
every step: [docs/usage.md](docs/usage.md#getting-set-up).

## Installation

| Route | Command | Upgrade with |
|---|---|---|
| A published binary | the four lines below | `gtunnel update` |
| From a clone | `cargo install --path .` | `git pull` and run it again |
| From the repository | `cargo install --git https://github.com/gcheliz/gtunnel` | the same command with `--force` |

Pick your machine and release version:

| Machine | File |
|---|---|
| Apple Silicon Mac | `gtunnel-macos-arm64-v<version>.tar.gz` |
| Intel Mac | `gtunnel-macos-intel-v<version>.tar.gz` |
| Linux, amd64 | `gtunnel-linux-amd64-v<version>.tar.gz` |
| Linux, arm64 | `gtunnel-linux-arm64-v<version>.tar.gz` |

```
VERSION="v0.1.25"
F="gtunnel-macos-arm64-${VERSION}.tar.gz"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F.sha256"
shasum -a 256 -c "$F.sha256" && tar -xzf "$F" && sudo mv gtunnel /usr/local/bin/
```

`sha256sum -c` in place of `shasum -a 256 -c` on Linux. The Linux builds are static musl
binaries, so they do not depend on the host's glibc.

The checksum proves the download arrived intact. To prove this repository built it, verify
the [attestation](https://docs.github.com/actions/security-guides/using-artifact-attestations-to-establish-provenance-for-builds)
with the GitHub CLI:

```
gh attestation verify "gtunnel-macos-arm64-${VERSION}.tar.gz" -o gcheliz
```

Silence and exit 0 mean it checked out; a file this repository did not build fails with
`HTTP 404` instead. Add `--format json` to read which workflow, tag and commit produced
it — [docs/usage.md](docs/usage.md#4-install-gtunnel) shows that output. Attestations
begin with v0.1.1.

There is no `cargo install gtunnel`: the name on crates.io belongs to an unrelated crate.
Use `--git` or `--path`, as above. Already ran it?
[docs/troubleshooting.md](docs/troubleshooting.md#installation) says what it left behind.

`gtunnel update` replaces a downloaded binary after verifying the published SHA-256, and
prints the right upgrade command instead when cargo or Homebrew installed it. Confirm the
install with `gtunnel --version`, then run `gtunnel doctor` before the first tunnel.

## Quick start

### A private port to a VM

```
gtunnel init                    # read a gcloud configuration into a gtunnel account
gtunnel add db my-vm:5432 -s    # localhost:15432 -> my-vm:5432, started now
psql -h localhost -p 15432
```

<p align="center">
  <img src="docs/demo-access.gif" alt="gtunnel add, list, start, stop and delete" width="720">
</p>

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
data and touches no GCP project and no config file. `?` opens all 32 bindings as a page,
`;` switches account and project together, `:` opens the accounts page to add, edit, select
and remove them, and the footer offers only what the selected tunnel can do. `e` chooses
between editing the target and ports, renaming, and moving the tunnel to another account.
Metrics refresh every 5s, health every 30s, load-balancer request counts
every 60s. Below 121 columns the list stacks above Details, Logs and Metrics.

Every binding and pane: [docs/usage.md](docs/usage.md#the-tui). Why each one looks the way
it does: [docs/design.md](docs/design.md).

## Commands

| Command | Description |
|---|---|
| `gtunnel` | Opens the interactive TUI dashboard. |
| `gtunnel init` | Creates a `gtunnel` account from a `gcloud` configuration. |
| `gtunnel add` | Adds a persistent Access tunnel. |
| `gtunnel expose` | Publishes a local service to a public URL. |
| `gtunnel run` | Runs an ephemeral tunnel in the foreground. |
| `gtunnel start` | Starts a stopped tunnel as a background service. |
| `gtunnel stop` | Stops a running tunnel service. |
| `gtunnel restart` | Restarts a tunnel service. |
| `gtunnel delete` | Deletes a tunnel. |
| `gtunnel logs` | Tails a tunnel's logs. |
| `gtunnel doctor` | Checks for common configuration problems and offers fixes. |
| `gtunnel account` | Manages accounts (`list`, `select`, `add`, `edit`, `remove`). |
| `gtunnel domains` | Manages Cloud DNS managed zones for public tunnels. |
| `gtunnel relay` | Manages the public relay (`up`, `down`, `recreate-cert`). |
| `gtunnel update` | Checks for and installs updates. |
| `gtunnel demo` | Launches the TUI with synthetic data. |
| `gtunnel mcp` | Runs the Model Context Protocol (MCP) stdio server. |

Every command accepts a global `--account <name>` flag, and `list`, `account list`, and `doctor` accept `--json`. The full command map, with every
flag and output, is in [docs/usage.md](docs/usage.md#command-map).

## AI Agents & MCP

gtunnel includes native integration for AI agents (Claude Code, Gemini CLI, Cursor, etc.):

- **In-Repo Skill:** Located at `.agents/skills/gtunnel/SKILL.md` using the open Agent Skills standard. Automatically equips AI agents with tunnel naming rules, target routing (e.g. MongoDB bastions), account scoping, and pre-completion verification protocols.
- **Model Context Protocol (MCP) Server:** `gtunnel mcp` provides a native in-binary JSON-RPC 2.0 stdio server exposing 10 tools and 2 live resources (`gtunnel://tunnels`, `gtunnel://accounts`).
- **Tool Validation:** `gtunnel mcp --validate` runs non-interactive schema validation across all registered tools. Developers can inspect tools interactively with `npx @modelcontextprotocol/inspector gtunnel mcp`.
- **Machine-Readable Outputs:** `gtunnel list --json`, `gtunnel account list --json`, and `gtunnel doctor --json` provide structured JSON for programmatic consumption.

Create a `.mcp.json` file in your workspace root (compatible with both Claude Code and Gemini CLI):

```json
{
  "mcpServers": {
    "gtunnel": {
      "command": "gtunnel",
      "args": ["mcp"]
    }
  }
}
```

- **Claude Code:** Reads `.mcp.json` automatically in the project directory, launching the MCP server on startup.
- **Gemini CLI:** Add the same configuration to `.gemini/settings.json` or reference `.mcp.json` under `"mcpServers"`.
- **Claude Desktop:** Add the entry to `~/Library/Application Support/Claude/claude_desktop_config.json`.

## Configuration

The configuration lives in `~/.gtunnel`, with fallbacks to standard platform
directories. The directory holds:

| File | Purpose |
|---|---|
| `config.toml` | Accounts, project IDs, default zones, and log level. |
| `tunnels.toml` | The list of every persistent tunnel. |
| `breakers.toml` | State for the watchdog's circuit breakers. |
| `logs/` | One log file per tunnel, named `<account>-<tunnel>.log`. |
| `services/` | `launchd` and `systemd` service definitions. |

Local ports are derived from the tunnel name (15000-15999 for Access, 30000-30998 for
Expose) to ensure stability across machines. See [docs/architecture.md](docs/architecture.md#files)
for the full file layout.

## Troubleshooting

`gtunnel doctor` is the first step. It checks IAM, firewall rules, and local state.

| Symptom | Cause | Fix |
|---|---|---|
| `4003` in log | IAP cannot reach the target port. | `gtunnel doctor` prints the `gcloud compute firewall-rules create` command to allow traffic from `35.235.240.0/20`. |
| `4033` in log | You lack `roles/iap.tunnelResourceAccessor`. | `gtunnel doctor` prints the `gcloud projects add-iam-policy-binding` command. |
| Public URL 503 | The load balancer backend is unhealthy. | `gtunnel restart <name>` re-creates the backend, host rule, and DNS record. |
| `cargo uninstall` fails | Ambiguous crate name. | The error message prints two specifications; uninstall the one with `registry+`. |

[docs/troubleshooting.md](docs/troubleshooting.md) has the full error table, IAM and firewall
requirements, service management commands, and cost breakdown.

## Documentation

| Document | Contents |
|---|---|
| [docs/usage.md](docs/usage.md) | Every command, with its output |
| [docs/architecture.md](docs/architecture.md) | Topology, process model, resource names |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Error table, firewall and IAM, services, cost |
| [docs/design.md](docs/design.md) | How the TUI displays information: hierarchy, density, colour |
| [docs/decisions.md](docs/decisions.md) | Why gtunnel is shaped the way it is, and what each choice costs |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Setting up, the CI gate, and how a change lands |
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

Every pull request, and every merge to main before it is released, runs
[CI](.github/workflows/ci.yml): rustfmt, clippy with `-D warnings`, the test suite,
`cargo audit --deny warnings`, a `cargo check` on the declared 1.88 floor, and zizmor
over the workflows.

## License

MIT. See [LICENSE](LICENSE).
