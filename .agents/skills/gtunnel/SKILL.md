---
name: gtunnel
description: Manage GCP IAP TCP access tunnels and local service exposure using gtunnel CLI or MCP tools. Use when connecting to private GCP compute instances, databases (Postgres, Mongo, Redis), Kubernetes clusters, or debugging tunnel connectivity.
---

# gtunnel

## Overview

gtunnel manages Google Cloud Identity-Aware Proxy (IAP) TCP access tunnels and local service exposure. It coordinates persistent background daemons (via launchd on macOS or systemd on Linux) to provide reliable, monitored port forwarding to private GCP resources without requiring external IP addresses or traditional VPNs.

Key capabilities:
- Persistent background access tunnels to GCP VMs, destination groups, and bastion jump hosts.
- Multi-account scoping with independent daemon management per account.
- Active TCP health probing and automated daemon supervision.
- Local service exposure via Cloudflare tunnels or Google Cloud Run.
- Native Model Context Protocol (MCP) tool integration and machine-readable CLI commands.

## Hybrid Tool Routing

When executing tunnel operations, select tools according to availability:

### 1. Primary: Native MCP Tools
If `gtunnel_*` MCP tools are available in the current environment, prioritize them over CLI commands:
- `gtunnel_list_tunnels`: List configured tunnels, daemon states, and listening ports.
- `gtunnel_start_tunnel`: Start a tunnel daemon.
- `gtunnel_stop_tunnel`: Stop a running tunnel daemon.
- `gtunnel_restart_tunnel`: Restart a tunnel daemon.
- `gtunnel_get_status`: Inspect runtime status, process PID, restart count, and active probe latency.
- `gtunnel_get_logs`: Read recent daemon stdout/stderr log lines.
- `gtunnel_add_tunnel`: Create and register a persistent access tunnel.
- `gtunnel_delete_tunnel`: Delete a tunnel and remove its daemon configuration.
- `gtunnel_list_accounts`: List configured GCP accounts and the active selection.
- `gtunnel_doctor`: Run diagnostic checks on prerequisites, permissions, and daemon configurations.

### 2. Fallback: Shell Commands
If MCP tools are not configured, use the CLI directly:
- List tunnels: `gtunnel list` (or `gtunnel list --json`)
- Start tunnel: `gtunnel start <name>`
- Stop tunnel: `gtunnel stop <name>`
- Restart tunnel: `gtunnel restart <name>`
- Status check: `gtunnel status <name>`
- Read logs: `gtunnel logs <name> --lines <N>`
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
gtunnel add my-vm-db my-vm:5432 -z europe-west1-b
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
- For MongoDB Atlas / Compass access, forward to the bastion SSH port (port 2222 in dev, port 2223 in prod):
  ```bash
  gtunnel add mongodb bastion:22 -p 2222
  ```
- Configure MongoDB Compass to use an SSH Tunnel via `localhost:2222` with your SSH identity key. Never configure Compass as an SSL Proxy, and avoid pinning replica-set member hostnames.

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
If `--port` is omitted during tunnel creation, gtunnel automatically assigns an available local port.

### Standard Port Conventions

| Platform | Dev Port | Prod Port | Purpose |
|----------|----------|-----------|---------|
| Kubernetes | 8443 | 8444 | Kubernetes API server |
| MongoDB / Bastion | 2222 | 2223 | Bastion SSH jump host |
| Redis (single instance) | 3379 | - | Dev Redis |
| Redis Session | - | 3380 | Prod session store |
| Redis Cache | - | 3381 | Prod cache |
| Redis Locker | - | 3382 | Prod distributed locks |
| Redis Realtime | - | 3383 | Prod pub/sub |
| Redis Storage | - | 3384 | Prod persistent store |

## Verification Protocol Before Completion

Never assume a tunnel is operational. Before reporting completion of tunnel creation or startup, produce live verification evidence across three steps:

### Step 1: Daemon State Verification
Check that the daemon status reports `running`:
- MCP: Call `gtunnel_get_status` and verify `"status": "running"` and non-zero PID.
- CLI: Run `gtunnel list` or `gtunnel status <name>` and verify state is `running`.

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
Or verify the active probe output reported by `gtunnel status <name>`.

## Troubleshooting

### Diagnostics (`gtunnel doctor`)
Run system diagnostics to verify credentials, launchd/systemd agent state, and configurations:
```bash
gtunnel doctor
```
Or via MCP: `gtunnel_doctor`.

### Supervision & Health Probes (`gtunnel watch`)
View continuous health check status, reconnect attempts, and probe latencies:
```bash
gtunnel watch
```

### Inspecting Daemon Logs
Access log files directly when detailed debugging is required:
- macOS: `~/Library/Application Support/gtunnel/logs/<account>.<name>.log`
- Linux: `~/.config/gtunnel/logs/<account>.<name>.log`
- Or use `gtunnel logs <name> --lines 50`.

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

- Confirmation Required: Always obtain confirmation before deleting a tunnel or modifying configuration (`gtunnel delete <name>` or `gtunnel_delete_tunnel`).
- Data Protection: Never leak real GCP project IDs, internal IP addresses, personal email addresses, or proprietary tokens in generated documentation, logs, or commit messages. Always use placeholders (`example-dev-1a2b`, `you@example.com`, `bastion`, `10.0.0.10`).
- No Emojis: Plain text only across all commands, documents, and tool responses.
- Idempotent Modifications: When changing tunnel parameters, delete the existing tunnel first to avoid orphaned launchd/systemd agent configurations, then add the new configuration.
