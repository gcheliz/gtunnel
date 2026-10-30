# Usage

[Home](../README.md) / Usage / [Commands](commands.md) / [Configuration](configuration.md) / [Architecture](architecture.md) / [Troubleshooting](troubleshooting.md)

Contents:

- [Getting set up](#getting-set-up)
- [Concepts](#concepts)
- [Command map](#command-map)
- [Startup](#startup)
- [The TUI](#the-tui)
- [Model Context Protocol (MCP) and AI Agent Integration](#model-context-protocol-mcp-and-ai-agent-integration)

From a bare laptop to a working tunnel in seven steps, then the TUI, CI runs and the AI agents
you use every day. The commands are in [commands.md](commands.md) and the files in
[configuration.md](configuration.md). Outputs were captured from a real run unless the section
says the opposite.

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

```console,ignore
$ gcloud version
Google Cloud SDK 590.0.0
alpha 2026.10.16
bq 2.1.40
```

### 2. Authenticate and pick a project

```text
gcloud init                 # creates a configuration: account, project, region
gcloud auth login           # the credential gtunnel will mint tokens from
gcloud config list          # confirm account and project
```

A gtunnel account wraps one gcloud configuration, which `gtunnel init` reads next. Across
environments, make one configuration each: `gcloud config configurations create <name>`.

### 3. Grant yourself IAP access

Tunnelling needs one IAP role and one firewall rule. [What GCP needs](gcp.md) lists them per
mode with the commands; `gtunnel doctor` checks both and prints the exact
`gcloud compute firewall-rules create` for your port. Without the role a tunnel starts, then
fails with `4033` ([error table](troubleshooting.md)).

### 4. Install gtunnel

Six routes. Homebrew comes first on macOS and Linux; the rest are for machines without it:

| Route | Command |
|---|---|
| Homebrew (macOS and Linux) | `brew install gcheliz/tap/gtunnel` |
| A published binary | the block below |
| From the repository | `cargo install --git https://github.com/gcheliz/gtunnel` |
| From a clone | `cargo install --path .` |
| Upgrade a Homebrew install | `brew upgrade gtunnel` |
| Upgrade a download or cargo install | [`gtunnel update`](commands.md#update) |

There is no `cargo install gtunnel`. The name on crates.io belongs to an unrelated crate
by another author, so a bare name would install somebody else's binary. If you have
already run it, [troubleshooting.md](troubleshooting.md#cargo-install-gtunnel-installed-something-else)
says what it left behind and how to undo it.

The Homebrew route installs a prebuilt binary, so it needs no Rust toolchain, and it taps
`gcheliz/tap` itself. The tap follows each release.

| Homebrew fact | Detail |
|---|---|
| Trust | Homebrew 6 and later requires trust for a non-official tap. The fully qualified name above trusts that one formula only and prints `Trusted formula gcheliz/tap/gtunnel`. Older Homebrew has no trust step; gtunnel never runs `brew trust` for you |
| Tapping by hand | `brew tap` then `brew install gtunnel` fails on Homebrew 6: a short name from an untrusted tap needs `brew trust --formula gcheliz/tap/gtunnel` first |
| Formula source | readable at [gcheliz/homebrew-tap](https://github.com/gcheliz/homebrew-tap/blob/main/Formula/gtunnel.rb) |
| Platforms | macOS arm64 and Intel, Linux amd64 and arm64. macOS arm64 is exercised end to end; Linux installs and runs `gtunnel --version` in a container; macOS Intel is untested |
| Old glibc | below 2.39 (Ubuntu 22.04), Homebrew first installs its own `glibc`, `gcc` and `binutils`, about 550 MB, as for any formula |
| Upgrades | `brew upgrade gtunnel`; `gtunnel update` points a Homebrew install there |

Specify the release version and substitute your machine's file: `gtunnel-macos-arm64`,
`gtunnel-macos-intel`, `gtunnel-linux-amd64` or `gtunnel-linux-arm64`.

```text
VERSION="v0.2.56"
F="gtunnel-macos-arm64-${VERSION}.tar.gz"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F"
curl -fsSLO "https://github.com/gcheliz/gtunnel/releases/download/${VERSION}/$F.sha256"
shasum -a 256 -c "$F.sha256" && tar -xzf "$F" && sudo mv gtunnel /usr/local/bin/
```

On Linux the checksum tool is `sha256sum -c`. Both Linux builds are static musl binaries,
so they run on any glibc.

```console
$ gtunnel --version
gtunnel 0.2.56

```

#### Already installed another way

A machine can hold a `cargo install` copy in `~/.cargo/bin` and a Homebrew one in
`/opt/homebrew/bin`. They share the config directory, the tunnel list and the service labels.
A bare `gtunnel` runs the copy first on `PATH`; each installed service names the binary that
wrote it.

| Check | Command |
|---|---|
| Which copies exist, in `PATH` order | `type -a gtunnel` |
| Which version each one is | `gtunnel --version`, and `/opt/homebrew/bin/gtunnel --version` |

Keep one. To move to Homebrew, remove the other copy, then run any command that lists or starts
tunnels. A service whose binary no longer exists is rewritten to name the running one, and
gtunnel prints `gtunnel: reinstalled N services to match this version`. `gtunnel restart <name>`
does the same for one tunnel.

```console,ignore
$ rm ~/.cargo/bin/gtunnel
$ hash -r
$ gtunnel list
```

Each upgrade command touches only its own copy: `gtunnel update` for cargo or a download,
`brew upgrade gtunnel` for Homebrew.

#### Proving where the binary came from

The `.sha256` proves the download arrived intact, not who built it: anyone able to replace
the tarball could replace the checksum. Every release
asset also carries a build attestation, signed by GitHub with a short-lived certificate
tied to this repository's Release workflow and recorded in a public transparency log.

```text
gh attestation verify "gtunnel-macos-arm64-${VERSION}.tar.gz" -o gcheliz
```

It exits 0 when the file checks out; an unattested file gets `HTTP 404: Not Found` and exit 1,
whatever its checksum says. To see what the attestation claims, ask for the certificate:

```console,ignore
$ gh attestation verify "gtunnel-macos-arm64-${VERSION}.tar.gz" -o gcheliz --format json \
  | python3 -c 'import json,sys; c=json.load(sys.stdin)[0]["verificationResult"]["signature"]["certificate"]; print(c["buildSignerURI"]); print(c["sourceRepositoryDigest"]); print(c["runnerEnvironment"])'
https://github.com/gcheliz/gtunnel/.github/workflows/release.yml@refs/tags/v0.1.1
78b5da3d7925cacd78832dc60b5ee160d98f6428
github-hosted
```

The workflow, the tag and the commit that produced the binary, on a GitHub-hosted runner
(captured from v0.1.1; yours names your release). Attestations begin with v0.1.1.

#### Building from source

The cargo routes need Rust 1.89 or newer and a C compiler, because the TLS stack builds C
code. A clean release build takes about three minutes on an Apple silicon laptop (fat LTO).

| Machine | Rust | C compiler |
|---|---|---|
| macOS | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` | `xcode-select --install` |
| Debian, Ubuntu | the same rustup line | `sudo apt install build-essential` |
| Fedora, RHEL | the same rustup line | `sudo dnf install gcc` |

Distribution packages of Rust often lag behind 1.89, so use rustup rather than `apt` or
`dnf`. On macOS and Linux, `brew install rust` also works and needs no rustup; skip the
`. "$HOME/.cargo/env"` line below.

Load the profile rustup wrote into the open shell, then check both tools:

```console,ignore
$ . "$HOME/.cargo/env"
$ cargo --version
cargo 1.88.0 (873a06493 2025-05-10)
$ cc --version | head -1
Apple clang version 21.0.0 (clang-2100.3.34.2)
```

Any `cargo` of 1.89 or later passes. Without a C compiler the build stops in its first
seconds:

```text
error: linker `cc` not found
  |
  = note: No such file or directory (os error 2)
```

Install the compiler from the table and run `cargo install` again. `cargo install` writes
`gtunnel` to `~/.cargo/bin`; if the shell cannot find it, see
[troubleshooting.md](troubleshooting.md#gtunnel-command-not-found-right-after-installing).

### 5. Create the account

Order matters: every other command needs an account, and says so until one exists.

```console
$ gtunnel doctor
? 1
Error: gtunnel is not initialized. Run `gtunnel init`.

```

```text
gtunnel init
```

On a machine never set up, `gtunnel` with no arguments from a terminal runs the same prompts,
then opens the TUI. A script or a pipe gets the error above.

[init](commands.md#init) shows the prompts. Each says what the value sets and, for a value
picked from a list, the command that lists it. `Enter = <value>` takes the default. A zone the
configuration holds is saved without asking; one it lacks is asked, and may be left empty.
Commands that reach an instance look its zone up by name, so the default zone is only a
fallback and the relay's zone; `gtunnel zones` lists the project's zones.

### 6. Check the ground before the first tunnel

```text
gtunnel doctor
```

[doctor](commands.md#doctor) prints the exact command for anything it cannot fix: missing
credentials, a gcloud Python without numpy, a missing firewall rule. Clear its findings first,
so a later failure is about the tunnel, not the setup.

### 7. Add a tunnel

```text
gtunnel add db bastion:5432 --local-port 15432 -s
psql -h localhost -p 15432
```

`--local-port` pins the port; without it the port is derived from the name, in 15000-15999.
`-s` installs and starts the user service, so the tunnel outlives the terminal.
[add](commands.md#add) covers the three target shapes: an instance port, a host behind `--via`, and an
IAP destination group.

Publishing a local service needs a Cloud DNS managed zone and a one-off relay per account:
`gtunnel relay up --managed-zone <zone>` ([relay](commands.md#relay)), then [expose](commands.md#expose).
[domains add](commands.md#zones-and-domains) registers further zones once the relay exists.

## Concepts

gtunnel has two modes. Everything else is a choice inside one of them.

| Mode | Direction | Command | What it creates in GCP |
|---|---|---|---|
| Access | Your laptop reaches a GCP instance, or a host in the VPC | [`add`](commands.md#add) | Nothing: a local process and an IAP connection |
| Expose | The internet reaches a service on your laptop, through the relay | [`expose`](commands.md#expose) | A relay instance and load balancer, once per account, shared by every tunnel |

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

The instance is always yours: the TUI picks it from the project's instances, `add` and `run`
take it as an argument. For `jump`, `instance` is where ssh runs and `host` is the destination.
`bastion` and `10.0.0.10` are placeholders.

### Lifetime and visibility

These two choices combine with either mode.

| Choice | Values | Chosen by |
|---|---|---|
| Lifetime | Persistent: recorded in `tunnels.toml`, run as a supervised service. Ephemeral: nothing recorded, Ctrl+C or `--ttl` ends it | `add` and `expose`, or [`run`](commands.md#run) |
| Visibility, Expose only | Public, or gated: `--private` puts IAP sign-in in front of the URL | `expose --private`, or `run --public` for an ephemeral Expose |

## Running in CI

A CI step has no terminal and no person to log in, so it needs a Google identity already in
place. It needs no service manager: [`gtunnel run`](commands.md#run) is a foreground process
that tears its tunnel down when it exits.

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

The role and the firewall rule are the Access ones: [What GCP needs, CI](gcp.md#ci).

### Declare the account from flags

A runner has no gcloud configuration to read a project from. Pass the project and the zone,
and gtunnel builds the account with no gcloud call. `GTUNNEL_CONFIG_DIR` keeps the runner's
files apart.

```console
$ gtunnel account add ci --configuration ci --project example-dev-1a2b --zone europe-west1-b
Account 'ci' saved to [HOME]/.gtunnel

```

With `GTUNNEL_CONFIG_DIR` exported, as on a runner, the files go there and the message names that directory instead. `[HOME]` stands for the home directory the run used.

The `--configuration` name need not exist: gcloud 590 accepts an unknown one
(`gcloud --configuration=ci config list` exits 0), but warns on each call, so every tunnel's
log carries:

```text
WARNING: Could not open the configuration file: [<gcloud config dir>/configurations/config_ci].
```

The warning is harmless. To silence it: `gcloud config configurations create ci --no-activate`.

### Name a credential file

Outside GCP, point the account at the file. gtunnel stores the path and passes it to gcloud on
every call as `--credential-file-override=<path>`; it never copies, stores or logs the
file, and `doctor` reads it to check its type and mode ([decision 1](decisions.md#1-gtunnel-stores-no-credential)).

```console,ignore
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

```text
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

The step needs `gtunnel` on its `PATH` ([Getting set up](#getting-set-up)). Read
[Limits](architecture.md#limits) before pointing a connection pool at a tunnel.

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
| `gtunnel` (the TUI), `add -s`, `add` without `--zone`, `expose`, `run`, `start`, `stop`, `restart`, `move`, `zones`, `domains`, `relay` | `account`, `add --zone` or `--dest-group` (without `-s`), `delete`, `rename`, `autostart`, `logs`, `list`, `zones default`, `domains default`, `domains add`, `doctor`, `watch`, `reset`, `demo`, `update`, `mcp` |

`doctor` skips it because it reports broken credentials as a finding; `init` has no account
yet, so it runs the same check itself before it lists projects and zones; a prompt would corrupt `mcp`'s JSON-RPC on stdin and stdout. Background services run
`gcloud` directly and never check. Only the selected account is checked: after switching
accounts in the TUI, a logged-out account surfaces as the error in that tunnel's log. An
operation the TUI or `mcp` starts, such as a move to a logged-out account, never prompts: it
fails with an `auth` error, shown in the status line or returned as the tool error.

On a terminal, a failed probe asks `[y/N]` (default `No`), runs
`gcloud auth login --configuration=<configuration>` on the terminal, then probes again. Only
the second probe counts: a login that exits 0 and still cannot mint a token fails. Illustrative
sample; the login output is gcloud's:

```console,ignore
$ gtunnel start db
gcloud has no usable credentials for account 'prod'.
gtunnel requires an authenticated GCP account to establish tunnels.
Do you want to log in now? [y/N] [n]: y
Starting `gcloud auth login`.
Your browser has been opened to visit: ...
You are now logged in as [you@example.com].
● db  localhost:5432
```

When the answer is no, the login fails, or there is no terminal (stdin or stdout redirected,
as under cron or CI), gtunnel exits 4 (`auth`, see
[Errors and exit codes](commands.md#errors-and-exit-codes)) without starting anything or
opening a browser.

```console,ignore
$ gtunnel start db < /dev/null
Error: gtunnel needs a logged-in Google account for 'prod'. Run `gcloud auth login`
```

### Update hint

Every command except `update` prints a one-line hint when a cached check found a newer
version, naming the command for how gtunnel was installed: `brew upgrade gtunnel` for
Homebrew, `gtunnel update` for a downloaded binary. [update](commands.md#update) has the
check itself. On a Homebrew install:

```console,ignore
$ gtunnel list
gtunnel v9.9.9 is available (current: v0.2.56). Run `brew upgrade gtunnel`.
```

## The TUI

`gtunnel` with no subcommand opens the TUI. `gtunnel demo` opens it with synthetic data: no
GCP, no config file, no login, and its forms open the real pickers filled with placeholder
configurations, projects, zones and instances.
A SIGTERM or SIGHUP, from `kill` or a closed terminal window, closes the TUI the way `q` does,
so the shell gets a working terminal back.

`?` opens the full keymap, in the same four groups:

| Group | Keys |
|---|---|
| Tunnel | `s` start, `S` stop (a run too, and a tunnel that keeps crashing: launchd would start it again), `R` restart, `A` toggle auto-start (`s`, `R` and `A` are refused on a run), `h` health check now |
| The list | `a` add, `e` edit, `d` delete (refused on a run), `m` import an ephemeral run as managed, `j` `k` or the arrows move the selection |
| This tunnel's URL and log | `c` copy the URL, `l` copy the log to clipboard, `o` open it, `L` clear the log, `r` refresh the list, metrics and health, `PgUp` `PgDn` scroll the log back and forward a page, `End` or `G` follow the newest line again |
| The program | `;` cycle accounts (the relay and its domains change with the account), `:` the accounts page, `?` the keymap, `C` the config page, `q` quit (it waits for a running operation to finish, and a second `q` quits at once), `Esc` close a page or cancel a prompt, `PgUp` `PgDn` and `g` `G` scroll the keymap page, `Ctrl+Z` suspend, `Ctrl+C` quit from anywhere, with the same wait |

### Small terminals and the mouse

From 121 columns the list sits left of Details, Logs and Metrics. Narrower, the panes stack: the
list is as tall as its entries, up to 40% of the height, and shows a scrollbar on its right
border when rows are hidden. Details folds into one line under it (status, url, target, uptime,
auto-start), and Logs and Metrics sit side by side below, so the chart is on screen at 80 columns.
Under 80 columns Logs takes the width and Metrics becomes one row of figures with the chart beside
them.

| Where | What it does |
|---|---|
| `PgUp` `PgDn` | scroll the log a page back and forward, in every layout. The title reads `LOGS: DB  ↓ 3 newer` while scrolled back, and a new line does not move the view |
| `End` or `G` | follow the newest line again. Moving to another tunnel does the same |
| Mouse wheel over TUNNELS | moves the selection, as `j` and `k` do |
| Mouse wheel over LOGS | scrolls the log three lines a notch |

The mouse is captured so the wheel can scroll a pane, which stops the terminal selecting text on a
plain drag. Hold `Shift` while dragging to select, as before (`Option` in some macOS terminals).
Other mouse events are ignored, and capture is released on exit, on a panic, on `Ctrl+Z` and
while an editor has the terminal.

The Metrics pane shows the tunnel's health as a symbol. A failed probe is named by what
the log says, so the fix is on screen instead of in the log:

| Health | Symbol | Meaning | Do |
|---|---|---|---|
| healthy | `✓` | the far end answered | nothing |
| unhealthy | `✗` | no answer, and no cause the log names | read the log |
| auth expired | `⊗` | no answer, and the log in the last two minutes says `Reauthentication required` or `gcloud auth login`; or the tunnel stopped itself for that reason and has not been started since | `gcloud auth login`, then `s` (`R` while it still runs) |
| crash loop | `↻` | the service restarted three times or more inside two minutes | check the log; a single restart after the machine wakes is expected: [troubleshooting](troubleshooting.md#restarts-after-the-laptop-wakes) |
| checking | `…` | the first probe is still in flight | wait |
| stopped | `○` | the tunnel is not running, or a stop is in flight, so there is nothing to probe | `s` to start |
| unknown | `?` | a running tunnel that cannot be probed: no local port is known | none |

A `[4033:` line is not auth expired: IAP refused an identity that is logged in, so the fix is
the `roles/iap.tunnelResourceAccessor` role, and the row stays unhealthy. The crash loop is
counted from one TUI session's metrics rounds, so it is empty for the first seconds after
`gtunnel` opens. `gtunnel list --json`, `gtunnel_list_tunnels` and `gtunnel_get_status`
return `health` as `healthy`, `unhealthy`, `auth_expired`, `stopped` or `unknown`, and do not report
crash loops, which need restarts over time.

The keymap page also has a **SESSION PATHS** section with the loaded `Config`
(`~/.gtunnel/config.toml`), `Tunnels` (`~/.gtunnel/tunnels.toml`) and `Logs` (`~/.gtunnel/logs/`).

`:` (the shift of `;`) opens the accounts page: it lists, adds, edits, selects and removes
accounts, and refuses to remove one that holds tunnels, as the CLI does.

| Form step | Behaviour |
|---|---|
| Add: configuration | a picker of the machine's gcloud configurations, `<name>  <project>  (active)`, the active one selected. One with no project is refused with `gcloud config set project <id> --configuration=<name>`. A typed name the list lacks is taken; with no configuration, or when gcloud cannot list them, the text field stays |
| Add: instance | a picker of the project's instances, `<name>  <zone>  iap`. Stopped instances and GKE nodes (label `goog-gke-node`) are hidden; the line under the prompt says how many, and `Tab` shows them or hides them again. The filter matches the name or the zone, a filter that matches no shown row searches the hidden ones too, and a hidden instance whose exact name is typed is listed first, so Enter selects it with its own zone. `iap` marks an instance an enabled ingress rule of the project's VPC lets `35.235.240.0/20` reach on any TCP port, and those rows come first. The check reads only the project's own firewall rules, not Shared VPC host rules or firewall policies, so an unmarked instance may still work; it orders the list and never hides a row. A firewall list that cannot be read leaves the list unmarked |
| Edit: project | a text field pre-filled with the current project, which becomes a picker filtering by id or name when the list arrives, the current project selected. A filter that matches nothing is taken as typed. Typing before the list arrives keeps the field; an empty, denied or failed list keeps it with `Could not list projects: <cause>` |
| Edit: zone | the same, with the zones of the project chosen in step 1. While the project is unchanged the current zone stays selected even if unlisted. A typed zone the list lacks is refused, `'<zone>' is not a zone of <project>`, because the list is the project's whole set. A failure says `Could not list zones: <cause>` |

While a list loads, the field says so with a spinner (`loading configurations...`), and the
instance picker says `nothing matches` only once the list has answered. The accounts page
shows the operation running (`add sandbox`) until its result replaces it.

Every form field has a line under its label saying what the value is for, and the title counts
the steps: `New account 1/2`, `Edit account 2/2`, `New access tunnel 3/5`, `New public URL 2/3`.
The tunnel edit dialog labels its value `Ports (remote:local):` for Access and `Local target:`
for Expose. A new local port meets the rules `add` applies: not 0, not another tunnel's, not
a live run's and not bound on the machine; an empty local target is refused.

`e` opens a chooser, except on a row with an op still running, where it is refused on the
status line (`db: start already in progress`) before anything is typed:

| Key | What it edits |
|---|---|
| `1` | Target and ports |
| `2` | Rename, within the same account |
| `3` | Move to another account |

`2` and `3` are the TUI's `rename` and `move`, and refuse what those refuse: an unmanaged row,
and an ephemeral run not yet imported with `m`. A move onto a name the destination already
holds asks for a new name, with the reason on screen, as `--rename` does.

Case matters in three pairs: `s`/`S` start and stop, `a`/`A` add and auto-start, `r`/`R`
refresh and restart. The footer shows only what the selected tunnel can do, so both halves of
a pair are never on screen together; the keymap page states the difference.

| List state | Shows |
|---|---|
| Selected tunnel failed | `R restart` leads the footer in the warning colour |
| Selected ephemeral run (a `gtunnel run` in a terminal) | `m import`, which makes it a managed tunnel with its own service, so it outlives the terminal |
| Account with no tunnels | `no tunnels in this account`, and the footer offers `a add` |
| First load, or after `;`, not answered yet | `loading` |
| First load failed | the error on the status line and `could not load tunnels`, until a load succeeds |

The header's right edge is the account field: the account name over its GCP project id, on
the same two rows as the logo. `;` switches both at once.

What each pane sheds as the terminal narrows or shortens, with the thresholds, is in
[design.md, Narrowing](design.md#narrowing).

When a newer release is cached, the header's closing rule carries it at its right end:

```text
 ───────────────────────────────────────────────────────────────────────────────────────── ↑ v9.9.9 · gtunnel update ──
```

| Question | Answer |
|---|---|
| Where | the rule under the header; the rule above the footer when the header is hidden (under 16 rows) or is the one-line form |
| What it says | `↑ v<latest> · <command>`: `brew upgrade gtunnel` for a Homebrew install, `gtunnel update` otherwise |
| Where it comes from | the background `update --check` (see [update](commands.md#update)); the TUI starts one when the cache is missing or older than `update_check_interval` (6 hours by default), and re-reads the cache every minute |
| When the check fails | the same place says `update check failed` in the muted colour, only while no update is showing, until a check succeeds. Silence then means "not known", not "up to date" |
| Waits on the network | never; it shows what an earlier check already learned, so the first run after a release shows nothing until a check finishes |
| `gtunnel demo` | shows no notice and starts no check |

Details repeats the project under the `target` row, next to the host it belongs to. An
account with no project prints no row.

Unmanaged `gcloud start-iap-tunnel` processes started outside gtunnel appear in the list
tagged `[not managed by gtunnel]`. Every action on them is refused except `m`, which takes
them over.

The list is a table with a `NAME  MODE  URL` header. A legacy name with an account suffix
(`k8s-dev` under `dev`) shows without it. The naming rule is
[Naming a tunnel](commands.md#naming-a-tunnel).

Metrics redraws every 5 seconds. Its `active` is the most client connections the local port
carried since the last redraw, sampled twice a second, because a `kubectl` call holds its
socket for only a few hundred milliseconds. The count is for the screen only: the tunnel's log
records each connection with its source port
([Connections in the log](commands.md#connections-in-the-log)).

### The config page

`C` opens a page that shows the two files gtunnel reads, with the absolute path of the one on
screen in its title. `Tab` switches between `config.toml` and `tunnels.toml`; `j` `k`, the
arrows, `PgUp` `PgDn`, `g` and `G` scroll as they do on the keymap page, and the count in the
bottom row (`1-16 of 29 lines`) says where the window is. A file that does not exist yet says
so: `tunnels.toml` is absent until the first `add`. `Esc` or `q` closes the page. The demo has
no config files, so `C` there says so and opens nothing.

`e` edits the file on screen; which editor it picks, and how the value is split, is in
[Environment](configuration.md#environment). gtunnel takes the screen back however the
editor exits.

The editor never opens the real file. It opens a private copy next to it
(`tunnels.edit-<pid>.toml`, owner-only), and the real file is replaced in one rename, only
after the text has passed the rules for that file:

| File | What has to be true |
|---|---|
| `config.toml` | parses, has at least one account, `selected_account` names one of them, and no key is one gtunnel would ignore |
| `tunnels.toml` | parses, every name is valid as for `gtunnel add`, no name repeats within an account, no two tunnels share a local port (in any account), no tunnel uses a local port a live `gtunnel run` holds (`Local port 17777 is already used by the run 'scratch'`), and every tunnel's account is in `config.toml`, and no key is one gtunnel would ignore |

| Outcome | What happens |
|---|---|
| Passes | committed (`config.toml` ends at mode 0600 whatever the editor did), the copy removed, and accounts, projects, the relay and the tunnels reloaded without restarting the TUI. The account on screen stays if it still exists, else the first remaining one takes over. The page reports in green: `reloaded: 1 changed, 1 added, 0 removed`, or `no changes to tunnels.toml` for unchanged text |
| Removes or renames a tunnel | a warning too: a launchd job is named for the tunnel, and editing the file does not unload it |
| Fails a rule | not saved. The page shows the text and the reason in red with its line (`not saved: line 16: key with no value, expected `=``); the keys become `e edit again` (reopens the same copy) and `Esc discard` (removes it). `Tab` does nothing meanwhile |
| Has a key gtunnel does not read | refused the same way, naming one key and counting the rest (`not saved: unknown key accounts[0].expose.domains[0].log_level and 1 more`). The path shows which table the key landed in, such as a `log_level` typed below the last `[[accounts]]`. A file that already has such a key still loads, and `gtunnel doctor` reports it |
| File changed since `e` opened it | refused with `not saved: tunnels.toml changed since you opened it` (or `config.toml`): another gtunnel, the CLI or the MCP server wrote it. The edit stays; a second save replaces that change, and a further write in between is refused again. Unchanged text is not a save, so it cannot overwrite the other change |
| Editor exits non-zero (`:cq` in vim) | the edit is cancelled and the file left as it was |

The save takes the same lock the commands do, so it never lands in the middle of one. A copy
left by a gtunnel that is no longer running is removed when the page opens; another running
gtunnel's copy is kept. Running tunnels keep their old settings until `R` restarts them, because
a launchd job is rewritten only on a restart; the page and the status line both say so.

A throwaway config (`GTUNNEL_CONFIG_DIR=/tmp/cfgdemo`, 80x24), after an `EDITOR` script that
appended one valid tunnel:

```text
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

## Model Context Protocol (MCP) and AI Agent Integration

Your agent opens, checks and closes tunnels with the binary you already have: 14 tools over
stdio, and a temporary tunnel it opens closes itself after a TTL of 1 to 480 minutes. An
in-repo Skill tells the agent how to use them.

### The in-repo Skill

`.agents/skills/gtunnel/SKILL.md` follows the open Agent Skills standard. It tells an agent
(Claude Code, Gemini CLI and others) to:

- follow [Naming a tunnel](commands.md#naming-a-tunnel)
- route a client that picks its own destination (MongoDB Compass over SSH) through one bastion tunnel, not one per backend host
- scope every command to the right account
- verify before it reports done: the service status, the log for IAP codes such as `4033`, and the local port

### The native MCP Server

`gtunnel mcp` is a newline-delimited JSON-RPC 2.0 stdio server in the same binary: 14 tools and
2 live resources, no external runtime.

<p align="center">
  <img src="demo-mcp.gif" alt="Real MCP calls to gtunnel mcp: the tools, gtunnel_run_tunnel opening db on bastion:5432 for 30 minutes, gtunnel list showing the run and its stop time, gtunnel_stop_tunnel closing it" width="720">
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
 "target": "bastion:5432 (europe-west1-b)", "ttl_minutes": 5, "expires_at": 1793103233}
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

```text
{"jsonrpc":"2.0","id":1,"result":{"content":[{"text":"Account 'nope' not found","type":"text"}],"isError":true,"structuredContent":{"error":{"kind":"not_found","message":"Account 'nope' not found"}}}}
{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"'local_port' must be between 1 and 65535"}}
```

Nothing but JSON-RPC frames is ever written to stdout.

#### Live Resources

| URI | What it returns |
|---|---|
| `gtunnel://tunnels` | JSON array of all persistent tunnels |
| `gtunnel://accounts` | The `gtunnel_list_accounts` object: `selected_account` and `accounts`, plus `impersonate_service_account` and `expose` on an account that sets them |

`resources/read` of `gtunnel://accounts` returns the `gtunnel_list_accounts` object in
`contents[0].text`; a client that expects a bare array reads `accounts` from it:

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

```console
$ gtunnel mcp --validate
Validated 14 tools and 2 resources successfully.

```

To inspect tools, schemas, and test calls interactively using the official MCP Inspector:

```text
npx @modelcontextprotocol/inspector gtunnel mcp
```

### Client Configuration

#### One-line install

Each client registers the server with one command. Add `--scope user` to make it available in every project; the default scope is this project only.

| Claude Code | Gemini CLI |
|---|---|
| `claude mcp add gtunnel -- gtunnel mcp` | `gemini mcp add gtunnel gtunnel mcp` |

The `--` in the Claude Code command ends its own flags, so `mcp` reaches gtunnel.

Gemini CLI refuses to write project settings in a workspace it does not trust; trust the folder first or add `--scope user`.

#### Workspace Configuration (`.mcp.json`)

For a workspace, create `.mcp.json` in the project root:

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

| Client | Where the block goes |
|---|---|
| Claude Code | `.mcp.json` at the workspace root, discovered on start |
| Gemini CLI | `.gemini/settings.json` (workspace) or `~/.gemini/settings.json` (global) |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows) |

Environment variables, such as a config directory or a log level, go in `"env"`:

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

`list`, `account list` and `doctor` take `--json`. The `list` fields are in
[list](commands.md#start-stop-restart-autostart-logs-list-delete) and the failure object in
[Errors and exit codes](commands.md#errors-and-exit-codes):

```console
$ gtunnel account list --json
[
  {
    "name": "ci",
    "gcloud_configuration": "ci",
    "project": "example-dev-1a2b",
    "default_zone": "europe-west1-b",
    "selected": true
  }
]

```

```console
$ gtunnel doctor --json
[]

```
