<p align="center">
  <img src="docs/logo.png" alt="gtunnel" width="420">
</p>

<p align="center">
  <a href="https://gcheliz.github.io/gtunnel/"><img src="https://img.shields.io/badge/website-gcheliz.github.io%2Fgtunnel-4DD0E1?logo=githubpages&logoColor=white" alt="Website: gcheliz.github.io/gtunnel"></a>
  <a href="https://github.com/gcheliz/gtunnel/releases/latest"><img src="https://img.shields.io/github/v/release/gcheliz/gtunnel?color=blue" alt="Latest release"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.89%2B-orange.svg" alt="Rust 1.89 or newer"></a>
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/edition-2024-orange.svg" alt="Rust 2024 edition"></a>
  <a href="SECURITY.md"><img src="https://img.shields.io/badge/security-policy-green.svg" alt="Security policy"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg" alt="Platform: macOS and Linux">
</p>

## What gtunnel is

gtunnel gives you private access to what runs inside a GCP VPC, and an HTTPS address for what
runs on your laptop. It needs no inbound firewall rule, no public IP on the target and no stored
credential. One binary, two modes, each persistent or ephemeral ([Concepts](docs/usage.md#concepts)):
**Access** puts a VM, or a host behind it, on a port of `localhost` through Identity-Aware Proxy.
**Expose** publishes a local service at an HTTPS URL through a relay in your own project.

You drive it three ways, all over the same tunnels. The **TUI** is the daily surface: every
tunnel with its live latency, connections and log, showing whether a tunnel carries traffic and
not merely whether a process is up. The **CLI** covers the same ground for scripts, from
`gtunnel add` to `gtunnel doctor --fix`, with `--json` where a script needs it. The **MCP
server** (`gtunnel mcp`, 14 tools) lets an AI agent list, start, expose and diagnose tunnels; an
expose stays private unless the agent asks for a public one.

Tunnels are declared once and run as supervised user services, launchd on macOS and systemd on
Linux. They outlive the terminal, restart on failure and can start at login.

<p align="center">
  <img src="docs/demo-tui.gif" alt="The gtunnel TUI: tunnel list, details, logs and metrics, the edit and add choosers, the accounts page and the key map" width="720">
</p>

| Property | What it means in practice |
|---|---|
| Nothing is exposed | No inbound firewall rule, no public IP on the target, no agent on the VM. IAP authorises each connection against your own Google identity |
| Nothing is stored | gtunnel keeps no credential. Every token comes from `gcloud auth print-access-token` at connect time |
| Access mode creates nothing in GCP | A private port is a local process plus an IAP connection: no resource left behind, no cost |
| Tunnels survive the terminal | launchd on macOS, systemd `--user` on Linux: restart on failure, optional start at login, one log per tunnel |
| The dashboard tells the truth | Live latency, active connections, load-balancer request counts and the tunnel's own log, not merely a running process |
| Agents can drive it | `gtunnel mcp` is a stdio MCP server with 14 tools and 2 resources, and an agent skill ships in the repo |

![gtunnel topology: a private port inward through IAP, a public URL outward through a relay VM with optional IAP sign-in, and the TUI or an MCP client driving gtunnel](docs/images/architecture/gtunnel-overview.png)

## Install

Needs macOS or Linux and a logged-in `gcloud` ([prerequisites and IAM](docs/usage.md#getting-set-up)).
Pick the file for your machine from the
[latest release](https://github.com/gcheliz/gtunnel/releases/latest):

```
VERSION="v0.2.7"
F="gtunnel-macos-arm64-${VERSION}.tar.gz"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F.sha256"
shasum -a 256 -c "$F.sha256" && tar -xzf "$F" && sudo mv gtunnel /usr/local/bin/
```

Other machines, `cargo install`, the build attestation and upgrades: [docs/usage.md](docs/usage.md#4-install-gtunnel).
There is no `cargo install gtunnel`: that crates.io name is an unrelated crate. Run `gtunnel doctor` next.

## Quick start

A private port to a VM:

```
gtunnel init                                     # read a gcloud configuration into a gtunnel account
gtunnel add db my-vm:5432 --local-port 15432 -s  # localhost:15432 -> my-vm:5432, started now
psql -h localhost -p 15432
```

A public URL to a local service:

```
gtunnel relay up --managed-zone my-dns-zone  # once per account: relay VM, load balancer, certificate
gtunnel expose web 3000 -s                   # https://web.example.com -> localhost:3000
```

`gtunnel` with no subcommand opens the TUI; `gtunnel demo` opens it on synthetic data.

## Where to go

| I want to | Read |
|---|---|
| Go from a clean machine to a first tunnel | [Getting set up](docs/usage.md#getting-set-up) |
| Understand Access, Expose and the target kinds | [Concepts](docs/usage.md#concepts) |
| Find a command, its flags and its output | [docs/commands.md](docs/commands.md) |
| Name a tunnel | [Naming a tunnel](docs/commands.md#naming-a-tunnel) |
| Use the TUI | [The TUI](docs/usage.md#the-tui) |
| Connect an AI agent | [MCP and agent integration](docs/usage.md#model-context-protocol-mcp-and-ai-agent-integration) |
| Edit `config.toml` or `tunnels.toml` | [docs/configuration.md](docs/configuration.md) |
| See the topology, process model and resource names | [docs/architecture.md](docs/architecture.md) |
| Fix an error, a firewall or an IAM problem, or check the cost | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Learn why it is shaped this way | [docs/decisions.md](docs/decisions.md), [docs/design.md](docs/design.md) |
| Contribute, read the release history or report a vulnerability | [CONTRIBUTING.md](CONTRIBUTING.md), [CHANGELOG.md](CHANGELOG.md), [SECURITY.md](SECURITY.md) |

## Environment variables

Four variables change how gtunnel behaves; the full list is in
[docs/configuration.md](docs/configuration.md#environment).

| Variable | Effect |
|---|---|
| `GTUNNEL_CONFIG_DIR` | Keeps every file gtunnel writes in that directory instead of `~/.gtunnel` |
| `GTUNNEL_LOG_LEVEL` | `quiet`, `info` or `debug` for every command; `--log-level` wins over it |
| `GTUNNEL_START_DEADLINE_MS` | How long an Access tunnel has to start listening, default 20000 |
| `VISUAL`, then `EDITOR` | The editor the TUI opens on `e` |

## Safety

| Command | Removes |
|---|---|
| `gtunnel doctor --fix` | Only per-tunnel leftovers no live tunnel claims: backend services tagged `gtunnel:<account>`, and host rules, named ports and DNS records on the relay gtunnel created. The claim is re-checked immediately before each removal |
| `gtunnel reset` | Local state only |
| `gtunnel relay down` | The relay and everything it owns in GCP, never the managed zone |

A Cloud DNS record carries no description, so gtunnel treats one as its own only while it
still holds a tagged backend, host rule or named port for the same tunnel.

## License

MIT. See [LICENSE](LICENSE).
