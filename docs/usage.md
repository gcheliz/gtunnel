# Usage

Every command, with the output it prints. Outputs were captured from a real run unless the
section says the opposite.

`--account <name>` works on every command and overrides the default account. Paths in
captured output are shown at their default location rather than the throwaway directory the
run used, and the instance names of tunnels gtunnel only observed are replaced with generic
ones.

## Command map

| Command | Purpose | Touches GCP |
|---|---|---|
| `init` | Import a gcloud configuration as a gtunnel account | read |
| `add` | Add a persistent Access tunnel | read |
| `expose` | Publish a local service at `https://<name>.<domain>` | write |
| `run` | Ephemeral tunnel in the foreground, gone on Ctrl+C | read, or write with `--public` |
| `start` / `stop` / `restart` | Service control | write, for a public tunnel only |
| `logs` | Read a tunnel's log | no |
| `list` | Every tunnel, managed or not | no |
| `delete` | Remove a tunnel and its route | write for public tunnels |
| `zones` / `domains` | Defaults for new tunnels | read |
| `relay up` / `down` / `status` | The shared relay for an account | write |
| `account` | Accounts | no |
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
| `gtunnel` (the TUI), `add`, `expose`, `run`, `start`, `stop`, `restart`, `autostart`, `logs`, `list`, `delete`, `zones`, `domains`, `relay` | `init`, `account`, `doctor`, `reset`, `demo`, `update` |

`doctor` is skipped because it reports broken credentials as a finding and a login prompt
would hide it; `init` has no account yet. Background services run `gcloud` directly and
never go through this check.

On a terminal, a failed probe runs `gcloud auth login --configuration=<configuration>` with
the terminal handed to it, then probes again. Only the second probe counts: a login that
exits 0 and still cannot mint a token fails like any other. The sample below is
illustrative, since the login output belongs to gcloud.

```
$ gtunnel list
gcloud has no usable credentials for 'prod'. Starting `gcloud auth login`.
Your browser has been opened to visit: ...
You are now logged in as [you@example.com].
NAME  STATUS  ...
```

When the login is cancelled, fails, or there is no terminal (stdin or stdout is redirected,
as under cron or CI), gtunnel exits 1 without starting anything.

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
| This tunnel's URL and log | `c` copy the URL, `o` open it, `L` clear the log, `r` refresh the list, metrics and health |
| The program | `;` cycle accounts, `?` the keymap, `C` the config page, `q` quit, `Esc` close a page or cancel a prompt, `PgUp` `PgDn` and `g` `G` scroll the keymap page, `Ctrl+Z` suspend, `Ctrl+C` quit from anywhere |

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

Details repeats the project under the `target` row it qualifies. The header carries it
once as a property of the account, at the far edge of the screen from the host it belongs
to; the pane open while a `gcloud` command of your own is being typed is the one that has
to answer which project that host is in. An account with no project prints no row, the
same silence the header keeps.

Unmanaged `gcloud start-iap-tunnel` processes started outside gtunnel appear in the list
tagged `[not managed by gtunnel]`. Every action on them is refused except `m`, which takes
them over.

The list is a table with a `NAME  MODE  URL` header. To keep the columns aligned it hides a
name suffix equal to the tunnel's own account (`k8s-dev` under `dev` reads `k8s`). The name
column is as wide as the longest name; it shrinks, to no fewer than 8 cells and with a `…`,
only when the pane could not otherwise hold a 12-cell URL. The Details pane starts with the
full stored name. `gtunnel list` and every command keep using the full names, so `gtunnel
start k8s-dev` is still the command.

The layout follows the terminal. From 121 columns the list is the left 46% and Details, Logs
and Metrics are stacked on the right; below 121 the list takes the full width above them,
which keeps the URL readable. The list scrolls only when the selected tunnel would leave the
window, so moving inside it moves the highlight and not the rows, and a
Details value too long for its row (a URL, a target, how the tunnel was started) wraps under
its label instead of being cut. When the terminal is short, Logs keeps at least 4 rows and
Metrics shrinks, then disappears, before Logs does.

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
│ account_name = "evidence"                                                │
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
│ account_name = "evidence"                                                │
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
puts IAP in front of the load balancer, so a visitor must sign in with Google.

Without a relay the command stops before changing anything:

```
$ gtunnel expose web 8000
Error: No relay for account 'evidence'. Run `gtunnel relay up`
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

```
$ gtunnel logs nope
Error: Tunnel 'nope' not found for account 'evidence'
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
No relay for account 'evidence'. Run `gtunnel relay up`
```

## account

```
$ gtunnel account list
* evidence         project=demo-project             zone=europe-west1-b
```

`account select <name>` sets the default, `account remove <name>` deletes it. The star marks
the default.

## doctor

Reports what gtunnel can see without changing anything.

```
$ gtunnel doctor
account evidence:
  ! gcloud has no usable credentials for account 'evidence'. Run `gcloud auth login`
```

```
$ gtunnel doctor          # with a gcloud whose Python has no numpy
gtunnel:
  ! gcloud's Python is missing numpy, which makes IAP tunnels slow. Run `gcloud components reinstall`
account evidence:
  ! gcloud has no usable credentials for account 'evidence'. Run `gcloud auth login`
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

## reset

Removes every local trace: services, config, state, logs. It never deletes anything in GCP.

```
$ gtunnel reset
This removes 1 local tunnel(s), 1 account(s) and /Users/you/Library/Application Support/gtunnel
Remove all local gtunnel configuration? [y/N] [n]: Cancelled
```

```
$ gtunnel reset -y
This removes 1 local tunnel(s), 1 account(s) and /Users/you/Library/Application Support/gtunnel
Removed /Users/you/Library/Application Support/gtunnel
Cloud resources were not deleted. Run gtunnel relay down first if you want them gone.
```

```
$ gtunnel reset -y
Nothing to remove: /Users/you/Library/Application Support/gtunnel does not exist
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
gtunnel v9.9.9 is available (current: v0.1.0). Run `gtunnel update`.
```

The check runs at most once a day, in the background, and never blocks a command. An install
from Homebrew or cargo is detected and `update` prints the right upgrade command instead of
replacing the binary.
