# Architecture

## Access mode

A local port forwarded to a port inside the VPC. Nothing is created in GCP: IAP already
accepts the connection, and the only requirement is a firewall rule that lets IAP's range
reach the target port.

Both inward paths, in one picture: the upper one is access mode, the lower one is the jump
tunnel described in the next section.

![Access and jump tunnels: a client on localhost reaching a VM through IAP, and a second client reaching a host behind a bastion](images/architecture/gtunnel-access-jump.png)

The service runs:

```
gcloud compute start-iap-tunnel <instance> <remote-port> \
  --local-host-port=localhost:<local-port> --zone=<zone> --verbosity=info \
  [--project ...] [--impersonate-service-account ...]
```

`--verbosity=info` is what makes a working tunnel say so: gcloud's per-connection lines are
`log.info` and its default level is `warning`. It is shown here at the default level; both
it and ssh's `LogLevel` below follow the configured one, which
[troubleshooting.md](troubleshooting.md#one-level-two-scales) maps. The service also sets
`PYTHONUNBUFFERED=1`,
because gcloud is Python and Python block-buffers a stdout that is not a terminal, so without
it a line waits for 4 KiB or for the process to exit.

With `--dest-group` the target is an IAP destination group instead of an instance, which
reaches a host that is not a compute instance.

## Jump tunnels

`--via <instance>` reaches a host that only that instance can reach: a private GKE control
plane, a managed database on a private address, anything inside the VPC with no IAP
endpoint of its own. It is the lower path in the diagram above: IAP stops at the bastion,
and the hop from there to `db.internal` is an `ssh -L` forward made from the far side.

This is a different gcloud command, not a different flag. IAP carries an SSH connection and
SSH makes the forward, so the service runs:

```
gcloud compute ssh <instance> --zone=<zone> --tunnel-through-iap --quiet \
  --verbosity=info [--project ...] -- \
  -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
  -o LogLevel=DEBUG -L <local-port>:<host>:<remote-port>
```

`--verbosity` governs gcloud, not the `ssh` it execs, so the forward needs its own level,
and the two are spelled on different scales. One setting drives both: the mapping, and why
the default reaches ssh as `DEBUG` rather than `VERBOSE`, is in
[troubleshooting.md](troubleshooting.md#one-level-two-scales).

Two consequences follow from the command being `ssh` rather than `start-iap-tunnel`:
there is no local listener inside gcloud, so nothing prints `Listening on port`; and the
keepalive options are what notice a dead bastion, since SSH will otherwise hold a forward
open against a host that is gone.

## Public mode

A relay VM holds a reverse SSH tunnel back to your laptop. A global HTTPS load balancer
terminates TLS and sends each hostname to the relay port that belongs to that tunnel.

![Public mode: a visitor reaching a global HTTPS load balancer, which forwards to a relay VM holding a reverse SSH tunnel back to the laptop, with IAP sign-in in front for a gated tunnel](images/architecture/gtunnel-public-mode.png)

The load balancer is drawn as one card. Behind it a request crosses five resources, which
[What belongs to whom](#what-belongs-to-whom) below lists in full: the forwarding rule
`gt-<acct>-fr`, the HTTPS proxy `gt-<acct>-https` with cert map `gt-<acct>-certs`, the URL
map `gt-<acct>-urlmap` carrying one host rule per tunnel, the per-tunnel backend service
`gt-<acct>-<tunnel>`, and the instance group `gt-<acct>-ig` whose named port `gt-<tunnel>`
selects the relay port. The Cloud DNS A record `<tunnel>.<domain>` resolves to the global
address `gt-<acct>-ip`.

A gated tunnel, `expose --private`, also turns on IAP for its backend service, so a visitor
signs in with Google before a request reaches the relay. `list` and the TUI show its mode
as `gated` rather than `public`.

The tunnel process runs:

```
gcloud compute ssh <relay-vm> --zone=<zone> --tunnel-through-iap --quiet -- \
  -N -o ExitOnForwardFailure=yes -R 0.0.0.0:<relay-port>:localhost:<local-port>
```

`ExitOnForwardFailure=yes` matters: without it SSH stays up after the remote bind fails and
the tunnel looks healthy while serving nothing. The relay VM's startup script sets
`GatewayPorts clientspecified` so the `0.0.0.0` bind is accepted, which is what lets the load
balancer's health check reach the port.

## What belongs to whom

| Scope | Resource | Name | Created by |
|---|---|---|---|
| Per account | Relay VM | `gt-<acct>-relay` | `relay up` |
| Per account | Instance group | `gt-<acct>-ig` | `relay up` |
| Per account | Health check | `gt-<acct>-hc` | `relay up` |
| Per account | Default backend | `gt-<acct>-default` | `relay up` |
| Per account | URL map | `gt-<acct>-urlmap` | `relay up` |
| Per account | HTTPS proxy | `gt-<acct>-https` | `relay up` |
| Per account | Global address | `gt-<acct>-ip` | `relay up` |
| Per account | Forwarding rule | `gt-<acct>-fr` | `relay up` |
| Per account | Firewall, IAP to SSH | `gt-<acct>-iap-ssh` | `relay up` |
| Per account | Firewall, health check to 30000-30999 | `gt-<acct>-lb-hc` | `relay up` |
| Per account | Certificate map | `gt-<acct>-certs` | `relay up` |
| Per domain | Certificate and DNS authorization | `gt-<acct>-<domain-slug>`, `...-auth` | `domains add` |
| Per tunnel | Backend service | `gt-<acct>-<tunnel>` | `expose`, `run --public` |
| Per tunnel | Instance-group named port | `gt-<tunnel>` | `expose`, `run --public` |
| Per tunnel | URL-map host rule | the hostname | `expose`, `run --public` |
| Per tunnel | Cloud DNS A record, TTL 60 | `<tunnel>.<domain>.` | `expose`, `run --public` |

Both firewall rules target the tag `gt-relay`, which only the relay VM carries. Every
backend service gtunnel creates carries `description: gtunnel:<account>`, and that tag,
not the name, is what `doctor --fix` uses to decide whether a backend is gtunnel's. A
backend someone created by hand that happens to match the naming scheme is never deleted.

Port ranges: local ports 15000-15999, relay ports 30000-30998. Relay port 30999 is the
default backend, which has no listener, so an unknown hostname gets a 503 instead of
reaching another tunnel.

### What `relay up` creates and `relay down` removes

![What relay up creates: on your laptop, gtunnel relay up creates the per-account resources once (HTTPS front end with global address, forwarding rule and HTTPS proxy; URL map with default backend and health check; relay VM and its instance group; firewall rules gt-ACCT-iap-ssh and gt-ACCT-lb-hc; certificate map gt-ACCT-certs), gtunnel domains add creates the per-domain certificate, DNS authorization and apex and wildcard certificate-map entries, and gtunnel expose or run --public creates the per-tunnel backend service and named port, URL-map host rule and Cloud DNS A record with TTL 60, all in GCP example-prod-3c4d; the certificate-map entries are added to the certificate map and the host rule to the URL map](images/architecture/gtunnel-relay-resources.png)

Only the per-tunnel backend service carries `description: gtunnel:<account>`. `doctor --fix`
treats a host rule on the relay's URL map as orphaned when no tunnel claims its hostname, and a
named port on the relay's instance group when it starts `gt-` and no tunnel claims it. A Cloud
DNS record has no description field, so gtunnel treats one as its own only while it still
holds a tagged backend, a host rule or a named port for the same tunnel.

`relay down` removes the per-account and per-domain halves, in dependency order: forwarding
rule, HTTPS proxy, global address, URL map, default backend, health check, each domain's
certificate-map entries, certificate and DNS authorization, the certificate map, the instance
group, the relay VM, then both firewall rules. It **refuses** while any expose tunnel or live
`gtunnel run --public` still holds a route — `--force` deletes those tunnels' routes and
services first. It never touches the Cloud DNS managed zone, which is yours.

## Process model

![Process model: on your laptop the gtunnel CLI / TUI reads and writes tunnels.toml and config.toml, installs one per-tunnel service (launchd plist / systemd unit) that runs gtunnel __supervise, which runs the gcloud child process and writes the tunnel log logs/&lt;account&gt;-&lt;name&gt;.log; the CLI reads that log and polls the Compute, Cloud DNS, Certificate Manager, IAP and Monitoring REST APIs in GCP; one machine watchdog (launchd StartInterval / systemd timer) runs gtunnel watch --once, which probes each service, writes breakers.toml and writes the log](images/architecture/gtunnel-process-model.png)

| Concern | Decision |
|---|---|
| Supervision | launchd on macOS, systemd `--user` on Linux. Both restart on failure |
| Reboot | Auto-start is per tunnel; off by default, toggled with `A` in the TUI. On macOS it is the presence of the plist in `~/Library/LaunchAgents`, because launchd loads everything there at login and a `KeepAlive` job runs the moment it is loaded. The definition gtunnel starts and stops itself lives outside that directory |
| Wedged tunnels | Supervision only catches a process that exits. A tunnel that stays up and refuses connections is caught instead by one watchdog job for the whole machine, which wakes every 30s, makes one pass over every tunnel and exits. It is an interval job rather than a supervised one precisely because it is supposed to exit every time |
| PATH | A daemon does not inherit a shell, so gcloud's directory is written into the unit. Without it the service dies at every start |
| Credentials | None stored. Each API call shells out to `gcloud auth print-access-token` for the account's configuration |
| Ephemeral runs | `run` writes a record with its pid, so `list`, the TUI and `doctor` see it. Every exit path tears the route down, including signals |
| TUI polling | Discovery, metrics and health are three pollers, each with at most one round in flight: a round slower than its period is waited for, never started again on top of itself. A result carries the generation its round began under and is dropped when that is no longer current, which is what an account switch does to every round begun for the old account. Every `gcloud`, `launchctl` and `systemctl` call is bounded (30s and 10s) and fails with `<command> timed out after <n>s`; the child is killed on timeout. The `lsof` and `ps` reads run on the blocking pool. `delete` treats a timed-out stop as a failure and keeps the record, because removing it would orphan a job that may still be running |
| Strangers | A `[not managed by gtunnel]` row is an IAP process nothing here accounts for. One `ps` snapshot is read, then a process is gtunnel's if it forwards a port that any account's `tunnels.toml` entry or any run record owns, or if it sits below a pid launchd or a run record names. The port is tried first because launchd reports a supervisor's pid before the supervisor has spawned its gcloud child, and a verdict that depends on that gap is a verdict that depends on timing |
| Concurrent writes | Read-modify-write against a fingerprinted resource (URL map, named ports) retries up to 5 times on HTTP 412 |
| Metrics | Latency every 5s, health every 5s, load-balancer request counts every 60s over a 10-minute window. Connections are sampled every 500ms from one `lsof` listing covering every tunnel, and the 5s figure is the peak of that window: a request shorter than the window would otherwise be missed entirely |

### Service lifecycle

One diagram for both platforms: the shape is the same and only the manager differs.

![Service lifecycle: gtunnel add records the tunnel in tunnels.toml (name, account, target, local port), gtunnel start or s in the TUI writes the definition labelled com.gtunnel.ACCOUNT.NAME, then on macOS launchctl bootstrap loads CONFIG/agents/LABEL.plist and on Linux systemctl --user start loads ~/.config/systemd/user/LABEL.service; both run the gcloud child process, which on a non-zero exit is restarted by KeepAlive or Restart=on-failure RestartSec=5; A toggles auto-start, which on macOS is ~/Library/LaunchAgents/LABEL.plist starting at login and on Linux is systemctl --user enable plus loginctl enable-linger](images/architecture/gtunnel-service-lifecycle.png)

The two auto-start mechanisms are not symmetric, and the asymmetry is deliberate. On macOS
the flag *is* the file: launchd loads everything in `~/Library/LaunchAgents` at login, and a
`KeepAlive` job runs the moment it is loaded, so a definition left there would start whether
or not anyone asked. That is why the copy gtunnel loads and unloads itself lives in
`<config>/agents/` instead. On Linux the unit always lives in one place and `enable` is a
separate switch, with `loginctl enable-linger` deciding whether it survives a logout.

## Files

| Path | Contents |
|---|---|
| `<config>/config.toml` | Accounts, default account, relay and domains per account |
| `<config>/tunnels.toml` | Persistent tunnels |
| `<config>/tunnels.lock`, `<config>/config.lock` | Empty files whose only job is to be locked. A command that changes `tunnels.toml` or `config.toml` holds the matching lock from its read to its write, so two gtunnels cannot drop each other's change. Safe to delete when no gtunnel is running |
| `<config>/run/<account>-<name>.json` | One record per live `gtunnel run` |
| `<config>/run/<label>.ready` | Written by a tunnel's supervisor once its log reports the listener; what `start` waits for |
| `<config>/run/probes/` | One file per local port gtunnel is probing, so the supervisor leaves gtunnel's own health checks out of the `conn` lines |
| `<config>/run/watchdog.heartbeat` | Time of the watchdog's last pass. `watch status` reports its age; while it is under 90s old the TUI leaves notifications to the watchdog |
| `<config>/lifecycle.json`, `<config>/lifecycle.lock` | The last state each tunnel was seen in, shared by the watchdog, the TUI and the CLI so that a change is notified once. The lock serialises its updates |
| `<config>/agents/<label>.plist` | macOS: the service definition gtunnel loads and unloads |
| `~/Library/LaunchAgents/<label>.plist` | macOS: present only while auto-start is on |
| `<config>/breakers.toml` | One row per tunnel: what the watchdog has seen of it, and how far up the restart ladder it is. Rewritten whole on every pass, by writing a sibling file and renaming it, and pruned to the tunnels that still exist |
| `<config>/logs/watchdog.log` | What the watchdog did. A pass where nothing needed doing writes nothing |
| `<config>/logs/<account>-<name>.log` | Per-tunnel log, written by the service |
| `<config>/update-check.json` | Last version check |

Every file gtunnel writes is written to a sibling `<name>.<pid>.new` and renamed into place,
so a crash or a full disk leaves the previous file whole, and with mode 0600 because the
contents name accounts, projects and hosts. No lock is held while a command waits on gcloud
or a service manager: the daemon work happens first, then the record is changed in one short
locked update ([decision 16](decisions.md#16-state-files-are-written-atomically-under-an-advisory-lock)).

`<config>` is `~/.gtunnel`. A legacy `~/Library/Application Support/gtunnel` on macOS or
`~/.config/gtunnel` on Linux is used only when it exists and `~/.gtunnel` does not.
`GTUNNEL_CONFIG_DIR` overrides both.

### The account and config model

![The account and config model: tunnels.toml holds dev / kubernetes and prod / kubernetes, each identified by name plus account_name, and each derives its own artifacts: launchd plist com.gtunnel.dev.kubernetes, log logs/dev-kubernetes.log and localhost:8443 for dev, and com.gtunnel.prod.kubernetes, logs/prod-kubernetes.log and localhost:8444 for prod; config.toml holds accounts (gcloud configuration, project, zone) and default_account, and each account has its relay (VM, zone, load balancer) and its domains](images/architecture/gtunnel-config-model.png)

A tunnel is identified by its name **and** its account, never by name alone, which is what
lets `kubernetes` exist once per account. Every artifact derived from a tunnel carries the
account in its own name — the launchd label, the log file, the run record — so two accounts'
copies never collide on disk or in the service manager. What must differ between them is the
local port, because both run on one laptop.

The practical consequence is a naming rule: a tunnel is named for what it connects to, never
for the account it lives in. An account suffix is redundant everywhere it could appear, and
it costs you the ability to use the same command in both accounts.
