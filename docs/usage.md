# Usage

[Home](../README.md) / Usage / [Commands](commands.md) / [Configuration](configuration.md) / [Architecture](architecture.md) / [Troubleshooting](troubleshooting.md)

Contents:

- [Getting set up](#getting-set-up)
- [Concepts](#concepts)
- [Command map](#command-map)
- [Startup](#startup)
- [The TUI](#the-tui)
- [Model Context Protocol (MCP) and AI Agent Integration](#model-context-protocol-mcp-and-ai-agent-integration)

Getting set up, the concepts, startup, the TUI and the MCP server. The commands are in
[commands.md](commands.md) and the files in [configuration.md](configuration.md). Outputs were captured from a real run unless the
section says the opposite.

`--account <name>` and `--log-level <level>` work on every command, and override the
default account and the configured log level respectively; see
[The log level](configuration.md#the-log-level). Paths in
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
| Already installed | [`gtunnel update`](commands.md#update) |

There is no `cargo install gtunnel`. The name on crates.io belongs to an unrelated crate
by another author, so a bare name would install somebody else's binary. If you have
already run it, [troubleshooting.md](troubleshooting.md#cargo-install-gtunnel-installed-something-else)
says what it left behind and how to undo it.

Specify the release version and substitute your machine's file: `gtunnel-macos-arm64`,
`gtunnel-macos-intel`, `gtunnel-linux-amd64` or `gtunnel-linux-arm64`.

```
VERSION="v0.2.22"
F="gtunnel-macos-arm64-${VERSION}.tar.gz"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F.sha256"
shasum -a 256 -c "$F.sha256" && tar -xzf "$F" && sudo mv gtunnel /usr/local/bin/
```

On Linux the checksum tool is `sha256sum -c`. Both Linux builds are static musl binaries,
so they run on any glibc.

```
$ gtunnel --version
gtunnel 0.2.22
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
This sample was captured from v0.1.1, so the tag and commit are those of that release; yours name
the file you downloaded. Attestations begin with v0.1.1; v0.1.0 predates them.

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

[init](commands.md#init) shows the prompts. A default zone is required — `gtunnel zones` lists the ones
in the project.

### 6. Check the ground before the first tunnel

```
gtunnel doctor
```

[doctor](commands.md#doctor) reports what it can see and prints the exact command for anything it
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
[add](commands.md#add) covers the three target shapes: an instance port, a host behind `--via`, and an
IAP destination group.

Publishing a local service needs a Cloud DNS managed zone and a one-off relay per account:
`gtunnel relay up --managed-zone <zone>` ([relay](commands.md#relay)), then [expose](commands.md#expose).
[domains add](commands.md#zones-and-domains) registers further zones once the relay exists.

## Concepts

gtunnel has two modes. Everything else is a choice inside one of them.

| Mode | Direction | Command | What it creates in GCP |
|---|---|---|---|
| Access | Your laptop reaches a VM, or a host in the VPC | [`add`](commands.md#add) | Nothing: a local process and an IAP connection |
| Expose | The internet reaches a service on your laptop, through the relay | [`expose`](commands.md#expose) | A relay VM and load balancer, once per account, shared by every tunnel |

In `tunnels.toml` the mode is `mode = "access"` or `mode = "expose"`
([configuration.md](configuration.md#tunnelstoml)).

### Three kinds of Access target

An Access tunnel reaches its target one of three ways. The kind is `[tunnels.spec.target] kind`.

| Kind | Flag | Engine | Ready line in the log | Needs |
|---|---|---|---|---|
| `instance` | none, `INSTANCE:PORT` | `start-iap-tunnel INSTANCE PORT` | `Listening on port` | The target port open to `35.235.240.0/20` |
| `jump` | `--via INSTANCE` | `compute ssh INSTANCE --tunnel-through-iap -- -N -L` | `Local forwarding listening on 127.0.0.1 port N` | ssh to the instance, and port 22 open to `35.235.240.0/20` |
| `dest_group` | `--dest-group G` | `start-iap-tunnel HOST PORT --dest-group` | `Listening on port` | An IAP destination group in the account's project |

`jump` is the one that differs: the forward is made by ssh on the instance, so the host
behind it can be anything the instance reaches, such as a private GKE control plane. Every
kind also needs `roles/iap.tunnelResourceAccessor`
([IAM](troubleshooting.md#iam), [firewall](troubleshooting.md#iap-firewall)).

The instance is always yours. In the TUI the add flow picks it from the project's
instances; `gtunnel add` and `gtunnel run` take it as an argument. For `jump`, `instance`
is the machine ssh runs on and `host` is the destination it reaches. `bastion`, `my-vm`
and `10.0.0.10` in the examples are placeholders.

### Lifetime and visibility

These two choices combine with either mode.

| Choice | Values | Chosen by |
|---|---|---|
| Lifetime | Persistent: recorded in `tunnels.toml`, run as a supervised service. Ephemeral: nothing recorded, Ctrl+C or `--ttl` ends it | `add` and `expose`, or [`run`](commands.md#run) |
| Visibility, Expose only | Public, or gated: `--private` puts IAP sign-in in front of the URL | `expose --private`, or `run --public` for an ephemeral Expose |

## Running in CI

A CI step runs gtunnel with no terminal and no person to log in. It needs a Google identity
that is already there, and it needs no service manager: [`gtunnel run`](commands.md#run) is a
foreground process that tears its tunnel down when it exits.

| Where the step runs | Identity to use | Key to keep |
|---|---|---|
| Cloud Build, a GCE or GKE runner | The service account attached to the runner. gcloud reads it from the metadata server | none |
| Another CI or a shared host | A workload identity federation file named by `credential_file` | none: the file holds no secret |
| Anything else | A service-account key named by `credential_file` | the key itself |

Google advises against service-account keys, and organisations created on or after
2024-05-03 block creating them by default (Google Cloud, "Best practices for managing
service account keys" and the `iam.disableServiceAccountKeyCreation` organisation policy).
Reach for a key last.

### What the identity needs

| Need | Value | Where it is set |
|---|---|---|
| Role | `roles/iap.tunnelResourceAccessor` on the project or the instance | [IAM](troubleshooting.md#iam) |
| Firewall | Ingress from `35.235.240.0/20` to the target port | [IAP firewall](troubleshooting.md#iap-firewall) |

### Declare the account from flags

On a runner there is no gcloud configuration to read a project from. Pass the project and the
zone, and gtunnel builds the account with no gcloud call. `GTUNNEL_CONFIG_DIR` keeps the
runner's files in a directory of its own.

```
$ export GTUNNEL_CONFIG_DIR=/workspace/gtunnel
$ gtunnel account add ci --configuration ci --project example-dev-1a2b --zone europe-west1-b
Account 'ci' saved to /workspace/gtunnel
```

The name after `--configuration` need not exist: gcloud accepts a configuration it has not
seen (checked with gcloud 590, `gcloud --configuration=ci config list` exits 0). It creates
nothing, though, and warns on each call that reads it, so every tunnel's log carries:

```
WARNING: Could not open the configuration file: [<gcloud config dir>/configurations/config_ci].
```

The warning is harmless. To silence it, create the configuration once, without making it the
active one: `gcloud config configurations create ci --no-activate`.

### Name a credential file

Outside GCP, point the account at the file. gtunnel stores the path and passes it to gcloud on
every call as `--credential-file-override=<path>`; it never copies, stores or logs the
file, and `doctor` reads it to check its type and mode ([decision 1](decisions.md#1-gtunnel-stores-no-credential)).

```
$ gtunnel account add ci --configuration ci --project example-dev-1a2b \
    --zone europe-west1-b --credential-file /secrets/ci-key.json
```

| Rule | Why |
|---|---|
| The path is absolute | A service runs from another directory with no shell, so `~` and a relative path resolve against nothing. `gtunnel` refuses both |
| The path may hold spaces | It travels as one argument, never through a shell |
| The account never opens a browser login | A login would sign in a person, not the key. A failed token names the file instead |
| `impersonate_service_account` still applies | The file's identity impersonates it, as the logged-in user would |

`gtunnel doctor` checks the file before it asks for a token and names what is wrong: it does
not exist, is not JSON, has a `type` other than `service_account` or `external_account`, or is
a key readable by group or others. It reads the `type` field and the mode, nothing else.

### A Cloud Build step

The step runs a tunnel in the background for the length of the job and stops it with `--ttl`
when the job overruns. `bastion` and `10.0.0.10` are placeholders.

```
steps:
  - name: gcr.io/google.com/cloudsdktool/google-cloud-cli
    entrypoint: bash
    args:
      - -c
      - |
        export GTUNNEL_CONFIG_DIR=/workspace/gtunnel
        gtunnel account add ci --configuration ci \
          --project example-dev-1a2b --zone europe-west1-b
        gtunnel --account ci run db bastion:5432 --local-port 15432 --ttl 20m &
        for i in $(seq 30); do nc -z localhost 15432 && break; sleep 2; done
        psql -h localhost -p 15432 -c 'select 1'
```

The step needs the `gtunnel` binary on its `PATH`; [Getting set up](#getting-set-up) installs
it. A tunnel through a VM, or one connection per query, behaves differently under load: read
[Limits](architecture.md#limits) before pointing a connection pool at one.

## Command map

| Command | Purpose | Touches GCP |
|---|---|---|
| `init` | Import a gcloud configuration as a gtunnel account | read |
| `add` | Add a persistent Access tunnel | read |
| `expose` | Publish a local service at `https://<name>.<domain>` | write |
| `run` | Ephemeral tunnel in the foreground, gone on Ctrl+C | read, or write with `--public` |
| `start` / `stop` / `restart` | Service control | write, for an Expose tunnel only |
| `autostart` | Turn start-at-login on or off for a tunnel | no |
| `logs` | Read a tunnel's log, or a live run's | no |
| `list` | Every tunnel, managed or not | no |
| `delete` | Remove a tunnel and its route | write for Expose tunnels |
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
| `mcp` | Run the MCP stdio server for an agent | per tool |

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
terminal (stdin or stdout is redirected, as under cron or CI), gtunnel exits 4 (`auth`, see [Errors and exit codes](commands.md#errors-and-exit-codes)) without
starting anything or opening a browser.

```
$ gtunnel list < /dev/null
Error: gtunnel needs a logged-in Google account for 'prod'. Run `gcloud auth login`
```

### Update hint

Every command except `update` prints a one-line hint when a cached check found a newer
version. [update](commands.md#update) has the check itself.

```
$ gtunnel list
gtunnel v9.9.9 is available (current: v0.2.22). Run `gtunnel update`.
```

## The TUI

`gtunnel` with no subcommand opens the TUI. `gtunnel demo` opens it filled with synthetic
data and never touches GCP or a config file. It needs no login, and its forms open the same
pickers as the real TUI, filled with placeholder configurations, projects, zones and
instances, so a form can be shown without an account behind it.

`?` opens the full keymap, in the same four groups:

| Group | Keys |
|---|---|
| Tunnel | `s` start, `S` stop (a run too), `R` restart (refused on a run), `A` toggle auto-start, `h` health check now |
| The list | `a` add, `e` edit, `d` delete, `m` import an ephemeral run as managed, `j` `k` or the arrows move the selection |
| This tunnel's URL and log | `c` copy the URL, `l` copy the log to clipboard, `o` open it, `L` clear the log, `r` refresh the list, metrics and health |
| The program | `;` cycle accounts (the relay and its domains change with the account), `:` the accounts page, `?` the keymap, `C` the config page, `q` quit, `Esc` close a page or cancel a prompt, `PgUp` `PgDn` and `g` `G` scroll the keymap page, `Ctrl+Z` suspend, `Ctrl+C` quit from anywhere |

Details shows the tunnel's health as a symbol and a word. A failed probe is named by what
the log says, so the fix is on screen instead of in the log:

| Health | Symbol | Meaning | Do |
|---|---|---|---|
| healthy | `✓` | the far end answered | nothing |
| unhealthy | `✗` | no answer, and no cause the log names | read the log |
| auth expired | `⊗` | no answer, and the log in the last two minutes says `Reauthentication required` or `gcloud auth login`; or the tunnel stopped itself for that reason and has not been started since | `gcloud auth login`, then `s` (`R` while it still runs) |
| crash loop | `↻` | the service restarted three times or more inside two minutes | check the log |
| checking | `…` | the first probe is still in flight | wait |
| unknown | `?` | nothing to probe: stopped, or the local port is unknown | none |

A `[4033:` line is not auth expired: IAP refused an identity that is logged in, so the fix is
the `roles/iap.tunnelResourceAccessor` role, and the row stays unhealthy. The crash loop is
counted from one TUI session's metrics rounds, so it is empty for the first seconds after
`gtunnel` opens. `gtunnel list --json`, `gtunnel_list_tunnels` and `gtunnel_get_status`
return `health` as `healthy`, `unhealthy`, `auth_expired` or `unknown`, and do not report
crash loops, which need restarts over time.

The keymap page also carries a **SESSION PATHS** summary section showing the active loaded
paths for `Config` (`~/.gtunnel/config.toml`), `Tunnels` (`~/.gtunnel/tunnels.toml`), and
`Logs` (`~/.gtunnel/logs/`).

`:` is the shift of `;`, so the key that cycles accounts and the key that opens all of them
are one key twice. The page lists every account, and adds, edits, selects and removes them;
its removal obeys the same refusal the CLI does, so an account holding tunnels stays.

Editing an account asks for its project first. The text field opens pre-filled with the
current project while the list is fetched; when the list arrives it becomes a picker that
filters by id or name as you type, with the current project selected so Enter keeps it. A
filter that matches nothing is taken as the project you typed. If the list is empty, denied
or fails, the text field stays and the line `Could not list projects: <cause>` says why.
Starting to type before the list arrives keeps the field.

The zone step works the same way, with the zones of the project chosen in step 1, which may
not be the saved one. While the project is unchanged, the current zone is selected even when
the project does not list it, so Enter keeps it. Unlike projects, a typed zone the list lacks
is refused, `'<zone>' is not a zone of <project>`, because the zone list is the project's whole
set. A failure says `Could not list zones: <cause>` and keeps the text field.

Adding an account starts from a picker of the machine's gcloud configurations, one row each
as `<name>  <project>  (active)`, with the active one selected. A configuration with no
project is refused with the command that sets one,
`gcloud config set project <id> --configuration=<name>`. A typed name the list lacks is taken,
and the text field stays when there is no configuration or gcloud cannot list them.

While a list is on its way, the field or picker says so with a spinner, e.g.
`loading configurations...`, and the instance picker says `nothing matches` only once the
list has answered. The accounts page shows the operation it is running, e.g. `add sandbox`,
until the result replaces it.

Every form field has a line under its label saying what the value is for, and the dialog
title counts the steps: `New account 1/2`, `Edit account 2/2`, `New access tunnel 3/5`,
`New public URL 2/3`. On a short terminal the description goes first, so the field always
fits. The tunnel edit dialog labels its value: `Ports (remote:local):` for an Access tunnel,
`Local target:` for an Expose one.

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
tunnels shows `no tunnels in this account` under the list header, and the footer offers
`a add`; until the first load has answered, and after `;` until the new account's has, the
row says `loading` instead. `m import` appears only on a selected ephemeral run, a
`gtunnel run` started in a terminal: it turns that run into a managed tunnel with its own
service, so it outlives the terminal. A first load that fails puts its error on the status line and
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
| Where it comes from | the daily background `update --check` (see [update](commands.md#update)); the TUI starts one when the cache is missing or a day old, and re-reads the cache every minute |
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
with its source port (see [Connections in the log](commands.md#connections-in-the-log)).

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
| `config.toml` | parses, has at least one account, `selected_account` names one of them, and no key is one gtunnel would ignore |
| `tunnels.toml` | parses, every name is valid as for `gtunnel add`, no name repeats within an account, no two tunnels share a local port (in any account), no tunnel uses a local port a live `gtunnel run` holds (`Local port 17777 is already used by the run 'scratch'`), and every tunnel's account is in `config.toml`, and no key is one gtunnel would ignore |

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

A key gtunnel does not read is refused the same way, naming one key and counting the rest (`not saved: unknown key accounts[0].expose.domains[0].log_level and 1 more`). The path says which table the key landed in, which is how a `log_level` typed below the last `[[accounts]]` shows itself. Only the save is refused: a file that already has such a key still loads, and `gtunnel doctor` reports it.

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

## Model Context Protocol (MCP) and AI Agent Integration

gtunnel provides first-class support for AI agents through both an in-repo Skill and a native
Model Context Protocol (MCP) server.

### The in-repo Skill

Located at `.agents/skills/gtunnel/SKILL.md` using the open Agent Skills standard. It automatically
guides AI agents (Claude Code, Gemini CLI, etc.) on:

- Enforcing the tunnel naming rule in [Naming a tunnel](commands.md#naming-a-tunnel).
- Routing client-side protocols through one bastion tunnel (e.g. MongoDB Compass over SSH) rather than one tunnel per backend host.
- Scoping commands to the correct GCP account.
- Performing mandatory pre-completion verification: checking daemon status, inspecting logs for IAP error codes (such as `4033`), and probing local listening ports.

### The native MCP Server

`gtunnel mcp` runs a newline-delimited JSON-RPC 2.0 stdio server in pure Rust. It exposes 14 tools
and 2 live resources without requiring an external runtime.

<p align="center">
  <img src="demo-mcp.gif" alt="Real MCP calls to gtunnel mcp: the tools, gtunnel_run_tunnel opening db on my-vm:5432 for 30 minutes, gtunnel list showing the run and its stop time, gtunnel_stop_tunnel closing it" width="720">
</p>

#### Exposed Tools

| Tool | Parameters | What it does |
|---|---|---|
| `gtunnel_list_tunnels` | `account?` | Lists configured tunnels and the account's live runs (`"run": true`), each with `status`, `local_port` (`null` on a public tunnel), `health` and `unmanaged` (always `false`): the rows of `gtunnel list --json` without the unmanaged ones |
| `gtunnel_start_tunnel` | `name`, `account?` | Starts a stopped tunnel daemon |
| `gtunnel_stop_tunnel` | `name`, `account?` | Stops a running tunnel daemon; with no persistent tunnel of that name, stops the live `gtunnel_run_tunnel` run of that name |
| `gtunnel_restart_tunnel` | `name`, `account?` | Reinstalls service spec and restarts daemon |
| `gtunnel_get_status` | `name`, `account?` | Returns `status`, `health`, PID, restarts and the TCP probe's round-trip latency; for a live run, its pid, `local_port` and `expires_at` instead of restarts (`"run": true`) |
| `gtunnel_get_logs` | `name`, `lines?`, `account?` | The last `lines` lines (default 50) of the tunnel's log, as written and without interpretation: the agent reads the error code itself. For a run launched by `gtunnel_run_tunnel`, the last lines of `logs/run-<account>-<name>.log` (a run started in a terminal has none) |
| `gtunnel_add_tunnel` | `name`, `target`, `via?`, `zone?`, `local_port?`, `dest_group?`, `region?`, `network?`, `start?`, `account?` | Adds an Access tunnel |
| `gtunnel_run_tunnel` | `target`, `ttl_minutes`, `name?`, `zone?`, `local_port?`, `account?` | Opens a short-lived Access tunnel that closes by itself after `ttl_minutes` |
| `gtunnel_delete_tunnel` | `name`, `account?` | Deletes a persistent tunnel and removes its service |
| `gtunnel_rename_tunnel` | `name`, `new_name`, `account?` | Renames a tunnel on its account, moving its service and log; a running tunnel restarts under the new name. Answers `Renamed <old> to <new>`, the line `gtunnel rename` prints. A name the account already uses is refused and nothing changes |
| `gtunnel_expose_tunnel` | `name`, `target`, `domain?`, `public?`, `start?`, `account?` | Publishes a local service at `https://<name>.<domain>` through the account's relay. Private by default (visitors sign in with Google); `public: true` opens it to anyone, the reverse of the CLI, which is public unless `--private`. Publishes only a service on this machine (a port, `localhost` or a loopback address; other hosts need the CLI). Never creates a relay: an account without one fails with `gtunnel relay up`. Returns `name`, `account`, `url`, `target`, `domain`, `public`, `certificate_state` and `started`; a failed start is an error that names the tunnel it created |
| `gtunnel_move_tunnel` | `name`, `to`, `rename?`, `account?` | Moves an Access tunnel to another configured account after checking its target there; `rename` gives it a new name when the destination already uses this one. Answers like `gtunnel move`. A destination that is not logged in fails with kind `auth`: MCP never prompts |
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

#### Protocol revision, annotations and structured output

| Part | Behaviour |
|---|---|
| Revisions | `2026-07-28` is served statelessly: a request whose `_meta` carries `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities` needs no handshake, and `server/discover` lists the versions, capabilities and server identity. Every such result gains `resultType: "complete"` and `_meta["io.modelcontextprotocol/serverInfo"]`. An unknown version in that `_meta` is `-32022` with the supported list, a missing capabilities object is `-32602` |
| Legacy revisions | `initialize` answers the `protocolVersion` the client sent when it is `2025-11-25`, `2025-06-18` or `2024-11-05`; a missing, unknown or newer one gets `2025-11-25`. A request without the modern `_meta` is served exactly as before |
| Titles and annotations | Every tool has a `title` and all four hints. `readOnlyHint` is true for `gtunnel_list_tunnels`, `gtunnel_get_status`, `gtunnel_get_logs`, `gtunnel_list_accounts` and `gtunnel_doctor`; `destructiveHint` is true for `gtunnel_stop_tunnel`, `gtunnel_restart_tunnel`, `gtunnel_delete_tunnel`, `gtunnel_rename_tunnel`, `gtunnel_move_tunnel` and `gtunnel_expose_tunnel` |
| Structured output | `gtunnel_list_tunnels`, `gtunnel_get_status`, `gtunnel_run_tunnel`, `gtunnel_expose_tunnel`, `gtunnel_list_accounts` and `gtunnel_doctor` return `structuredContent` and declare an `outputSchema`. The list and the doctor wrap their array as `{"tunnels": [...]}` and `{"findings": [...]}`; the `content[0].text` of every tool is unchanged |
| Failures | A failed call under an `outputSchema` returns the `{"error": {"kind", "message"}}` object, which each schema also admits |

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
Validated 14 tools and 2 resources successfully.
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
