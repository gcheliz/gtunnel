# Command reference

[Home](../README.md) / [Usage](usage.md) / Commands / [Configuration](configuration.md) / [Architecture](architecture.md) / [Troubleshooting](troubleshooting.md)

Contents:

- [init](#init)
- [add](#add)
- [expose](#expose)
- [run](#run)
- [start, stop, restart, autostart, logs, list, delete](#start-stop-restart-autostart-logs-list-delete)
- [rename and move](#rename-and-move)
- [zones and domains](#zones-and-domains)
- [relay](#relay)
- [account](#account)
- [watch](#watch)
- [doctor](#doctor)
- [reset](#reset)
- [update](#update)
- [Errors and exit codes](#errors-and-exit-codes)
- [Shell completions](#shell-completions)

Every command, its flags and its real output. The two modes, the target kinds and the
lifetime axis are defined once in [Concepts](usage.md#concepts); the files and environment
variables are in [configuration.md](configuration.md).

## init

Reads a gcloud configuration and writes a gtunnel account. It prompts for everything it
cannot infer. A bare `gtunnel` runs it too when there is no `config.toml` and stdin and
stdout are a terminal, then opens the TUI; without a terminal it prints the `not initialized`
error and exits 1.

```
$ gtunnel init
An account is a name gtunnel gives to one GCP project. It is built from a gcloud configuration (the profile `gcloud init` made: login, project, zone). Enter takes the value shown.

gcloud configurations:
  1. work (active)  project=example-dev-1a2b  account=you@example.com

  The gcloud configuration whose login, project and zone this account uses
  To list: gcloud config configurations list
Configuration number (Enter = 1): 
  What you pass to `--account`. Lower case, digits and dashes, e.g. `dev`
Account name (Enter = work): Could not list projects: Could not get a gcloud access token. No terminal to log in from. On a workstation run `gcloud auth login --configuration=work`; on a CI runner, give the runner a service account with roles/iap.tunnelResourceAccessor, or set credential_file on the account: gcloud auth print-access-token --configuration=work --project= --verbosity=info failed: ERROR: (gcloud.auth.print-access-token) Your current active account [you@example.com] does not have any valid credentials

  The GCP project every tunnel and API call of this account targets
  To list: gcloud projects list
Project (Enter = example-dev-1a2b): 
  Every gcloud call of this account acts as this service account; you need roles/iam.serviceAccountTokenCreator on it. Most people press Enter and act as themselves
Service account to impersonate (optional, Enter to skip): 
  Used when a tunnel names an instance without a zone
  To list: gcloud compute zones list
Default zone (optional, Enter to skip): Account 'sandbox' saved to ~/Library/Application Support/gtunnel
No default zone saved; pass --zone to commands that need one
```

The answers were `1`, `sandbox`, then Enter for the project, the service account and the zone, piped in, so they do not echo. This configuration has no zone, so Enter skipped it.

`init` also writes the `[restart]` section of `config.toml` with its five keys at their
defaults, adding only the keys that are missing and never changing one that is set, so the
[restart budget](configuration.md#the-restart-budget) is visible and tunable from the first
run. The same section is written by every command that saves the file.

Every prompt is preceded by a line saying what the value sets, and, for a value you pick from
something that exists, the command that lists it. The text is the one the TUI's account form
shows, from one source (`src/fields.rs`). In the brackets after the label, `Enter = <value>`
is the default and `optional, Enter to skip` marks an answer that may be empty. The helper
lines are plain text on stdout, so a piped answer is read exactly as before. The impersonation
line says what it does: every gcloud call of the account runs as that service account, which
needs `roles/iam.serviceAccountTokenCreator` granted to you; most people press Enter.

The project step lists the projects the logged-in account can see, one numbered row each
as `N. <project id>  <name>`, the gcloud configuration's own project marked `(default)`.
The prompt is `Project, by number or id`: a number picks that row, and anything else is taken
as the project id you typed. Typing stays possible whatever the list holds, so a project it
does not show still works.

| The list | What `init` does |
|---|---|
| Has rows | Prints them, asks `Project, by number or id` |
| Is empty, or the call fails | Prints `Could not list projects: <cause>` once, then asks `Project` as a typed field; the transcript above is this path |

The zone step works the same way: the project's zones are printed as `N. <zone>`, the
configuration's zone marked `(default)`, and `Default zone, by number or name`
takes a row number or a zone name. A zone the project does not have is refused. When the
zones cannot be listed, as in the transcript, the prompt is `Default zone`
with the same `optional, Enter to skip` hint.

The zone is optional, because many gcloud configurations have none. An account saved without
one works for everything except the commands that reach an instance by zone: `add --via` and
`add` to an instance, `run` to an instance, and `relay up`. Those stop with exit code 2 and
`no zone for account '<name>'` before any request is made, until you pass `--zone` or set a
default with `gtunnel account edit <name> --zone <zone>`. A tunnel through a destination group
(`--dest-group`) takes a region and needs no zone.

The list is fetched with the configuration's own credentials, and only projects in the
`ACTIVE` state are offered, up to 5,000. The cause is one line: a `403` means the account
lacks `resourcemanager.projects.get` or the Cloud Resource Manager API is not enabled, see
[troubleshooting.md](troubleshooting.md#could-not-list-projects).

An account stores a gcloud configuration name, a project, a default zone, an optional
service account to impersonate, and the relay once one exists. gtunnel never stores a
credential: every token comes from `gcloud auth print-access-token` at the moment it is used.

## add

```
gtunnel add <NAME> <INSTANCE:PORT> [-z ZONE] [--local-port N] [-s]
gtunnel add <NAME> <HOST:PORT> --via INSTANCE [-z ZONE] [--local-port N] [-s]
gtunnel add <NAME> <HOST:PORT> --dest-group G --region R --network N [-s]
```

| Flag | Effect |
|---|---|
| `-z`, `--zone` | Zone of the instance; the account default otherwise |
| `--local-port` | Fixed local port; otherwise one is allocated from 15000-15999, stable per name, skipping any a saved tunnel or a live run holds |
| `--via` | Reach a host the instance can reach, rather than a port of the instance |
| `--dest-group` | Target an IAP destination group instead of an instance |
| `-s`, `--start` | Install the service and start it immediately |

The local port is derived from the tunnel name, so the same name gets the same port on
every machine unless it is taken. A port held by a live `gtunnel run`, in any account,
counts as taken: an explicit `--local-port` for it is refused with `Local port P is already
used by the run 'NAME'`, and the automatic choice moves on to the next free one.

gcloud reads any argument that starts with a dash as an option, so `add` refuses one. An
instance is a GCE name (up to 63 lowercase letters, digits and dashes, starting with a letter),
a zone is lowercase letters, digits and dashes, and a host is a hostname or an IP address. The
error names the field and the value, and the same rule holds for a hand-edited `tunnels.toml`
and for the MCP tools.

### Naming a tunnel

A name is unique within its account, not across them, so the same service keeps the same
name in every account. The account is what you switch with `;` in the TUI or pass with
`--account`; repeating it in the name buys nothing and costs you a command that works in
only one account.

| | |
|---|---|
| Shape | `<platform>`, or `<platform>-<service>` when the platform has more than one |
| Leave out | the account, environment or project — no `-dev`, `-prod`, `-staging` |
| Allowed | up to 40 characters of `a-z`, `0-9` and `-`, starting with a letter and ending with a letter or digit |
| Refused | a name whose `<account>-<name>` another account already holds, e.g. `eu-api` on `prod` when `prod-eu` has `api`; both would write `prod-eu-api.log` |

```
gtunnel add k8s   … --account dev     # same name
gtunnel add k8s   … --account prod    # different account, different port
```

Both accounts then list the same platforms in the same order, and `gtunnel start k8s`
differs only in which account it names. A name that does carry its account still works —
the TUI hides the suffix — but every command spells it out.

A tunnel forwards to one host, so when the client picks its own destination over the
tunnel, as MongoDB Compass does over SSH, give the tunnel the bastion and let the client
choose. One tunnel per database behind it pins a hostname gtunnel has no reason to know.

### Reaching a host behind an instance

The three target kinds, their engines and what each needs are in
[Concepts](usage.md#three-kinds-of-access-target). Use `instance` for SSH or a database on the
VM itself, `--dest-group` for a host IAP can address directly, and `--via` for the rest.

An IAP TCP forward lands on a port of the instance itself, so a private endpoint behind a
bastion needs `--via`: IAP carries an ssh connection and ssh makes the forward from the far
side. A private GKE master, for example:

```
$ gtunnel add k8s 10.0.0.10:443 --via bastion -z europe-west1-b --local-port 8443
○ k8s  localhost:8443 -> 10.0.0.10:443 via bastion
```

The tunnel is installed and stopped; `-s` starts it in the same command.

The kubeconfig has to name the master in `tls-server-name`, because the API server's
certificate is issued for its own IP and not for `127.0.0.1`:

```yaml
    server: https://127.0.0.1:8443
    tls-server-name: 10.0.0.10
```

## expose

```
gtunnel expose <NAME> <TARGET> [-d DOMAIN] [--private] [-s]
```

`TARGET` is `3000` or `localhost:3000`. The URL is `https://<NAME>.<DOMAIN>`. `--private`
puts IAP in front of the load balancer, so a visitor must sign in with Google. That
sign-in only holds if the relay's own port range is closed to everything but the load
balancer: see [The relay's firewall is yours to close](#the-relays-firewall-is-yours-to-close).

Without a relay the command stops before changing anything:

```
$ gtunnel expose web 8000
Error: No relay for account 'dev'. Run `gtunnel relay up`
```

## run

An ephemeral tunnel in the foreground. It is recorded while it runs so `list`, the TUI and
`doctor` can see it, and every route it created is torn down on exit, including on Ctrl+C
and on a crash.

```
gtunnel run my-vm:5432                 # access, prints the local port
gtunnel run db my-vm:5432              # access, under a name
gtunnel run --public 3000              # https://gt-<random>.<domain>
gtunnel run my-vm:5432 --ttl 30m       # stops itself after 30 minutes
```

| | |
|---|---|
| Name | Refused when a live run or a managed tunnel in the account has it: `Error: A run named 'db' is already live for account 'dev'` |
| `--local-port` | The checks `add` makes, plus every live run of every account: refused when a tunnel or run already owns the port, when something on the machine listens on it, and for `0` |
| `--ttl` | Counted on the wall clock, so time asleep counts: a laptop that wakes past the stop time ends the run within a second |
| Stopping | Ctrl+C, SIGTERM, SIGHUP, the TTL and gcloud exiting all end the same way: SIGTERM to gcloud and its children, up to 2 seconds, then SIGKILL |

<p align="center">
  <img src="demo-run.gif" alt="gtunnel run --ttl 30m db my-vm:5432 prints the local port and its stop time, gtunnel list shows the run with its stop time, Ctrl+C closes it, and the list is empty again" width="720">
</p>

### run --ttl

`gtunnel run --ttl DURATION` stops the run by itself, through the same teardown as Ctrl+C: the
route and the record in `run/` go the same way.

| Form | Meaning |
|---|---|
| `90s`, `30m`, `2h` | Seconds, minutes, hours |
| `45` | A bare number is minutes |
| `0`, `1.5h`, `10d`, over `24h` | Refused before anything starts |

## start, stop, restart, autostart, logs, list, delete

```
$ gtunnel list
STATUS     NAME             MODE    URL                              TARGET
○ stopped  db               access  localhost:15432                  postgres-1:5432 (europe-west1-b)
● running  scratch          access  localhost:15999                  my-vm:5432 (europe-west1-b)  [run, stops at 14:32]
● running  pid 59670        access  -                                bastion-1:22 (europe-west1-b)  [not managed by gtunnel]
```

A live `gtunnel run` is listed with the mode it really is, `access` or `public`, and a marker
after the target: `[run, stops at 14:32]` (local time) when it has a TTL, `[run]` when it does
not. A run whose process is gone is not listed; `doctor` reports its leftover record.

A stopped tunnel whose supervisor gave up on expired credentials carries `[auth expired]` in
the same place, and `gtunnel_get_status` reports its `health` as `auth_expired`. Both clear on
the next `gtunnel start`.

A stopped tunnel whose supervisor spent its [restart budget](configuration.md#the-restart-budget)
carries `[restart budget spent: exit status: 1]`, the last thing gcloud died of, and the TUI
shows it as a crash loop. `gtunnel start` clears the marker and the count; `gtunnel watch`
never restarts it.

The TUI tags the same row `[run, stops at 14:32]`, or `[ephemeral]` without a TTL, and its
details pane counts down: `stops at  14:32 (in 25m)`.

Add `--json` for machine-readable output:

```
$ gtunnel list --json
[
  {
    "status": "stopped",
    "name": "db",
    "mode": "access",
    "url": "localhost:15432",
    "target": "postgres-1:5432 (europe-west1-b)",
    "account": "demo",
    "auto_start": false,
    "unmanaged": false,
    "run": false,
    "local_port": 15432,
    "health": "unknown"
  },
  {
    "status": "running",
    "name": "scratch",
    "mode": "access",
    "url": "localhost:15999",
    "target": "my-vm:5432 (europe-west1-b)",
    "account": "demo",
    "auto_start": false,
    "unmanaged": false,
    "run": true,
    "expires_at": 1793103233,
    "local_port": 15999,
    "health": "healthy"
  }
]
```

| Field | Type | Meaning |
|---|---|---|
| `status` | string | `running`, `stopped` or `error` |
| `name`, `account` | string | The tunnel, and the account it belongs to |
| `mode` | string | `access` or `public`, for a declared tunnel and a run alike |
| `url` | string | What to connect to: `localhost:<port>` or `https://<hostname>` |
| `target` | string | What sits on the other end |
| `auto_start` | bool | Starts at login; always false on a run |
| `unmanaged` | bool | An IAP tunnel gtunnel did not start, listed as `pid N` |
| `run` | bool | A `gtunnel run`, which ends by itself |
| `expires_at` | number | Unix seconds at which a run started with `--ttl` stops; absent otherwise |
| `local_port` | number or null | The local port to connect to; `null` on a public tunnel |
| `health` | string | `healthy`, `unhealthy`, `auth_expired` or `unknown` |

Only a running row with a local port is probed, and every such row is probed at once, so
`list --json` waits at most about 750 ms however many tunnels run: one 500 ms probe, plus
250 ms to read the log on a silent close. A stopped tunnel, a
public one and an unmanaged one are `unknown`. `gtunnel_list_tunnels` returns the same rows
less the unmanaged ones, so its `unmanaged` is always `false`.

```
$ gtunnel logs nope
Error: Tunnel 'nope' not found for account 'dev'
```

`logs` and `stop` take the name of a live run when no saved tunnel has it, and a saved tunnel
wins when both exist. A run launched by an MCP client has a log, `logs/run-<account>-<name>.log`;
a run started in a terminal prints there instead and has none, so `logs` says so:
`Run 'name' has no log: it was started from a terminal, which has its output`.
Each launch starts the log fresh, and the log stays after the run ends, so `logs` reads a finished
run until its name is launched again.

```
$ gtunnel autostart db           # ask
db auto-start is off
$ gtunnel autostart db --on      # set
db auto-start is on
```

`autostart` with no flag reports and changes nothing. `--on` and `--off` write the record and
bring the service manager to it, which is the same thing `A` does in the TUI. When the two
disagree — a plist left at login by an older gtunnel, say — `gtunnel doctor` reports the
drift and `doctor --fix` reconciles the service manager to the record.

A start waits for the tunnel to listen. An Access tunnel, `--via` included, is given up to
20 seconds (`GTUNNEL_START_DEADLINE_MS`) and passes as soon as its log reports the listener;
a public tunnel has no listener to wait for, so it is judged after a fixed 3 seconds. A
tunnel whose service failed, or that is not listening by the deadline, is a failed start:
the command prints the lines below on stdout, exits 1, and the record stays. The hint line appears when the log holds a line gtunnel recognises, and
the TUI status line carries the same text.

```
$ gtunnel start db
✗ db failed to start
  Logs: gtunnel logs db
$ echo $?
1
```

`add -s` and `restart` start the tunnel the same way, so a tunnel `add -s` could not start is
still added and the command exits 1.

`logs -f` follows. `restart` rewrites the service file before starting, so it also picks up
an edited target. Starting a tunnel empties its log first, so what the pane shows belongs to
the run in front of you; `L` in the TUI clears the selected tunnel's log without restarting it. `delete` removes the relay route first, then the service, then the record and the log.

A tunnel's log opens with the tunnel's own name and target, and gtunnel adds a line of its
own for every start, stop and clear. A tunnel reached through a jump host prints nothing
else while it is healthy, so without those lines its log was empty whether it had just
started, failed, or never existed at all.

#### Connections in the log

At log level `info` or `debug` the supervisor writes one line when a client connects and one
when it leaves, in the same shape for an IAP tunnel and a jump tunnel. The source port is the
client's local port, so it names the process: `lsof -nP -iTCP:54321` finds it while it is open.

```
2026-10-27T10:15:02Z gtunnel: conn iap-7 open
2026-10-27T10:15:14Z gtunnel: conn iap-7 from 127.0.0.1:54321 closed after 12.0s
```

| Rule | Why |
|---|---|
| The open is written after two seconds, or with the close if that comes first | gtunnel's own health probe connects and closes in milliseconds; its source port is only known at the close |
| A probe by gtunnel writes no line | `tcp_probe` records its source port in `run/probes/<port>` before it closes, and the supervisor drops the matching close. A probe by anything else, `nc -z` included, is a connection |
| At most 60 connection lines a minute | a client that reconnects in a loop would bury the lines that explain a failure; the rest become `gtunnel: conn ... N more in the last minute` |
| A jump tunnel counts only ssh channels, an instance tunnel only IAP connections | a jump tunnel's ssh rides on gcloud's own IAP connection to the bastion, which is the tunnel and never closes |
| `quiet` writes none | and ssh and gcloud print none at that level either |
| Only the supervisor writes them | the TUI never appends to a tunnel's log, so two open TUIs, or none, give the same log |

Starting a public tunnel re-creates its backend, host rule, named port and DNS record if any
of them went missing, so `gtunnel restart <name>` is the fix for a tunnel that comes up
serving a 503. Starting an Access tunnel makes no API call.

## rename and move

A tunnel's name is also the name of its service and of its log file, so neither command is an
edit of `tunnels.toml`. Both stop the tunnel, rewrite the record, carry the log across and
reinstall the service under the new label.

```
$ gtunnel rename kubernetes k8s
Renamed kubernetes to k8s
```

`move` hands an Access tunnel to another account. The global `--account` names the source and
`--to` the destination, so neither has to be inferred from the default.

```
gtunnel move <name> --to <account> [--rename <new>]
```

Both refuse before they write anything. A rename of a tunnel that is not there, and a move to
the account the tunnel is already on:

```
$ gtunnel rename nope other
Error: Tunnel 'nope' not found for account 'dev'

$ gtunnel move kubernetes --to dev
Error: Tunnel 'kubernetes' is already on account 'dev'; use `gtunnel rename` to change its name
```

A move verifies the destination before the first write, so a target that does not exist over
there costs nothing:

```
$ gtunnel move k8s --to prod
Error: Instance 'bastion' not found in zone 'europe-west1-b' of project 'example-prod-3c4d' (account 'prod'); nothing was changed. If it lives in another zone, `gtunnel delete` the tunnel and `gtunnel add` it on 'prod'
```

On success `move` prints `Moved <name> from <source> to <destination>`, and a second line
naming the new name when `--rename` gave it one. `--rename` exists because names are scoped
per account: the destination may already hold a tunnel called the same thing, and that is the
one collision a move cannot resolve on its own.

Only Access tunnels move. A public tunnel's URL, certificate and load-balancer rules belong to
the account that created them, so moving one would mean tearing the route down and building it
again, which is what `delete` and `expose` already do and say.

## zones and domains

```
gtunnel zones                       # list zones in the project
gtunnel zones default <zone>        # set the account default
gtunnel domains                     # list registered domains
gtunnel domains default <domain>    # set the default for new Expose tunnels
gtunnel domains add <managed-zone>  # register a Cloud DNS managed zone
```

`domains add` reads the managed zone's DNS name, requests a managed certificate for it and
the wildcard, and writes the DNS authorization record. The certificate takes a few minutes
to reach ACTIVE; `relay status` shows the state.

## relay

```
gtunnel relay up [--zone Z] [--network N] [--managed-zone M]
gtunnel relay status
gtunnel relay down [--force] [-y]
```

`up` is idempotent and prints one line per resource, marking what it created and what was
already there. `down` refuses while tunnels still use the relay unless `--force` is given.

```
$ gtunnel relay status
No relay for account 'dev'. Run `gtunnel relay up`
```

`up` prompts for confirmation before creating cloud resources:

```
$ gtunnel relay up
Relay for account 'dev' in europe-west1-b:
  Global forwarding rule (~USD 18/month) and e2-micro VM (~USD 7/month, free tier in some US regions)
Create it? [y/N] [n]: y
```

Declining cancels cleanly:

```
$ gtunnel relay up
Relay for account 'dev' in europe-west1-b:
  Global forwarding rule (~USD 18/month) and e2-micro VM (~USD 7/month, free tier in some US regions)
Create it? [y/N] [n]: Cancelled
```

`down` confirms before deleting the relay:

```
$ gtunnel relay down
Delete the relay for account 'dev' and everything it owns? [y/N] [n]: Cancelled
```

### The relay's firewall is yours to close

`relay up` opens exactly two ingress rules, both on the network tag `gt-relay`; a tunnel
opens nothing.

| Rule | Allows | From |
|---|---|---|
| `gt-<account>-iap-ssh` | tcp `22` | IAP, `35.235.240.0/20` |
| `gt-<account>-lb-hc` | tcp `30000-30999`, the relay ports | the load balancer and its health checkers, `130.211.0.0/22` and `35.191.0.0/16` |

A public tunnel forwards with `ssh -R 0.0.0.0:<relay-port>:<local target>`, so the port
listens on every interface of the relay VM: the load balancer reaches it from another host.
Any other rule on that network that admits `30000-30999` from elsewhere, such as an
allow-all, lets a client who knows the relay's address connect to the forwarded service
directly. That skips the load balancer and, with `--private`, the IAP sign-in in front of it.
Check the network's rules before the first `gtunnel expose`.

## account

```
$ gtunnel account list
* dev              project=demo-project             zone=europe-west1-b
```

`account select <name>` sets the default. The star marks it.

```
gtunnel account add <name> --configuration <gcloud-configuration> [--project P] [--zone Z] [--credential-file PATH]
gtunnel account edit <name> [--project P] [--zone Z] [--credential-file PATH]
gtunnel account remove <name>
```

`add` reads the project and default zone from an existing gcloud configuration, so an account
is never a second place to type what gcloud already knows. Given `--project`, it reads
nothing: the account is built from the flags, and the configuration name need not exist. That
is the form for a CI runner ([Running in CI](usage.md#running-in-ci)).

```
$ gtunnel account add ci --configuration ci --project example-dev-1a2b --zone europe-west1-b --credential-file /secrets/ci-key.json
Account 'ci' saved to /workspace/gtunnel
$ cat /workspace/gtunnel/config.toml
selected_account = "ci"
log_level = "info"

[[accounts]]
name = "ci"
gcloud_configuration = "ci"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"
credential_file = "/secrets/ci-key.json"
```

`--credential-file` takes an absolute path to a service-account key or a workload identity
federation file; a relative path is refused on `add` and `edit`, and on `edit` an empty value
(`--credential-file ""`) clears it. A relative path written into `config.toml` by hand does not
stop the file loading: that account's tunnels exit 2 naming it, `doctor` reports it, and `edit`
repairs it. `--configuration` cannot be empty, because gcloud reads an empty one as the active
configuration and the tunnel would run as whoever is logged in to the shell; `--zone` is
checked the same way `add` checks a tunnel's zone. Both are refused with exit 2 before anything
is written. With a file set, a missing token is named after the file, never `gcloud auth login`:

```
$ gtunnel --account ci run bastion:22 --ttl 2m
Error: gtunnel could not get a token for 'ci'. The credential file /nonexistent/ci-key.json gave no token. Check that it exists, is a service-account key or a workload identity federation file, and that its identity is not disabled; gcloud said: ERROR: (gcloud.auth.print-access-token) Failed to load credential file: [/nonexistent/ci-key.json].  File /nonexistent/ci-key.json was not found.
```

An account name is 1-16 characters of `a-z`, `0-9` and `-`, starting with a letter:

| Name | Allowed | Why |
|---|---|---|
| `run`, `run-eu` | no | `run` and `run-*` are reserved: a run logs to `run-<account>-<name>.log`, which an account of that name could share with a declared tunnel |
| `runner`, `prod-run` | yes | only `run` itself and the `run-` prefix collide |

An account made before the rule keeps loading and working; `gtunnel doctor` names it and says
to rename it. `edit` changes only what it is given, and refuses with exit 2 a call with
nothing to change or an empty project or zone:

```
$ gtunnel account edit dev --zone europe-west4-a
Account 'dev' updated
```

An installed tunnel keeps the `--project` and `--credential-file-override` it was installed
with, so changing either does not reach it until it restarts. `edit` does not restart, because
a restart drops live connections; it names the installed tunnels that are now stale and says how:

```
$ gtunnel account edit ci --credential-file /secrets/new-key.json
Account 'ci' updated
Installed tunnels still use the old value: db. Restart to apply: `gtunnel restart db`
```

With no installed tunnel on the account, nothing extra is printed. A changed `--zone` needs no
restart: the zone is not in a service file.

`remove` refuses while the account still holds tunnels, and names both ways out rather than
offering a flag that would strand the services:

```
$ gtunnel account remove dev
Error: Account 'dev' still has 1 tunnel(s). Delete them with `gtunnel delete`, or move them to another account with `gtunnel move`, then remove the account
```

When the account holds no tunnels, it confirms before removing:

```
$ gtunnel account remove dev
Remove account 'dev'? [y/N] [n]: Removed account 'dev'
```

Declining leaves the account untouched:

```
$ gtunnel account remove dev
Remove account 'dev'? [y/N] [n]: Cancelled
```

## watch

A tunnel can be running and useless at the same time: the process is alive, so nothing
restarts it, and every new client is refused. `watch` is the thing that notices. It looks
at every tunnel, and for each one asks two questions: does the service manager call it
running, and does its local port accept a connection. A tunnel that answers its port is
healthy, whatever its log says. When one does not answer, the log decides what to call the
failure — a listener that said `Too many open files` since the last pass is out of
descriptors, anything else is simply refusing connections — and that word is what ends up
in the log line and in `gtunnel doctor`.

```
$ gtunnel watch --once --verbose
dev.mongodb: NotRunning -> Nothing
dev.kubernetes: Pass -> Nothing
prod.kubernetes: Pass -> Nothing
```

Three lines of a longer pass: there is one per tunnel it watches, whether or not anything
happened. `Pass` means the port accepted a connection, `NotRunning` that the service is not
up, and the word after the arrow is what the watchdog did about it.

A public tunnel gets a `skipped <account>.<name>: …` line instead of an arrow, because its
health lives in a load balancer and a TCP probe to a local port would answer a different
question. Both kinds of line appear only under `-v`.

| Form | What it does |
|---|---|
| `gtunnel watch` | The loop, in the foreground, one pass every 30s until Ctrl+C |
| `gtunnel watch --once` | One pass, then exit. What the agent runs |
| `gtunnel watch -v` | Say what each pass saw, rather than only what it did |
| `gtunnel watch install` | Install the agent, so the loop runs with no terminal open |
| `gtunnel watch uninstall` | Remove the agent |
| `gtunnel watch status` | Whether the agent is installed, where its log is, and when its last pass finished |

`watch` needs no Google credentials, and asks for none: it runs from an agent with no
terminal, where a login prompt would hang forever rather than fail.

### What it does about a tunnel that is failing

Nothing, the first time. One failed check is a slow moment; two in a row is a tunnel. From
there it restarts, waits longer each time, and eventually stops rather than restarting a
tunnel that will never come back.

| Failed check | What the watchdog does | Then waits |
|---|---|---|
| 1st | Counts it | |
| 2nd in a row | Trips: from here this is a problem, not a blip | 10s |
| The first one after that wait | Restart 1 | 20s |
| The first one after that wait | Restart 2 | 40s |
| The first one after that wait | Restart 3 | 80s |
| The first one after that wait | Restart 4 | |
| The one after restart 4 | Stops it, with a line in the log saying why | |

A failed check inside a wait does nothing at all, so at one pass every 30 seconds the
longer waits cost a pass or two before the next restart. At that interval the earliest a
tunnel can be stopped is six minutes after its first failed check.

Two passing checks in a row clear the breaker outright, so a restart that holds costs
nothing afterwards; so does stopping the tunnel yourself, because the breaker only has an
opinion about tunnels that are meant to be running.

Stopping it is the end of the ladder, and it is deliberate — four restarts that did not
hold mean the problem is not the tunnel. The log says what the watchdog saw and who was
holding the connections. Nothing starts it again by itself: the next pass sees a tunnel
that is not running, forgets everything it knew about it, and leaves it alone. `gtunnel
start <name>` is how it comes back, once the cause is dealt with — and if the cause is
still there, the ladder runs again from the beginning.

A tunnel whose supervisor stopped it on a spent [restart budget](configuration.md#the-restart-budget)
is skipped altogether: the pass leaves it stopped, because restarting it would undo that
decision and begin the loop with a fresh count.

### The agent

`watch install` writes one job for the machine, not one per tunnel: `com.gtunnel.watchdog`
on macOS, `gtunnel-watchdog.timer` on Linux. It wakes every 30 seconds, makes one pass,
and exits — it is a timer, not a service, because a job that exits immediately under
`KeepAlive` is one the service manager respawns in a tight loop.

```
$ gtunnel watch install
Watchdog installed: /usr/local/bin/gtunnel every 30s
Log: ~/.gtunnel/logs/watchdog.log
```

The job runs the binary that installed it, by absolute path. Move or replace that binary
and `gtunnel watch install` again. The log holds only what the watchdog did: a pass where
every tunnel passed writes nothing.

`gtunnel doctor` reports a machine that has tunnels but no watchdog, and `--fix` installs
it.

### Notifications

Every pass also asks of each tunnel of every account whether its state changed since anyone
last looked, and says so with one desktop notification. The states are stopped, starting,
up, degraded (running but failing its probe for two checks in a row, the breaker's own
count) and failed. Which changes are worth a banner:

| Change | Default | Switch |
|---|---|---|
| Up to degraded, or a start that comes up not answering | on | `degraded` |
| Degraded or failed back to up | on | `recovered` |
| Up to failed, a tunnel that died, a start that failed | on | `failed` |
| A start that works | off | `up` |
| Anything you did yourself: `start`, `stop`, `restart`, `delete`, `rename`, `move` | never | none |

The edges one pass finds become one notification, so a laptop waking from sleep is one
alert (`3 tunnels: 2 unhealthy, 1 recovered`), not one per tunnel.

```toml
[notifications]
up = false
degraded = true
recovered = true
failed = true
```

An absent section is the defaults above, and a section with some keys keeps the defaults
for the rest.

Every observer shares one ledger, `lifecycle.json` in the configuration directory, so a
change is announced by whoever stores it first and never twice. The watchdog writes
`run/watchdog.heartbeat` at the end of each pass. While that file is under 90 seconds old
the TUI stays quiet and leaves notifying to the watchdog; without a fresh heartbeat the
TUI announces the changes it sees itself, for the account on screen only. The first time
the TUI opens on a machine with no watchdog installed it says
`notifications need the watchdog when the TUI is closed: gtunnel watch install` in the
status line, once, and installs nothing. `gtunnel watch status` shows when the last pass
finished.

A tunnel gtunnel has never seen before is recorded without a notification, so the first pass
after installing the watchdog does not announce every tunnel's current state as news.

## doctor

Reports what gtunnel can see without changing anything.

```
$ gtunnel doctor
account dev:
  - gcloud has no usable credentials for account 'dev'. Run `gcloud auth login --configuration=dev` and try again
```

```
$ gtunnel doctor          # with a gcloud whose Python has no numpy
gtunnel:
  - gcloud's Python is missing numpy, which makes IAP tunnels slow. Run `gcloud components reinstall`
account dev:
  - gcloud has no usable credentials for account 'dev'. Run `gcloud auth login --configuration=dev` and try again
```

A service with no record is machine-wide, so it is filed under `gtunnel:` like the watchdog.
It is a `com.gtunnel.*` plist (macOS) or `gtunnel-*.service` unit (Linux) whose label no
record in any account produces and that belongs to this config directory: it sits in
`<config dir>/agents`, or its log path is under `<config dir>/logs/`. A service another
config directory installed is never reported, and neither is the watchdog:

```
$ gtunnel doctor
gtunnel:
  - Service com.gtunnel.dev.old-tunnel is installed but no tunnel in tunnels.toml owns it, so it may still hold a local port at login
```

A key in `config.toml` or `tunnels.toml` that gtunnel does not read is machine-wide too, one
finding per key with its path. Loading ignores such a key, so nothing else shows it; the
usual cause is `log_level` typed below an `[[accounts]]` table, where TOML files it under that
table. Nothing is removed for you: edit the file, or fix it in the config page, which refuses
to save one. This is the output for that file, with `gcloud_configuration = "scratch-docs"`
naming a gcloud configuration that does not exist:

```
$ gtunnel doctor
gtunnel:
  ! config.toml has a key gtunnel ignores: accounts[0].log_level. Edit the file and remove or move it
account scratch-docs:
  ! gcloud has no usable credentials for account 'scratch-docs'. Run `gcloud auth login --configuration=scratch-docs` and try again
```

An account whose `credential_file` is relative or starts with `~` is reported per account,
and `--fix` does not touch it:

```
$ gtunnel doctor
account ci:
  ! Account 'ci': credential_file ~/ci-key.json is not an absolute path, so a service cannot resolve it. Run `gtunnel account edit ci --credential-file <absolute path>`, or `--credential-file ""` to clear it
```

| Flag | Effect |
|---|---|
| `--fix` | Fix what can be fixed safely |
| `-y`, `--yes` | Skip the confirmation that `--fix` asks for |
| `-a`, `--all` | Check every account, not only the selected one |
| `--json` | Print the findings as a JSON array of `account`, `description` and `fixable` |

`--fix` removes stale local records, services of this config directory that no record owns
(a plist or unit left behind by a removed tunnel, which would otherwise keep its port at
login), dead ephemeral records and per-tunnel relay leftovers no live tunnel claims,
reconciles auto-start to what `tunnels.toml` says, and installs a missing watchdog;
[troubleshooting.md](troubleshooting.md#leftovers) lists each finding. It
re-checks the claim immediately before each removal, so a tunnel started during the scan is
left alone. Findings it cannot fix, such as a missing firewall rule, are printed with the
exact command that fixes them.

A reserved account name (`run` or `run-*`, see `account add`) is reported the same way and is
never fixed for you: renaming an account means adding one and moving its tunnels.

The report comes first, then the question; a declined one changes nothing. The samples
below are written from the format strings in `src/doctor.rs`, since a scan reaches GCP:

```
$ gtunnel doctor --fix
account dev:
  - Tunnel 'old-tunnel' is in tunnels.toml but has no service installed

Remove 1 leftover(s)? [y/N] [n]: y
  fixed: Tunnel 'old-tunnel' is in tunnels.toml but has no service installed
```

When the findings hold nothing gtunnel may remove:

```
$ gtunnel doctor --fix
account dev:
  ! Tunnel 'db' wants local port 15432, which is already in use

Nothing here is gtunnel's to remove
```

`-` marks a finding `--fix` handles, `!` one it only explains.

## reset

Removes every local trace: services, config, state, logs. It never deletes anything in GCP.

```
$ gtunnel reset
This removes 1 local tunnel(s), 1 account(s) and /Users/you/.gtunnel
Remove all local gtunnel configuration? [y/N] [n]: Cancelled
```

```
$ gtunnel reset -y
This removes 1 local tunnel(s), 1 account(s) and /Users/you/.gtunnel
Removed /Users/you/.gtunnel
Cloud resources were not deleted. Run gtunnel relay down first if you want them gone.
```

```
$ gtunnel reset -y
Nothing to remove: /Users/you/.gtunnel does not exist
```

## update

```
$ gtunnel update --check
Checking for updates...
Update available: v0.1.34 -> v0.1.35

Run `gtunnel update` to install.
```

A format sample: the versions are those of the day it was captured and differ on every run. On
the latest release the second line is `gtunnel v<version> is already the latest version.` instead.

Every other command prints a one-line hint when a cached check found a newer version: see
[Update hint](usage.md#update-hint).

The check runs at most once a day, in the background, and never blocks a command. A
downloaded binary and one in `~/.cargo/bin` replace themselves after verifying the published
SHA-256; a Homebrew install is detected and pointed at `brew upgrade gtunnel` instead.

## Errors and exit codes

A command that did what it said exits 0. A failure has a kind, and the kind decides the exit
code. A script branches on the code, or on `error.kind` when a command runs with `--json`.
The failure itself is one line on stderr beginning `Error:`; a start that did not come up
keeps the `✗` lines on stdout with no `Error:` line after them. The text is written for a
person and is not a stable interface: branch on the code.

| Kind | Exit | Meaning |
|---|---|---|
| `internal` | 1 | Anything unclassified: an unreadable config, a failed write, a tunnel that did not come up |
| `usage` | 2 | A request gtunnel refuses: a bad argument or setting (a target without a port, a bad `GTUNNEL_LOG_LEVEL`), a name already taken (`add`, `expose`, `rename`, `move`, `account add`), an empty `--configuration`, an invalid `--zone`, an `account edit` with nothing to change or an empty project or zone, a local port a run holds, a move that cannot happen, `expose` or `run --public` on an account with no relay, or a local target that is not a port or host:port. Same code clap uses for a malformed command line |
| `not_found` | 3 | The tunnel or the account named does not exist |
| `auth` | 4 | gcloud has no usable credentials for the account |
| `gcloud` | 5 | A gcloud call ran and failed |

`list --json`, `account list --json` and `doctor --json` print the failure on stdout as one
object, and nothing on stderr. `message` is the whole cause chain joined with `: `.

```
$ gtunnel list --json --account nope
{"error":{"kind":"not_found","message":"Account 'nope' not found"}}
$ echo $?
3
```

Without `--json` the text stays on stderr as before and only the exit code follows the kind.
A command-line mistake that clap rejects itself (an unknown flag) prints clap's usage text and
exits 2 even under `--json`. MCP tool errors carry the same object as `structuredContent`
beside the text, so an agent matches the same kind strings.

## Shell completions

`gtunnel completions <bash|zsh|fish>` prints a script that completes commands, flags and
tunnel names. Press TAB after `gtunnel start ` and the selected account's tunnels are offered;
`add` and `expose`, which take a name that does not exist yet, offer nothing.

| Shell | Install |
|---|---|
| zsh | `mkdir -p ~/.zsh/completions && gtunnel completions zsh > ~/.zsh/completions/_gtunnel`, then put `fpath=(~/.zsh/completions $fpath)` before `compinit` in `~/.zshrc` |
| bash | `mkdir -p ~/.local/share/bash-completion/completions && gtunnel completions bash > ~/.local/share/bash-completion/completions/gtunnel` (needs the `bash-completion` package) |
| fish | `gtunnel completions fish > ~/.config/fish/completions/gtunnel.fish` |

Open a new shell afterwards. The script is a thin caller: on each TAB it runs `gtunnel`
again to ask what fits, so the binary on `PATH` must be the one that printed it. Regenerate the
file after `gtunnel update` or `brew upgrade`.

```text
$ gtunnel completions zsh | head -9
#compdef gtunnel
function _clap_dynamic_completer_gtunnel() {
    local _CLAP_COMPLETE_INDEX=$(expr $CURRENT - 1)
    local _CLAP_IFS=$'\n'

    local completions=("${(@f)$( \
        _CLAP_IFS="$_CLAP_IFS" \
        _CLAP_COMPLETE_INDEX="$_CLAP_COMPLETE_INDEX" \
        COMPLETE="zsh" \
```

| Property | Behaviour |
|---|---|
| Needs a login or a config | No. `completions` prints text and works before `gtunnel init` |
| Account | The selected account. `--account` on the same line is not applied to the names offered |
| No config, or a broken one | No names are offered and nothing is printed to the terminal |
