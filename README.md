<p align="center">
  <img src="docs/logo.png" alt="gtunnel" width="420">
</p>

<p align="center">
  <a href="https://gcheliz.github.io/gtunnel/"><img src="https://img.shields.io/badge/website-live-4DD0E1?logo=githubpages&logoColor=white" alt="Website: gcheliz.github.io/gtunnel"></a>
  <a href="https://github.com/gcheliz/gtunnel/releases/latest"><img src="https://img.shields.io/github/v/release/gcheliz/gtunnel?color=blue" alt="Latest release"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.89%2B-orange.svg" alt="Rust 1.89 or newer"></a>
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/edition-2024-orange.svg" alt="Rust 2024 edition"></a>
  <a href="SECURITY.md"><img src="https://img.shields.io/badge/security-policy-green.svg" alt="Security policy"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg" alt="Platform: macOS and Linux">
</p>

## What gtunnel is

Put any port inside your GCP VPC on `localhost`, or give a laptop service an HTTPS URL, with
one command. No public IP, no port open to the internet, no stored credential. Healthy means
the far end answered, not that a process exists. The TUI, the CLI and your AI agent see the
same tunnels and the same health.

<p align="center">
  <img src="docs/demo-tui.gif" alt="The gtunnel TUI: tunnel list, details, logs and metrics, the edit and add choosers, the accounts page and the key map" width="720">
</p>

One Rust binary, two modes, each persistent or ephemeral ([Concepts](docs/usage.md#concepts)).
It orchestrates gcloud instead of reimplementing Google's protocols.

| Part | What you get |
|---|---|
| **Access** | `gcloud compute start-iap-tunnel` puts a GCP instance port on `localhost` through Identity-Aware Proxy; a **jump tunnel** reaches a host behind that instance. Creates nothing in GCP, costs nothing |
| **Expose** | A **relay instance** in your own project, with no public IP, holds a **reverse SSH** tunnel to your laptop through IAP. A **global HTTPS load balancer** with a **managed certificate** and a **Cloud DNS** record routes each hostname to its relay port; a gated tunnel adds **IAP sign-in** |
| **Exposure** | No public IP on the target, no agent on the instance. The only ingress is from IAP's range, and `gtunnel doctor` prints the command for it. IAP authorises each connection against your own Google identity |
| **Credentials** | None stored. Every token comes from `gcloud auth print-access-token` at connect time |
| **Supervision** | Each tunnel is a user service (launchd on macOS, systemd `--user` on Linux): one supervisor around one gcloud child, one log per tunnel. It outlives the terminal, restarts on failure and can start at login. A **machine watchdog** checks every tunnel every 30s and restarts one that is up but refusing connections |
| **TUI** | The daily surface: every tunnel with live latency, active connections, load-balancer request counts and its own log |
| **CLI** | The same ground for scripts, from `gtunnel add` to `gtunnel doctor --fix`, with `--json` where a script needs it |
| **MCP** | `gtunnel mcp`, a stdio server with 14 tools and 2 resources: an agent lists, starts, exposes and diagnoses tunnels. An expose stays private unless the agent asks for a public one. An agent skill ships in the repo |

All three surfaces share the per-account tunnel list and the same health probe.

![gtunnel topology: a private port inward through IAP, a public URL outward through a relay instance with optional IAP sign-in, and the TUI or an MCP client driving gtunnel](docs/images/architecture/gtunnel-overview.png)

## Install

Needs macOS or Linux and a logged-in `gcloud` ([what GCP needs](docs/gcp.md), [getting set up](docs/usage.md#getting-set-up)).
With Homebrew, no Rust needed:

```
brew install gcheliz/tap/gtunnel
```

`brew upgrade gtunnel` upgrades it; the tap follows each release. Without Homebrew, pick the file for your machine from the
[latest release](https://github.com/gcheliz/gtunnel/releases/latest):

```
VERSION="v0.2.48"
F="gtunnel-macos-arm64-${VERSION}.tar.gz"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F.sha256"
shasum -a 256 -c "$F.sha256" && tar -xzf "$F" && sudo mv gtunnel /usr/local/bin/
```

Other machines, `cargo install`, the build attestation and upgrades (`gtunnel update` for a download or cargo install): [docs/usage.md](docs/usage.md#4-install-gtunnel).
There is no `cargo install gtunnel`: that crates.io name is an unrelated crate. Run `gtunnel doctor` next.

## Quick start

A private port to a GCP instance:

```
gtunnel init                                     # read a gcloud configuration into a gtunnel account (a bare `gtunnel` does this on a first run)
gtunnel add db bastion:5432 --local-port 15432 -s  # localhost:15432 -> bastion:5432, started now
psql -h localhost -p 15432
```

A public URL to a local service:

```
gtunnel relay up --managed-zone my-dns-zone  # once per account: relay instance, load balancer, certificate
gtunnel expose web 3000 -s                   # https://web.example.com -> localhost:3000
```

`gtunnel` with no subcommand opens the TUI; `gtunnel demo` opens it on synthetic data.

## Where to go

| I want to | Read |
|---|---|
| Check the roles, APIs and firewall rules a mode needs | [docs/gcp.md](docs/gcp.md) |
| Go from a clean machine to a first tunnel | [Getting set up](docs/usage.md#getting-set-up) |
| Understand Access, Expose and the target kinds | [Concepts](docs/usage.md#concepts) |
| Find a command, its flags and its output | [docs/commands.md](docs/commands.md) |
| Name a tunnel | [Naming a tunnel](docs/commands.md#naming-a-tunnel) |
| Run gtunnel in CI | [Running in CI](docs/usage.md#running-in-ci) |
| Use the TUI | [The TUI](docs/usage.md#the-tui) |
| Connect an AI agent | [MCP and agent integration](docs/usage.md#model-context-protocol-mcp-and-ai-agent-integration) |
| Edit `config.toml` or `tunnels.toml` | [docs/configuration.md](docs/configuration.md) |
| See the topology, process model and resource names | [docs/architecture.md](docs/architecture.md) |
| Fix an error, a firewall or an IAM problem, or check the cost | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Learn why it is shaped this way | [docs/decisions.md](docs/decisions.md), [docs/design.md](docs/design.md) |
| Contribute, read the release history or report a vulnerability | [CONTRIBUTING.md](CONTRIBUTING.md), [CHANGELOG.md](CHANGELOG.md), [SECURITY.md](SECURITY.md) |

## Environment variables

Every variable gtunnel reads, with its default and what it beats: [docs/configuration.md](docs/configuration.md#environment).

## Safety

| Command | Removes |
|---|---|
| `gtunnel doctor --fix` | Only leftovers gtunnel can prove are its own and no live tunnel claims, after asking: [Leftovers](docs/troubleshooting.md#leftovers) |
| `gtunnel reset` | Local state only |
| `gtunnel relay down` | The relay and everything it owns in GCP, never the managed zone |

## License

MIT. See [LICENSE](LICENSE).
