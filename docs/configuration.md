# Configuration

[Home](../README.md) / [Usage](usage.md) / [Commands](commands.md) / Configuration / [Architecture](architecture.md) / [Troubleshooting](troubleshooting.md)

Two files you may edit, every default, and the variables that beat them. Read the pitfalls
first. The words Access, Expose, target kind and lifetime are defined once in
[Concepts](usage.md#concepts).

## Read before you edit

`config.toml` and `tunnels.toml` are meant to be edited, in the TUI's config page (`C`, then
`e`) or by hand. Each of these costs an afternoon once:

| Pitfall | What happens | Do instead |
|---|---|---|
| Editing `tunnels.toml` while a tunnel runs | The running job keeps its old settings until its definition is rewritten, which the next gtunnel command does and restarts the job | Run any command, or `restart` (`R` in the TUI) to do it now |
| Renaming a tunnel by hand | The old launchd or systemd job stays behind, because the service is named for the tunnel | `gtunnel rename`, which moves the record, the service and the log together |
| Treating `name` as the identity | The identity is `name` plus `account_name`. The service and log are named `<account>-<name>`, so `prod` + `eu-api` and `prod-eu` + `api` collide | Pick a name that makes the joined label unique. `add` refuses a clash |
| Reusing a local port | Two tunnels cannot share a `local_port`, in any account, and none may use a port a live `gtunnel run` holds | Give each tunnel its own port; the config page refuses a duplicate |
| `log_level` after the first `[[accounts]]` | TOML files it under the table above it, where gtunnel has no such key, so the global level never changes. Loading ignores a key it does not know, so a file that has one keeps working. The config page refuses to save it (`not saved: unknown key accounts[0].log_level`), and `gtunnel doctor` reports it | Keep `log_level` at the top of the file, before any table |
| `selected_account` naming no account | Every command fails with `Account 'nope' not found` and exit 3 | Name an existing account, or run `gtunnel account select` |
| A hand-written tunnel whose `mode` and `kind` disagree with its keys | The field set differs per `mode` and per `target.kind` | Copy the matching example below |
| Hand-editing `breakers.toml`, `lifecycle.json` or `run/` | The watchdog rewrites them whole on its next pass | Leave them alone |

## Where the files live

| | |
|---|---|
| Directory | `~/.gtunnel`. A legacy `~/Library/Application Support/gtunnel` (macOS) or `~/.config/gtunnel` (Linux) is used only when it exists and `~/.gtunnel` does not. `GTUNNEL_CONFIG_DIR` overrides both |
| Mode | 0600, because the contents name accounts, projects and hosts |
| Writes | Atomic: a sibling `<name>.<pid>.new`, then a rename, so a crash leaves the previous file whole |
| Locks | `tunnels.lock` and `config.lock` serialise writers, so two gtunnels cannot drop each other's change |
| Unknown keys | Ignored on load, so a file that has one keeps working. The config page refuses to save one and `gtunnel doctor` names each, with its path |

## config.toml

Written by `init`, `account` and `relay up`, and editable in the TUI's config page. Lives
in the configuration directory (see [Environment](#environment) below).

| Key | Type | Required | What it is |
|---|---|---|---|
| `selected_account` | string | yes | The account used when no `--account` is given |
| `log_level` | string | no | `quiet`, `info` or `debug`; `info` when absent. See [The log level](#the-log-level) |
| `start_deadline` | string | no | How long a start waits for an Access tunnel to report it is listening, as a duration; `"20s"` when absent. See [Settings the environment can override](#settings-the-environment-can-override) |
| `discovery` | boolean | no | Whether `list`, the TUI and `mcp` look for tunnel processes started outside gtunnel; `true` when absent |
| `service_call_timeout` | string | no | How long one `launchctl` or `systemctl` call may take before it is reported as `timed out after`, as a duration; `"10s"` when absent |
| `update_check_interval` | string | no | How old the cached release answer may get before the next start runs `update --check` in the background, as a duration; `"6h"` when absent. A failed check counts as a check, so an offline laptop does not retry on every start. See [Settings the environment can override](#settings-the-environment-can-override) |
| `list_probe_timeout` | string | no | How long `list` and the MCP list and status tools wait for one probe of a running tunnel's local port, as a duration; `"500ms"` when absent. Not `[watch] probe_timeout`, the watchdog's wait. See [Settings the environment can override](#settings-the-environment-can-override) |
| `editor` | string | no | The editor `e` opens, below `$VISUAL` and `$EDITOR`; `""` (none chosen) when absent |
| `notifications` | table | no | Which lifecycle changes raise a desktop notification: `enabled` (the master switch), then `up`, `degraded`, `recovered`, `failed`, all booleans. See [Notifications](commands.md#notifications) |
| `restart` | table | no | When a tunnel that keeps failing is stopped: `max_restarts`, `window`, `min_uptime`, `base_backoff`, `max_backoff`. `init` writes all five at their defaults. See [The restart budget](#the-restart-budget) |
| `watch` | table | no | How the watchdog probes and when it calls a tunnel degraded: `probe_timeout`, `degraded_after`, `interval`. `init` writes all three at their defaults. See [Settings the environment can override](#settings-the-environment-can-override) |
| `accounts` | array of tables | yes | One entry per account; may be empty |

Each entry in `accounts`:

| Key | Type | Required | What it is |
|---|---|---|---|
| `name` | string | yes | What you type after `--account`, and the first half of the service label |
| `gcloud_configuration` | string | yes | The gcloud configuration whose credentials and project gtunnel uses |
| `project` | string | yes | The GCP project every API call targets. Picked or typed: `init` and the TUI's edit dialog list the projects the account can see and accept a typed id as well, with the gcloud configuration's project as the default |
| `default_zone` | string | yes | The zone assumed when `add` is given an instance and no `--zone`. Typed |
| `impersonate_service_account` | string | no | A service account to impersonate; omitted entirely when unset. The logged-in user needs `roles/iam.serviceAccountTokenCreator` on it |
| `credential_file` | string | no | Absolute path of a service-account key or a workload identity federation file. gtunnel passes it to gcloud on every call as `--credential-file-override=<path>` and never copies, stores or logs it; `doctor` reads it to check its type and mode. Omitted entirely when unset; a relative path or `~` does not stop the config loading: the commands of that account exit 2 naming it, `account edit` clears it, and `doctor` reports it. See [Running in CI](usage.md#running-in-ci) |
| `expose` | table | no | The account's relay, written by `relay up` and removed by `relay down` |

`expose`, when present:

| Key | Type | Required | What it is |
|---|---|---|---|
| `relay_instance` | string | yes | The relay instance's instance name |
| `relay_zone` | string | yes | The zone that instance is in |
| `network` | string | yes | The VPC network the relay and its load balancer sit in |
| `default_domain` | string | yes | The domain used when `expose` is given no `--domain` |
| `domains` | array of tables | no | Every registered domain; `default_domain` must name one of them |

Each entry in `domains` has `domain` (the DNS name) and `managed_zone` (the Cloud DNS
managed zone that serves it). `domains add` writes these; `gtunnel domains` lists them.

## tunnels.toml

Written by `add`, `expose`, `rename`, `move`, `autostart` and `delete`. Never edit it by
hand while a tunnel is running: the service label is derived from the name, so a renamed
tunnel leaves its old launchd or systemd job behind. Use `rename`, which moves both.

Every tunnel is an entry in a `[[tunnels]]` array:

| Key | Type | Required | What it is |
|---|---|---|---|
| `name` | string | yes | Up to 40 characters of `[a-z0-9-]`, unique within the account |
| `account_name` | string | yes | Which account owns it; the same name may exist in another account |
| `auto_start` | bool | no, defaults false | Start at login |
| `spec` | table | yes | What the tunnel does, tagged by `mode` |

`spec` is one of two shapes, told apart by `mode`.

`mode = "access"` forwards a remote port to localhost over IAP:

| Key | Type | What it is |
|---|---|---|
| `remote_port` | u16 | The port on the far side |
| `local_port` | u16 | The port on this machine, derived from the name unless `--local-port` was given |
| `target` | table | Where the far side is, tagged by `kind` |

`target` is one of three shapes:

| `kind` | Keys | Reaches |
|---|---|---|
| `instance` | `instance`, `zone` | A port on a Compute Engine instance |
| `dest_group` | `host`, `region`, `group`, `network` | A host behind an IAP destination group, with no instance in the path |
| `jump` | `instance`, `zone`, `host` | A host only the instance can see, forwarded by ssh on the instance with IAP carrying the ssh connection |

`mode = "expose"` publishes a local service through the relay:

| Key | Type | What it is |
|---|---|---|
| `local_target` | string | The local address being published, as `host:port` |
| `domain` | string | Which registered domain serves it |
| `hostname` | string | The full name the service answers on |
| `relay_port` | u16 | The port reserved on the relay instance, derived from the name |
| `private` | bool | Whether IAP sign-in sits in front of the URL |

### One example per tunnel

The first block is a whole `[[tunnels]]` entry; the others show only `[tunnels.spec]`,
because `name`, `account_name` and `auto_start` do not change. The destination-group entry was
written by `gtunnel add`; the others are the same schema written by hand, because `add` and
`expose` check GCP, and `gtunnel list` read all five back. Names, project and hosts are
placeholders.

Access, `kind = "instance"`: a port of a GCP instance (`gtunnel add db bastion:5432`).

```toml
[[tunnels]]
name = "db"
account_name = "dev"
auto_start = false

[tunnels.spec]
mode = "access"
remote_port = 5432
local_port = 15432

[tunnels.spec.target]
kind = "instance"
instance = "bastion"
zone = "europe-west1-b"
```

Access, `kind = "jump"`: a host only the instance can reach, forwarded by ssh on the
instance (`gtunnel add k8s 10.0.0.10:443 --via bastion`). `instance` is the machine ssh runs
on, `host` is the destination. It also needs ssh to the instance and port 22 open to
`35.235.240.0/20`; see [Three kinds of Access target](usage.md#three-kinds-of-access-target).

```toml
[tunnels.spec]
mode = "access"
remote_port = 443
local_port = 8443

[tunnels.spec.target]
kind = "jump"
instance = "bastion"
zone = "europe-west1-b"
host = "10.0.0.10"
```

Access, `kind = "dest_group"`: a host behind an IAP destination group
(`gtunnel add pg 10.0.0.5:5432 --dest-group prod --region europe-west1 --network default`).

```toml
[tunnels.spec]
mode = "access"
remote_port = 5432
local_port = 15433

[tunnels.spec.target]
kind = "dest_group"
host = "10.0.0.5"
region = "europe-west1"
group = "prod"
network = "default"
```

Expose, public (`gtunnel expose web 3000`) and gated (`gtunnel expose admin 3001 --private`).
Only `private` differs. An Expose tunnel has no `local_port`: the relay holds the port.

```toml
[tunnels.spec]
mode = "expose"
local_target = "localhost:3000"
domain = "example.com"
hostname = "web.example.com"
relay_port = 30001
private = false
```

`gtunnel list` labels these rows `access`, `jump`, `group`, `public` and `gated`. That
column is a display label for the kind or the visibility, not the `mode` key.


## Files gtunnel owns

`breakers.toml`, `lifecycle.json`, `run/`, `agents/`, `logs/` and `update-check.json` are
written by gtunnel and rewritten whole, so a hand edit is lost. What each one holds is in the
[file inventory](architecture.md#files).

## The log level

How much every tunnel and every gcloud call says. One setting, `info` by default, mapped
to each tool's own scale — the table is in
[troubleshooting.md](troubleshooting.md#one-level-two-scales), which is where it lives.

| Where | How | Beats |
|---|---|---|
| `--log-level <level>` | `gtunnel --log-level debug list`; works on every command | everything |
| `GTUNNEL_LOG_LEVEL` | `GTUNNEL_LOG_LEVEL=quiet gtunnel …` | the file |
| `log_level` in `config.toml` | `log_level = "debug"` | nothing |
| nothing set | `info` | — |

```
$ gtunnel --log-level verbose list
error: invalid value 'verbose' for '--log-level <LEVEL>': 'verbose' is not a log level: use quiet, info or debug

For more information, try '--help'.
$ GTUNNEL_LOG_LEVEL=verbose gtunnel list
Error: GTUNNEL_LOG_LEVEL: 'verbose' is not a log level: use quiet, info or debug
```

Two things the precedence is for. `--log-level` answers "why is this tunnel silent"
without editing a file and putting it back after, and because it is read before the
environment it also works when a shell profile holds the typo you are trying to escape.

The level is global rather than per account or per tunnel: it answers how much you want
to see right now, which is a property of the moment. A running tunnel keeps the level it
started under, because the level is written into the service's command line when the
service is installed — so anything that reinstalls the service moves it: `start`,
`restart`, and editing the tunnel in the TUI. Toggling auto-start (`autostart --on|--off`,
`A` in the TUI, `doctor --fix` on an auto-start finding) never restarts a running tunnel:
on macOS it writes or removes only the copy launchd loads at login, so that copy carries
the level in force at the moment of the toggle. Worth knowing if you exported
`GTUNNEL_LOG_LEVEL` to diagnose something and left it set: the next `autostart --on` puts
that level on the tunnel's start at login.

## Settings the environment can override

Twenty keys of `config.toml` have a variable each. Where they disagree the variable wins, always:
there is no key that opts out, so a value edited in the file that has no effect has one cause,
and `gtunnel doctor` names it.

| Key | Variable | Older variable | Default | Value |
|---|---|---|---|---|
| `start_deadline` | `GTUNNEL_START_DEADLINE` | `GTUNNEL_START_DEADLINE_MS` | `"20s"` | a duration above zero |
| `service_call_timeout` | `GTUNNEL_SERVICE_CALL_TIMEOUT` | `GTUNNEL_SERVICE_BOUND_MS` | `"10s"` | a duration above zero |
| `list_probe_timeout` | `GTUNNEL_LIST_PROBE_TIMEOUT` | none | `"500ms"` | a duration above zero |
| `update_check_interval` | `GTUNNEL_UPDATE_CHECK_INTERVAL` | none | `"6h"` | a duration above zero |
| `discovery` | `GTUNNEL_DISCOVERY` | `GTUNNEL_NO_DISCOVERY` | `true` | `true` or `false` |
| `notifications.enabled` | `GTUNNEL_NOTIFICATIONS` | `GTUNNEL_NO_NOTIFY` | `true` | `true` or `false` |
| `editor` | `GTUNNEL_EDITOR` | none | none | a command line |
| `watch.probe_timeout` | `GTUNNEL_WATCH_PROBE_TIMEOUT` | none | `"3s"` | a duration above zero |
| `watch.degraded_after` | `GTUNNEL_WATCH_DEGRADED_AFTER` | none | `2` | a whole number from 1 |
| `watch.interval` | `GTUNNEL_WATCH_INTERVAL` | none | `"30s"` | a whole number of seconds above zero, as a duration |
| `logs.max_size` | `GTUNNEL_LOGS_MAX_SIZE` | none | `"8MiB"` | a size above zero |
| `logs.keep_size` | `GTUNNEL_LOGS_KEEP_SIZE` | none | `"2MiB"` | a size above zero and below `logs.max_size` |
| `logs.lines_per_minute` | `GTUNNEL_LOGS_LINES_PER_MINUTE` | none | `60` | a whole number from 1 |
| `logs.auth_exit_window` | `GTUNNEL_LOGS_AUTH_EXIT_WINDOW` | none | `"30s"` | a duration above zero |
| `gcp.token_ttl` | `GTUNNEL_GCP_TOKEN_TTL` | none | `"45m"` | a duration above zero |
| `gcp.operation_timeout` | `GTUNNEL_GCP_OPERATION_TIMEOUT` | none | `"10m"` | a duration above zero |
| `gcp.http_timeout` | `GTUNNEL_GCP_HTTP_TIMEOUT` | none | `"30s"` | a duration above zero |
| `gcp.poll_interval` | `GTUNNEL_GCP_POLL_INTERVAL` | none | `"2s"` | a duration above zero |
| `gcp.lock_deadline` | `GTUNNEL_GCP_LOCK_DEADLINE` | none | `"5s"` | a duration above zero |
| `gcp.fingerprint_attempts` | `GTUNNEL_GCP_FINGERPRINT_ATTEMPTS` | none | `5` | a whole number from 1 |

| Where | Beats |
|---|---|
| The variable (the plain spelling, then the older one) | the file |
| The key in `config.toml` | the default |
| nothing set | the default above |

Sizes are bytes (`1048576`) or a number with `B`, `KiB`, `MiB` or `GiB`; there are no decimal
units, so `8MB` is refused instead of meaning two things. A `keep_size` that is not below
`max_size` fails with exit 2 naming both keys, and the variable that set either.
`token_ttl` is how long gtunnel reuses an access token before asking gcloud for another; Google
issues them for an hour, so a value past `1h` makes requests fail with 401 until it lapses.
`lock_deadline` and `fingerprint_attempts` sit under `[gcp]` for want of a section of their own:
the first is how long a command waits for the lock another gtunnel holds on a file it is writing,
the second how many times a change to a URL map or named port is redone after a concurrent edit.
`list_probe_timeout` is the wait for one probe of a running tunnel's local port, in `list` and
the MCP list and status tools; the TUI's own probes keep a fixed 2 seconds. `list` probes every tunnel at once, so it waits one probe's worth however many tunnels
run; a longer value helps a slow far end and makes `list` slower when one tunnel is silent. It is
not `watch.probe_timeout` (3s), which is how long the watchdog waits before it counts a probe as
failed.
`update_check_interval` is how long the cached answer of `gtunnel update --check` is trusted: once
it is older, the next TUI or command start runs the check again in the background and never waits
for it. A failed check is recorded and holds the next one off for the same interval; the TUI says
`update check failed` in the header rule until a check succeeds. The tunnels' file-descriptor limit (4096) has no key: it is checked at compile time
(above 256, at most the kernel's 61440), it is written into the launchd plist and the systemd
unit, and a change needs a reinstall of every service, so a value in `config.toml` would not
reach a running tunnel.
Durations are `humantime` text (`500ms`, `20s`, `1m30s`). Booleans take `true`, `false`, `yes`,
`no`, `on`, `off`, `1` or `0`. The older variables keep working: `GTUNNEL_START_DEADLINE_MS` and
`GTUNNEL_SERVICE_BOUND_MS` are whole milliseconds, and `GTUNNEL_NO_DISCOVERY` and
`GTUNNEL_NO_NOTIFY` turn the feature off when set to anything but empty or `0`. When both
spellings of one key are set the plain one wins. An empty variable counts as unset.

A bad `config.toml` value fails every command with exit 2 naming the key; a bad variable fails
with exit 2 naming the variable. The older variables are checked now too, where they used to be
ignored when they did not parse:

```
$ GTUNNEL_START_DEADLINE=soon gtunnel list
Error: GTUNNEL_START_DEADLINE: 'soon' is not a duration like 30s or 1m (expected number at 0)
```

`editor` ranks below `$VISUAL` and `$EDITOR`: `GTUNNEL_EDITOR`, then `$VISUAL`, then
`$EDITOR`, then the file, then `vi`. A person who set `$EDITOR` in a profile is not overruled by
a key written once.

`gtunnel init` writes all of these at their defaults, with `editor = ""` for none chosen. It
adds only the keys that are missing and never changes a value already there; a file without them
loads with the defaults:

```toml
start_deadline = "20s"
discovery = true
service_call_timeout = "10s"
list_probe_timeout = "500ms"
update_check_interval = "6h"
editor = ""

[notifications]
enabled = true
up = false
degraded = true
recovered = true
failed = true

[watch]
probe_timeout = "3s"
degraded_after = 2
interval = "30s"

[logs]
max_size = "8MiB"
keep_size = "2MiB"
lines_per_minute = 60
auth_exit_window = "30s"

[gcp]
token_ttl = "45m"
operation_timeout = "10m"
http_timeout = "30s"
poll_interval = "2s"
lock_deadline = "5s"
fingerprint_attempts = 5
```

When a variable gives a value other than the file's, `gtunnel doctor` says so and what to unset:

```
start_deadline is 1m 30s here because GTUNNEL_START_DEADLINE is set in the environment; config.toml says '45s' and is ignored for it. Unset GTUNNEL_START_DEADLINE to use the file
```

The file is read on each use, so a change made in the TUI's config page applies to the next
start without a relaunch.

`watch.interval` is the one key a service manager also reads: `gtunnel watch install` writes it
into the launchd `StartInterval` or the systemd timer, so a new value reaches the installed
watchdog only after `gtunnel watch install` runs again (the foreground `gtunnel watch` loop
reads it at once). The TUI trusts the watchdog's heartbeat for three intervals and never less
than 90 seconds, so a slower watchdog is not read as a dead one; there is no key for that.

## The restart budget

A tunnel whose gcloud keeps dying for a reason gtunnel does not recognise stops instead of
looping. The supervisor counts runs that died before `min_uptime`; past `max_restarts` of
them inside `window` it writes `run/<label>.crash` with the last exit reason and ends with the
exit the service manager leaves alone (0 on macOS, 78 on Linux). The tunnel stays stopped
until `gtunnel start`, which clears the marker and the count. A run that lasts `min_uptime`
or longer clears the count, so a laptop that sleeps and wakes never adds up to a stop.

| Key under `[restart]` | Variable | Default | Range |
|---|---|---|---|
| `max_restarts` | `GTUNNEL_RESTART_MAX_RESTARTS` | `5` | whole number, `0` to 4294967295; `0` turns the budget off |
| `window` | `GTUNNEL_RESTART_WINDOW` | `"10m"` | a duration above zero, not shorter than `base_backoff` |
| `min_uptime` | `GTUNNEL_RESTART_MIN_UPTIME` | `"60s"` | a duration above zero; keep it above gcloud's normal startup |
| `base_backoff` | `GTUNNEL_RESTART_BASE_BACKOFF` | `"10s"` | a duration above zero; the wait after the first failure, doubled on each next one |
| `max_backoff` | `GTUNNEL_RESTART_MAX_BACKOFF` | `"5m"` | a duration not shorter than `base_backoff` |

Durations are `humantime` text: `500ms`, `60s`, `10m`, `1h30m`.

| Where | Beats |
|---|---|
| The `GTUNNEL_RESTART_*` variable | the file |
| `[restart]` in `config.toml` | the default |
| nothing set | the default above |

The rules run on the merged result, so a variable that makes the file's other keys
inconsistent is refused like any bad value. A bad `config.toml` value fails every command with
exit 2 naming the key; a bad variable fails with exit 2 naming the variable:

```
$ GTUNNEL_RESTART_WINDOW=soon gtunnel list
Error: GTUNNEL_RESTART_WINDOW: 'soon' is not a duration (expected number at 0)
```

`gtunnel init` writes the whole section, all five keys at their defaults, so the file shows
what can be tuned. It adds only the keys that are missing and never changes a value already
there; a file without the section loads with the defaults:

```toml
[restart]
max_restarts = 5
window = "10m"
min_uptime = "60s"
base_backoff = "10s"
max_backoff = "5m"
```

A supervisor reads the budget once, when it starts. A launchd or systemd job does not see your
shell, so the `GTUNNEL_RESTART_*` variables set when a tunnel is started or installed are
written into its service definition; a variable exported later reaches it on the next
`start` or `restart`. Cost: a tunnel that needs longer than `min_uptime` to come up and keeps
failing is judged fast-failing.

A tunnel installed before this feature has no `--crash-marker` in its definition. The first
command after an upgrade compares every installed definition with the one the new binary
writes, and rewrites each that differs. A running tunnel is restarted so it reads the new
definition, which drops its connections for a moment; a stopped tunnel stays stopped and its
log is left alone. One line on stderr names what was reinstalled, and a failure names the
service and the reason without changing the exit status. A tunnel whose service was never
installed, a definition that already matches, and any run with `GTUNNEL_CONFIG_DIR` set are
skipped, so a second run does nothing. A definition counts as out of date only when its argument list differs in shape or its binary no longer exists; the values of `--log-level` and `GTUNNEL_*`, which come from whichever shell ran the command, are ignored, so exporting `GTUNNEL_LOG_LEVEL` for one command restarts nothing.

## Environment

### At a glance

| Variable | Use it to | For |
|---|---|---|
| `GTUNNEL_CONFIG_DIR` | keep every file in another directory | a throwaway configuration |
| `GTUNNEL_LOG_LEVEL` | set `quiet`, `info` or `debug` without a flag | every command |
| `GTUNNEL_START_DEADLINE` | give a slow tunnel longer to listen, e.g. `45s` (older: `GTUNNEL_START_DEADLINE_MS`) | a start that fails before the tunnel is up |
| `GTUNNEL_LIST_PROBE_TIMEOUT` | set how long `list` waits for one probe, e.g. `1s` | a healthy tunnel shown as `unknown` over a slow link |
| `GTUNNEL_UPDATE_CHECK_INTERVAL` | set how often the TUI and CLI look for a new release, e.g. `1h` | a notice that arrives a day late, or a check you want off the network less often |
| `GTUNNEL_RESTART_MAX_RESTARTS` | set how many fast failures a tunnel survives; `0` turns the budget off | a tunnel that loops, or one that must never be parked |
| `GTUNNEL_RESTART_WINDOW` | set how long a failure counts, e.g. `10m` | the restart budget |
| `GTUNNEL_RESTART_MIN_UPTIME` | set how long a run must last to clear the count, e.g. `60s` | the restart budget |
| `GTUNNEL_RESTART_BASE_BACKOFF` | set the wait after the first failure, e.g. `10s` | the restart budget |
| `GTUNNEL_RESTART_MAX_BACKOFF` | cap the doubling wait, e.g. `5m` | the restart budget |
| `GTUNNEL_WATCH_PROBE_TIMEOUT` | set how long the watchdog waits for a tunnel's local port, e.g. `3s` | a slow tunnel read as wedged |
| `GTUNNEL_WATCH_DEGRADED_AFTER` | set how many failed probes in a row make a tunnel degraded | a flapping tunnel, or a late alert |
| `GTUNNEL_WATCH_INTERVAL` | set how often the watchdog looks at every tunnel, e.g. `30s` | `watch` and `watch install` |
| `GTUNNEL_LOGS_MAX_SIZE` | set the size a tunnel's log may reach before it is trimmed, e.g. `8MiB` | a log that grows too fast or is trimmed too soon |
| `GTUNNEL_LOGS_KEEP_SIZE` | set what a trim keeps, e.g. `2MiB`; must stay below the maximum | the same |
| `GTUNNEL_LOGS_LINES_PER_MINUTE` | set how many connection lines a tunnel's log takes a minute | a noisy client, or lines you want to see |
| `GTUNNEL_LOGS_AUTH_EXIT_WINDOW` | set how long a credential line is held to explain gcloud's exit, e.g. `30s` | a login expiry read as a crash |
| `GTUNNEL_GCP_TOKEN_TTL` | set how long an access token is reused, e.g. `45m` | `401` from a Google API after a long session |
| `GTUNNEL_GCP_OPERATION_TIMEOUT` | set how long gtunnel waits for a compute or DNS operation, e.g. `10m` | `did not finish within` errors |
| `GTUNNEL_GCP_HTTP_TIMEOUT` | set how long one API request may take, e.g. `30s` | a slow network or VPN |
| `GTUNNEL_GCP_POLL_INTERVAL` | set the pause between looks at a running operation, e.g. `2s` | API quota, or a wait that feels padded |
| `GTUNNEL_GCP_LOCK_DEADLINE` | set how long a write waits for the file lock another gtunnel holds, e.g. `5s` | `is held by another gtunnel process` |
| `GTUNNEL_GCP_FINGERPRINT_ATTEMPTS` | set how many times a concurrent edit of a URL map or named port is retried | `Gave up after N attempts` |
| `GTUNNEL_EDITOR`, then `VISUAL`, then `EDITOR` | choose the editor `e` opens | the TUI's config page |
| `GTUNNEL_NOTIFICATIONS` | switch desktop notifications on or off (older: `GTUNNEL_NO_NOTIFY`) | tests and diagnostics |
| `GTUNNEL_DISCOVERY` | switch the look for processes outside gtunnel on or off (older: `GTUNNEL_NO_DISCOVERY`) | tests and diagnostics |
| `GTUNNEL_SERVICE_CALL_TIMEOUT` | bound one `launchctl` or `systemctl` call (older: `GTUNNEL_SERVICE_BOUND_MS`) | tests and diagnostics |

### Reference

| Variable | Read by | Effect |
|---|---|---|
| `GTUNNEL_CONFIG_DIR` | every command | Also turns off the automatic reinstall of out-of-date service definitions, because the label is shared with the live job. Moves `config.toml`, `tunnels.toml`, `lifecycle.json`, `run/` (including the watchdog heartbeat), `agents/` and `logs/` into that directory. The service label is still `com.gtunnel.<account>.<name>`, so give a throwaway configuration an account name of its own or `install` restarts the live job that shares the label. No `config.toml` key: it locates the file, so the file cannot hold it |
| `GTUNNEL_LOG_LEVEL` | every command | `quiet`, `info` or `debug`. Beaten by `--log-level`, beats `log_level` in `config.toml`. An empty value counts as unset; anything else that is not a level is an error naming the variable. See [The log level](#the-log-level) |
| `GTUNNEL_NOTIFICATIONS` | every command | `true` or `false`: whether desktop notifications are raised. Beats `enabled` under `[notifications]`. An empty value counts as unset; anything else that is not a boolean is an error naming the variable. See [Settings the environment can override](#settings-the-environment-can-override) |
| `GTUNNEL_NO_NOTIFY` | every command | The older spelling of `GTUNNEL_NOTIFICATIONS=false`: silences desktop notifications when set to anything but empty or `0`. `GTUNNEL_NOTIFICATIONS` wins when both are set. The integration tests set it, so `cargo test` never posts a banner |
| `GTUNNEL_DISCOVERY` | `list`, the TUI, `mcp` | `true` or `false`: whether processes outside gtunnel are looked for. Beats `discovery` in `config.toml`. `false` makes `list` show no `pid N` rows |
| `GTUNNEL_NO_DISCOVERY` | `list`, the TUI, `mcp` | The older spelling of `GTUNNEL_DISCOVERY=false`, for tests: when set to anything but empty or `0`, no process outside gtunnel is looked for. The integration tests set it so a tunnel running on the host cannot change a row count |
| `GTUNNEL_START_DEADLINE` | `start`, `restart`, `add -s` | How long an Access tunnel is given to report it is listening before the start counts as failed, as a duration (`45s`). Beats `start_deadline`. Default 20s; an empty value counts as unset, and anything else that is not a duration above zero is an error naming the variable. Public tunnels use a fixed 3-second settle instead |
| `GTUNNEL_START_DEADLINE_MS` | `start`, `restart`, `add -s` | The older spelling, in whole milliseconds above zero. `GTUNNEL_START_DEADLINE` wins when both are set |
| `GTUNNEL_WATCH_PROBE_TIMEOUT` | `watch` | How long one probe of a tunnel's local port may take before it counts as a failure, as a duration (`3s`). Beats `probe_timeout` under `[watch]`. Default 3s; an empty value counts as unset, and anything else that is not a duration above zero is an error naming the variable |
| `GTUNNEL_WATCH_DEGRADED_AFTER` | `watch` | Consecutive failed probes before a tunnel is called degraded; a whole number from 1. Beats `degraded_after`. Default 2, the breaker's own run length |
| `GTUNNEL_WATCH_INTERVAL` | `watch`, `watch install` | How often the watchdog makes a pass, as a whole number of seconds written as a duration (`45s`). Beats `interval`. Default 30s. Installed units keep the value they were written with until `watch install` runs again |
| `GTUNNEL_LOGS_MAX_SIZE` | every command that writes a tunnel's log, the supervisor | The size a log may reach before gtunnel keeps only its tail, as bytes or with `KiB`, `MiB`, `GiB` (`8MiB`). Beats `max_size` under `[logs]`. Default 8MiB. An empty value counts as unset; anything else that is not a size above zero is an error naming the variable |
| `GTUNNEL_LOGS_KEEP_SIZE` | the same | What a trim keeps, same units. Beats `keep_size`. Default 2MiB. Must be below the maximum: both keys are named when it is not |
| `GTUNNEL_LOGS_LINES_PER_MINUTE` | the supervisor | Connection lines written per tunnel per minute; the rest are counted into one line. A whole number from 1. Beats `lines_per_minute`. Default 60 |
| `GTUNNEL_LOGS_AUTH_EXIT_WINDOW` | the supervisor | How long before gcloud exits a credential-failure line still explains it, as a duration. Beats `auth_exit_window`. Default 30s |
| `GTUNNEL_GCP_TOKEN_TTL` | every command that calls a Google API | How long an access token is reused before gcloud is asked for another, as a duration (`45m`). Beats `token_ttl` under `[gcp]`. Default 45m, inside the hour Google issues them for. An empty value counts as unset; anything else that is not a duration above zero is an error naming the variable |
| `GTUNNEL_GCP_OPERATION_TIMEOUT` | the same | How long a compute or DNS operation is waited on before it is reported as not finished. Beats `operation_timeout`. Default 10m |
| `GTUNNEL_GCP_HTTP_TIMEOUT` | the same | The limit on one API request. Beats `http_timeout`. Default 30s |
| `GTUNNEL_GCP_POLL_INTERVAL` | the same | The pause between looks at a running operation. Beats `poll_interval`. Default 2s |
| `GTUNNEL_GCP_LOCK_DEADLINE` | every command that writes `config.toml`, `tunnels.toml` or the lifecycle ledger | How long a writer waits for the lock another gtunnel holds before reporting it. Beats `lock_deadline`. Default 5s |
| `GTUNNEL_GCP_FINGERPRINT_ATTEMPTS` | `expose`, `doctor --fix` | Tries of a read-modify-write on a fingerprinted resource after a `412`; a whole number from 1. Beats `fingerprint_attempts`. Default 5 |
| `GTUNNEL_RESTART_MAX_RESTARTS` | the supervisor, `start`, `restart` | Fast failures tolerated inside the window; a whole number, `0` disables the budget. Beats `max_restarts` under `[restart]`. An empty value counts as unset; anything else that is not a number in range is an error naming the variable. See [The restart budget](#the-restart-budget) |
| `GTUNNEL_RESTART_WINDOW` | the supervisor, `start`, `restart` | How long a failure counts, as a duration (`10m`). Beats `window`. Must not be shorter than the base backoff |
| `GTUNNEL_RESTART_MIN_UPTIME` | the supervisor, `start`, `restart` | How long a run must last to clear the count (`60s`). Beats `min_uptime` |
| `GTUNNEL_RESTART_BASE_BACKOFF` | the supervisor, `start`, `restart` | The wait after the first failure (`10s`), doubled on each next one. Beats `base_backoff` |
| `GTUNNEL_RESTART_MAX_BACKOFF` | the supervisor, `start`, `restart` | The cap on that wait (`5m`). Beats `max_backoff`; not shorter than the base backoff |
| `GTUNNEL_UPDATE_CHECK_INTERVAL` | the TUI and every command that prints the update hint | How old the cached `update --check` answer may get before the next start checks again, as a duration (`6h`). Beats `update_check_interval`. Default 6h; an empty value counts as unset, and anything that is not a duration above zero is an error naming the variable |
| `GTUNNEL_LIST_PROBE_TIMEOUT` | `list`, `mcp` | How long one probe of a running tunnel's local port may take, as a duration (`500ms`). Beats `list_probe_timeout`. Default 500ms; an empty value counts as unset, and anything that is not a duration above zero is an error naming the variable. Not `GTUNNEL_WATCH_PROBE_TIMEOUT`, which is the watchdog's wait |
| `GTUNNEL_SERVICE_CALL_TIMEOUT` | every command | A test and diagnostics knob: how long to wait for one `launchctl` or `systemctl` call before it is reported as `timed out after`, as a duration (`10s`). Beats `service_call_timeout`. Default 10s; an empty value counts as unset. Leave it alone unless you are testing the timeout path or diagnosing a slow service manager |
| `GTUNNEL_SERVICE_BOUND_MS` | every command | The older spelling, in whole milliseconds above zero. `GTUNNEL_SERVICE_CALL_TIMEOUT` wins when both are set |
| `GTUNNEL_EDITOR` | the TUI's config page | Which editor `e` opens, ahead of `VISUAL` and `EDITOR`. Beats `editor` in `config.toml`, which sits below both |
| `VISUAL`, then `EDITOR` | the TUI's config page | Which editor `e` opens. `VISUAL` wins because it names the full-screen editor. An empty value counts as unset, since `EDITOR=` is how a shell clears one; neither set falls back to `editor` in `config.toml`, then to the platform default |
| `PATH` | startup | Where `gcloud` is found. No `config.toml` key: it is the shell's, shared with every other program |

The value of `VISUAL` or `EDITOR` is split into a program and its arguments here, never
handed to a shell, so `code -w` works and nothing in the variable is interpreted as a
command line. Quotes group a path containing spaces and do nothing else.

With `GTUNNEL_CONFIG_DIR` unset the directory is `~/.gtunnel`, unless only a legacy
directory exists: [Where the files live](#where-the-files-live).
