# Changelog

Notable changes per release. Dates are the release date; an unreleased section is
what is on `main` and not yet tagged.

## v0.1.0 - unreleased

First release. gtunnel is a port of [ytunnel](https://yetidevworks.com/ytunnel) to GCP
Identity-Aware Proxy: the same two modes and the same TUI-first shape, with IAP and a
GCP relay in place of Cloudflare Tunnel.

### Access mode

- **IAP port forwarding as a managed service** - `gtunnel add db my-vm:5432` gives a
  local port that reaches a VM inside the VPC, supervised by launchd or systemd so it
  survives a crash and, optionally, a reboot.
- **Destination groups** - `--dest-group` targets an IAP destination group, which
  reaches a host that is not a compute instance.
- **Stable local ports** - the port is derived from the tunnel name, so the same tunnel
  gets the same port on every machine unless it is taken.
- **Nothing is created in GCP** - Access mode makes no API call beyond reading, so a
  tunnel costs nothing and leaves nothing behind.

### Public mode

- **A public HTTPS URL for a local service** - `gtunnel expose web 3000` publishes
  `https://web.<domain>` through a relay VM, a global load balancer and Cloud DNS.
- **One relay per account** - the VM, load balancer, certificate map and firewall rules
  are created once by `gtunnel relay up` and shared by every tunnel on the account.
- **Managed certificates** - `gtunnel domains add` requests a certificate for the zone
  and its wildcard and writes the DNS authorization record.
- **IAP in front of the load balancer** - `expose --private` requires a Google sign-in
  before a visitor reaches the tunnel.
- **Self-healing routes** - starting a public tunnel re-creates its backend, host rule,
  named port and DNS record if any of them went missing, so `restart` fixes a tunnel
  that comes up serving a 503.

### TUI

- **Live dashboard** - tunnel list, details, logs and metrics in one screen, with
  latency, connection counts and load-balancer request classes.
- **Full lifecycle from the keyboard** - add, edit, start, stop, restart, delete,
  auto-start, copy and open, without leaving the TUI.
- **Unmanaged tunnels** - a `gcloud start-iap-tunnel` process started outside gtunnel
  is listed and can be imported with `m`; every other action on it is refused.
- **List columns** - the tunnel list has a `NAME  MODE  URL` header and fixed columns,
  including one for the auto-start and health flags. It hides a name suffix equal to the
  tunnel's account and cuts names to 12 cells; Details shows the full name, and
  `gtunnel list` is unchanged.
- **Health notifications** - a tunnel going unhealthy raises a system notification.
- **`gtunnel demo`** - the whole dashboard filled with synthetic data, touching no GCP
  project and no config file.

### Operations

- **Ephemeral runs** - `gtunnel run` holds a tunnel in the foreground and tears down
  every route it created on exit, including on Ctrl+C and on a crash.
- **`gtunnel doctor`** - reports stale records, dead runs, orphaned relay resources,
  missing IAP firewall rules, broken credentials and a gcloud Python without NumPy.
  `--fix` removes only what is tagged `gtunnel:<account>` and unclaimed.
- **Ownership by tag, not by name** - every resource gtunnel creates carries
  `description: gtunnel:<account>`, so a hand-made resource whose name matches the
  scheme is never deleted.
- **Multiple accounts** - one gcloud configuration, project, zone and relay per account.
- **Self-update** - `gtunnel update` verifies the published SHA-256 before replacing the
  binary, and defers to Homebrew or cargo when that is how it was installed.

### Startup

- **Login check** - gtunnel probes the selected account's gcloud credentials before the
  TUI or any GCP command starts, runs `gcloud auth login` on a terminal when they are
  missing or expired, and exits 1 if the account is still not logged in afterwards.
  Without a terminal it exits 1 instead of prompting. `doctor`, `account`, `init`,
  `reset`, `demo` and `update` skip it.

### Platforms

- **macOS** - launchd user agents. Auto-start is the presence of the plist in
  `~/Library/LaunchAgents`; the definition gtunnel loads and unloads itself lives
  outside that directory, because launchd runs a `KeepAlive` job the moment it is
  loaded.
- **Linux** - systemd `--user` units. `loginctl enable-linger` is what makes a tunnel
  survive a logout.
- **Release binaries** - `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`.
