# Configuration

[Home](../README.md) / [Usage](usage.md) / [Commands](commands.md) / Configuration / [Architecture](architecture.md) / [Troubleshooting](troubleshooting.md)

Every file gtunnel writes, every key in the two you may edit, and every environment variable
it reads. The words Access, Expose, target kind and lifetime are defined once in
[Concepts](usage.md#concepts).

## Read before you edit

`config.toml` and `tunnels.toml` are meant to be edited, in the TUI's config page (`C`, then
`e`) or by hand. Each of these costs an afternoon once:

| Pitfall | What happens | Do instead |
|---|---|---|
| Editing `tunnels.toml` while a tunnel runs | The running job keeps its old settings. A launchd or systemd job is rewritten only on a restart | `restart`, or `R` in the TUI, after the edit |
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
| `notifications` | table | no | Which lifecycle changes raise a desktop notification: `up`, `degraded`, `recovered`, `failed`, all booleans. See [Notifications](commands.md#notifications) |
| `restart` | table | no | When a tunnel that keeps failing is stopped: `max_restarts`, `window`, `min_uptime`, `base_backoff`, `max_backoff`. `init` writes all five at their defaults. See [The restart budget](#the-restart-budget) |
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
| `relay_instance` | string | yes | The relay VM's instance name |
| `relay_zone` | string | yes | The zone that VM is in |
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
| `instance` | `instance`, `zone` | A port on a Compute Engine VM |
| `dest_group` | `host`, `region`, `group`, `network` | A host behind an IAP destination group, with no VM in the path |
| `jump` | `instance`, `zone`, `host` | A host only the VM can see, forwarded by ssh on the VM with IAP carrying the ssh connection |

`mode = "expose"` publishes a local service through the relay:

| Key | Type | What it is |
|---|---|---|
| `local_target` | string | The local address being published, as `host:port` |
| `domain` | string | Which registered domain serves it |
| `hostname` | string | The full name the service answers on |
| `relay_port` | u16 | The port reserved on the relay VM, derived from the name |
| `private` | bool | Whether IAP sign-in sits in front of the URL |

### One example per tunnel

The first block is a whole `[[tunnels]]` entry; the others show only `[tunnels.spec]`,
because `name`, `account_name` and `auto_start` do not change. The destination-group entry was
written by `gtunnel add`; the others are the same schema written by hand, because `add` and
`expose` check GCP, and `gtunnel list` read all five back. Names, project and hosts are
placeholders.

Access, `kind = "instance"`: a port of a VM (`gtunnel add db my-vm:5432`).

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
instance = "my-vm"
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
failing is judged fast-failing. A tunnel installed before this feature has no
`--crash-marker` in its definition and keeps looping until its next `start` or `restart`.

## Environment

### At a glance

| Variable | Use it to | For |
|---|---|---|
| `GTUNNEL_CONFIG_DIR` | keep every file in another directory | a throwaway configuration |
| `GTUNNEL_LOG_LEVEL` | set `quiet`, `info` or `debug` without a flag | every command |
| `GTUNNEL_START_DEADLINE_MS` | give a slow tunnel longer to listen | a start that fails before the tunnel is up |
| `GTUNNEL_RESTART_MAX_RESTARTS` | set how many fast failures a tunnel survives; `0` turns the budget off | a tunnel that loops, or one that must never be parked |
| `GTUNNEL_RESTART_WINDOW` | set how long a failure counts, e.g. `10m` | the restart budget |
| `GTUNNEL_RESTART_MIN_UPTIME` | set how long a run must last to clear the count, e.g. `60s` | the restart budget |
| `GTUNNEL_RESTART_BASE_BACKOFF` | set the wait after the first failure, e.g. `10s` | the restart budget |
| `GTUNNEL_RESTART_MAX_BACKOFF` | cap the doubling wait, e.g. `5m` | the restart budget |
| `VISUAL`, then `EDITOR` | choose the editor `e` opens | the TUI's config page |
| `GTUNNEL_NO_NOTIFY` | silence desktop notifications | tests and diagnostics |
| `GTUNNEL_NO_DISCOVERY` | stop looking for processes outside gtunnel | tests and diagnostics |
| `GTUNNEL_SERVICE_BOUND_MS` | bound one `launchctl` or `systemctl` call | tests and diagnostics |

### Reference

| Variable | Read by | Effect |
|---|---|---|
| `GTUNNEL_CONFIG_DIR` | every command | Moves `config.toml`, `tunnels.toml`, `lifecycle.json`, `run/` (including the watchdog heartbeat), `agents/` and `logs/` into that directory. The service label is still `com.gtunnel.<account>.<name>`, so give a throwaway configuration an account name of its own or `install` restarts the live job that shares the label |
| `GTUNNEL_LOG_LEVEL` | every command | `quiet`, `info` or `debug`. Beaten by `--log-level`, beats `log_level` in `config.toml`. An empty value counts as unset; anything else that is not a level is an error naming the variable. See [The log level](#the-log-level) |
| `GTUNNEL_NO_NOTIFY` | every command | Silences desktop notifications when set to anything but empty or `0`. The integration tests set it, so `cargo test` never posts a banner |
| `GTUNNEL_NO_DISCOVERY` | `list`, the TUI, `mcp` | For tests: when set to anything but empty or `0`, no process outside gtunnel is looked for, so `list` shows no `pid N` rows. The integration tests set it so a tunnel running on the host cannot change a row count |
| `GTUNNEL_START_DEADLINE_MS` | `start`, `restart`, `add -s` | Milliseconds an Access tunnel is given to report it is listening before the start counts as failed. Default 20000; an empty or non-numeric value counts as unset. Public tunnels use a fixed 3-second settle instead |
| `GTUNNEL_RESTART_MAX_RESTARTS` | the supervisor, `start`, `restart` | Fast failures tolerated inside the window; a whole number, `0` disables the budget. Beats `max_restarts` under `[restart]`. An empty value counts as unset; anything else that is not a number in range is an error naming the variable. See [The restart budget](#the-restart-budget) |
| `GTUNNEL_RESTART_WINDOW` | the supervisor, `start`, `restart` | How long a failure counts, as a duration (`10m`). Beats `window`. Must not be shorter than the base backoff |
| `GTUNNEL_RESTART_MIN_UPTIME` | the supervisor, `start`, `restart` | How long a run must last to clear the count (`60s`). Beats `min_uptime` |
| `GTUNNEL_RESTART_BASE_BACKOFF` | the supervisor, `start`, `restart` | The wait after the first failure (`10s`), doubled on each next one. Beats `base_backoff` |
| `GTUNNEL_RESTART_MAX_BACKOFF` | the supervisor, `start`, `restart` | The cap on that wait (`5m`). Beats `max_backoff`; not shorter than the base backoff |
| `GTUNNEL_SERVICE_BOUND_MS` | every command | A test and diagnostics knob: milliseconds to wait for one `launchctl` or `systemctl` call before it is reported as `timed out after`. Default 10000; an empty, zero or non-numeric value counts as unset. Leave it alone unless you are testing the timeout path or diagnosing a slow service manager |
| `VISUAL`, then `EDITOR` | the TUI's config page | Which editor `e` opens. `VISUAL` wins because it names the full-screen editor. An empty value counts as unset, since `EDITOR=` is how a shell clears one, and neither set falls back to the platform default |
| `PATH` | startup | Where `gcloud` is found |

The value of `VISUAL` or `EDITOR` is split into a program and its arguments here, never
handed to a shell, so `code -w` works and nothing in the variable is interpreted as a
command line. Quotes group a path containing spaces and do nothing else.

With `GTUNNEL_CONFIG_DIR` unset the directory is `~/.gtunnel` (falling back to
`~/Library/Application Support/gtunnel` on macOS and `$XDG_CONFIG_HOME/gtunnel` on
Linux if `~/.gtunnel` does not exist).
