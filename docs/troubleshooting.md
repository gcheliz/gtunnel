# Troubleshooting

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

```
cargo uninstall gtunnel --root ~/.cargo
cargo install --git https://github.com/gcheliz/gtunnel --force
```

The first command removes whichever `gtunnel` entry cargo has registered, so run the
second afterwards whether or not you had a working copy. `cargo install --list` shows what
is left, and the entry you want names a `path+` or `git+` source, never
`registry+https://github.com/rust-lang/crates.io-index`.

v0.1.0 is how most people get here: its `gtunnel update` printed `cargo install gtunnel`
for a cargo-installed copy, sending its own users to the wrong crate. v0.1.1 and later
print the `--git` command above instead.

### `gtunnel --version` reports an older version than the latest release

A binary reports the version its source tree carried when it was compiled, not the newest
one published. A `cargo install --path .` from a checkout that is behind `main` produces
exactly this, and nothing about it is broken.

| Check | Command | What it means |
|---|---|---|
| Which binary is running | `which gtunnel` | `~/.cargo/bin` means cargo installed it, `/usr/local/bin` a download |
| Where cargo got it | `cargo install --list` | the source in parentheses is the truth |
| What is published | `gh release list --repo gcheliz/gtunnel --limit 1` | compare the tag against `--version` |

Fix it the way it was installed: `git pull && cargo install --path .` from a clone,
`cargo install --git https://github.com/gcheliz/gtunnel --force` from the repository, or
`gtunnel update` for a downloaded binary.

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
| `Reauthentication required` | gcloud credentials expired | `gcloud auth login` |
| `Address already in use` | Another process holds the local port | `gtunnel doctor`, or set `--local-port` |
| `remote port forwarding failed` | The relay port is busy on the relay VM | `gtunnel restart <name>`, then `gtunnel doctor` |
| `numpy` | gcloud's Python has no NumPy | See NumPy below |

### Who writes what

Two processes append to the same file. The service's child writes continuously, whether
or not anyone is watching; gtunnel writes the few facts only it knows, and only while a
gtunnel process is running.

| Writer | Lines | When |
|---|---|---|
| The service's child | gcloud's own output at `--verbosity=info`, and, for the kinds that run ssh (`--via` and public), ssh's at `LogLevel=DEBUG` | Always, from start to exit |
| gtunnel | `gtunnel: <message>`, always prefixed and timestamped | Only while the TUI or a gtunnel command is running |

The shapes below are what each kind of line looks like, not a captured transcript:

| Kind | Looks like | Written by |
|---|---|---|
| Access, listener up | `Listening on port [15432].` | gcloud |
| Access, a client connects | `INFO: [0] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect?project=...&port=5432&zone=...']` | gcloud |
| Access, a client leaves | `INFO: Client closed connection from ...`, then `INFO: Terminating connection to ...` | gcloud |
| Jump (`--via`), forward up | `debug1: Local forwarding listening on 127.0.0.1 port 8443.` | ssh |
| Jump, a client connects | `debug1: Connection to port 8443 forwarding to 10.0.0.10 port 443 requested.` | ssh |
| Public, forward up | `debug1: Remote connections from 0.0.0.0:30001 forwarded to local address 127.0.0.1:3000` | ssh |
| Public, a visitor arrives | `debug1: client_request_forwarded_tcpip: listen 0.0.0.0 port 30001, originator ...` | ssh |
| Access or jump, count changed | `gtunnel: connections: 0 -> 2` | gtunnel |
| Any kind, health changed | `gtunnel: unhealthy`, `gtunnel: healthy again` | gtunnel |

Only Access and jump tunnels get the `connections:` line. The count is the number of
established connections on the tunnel's local port, and a public tunnel has no local
port of its own: it dials out to the service you exposed, on a port other things may
also be using. There is nothing there that could be counted as the tunnel's, so gtunnel
counts nothing and writes nothing. The `debug1:` lines above are what a public tunnel
has instead.

A `--via` tunnel never prints `Listening on port`. It runs `gcloud compute ssh
--tunnel-through-iap`, which is a different command from `start-iap-tunnel` rather than a
flag on it, and there is no listener inside gcloud to announce one. Its equivalent is
ssh's forwarding line.

`DEBUG` rather than `VERBOSE` for ssh, because ssh's levels run `QUIET < FATAL < ERROR <
INFO < VERBOSE < DEBUG` and every line in the table above is a debug one. At `VERBOSE`
ssh prints three lines for a whole session and nothing at all per connection.

### A gap in the connection counts is not an idle tunnel

gtunnel counts the established connections on a tunnel's local port every 5 seconds and
writes a line only when that count changes. It can only do so while a gtunnel process is
running. A log showing `connections: 0 -> 2` at 14:03 and nothing afterwards means nobody
was watching after 14:03, not that the tunnel went idle. The lines the service's child
writes have no such gap.

A tunnel gtunnel does not manage owns no log here, so it gets neither kind of line.

### How large a log gets

`--verbosity=info` and `LogLevel=DEBUG` make a tunnel talkative: around 90 lines per ssh
handshake, and a line or two per client connection after that. A log is emptied when its
tunnel starts, and past 8 MiB gtunnel keeps the last 2 MiB and replaces the rest with a
first line saying it did.

That trim runs when a gtunnel process writes to the log, which is while the TUI is open
or a command is running. A tunnel nobody has looked at for a month is bounded the next
time somebody does, not before, so `du -sh` over the logs directory is still worth a
look; `gtunnel restart <name>` empties one outright.

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
| The account impersonates a service account | `gcloud auth login` is run without `--impersonate-service-account`, because it rejects that flag. It logs in the user whose credentials mint the impersonation token, so that user needs `roles/iam.serviceAccountTokenCreator` on the service account |

Only the selected account (or `--account`) is checked at startup. Switching to another
account in the TUI does not check again, so an account that is logged out surfaces as the
error in that tunnel's log. `doctor`, `account`, `init`, `reset`, `demo` and `update` skip
the check.

## IAP firewall

IAP connects from `35.235.240.0/20`, and that range must be allowed to the exact port you
are tunnelling to. `doctor` prints the rule it would create:

```
gcloud compute firewall-rules create gt-allow-iap-<port> --project=<project> \
  --network=<network> --direction=INGRESS --allow=tcp:<port> \
  --source-ranges=35.235.240.0/20
```

The relay VM's two rules are created by `relay up` and target the tag `gt-relay`:

| Rule | Source | Ports |
|---|---|---|
| `gt-<acct>-iap-ssh` | `35.235.240.0/20` | 22 |
| `gt-<acct>-lb-hc` | `130.211.0.0/22`, `35.191.0.0/16` | 30000-30999 |

A public tunnel that returns 502 with no error in the log is usually the second rule: the
load balancer's health check cannot reach the relay port, so the backend has no healthy
instance.

## IAM

| Role | On | Needed for |
|---|---|---|
| `roles/iap.tunnelResourceAccessor` | The instance, destination group or project | Every tunnel |
| `roles/compute.viewer` | The project | Listing instances and zones |
| `roles/compute.instanceAdmin.v1` | The project | `relay up` and `relay down` |
| `roles/compute.networkAdmin` | The project | Load balancer and firewall rules |
| `roles/dns.admin` | The managed zone | `domains add` and public tunnels |
| `roles/certificatemanager.owner` | The project | Managed certificates |
| `roles/iap.admin` | The project | `expose --private` |

`doctor` checks the first one for every tunnel, for instances and destination groups alike.
The rest surface as a permission error at the moment the command runs.

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
cat "$HOME/Library/Application Support/gtunnel/agents/com.gtunnel.<account>.<name>.plist"
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
| e2-micro relay VM | Per account | USD 7/month, free tier in some US regions |
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
| Orphan DNS record, backend, host rule, named port | Relay resources tagged `gtunnel:<account>` that no live tunnel claims |

It re-checks the claim against `tunnels.toml` and the live runs immediately before each
removal, so a tunnel created while the scan was running is never torn out from under you.
Anything created by hand is left alone, even when its name matches gtunnel's scheme, because
the decision is made on the `gtunnel:<account>` tag rather than on the name.

A Cloud DNS record set carries no description, so pointing at the relay address is not proof
of ownership. gtunnel treats such a record as its own only while it still holds a tagged
backend, a host rule or a named port for the same tunnel. Any other record aimed at the relay
is reported with the `gcloud dns record-sets delete` command and left in place.

To confirm nothing is left in GCP after `relay down`:

```
gcloud compute forwarding-rules list --filter="name~^gt-"
gcloud compute backend-services list --filter="name~^gt-"
gcloud compute instances list --filter="name~^gt-"
```
