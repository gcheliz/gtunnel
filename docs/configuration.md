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
| Writes | Atomic, for every file gtunnel writes: a sibling `<name>.<pid>.<n>.new` unique to each write, then a rename and an fsync of the directory, so a crash, a full disk or two writers at once leave one whole file. This covers the launchd plists and systemd units too |
| Locks | `tunnels.lock` and `config.lock` serialise writers, so two gtunnels cannot drop each other's change. No lock is held while a command waits on gcloud or a service manager: that work happens first, then the record changes in one short locked update ([decision 16](decisions.md#16-state-files-are-written-atomically-under-an-advisory-lock)) |
| Unknown keys | Ignored on load, so a file that has one keeps working. The config page refuses to save one and `gtunnel doctor` names each, with its path |

## config.toml

Written by `init`, `account` and `relay up`, and editable in the TUI's config page. Lives
in the configuration directory (see [Where the files live](#where-the-files-live)).

| Key | Type | Required | What it is |
|---|---|---|---|
| `selected_account` | string | yes | The account used when no `--account` is given |
| `log_level` | string | no | `quiet`, `info` or `debug`; `info` when absent. See [The log level](#the-log-level) |
| `start_deadline` | string | no | How long a start waits for an Access tunnel to report it is listening, as a duration; `"20s"` when absent. See [Settings the environment can override](#settings-the-environment-can-override) |
| `discovery` | boolean | no | Whether `list`, the TUI and `mcp` look for tunnel processes started outside gtunnel; `true` when absent |
| `service_call_timeout` | string | no | How long one `launchctl` or `systemctl` call may take before it is reported as `timed out after`, as a duration; `"10s"` when absent |
| `update_check_interval` | string | no | How old the cached release answer may get before the next start runs `update --check` in the background, as a duration; `"6h"` when absent. See [update](commands.md#update) |
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
| `default_zone` | string | yes | The zone `relay up` uses without `--zone`, and the fallback when `add` or `run` cannot look an instance's zone up (an instance's own zone is read from the project by name). Saved from the gcloud configuration when it has one, else typed or left empty |
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

Twenty keys of `config.toml` have a variable each, and so do the five `[restart]` keys; every
one is in the table under [Environment](#environment). Where they disagree the variable wins,
always: there is no key that opts out, so a value edited in the file that has no effect has one
cause, and `gtunnel doctor` names it.

| Where | Beats |
|---|---|
| The variable (the plain spelling, then the older one) | the file |
| The key in `config.toml` | the default |
| nothing set | the default in [Environment](#environment) |

The order is resolved in one place (`src/settings.rs`). A bad variable stops a command with
exit 2 before it runs. The supervisor is exempt, because it must not die of a typo and restart
into it: it falls back to the file with the environment, then the file alone, then the defaults.

| Value | Rule |
|---|---|
| Durations | `humantime` text: `500ms`, `20s`, `1m30s`, `1h30m` |
| Booleans | `true`, `false`, `yes`, `no`, `on`, `off`, `1` or `0` |
| Sizes | bytes (`1048576`) or a number with `B`, `KiB`, `MiB` or `GiB`; no decimal units, so `8MB` is refused. A `keep_size` not below `max_size` fails with exit 2 naming both keys and the variable that set either |
| Older variables | `GTUNNEL_START_DEADLINE_MS` and `GTUNNEL_SERVICE_BOUND_MS` are whole milliseconds above zero; `GTUNNEL_NO_DISCOVERY` and `GTUNNEL_NO_NOTIFY` turn the feature off when set to anything but empty or `0`. When both spellings are set the plain one wins. An empty variable counts as unset |

| Key | What to know |
|---|---|
| `token_ttl` | how long an access token is reused; Google issues them for an hour, so a value past `1h` gets 401s until it lapses |
| `lock_deadline`, `fingerprint_attempts` | under `[gcp]` for want of a section of their own: the wait for a lock another gtunnel holds on a file, and the redos of a URL map or named port change after a concurrent edit |
| `list_probe_timeout` | one probe of a running tunnel's local port, in `list` and the MCP list and status tools; the TUI's probes keep a fixed 2 seconds. `list` probes all tunnels at once, so it waits one probe however many run. Longer helps a slow far end and slows `list` when a tunnel is silent. Not `watch.probe_timeout` (3s), the watchdog's wait |
| `update_check_interval` | paces the background release check of [update](commands.md#update) |
| File-descriptor limit (4096) | no key: checked at compile time (above 256, at most the kernel's 61440) and written into the plist and the unit, so a change needs every service reinstalled and a file value would not reach a running tunnel |

A bad `config.toml` value fails every command with exit 2 naming the key; a bad variable,
older spellings included, fails with exit 2 naming the variable:

```
$ GTUNNEL_START_DEADLINE=soon gtunnel list
Error: GTUNNEL_START_DEADLINE: 'soon' is not a duration like 30s or 1m (expected number at 0)
```

`editor` ranks below `$VISUAL` and `$EDITOR`: `GTUNNEL_EDITOR`, then `$VISUAL`, then
`$EDITOR`, then the file, then `vi`, so a profile's `$EDITOR` is not overruled by a key written
once.

`gtunnel init` writes all of these at their defaults, with `editor = ""` for none chosen, so the
file shows what can be tuned. It adds only the keys that are missing and never changes a value
already there, and every command that saves the file does the same; a file without them loads
with the defaults:

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

[restart]
max_restarts = 5
window = "10m"
min_uptime = "60s"
base_backoff = "10s"
max_backoff = "5m"

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

| Key under `[restart]` | Default | Range |
|---|---|---|
| `max_restarts` | `5` | whole number, `0` to 4294967295; `0` turns the budget off |
| `window` | `"10m"` | a duration above zero, not shorter than `base_backoff` |
| `min_uptime` | `"60s"` | a duration above zero; keep it above gcloud's normal startup |
| `base_backoff` | `"10s"` | a duration above zero; the wait after the first failure, doubled on each next one |
| `max_backoff` | `"5m"` | a duration not shorter than `base_backoff` |

Each key has a `GTUNNEL_RESTART_<KEY>` variable ([Environment](#environment)), resolved in the
order of [Settings the environment can override](#settings-the-environment-can-override), and
`init` writes all five at their defaults. The rules run on the merged result, so a variable
that makes the file's other keys inconsistent is refused like any bad value:

```
$ GTUNNEL_RESTART_WINDOW=soon gtunnel list
Error: GTUNNEL_RESTART_WINDOW: 'soon' is not a duration (expected number at 0)
```

A supervisor reads the budget once, at start. A launchd or systemd job does not see your shell,
so the `GTUNNEL_RESTART_*` variables set at start or install are written into its service
definition; one exported later arrives on the next `start` or `restart`. Cost: a tunnel that
needs longer than `min_uptime` to come up and keeps failing is judged fast-failing.

The first command after an upgrade compares each installed service definition with the one
the new binary writes, and rewrites each that differs, such as one without `--crash-marker`.

| Case | Result |
|---|---|
| A running tunnel | restarted to read the new definition, which drops its connections for a moment |
| A stopped tunnel | stays stopped, its log untouched |
| Reporting | one line on stderr names what was reinstalled; a failure names the service and the reason without changing the exit status |
| Skipped | a tunnel whose service was never installed, a definition that already matches, any run with `GTUNNEL_CONFIG_DIR` set; so a second run does nothing |
| Out of date means | the argument list differs in shape, or the binary no longer exists. The values of `--log-level` and `GTUNNEL_*` come from the calling shell and are ignored, so exporting `GTUNNEL_LOG_LEVEL` for one command restarts nothing |

## Environment

Every variable gtunnel reads. Unless a row says otherwise, a duration or a size must be above
zero and a count is a whole number from 1; the units, the older spellings and the order of
precedence are under [Settings the environment can override](#settings-the-environment-can-override).

| Variable | Older spelling | Beats | Default | What it does, and when to set it |
|---|---|---|---|---|
| `GTUNNEL_CONFIG_DIR` | | nothing: it locates `config.toml`, so the file cannot hold it | `~/.gtunnel` ([Where the files live](#where-the-files-live)) | Keeps every file in another directory, for a throwaway configuration or a CI runner: `config.toml`, `tunnels.toml`, `lifecycle.json`, `run/` (the watchdog heartbeat included), `agents/` and `logs/`. Read by every command. Also turns off the automatic reinstall of out-of-date service definitions, because the label is shared with the live job. See the warning below |
| `GTUNNEL_LOG_LEVEL` | | `log_level`; beaten by `--log-level` | `info` | `quiet`, `info` or `debug` for every command, without a flag. See [The log level](#the-log-level) |
| `GTUNNEL_START_DEADLINE` | `GTUNNEL_START_DEADLINE_MS` | `start_deadline` | `20s` | How long an Access tunnel is given to report it is listening before `start`, `restart` or `add -s` counts the start as failed, e.g. `45s` for a slow tunnel. Public tunnels use a fixed 3-second settle instead. See [start](commands.md#start-stop-restart-autostart-logs-list-delete) |
| `GTUNNEL_LIST_PROBE_TIMEOUT` | | `list_probe_timeout` | `500ms` | How long `list` and the MCP list and status tools wait for one probe of a running tunnel's local port, e.g. `1s` when a healthy tunnel shows `unknown` over a slow link. Not `GTUNNEL_WATCH_PROBE_TIMEOUT`, the watchdog's wait |
| `GTUNNEL_UPDATE_CHECK_INTERVAL` | | `update_check_interval` | `6h` | How old the cached `update --check` answer may get before the next TUI or command start checks again, e.g. `1h` for a notice that arrives a day late, or longer to check less often. Read by the TUI and every command that prints the update hint. See [update](commands.md#update) |
| `GTUNNEL_SERVICE_CALL_TIMEOUT` | `GTUNNEL_SERVICE_BOUND_MS` | `service_call_timeout` | `10s` | How long one `launchctl` or `systemctl` call may take before it is reported as `timed out after`. Read by every command. A test and diagnostics knob: leave it alone unless you are testing the timeout path or diagnosing a slow service manager |
| `GTUNNEL_DISCOVERY` | `GTUNNEL_NO_DISCOVERY` | `discovery` | `true` | Whether `list`, the TUI and `mcp` look for tunnel processes started outside gtunnel; `false` makes `list` show no `pid N` rows. For tests and diagnostics |
| `GTUNNEL_NOTIFICATIONS` | `GTUNNEL_NO_NOTIFY` | `notifications.enabled` | `true` | Whether desktop notifications are raised, by every command. For tests and diagnostics. See [Notifications](commands.md#notifications) |
| `GTUNNEL_EDITOR` | | `editor`, and `VISUAL` and `EDITOR` | none | The editor `e` opens in the TUI's config page |
| `VISUAL`, then `EDITOR` | | `editor` | none | The editor `e` opens when `GTUNNEL_EDITOR` is unset. `VISUAL` wins because it names the full-screen editor. An empty value counts as unset, since `EDITOR=` is how a shell clears one; neither set falls back to `editor`, then to `vi` |
| `GTUNNEL_WATCH_PROBE_TIMEOUT` | | `watch.probe_timeout` | `3s` | How long the watchdog waits for one probe of a tunnel's local port before it counts a failure; raise it when a slow tunnel is read as wedged |
| `GTUNNEL_WATCH_DEGRADED_AFTER` | | `watch.degraded_after` | `2` | Consecutive failed probes before the watchdog calls a tunnel degraded, the breaker's own run length; for a flapping tunnel or a late alert |
| `GTUNNEL_WATCH_INTERVAL` | | `watch.interval` | `30s` | How often the watchdog makes a pass, read by `watch` and `watch install`; a whole number of seconds written as a duration (`45s`). Installed units keep the value they were written with until `watch install` runs again |
| `GTUNNEL_LOGS_MAX_SIZE` | | `logs.max_size` | `8MiB` | The size a tunnel's log may reach before gtunnel keeps only its tail. Read by every command that writes a tunnel's log, and by the supervisor. For a log that grows too fast or is trimmed too soon |
| `GTUNNEL_LOGS_KEEP_SIZE` | | `logs.keep_size` | `2MiB` | What a trim keeps. Must be below the maximum: both keys are named when it is not |
| `GTUNNEL_LOGS_LINES_PER_MINUTE` | | `logs.lines_per_minute` | `60` | Connection lines the supervisor writes per tunnel per minute; the rest are counted into one line. For a noisy client, or lines you want to see |
| `GTUNNEL_LOGS_AUTH_EXIT_WINDOW` | | `logs.auth_exit_window` | `30s` | How long before gcloud exits a credential-failure line still explains the exit, so a login expiry is not read as a crash. Read by the supervisor |
| `GTUNNEL_GCP_TOKEN_TTL` | | `gcp.token_ttl` | `45m` | How long an access token is reused before gcloud is asked for another, inside the hour Google issues them for. For `401` from a Google API after a long session. Read by every command that calls a Google API, as are the next three |
| `GTUNNEL_GCP_OPERATION_TIMEOUT` | | `gcp.operation_timeout` | `10m` | How long a compute or DNS operation is waited on before it is reported as not finished; for `did not finish within` errors |
| `GTUNNEL_GCP_HTTP_TIMEOUT` | | `gcp.http_timeout` | `30s` | The limit on one API request; for a slow network or VPN |
| `GTUNNEL_GCP_POLL_INTERVAL` | | `gcp.poll_interval` | `2s` | The pause between looks at a running operation; for API quota, or a wait that feels padded |
| `GTUNNEL_GCP_LOCK_DEADLINE` | | `gcp.lock_deadline` | `5s` | How long a command that writes `config.toml`, `tunnels.toml` or the lifecycle ledger waits for the lock another gtunnel holds before reporting `is held by another gtunnel process` |
| `GTUNNEL_GCP_FINGERPRINT_ATTEMPTS` | | `gcp.fingerprint_attempts` | `5` | Tries of a read-modify-write on a URL map or named port after a `412` from a concurrent edit, in `expose` and `doctor --fix`; for `Gave up after N attempts` |
| `GTUNNEL_RESTART_MAX_RESTARTS` | | `restart.max_restarts` | `5` | Fast failures tolerated inside the window; `0` to 4294967295, and `0` turns the budget off. For a tunnel that loops, or one that must never be parked. Read by the supervisor, `start` and `restart`, as are the next four. See [The restart budget](#the-restart-budget) |
| `GTUNNEL_RESTART_WINDOW` | | `restart.window` | `10m` | How long a failure counts. Not shorter than the base backoff |
| `GTUNNEL_RESTART_MIN_UPTIME` | | `restart.min_uptime` | `60s` | How long a run must last to clear the count |
| `GTUNNEL_RESTART_BASE_BACKOFF` | | `restart.base_backoff` | `10s` | The wait after the first failure, doubled on each next one |
| `GTUNNEL_RESTART_MAX_BACKOFF` | | `restart.max_backoff` | `5m` | The cap on that doubling wait. Not shorter than the base backoff |
| `GTUNNEL_TEST_GCP_ENDPOINT` | | nothing | none | Debug builds only, for tests: points every Google API at one base URL, such as a wiremock server. A release build does not read it, and CI checks the name is absent from the release binary |
| `PATH` | | nothing: it is the shell's, shared with every other program | | Where `gcloud` is found at startup |

**`GTUNNEL_CONFIG_DIR` does not move the service label.** The label is still
`com.gtunnel.<account>.<name>`, so give a throwaway configuration an account name of its own,
or `install` restarts the live job that shares the label.

The older spellings exist for tests: the integration tests set `GTUNNEL_NO_NOTIFY`, so
`cargo test` never posts a banner, and `GTUNNEL_NO_DISCOVERY`, so a tunnel running on the host
cannot change a row count.

The value of `VISUAL` or `EDITOR` is split into a program and its arguments here, never
handed to a shell, so `code -w` works and nothing in the variable is interpreted as a
command line. Quotes group a path containing spaces and do nothing else.
