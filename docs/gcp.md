# What GCP needs

[Home](../README.md) / [Usage](usage.md) / [Commands](commands.md) / [Configuration](configuration.md) / [Architecture](architecture.md) / [Troubleshooting](troubleshooting.md)

Every requirement gtunnel places on a GCP project, by mode. Find your mode, run the one-off
commands, then confirm with `gtunnel doctor`, which checks the IAP role and the firewall rule
for every tunnel. The rest of the roles surface as a permission error when the command that
needs them runs.

## At a glance

| Mode | You need | Creates in GCP |
|---|---|---|
| [Access](#access) | One role and one firewall rule | Nothing |
| [Jump](#jump) | The Access role, plus ssh on the bastion | Nothing |
| [Expose](#expose) | Seven roles, a Cloud DNS managed zone, a relay | A relay, a load balancer, two firewall rules, certificates, DNS records |
| [CI](#ci) | The Access role on a service account | Nothing |

## Access

A local port forwarded to a port on an instance or an IAP destination group
([the three target kinds](usage.md#three-kinds-of-access-target)).

| Need | Value | One-off command |
|---|---|---|
| Role | `roles/iap.tunnelResourceAccessor` on the instance, the destination group or the project | Grant it in IAM; [Google's steps](https://cloud.google.com/iap/docs/using-tcp-forwarding#grant-permission) |
| Role, to list instances and zones | `roles/compute.viewer` on the project | Same |
| Firewall | Ingress `tcp:<port>` from `35.235.240.0/20` to the target port | `gtunnel doctor` prints the exact command for your port; its shape is below |
| APIs gtunnel calls | `iap.googleapis.com` (the IAP check), `compute.googleapis.com` (instances, zones, firewall rules) | none |
| Login | `gcloud auth login`, see [Getting set up](usage.md#getting-set-up) | `gcloud auth login` |

```
gcloud compute firewall-rules create gt-allow-iap-<port> --project=<project> \
  --network=<network> --direction=INGRESS --allow=tcp:<port> \
  --source-ranges=35.235.240.0/20
```

Without the role a tunnel starts and then fails with `4033`; without the firewall rule it
fails with `4003`. [The error table](troubleshooting.md#what-the-log-says-and-what-it-means)
lists every close code. `gtunnel init` lists projects through
`cloudresourcemanager.googleapis.com`; a `403` naming that API is fixed in
[Could not list projects](troubleshooting.md#could-not-list-projects).

## Jump

A tunnel through a bastion to a host only the bastion reaches (`--via`). IAP carries an ssh
connection and ssh makes the forward ([how](architecture.md#jump-tunnels)).

| Need | Value | One-off command |
|---|---|---|
| Role | The Access roles | see [Access](#access) |
| Firewall | Ingress `tcp:22` from `35.235.240.0/20` to the bastion | The Access command with `<port>` set to `22` |
| On the bastion | A route to the target host | none; gtunnel opens nothing on the instance |

The forward's target (`10.0.0.10:443`, `db.internal:5432`) needs no rule from IAP: the
connection to it starts at the bastion.

## Expose

A local service published at `https://<name>.<domain>` through a relay instance and a global
HTTPS load balancer ([how](architecture.md#expose-mode)).

| Need | Value | One-off command |
|---|---|---|
| Role, IAP | `roles/iap.tunnelResourceAccessor` (the relay is reached over IAP ssh) | see [Access](#access) |
| Roles, relay | `roles/compute.instanceAdmin.v1`, `roles/compute.networkAdmin` on the project | none |
| Role, certificates | `roles/certificatemanager.owner` on the project | none |
| Role, DNS | `roles/dns.admin` on the managed zone | none |
| Role, `--private` | `roles/iap.admin` on the project | none |
| Cloud DNS managed zone | One zone for the domain; gtunnel never creates or deletes it | `gcloud dns managed-zones create my-zone --dns-name=example.com. --description="gtunnel zone"` |
| Relay | One per account: instance, instance group, load balancer, certificate map | `gtunnel relay up --managed-zone my-zone` |
| More domains | Each needs a certificate and a DNS authorization | `gtunnel domains add <zone>` |
| APIs gtunnel calls | `compute`, `dns`, `certificatemanager`, `iap`, `monitoring` (the TUI's request counts), each under `googleapis.com` | none |
| Cost | A global forwarding rule (about USD 18 a month) and an `e2-micro` instance (about USD 7 a month) | `relay up` asks before creating either |

### Firewall rules `relay up` creates

Both target the network tag `gt-relay`, which only the relay instance carries. A tunnel
creates no further rule. Resource names are in
[What belongs to whom](architecture.md#what-belongs-to-whom).

| Rule | Allows | From |
|---|---|---|
| `gt-<acct>-iap-ssh` | `tcp:22` | `35.235.240.0/20`, IAP |
| `gt-<acct>-lb-hc` | `tcp:30000-30999`, the relay ports | `130.211.0.0/22` and `35.191.0.0/16`, the load balancer and its health checkers |

An Expose tunnel that returns 502 with no error in the log is usually the second rule: the
health check cannot reach the relay port, so the backend has no healthy instance. A rule on
the same network that admits `30000-30999` from elsewhere skips the load balancer, and with
`--private` the sign-in too: [The relay's firewall is yours to close](commands.md#the-relays-firewall-is-yours-to-close).

## CI

A step with no terminal and no person to log in ([the three identities and a Cloud Build
step](usage.md#running-in-ci)).

| Need | Value | One-off command |
|---|---|---|
| Identity | The runner's attached service account, a workload identity federation file, or a key file named by `credential_file` | [Name a credential file](usage.md#name-a-credential-file) |
| Role | `roles/iap.tunnelResourceAccessor` on the project or the instance | see [Access](#access) |
| Firewall | Ingress from `35.235.240.0/20` to the target port | see [Access](#access) |
| Account | Declared from flags, no gcloud configuration to read | [Declare the account from flags](usage.md#declare-the-account-from-flags) |
| Impersonation | `roles/iam.serviceAccountTokenCreator` on the service account, granted to the identity that impersonates it | none |
