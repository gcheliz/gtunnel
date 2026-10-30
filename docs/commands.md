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
- [mcp](#mcp)
- [Errors and exit codes](#errors-and-exit-codes)
- [Shell completions](#shell-completions)

Every command with the output it really prints, success and failure alike. A block fenced `console` is run against the binary by `cargo test`, so it cannot drift (the rest need a live tunnel, a prompt or a real project): `[HOME]` stands for the home directory the run used, and a line `? N` after a command is its exit code when that is not 0. The two modes, the target kinds and the
lifetime axis are defined once in [Concepts](usage.md#concepts); the files and environment
variables are in [configuration.md](configuration.md).

## init

Reads a gcloud configuration and writes a gtunnel account. It prompts for everything it
cannot infer. A bare `gtunnel` runs it too when there is no `config.toml` and stdin and
stdout are a terminal, then opens the TUI; without a terminal it prints the `not initialized`
error and exits 1.

```console
$ gtunnel init
An account is a name gtunnel gives to one GCP project. It is built from a gcloud configuration (the profile `gcloud init` made: login, project, zone). Enter takes the value shown.

gcloud configurations:
  1. sandbox (active)  project=example-dev-1a2b  account=you@example.com

  The gcloud configuration whose login, project and zone this account uses
  To list: gcloud config configurations list
Configuration number (Enter = 1): 
  What you pass to `--account`. Lower case, digits and dashes, e.g. `dev`
Account name (Enter = sandbox): Could not list projects: not found (404)

  The GCP project every tunnel and API call of this account targets
  To list: gcloud projects list
Project (Enter = example-dev-1a2b): 
  Every gcloud call of this account acts as this service account; you need roles/iam.serviceAccountTokenCreator on it. Most people press Enter and act as themselves
Service account to impersonate (optional, Enter to skip): 
  Fallback when an instance's zone cannot be looked up, and the relay's zone
  To list: gcloud compute zones list
Default zone (optional, Enter to skip): Account 'sandbox' saved to [HOME]/.gtunnel
No default zone saved; commands look an instance's zone up by name

```

Standard input was empty, so every prompt took its default and nothing echoed. The gcloud configuration has no zone, so the zone was asked and left empty.

`init` also writes every tunable key of `config.toml` at its default, `[restart]`, `[watch]`,
`[logs]` and `[gcp]` included, adding only the keys that are missing:
[the block it writes](configuration.md#settings-the-environment-can-override).

Every prompt is preceded by a line saying what the value sets, and, for a value you pick from
something that exists, the command that lists it. The text is the one the TUI's account form
shows, from one source (`src/fields.rs`). In the brackets after the label, `Enter = <value>`
is the default and `optional, Enter to skip` marks an answer that may be empty. The helper
lines are plain text on stdout, so a piped answer is read unchanged. The impersonation
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

A zone the gcloud configuration holds is saved without a question. Only a configuration
with none gets the zone step, which works like the project step: the project's zones are
printed as `N. <zone>` and `Default zone, by number or name` takes a row number or a zone
name. A zone the project does not have is refused. When the zones cannot be listed, as in
the transcript, the prompt is `Default zone` with the same `optional, Enter to skip` hint.

The zone is optional, because `add` and `run` look an instance's zone up by its name. The
default is the fallback when that lookup cannot run (no permission, no network) and the
relay's zone: `relay up` stops with exit code 2 and `no zone for account '<name>'` without
`--zone` or a default. Set one later with `gtunnel account edit <name> --zone <zone>`.

`init` checks the gcloud login before it lists projects and zones, offering `gcloud auth
login` on a terminal, as [Startup](usage.md#startup) describes.

The list is fetched with the configuration's own credentials, and only projects in the
`ACTIVE` state are offered, up to 5,000. The cause is one line: a `403` means the account
lacks `resourcemanager.projects.get` or the Cloud Resource Manager API is not enabled, see
[troubleshooting.md](troubleshooting.md#could-not-list-projects).

An account stores a gcloud configuration name, a project, a default zone, an optional
service account to impersonate, and the relay once one exists; never a credential
([decision 1](decisions.md#1-gtunnel-stores-no-credential)).

## add

```text
gtunnel add <NAME> <INSTANCE:PORT> [-z ZONE] [--local-port N] [-s]
gtunnel add <NAME> <HOST:PORT> --via INSTANCE [-z ZONE] [--local-port N] [-s]
gtunnel add <NAME> <HOST:PORT> --dest-group G --region R --network N [-s]
```

| Flag | Effect |
|---|---|
| `-z`, `--zone` | Zone of the instance. Looked up from the instance name when omitted, see below |
| `--local-port` | Fixed local port; otherwise one is allocated from 15000-15999, stable per name, skipping any a saved tunnel or a live run holds |
| `--via` | Reach a host the instance can reach, rather than a port of the instance |
| `--dest-group` | Target an IAP destination group instead of an instance |
| `-s`, `--start` | Install the service and start it immediately |

`--help` says what each value is and shows a placeholder to copy:

```console
$ gtunnel add --help
Add a persistent Access tunnel (IAP TCP forward to localhost)

Usage: gtunnel add [OPTIONS] <NAME> <TARGET>

Arguments:
  <NAME>    Tunnel name: what it connects to, e.g. postgres
  <TARGET>  INSTANCE:PORT (GCP instance name, port 22 open), or HOST:PORT with --dest-group or --via, e.g. bastion:5432

Options:
      --account <ACCOUNT>        Use a specific account (overrides the default)
      --via <VIA>                GCP instance name (port 22 open) to forward from, for a host only it can reach, e.g. bastion
      --log-level <LEVEL>        How much to log: quiet, info or debug (default: info)
  -z, --zone <ZONE>              Zone of the instance, e.g. europe-west1-b (default: looked up from the instance)
      --local-port <LOCAL_PORT>  Port on localhost you connect to, e.g. 15432 (allocated automatically when omitted)
      --dest-group <DEST_GROUP>  IAP destination group, for targets that are not a compute instance, e.g. my-group
      --region <REGION>          Region of the destination group, e.g. europe-west1
      --network <NETWORK>        VPC network of the destination group, e.g. default
  -s, --start                    Start the tunnel immediately after adding
  -h, --help                     Print help

```

Without `--zone`, `add` asks the project which zone holds the instance (one filtered
`aggregated/instances` call), so an instance in a zone other than the account's default needs
no flag, and the instance is checked in the same call.

| The lookup finds | `add` does |
|---|---|
| One instance | Saves its zone |
| None | Exit 3, `Instance '<name>' not found in project '<project>'` |
| Several, in different zones | Exit 2, naming the zones and asking for `--zone` |
| Cannot run (permission, network) | Without a terminal: the account's default zone with one warning, or with none an error asking for `--zone`: exit 4 when GCP refuses the credentials (401, 403), exit 6 when GCP cannot be reached or answers 429, exit 2 otherwise |

On a terminal, the last three, and a lookup that cannot run, ask for the zone instead of failing: the prompt lists the
zones when there are several, starts from the account default (or blank), and accepts a
number or a name. A script and the MCP server never prompt and keep the exit codes above;
the TUI reopens its add dialog at an extra `Zone (Enter keeps it)` step, filled with the
account default or blank. `run` resolves the zone the same way. Because the lookup reaches GCP, `add`
without `--zone` checks the login first, as [Startup](usage.md#startup) lists; `add --zone`
without `-s` does not.

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
name in every account. The account is what `;` or `--account` selects; repeating it in the
name only makes a command that works in one account.

| | |
|---|---|
| Shape | `<platform>`, or `<platform>-<service>` when the platform has more than one |
| Leave out | the account, environment or project — no `-dev`, `-prod`, `-staging` |
| Allowed | up to 40 characters of `a-z`, `0-9` and `-`, starting with a letter and ending with a letter or digit |
| Refused | a name whose `<account>-<name>` another account already holds, e.g. `eu-api` on `prod` when `prod-eu` has `api`; both would write `prod-eu-api.log` |

```text
gtunnel add k8s   … --account dev     # same name
gtunnel add k8s   … --account prod    # different account, different port
```

A name that carries its account still works (the TUI hides the suffix), but every command
spells it out.

A tunnel forwards to one host. When the client picks its own destination, as MongoDB Compass
does over SSH, give the tunnel the bastion and let the client choose. One tunnel per database
pins a hostname, and for a replica set a shard that may not be the primary, which defeats
failover.

### Reaching a host behind an instance

The three target kinds, their engines and what each needs are in
[Concepts](usage.md#three-kinds-of-access-target). Use `instance` for SSH or a database on the
instance itself, `--dest-group` for a host IAP can address directly, and `--via` for the rest.

An IAP TCP forward lands on a port of the instance itself, so a private endpoint behind a
bastion needs `--via`: IAP carries ssh, and ssh forwards from the far side. A private GKE
master:

```console
$ gtunnel add k8s 10.0.0.10:443 --via bastion -z europe-west1-b --local-port 8443
○ k8s  localhost:8443 -> 10.0.0.10:443 via bastion

```

The tunnel is installed and stopped; `-s` starts it in the same command.

The kubeconfig names the master in `tls-server-name`, because its certificate is for its own
IP, not `127.0.0.1`:

```yaml
    server: https://127.0.0.1:8443
    tls-server-name: 10.0.0.10
```

## expose

```text
gtunnel expose <NAME> <TARGET> [-d DOMAIN] [--private] [-s]
```

`TARGET` is `3000` or `localhost:3000`. The URL is `https://<NAME>.<DOMAIN>`. `--private`
puts IAP in front of the load balancer, so a visitor must sign in with Google. That
sign-in only holds if the relay's own port range is closed to everything but the load
balancer: see [The relay's firewall is yours to close](#the-relays-firewall-is-yours-to-close).

The name and the relay port are claimed in `tunnels.toml` before the route is built, so two
exposes at once cannot pick the same port. A failed route gives the claim back; one left by
a killed command is cleared by `gtunnel doctor --fix` after ten minutes.

Without a relay the command stops before changing anything:

```console
$ gtunnel expose web 8000
? 2
Error: No relay for account 'dev'. Run `gtunnel relay up`

```

## run

An ephemeral tunnel in the foreground. It is recorded while it runs so `list`, the TUI and
`doctor` can see it, and every route it created is torn down on exit, including on Ctrl+C
and on a crash.

```text
gtunnel run bastion:5432                 # access, prints the local port
gtunnel run db bastion:5432              # access, under a name
gtunnel run --public 3000              # https://gt-<random>.<domain>
gtunnel run bastion:5432 --ttl 30m       # stops itself after 30 minutes
```

| | |
|---|---|
| Name | Refused when a live run or a managed tunnel in the account has it: `Error: A run named 'db' is already live for account 'dev'` |
| `--local-port` | The checks `add` makes, plus every live run of every account: refused when a tunnel or run already owns the port, when something on the machine listens on it, and for `0` |
| `--ttl` | Counted on the wall clock, so time asleep counts: a laptop that wakes past the stop time ends the run within a second |
| Stopping | Ctrl+C, SIGTERM, SIGHUP, the TTL and gcloud exiting all end the same way: SIGTERM to gcloud and its children, up to 2 seconds, then SIGKILL |
| Exit code | `0` when Ctrl+C, SIGTERM, SIGHUP or the TTL stopped it. When gcloud ends the run by itself, gcloud's own code (`1` when it has none, or exits 0), so `gtunnel run ... && ./migrate` does not go on after a tunnel that failed to open |
| A second Ctrl+C | While the route is being removed, a second Ctrl+C (or SIGTERM, SIGHUP) abandons that at once and exits `130`. The route and the record stay: it prints that the route was left, and `gtunnel doctor --fix` or the next watchdog pass removes it |
| Output | A run keeps its tunnel when its output goes away (`gtunnel run ... \| head -1`, a closed terminal): it stops printing and runs on until a signal or the TTL |
| Its process dies | A `kill -9` leaves gcloud forwarding, so the record names the run's own process as well as gcloud's. The next `gtunnel list` or watchdog pass stops a run whose process is gone or whose TTL has passed, removes its route and its record, and says so on stderr |

<p align="center">
  <img src="demo-run.gif" alt="gtunnel run --ttl 30m db bastion:5432 prints the local port and its stop time, gtunnel list shows the run with its stop time, Ctrl+C closes it, and the list is empty again" width="720">
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

```console,ignore
$ gtunnel list
STATUS     NAME             MODE    URL                              TARGET
○ stopped  db               access  localhost:15432                  postgres-1:5432 (europe-west1-b)
● running  scratch          access  localhost:15999                  bastion:5432 (europe-west1-b)  [run, stops at 14:32]
● running  pid 59670        access  -                                bastion-1:22 (europe-west1-b)  [not managed by gtunnel]
```

A live `gtunnel run` is listed with the mode it really is, `access` or `public`, and a marker
after the target: `[run, stops at 14:32]` (local time) when it has a TTL, `[run]` when it does
not. A run whose process is gone is not listed; `doctor` reports its leftover record.

A saved tunnel of a kind a newer gtunnel added is named under the table as `  <name>  unsupported by this version (<kind>)`, and a run record this version cannot parse as `  run record <file> cannot be read by this version; its ports stay reserved`. Neither is a row: nothing in them can be trusted to fill a column. See [A file written by a newer gtunnel](troubleshooting.md#a-file-written-by-a-newer-gtunnel).

A stopped tunnel whose supervisor gave up on expired credentials carries `[auth expired]` in
the same place, and `gtunnel_get_status` reports its `health` as `auth_expired`. One it stopped
for a denied impersonation, an invalid key or a missing gcloud configuration carries
`[credentials refused]` and `credentials_refused`: the log names which, and
[troubleshooting](troubleshooting.md#what-a-token-or-api-failure-means) says what to do. Both
clear on the next `gtunnel start`.

A stopped tunnel whose supervisor spent its [restart budget](configuration.md#the-restart-budget)
carries `[restart budget spent: exit status: 1]`, the last thing gcloud died of, and the TUI
shows it as a crash loop. `gtunnel start` clears the marker and the count; `gtunnel watch`
never restarts it.

`stop` makes no GCP call, so it works with an expired login and never asks for one; `start` and
`restart` do reach GCP and check the login first.

The TUI tags the same row `[run, stops at 14:32]`, or `[ephemeral]` without a TTL, and its
details pane counts down: `stops at  14:32 (in 25m)`.

Add `--json` for machine-readable output:

```console,ignore
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
    "health": "stopped"
  },
  {
    "status": "running",
    "name": "scratch",
    "mode": "access",
    "url": "localhost:15999",
    "target": "bastion:5432 (europe-west1-b)",
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
| `health` | string | `healthy`, `unhealthy`, `auth_expired`, `credentials_refused`, `stopped` or `unknown` |

Running rows with a local port are probed all at once, so `list --json` waits at most about
750 ms: one probe (500 ms, or `list_probe_timeout`) plus 250 ms to read the log on a silent
close. A tunnel not running is `stopped` (or `auth_expired`, or `credentials_refused`); a running public or unmanaged one
cannot be probed and stays `unknown`. `gtunnel_list_tunnels` returns the same rows
less the unmanaged ones, so its `unmanaged` is always `false`.

```console
$ gtunnel logs nope
? 3
Error: Tunnel 'nope' not found for account 'dev'

```

`logs` and `stop` take the name of a live run when no saved tunnel has it, and a saved tunnel
wins when both exist. A run launched by an MCP client has a log, `logs/run-<account>-<name>.log`;
a run started in a terminal prints there instead and has none, so `logs` says so:
`Run 'name' has no log: it was started from a terminal, which has its output`.
Each launch starts the log fresh, and the log stays after the run ends, so `logs` reads a finished
run until its name is launched again.

```console
$ gtunnel autostart db           # ask
db auto-start is off

$ gtunnel autostart db --on      # set
db auto-start is on

```

`autostart` with no flag reports only. `--on` and `--off` write the record and bring the
service manager to it, as `A` does in the TUI. When the two disagree (a plist left at login by
an older gtunnel), `gtunnel doctor` reports the drift and `doctor --fix` follows the record.

A start waits for the tunnel to listen.

| Case | Rule |
|---|---|
| Access, `--via` included | up to 20 seconds (`start_deadline`, or `GTUNNEL_START_DEADLINE`); passes as soon as the log reports the listener |
| Public | no listener to wait for: judged after a fixed 3 seconds. One still connecting then reads as running, so `gtunnel list` is the check for a later failure |
| Failed start (service failed, or not listening by the deadline) | the lines below on stdout, exit 1 (4 when the hint is expired credentials), the record stays. A hint line follows when the log holds a line gtunnel recognises (the newest, from [the log table](troubleshooting.md#what-the-log-says-and-what-it-means)), or `not listening after 20s`. The TUI status line shows the same text; an MCP client gets an `isError` result |

```console,ignore
$ gtunnel start db
✗ db failed to start
  Logs: gtunnel logs db
$ echo $?
1
```

`add -s` and `restart` start the tunnel the same way, so a tunnel `add -s` could not start is
still added and the command exits 1.

`logs -f` follows, printing each line once even while the tunnel writes. `restart` rewrites the service file before starting, so it also picks up
an edited target. Starting a tunnel empties its log first, so what the pane shows belongs to
the run in front of you; `L` in the TUI clears the selected tunnel's log without restarting it. `delete` removes the relay route first, then the service, then the record and the log.

A tunnel's log opens with the tunnel's own name and target, and gtunnel adds a line of its
own for every start, stop and clear. A healthy jump tunnel prints nothing else, so those lines
tell a fresh start from a failure or a tunnel that never existed.

#### Connections in the log

At log level `info` or `debug` the supervisor writes one line when a client connects and one
when it leaves, in the same shape for an IAP tunnel and a jump tunnel, whether or not the TUI
is open. The source port is the client's local port, so it names the process:
`lsof -nP -iTCP:54321` finds it while it is open, and the timestamp beside it afterwards.

```text
2026-10-27T10:15:02Z gtunnel: conn iap-7 open
2026-10-27T10:15:14Z gtunnel: conn iap-7 from 127.0.0.1:54321 closed after 12.0s
```

| Rule | Why |
|---|---|
| The open is written after two seconds, or with the close if that comes first | gtunnel's own health probe connects and closes in milliseconds; its source port is only known at the close |
| A probe by gtunnel writes no line | `tcp_probe` records its source port in `run/probes/<port>` before it closes, and the supervisor drops the matching close. A probe by anything else, `nc -z` included, is a connection |
| At most `logs.lines_per_minute` (60) connection lines a minute | a client that reconnects in a loop would bury the lines that explain a failure; the rest become `gtunnel: conn ... N more in the last minute` |
| A jump tunnel counts only ssh channels, an instance tunnel only IAP connections | a jump tunnel's ssh rides on gcloud's own IAP connection to the bastion, which is the tunnel and never closes |
| `quiet` writes none | and ssh and gcloud print none at that level either |
| Only the supervisor writes them | the TUI never appends to a tunnel's log, so two open TUIs, or none, give the same log |
| They come from the tools' own lines | the supervisor reads gcloud's `[N] Connecting with URL` and `[N] Client closed connection from [...]`, and ssh's `channel N: new` and `channel N: free: ... connect from H port P`, and pairs them by id |
| A tunnel gtunnel does not manage gets none | it has no log here |

Starting a public tunnel re-creates a missing backend, host rule, named port or DNS record, so
`gtunnel restart <name>` fixes one serving a 503. Starting an Access tunnel makes no API call.

## rename and move

A tunnel's name is also its service's and its log's, so both commands stop the tunnel, rewrite
the record, carry the log across and reinstall the service under the new label.

```console
$ gtunnel rename k8s kubernetes
Renamed k8s to kubernetes

```

`move` hands an Access tunnel to another account: `--account` names the source, `--to` the
destination.

```text
gtunnel move <name> --to <account> [--rename <new>]
```

Both refuse before they write anything:

```console
$ gtunnel rename nope other
? 3
Error: Tunnel 'nope' not found for account 'dev'

$ gtunnel move kubernetes --to dev
? 2
Error: Tunnel 'kubernetes' is already on account 'dev'; use `gtunnel rename` to change its name

```

A move verifies the target in the destination before the first write:

```console
$ gtunnel move kubernetes --to prod
? 3
Error: Instance 'bastion' not found in zone 'europe-west1-b' of project 'example-prod-3c4d' (account 'prod'); nothing was changed. If it lives in another zone, `gtunnel delete` the tunnel and `gtunnel add` it on 'prod'

```

On success `move` prints `Moved <name> from <source> to <destination>`, plus a line with the
new name when `--rename` gave one; `--rename` is for a destination that already uses the name.
Only Access tunnels move: a public tunnel's URL, certificate and load-balancer rules belong to
its account, so use `delete` and `expose`.

## zones and domains

```text
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

```text
gtunnel relay up [--zone Z] [--network N] [--managed-zone M]
gtunnel relay status
gtunnel relay down [--force] [-y]
```

`up` is idempotent and brings an existing relay up to date in place. It prints one line per
resource: `✓` created, `~` updated, `·` already as wanted. `down` refuses while tunnels use the
relay, unless `--force`.

| Resource | What `up` compares, and does on a difference |
|---|---|
| Health check | Interval, timeout, thresholds and port specification: one `PATCH` |
| Backend services | `timeoutSec`, `connectionDraining` and `iap`: one `PATCH` with the fields that differ. `expose`, `start` and `restart` do the same for a tunnel's backend |
| Relay instance | The `startup-script` metadata: `setMetadata` with the new script, then the script is re-run over IAP ssh (`sudo google_metadata_script_runner startup`), so sshd changes without recreating the VM. A stopped instance gets the metadata and runs it at its next boot |

A relay that already matches receives no write. If re-running the script fails, `up` stops with
the `gcloud compute ssh` command to run by hand: the metadata is already written, so a second
`up` finds nothing to change. `doctor` reports a relay whose startup script or health check is
out of date.

```console,ignore
$ gtunnel relay up
  · relay instance gt-dev-relay (exists)
  ~ health check gt-dev-hc (updated)
  ~ default backend gt-dev-default (updated)
```

```console,ignore
$ gtunnel relay status
No relay for account 'dev'. Run `gtunnel relay up`
```

`up` prompts for confirmation before creating cloud resources:

```console,ignore
$ gtunnel relay up
Relay for account 'dev' in europe-west1-b:
  Global forwarding rule (~USD 18/month) and e2-micro instance (~USD 7/month, free tier in some US regions)
Create it? [y/N] [n]: y
```

Declining prints `Cancelled` and creates nothing. `down` confirms before deleting the relay:

```console,ignore
$ gtunnel relay down
Delete the relay for account 'dev' and everything it owns? [y/N] [n]: Cancelled
```

### The relay's firewall is yours to close

`relay up` opens exactly two ingress rules, both on the network tag `gt-relay`; a tunnel
opens nothing.

The rules and their sources are in [What GCP needs](gcp.md#firewall-rules-relay-up-creates).

A public tunnel forwards with `ssh -R 0.0.0.0:<relay-port>:<local target>`, so the port listens
on every interface of the relay. Any other rule on that network admitting `30000-30999` from
elsewhere, such as an allow-all, lets a client who knows the relay's address reach the service
directly. **That skips the load balancer and, with `--private`, the IAP sign-in in front of
it.** Check the network's rules before the first `gtunnel expose`.

## account

```console
$ gtunnel account list
* dev              project=example-dev-1a2b         zone=europe-west1-b
  prod             project=example-prod-3c4d        zone=europe-west1-b
  sandbox          project=example-dev-1a2b         zone=

```

`account select <name>` sets the default. The star marks it.

```text
gtunnel account add <name> --configuration <gcloud-configuration> [--project P] [--zone Z] [--credential-file PATH]
gtunnel account edit <name> [--project P] [--zone Z] [--credential-file PATH]
gtunnel account remove <name>
```

`add` reads the project and default zone from an existing gcloud configuration. Given
`--project`, it reads nothing and the configuration name need not exist: the form for a CI
runner ([Running in CI](usage.md#running-in-ci)).

```console
$ gtunnel account add ci --configuration ci --project example-dev-1a2b --zone europe-west1-b --credential-file /secrets/ci-key.json
Account 'ci' saved to [HOME]/.gtunnel

```

The account is then in `config.toml`:

```toml
[[accounts]]
name = "ci"
gcloud_configuration = "ci"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"
credential_file = "/secrets/ci-key.json"
```

| Flag | Rule |
|---|---|
| `--credential-file` | an absolute path to a service-account key or a workload identity federation file; relative is refused on `add` and `edit`, and `--credential-file ""` on `edit` clears it. A relative path written by hand still loads: that account's tunnels exit 2 naming it, `doctor` reports it, `edit` repairs it |
| `--configuration` | cannot be empty: gcloud reads an empty one as the active configuration, so the tunnel would run as whoever is logged in to the shell. Exit 2 before any write |
| `--zone` | checked as `add` checks a tunnel's zone. Exit 2 before any write |

With a file set, a missing token names the file, never `gcloud auth login`:

```console,ignore
$ gtunnel --account ci run bastion:22 --ttl 2m
Error: gtunnel could not get a token for 'ci'. The credential file /nonexistent/ci-key.json gave no token. Check that it exists, is a service-account key or a workload identity federation file, and that its identity is not disabled; gcloud said: ERROR: (gcloud.auth.print-access-token) Failed to load credential file: [/nonexistent/ci-key.json].  File /nonexistent/ci-key.json was not found.
```

An account name is 1-16 characters of `a-z`, `0-9` and `-`, starting with a letter:

| Name | Allowed | Why |
|---|---|---|
| `run`, `run-eu` | no | `run` and `run-*` are reserved: a run logs to `run-<account>-<name>.log`, which an account of that name could share with a declared tunnel |
| `runner`, `prod-run` | yes | only `run` itself and the `run-` prefix collide |

An older account that breaks the rule keeps working; `gtunnel doctor` says to rename it.
`edit` changes only what it is given, and exits 2 on nothing to change or an empty project or
zone:

```console
$ gtunnel account edit dev --zone europe-west4-a
Account 'dev' updated

```

An installed tunnel keeps its `--project` and `--credential-file-override` until it restarts.
`edit` does not restart, because that drops live connections; it names the stale tunnels:

```console,ignore
$ gtunnel account edit ci --credential-file /secrets/new-key.json
Account 'ci' updated
Installed tunnels still use the old value: db. Restart to apply: `gtunnel restart db`
```

A changed `--zone` needs no restart: the zone is not in a service file. `remove` refuses while
the account holds tunnels, or an `expose` still in flight, and names both ways out:

```console
$ gtunnel account remove dev
? 1
Error: Account 'dev' still has 2 tunnel(s). Delete them with `gtunnel delete`, or move them to another account with `gtunnel move`, then remove the account

```

An account with no tunnels is removed after a confirmation (`Removed account 'dev'`);
declining leaves it untouched:

```console
$ gtunnel account remove sandbox
Remove account 'sandbox'? [y/N] [n]: Cancelled

```

## watch

A tunnel can be running and useless: the process is alive, so nothing restarts it, and every
new client is refused. `watch` notices. For each tunnel it asks whether the service manager
calls it running and whether its local port accepts a connection. A tunnel that answers its
port is healthy, whatever its log says. When one does not answer, the log names the failure:
`Too many open files` since the last pass means out of descriptors, anything else is refusing
connections. That word goes into the log line and `gtunnel doctor`.

```console,ignore
$ gtunnel watch --once --verbose
dev.mongodb: NotRunning -> Nothing
dev.kubernetes: Pass -> Nothing
prod.kubernetes: Pass -> Nothing
```

Three lines of a longer pass, one per tunnel. `Pass` means the port accepted a connection,
`NotRunning` that the service is not up; the word after the arrow is what the watchdog did.
A public tunnel gets `skipped <account>.<name>: …` instead, because its health lives in a load
balancer. Both kinds of line appear only under `-v`.

| Form | What it does |
|---|---|
| `gtunnel watch` | The loop, in the foreground, one pass every `watch.interval` (30s) until Ctrl+C |
| `gtunnel watch --once` | One pass, then exit. What the agent runs |
| `gtunnel watch -v` | Say what each pass saw, rather than only what it did |
| `gtunnel watch install` | Install the agent, so the loop runs with no terminal open |
| `gtunnel watch uninstall` | Remove the agent |
| `gtunnel watch status` | Whether the agent is installed, where its log is, and when its last pass finished |

`watch` needs no Google credentials and asks for none: under an agent with no terminal a
login prompt would hang forever.

### What it does about a tunnel that is failing

Nothing, the first time: one failed check is a slow moment, two in a row is a problem.

| Failed check | What the watchdog does | Then waits |
|---|---|---|
| 1st | Counts it | |
| 2nd in a row | Trips: from here this is a problem, not a blip | 10s |
| The first one after that wait | Restart 1 | 20s |
| The first one after that wait | Restart 2 | 40s |
| The first one after that wait | Restart 3 | 80s |
| The first one after that wait | Restart 4 | |
| The one after restart 4 | Stops it, with a line in the log saying why | |

| Fact | Detail |
|---|---|
| A failed check inside a wait | does nothing; at a 30s interval the earliest stop is six minutes after the first failed check |
| Two passing checks in a row | clear the breaker; so does stopping the tunnel yourself |
| After the stop | the log says what the watchdog saw and who held the connections. Nothing restarts it: the next pass forgets it. `gtunnel start <name>` brings it back, and the ladder starts over |
| A tunnel you stop or park while a check is running | is left alone: the pass reads its state again before acting on what it saw |
| Two passes at once | the second skips its round (`-v` says so), so neither overwrites the other's count |

A tunnel stopped on a spent [restart budget](configuration.md#the-restart-budget) is skipped:
restarting it would undo that decision with a fresh count.

### The agent

`watch install` writes one job for the machine: `com.gtunnel.watchdog` on macOS,
`gtunnel-watchdog.timer` on Linux. It wakes every `watch.interval` (30s, read at install),
makes one pass and exits. It is a timer, not a service, because a job that exits at once under
`KeepAlive` is respawned in a tight loop.

```console,ignore
$ gtunnel watch install
Watchdog installed: /usr/local/bin/gtunnel every 30s
Log: ~/.gtunnel/logs/watchdog.log
```

The job runs the installing binary by absolute path; after moving or replacing it, run
`gtunnel watch install` again. The log holds only actions: a clean pass writes nothing.
`gtunnel doctor` reports tunnels with no watchdog, and `--fix` installs it.

### Notifications

Every pass also posts one desktop notification for the state changes it finds across every
account. The states are stopped, starting, up, degraded (running but failing its probe twice in
a row, the breaker's count) and failed.

| Change | Default | Switch |
|---|---|---|
| Up to degraded, or a start that comes up not answering | on | `degraded` |
| Degraded or failed back to up | on | `recovered` |
| Up to failed, a tunnel that died, a start that failed | on | `failed` |
| A start that works | off | `up` |
| Anything you did yourself: `start`, `stop`, `restart`, `delete`, `rename`, `move` | never | none |

A pass's changes become one notification, so a laptop waking is one alert
(`3 tunnels: 2 unhealthy, 1 recovered`).

```toml
[notifications]
enabled = false
up = false
degraded = true
recovered = true
failed = true
```

Notifications are off until you turn them on: press `N` in the TUI, or set `enabled = true` in
`config.toml`. `N` changes only that line. Missing keys take the defaults, and `init` writes
`enabled = false`. `GTUNNEL_NOTIFICATIONS=true` turns them on for one run and
`GTUNNEL_NOTIFICATIONS=false` forces them off, whatever the file says.

A `config.toml` written before `config_version = 2` was switched off once, on the first
command that read it, and says so on stderr from an interactive command (not from the
watchdog or `mcp`). Setting `enabled = true` afterwards stays on.

| Who notifies | Rule |
|---|---|
| Shared ledger | `lifecycle.json` in the configuration directory: a change is announced by whoever stores it first, never twice |
| Watchdog | writes `run/watchdog.heartbeat` after each pass |
| TUI | quiet while the heartbeat is under 90 seconds old; otherwise it announces what it sees, for the account on screen only |
| TUI, notifications on, no watchdog installed | says `notifications need the watchdog when the TUI is closed: gtunnel watch install` once, installs nothing. With notifications off it says nothing |
| A tunnel never seen before | recorded silently, so the first pass does not announce every state |

## doctor

Reports what gtunnel can see without changing anything.

```console,ignore
$ gtunnel doctor          # with a gcloud whose Python has no numpy
gtunnel:
  - gcloud's Python is missing numpy, which makes IAP tunnels slow. Run `gcloud components reinstall`
account dev:
  - gcloud has no usable credentials for account 'dev'. Run `gcloud auth login --configuration=dev` and try again
```

A service with no record is machine-wide, so it is filed under `gtunnel:` like the watchdog;
which services count is in [Leftovers](troubleshooting.md#leftovers):

```console,ignore
$ gtunnel doctor
gtunnel:
  - Service com.gtunnel.dev.old-tunnel is installed but no tunnel in tunnels.toml owns it, so it may still hold a local port at login
```

A run record this version cannot parse is one finding per file, for the account it names. Nothing is removed: the process behind it may be alive and its ports are held until you delete the file.

A key in `config.toml` or `tunnels.toml` that gtunnel does not read is machine-wide too, one
finding per key with its path. Loading ignores such a key, so nothing else shows it; the
usual cause is `log_level` typed below an `[[accounts]]` table, where TOML files it under that
table. Nothing is removed for you: edit the file, or fix it in the config page, which refuses
to save one. This is the output for that file, with `gcloud_configuration = "scratch-docs"`
naming a gcloud configuration that does not exist:

```console,ignore
$ gtunnel doctor
gtunnel:
  ! config.toml has a key gtunnel ignores: accounts[0].log_level. Edit the file and remove or move it
account scratch-docs:
  ! gcloud has no usable credentials for account 'scratch-docs'. Run `gcloud auth login --configuration=scratch-docs` and try again
```

An account whose `credential_file` is relative or starts with `~` is reported per account,
and `--fix` does not touch it:

```console,ignore
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

`--fix` removes leftovers no live tunnel claims, brings auto-start to `tunnels.toml`, and
installs a missing watchdog ([Leftovers](troubleshooting.md#leftovers)). A finding it cannot
fix, such as a missing firewall rule, comes with the exact command.

A gcloud, proxy or CA variable your shell sets whose value differs from the one in a tunnel's
service is reported per account, by name only, and never fixed for you; `gtunnel restart
<name>` writes the shell's value. The list is under
[Variables a service takes from your shell](configuration.md#variables-a-service-takes-from-your-shell).

A reserved account name (`run` or `run-*`, see `account add`) is reported the same way and is
never fixed for you: renaming an account means adding one and moving its tunnels.

The report comes first, then the question; declining changes nothing. Written from the format
strings in `src/doctor.rs`:

```console,ignore
$ gtunnel doctor --fix
account dev:
  - Tunnel 'old-tunnel' is in tunnels.toml but has no service installed

Remove 1 leftover(s)? [y/N] [n]: y
  fixed: Tunnel 'old-tunnel' is in tunnels.toml but has no service installed
```

When the findings hold nothing gtunnel may remove:

```console,ignore
$ gtunnel doctor --fix
account dev:
  ! Tunnel 'db' wants local port 15432, which is already in use

Nothing here is gtunnel's to remove
```

`-` marks a finding `--fix` handles, `!` one it only explains.

## reset

Removes every local trace: services, config, state, logs. It never deletes anything in GCP.

A `gtunnel run` still alive is stopped first, and the config is removed only after the run has exited, so no run is left holding a port with no record to find it by. If the runs cannot be listed, `reset` stops before it changes anything; if one does not stop within 10s, the config is kept and `reset` says which run to stop. A run that cannot be inspected counts as still running, never as stopped, and an exited run that has not been reaped counts as stopped. Nothing writes into the config after it is removed: `reset` prints no update hint, and a stopping run does not recreate its record directory.

```console
$ gtunnel reset
This removes 2 local tunnel(s), 4 account(s) and [HOME]/.gtunnel
Remove all local gtunnel configuration? [y/N] [n]: Cancelled

```

```console,ignore
$ gtunnel reset -y
This removes 1 local tunnel(s), 1 account(s) and /Users/you/.gtunnel
Removed /Users/you/.gtunnel
Cloud resources were not deleted. Run gtunnel relay down first if you want them gone.
```

```console,ignore
$ gtunnel reset -y
Nothing to remove: /Users/you/.gtunnel does not exist
```

## update

```console,ignore
$ gtunnel update --check
Checking for updates...
Update available: v0.1.34 -> v0.1.35

Run `gtunnel update` to install.
```

A format sample from a downloaded binary; versions differ per run. A Homebrew install names
`brew upgrade gtunnel` instead. On the latest release the second line is
`gtunnel v<version> is already the latest version.`

Every other command prints a one-line hint when a cached check found a newer version: see
[Update hint](usage.md#update-hint).

The check runs in the background, never blocks a command, and runs at most once per
[`update_check_interval`](configuration.md#settings-the-environment-can-override) (6 hours by
default). A failed check is recorded and holds the next off for the same interval; the TUI says
`update check failed` until one succeeds. A downloaded binary and one in `~/.cargo/bin` replace
themselves after verifying the published SHA-256, by renaming a new file into place, never by
rewriting the running one; a Homebrew install is pointed at
`brew upgrade gtunnel`.

Before the swap the new binary runs `--version` (10 s limit) and must report the release it was
downloaded for, so a build that cannot start on this machine is refused and the installed one is
left as it was. A download over 64 MiB is refused. The replaced binary is kept as `gtunnel.old`
beside the installed one.

| Flag | Effect |
|---|---|
| `--check` | Report whether a newer release exists; install nothing |
| `--rollback` | Rename `gtunnel.old` over the installed binary. Needs no network. Refused when there is no backup, and cannot be combined with `--check` |

```console,ignore
$ gtunnel update --rollback
Restored the previous version at /usr/local/bin/gtunnel
```

The backup is spent by a rollback, so a second one is refused; the next `update` writes a new
one. A Homebrew install keeps no backup here and is pointed at `brew upgrade gtunnel`.

The install is read from where the file really is, so a symlink does not hide it:

| Install | `update` does |
|---|---|
| `/usr/local/bin/gtunnel` linked into `/usr/local/Cellar/gtunnel/<version>/` (Intel Homebrew), or the Linuxbrew equivalent | Points at `brew upgrade gtunnel`; touches nothing |
| A symlink you made, such as `~/bin/gtunnel` | Replaces the file it points to and keeps the symlink; `gtunnel.old` sits beside that file |

Services, the watchdog and `gtunnel_run` name the binary by a stable path: a trailing
` (deleted)` that Linux appends after an update is dropped, and when `/opt/homebrew/bin`,
`/usr/local/bin`, a Homebrew `opt/gtunnel/bin` or a `PATH` entry reaches the same file, that path
is used instead of the versioned Cellar one that `brew upgrade` removes.

## mcp

```console
$ gtunnel mcp --help
Run Model Context Protocol (MCP) stdio server

Usage: gtunnel mcp [OPTIONS]

Options:
      --account <ACCOUNT>  Use a specific account (overrides the default)
      --validate           Validate tool definitions and exit
      --log-level <LEVEL>  How much to log: quiet, info or debug (default: info)
  -h, --help               Print help

```

`mcp` serves newline-delimited JSON-RPC 2.0 on stdin and stdout, so an MCP client starts it as a
child process. The tools and the client setup are in [The native MCP Server](usage.md#the-native-mcp-server).

## Errors and exit codes

Success exits 0. A failure has a kind, and the kind decides the exit code: branch on the code,
or on `error.kind` under `--json`, never on the text. The failure is one line on stderr
beginning `Error:`; a start that did not come up keeps its `✗` lines on stdout with no `Error:`
line.

| Kind | Exit | Meaning |
|---|---|---|
| `internal` | 1 | Anything unclassified: an unreadable config, a failed write, a tunnel that did not come up for a reason other than credentials, a GCP error that is none of the rows below (a 409, a 500) |
| `usage` | 2 | A request gtunnel refuses: a bad argument or setting (a target without a port, a bad `GTUNNEL_LOG_LEVEL`), a name already taken (`add`, `expose`, `rename`, `move`, `account add`), an empty `--configuration`, a gcloud configuration that does not exist, an API that is disabled in the project (403 `SERVICE_DISABLED`), an invalid `--zone`, an `account edit` with nothing to change or an empty project or zone, a local port a run holds, a move that cannot happen, `expose` or `run --public` on an account with no relay, or a local target that is not a port or host:port. Same code clap uses for a malformed command line |
| `not_found` | 3 | The tunnel or the account named does not exist, the instance a `move` names is missing from the destination zone, or a GCP API answers 404 |
| `auth` | 4 | gcloud has no usable credentials for the account, impersonation was denied, a key was rejected, a GCP API answers 401 or a 403 that is not a quota or a disabled API, or a start failed for one of those reasons |
| `gcloud` | 5 | A gcloud call ran and failed |
| `unavailable` | 6 | Retry later, do not re-authenticate: a gcloud or API call timed out, the network could not be reached, or a quota or rate limit (429, `RATE_LIMIT_EXCEEDED`) was hit |

A reader that stops early, such as `gtunnel list | head -1`, a pager quit with `q`, or an MCP
client that closes its end, ends the command with exit 0 and nothing on stderr: the reader
chose to stop, so there is no failure to report.

`list --json`, `account list --json` and `doctor --json` print the failure on stdout as one
object, and nothing on stderr. `message` is the whole cause chain joined with `: `.

```console
$ gtunnel list --json --account nope
? 3
{"error":{"kind":"not_found","message":"Account 'nope' not found"}}

```

Without `--json` the text stays on stderr and only the exit code follows the kind.
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

Open a new shell afterwards. On each TAB the script runs `gtunnel` again, so the binary on
`PATH` must be the one that printed it: regenerate the file after `gtunnel update` or
`brew upgrade`.

```console,ignore
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
