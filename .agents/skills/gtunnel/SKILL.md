---
name: gtunnel
description: Manage GCP IAP TCP access tunnels and local service exposure using gtunnel CLI or MCP tools. Use when connecting to private GCP compute instances, databases (Postgres, Mongo, Redis), Kubernetes clusters, or debugging tunnel connectivity.
---

# gtunnel

## Overview

Reach private GCP ports for the user, or publish a local service at an HTTPS URL. Prefer a TTL run for one-off access, and prove health before reporting done. gtunnel forwards through Google Cloud Identity-Aware Proxy (IAP) with no external IP on the target and no VPN, and supervises persistent tunnels as background services (launchd on macOS, systemd on Linux).

Key capabilities:
- Persistent background access tunnels to GCP instances, destination groups, and bastion jump hosts.
- Multi-account scoping with independent daemon management per account.
- Active TCP health probing and automated daemon supervision.
- Local service exposure through a GCP relay instance behind a global HTTPS load balancer.
- Native Model Context Protocol (MCP) tool integration and machine-readable CLI commands.

## Hybrid Tool Routing

When executing tunnel operations, select tools according to availability:

### 1. Primary: Native MCP Tools
If `gtunnel_*` MCP tools are available in the current environment, prioritize them over CLI commands:
- `gtunnel_list_tunnels`: List configured tunnels and live runs, each with `status`, `local_port` (`null` on a public tunnel) and `health` (`healthy`, `unhealthy`, `auth_expired`, `stopped` or `unknown`). One call answers whether every tunnel works; call `gtunnel_get_status` only for the pid, restarts or probe latency of one.
- `gtunnel_start_tunnel`: Start a tunnel daemon.
- `gtunnel_stop_tunnel`: Stop a running tunnel daemon, or a live run.
- `gtunnel_restart_tunnel`: Restart a tunnel daemon.
- `gtunnel_get_status`: Inspect `status`, `health` (`healthy`, `unhealthy`, `auth_expired`, `stopped` or `unknown`), process PID, restart count, and active probe latency, for a tunnel or a live run (`"run": true`, with `local_port` and `expires_at`).
- `gtunnel_get_logs`: Read the last `lines` lines (default 50) of the log of a tunnel or of a run launched by `gtunnel_run_tunnel`. It returns the raw lines: you read the error code in them.
- `gtunnel_add_tunnel`: Create and register a persistent access tunnel.
- `gtunnel_run_tunnel`: Open a short-lived access tunnel that closes by itself after `ttl_minutes` (1 to 480, required). Never public. `gtunnel_stop_tunnel` closes it early.
- `gtunnel_delete_tunnel`: Delete a tunnel and remove its daemon configuration.
- `gtunnel_rename_tunnel`: Rename a tunnel (`name`, `new_name`); its service and log follow, and a running tunnel restarts under the new name. A name already in use is refused.
- `gtunnel_expose_tunnel`: Publish a local service at `https://<name>.<domain>` through the account's relay (`name`, `target`, optional `domain`, `public`, `start`). Private by default, behind Google sign-in; this is the reverse of the CLI. Pass `public: true` only when the user asked for anyone on the internet to reach it, and confirm first. It publishes only a service on this machine, so refuse any other host and point at the CLI. It never creates a relay: an account without one fails, and `gtunnel relay up` is CLI only.
- `gtunnel_move_tunnel`: Move an Access tunnel to another configured account (`name`, `to`, optional `rename` when the destination already uses the name). The destination must already be logged in; a failure of kind `auth` means the user must run `gcloud auth login` for that account, since MCP cannot prompt. Confirm with the user first.
- `gtunnel_list_accounts`: List configured GCP accounts and the active selection (`selected_account`, and `selected` on each account). The `gtunnel://accounts` resource returns the same object.
- `gtunnel_doctor`: Run diagnostic checks on prerequisites, permissions, and daemon configurations.

The JSON tools (`gtunnel_list_tunnels`, `gtunnel_get_status`, `gtunnel_run_tunnel`, `gtunnel_expose_tunnel`, `gtunnel_list_accounts`, `gtunnel_doctor`) also return `structuredContent`; the list is `{"tunnels": [...]}` and the doctor `{"findings": [...]}`. A failure is `{"error": {"kind", "message"}}`. Tools annotated `readOnlyHint` change nothing, so call them freely.

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
- Allowed characters: Lowercase alphanumeric and hyphens (`[a-z0-9-]`), up to 40 characters.
- Prohibited: NEVER include environment or account suffixes (do not use `-dev`, `-prod`, `-staging`, `-test`).
- Account Scoping: gtunnel automatically scopes tunnels by account. A tunnel named `kubernetes` in account `dev` and a tunnel named `kubernetes` in account `prod` are separate, isolated tunnels.

### Examples

| Target Type | Valid Name | Invalid Name | Reason |
|-------------|------------|--------------|--------|
| Kubernetes API | `kubernetes` | `kubernetes-dev` | Redundant environment suffix |
| Primary Database | `postgres` | `postgres-prod` | Redundant environment suffix |
| Cache Service | `redis` or `redis-cache` | `redis-staging` | Redundant environment suffix |
| Session Store | `redis-session` | `redis-session-prod` | Redundant environment suffix |
| Document Database | `mongodb` | `mongo-prod` | Redundant environment suffix |

The `<service>` suffix is used only to distinguish multiple instances within the same platform and account. If an account has a single Redis instance, use `redis`. If an account has multiple Redis instances, distinguish them by service purpose (`redis-cache`, `redis-session`).

## Target Selection

Configure tunnel destinations according to network topology:

### 1. Direct Compute Instance
Connect directly to a Compute Engine virtual machine:
```bash
gtunnel add db bastion:5432 -z europe-west1-b
```
Use `-z <zone>` if the instance is not in the account's default zone.

### 2. Destination Group (Internal IP via IAP)
Connect to an internal IP routed through a GCP IAP destination group:
```bash
gtunnel add private-api 10.0.0.5:5432 --dest-group prod --region europe-west1
```

### 3. Jump Host (Bastion)
Forward traffic through a bastion instance into a private VPC subnet:
```bash
gtunnel add k8s-api 10.0.0.10:443 --via bastion
```

### 4. Client-Side Routing (MongoDB Compass and Bastion Forwarding)
- Create one tunnel to the bastion rather than separate tunnels per backend host or database shard.
- For MongoDB Compass, forward a local port to the bastion's SSH port:
  ```bash
  gtunnel add mongodb bastion:22 --local-port 10022
  ```
- Configure MongoDB Compass to use an SSH Tunnel via `localhost:10022` with your SSH identity key, and put the cluster URI in Compass. Never configure Compass as an SSL Proxy, and avoid pinning replica-set member hostnames.

## Account Scoping

gtunnel isolates configurations, state, and daemon plists/services per account:
- Run commands against a specific account using `--account <name>`:
  ```bash
  gtunnel list --account dev
  gtunnel start redis --account prod
  ```
- Or switch active account context:
  ```bash
  gtunnel account select dev
  ```
- With MCP tools, pass the `account` parameter:
  ```json
  {"name": "redis", "account": "prod"}
  ```

## Port Allocation

### Automatic Allocation
If `--local-port` is omitted, gtunnel derives a free local port from the tunnel name, in 15000-15999.

### Same Name, Different Port per Account
Both accounts' tunnels run on one machine, so the same tunnel name takes a different `--local-port` in each account:

| Tunnel | Account `dev` | Account `prod` |
|--------|---------------|----------------|
| `k8s` | `--local-port 8443` | `--local-port 8444` |

## Verification Protocol Before Completion

Never assume a tunnel is operational. Before reporting completion of tunnel creation or startup, produce live verification evidence across three steps:

### Step 1: Daemon State Verification
Read `isError` on every mutating MCP result before anything else: `true` means the call failed, whatever the text says, and a start that did not come up is one. Then check that the daemon status reports `running`:
- MCP: Call `gtunnel_get_status` and verify `"status": "running"`, a non-zero PID and `"health": "healthy"`. `auth_expired` is a credentials problem (`gcloud auth login`), not a tunnel one; `unhealthy` means read the log.
- CLI: Run `gtunnel list --json` and verify the tunnel's row has `"status": "running"` and `"health": "healthy"`, read the same way as above.

### Step 2: Log Inspection
Inspect the last 10 log lines to ensure the connection initialized without errors:
- MCP: Call `gtunnel_get_logs` with `lines: 10`.
- CLI: Run `gtunnel logs <name> --lines 10`.
- Check for common handshake and IAP failures:
  - Error code `4033`: Missing `roles/iap.tunnelResourceAccessor` IAM permission.
  - Token refresh failure: gcloud credentials expired or invalid configuration.
  - Connection refused: Target service not listening on specified port.

### Step 3: Active TCP Probe
Confirm the local listener is active and accepting connections:
```bash
nc -z -v -w 3 127.0.0.1 <port>
```
Or read `probe_latency_ms` from `gtunnel_get_status` (MCP): a number means the local port accepted a connection.

## Troubleshooting

### Diagnostics (`gtunnel doctor`)
Run system diagnostics to verify credentials, launchd/systemd agent state, and configurations:
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

When testing or debugging MCP tool integrations:

### 1. Built-in Schema Validation
Run the native validator to verify all registered tool schemas, argument definitions, and descriptions:
```bash
gtunnel mcp --validate
```
Exits with code 0 on success. Exits with non-zero code if any tool schema is invalid.

### 2. MCP Inspector Testing
Test MCP protocol communication and tool definitions interactively using the official inspector:
```bash
npx @modelcontextprotocol/inspector gtunnel mcp
```

## Safety Rules

- Confirmation Required: Always obtain confirmation before deleting a tunnel or modifying configuration (`gtunnel delete <name>` or `gtunnel_delete_tunnel`), and before renaming, moving or exposing one (`gtunnel_rename_tunnel`, `gtunnel_move_tunnel`, `gtunnel_expose_tunnel`).
- Data Protection: Never leak real GCP project IDs, internal IP addresses, personal email addresses, or proprietary tokens in generated documentation, logs, or commit messages. Always use placeholders (`example-dev-1a2b`, `you@example.com`, `bastion`, `10.0.0.10`).
- No Emojis: Plain text only across all commands, documents, and tool responses.
- Idempotent Modifications: To rename a tunnel use `gtunnel rename <old> <new>`, and to move an Access tunnel to another account `gtunnel move <name> --to <account>`; both carry the service and log along. To change other tunnel parameters, delete the existing tunnel first to avoid orphaned launchd/systemd agent configurations, then add the new configuration.
