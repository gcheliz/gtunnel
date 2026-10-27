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
VERSION="v0.1.51"
F="gtunnel-macos-arm64-${VERSION}.tar.gz"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F.sha256"
shasum -a 256 -c "$F.sha256" && tar -xzf "$F" && sudo mv gtunnel /usr/local/bin/
```

On Linux the checksum tool is `sha256sum -c`. Both Linux builds are static musl binaries,
so they run on any glibc.

```
$ gtunnel --version
gtunnel 0.1.51
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
a C compiler: the TLS stack builds C code. A published binary needs neither. A release
build takes about three minutes from clean on an Apple silicon laptop, because the release
profile uses fat LTO.

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
gtunnel add db my-vm:5432 --local-port 15432 -s
psql -h localhost -p 15432
```

`--local-port` pins the port; without it the port is derived from the name, in 15000-15999.
`-s` installs the user service and starts it, so the tunnel survives closing the terminal.
[add](#add) covers the three target shapes: an instance port, a host behind `--via`, and an
IAP destination group.

Publishing a local service needs a Cloud DNS managed zone and a one-off relay per account:
`gtunnel relay up --managed-zone <zone>` ([relay](#relay)), then [expose](#expose).
[domains add](#zones-and-domains) registers further zones once the relay exists.

## Command map

| Command | Purpose | Touches GCP |
|---|---|---|
| `init` | Import a gcloud configuration as a gtunnel account | read |
| `add` | Add a persistent Access tunnel | read |
| `expose` | Publish a local service at `https://<name>.<domain>` | write |
| `run` | Ephemeral tunnel in the foreground, gone on Ctrl+C | read, or write with `--public` |
| `start` / `stop` / `restart` | Service control | write, for a public tunnel only |
| `autostart` | Turn start-at-login on or off for a tunnel | no |
| `logs` | Read a tunnel's log, or a live run's | no |
| `list` | Every tunnel, managed or not | no |
| `delete` | Remove a tunnel and its route | write for public tunnels |
| `rename` | Rename a tunnel, its service and its log | no |
| `move` | Hand an Access tunnel to another account | read, to verify the destination |
| `zones` / `domains` | Defaults for new tunnels | read |
| `relay up` / `down` / `status` | The shared relay for an account | write |
| `account` | Accounts: `list`, `select` (alias `default`), `add`, `edit`, `remove` | read, for `add` |
| `watch` | Restart tunnels that are running but refusing connections, and announce state changes | no |
| `doctor` | Report, and optionally remove, leftovers | read, write with `--fix` |
| `reset` | Remove all local configuration | no |
| `demo` | TUI with synthetic data | no |
| `update` | Check for and install a new version | no |
| `completions` | Print a shell completion script | no |

## Startup

Before it opens the TUI or runs a command that reaches GCP, gtunnel asks gcloud for an
access token for the selected account (or `--account`), the same probe `doctor` uses. A
token that cannot be minted, including an expired refresh token, counts as logged out.

| Checks the login | Skips the check |
|---|---|
| `gtunnel` (the TUI), `add -s`, `expose`, `run`, `start`, `stop`, `restart`, `move`, `zones`, `domains`, `relay` | `init`, `account`, `add` (without `-s`), `delete`, `rename`, `autostart`, `logs`, `list`, `zones default`, `domains default`, `domains add`, `doctor`, `watch`, `reset`, `demo`, `update`, `mcp` |

`doctor` is skipped because it reports broken credentials as a finding and a login prompt
would hide it; `init` has no account yet; `mcp` speaks JSON-RPC on stdin and stdout, so a
prompt there would corrupt the protocol. Background services run `gcloud` directly and never
go through this check.

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
terminal (stdin or stdout is redirected, as under cron or CI), gtunnel exits 4 (`auth`, see [Errors and exit codes](#errors-and-exit-codes)) without
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

Details shows the tunnel's health as a symbol and a word. A failed probe is named by what
the log says, so the fix is on screen instead of in the log:

| Health | Symbol | Meaning | Do |
|---|---|---|---|
| healthy | `✓` | the far end answered | nothing |
| unhealthy | `✗` | no answer, and no cause the log names | read the log |
| auth expired | `⊗` | no answer, and the log in the last two minutes says `Reauthentication required` or `gcloud auth login` | `gcloud auth login`, then `R` |
| crash loop | `↻` | the service restarted three times or more inside two minutes | check the log |
| checking | `…` | the first probe is still in flight | wait |
| unknown | `?` | nothing to probe: stopped, or the local port is unknown | none |

A `[4033:` line is not auth expired: IAP refused an identity that is logged in, so the fix is
the `roles/iap.tunnelResourceAccessor` role, and the row stays unhealthy. The crash loop is
counted from one TUI session's metrics rounds, so it is empty for the first seconds after
`gtunnel` opens. `gtunnel list --json` carries no health, because it never probes; the MCP
`gtunnel_get_status` tool returns `health` as `healthy`, `unhealthy`, `auth_expired` or
`unknown`, and does not report crash loops, which need restarts over time.

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
traffic. The count is for the screen only: the tunnel's log records each connection itself,
with its source port (see [Connections in the log](#connections-in-the-log)).

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
| `tunnels.toml` | parses, every name is valid as for `gtunnel add`, no name repeats within an account, no two tunnels share a local port (in any account), no tunnel uses a local port a live `gtunnel run` holds (`Local port 17777 is already used by the run 'scratch'`), and every tunnel's account is in `config.toml` |

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

A file that changed since `e` opened it is refused too, with `not saved: tunnels.toml changed since you opened it` (or `config.toml`): another gtunnel, the CLI or the MCP server wrote it while the editor was open. The edit stays on the page and in its copy, and a second save replaces that change, so the choice is yours; a further write in between is refused again. The save takes the same lock the commands do, so it never lands in the middle of one. Leaving the text as it was is not a save, so it cannot overwrite the other change either: it reports `no changes to tunnels.toml`.

A copy left by a gtunnel that is no longer running, after a crash or a closed terminal, is removed when the page opens; the copy of another running gtunnel is kept.

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
    "run": false
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
    "expires_at": 1793103233
  }
]
```

`run` is on every row, so one schema covers declared tunnels and runs, and `mode` still says
what each is. `expires_at` (unix seconds) appears only on a
run started with `--ttl`. `gtunnel_list_tunnels` returns the same two fields.

```
$ gtunnel logs nope
Error: Tunnel 'nope' not found for account 'dev'
```

`logs` and `stop` take the name of a live run when no saved tunnel has it, and a saved tunnel
wins when both exist. A run launched by an MCP client has a log, `logs/run-<account>-<name>.log`;
a run started in a terminal prints there instead and has none, so `logs` says so:
`Run 'name' has no log: it was started from a terminal, which has its output`.

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
gtunnel account add <name> --configuration <gcloud-configuration>
gtunnel account edit <name> [--project P] [--zone Z]
gtunnel account remove <name>
```

`add` reads the project and default zone from an existing gcloud configuration, so an account
is never a second place to type what gcloud already knows. `edit` changes only what it is
given:

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
| `--json` | Print the findings as a JSON array of `account`, `description` and `fixable` |

`--fix` removes stale local records, dead ephemeral records and per-tunnel relay leftovers
no live tunnel claims, reconciles auto-start to what `tunnels.toml` says, and installs a
missing watchdog; [troubleshooting.md](troubleshooting.md#leftovers) lists each finding. It
re-checks the claim immediately before each removal, so a tunnel started during the scan is
left alone. Findings it cannot fix, such as a missing firewall rule, are printed with the
exact command that fixes them.

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

Captured from a v0.1.34 build while v0.1.35 was the latest release. On the latest release the
second line is `gtunnel v<version> is already the latest version.` instead.

Every other command prints a one-line hint when a cached check found a newer version:

```
$ gtunnel list
gtunnel v9.9.9 is available (current: v0.1.51). Run `gtunnel update`.
```

The check runs at most once a day, in the background, and never blocks a command. A
downloaded binary and one in `~/.cargo/bin` replace themselves after verifying the published
SHA-256; a Homebrew install is detected and pointed at `brew upgrade gtunnel` instead.

## Errors and exit codes

A failure has a kind, and the kind decides the exit code. A script branches on the code, or
on `error.kind` when a command runs with `--json`.

| Kind | Exit | Meaning |
|---|---|---|
| `internal` | 1 | Anything unclassified: an unreadable config, a failed write, a tunnel that did not come up |
| `usage` | 2 | A bad argument or setting, such as a target without a port or a bad `GTUNNEL_LOG_LEVEL`. Same code clap uses for a malformed command line |
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

## Model Context Protocol (MCP) and AI Agent Integration

gtunnel provides first-class support for AI agents through both an in-repo Skill and a native
Model Context Protocol (MCP) server.

### The in-repo Skill

Located at `.agents/skills/gtunnel/SKILL.md` using the open Agent Skills standard. It automatically
guides AI agents (Claude Code, Gemini CLI, etc.) on:

- Enforcing tunnel naming conventions (`<platform>` or `<platform>-<service>`, never account/env suffixes).
- Routing client-side protocols through one bastion tunnel (e.g. MongoDB Compass over SSH) rather than one tunnel per backend host.
- Scoping commands to the correct GCP account.
- Performing mandatory pre-completion verification: checking daemon status, inspecting logs for IAP error codes (such as `4033`), and probing local listening ports.

### The native MCP Server

`gtunnel mcp` runs a newline-delimited JSON-RPC 2.0 stdio server in pure Rust. It exposes 11 tools
and 2 live resources without requiring an external runtime.

<p align="center">
  <img src="demo-mcp.gif" alt="Real MCP calls to gtunnel mcp: the 11 tools, gtunnel_run_tunnel opening db on my-vm:5432 for 30 minutes, gtunnel list showing the run and its stop time, gtunnel_stop_tunnel closing it" width="720">
</p>

#### Exposed Tools

| Tool | Parameters | What it does |
|---|---|---|
| `gtunnel_list_tunnels` | `account?` | Lists configured tunnels and their daemon statuses, and the account's live runs (`"run": true`) |
| `gtunnel_start_tunnel` | `name`, `account?` | Starts a stopped tunnel daemon |
| `gtunnel_stop_tunnel` | `name`, `account?` | Stops a running tunnel daemon; with no persistent tunnel of that name, stops the live `gtunnel_run_tunnel` run of that name |
| `gtunnel_restart_tunnel` | `name`, `account?` | Reinstalls service spec and restarts daemon |
| `gtunnel_get_status` | `name`, `account?` | Returns `status`, `health`, PID, restarts and the TCP probe's round-trip latency; for a live run, its pid, `local_port` and `expires_at` instead of restarts (`"run": true`) |
| `gtunnel_get_logs` | `name`, `lines?`, `account?` | The last `lines` lines (default 50) of the tunnel's log, as written and without interpretation: the agent reads the error code itself. For a run launched by `gtunnel_run_tunnel`, the last lines of `logs/run-<account>-<name>.log` (a run started in a terminal has none) |
| `gtunnel_add_tunnel` | `name`, `target`, `via?`, `zone?`, `local_port?`, `dest_group?`, `region?`, `network?`, `start?`, `account?` | Adds an Access tunnel |
| `gtunnel_run_tunnel` | `target`, `ttl_minutes`, `name?`, `zone?`, `local_port?`, `account?` | Opens a short-lived Access tunnel that closes by itself after `ttl_minutes` |
| `gtunnel_delete_tunnel` | `name`, `account?` | Deletes a persistent tunnel and removes its service |
| `gtunnel_list_accounts` | none | `selected_account` and every account, each with a `selected` flag |
| `gtunnel_doctor` | `account?`, `all?` | Runs diagnostic checks on GCP resources and local config |

`gtunnel_run_tunnel` is the agent's way to ask for temporary access. It starts
`gtunnel run <name> <target> --ttl <n>m` as a detached process, so the tunnel outlives the MCP
server and its only owner is the TTL, and it answers once the run has published its record:

```json
{"name": "gt-4k9x2m", "account": "demo", "url": "localhost:15247", "local_port": 15247,
 "target": "my-vm:5432 (europe-west1-b)", "ttl_minutes": 5, "expires_at": 1793103233}
```

| Rule | Why |
|---|---|
| `ttl_minutes` is required, 1 to 480 | An agent may never come back to close what it opened, so the lifetime is part of the request and has a ceiling. Longer-lived access is `gtunnel_add_tunnel`, which the user can see and delete |
| Access mode only; `public` is refused | Publishing a hostname is something a person asks for, never a side effect of an agent's call |
| `name` is optional | A `gt-xxxxxx` name is generated, as for `gtunnel run` without one |
| A `name` already live, or held by a managed tunnel in the account, is refused | The run would replace the live run's record and leave it running with nothing able to stop it |
| Output goes to `logs/run-<account>-<name>.log` | A start that fails returns that log's tail as the tool error |
| `gtunnel_stop_tunnel` closes it early | A persistent tunnel of the same name wins; without one, the name means the live run. `gtunnel stop`, `gtunnel logs`, `gtunnel_get_status` and `gtunnel_get_logs` look in the same order |

Prefer `gtunnel_run_tunnel` for access that lasts an afternoon; use `gtunnel_add_tunnel` for
something the user keeps.

A tool that fails returns a normal result with `isError: true` and the whole cause as its
text, such as `db failed to start` and the hint under it. A start that fails is such a failure:
`gtunnel_add_tunnel` with `start: true` reports it and the tunnel stays in `tunnels.toml`, as
`gtunnel add -s` leaves it. An account or tunnel that does not exist is such a failure too:
`isError: true` and a `structuredContent.error.kind` of `not_found`, the same kind strings the
CLI's `--json` errors carry. Only a call the server cannot act on is a JSON-RPC error:

| Input | Reply |
|---|---|
| A missing or mistyped parameter, a `local_port` outside 1 to 65535, an instance, zone or host that gcloud would read as an option | `error.code` -32602 |
| A tool name the server does not have | `error.code` -32601 |
| A line that is not JSON | `error.code` -32700 |
| A notification (`notifications/...`) | No reply at all, as JSON-RPC requires |

```
{"jsonrpc":"2.0","id":1,"result":{"content":[{"text":"Account 'nope' not found","type":"text"}],"isError":true,"structuredContent":{"error":{"kind":"not_found","message":"Account 'nope' not found"}}}}
{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"'local_port' must be between 1 and 65535"}}
```

Nothing but JSON-RPC frames is ever written to stdout.

#### Live Resources

| URI | What it returns |
|---|---|
| `gtunnel://tunnels` | JSON array of all persistent tunnels |
| `gtunnel://accounts` | The `gtunnel_list_accounts` object: `selected_account` and `accounts`, plus `impersonate_service_account` and `expose` on an account that sets them |

`resources/read` of `gtunnel://accounts` returns this in `contents[0].text`, the object
`gtunnel_list_accounts` returns as its content. The resource used to be the bare array of
accounts, without the selection; a client that cached that shape reads `accounts` from the
object instead:

```json
{
  "accounts": [
    {
      "default_zone": "europe-west1-b",
      "gcloud_configuration": "default",
      "name": "scratch119",
      "project": "example-dev-1a2b",
      "selected": true
    }
  ],
  "selected_account": "scratch119"
}
```

### Tool Validation and Testing

To verify tool definitions and JSON schemas non-interactively:

```
$ gtunnel mcp --validate
Validated 11 tools and 2 resources successfully.
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
| `notifications` | table | no | Which lifecycle changes raise a desktop notification: `up`, `degraded`, `recovered`, `failed`, all booleans. See [Notifications](#notifications) |
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
managed zone that serves it). `domains add` writes these; `gtunnel domains` lists them.

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
`restart`, and editing the tunnel in the TUI. Toggling auto-start (`autostart --on|--off`,
`A` in the TUI, `doctor --fix` on an auto-start finding) never restarts a running tunnel:
on macOS it writes or removes only the copy launchd loads at login, so that copy carries
the level in force at the moment of the toggle. Worth knowing if you exported
`GTUNNEL_LOG_LEVEL` to diagnose something and left it set: the next `autostart --on` puts
that level on the tunnel's start at login.

### run --ttl

`gtunnel run --ttl DURATION` stops the run by itself, through the same teardown as Ctrl+C: the
route and the record in `run/` go the same way.

| Form | Meaning |
|---|---|
| `90s`, `30m`, `2h` | Seconds, minutes, hours |
| `45` | A bare number is minutes |
| `0`, `1.5h`, `10d`, over `24h` | Refused before anything starts |

### Environment

| Variable | Read by | Effect |
|---|---|---|
| `GTUNNEL_CONFIG_DIR` | every command | Moves `config.toml`, `tunnels.toml`, `lifecycle.json`, `run/` (including the watchdog heartbeat), `agents/` and `logs/` into that directory. The service label is still `com.gtunnel.<account>.<name>`, so give a throwaway configuration an account name of its own or `install` restarts the live job that shares the label |
| `GTUNNEL_LOG_LEVEL` | every command | `quiet`, `info` or `debug`. Beaten by `--log-level`, beats `log_level` in `config.toml`. An empty value counts as unset; anything else that is not a level is an error naming the variable. See [The log level](#the-log-level) |
| `GTUNNEL_NO_NOTIFY` | every command | Silences desktop notifications when set to anything but empty or `0`. The integration tests set it, so `cargo test` never posts a banner |
| `GTUNNEL_NO_DISCOVERY` | `list`, the TUI, `mcp` | For tests: when set to anything but empty or `0`, no process outside gtunnel is looked for, so `list` shows no `pid N` rows. The integration tests set it so a tunnel running on the host cannot change a row count |
| `GTUNNEL_START_DEADLINE_MS` | `start`, `restart`, `add -s` | Milliseconds an Access tunnel is given to report it is listening before the start counts as failed. Default 20000; an empty or non-numeric value counts as unset. Public tunnels use a fixed 3-second settle instead |
| `GTUNNEL_SERVICE_BOUND_MS` | every command | A test and diagnostics knob: milliseconds to wait for one `launchctl` or `systemctl` call before it is reported as `timed out after`. Default 10000; an empty, zero or non-numeric value counts as unset. Leave it alone unless you are testing the timeout path or diagnosing a slow service manager |
| `VISUAL`, then `EDITOR` | the TUI's config page | Which editor `e` opens. `VISUAL` wins because it names the full-screen editor. An empty value counts as unset, since `EDITOR=` is how a shell clears one, and neither set falls back to the platform default |
| `PATH` | startup | Where `gcloud` is found |

The value of `VISUAL` or `EDITOR` is split into a program and its arguments here, never
handed to a shell, so `code -w` works and nothing in the variable is interpreted as a
command line. Quotes group a path containing spaces and do nothing else.

With `GTUNNEL_CONFIG_DIR` unset the directory is `~/.gtunnel` (falling back to
`~/Library/Application Support/gtunnel` on macOS and `$XDG_CONFIG_HOME/gtunnel` on
Linux if `~/.gtunnel` does not exist).

### Exit codes

gtunnel never calls `exit` with a code of its own, so there are three:

| Code | Meaning |
|---|---|
| `0` | The command did what it said |
| `1` | It did not, and the reason is on stderr, or on stdout for a failed start |
| `2` | The command line did not parse: an unknown command or flag, or a missing argument. The usage error is on stderr |

A failure is one line on stderr beginning `Error:`, so a script can branch on the status
and show the line. A start that did not come up is the exception: it keeps the `✗` lines it
always printed on stdout, with no `Error:` line after them. Do not branch on which error it was: the text is written for a person
and is not a stable interface.
