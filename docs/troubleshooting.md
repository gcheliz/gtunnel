# Troubleshooting

[Home](../README.md) / [Usage](usage.md) / [Commands](commands.md) / [Configuration](configuration.md) / [Architecture](architecture.md) / Troubleshooting

Contents:

- [Installation](#installation)
- [What the log says, and what it means](#what-the-log-says-and-what-it-means)
- [Local port collisions](#local-port-collisions)
- [gtunnel needs a logged-in Google account](#gtunnel-needs-a-logged-in-google-account)
- [no zone for account](#no-zone-for-account)
- [Could not list projects](#could-not-list-projects)
- [IAP firewall](#iap-firewall)
- [IAM](#iam)
- [Cloud DNS managed zones](#cloud-dns-managed-zones)
- [NumPy](#numpy)
- [Services](#services)
- [Cost](#cost)
- [Leftovers](#leftovers)

Start with `gtunnel doctor`. It reports everything below that can be detected, and prints
the exact command for the things it cannot fix itself.

`doctor` runs after gtunnel is installed and working, so the installation problems come
first: nothing below the next section is reachable until `gtunnel --version` answers.

## Installation

### `cargo install gtunnel` installed something else

`gtunnel` on crates.io is an unrelated crate by another author, published in 2022. It is a
tcp-over-grpc proxy and has nothing to do with this project. Installing it is harmless but
useless, and it leaves two foreign binaries behind:

```
$ ls ~/.cargo/bin
gtunnel  gtunnel-client  gtunnel-server
```

`gtunnel-client` and `gtunnel-server` are the stranger's. Your own `gtunnel`, if you have
one, is untouched — that crate ships neither binary under that name, which is the only
reason a bare `cargo install` does not overwrite it.

If you also have a real gtunnel, cargo now has two packages under one name and refuses to
guess which you mean:

```
$ cargo uninstall gtunnel
error: specification `gtunnel` is ambiguous
help: re-run this command with one of the following specifications
  path+file:///Users/you/workspace/gtunnel#0.1.0
  registry+https://github.com/rust-lang/crates.io-index#gtunnel@0.1.0
```

That error is the useful part: it prints both specifications, and the one to remove is
always the `registry+` line. Pass it verbatim, quoted, because it contains a `#`:

```
$ cargo uninstall 'registry+https://github.com/rust-lang/crates.io-index#gtunnel@0.1.0'
    Removing /Users/you/.cargo/bin/gtunnel-client
    Removing /Users/you/.cargo/bin/gtunnel-server
```

With no real gtunnel installed there is nothing to disambiguate and a bare
`cargo uninstall gtunnel` works. Either way, install the right one afterwards:

```
cargo install --git https://github.com/gcheliz/gtunnel --force
```

`cargo install --list` shows what is left. The entry you want names a `git+` or `path+`
source, never `registry+https://github.com/rust-lang/crates.io-index`.

A v0.1.0 binary is the usual way here: its `gtunnel update` printed `cargo install gtunnel`
for a cargo-installed copy. Current versions replace a cargo-installed binary themselves, as
they do a download, and point only a Homebrew install at `brew upgrade gtunnel`.

### `gtunnel --version` reports an older version than the latest release

A binary reports the version its source tree carried when it was compiled, not the newest
one published. A `cargo install --path .` from a checkout that is behind `main` produces
exactly this, and nothing about it is broken.

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
| A tunnel installed before the restart budget keeps restarting | Its definition has no `--crash-marker` | Run any gtunnel command: it reinstalls the definition and prints `gtunnel: reinstalled ...` on stderr. If it prints `could not reinstall`, the reason follows; fix it and run a command again |
| `4004` | IAP asked gcloud to reauthenticate; gcloud resumes the session on its own | Nothing, unless it repeats: then check the account's credentials with `gtunnel doctor` |
| `Unable to reconnect within` | gcloud gave up reconnecting to IAP after 15 minutes and the clients on the tunnel were cut | Restart the tunnel; see [Limits](architecture.md#limits) |
| `Address already in use` | Another process holds the local port | `gtunnel doctor`, or set `--local-port` |
| `remote port forwarding failed` | The relay port is busy on the relay instance | `gtunnel restart <name>`, then `gtunnel doctor` |
| `Too many open files` | The tunnel ran out of file descriptors | `gtunnel doctor` names the process holding them, see below |
| `numpy` | gcloud's Python has no NumPy | See NumPy below |
| `timed out after` (in the TUI status line or a command's error) | A subprocess did not answer within its bound: 30s for `gcloud`, 10s for `launchctl` and `systemctl`. Usually `gcloud` waiting on a browser login or a dead network, or a wedged launchd | Run the named command by hand to see what it waits on, then `gcloud auth login` or `gtunnel doctor`. `delete` that times out stopping the job keeps the record, so retry it once the job answers |

### A start that says `failed to start`

`gtunnel start`, `restart` and `add -s` wait for the tunnel to come up. An Access tunnel,
`--via` included, has up to 20 seconds to report it is listening (`start_deadline` in `config.toml`
or `GTUNNEL_START_DEADLINE` moves the bound); an Expose tunnel is judged after a fixed 3 seconds.
`<name> failed to start` means the service exited with an error or was not listening by then,
so the command exits 1 and the TUI status line, or an MCP client's `isError` result, says the
same. The line under it is the hint for the newest log line gtunnel recognises, from the table
above, or `not listening after 20s`; `gtunnel logs <name>` shows the rest. An Expose tunnel
still connecting after 3 seconds reads as running, so `gtunnel list` is the check for one that
fails later.

### One level, two scales

How much a tunnel says is one setting, `info` by default. gcloud and ssh scale verbosity
differently and the same word means different things to each, so gtunnel maps the level
to both rather than making you set two:

| gtunnel level | gcloud `--verbosity` | ssh `-o LogLevel` | What you get |
|---|---|---|---|
| `quiet` | `warning` | `ERROR` | Failures only, plus gcloud's own `Listening on port` line |
| `info` (default) | `info` | `DEBUG` | The listener or forward coming up, and one line per client connecting |
| `debug` | `debug` | `DEBUG3` | The above plus gcloud's and ssh's own internals, for a tunnel that will not carry traffic |

`info` maps to ssh `DEBUG`, one step further than the name suggests, because ssh's levels
run `QUIET < FATAL < ERROR < INFO < VERBOSE < DEBUG` and both lines a person means by
"tell me it is working" are `debug()` calls; at `VERBOSE` ssh prints three lines for a
whole session and nothing per connection.

`quiet` maps to ssh `ERROR` rather than `QUIET`, because `QUIET` suppresses the failure
messages that are the only reason to open a quiet tunnel's log at all.

`quiet` is not silence. An Access tunnel still writes gcloud's `Listening on port [N].`
and `Testing if tunnel connection works.` at every start, because those are user output
rather than log records and `--verbosity` does not govern them. What `quiet` removes is
the per-connection line, which is a log record.

Set it with `--log-level`, `GTUNNEL_LOG_LEVEL` or `log_level` in `config.toml`, in that
order of precedence — see [configuration.md](configuration.md#the-log-level). A running tunnel keeps the
level it started under until its service is reinstalled, which `start`, `restart`,
`autostart` and an edit in the TUI all do.

### Who writes what

Three writers append to the same file. The service and its supervisor write for as long as
the tunnel runs; a gtunnel command or the TUI adds the few facts only it knows.

| Writer | Lines | When |
|---|---|---|
| The service's child | gcloud's own output at the configured `--verbosity`, and, for the kinds that run ssh (`--via` and Expose), ssh's at the matching `LogLevel` (see the table above) | Always, from start to exit |
| The tunnel's supervisor | `gtunnel: conn ...` lines, prefixed and timestamped | Always, from start to exit |
| gtunnel | every other `gtunnel: <message>` line, prefixed and timestamped | Only while the TUI or a gtunnel command is running |

The shapes below are what each kind of line looks like, not a captured transcript:

| Kind | Looks like | Written by |
|---|---|---|
| Access, listener up | `Listening on port [15432].` | gcloud |
| Jump (`--via`), forward up | `Local forwarding listening on 127.0.0.1 port 8443.` | ssh |
| Access, a client connects | `gtunnel: conn iap-4 open` (or `conn ssh-3 open from 127.0.0.1:54321` when the tool names the client at once) | gtunnel, from the tool's own line |
| Access, a client leaves | `gtunnel: conn iap-4 from 127.0.0.1:54321 closed after 12.4s` | gtunnel, from the tool's own line |
| Access, a burst | `gtunnel: conn ... 240 more in the last minute` | gtunnel |
| Expose, forward up | `debug1: Remote connections from 0.0.0.0:30001 forwarded to local address 127.0.0.1:3000` | ssh |
| Expose, a visitor arrives | `debug1: client_request_forwarded_tcpip: listen 0.0.0.0 port 30001, originator ...` | ssh |

The `conn` lines are written by the tunnel's own supervisor, which reads gcloud's
`[N] Connecting with URL` and `[N] Client closed connection from [...]` and ssh's
`channel N: new` and `channel N: free: ... connect from H port P` and pairs them by id.
They are there whether or not the TUI is open. A connection gtunnel itself makes to check
the tunnel's health writes no line, at most 60 lines a minute (`logs.lines_per_minute`) are written per tunnel, and
the `quiet` level writes none.

A `--via` tunnel never prints `Listening on port`. It runs `gcloud compute ssh
--tunnel-through-iap`, which is a different command from `start-iap-tunnel` rather than a
flag on it, and there is no listener inside gcloud to announce one. Its equivalent is
ssh's forwarding line.

Every ssh line in the table above is a debug one, which is why the default level
reaches ssh as `DEBUG`; see [One level, two scales](#one-level-two-scales).

### A connection is in the log with the port it came from

Each `conn` close line names the client's address and source port, so `lsof -nP -iTCP:54321`
while it is still open, or the time next to it afterwards, says which program it was.
The connection count in the TUI's Metrics pane is sampled and is not written to the log.

A tunnel gtunnel does not manage owns no log here, so it gets neither kind of line.

### How large a log gets

The default level makes a tunnel talkative: around 90 lines per ssh handshake, and a line
or two per client connection after that. The level is the lever on that figure.

| Level | Per ssh handshake | Per client connection |
|---|---|---|
| `quiet` | almost nothing; a failure only | nothing |
| `info` (default) | around 90 lines | a line or two |
| `debug` | substantially more, gcloud's and ssh's internals included | several |

The bound is the same at all three: a log is emptied when its tunnel starts, and past
8 MiB (`logs.max_size`) gtunnel keeps the last 2 MiB (`logs.keep_size`) and replaces the rest with a first line saying it did.
The rate of reaching it is not: at `debug` a busy tunnel covers 8 MiB in minutes rather
than weeks, and the paragraph below is why that matters for a tunnel left unattended.

That trim runs when a gtunnel process writes to the log, which is while the TUI is open
or a command is running. A tunnel nobody has looked at for a month is bounded the next
time somebody does, not before, so `du -sh` over the logs directory is still worth a
look; `gtunnel restart <name>` empties one outright.

### A tunnel that stops accepting connections

The worst shape a tunnel takes: the service is `Running`, the process is alive, and every
new client is refused. Nothing exits, so nothing restarts it, and `gtunnel list` says it is
up because by its own lights it is.

The usual cause is file descriptors. Each client of a forwarding tunnel costs one, and a
service that has run out writes `Too many open files` and then refuses everything while
staying up.

| | |
|---|---|
| The ceiling a tunnel gets now | 4096, written into the unit by gtunnel |
| What it was before | 256 on macOS, which a launchd job inherits and nothing chose |
| Where it comes from | `SoftResourceLimits`/`NumberOfFiles` in the plist, `LimitNOFILE` in the systemd unit |
| Applied when | The service is installed. A tunnel installed by an older gtunnel gets the new ceiling from the next gtunnel command, which rewrites its unit |

Four thousand descriptors is more than a tunnel needs, so hitting the new ceiling means
something is holding connections open rather than closing them. `gtunnel doctor` reads the
tunnel's log, and when it finds that line it names the process holding the connections:

```
$ gtunnel doctor
account dev:
  ! Tunnel 'k8s' ran out of file descriptors: my-client (pid 4471) is holding 3804 of its connections right now. Deal with that, then `gtunnel restart k8s`
```

The numbers above are an example; the shape is what the line looks like.

Both ends of a loopback connection are listed, which is why the two counts match: the
tunnel's own process holds one end of every one of them. gtunnel names the client, because
naming the listener would only ever point at the victim. The
check runs before the credentials check, because a wedged tunnel and an expired login tend
to arrive together and the one must not hide the other.

By hand, the same question is `lsof` on the tunnel's local port. Ask for the whole command
name — without `+c 0` lsof cuts it to nine characters, which is enough to mislead and not
enough to grep for:

```
$ lsof -nP +c 0 -iTCP:8443 -sTCP:ESTABLISHED | awk 'NR>1{print $1, $2}' | sort | uniq -c | sort -rn
3804 my-client 4471
3804 ssh 1545
```

A tunnel in this state is what the watchdog exists for: it restarts one that is running but
refusing connections, and stops one that will not come back. See `watch` in
[commands.md](commands.md#watch).

## Local port collisions

A tunnel needs an unused local port to bind to. gtunnel checks both against its own
configured tunnels and against listening sockets on the machine before writing the
tunnel record.

When another gtunnel tunnel already uses the port:

```
$ gtunnel add db postgres:5432 --local-port 5432
Error: Local port 5432 is already used by another gtunnel tunnel
```

When an outside process holds the port:

```
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

gtunnel could not get an access token for the account at startup and either the login did
not finish or it was not allowed to start.

| Cause | Fix |
|---|---|
| No terminal: stdin or stdout is redirected, as under cron, a pipe or CI | A browser login cannot finish there. Run `gcloud auth login --configuration=<configuration>` once in a terminal, or use a credential that needs no login |
| The login was cancelled or failed | Run it again; `gcloud auth list` shows who is logged in |
| The login exited 0 but the probe still fails | The configuration gtunnel uses is not the one that was logged in. Check `gcloud config configurations list` against the account's `gcloud_configuration` |
| The account runs on a CI runner with an attached service account | There is no login to run. Give the runner's service account `roles/iap.tunnelResourceAccessor`; see [Running in CI](usage.md#running-in-ci) |
| The account names a `credential_file` | gtunnel never offers a login for it. The error names the file and ends with gcloud's own reason (`; gcloud said: ERROR: ...`): check that it exists, is a service-account key or a workload identity federation file, and that its identity is not disabled. `gtunnel doctor` names the file and repeats the reason |
| The account impersonates a service account | `gcloud auth login` is run without `--impersonate-service-account`, because it rejects that flag. It logs in the user whose credentials mint the impersonation token, so that user needs `roles/iam.serviceAccountTokenCreator` on the service account |

Only the selected account (or `--account`) is checked at startup. Switching to another
account in the TUI does not check again, so an account that is logged out surfaces as the
error in that tunnel's log. `doctor`, `account`, `init`, `reset`, `demo` and `update` skip
the check.

## `credential_file must be an absolute path`

An account whose `credential_file` is relative or starts with `~` stops that account's tunnels
with exit 2, naming the account. Every other account, and every command that edits or lists
accounts, keeps working, so the fix is one command:

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

The log shows the sequence (placeholder names; the shape of the lines, not a captured transcript):

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

Exit code 2, before any request is made. `gtunnel init` lets the default zone be skipped, so an
account can exist without one, and the commands that reach an instance by zone refuse it:
`add --via`, `add` to an instance, `run` to an instance and `relay up`.

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

IAP connects from `35.235.240.0/20`, and that range must be allowed to the exact port you
are tunnelling to. `doctor` prints the rule it would create:

```
gcloud compute firewall-rules create gt-allow-iap-<port> --project=<project> \
  --network=<network> --direction=INGRESS --allow=tcp:<port> \
  --source-ranges=35.235.240.0/20
```

The relay instance's two rules are created by `relay up` and target the tag `gt-relay`:

| Rule | Source | Ports |
|---|---|---|
| `gt-<acct>-iap-ssh` | `35.235.240.0/20` | 22 |
| `gt-<acct>-lb-hc` | `130.211.0.0/22`, `35.191.0.0/16` | 30000-30999 |

An Expose tunnel that returns 502 with no error in the log is usually the second rule: the
load balancer's health check cannot reach the relay port, so the backend has no healthy
instance.

## IAM

| Role | On | Needed for |
|---|---|---|
| `roles/iap.tunnelResourceAccessor` | The instance, destination group or project | Every tunnel |
| `roles/compute.viewer` | The project | Listing instances and zones |
| `roles/compute.instanceAdmin.v1` | The project | `relay up` and `relay down` |
| `roles/compute.networkAdmin` | The project | Load balancer and firewall rules |
| `roles/dns.admin` | The managed zone | `domains add` and Expose tunnels |
| `roles/certificatemanager.owner` | The project | Managed certificates |
| `roles/iap.admin` | The project | `expose --private` |

`doctor` checks the first one for every tunnel, for instances and destination groups alike.
The rest surface as a permission error at the moment the command runs.

## Cloud DNS managed zones

`gtunnel expose` and `gtunnel relay up` require a Cloud DNS managed zone in the GCP
project to publish public hostnames. If the project has none configured:

```
Error: No Cloud DNS managed zones in project 'example-dev-1a2b'. Create one with `gcloud dns managed-zones create`, then run `gtunnel domains add`
```

Creating a managed zone requires `roles/dns.admin` on the GCP project:

```
gcloud dns managed-zones create my-zone --dns-name=example.com. --description="gtunnel zone"
```

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

A plist in `~/Library/LaunchAgents` means auto-start is on for that tunnel, nothing more:
launchd loads everything in that directory at login, and a `KeepAlive` job runs the moment
it is loaded. The definition `gtunnel start` and `gtunnel stop` load and unload lives in
gtunnel's own directory, so a stopped tunnel is a label launchd does not have.

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

A service that starts and dies immediately is almost always PATH: the unit must carry
gcloud's directory, which gtunnel writes into it. `gtunnel restart <name>` rewrites the unit
and is the fix after gcloud moves.

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
| Dead ephemeral | A `run` record whose process is gone |
| Auto-start drift | Nothing: the service manager is brought to the `auto_start` in `tunnels.toml` |
| Missing watchdog | Nothing: the watchdog agent is installed |
| Orphan service | A `com.gtunnel.*` plist (macOS) or `gtunnel-*.service` unit (Linux) that no record in `tunnels.toml` produces a label for. Only a service that belongs to this config directory counts: the plist in `<config dir>/agents`, or a `LaunchAgents` plist or systemd unit whose log path is under `<config dir>/logs/`. A file whose log path cannot be read is left alone, and so is a loaded job that came from another config directory. The watchdog is never reported |
| Orphan backend | A backend service whose description is `gtunnel:<account>` and that no tunnel in `tunnels.toml` claims |
| Orphan host rule | A host rule on the relay's URL map for a hostname no tunnel claims |
| Orphan named port | A named port starting `gt-` on the relay's instance group that no tunnel claims |
| Orphan DNS record | An A record aimed at the relay that one of the above still ties to the tunnel |

It re-checks the claim against `tunnels.toml` and the live runs immediately before each
removal, so a tunnel created while the scan was running is never torn out from under you.
Only backend services carry the `gtunnel:<account>` tag, so a hand-made backend is left
alone even when its name matches gtunnel's scheme. Host rules and named ports live on the
relay's own URL map and instance group, which gtunnel created.

A Cloud DNS record set carries no description, so pointing at the relay address is not proof
of ownership. gtunnel treats such a record as its own only while it still holds a tagged
backend, a host rule or a named port for the same tunnel. Any other record aimed at the relay
is reported with the `gcloud dns record-sets delete` command and left in place.

A command that fails with `<config>/tunnels.lock is held by another gtunnel process` (or
`config.lock`) waited 5 seconds (`gcp.lock_deadline`) for another gtunnel to finish saving. Find it with
`pgrep -fl gtunnel`: a `gtunnel` command left waiting on a prompt or a stopped terminal is the
usual holder, and ending it releases the lock. A `.lock` file that is left behind is harmless,
because the lock is held by the process and not by the file; delete it only when no gtunnel
runs.

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
| A tunnel you just started is shown as stranger for a moment | Not expected: a process on a port one of your tunnels owns is never listed, whichever account started it. If you see this, report it with the `ps` line |
