# Troubleshooting

Start with `gtunnel doctor`. It reports everything below that can be detected, and prints
the exact command for the things it cannot fix itself.

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
