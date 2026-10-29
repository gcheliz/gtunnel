---
name: gtunnel
description: Manage GCP IAP TCP access tunnels and local service exposure using gtunnel CLI or MCP tools. Use when connecting to private GCP compute instances, databases (Postgres, Mongo, Redis), Kubernetes clusters, or debugging tunnel connectivity.
---

# gtunnel

## Overview

Reach private GCP ports for the user, or publish a local service at an HTTPS URL. Prefer a TTL run for one-off access, and prove health before reporting done. gtunnel forwards through Google Cloud Identity-Aware Proxy (IAP) with no external IP on the target and no VPN, and supervises persistent tunnels as background services (launchd on macOS, systemd on Linux).

Capabilities:
- Persistent access tunnels to instances, destination groups and through bastion jump hosts.
- Per-account scoping, with a service per tunnel per account.
- TCP health probes and supervised restarts.
- Publishing a local service through a GCP relay behind a global HTTPS load balancer.
- MCP tools and `--json` CLI output.

## Hybrid Tool Routing

When executing tunnel operations, select tools according to availability:

### 1. Primary: Native MCP Tools
If `gtunnel_*` MCP tools are available, prefer them over the CLI:
- `gtunnel_list_tunnels`: tunnels and live runs, each with `status`, `local_port` (`null` on a public tunnel) and `health` (`healthy`, `unhealthy`, `auth_expired`, `stopped` or `unknown`). One call answers whether every tunnel works.
- `gtunnel_start_tunnel`: start a tunnel.
- `gtunnel_stop_tunnel`: stop a tunnel, or a live run.
- `gtunnel_restart_tunnel`: restart a tunnel.
- `gtunnel_get_status`: one tunnel's `status`, `health`, PID, restart count and probe latency; for a live run (`"run": true`), `local_port` and `expires_at`.
- `gtunnel_get_logs`: the last `lines` lines (default 50) of a tunnel's log, or of a run launched by `gtunnel_run_tunnel`. Raw lines: read the error code yourself.
- `gtunnel_add_tunnel`: create a persistent Access tunnel.
- `gtunnel_run_tunnel`: a short-lived Access tunnel that closes itself after `ttl_minutes` (required, 1 to 480). Never public. `gtunnel_stop_tunnel` closes it early.
- `gtunnel_delete_tunnel`: delete a tunnel and its service.
- `gtunnel_rename_tunnel`: rename (`name`, `new_name`); service and log follow, a running tunnel restarts. A name in use is refused.
- `gtunnel_expose_tunnel`: publish a local service at `https://<name>.<domain>` through the account's relay (`name`, `target`, optional `domain`, `public`, `start`). Private by default (Google sign-in), the reverse of the CLI. Pass `public: true` only when the user asked for anyone on the internet to reach it, and confirm first. Only a service on this machine: refuse another host and point at the CLI. Never creates a relay; `gtunnel relay up` is CLI only.
- `gtunnel_move_tunnel`: move an Access tunnel to another account (`name`, `to`, optional `rename` when the destination uses the name). Confirm first. A failure of kind `auth` means the user must run `gcloud auth login` for that account: MCP cannot prompt.
- `gtunnel_list_accounts`: accounts and the selection (`selected_account`, `selected` on each). The `gtunnel://accounts` resource returns the same object.
- `gtunnel_doctor`: diagnostics on prerequisites, permissions and service configuration.

The JSON tools (`gtunnel_list_tunnels`, `gtunnel_get_status`, `gtunnel_run_tunnel`, `gtunnel_expose_tunnel`, `gtunnel_list_accounts`, `gtunnel_doctor`) also return `structuredContent`; the list is `{"tunnels": [...]}` and the doctor `{"findings": [...]}`. A failure is `{"error": {"kind", "message"}}`, and `message` carries the whole cause, such as the line and column of a broken `tunnels.toml`. Tools annotated `readOnlyHint` change nothing, so call them freely.

Routing: for short-lived access (a query, a one-off check) prefer `gtunnel_run_tunnel`: it needs no cleanup. Use `gtunnel_add_tunnel` only for something the user keeps. Choose the TTL for the task at hand, not the maximum.

### 2. Fallback: Shell Commands
If MCP tools are not configured, use the CLI directly:
- List tunnels: `gtunnel list` (or `gtunnel list --json`)
- Start tunnel: `gtunnel start <name>`
- Stop tunnel: `gtunnel stop <name>` (a tunnel or a live run)
- Restart tunnel: `gtunnel restart <name>`
- Status check: `gtunnel list --json` (one row per tunnel, with `status`, `local_port` and the probed `health`)
- Read logs: `gtunnel logs <name> --lines <N>` (a tunnel or a live run launched from MCP)
- Add tunnel: `gtunnel add <name> <target> [flags]`
- Delete tunnel: `gtunnel delete <name>`
- Account management: `gtunnel account list` (or `gtunnel account select <name>`)
- Diagnostics: `gtunnel doctor` (or `gtunnel doctor --json`)

## Tunnel Naming Conventions

Tunnel names identify what is being reached, never the account or environment.

### Naming Syntax and Rules
- Pattern: `<platform>` or `<platform>-<service>`
- Allowed: `[a-z0-9-]`, up to 40 characters.
- NEVER an environment or account suffix: no `-dev`, `-prod`, `-staging`, `-test`.
- Names are scoped per account: `kubernetes` in `dev` and `kubernetes` in `prod` are separate tunnels.

### Examples

| Target Type | Valid Name | Invalid Name | Reason |
|-------------|------------|--------------|--------|
| Kubernetes API | `kubernetes` | `kubernetes-dev` | Redundant environment suffix |
| Primary Database | `postgres` | `postgres-prod` | Redundant environment suffix |
| Cache Service | `redis` or `redis-cache` | `redis-staging` | Redundant environment suffix |
| Session Store | `redis-session` | `redis-session-prod` | Redundant environment suffix |
| Document Database | `mongodb` | `mongo-prod` | Redundant environment suffix |

The `<service>` suffix only tells apart several instances of one platform in one account: one Redis is `redis`; several are `redis-cache`, `redis-session`.

## Target Selection

### 1. Direct Compute Instance
```bash
gtunnel add db bastion:5432 -z europe-west1-b
```
`-z <zone>` when the instance is not in the account's default zone.

### 2. Destination Group (internal IP via IAP)
```bash
gtunnel add private-api 10.0.0.5:5432 --dest-group prod --region europe-west1
```

### 3. Jump Host (bastion into a private subnet)
```bash
gtunnel add k8s-api 10.0.0.10:443 --via bastion
```

### 4. Client-Side Routing (MongoDB Compass and Bastion Forwarding)
- One tunnel to the bastion, not one per backend host or shard. For MongoDB Compass, forward to the bastion's SSH port:
  ```bash
  gtunnel add mongodb bastion:22 --local-port 10022
  ```
- In Compass: SSH Tunnel via `localhost:10022` with your SSH key, cluster URI in Compass. Never SSL Proxy; never pin replica-set member hostnames.

## Account Scoping

Configuration, state and services are per account.
- One command: `--account <name>`:
  ```bash
  gtunnel list --account dev
  gtunnel start redis --account prod
  ```
- Switch the active account:
  ```bash
  gtunnel account select dev
  ```
- MCP: the `account` parameter:
  ```json
  {"name": "redis", "account": "prod"}
  ```

## Port Allocation

Without `--local-port`, a free port is derived from the name, in 15000-15999. Both accounts run on one machine, so the same name takes a different `--local-port` per account:

| Tunnel | Account `dev` | Account `prod` |
|--------|---------------|----------------|
| `k8s` | `--local-port 8443` | `--local-port 8444` |

## Verification Protocol Before Completion

Never assume a tunnel works. Before reporting a tunnel created or started, produce live evidence in three steps:

### Step 1: Daemon State Verification
Read `isError` on every mutating MCP result first: `true` means the call failed, whatever the text says, including a start that did not come up. Then:
- MCP: Call `gtunnel_get_status` and verify `"status": "running"`, a non-zero PID and `"health": "healthy"`. `auth_expired` is a credentials problem (`gcloud auth login`), not a tunnel one; `unhealthy` means read the log.
- CLI: `gtunnel list --json`; the row has `"status": "running"` and `"health": "healthy"`, read the same way.

### Step 2: Log Inspection
Read the last 10 lines: `gtunnel_get_logs` with `lines: 10`, or `gtunnel logs <name> --lines 10`. Look for:
- `4033`: missing `roles/iap.tunnelResourceAccessor`.
- Token refresh failure: gcloud credentials expired, or a wrong configuration.
- Connection refused: nothing listens on the target port.

### Step 3: Active TCP Probe
Confirm the local port accepts connections:
```bash
nc -z -v -w 3 127.0.0.1 <port>
```
Or read `probe_latency_ms` from `gtunnel_get_status` (MCP): a number means the local port accepted a connection.

## Troubleshooting

### Diagnostics (`gtunnel doctor`)
Checks credentials, launchd/systemd state and configuration:
```bash
gtunnel doctor
```
Or via MCP: `gtunnel_doctor`.

### Supervision & Health Probes (`gtunnel watch`)
`gtunnel watch` restarts tunnels that are running but refusing connections:
```bash
gtunnel watch status   # is the watchdog agent installed, where is its log, when was its last pass
gtunnel watch -v       # the restart loop in the foreground, saying what each pass saw
```
Bare `gtunnel watch` runs the same loop quietly: nothing per pass on screen, and each action written to the tunnel's log. `gtunnel watch install` makes it run without a terminal.

### Inspecting Daemon Logs
- Prefer `gtunnel logs <name> --lines 50` (`-f` follows).
- The file is `<config>/logs/<account>-<name>.log`, where `<config>` is `~/.gtunnel` (or `GTUNNEL_CONFIG_DIR`).

## Tool Validation & Self-Testing

### 1. Schema validation
Checks every tool schema, argument and description:
```bash
gtunnel mcp --validate
```
Exit 0 on success, non-zero if any schema is invalid.

### 2. MCP Inspector
Interactive protocol and tool testing:
```bash
npx @modelcontextprotocol/inspector gtunnel mcp
```

## Safety Rules

- Confirm with the user before deleting a tunnel or changing configuration (`gtunnel delete <name>`, `gtunnel_delete_tunnel`), and before renaming, moving or exposing one (`gtunnel_rename_tunnel`, `gtunnel_move_tunnel`, `gtunnel_expose_tunnel`).
- Never leak real project IDs, internal IPs, personal emails or tokens in docs, logs or commit messages. Use placeholders: `example-dev-1a2b`, `you@example.com`, `bastion`, `10.0.0.10`.
- No emojis in commands, documents or tool responses.
- Rename with `gtunnel rename <old> <new>`, move with `gtunnel move <name> --to <account>`; both carry the service and log. For any other change, delete the tunnel first, then add it again, so no launchd/systemd job is orphaned.
