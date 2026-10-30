# Troubleshooting

[Home](../README.md) / [Usage](usage.md) / [Commands](commands.md) / [Configuration](configuration.md) / [Architecture](architecture.md) / Troubleshooting

Contents:

- [Installation](#installation)
- [What the log says, and what it means](#what-the-log-says-and-what-it-means)
- [Local port collisions](#local-port-collisions)
- [gtunnel needs a logged-in Google account](#gtunnel-needs-a-logged-in-google-account)
- [credential_file must be an absolute path](#credential_file-must-be-an-absolute-path)
- [Failed to parse .../config.toml](#failed-to-parse-configtoml)
- [Connections drop after an hour](#connections-drop-after-an-hour)
- [Restarts after the laptop wakes](#restarts-after-the-laptop-wakes)
- [no zone for account](#no-zone-for-account)
- [Could not list projects](#could-not-list-projects)
- [IAP firewall](#iap-firewall)
- [IAM](#iam)
- [Cloud DNS managed zones](#cloud-dns-managed-zones)
- [NumPy](#numpy)
- [Services](#services)
- [Cost](#cost)
- [Leftovers](#leftovers)

Run `gtunnel doctor` first: it finds most of what follows and prints the exact fix. It needs
a working install, so installation problems come first.

## Installation

### `cargo install gtunnel` installed something else

`gtunnel` on crates.io is an unrelated tcp-over-grpc proxy by another author, published in
2022. Installing it is harmless but leaves two foreign binaries behind:

```
$ ls ~/.cargo/bin
gtunnel  gtunnel-client  gtunnel-server
```

`gtunnel-client` and `gtunnel-server` are the other crate's; your own `gtunnel` is untouched,
because that crate ships no binary under that name. With a real gtunnel installed too, cargo
has two packages under one name and refuses to guess:

```
$ cargo uninstall gtunnel
error: specification `gtunnel` is ambiguous
help: re-run this command with one of the following specifications
  path+file:///Users/you/workspace/gtunnel#0.1.0
  registry+https://github.com/rust-lang/crates.io-index#gtunnel@0.1.0
```

Remove the `registry+` line, verbatim and quoted, because it contains a `#`:

```
$ cargo uninstall 'registry+https://github.com/rust-lang/crates.io-index#gtunnel@0.1.0'
    Removing /Users/you/.cargo/bin/gtunnel-client
    Removing /Users/you/.cargo/bin/gtunnel-server
```

With no real gtunnel installed, a bare `cargo uninstall gtunnel` works. Then install the right
one:

```
cargo install --git https://github.com/gcheliz/gtunnel --force
```

`cargo install --list` shows what is left. The entry you want names a `git+` or `path+`
source, never `registry+https://github.com/rust-lang/crates.io-index`.

### `gtunnel --version` reports an older version than the latest release

A binary reports the version of the source it was compiled from. A `cargo install --path .`
from a checkout behind `main` does this, and nothing is broken.

| Check | Command | What it means |
|---|---|---|
| Which binary is running | `which gtunnel` | `~/.cargo/bin` means cargo installed it, `/usr/local/bin` a download |
| Where cargo got it | `cargo install --list` | the source in parentheses is the truth |
| What is published | `gh release list --repo gcheliz/gtunnel --limit 1` | compare the tag against `--version` |

`gtunnel update` brings a downloaded or cargo-installed binary to the latest release, and
`brew upgrade gtunnel` a Homebrew one. To stay on a source build instead:
`git pull && cargo install --path .` from a clone, or
`cargo install --git https://github.com/gcheliz/gtunnel --force` from the repository.

### `gtunnel: command not found` right after installing

The directory holding the binary is not on `PATH`. Cargo installs to `~/.cargo/bin`, which
some shells do not add; the download instructions move the binary to `/usr/local/bin`,
which can be absent on a fresh machine.

```
$ echo "$PATH" | tr ':' '\n' | grep -c -e "$HOME/.cargo/bin" -e /usr/local/bin
0
```

A zero means neither is there. Add the one that holds your binary to `PATH` in
`~/.zshrc`, then open a new shell.

### The checksum or the attestation fails

| Symptom | Cause | Fix |
|---|---|---|
| `shasum: WARNING: 1 computed checksum did NOT match` | truncated or corrupted download | delete both files and fetch them again |
| `gh attestation verify` exits 1 with `HTTP 404: Not Found` | no attestation exists for that exact file | check the file is a release asset and not rebuilt or re-compressed locally; v0.1.0 predates attestations entirely |
| `gh: To get started with GitHub CLI, please run: gh auth login` | `gh` is not authenticated | `gh auth login`, or skip the attestation and rely on the checksum |

A verification that succeeds says nothing at all and exits 0. Silence is the pass.

### macOS refuses to run the downloaded binary

Gatekeeper quarantines anything fetched by a browser. `curl` does not set the quarantine
attribute, so the download block in [usage.md](usage.md#4-install-gtunnel) avoids this
entirely — it appears when the tarball came from the releases page in a browser.

```
xattr -d com.apple.quarantine /usr/local/bin/gtunnel
```

## What the log says, and what it means

gtunnel annotates these lines in `gtunnel logs <name>` and in the TUI's log pane. A log is
emptied when its tunnel starts, so a line in it was written by the process running now.

| Log line contains | Cause | Fix |
|---|---|---|
| `4003` | IAP cannot reach the target port | Allow `35.235.240.0/20` to that port, see below |
| `4033` | Not authorized to tunnel through IAP | Grant `roles/iap.tunnelResourceAccessor` |
| A public URL answers `503` (nothing in the log) | The tunnel's backend, host rule, named port or DNS record is missing | `gtunnel restart <name>` re-creates them, see [the log](commands.md#start-stop-restart-autostart-logs-list-delete) |
| `Reauthentication required` | gcloud credentials expired. The supervisor stops the job instead of restarting it every few seconds, and `list` keeps showing `auth expired` until the next start | `gcloud auth login`, then `gtunnel start <name>` |
| `list` shows `[restart budget spent: ...]` | gcloud died more than `max_restarts` times, each before `min_uptime`, for a reason gtunnel does not recognise. The supervisor stopped the job, and the log says why | Read the log, fix the cause, then `gtunnel start <name>`. To tune or disable the budget see [The restart budget](configuration.md#the-restart-budget) |
| A tunnel keeps restarting and never shows `[restart budget spent: ...]` | Its definition has no `--crash-marker` | Run any gtunnel command: it reinstalls the definition and prints `gtunnel: reinstalled ...` on stderr. If it prints `could not reinstall`, the reason follows; fix it and run a command again |
| `4004` | IAP asked gcloud to reauthenticate; gcloud resumes the session on its own | Nothing, unless it repeats: then check the account's credentials with `gtunnel doctor` |
| `Unable to reconnect within` | gcloud gave up reconnecting to IAP after 15 minutes and the clients on the tunnel were cut | Restart the tunnel; see [Limits](architecture.md#limits) |
| `Address already in use` | Another process holds the local port | `gtunnel doctor`, or set `--local-port` |
| `remote port forwarding failed` | The relay port is busy on the relay instance | `gtunnel restart <name>`, then `gtunnel doctor` |
| `Too many open files` | The tunnel ran out of file descriptors | `gtunnel doctor` names the process holding them, see below |
| `numpy` | gcloud's Python has no NumPy | See NumPy below |
| `timed out after` (in the TUI status line or a command's error) | A subprocess did not answer within its bound: 30s for `gcloud`, 10s for `launchctl` and `systemctl`. Usually `gcloud` waiting on a browser login or a dead network, or a wedged launchd | Run the named command by hand to see what it waits on, then `gcloud auth login` or `gtunnel doctor`. `delete` that times out stopping the job keeps the record, so retry it once the job answers |

### A start that says `failed to start`

The service exited with an error, or an Access tunnel was not listening within the start
deadline (20 seconds by default). The line under it matches a row of the table above, or reads
`not listening after 20s`; `gtunnel logs <name>` shows the rest. For a tunnel that is only slow,
raise `start_deadline` or `GTUNNEL_START_DEADLINE`. How a start is judged:
[start](commands.md#start-stop-restart-autostart-logs-list-delete).

### One level, two scales

How much a tunnel says is one setting, `info` by default. gcloud and ssh scale verbosity
differently and the same word means different things to each, so gtunnel maps the level
to both rather than making you set two:

| gtunnel level | gcloud `--verbosity` | ssh `-o LogLevel` | What you get |
|---|---|---|---|
| `quiet` | `warning` | `ERROR` | Failures only, plus gcloud's own `Listening on port` line |
| `info` (default) | `info` | `DEBUG` | The listener or forward coming up, and one line per client connecting |
| `debug` | `debug` | `DEBUG3` | The above plus gcloud's and ssh's own internals, for a tunnel that will not carry traffic |

`info` maps to ssh `DEBUG` because ssh's forward-up and per-connection lines are `debug()` calls.
`quiet` maps to `ERROR`, not `QUIET`, because `QUIET` hides the failures.

`quiet` is not silence: an Access tunnel still writes gcloud's `Listening on port [N].` and
`Testing if tunnel connection works.` at every start, which `--verbosity` does not govern.
`quiet` removes the per-connection line.

Where to set it, and when a running tunnel picks up a new level, is in
[The log level](configuration.md#the-log-level).

### Who writes what

Three writers append to the same file. The service and its supervisor write for as long as
the tunnel runs; a gtunnel command or the TUI adds the few facts only it knows.

| Writer | Lines | When |
|---|---|---|
| The service's child | gcloud's own output at the configured `--verbosity`, and, for the kinds that run ssh (`--via` and Expose), ssh's at the matching `LogLevel` (see the table above) | Always, from start to exit |
| The tunnel's supervisor | `gtunnel: conn ...` lines, prefixed and timestamped | Always, from start to exit |
| gtunnel | every other `gtunnel: <message>` line, prefixed and timestamped | Only while the TUI or a gtunnel command is running |

Shapes, not a transcript:

| Kind | Looks like | Written by |
|---|---|---|
| Access, listener up | `Listening on port [15432].` | gcloud |
| Jump (`--via`), forward up | `Local forwarding listening on 127.0.0.1 port 8443.` | ssh |
| Access, a client connects | `gtunnel: conn iap-4 open` (or `conn ssh-3 open from 127.0.0.1:54321` when the tool names the client at once) | gtunnel, from the tool's own line |
| Access, a client leaves | `gtunnel: conn iap-4 from 127.0.0.1:54321 closed after 12.4s` | gtunnel, from the tool's own line |
| Access, a burst | `gtunnel: conn ... 240 more in the last minute` | gtunnel |
| Expose, forward up | `debug1: Remote connections from 0.0.0.0:30001 forwarded to local address 127.0.0.1:3000` | ssh |
| Expose, a visitor arrives | `debug1: client_request_forwarded_tcpip: listen 0.0.0.0 port 30001, originator ...` | ssh |

How the `conn` lines are made, what they leave out and how to find the client from one:
[Connections in the log](commands.md#connections-in-the-log).

A `--via` tunnel never prints `Listening on port`: it runs `gcloud compute ssh
--tunnel-through-iap`, which has no listener inside gcloud. Its equivalent is ssh's forwarding
line. Every ssh line above is a debug one ([One level, two scales](#one-level-two-scales)).

### How large a log gets

The level sets how fast a log grows:

| Level | Per ssh handshake | Per client connection |
|---|---|---|
| `quiet` | almost nothing; a failure only | nothing |
| `info` (default) | around 90 lines | a line or two |
| `debug` | substantially more, gcloud's and ssh's internals included | several |

The bound is the same at all three: a log is emptied when its tunnel starts, and past 8 MiB
(`logs.max_size`) gtunnel keeps the last 2 MiB (`logs.keep_size`) behind a first line saying so.
At `debug` a busy tunnel reaches 8 MiB in minutes rather than weeks.

A log that cannot be written, on a full disk for example, stops growing but does not stop the
tunnel: the supervisor keeps reading gcloud's output, so it still stops the job on expired
credentials.

The trim runs only when a gtunnel process writes to the log (the TUI open or a command
running), so an unattended tunnel grows until someone looks. `du -sh` over the logs directory
is worth a look; `gtunnel restart <name>` empties one.

### A tunnel that stops accepting connections

The service is `Running`, the process is alive, and every new client is refused. Nothing
exits, so nothing restarts it, and `gtunnel list` says it is up. The usual cause is file
descriptors: each client costs one, and a service that runs out writes `Too many open files`
and refuses everything while staying up.

| | |
|---|---|
| The ceiling a tunnel gets | 4096, written into the unit by gtunnel; a launchd job left alone inherits 256 on macOS |
| Where it comes from | `SoftResourceLimits`/`NumberOfFiles` in the plist, `LimitNOFILE` in the systemd unit |
| Applied when | The service is installed; an older unit is rewritten by the next gtunnel command |

4096 is more than a tunnel needs, so hitting it means something holds connections open.
`gtunnel doctor` finds the line in the log and names the process holding them:

```console,ignore
$ gtunnel doctor
account dev:
  ! Tunnel 'k8s' ran out of file descriptors: my-client (pid 4471) is holding 3804 of its connections right now. Deal with that, then `gtunnel restart k8s`
```

Example numbers; the shape is real. gtunnel names the client, not the listener, which is only
the victim. The check runs before the credentials check, because a wedged tunnel and an
expired login tend to arrive together.

By hand, run `lsof` on the local port, with `+c 0` for the whole command name (lsof otherwise
cuts it to nine characters). Both ends of each loopback connection are listed, so the two
counts match:

```
$ lsof -nP +c 0 -iTCP:8443 -sTCP:ESTABLISHED | awk 'NR>1{print $1, $2}' | sort | uniq -c | sort -rn
3804 my-client 4471
3804 ssh 1545
```

The watchdog restarts a tunnel in this state and stops one that will not come back:
[watch](commands.md#watch).

## Local port collisions

Before writing a tunnel record, gtunnel checks the local port against its own tunnels and
the machine's listening sockets.

When another gtunnel tunnel already uses the port:

```console,ignore
$ gtunnel add db postgres:5432 --local-port 5432
Error: Local port 5432 is already used by another gtunnel tunnel
```

When an outside process holds the port:

```console,ignore
$ gtunnel add db postgres:5432 --local-port 5432
Error: Local port 5432 is already in use on this machine
```

To see which process holds it:

```
$ lsof -nP -iTCP:5432 -sTCP:LISTEN
```

Pass a different port with `--local-port <port>`, or stop the conflicting service.

## gtunnel needs a logged-in Google account

```
Error: gtunnel needs a logged-in Google account for 'prod'. Run `gcloud auth login`
```

gtunnel could not get an access token at startup, and the login did not finish or could not
start.

| Cause | Fix |
|---|---|
| No terminal: stdin or stdout is redirected, as under cron, a pipe or CI | A browser login cannot finish there. Run `gcloud auth login --configuration=<configuration>` once in a terminal, or use a credential that needs no login |
| The login was cancelled or failed | Run it again; `gcloud auth list` shows who is logged in |
| The login exited 0 but the probe still fails | The configuration gtunnel uses is not the one that was logged in. Check `gcloud config configurations list` against the account's `gcloud_configuration` |
| The account runs on a CI runner with an attached service account | There is no login to run. Give the runner's service account `roles/iap.tunnelResourceAccessor`; see [Running in CI](usage.md#running-in-ci) |
| The account names a `credential_file` | gtunnel never offers a login for it. The error names the file and ends with gcloud's own reason (`; gcloud said: ERROR: ...`): check that it exists, is a service-account key or a workload identity federation file, and that its identity is not disabled. `gtunnel doctor` names the file and repeats the reason |
| The account impersonates a service account | `gcloud auth login` is run without `--impersonate-service-account`, because it rejects that flag. It logs in the user whose credentials mint the impersonation token, so that user needs `roles/iam.serviceAccountTokenCreator` on the service account |

Which commands run the check, for which account, and what the prompt does: [Startup](usage.md#startup).

## `credential_file must be an absolute path`

A relative or `~` `credential_file` stops that account's tunnels with exit 2, naming the
account. Other accounts, and the commands that edit or list accounts, keep working:

```
gtunnel doctor                                   # names the account and the value
gtunnel account edit ci --credential-file /secrets/ci-key.json
gtunnel account edit ci --credential-file ""     # or clear it
```

`doctor --fix` leaves it alone: the right path is yours to choose.

## `Failed to parse .../config.toml`

A `config.toml` that cannot be read (a TOML error, or a value such as `log_level` that is not
one of the choices) stops every command, and `account add` and `init` refuse to write over it:
the error names the file and the line, and the file is left byte for byte as it was. Fix that
line, or move the file aside to start again with `gtunnel init`.

## Connections drop after an hour

A pooled connection that sat idle comes back dead, or a client stalls and then recovers.

| Cause | Fix |
|---|---|
| IAP drops a session after one hour of inactivity, and a direct tunnel gives every connection its own session | Set the pool's idle lifetime under 60 minutes, or tunnel through a GCP instance with `--via`, which keeps one stream busy |
| gcloud is reconnecting: the log shows `Reconnecting with URL`, or a `4004` close | It retries from 1.5 s to 20 s apart for up to 15 minutes and resumes the session. The stall ends when it does |
| The log shows `Unable to reconnect within` | The 15 minutes ran out. Restart the tunnel |

The numbers and their sources are in [Limits](architecture.md#limits).

## Restarts after the laptop wakes

After the machine sleeps, a tunnel's restart count goes up by one and the tunnel is back shortly after. This is expected, not instability.

| Step | What happens |
|---|---|
| Asleep | ssh keepalive timers do not run, and gcloud's IAP websocket to the GCP instance is reset |
| Wake | gcloud notices the dead socket, ssh exits with 255, and the service manager restarts the tunnel |
| Recovered | The listener is back about 40 seconds after the wake |

The log shows the sequence (placeholder names; the shape, not a transcript):

```
Connection to bastion closed by remote host.
gcloud compute ssh exited with return code [255].
Local forwarding listening on 127.0.0.1 port 8443.
```

| You see | Meaning |
|---|---|
| One restart after each wake | Expected. The crash-loop state in the [health table](usage.md#the-tui) needs three restarts inside two minutes |
| A count that rises while the machine stays awake | Not the wake case. Read the log with `gtunnel logs <name>` and look for the cause |
| `Unable to reconnect within [900000] ms` | The network stayed down for gcloud's 15 minute window. The restart is the same one; see [Limits](architecture.md#limits) |

## `no zone for account`

```
Error: no zone for account 'work': pass --zone, or set a default with gtunnel account edit work --zone <zone>
```

Exit code 2, before any request. An account saved without a default zone cannot run the
commands that reach an instance by zone: `add --via`, `add` and `run` to an instance, and
`relay up`.

| Fix | Command |
|---|---|
| For one command | pass `--zone europe-west1-b` |
| For the account | `gtunnel account edit work --zone europe-west1-b` |
| Find a zone | `gtunnel zones` lists the project's zones |

A tunnel through a destination group takes `--region` and needs no zone.

## Could not list projects

```
Could not list projects: GCP API error 403: ...
```

`init` and the TUI's account edit dialog list projects before asking for one. A failure
never blocks the account: the prompt falls back to a typed project id.

| Cause | Fix |
|---|---|
| The list is empty, or `403`, and the account lacks `resourcemanager.projects.get` | The search returns only projects the caller holds that permission on. Ask for a role that carries it (`roles/browser` does), or type the project id: the list is a convenience, and the project id is all gtunnel stores |
| `403` naming the Cloud Resource Manager API as disabled | `gcloud services enable cloudresourcemanager.googleapis.com --project <project>`, where `<project>` is the one the error names, then run `init` again |
| The login expired | `gcloud auth login`, see [above](#gtunnel-needs-a-logged-in-google-account) |
| No network | The line names the connection error; the typed field still works once the project id is known |
| The list is empty | The account sees no active project; type the id of one it can use |

## IAP firewall

The rule for a tunnel's port, and the relay's two rules, are listed in
[What GCP needs](gcp.md#access); `doctor` prints the exact command for the port it checks.

When `doctor` cannot list the project's firewall rules (the identity lacks
`compute.firewalls.list`, or the call failed), it prints one line per account, `could not read
firewall rules: <reason>; the IAP firewall check was skipped`, and judges no tunnel. Grant the
permission or run `doctor` again; the rule is not reported missing, because it was never looked for.

An Expose tunnel that returns 502 with no error in the log is usually the relay's
`gt-<acct>-lb-hc` rule: the load balancer's health check cannot reach the relay port
(`tcp:30000-30999` from `130.211.0.0/22` and `35.191.0.0/16`), so the backend has no healthy
instance.

## IAM

Every role, and what each is needed for, is in [What GCP needs](gcp.md#expose).

`doctor` checks `roles/iap.tunnelResourceAccessor` for every tunnel, for instances and destination groups alike.
The rest surface as a permission error at the moment the command runs.

## Cloud DNS managed zones

`gtunnel expose` and `gtunnel relay up` require a Cloud DNS managed zone in the GCP
project to publish public hostnames. If the project has none configured:

```
Error: No Cloud DNS managed zones in project 'example-dev-1a2b'. Create one with `gcloud dns managed-zones create`, then run `gtunnel domains add`
```

The command to create one, and the role it needs, are in [What GCP needs](gcp.md#expose).
Once created, register it with gtunnel:

```
gtunnel domains add my-zone
```

## NumPy

Without NumPy, gcloud's IAP tunnel falls back to a pure-Python socket loop and runs several
times slower. gcloud bundles its own Python, so the system's NumPy does not count.

```
gcloud components reinstall
```

To see what `doctor` sees:

```
PY=$(gcloud info --format="value(basic.python_location)")
"$PY" -c "import numpy; print(numpy.__version__)"
```

A non-zero exit from that second line is exactly the condition `doctor` reports.

## Services

gtunnel manages these for you. These are the commands to inspect one by hand.

### macOS, launchd

```
launchctl list | grep gtunnel
launchctl print gui/$(id -u)/com.gtunnel.<account>.<name>
launchctl kickstart -k gui/$(id -u)/com.gtunnel.<account>.<name>
launchctl bootout gui/$(id -u)/com.gtunnel.<account>.<name>
cat "$HOME/.gtunnel/agents/com.gtunnel.<account>.<name>.plist"
```

A plist in `~/Library/LaunchAgents` means auto-start is on for that tunnel, nothing more; a
stopped tunnel is a label launchd does not have. Turn it off with `gtunnel autostart <name> --off`
or `A` ([why the file is the flag](architecture.md#service-lifecycle)).

### Linux, systemd user units

```
systemctl --user status gtunnel-<account>-<name>.service
systemctl --user restart gtunnel-<account>-<name>.service
journalctl --user -u gtunnel-<account>-<name>.service -f
cat ~/.config/systemd/user/gtunnel-<account>-<name>.service
```

A user unit stops when the last session of the user ends. For tunnels that must survive a
logout:

```
loginctl enable-linger $USER
```

A service that starts and dies at once is almost always PATH: gtunnel writes gcloud's
directory into the unit, and `gtunnel restart <name>` rewrites it after gcloud moves.

## Cost

Access mode creates nothing and costs nothing beyond egress.

| Item | Scope | Approximate cost |
|---|---|---|
| Global forwarding rule | Per account | USD 18/month |
| e2-micro relay instance | Per account | USD 7/month, free tier in some US regions |
| Managed certificate | Per domain | Free |
| Backend service, named port, DNS record | Per tunnel | Free, DNS queries billed per million |

So a relay is about USD 25/month per account, and a tunnel on top of it is free.
`gtunnel relay down` removes all of it; `gtunnel reset` does not.

## Leftovers

`gtunnel doctor --fix` removes, after asking:

| Finding | What it removes |
|---|---|
| Stale local record | A tunnel in `tunnels.toml` with no service installed |
| Stale reservation | An `expose` that claimed a name and relay port in `tunnels.toml` ten minutes ago and never finished, so the port stays held. A reservation is not a tunnel: `list`, `status` and the TUI do not show it |
| Dead ephemeral | A `run` record whose process is gone |
| Auto-start drift | Nothing: the service manager is brought to the `auto_start` in `tunnels.toml` |
| Missing watchdog | Nothing: the watchdog agent is installed |
| Unreadable run directory | Nothing: it is reported, and the checks that need the live runs are skipped. Fix the directory's permissions, then run `doctor` again |
| Relay out of date | Nothing: it is reported, and `gtunnel relay up` updates the relay's startup script and health check in place |
| Orphan service | A `com.gtunnel.*` plist (macOS) or `gtunnel-*.service` unit (Linux) that no record in `tunnels.toml` produces a label for, which would otherwise keep its port at login. Only a service that belongs to this config directory counts: the plist in `<config dir>/agents`, or a `LaunchAgents` plist or systemd unit whose log path is under `<config dir>/logs/`. A file whose log path cannot be read is left alone, and so is a loaded job that came from another config directory. The watchdog is never reported |
| Orphan backend | A backend service whose description is `gtunnel:<account>` and that no tunnel in `tunnels.toml` claims |
| Orphan host rule | A host rule on the relay's URL map for a hostname no tunnel claims |
| Orphan named port | A named port starting `gt-` on the relay's instance group that no tunnel claims |
| Orphan DNS record | An A record aimed at the relay that one of the above still ties to the tunnel |

Each claim is re-checked against `tunnels.toml` and the live runs right before removal, so a
tunnel created during the scan is never removed. Only backend services carry the
`gtunnel:<account>` tag, so a hand-made backend is left alone even when its name matches.
Host rules and named ports live on the relay's own URL map and instance group.

A DNS record set has no description, so aiming at the relay is not proof of ownership.
gtunnel removes one only while a tagged backend, host rule or named port ties it to the same
tunnel; any other record aimed at the relay is reported with its
`gcloud dns record-sets delete` command and left in place.

`<config>/tunnels.lock is held by another gtunnel process` (or `config.lock`) means the
command waited 5 seconds (`gcp.lock_deadline`) for another gtunnel to finish saving. The usual
holder is a `gtunnel` waiting on a prompt or in a stopped terminal: find it with
`pgrep -fl gtunnel` and end it. A leftover `.lock` file is harmless, because the process holds
the lock, not the file; delete it only when no gtunnel runs.

To confirm nothing is left in GCP after `relay down`:

```
gcloud compute forwarding-rules list --filter="name~^gt-"
gcloud compute backend-services list --filter="name~^gt-"
gcloud compute instances list --filter="name~^gt-"
```

### A row tagged `[not managed by gtunnel]`

| Symptom | Meaning |
|---|---|
| The row is listed from the first refresh | It is real: an IAP process forwarding a port no tunnel in `tunnels.toml` and no run record owns. Find it with `lsof -nP -iTCP:<port> -sTCP:LISTEN`, then stop it or take it over with `m` in the TUI |
| A tunnel you started a moment ago shows as a stranger briefly | Not expected: a process on a port one of your tunnels owns is never listed, whichever account started it. If you see this, report it with the `ps` line |
