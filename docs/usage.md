# Usage

Every command, with the output it prints. Outputs were captured from a real run unless the
section says the opposite.

`--account <name>` and `--log-level <level>` work on every command, and override the
default account and the configured log level respectively; see
[The log level](#the-log-level). Paths in
captured output are shown at their default location rather than the throwaway directory the
run used, and the instance names of tunnels gtunnel only observed are replaced with generic
ones.

## Getting set up

From a machine with nothing installed to a first working tunnel. Each step links to the
section below that shows its full output.

### 1. Install the Google Cloud SDK

gtunnel shells out to `gcloud` for every connection and stores no credential of its own, so
the SDK comes first: [cloud.google.com/sdk/docs/install](https://cloud.google.com/sdk/docs/install).

```
$ gcloud version
Google Cloud SDK 590.0.0
alpha 2026.10.16
bq 2.1.40
```

### 2. Authenticate and pick a project

```
gcloud init                 # creates a configuration: account, project, region
gcloud auth login           # the credential gtunnel will mint tokens from
gcloud config list          # confirm account and project
```

A gtunnel account wraps one gcloud configuration, so getting this right is what `gtunnel
init` reads in the next step. If you work across environments, make one gcloud
configuration per environment now — `gcloud config configurations create <name>`.

### 3. Grant yourself IAP access

Tunnelling needs `roles/iap.tunnelResourceAccessor` on the instance, the destination group,
or the project:
[cloud.google.com/iap/docs/using-tcp-forwarding](https://cloud.google.com/iap/docs/using-tcp-forwarding#grant-permission).
Without it a tunnel starts and then fails with `4033`; the
[error table](troubleshooting.md) lists what each code means.

IAP also connects from a fixed range, so the target port needs a firewall rule allowing
`35.235.240.0/20`. `gtunnel doctor` prints the exact `gcloud compute firewall-rules create`
for the port you are tunnelling to rather than making you compose it.

### 4. Install gtunnel

Four routes, in rough order of how most people arrive:

| Route | Command |
|---|---|
| A published binary | the block below |
| From the repository | `cargo install --git https://github.com/gcheliz/gtunnel` |
| From a clone | `cargo install --path .` |
| Already installed | [`gtunnel update`](#update) |

There is no `cargo install gtunnel`. The name on crates.io belongs to an unrelated crate
by another author, so a bare name would install somebody else's binary. If you have
already run it, [troubleshooting.md](troubleshooting.md#cargo-install-gtunnel-installed-something-else)
says what it left behind and how to undo it.

Specify the release version and substitute your machine's file: `gtunnel-macos-arm64`,
`gtunnel-macos-intel`, `gtunnel-linux-amd64` or `gtunnel-linux-arm64`.

```
VERSION="v0.1.27"
F="gtunnel-macos-arm64-${VERSION}.tar.gz"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F.sha256"
shasum -a 256 -c "$F.sha256" && tar -xzf "$F" && sudo mv gtunnel /usr/local/bin/
```

On Linux the checksum tool is `sha256sum -c`. Both Linux builds are static musl binaries,
so they run on any glibc.

```
$ gtunnel --version
gtunnel 0.1.27
```

#### Proving where the binary came from

The `.sha256` tells you the download arrived intact. It does not tell you who built it:
anyone able to replace the tarball could replace the checksum beside it. Every release
asset also carries a build attestation, signed by GitHub with a short-lived certificate
tied to this repository's Release workflow and recorded in a public transparency log.

```
gh attestation verify "gtunnel-macos-arm64-${VERSION}.tar.gz" -o gcheliz
```

It says nothing and exits 0 when the file checks out, and fails loudly when it does not —
an unattested file gets `HTTP 404: Not Found` and exit 1, whatever its checksum says.

To see what the attestation actually claims, ask for the certificate:

```
$ gh attestation verify "gtunnel-macos-arm64-${VERSION}.tar.gz" -o gcheliz --format json \
  | python3 -c 'import json,sys; c=json.load(sys.stdin)[0]["verificationResult"]["signature"]["certificate"]; print(c["buildSignerURI"]); print(c["sourceRepositoryDigest"]); print(c["runnerEnvironment"])'
https://github.com/gcheliz/gtunnel/.github/workflows/release.yml@refs/tags/v0.1.1
78b5da3d7925cacd78832dc60b5ee160d98f6428
github-hosted
```

The workflow, the tag and the commit that produced the binary, on a GitHub-hosted runner.
Attestations begin with v0.1.1; v0.1.0 predates them.

#### Building from source

The two cargo routes compile gtunnel on your machine, so they need Rust 1.89 or newer and
a C compiler: the TLS stack builds C code. A published binary needs neither.

| Machine | Rust | C compiler |
|---|---|---|
| macOS | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` | `xcode-select --install` |
| Debian, Ubuntu | the same rustup line | `sudo apt install build-essential` |
| Fedora, RHEL | the same rustup line | `sudo dnf install gcc` |

rustup adds `~/.cargo/bin` to your shell's profile, which a shell that is already open has
not read. Load it, then check both tools:

```
$ . "$HOME/.cargo/env"
$ cargo --version
cargo 1.88.0 (873a06493 2025-05-10)
$ cc --version | head -1
Apple clang version 21.0.0 (clang-2100.3.34.2)
```

Any `cargo` of 1.89 or later passes, and Linux prints its own compiler, such as
`cc (Debian 12.2.0-14+deb12u1) 12.2.0`. Distribution packages of Rust often lag behind
1.89, which is why rustup is the one route listed; `rustup update` moves to a newer
toolchain later. No other build tool is needed. Without a C compiler the build stops in its
first seconds:

```
error: linker `cc` not found
  |
  = note: No such file or directory (os error 2)
```

Install the compiler from the table and run the same `cargo install` again. If `gtunnel`
is still not found once it is installed, see
[troubleshooting.md](troubleshooting.md#gtunnel-command-not-found-right-after-installing).

### 5. Create the account

Order matters: every other command needs an account, and says so until one exists.

```
$ gtunnel doctor
Error: gtunnel is not initialized. Run `gtunnel init`.
```

```
gtunnel init
```

[init](#init) shows the prompts. A default zone is required — `gtunnel zones` lists the ones
in the project.

### 6. Check the ground before the first tunnel

```
gtunnel doctor
```

[doctor](#doctor) reports what it can see and prints the exact command for anything it
cannot fix itself: missing credentials, a gcloud Python without numpy, a missing firewall
rule. Clear its findings before adding a tunnel, so a failure later is about the tunnel and
not about the setup.

### 7. Add a tunnel

```
gtunnel add db my-vm:5432 -s
psql -h localhost -p 15432
```

[add](#add) covers the three shapes a target can take: a port of an instance, a host behind
a jump instance with `--via`, and an IAP destination group. `-s` installs the user service
and starts it, so the tunnel survives closing the terminal.

Publishing a local service instead needs a Cloud DNS managed zone and a one-off relay per
account: [domains](#zones-and-domains), [relay](#relay), then [expose](#expose).

## Command map

| Command | Purpose | Touches GCP |
|---|---|---|
| `init` | Import a gcloud configuration as a gtunnel account | read |
| `add` | Add a persistent Access tunnel | read |
| `expose` | Publish a local service at `https://<name>.<domain>` | write |
| `run` | Ephemeral tunnel in the foreground, gone on Ctrl+C | read, or write with `--public` |
| `start` / `stop` / `restart` | Service control | write, for a public tunnel only |
| `autostart` | Turn start-at-login on or off for a tunnel | no |
| `logs` | Read a tunnel's log | no |
| `list` | Every tunnel, managed or not | no |
| `delete` | Remove a tunnel and its route | write for public tunnels |
| `rename` | Rename a tunnel, its service and its log | no |
| `move` | Hand an Access tunnel to another account | read, to verify the destination |
| `zones` / `domains` | Defaults for new tunnels | read |
| `relay up` / `down` / `status` | The shared relay for an account | write |
| `account` | Accounts: `list`, `select`, `add`, `edit`, `remove` | read, for `add` |
| `watch` | Restart tunnels that are running but refusing connections | no |
| `doctor` | Report, and optionally remove, leftovers | read, write with `--fix` |
| `reset` | Remove all local configuration | no |
| `demo` | TUI with synthetic data | no |
| `update` | Check for and install a new version | no |

## Startup

Before it opens the TUI or runs a command that reaches GCP, gtunnel asks gcloud for an
access token for the selected account (or `--account`), the same probe `doctor` uses. A
token that cannot be minted, including an expired refresh token, counts as logged out.

| Checks the login | Skips the check |
|---|---|
| `gtunnel` (the TUI), `add -s`, `expose`, `run`, `start`, `stop`, `restart`, `list`, `move`, `zones`, `domains`, `relay` | `init`, `account`, `add` (without `-s`), `delete`, `rename`, `autostart`, `logs`, `zones default`, `domains default`, `domains add`, `doctor`, `watch`, `reset`, `demo`, `update` |

`doctor` is skipped because it reports broken credentials as a finding and a login prompt
would hide it; `init` has no account yet. Background services run `gcloud` directly and
never go through this check.

On a terminal, a failed probe prompts for confirmation (`[y/N]`, defaulting to `No`)
before running `gcloud auth login --configuration=<configuration>` with the terminal handed
to it, then probes again. Only the second probe counts: a login that exits 0 and still cannot
mint a token fails like any other. The sample below is illustrative, since the login output
belongs to gcloud.

```
$ gtunnel list
gcloud has no usable credentials for account 'prod'.
gtunnel requires an authenticated GCP account to establish tunnels.
Do you want to log in now? [y/N] [n]: y
Starting `gcloud auth login`.
Your browser has been opened to visit: ...
You are now logged in as [you@example.com].
NAME  STATUS  ...
```

When the confirmation is declined, the login is cancelled or fails, or there is no
terminal (stdin or stdout is redirected, as under cron or CI), gtunnel exits 1 without
starting anything or opening a browser.

```
$ gtunnel list < /dev/null
Error: gtunnel needs a logged-in Google account for 'prod'. Run `gcloud auth login`
```

## The TUI

`gtunnel` with no subcommand opens the TUI. `gtunnel demo` opens it filled with synthetic
data and never touches GCP or a config file.

`?` opens the full keymap, in the same four groups:

| Group | Keys |
|---|---|
| Tunnel | `s` start, `S` stop, `R` restart, `A` toggle auto-start, `h` health check now |
| The list | `a` add, `e` edit, `d` delete, `m` import an ephemeral run as managed, `j` `k` or the arrows move the selection |
| This tunnel's URL and log | `c` copy the URL, `l` copy the log to clipboard, `o` open it, `L` clear the log, `r` refresh the list, metrics and health |
| The program | `;` cycle accounts (the relay and its domains change with the account), `:` the accounts page, `?` the keymap, `C` the config page, `q` quit, `Esc` close a page or cancel a prompt, `PgUp` `PgDn` and `g` `G` scroll the keymap page, `Ctrl+Z` suspend, `Ctrl+C` quit from anywhere |

The keymap page also carries a **SESSION PATHS** summary section showing the active loaded
paths for `Config` (`~/.gtunnel/config.toml`), `Tunnels` (`~/.gtunnel/tunnels.toml`), and
`Logs` (`~/.gtunnel/logs/`).

`:` is the shift of `;`, so the key that cycles accounts and the key that opens all of them
are one key twice. The page lists every account, and adds, edits, selects and removes them;
its removal obeys the same refusal the CLI does, so an account holding tunnels stays.

`e` opens a chooser rather than a prompt, because there are now three things to edit:

| Key | What it edits |
|---|---|
| `1` | Target and ports — the prompt `e` used to open directly |
| `2` | Rename, within the same account |
| `3` | Move to another account |

`2` and `3` are the TUI's way to the `rename` and `move` commands, and they refuse what those
refuse: an unmanaged row, and an ephemeral run that has not been imported with `m`. A move
onto a name the destination already holds does not cancel — it asks for a new name with the
reason on screen, which is the `--rename` flag arriving when it is needed rather than being
remembered in advance.

Case matters in three pairs: `s`/`S` start and stop, `a`/`A` add and auto-start, `r`/`R`
refresh and restart. The footer shows only what the selected tunnel can do, so it never has
both halves of a pair on screen; the keymap page states the difference. When the selected
tunnel has failed, `R restart` leads the footer in the warning colour. An account with no
tunnels shows `no tunnels in this account` under the list header, and the footer offers `a`
and `m`; until the first load has answered, and after `;` until the new account's has, the
row says `loading` instead. A first load that fails puts its error on the status line and
replaces `loading` with `could not load tunnels`, until a load succeeds. The
keymap page is two columns from 92 cells inside its frame and one below that. On a
terminal too short for it, the page scrolls: `j` `k` and the arrows move a row, `PgUp` and
`PgDn` a screen, `g` and `G` jump to the top and the end, and a row such as
`3-14 of 31 rows` says where the window is. Both columns scroll together; the count line
and the `Esc back  q back` line stay in place; closing the page resets it to the top. A page
that fits shows no counter.

The header's right edge is the account field: the account name over its GCP project id, on
the same two rows as the logo. `;` switches both at once. When the terminal narrows, the
subtitle beside the logo goes first, then the project id is cut with a `…` (never below four
cells, and dropped whole if there is no room for that), and the account name is the last to
go. An account with no project shows the name alone.

When a newer release is cached, the header's closing rule carries it at its right end:

```
 ───────────────────────────────────────────────────────────────────────────────────────── ↑ v9.9.9 · gtunnel update ──
```

| Question | Answer |
|---|---|
| Where | the rule under the header; the rule above the footer when the header is hidden (under 16 rows) or is the one-line form |
| What it says | `↑ v<latest> · <command>`: `brew upgrade gtunnel` for a Homebrew install, `gtunnel update` otherwise |
| Where it comes from | the daily background `update --check` (see [update](#update)); the TUI starts one when the cache is missing or a day old, and re-reads the cache every minute |
| Waits on the network | never; it shows what an earlier check already learned, so the first run after a release shows nothing until a check finishes |
| When it narrows | the command goes first, leaving `↑ v<latest>`, then the whole notice; a step shows only when at least 8 dashes stay before it, and a string is never cut |
| `gtunnel demo` | shows no notice and starts no check |

Details repeats the project under the `target` row it qualifies. The header carries it
once as a property of the account, at the far edge of the screen from the host it belongs
to; the pane open while a `gcloud` command of your own is being typed is the one that has
to answer which project that host is in. An account with no project prints no row, the
same silence the header keeps.

Unmanaged `gcloud start-iap-tunnel` processes started outside gtunnel appear in the list
tagged `[not managed by gtunnel]`. Every action on them is refused except `m`, which takes
them over.

The list is a table with a `NAME  MODE  URL` header. If a legacy tunnel carries an account
suffix (such as `k8s-dev` under `dev`), the list hides the redundant suffix so columns stay
clean. The name column is as wide as the longest name; it shrinks, to no fewer than 8 cells
and with a `…`, only when the pane could not otherwise hold a 12-cell URL. The Details pane
shows the stored name. Tunnels should be named for what they reach (e.g. `k8s`) without
account suffixes.

The layout follows the terminal. From 121 columns the list is the left 46% and Details, Logs
and Metrics are stacked on the right; below 121 the list takes the full width above them,
which keeps the URL readable. The list scrolls only when the selected tunnel would leave the
window, so moving inside it moves the highlight and not the rows, and a
Details value too long for its row (a URL, a target, how the tunnel was started) wraps under
its label instead of being cut. When the terminal is short, Logs keeps at least 4 rows and
Metrics shrinks, then disappears, before Logs does.

Metrics redraws every 5 seconds, and its `active` is the most client connections the local
port carried since the last redraw, not the number open at the instant it drew. The count is
sampled twice a second: a `kubectl` call holds its socket for a few hundred milliseconds, so
a figure read once every 5 seconds landed between requests and reported 0 under real
traffic. The tunnel's log records the same number, and only when it changes.

### The config page

`C` opens a page that shows the two files gtunnel reads, with the absolute path of the one on
screen in its title. `Tab` switches between `config.toml` and `tunnels.toml`; `j` `k`, the
arrows, `PgUp` `PgDn`, `g` and `G` scroll as they do on the keymap page, and the count in the
bottom row (`1-16 of 29 lines`) says where the window is. A file that does not exist yet says
so: `tunnels.toml` is absent until the first `add`. `Esc` or `q` closes the page. The demo has
no config files, so `C` there says so and opens nothing.

`e` edits the file on screen in `$VISUAL`, then `$EDITOR`, then `vi`. The value is split on
whitespace, so `code -w` works, and a quoted path with a space stays one program. gtunnel
leaves the screen while the editor runs and takes it back when the editor exits, whatever
way it exits.

The editor never opens the real file. It opens a private copy next to it
(`tunnels.edit-<pid>.toml`, owner-only), and the real file is replaced in one rename, only
after the text has passed the rules for that file:

| File | What has to be true |
|---|---|
| `config.toml` | parses, has at least one account, and `selected_account` names one of them |
| `tunnels.toml` | parses, every name is valid as for `gtunnel add`, no name repeats within an account, and every tunnel's account is in `config.toml` |

A file that passes is committed (`config.toml` ends at mode 0600 whatever the editor did),
the copy is removed, and the list is reloaded without restarting the TUI: accounts, projects,
the relay and the tunnels. The account on screen is kept if it still exists; if it was
removed, the first remaining account takes over. The page says what happened, in green:
`reloaded: 1 changed, 1 added, 0 removed`, or `no changes to tunnels.toml` when the text came
back as it went in. A removed or renamed tunnel adds a warning, because a launchd job is named
for the tunnel and editing the file does not unload it.

A file that fails is not saved. The page shows what the editor left, the reason in red with its
line (`not saved: line 16: key with no value, expected `=``), and the keys become
`e edit again` and `Esc discard`: `e` reopens the same copy, so the work is not lost, and `Esc`
removes it. `Tab` does nothing while a refused edit waits. An editor that exits non-zero, as
`:cq` does in vim, cancels the edit and leaves the file as it was.

Running tunnels keep their old settings until `R` restarts them: a launchd job is rewritten
only on a restart, so the page and the status line both say so.

A transcript from a throwaway config, with `EDITOR` set to a script that appends one valid
tunnel to the file it is given (`GTUNNEL_CONFIG_DIR=/tmp/cfgdemo`, 80x24):

```
┌ /tmp/cfgdemo/tunnels.toml ───────────────────────────────────────────────┐
│ [[tunnels]]                                                              │
│ name = "db"                                                              │
│ account_name = "dev"                                                     │
│ auto_start = false                                                       │
│                                                                          │
│ [tunnels.spec]                                                           │
│ mode = "access"                                                          │
│ remote_port = 5432                                                       │
│ local_port = 15432                                                       │
│                                                                          │
│ [tunnels.spec.target]                                                    │
│ kind = "instance"                                                        │
│ instance = "bastion"                                                     │
│ zone = "europe-west1-b"                                                  │
│                                                                          │
│ [[tunnels]]                                                              │
│                                                                          │
│ reloaded: 0 changed, 1 added, 0 removed; running tunnels keep their old  │
│ settings until R                                                         │
│ 1-16 of 29 lines  j/k scroll                 e edit  Tab files  Esc back │
└──────────────────────────────────────────────────────────────────────────┘
```

The same page after an editor that wrote text which is not TOML:

```
┌ /tmp/cfgdemo/tunnels.toml ───────────────────────────────────────────────┐
│ [[tunnels]]                                                              │
│ name = "db"                                                              │
│ account_name = "dev"                                                     │
│ auto_start = false                                                       │
│                                                                          │
│ [tunnels.spec]                                                           │
│ mode = "access"                                                          │
│ remote_port = 5432                                                       │
│ local_port = 15432                                                       │
│                                                                          │
│ [tunnels.spec.target]                                                    │
│ kind = "instance"                                                        │
│ instance = "bastion"                                                     │
│ zone = "europe-west1-b"                                                  │
│                                                                          │
│ this is = = not toml                                                     │
│                                                                          │
│ not saved: line 16: key with no value, expected `=`                      │
│ 16 lines                                       e edit again  Esc discard │
└──────────────────────────────────────────────────────────────────────────┘
```

## init

Reads a gcloud configuration and writes a gtunnel account. It prompts for everything it
cannot infer.

```
$ gtunnel init
gcloud configurations:
  1. default (active)  project=example-dev-1a2b  account=you@example.com
Configuration number [1]: Account name [default]: Project [example-dev-1a2b]:
Service account to impersonate (optional) []: Default zone: Error: A default zone is required
```

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
| `--local-port` | Fixed local port; otherwise one is allocated from 15000-15999, stable per name |
| `--via` | Reach a host the instance can reach, rather than a port of the instance |
| `--dest-group` | Target an IAP destination group instead of an instance |
| `-s`, `--start` | Install the service and start it immediately |

The local port is derived from the tunnel name, so the same name gets the same port on
every machine unless it is taken.

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

### Three ways to reach a target

| Target | Command gtunnel runs | Use it for |
|---|---|---|
| A port of the instance | `start-iap-tunnel INSTANCE PORT --local-host-port=…` | SSH, a database on the VM itself |
| A host the instance reaches (`--via`) | `compute ssh INSTANCE --tunnel-through-iap -- -N -L LOCAL:HOST:PORT` | A private GKE control plane, anything behind a bastion |
| A host in a destination group | `start-iap-tunnel HOST PORT --dest-group=… --region=…` | A host IAP can address directly, where a group exists |

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
```

## start, stop, restart, autostart, logs, list, delete

```
$ gtunnel list
STATUS     NAME             MODE    URL                              TARGET
○ stopped  db               access  localhost:15432                  postgres-1:5432 (europe-west1-b)
● running  pid 59670        access  -                                bastion-1:22 (europe-west1-b)  [not managed by gtunnel]
```

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
    "unmanaged": false
  }
]
```

```
$ gtunnel logs nope
Error: Tunnel 'nope' not found for account 'dev'
```

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

A start is judged three seconds after it is asked for. A tunnel whose service has failed by
then is a failed start: the command prints the same lines it always did on stdout, exits 1,
and the record stays. The hint line appears when the log holds a line gtunnel recognises, and
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
  e2-micro VM + Global HTTPS Load Balancer (~$25/mo per account)
Create it? [y/N] [n]: y
```

Declining cancels cleanly:

```
$ gtunnel relay up
Relay for account 'dev' in europe-west1-b:
  e2-micro VM + Global HTTPS Load Balancer (~$25/mo per account)
Create it? [y/N] [n]: Cancelled
```

`down` confirms before deleting the relay:

```
$ gtunnel relay down
Delete the relay for account 'dev' and everything it owns? [y/N] [n]: Cancelled
```

### The relay's firewall is yours to close

A public tunnel forwards with `ssh -R 0.0.0.0:<relay-port>:<local target>`, so the port
listens on every interface of the relay VM. It has to: the load balancer reaches it from
another host, and a loopback bind would leave the URL unreachable. gtunnel allocates the
port; it never opens or closes a firewall for it.

| | |
|---|---|
| Port range | `30000-30999` |
| Should reach it | the load balancer, and Google's health checkers at `35.191.0.0/16` and `130.211.0.0/22` |
| Must not reach it | anything else, and above all the internet |

Leave that range open and a client who knows the relay's address and a port connects to
the forwarded service directly. That skips the load balancer, and with `--private` it
skips the IAP sign-in in front of it too, which is the whole protection that flag buys.
Restrict the range on the relay's network tag before the first `gtunnel expose`.

## account

```
$ gtunnel account list
* dev              project=demo-project             zone=europe-west1-b
```

`account select <name>` sets the default. The star marks it.

```
gtunnel account add <gcloud-configuration> [<name>]
gtunnel account edit <name> [--project P] [--zone Z]
gtunnel account remove <name>
```

`add` takes an existing gcloud configuration and reads the project and default zone from it,
so an account is never a second place to type what gcloud already knows; the name defaults to
the configuration's. `edit` changes only what it is given:

```
$ gtunnel account edit dev --zone europe-west4-a
Account 'dev' updated
```

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
| `gtunnel watch status` | Whether the agent is installed, and where its log is |

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

## doctor

Reports what gtunnel can see without changing anything.

```
$ gtunnel doctor
account dev:
  - gcloud has no usable credentials for account 'dev'. Run `gcloud auth login`
```

```
$ gtunnel doctor          # with a gcloud whose Python has no numpy
gtunnel:
  - gcloud's Python is missing numpy, which makes IAP tunnels slow. Run `gcloud components reinstall`
account dev:
  - gcloud has no usable credentials for account 'dev'. Run `gcloud auth login`
```

| Flag | Effect |
|---|---|
| `--fix` | Fix what can be fixed safely |
| `-y`, `--yes` | Skip the confirmation that `--fix` asks for |
| `-a`, `--all` | Check every account, not only the selected one |

`--fix` only ever removes stale local records, dead ephemeral records, and relay resources
tagged `gtunnel:<account>` that no live tunnel claims, and reconciles auto-start to what
`tunnels.toml` says. It re-checks that claim immediately
before each removal, so a tunnel started during the scan is left alone. Findings it cannot
fix, such as a missing firewall rule, are printed with the exact command that fixes them.

```
$ gtunnel doctor --fix
Remove 2 leftover(s)? [y/N] [n]: Cancelled
```

```
$ gtunnel doctor --fix
Remove 2 leftover(s)? [y/N] [n]: y
  fixed: Stale local record for 'old-tunnel'
```

When no fixable leftovers remain:

```
$ gtunnel doctor --fix
Nothing here is gtunnel's to remove
```

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
gcheliz/gtunnel has no published releases yet, so there is nothing to update to
```

Every other command prints a one-line hint when a cached check found a newer version:

```
$ gtunnel list
gtunnel v9.9.9 is available (current: v0.1.27). Run `gtunnel update`.
```

The check runs at most once a day, in the background, and never blocks a command. An install
from Homebrew is detected and `update` prints `brew upgrade gtunnel` instead of replacing the
binary. Standalone downloads and `~/.cargo/bin` binaries update themselves directly.

## Model Context Protocol (MCP) and AI Agent Integration

gtunnel provides first-class support for AI agents through both an in-repo Skill and a native
Model Context Protocol (MCP) server.

### The in-repo Skill

Located at `.agents/skills/gtunnel/SKILL.md` using the open Agent Skills standard. It automatically
guides AI agents (Claude Code, Gemini CLI, etc.) on:

- Enforcing tunnel naming conventions (`<platform>` or `<platform>-<service>`, never account/env suffixes).
- Routing client-side protocols through bastions (e.g. MongoDB Compass over SSH on port 2222/2223).
- Scoping commands to the correct GCP account.
- Performing mandatory pre-completion verification: checking daemon status, inspecting logs for IAP error codes (such as `4033`), and probing local listening ports.

### The native MCP Server

`gtunnel mcp` runs a newline-delimited JSON-RPC 2.0 stdio server in pure Rust. It exposes 10 tools
and 2 live resources without requiring an external runtime.

#### Exposed Tools

| Tool | Parameters | What it does |
|---|---|---|
| `gtunnel_list_tunnels` | `account?` | Lists configured tunnels and their daemon statuses |
| `gtunnel_start_tunnel` | `name`, `account?` | Starts a stopped tunnel daemon |
| `gtunnel_stop_tunnel` | `name`, `account?` | Stops a running tunnel daemon |
| `gtunnel_restart_tunnel` | `name`, `account?` | Reinstalls service spec and restarts daemon |
| `gtunnel_get_status` | `name`, `account?` | Returns PID, restarts, and active TCP probe round-trip latency |
| `gtunnel_get_logs` | `name`, `lines?`, `account?` | Tails daemon log with automatic error diagnosis |
| `gtunnel_add_tunnel` | `name`, `target`, `via?`, `zone?`, `local_port?`, `dest_group?`, `region?`, `network?`, `start?`, `account?` | Adds an Access tunnel |
| `gtunnel_delete_tunnel` | `name`, `account?` | Deletes a persistent tunnel and removes its service |
| `gtunnel_list_accounts` | none | Lists accounts and shows active selection |
| `gtunnel_doctor` | `account?`, `all?` | Runs diagnostic checks on GCP resources and local config |

A tool that fails returns a normal result with `isError: true` and the whole cause as its
text, such as `db failed to start` and the hint under it. A start that fails is such a failure:
`gtunnel_add_tunnel` with `start: true` reports it and the tunnel stays in `tunnels.toml`, as
`gtunnel add -s` leaves it. Only a call with missing or mistyped parameters is a JSON-RPC
error. Nothing but JSON-RPC frames is ever written to stdout.

#### Live Resources

| URI | What it returns |
|---|---|
| `gtunnel://tunnels` | JSON array of all persistent tunnels |
| `gtunnel://accounts` | JSON snapshot of configured accounts and active selection |

### Tool Validation and Testing

To verify tool definitions and JSON schemas non-interactively:

```
$ gtunnel mcp --validate
Validated 10 tools and 2 resources successfully.
```

To inspect tools, schemas, and test calls interactively using the official MCP Inspector:

```
npx @modelcontextprotocol/inspector gtunnel mcp
```

### Client Configuration

#### Workspace Configuration (`.mcp.json`)

To enable the `gtunnel` MCP server in your workspace, create a `.mcp.json` file in the project
root:

```json
{
  "mcpServers": {
    "gtunnel": {
      "command": "gtunnel",
      "args": ["mcp"]
    }
  }
}
```

This configuration schema is supported by both **Claude Code** and **Gemini CLI**:

- **Claude Code**: Automatically discovers `.mcp.json` at the root of the workspace directory. When you run Claude Code in this repository, the `gtunnel_*` tools are registered immediately.
- **Gemini CLI**: Place the same `"mcpServers"` block in `.gemini/settings.json` (workspace level) or `~/.gemini/settings.json` (global level).
- **Claude Desktop**: Add the `gtunnel` block under `"mcpServers"` in `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows).

Optional environment variables (such as custom configuration directories or log levels) can be
passed using the `"env"` object:

```json
{
  "mcpServers": {
    "gtunnel": {
      "command": "gtunnel",
      "args": ["mcp"],
      "env": {
        "GTUNNEL_LOG_LEVEL": "info"
      }
    }
  }
}
```

### Machine-readable JSON output

CLI commands support `--json` for automated workflows and scripting:

```
$ gtunnel account list --json
[
  {
    "name": "demo",
    "gcloud_configuration": "work",
    "project": "example-dev-1a2b",
    "default_zone": "europe-west1-b",
    "selected": true
  }
]
```

```
$ gtunnel doctor --json
[]
```

## Reference

The command sections above are task-ordered; this one is exhaustive. It describes the two
files gtunnel writes, the environment it reads, and what it exits with.

### config.toml

Written by `init`, `account` and `relay up`, and editable in the TUI's config page. Lives
in the configuration directory (see [Environment](#environment) below).

| Key | Type | Required | What it is |
|---|---|---|---|
| `selected_account` | string | yes | The account used when no `--account` is given |
| `log_level` | string | no | `quiet`, `info` or `debug`; `info` when absent. See [The log level](#the-log-level) |
| `accounts` | array of tables | yes | One entry per account; may be empty |

Each entry in `accounts`:

| Key | Type | Required | What it is |
|---|---|---|---|
| `name` | string | yes | What you type after `--account`, and the first half of the service label |
| `gcloud_configuration` | string | yes | The gcloud configuration whose credentials and project gtunnel uses |
| `project` | string | yes | The GCP project every API call targets |
| `default_zone` | string | yes | The zone assumed when `add` is given an instance and no `--zone` |
| `impersonate_service_account` | string | no | A service account to impersonate; omitted entirely when unset. The logged-in user needs `roles/iam.serviceAccountTokenCreator` on it |
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
managed zone that serves it). `domains add` writes these; `domains list` reads them.

### tunnels.toml

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

### The log level

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
`restart`, `autostart on|off`, `doctor --fix` on an auto-start finding, and editing the
tunnel or toggling its auto-start in the TUI. Worth knowing if you exported
`GTUNNEL_LOG_LEVEL` to diagnose something and left it set: the next `autostart` puts
that level on the tunnel for good.

### Environment

| Variable | Read by | Effect |
|---|---|---|
| `GTUNNEL_CONFIG_DIR` | every command | Moves `config.toml`, `tunnels.toml`, `agents/` and `logs/` into that directory. The service label is still `com.gtunnel.<account>.<name>`, so give a throwaway configuration an account name of its own or `install` restarts the live job that shares the label |
| `GTUNNEL_LOG_LEVEL` | every command | `quiet`, `info` or `debug`. Beaten by `--log-level`, beats `log_level` in `config.toml`. An empty value counts as unset; anything else that is not a level is an error naming the variable. See [The log level](#the-log-level) |
| `VISUAL`, then `EDITOR` | the TUI's config page | Which editor `e` opens. `VISUAL` wins because it names the full-screen editor. An empty value counts as unset, since `EDITOR=` is how a shell clears one, and neither set falls back to the platform default |
| `PATH` | startup | Where `gcloud` is found |

The value of `VISUAL` or `EDITOR` is split into a program and its arguments here, never
handed to a shell, so `code -w` works and nothing in the variable is interpreted as a
command line. Quotes group a path containing spaces and do nothing else.

With `GTUNNEL_CONFIG_DIR` unset the directory is `~/.gtunnel` (falling back to
`~/Library/Application Support/gtunnel` on macOS and `$XDG_CONFIG_HOME/gtunnel` on
Linux if `~/.gtunnel` does not exist).

### Exit codes

gtunnel never calls `exit` with a code of its own, so there are only two:

| Code | Meaning |
|---|---|
| `0` | The command did what it said |
| `1` | It did not, and the reason is on stderr, or on stdout for a failed start |

A failure is one line on stderr beginning `Error:`, so a script can branch on the status
and show the line. A start that did not come up is the exception: it keeps the `✗` lines it
always printed on stdout, with no `Error:` line after them. Do not branch on which error it was: the text is written for a person
and is not a stable interface.
