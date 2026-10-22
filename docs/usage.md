# Usage

Every command, with the output it prints. Outputs were captured from a real run unless the
section says the opposite.

`--account <name>` works on every command and overrides the default account. Paths in
captured output are shown at their default location rather than the throwaway directory the
run used.

## Command map

| Command | Purpose | Touches GCP |
|---|---|---|
| `init` | Import a gcloud configuration as a gtunnel account | read |
| `add` | Add a persistent Access tunnel | read |
| `expose` | Publish a local service at `https://<name>.<domain>` | write |
| `run` | Ephemeral tunnel in the foreground, gone on Ctrl+C | read, or write with `--public` |
| `start` / `stop` / `restart` | Service control | no |
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

## The TUI

`gtunnel` with no subcommand opens the TUI. `gtunnel demo` opens it filled with synthetic
data and never touches GCP or a config file.

| Key | Action | Key | Action |
|---|---|---|---|
| `j` `k` arrows | Move | `a` | Add a tunnel |
| `s` | Start | `e` | Edit the selected tunnel |
| `S` | Stop | `d` | Delete |
| `R` | Restart | `A` | Toggle auto-start |
| `r` | Refresh now | `c` | Copy the URL |
| `;` | Next account | `o` | Open the URL |
| `m` | Import an unmanaged run | `h` | Health check now |
| `?` | Help | `q` | Quit |

Unmanaged `gcloud start-iap-tunnel` processes started outside gtunnel appear in the list
tagged `[not managed by gtunnel]`. Every action on them is refused except `m`, which takes
them over.

## init

Reads a gcloud configuration and writes a gtunnel account. It prompts for everything it
cannot infer.

```
$ gtunnel init
gcloud configurations:
  1. default (active)  project=holded-app-dev-a612  account=gonzalo.cheliz@holded.com
Configuration number [1]: Account name [default]: Project [holded-app-dev-a612]:
Service account to impersonate (optional) []: Default zone: Error: A default zone is required
```

An account stores a gcloud configuration name, a project, a default zone, an optional
service account to impersonate, and the relay once one exists. gtunnel never stores a
credential: every token comes from `gcloud auth print-access-token` at the moment it is used.

## add

```
gtunnel add <NAME> <INSTANCE:PORT> [-z ZONE] [--local-port N] [-s]
gtunnel add <NAME> <HOST:PORT> --dest-group G --region R --network N [-s]
```

| Flag | Effect |
|---|---|
| `-z`, `--zone` | Zone of the instance; the account default otherwise |
| `--local-port` | Fixed local port; otherwise one is allocated from 15000-15999, stable per name |
| `--dest-group` | Target an IAP destination group instead of an instance |
| `-s`, `--start` | Install the service and start it immediately |

The local port is derived from the tunnel name, so the same name gets the same port on
every machine unless it is taken.

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

## start, stop, restart, logs, list, delete

```
$ gtunnel list
STATUS     NAME             MODE    URL                              TARGET
○ stopped  db               access  localhost:15432                  postgres-1:5432 (europe-west1-b)
● running  pid 59670        access  -                                private-cluster-bastion:22 (europe-west1-b)  [not managed by gtunnel]
```

```
$ gtunnel logs nope
Error: Tunnel 'nope' not found for account 'evidence'
```

`logs -f` follows. `restart` rewrites the service file before starting, so it also picks up
an edited target. `delete` removes the relay route first, then the service, then the record.

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
| `--fix` | Remove what can be removed safely |
| `-y`, `--yes` | Skip the confirmation that `--fix` asks for |
| `-a`, `--all` | Check every account, not only the selected one |

`--fix` only ever removes stale local records, dead ephemeral records, and relay resources
tagged `gtunnel:<account>` that no live tunnel claims. It re-checks that claim immediately
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
