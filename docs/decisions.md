# Decisions

Why gtunnel is shaped the way it is. One section per decision, oldest first, each
recording what was true when the choice was made and what it costs.

A decision belongs here when reversing it would change the program's shape, its security
posture, or what a user has to type. A choice that only one function can see belongs in a
comment next to that function.

| | |
|---|---|
| Format | heading, status line, the constraint, the decision, the cost |
| Status | `Accepted` or `Superseded by N` — nothing is deleted, only superseded |
| Numbering | append only; a number is never reused |
| When to add one | in the pull request that makes the choice, not afterwards |

---

## 1. gtunnel stores no credential

**Status:** Accepted, 0.1.0

IAP needs a Google identity on every connection, and a tool that keeps one becomes a
target worth stealing.

Every token comes from `gcloud auth print-access-token` at connect time
(`src/gcp/gcloud.rs`). gtunnel holds no refresh token, no service-account key and no
password, and writes none to disk.

The cost is a hard dependency on a working `gcloud` on `PATH` and a shell-out per
connection. In exchange, a stolen `tunnels.toml` is worth nothing, and revoking access is
something the user does in Google, not in gtunnel.

## 2. An Access tunnel creates nothing in GCP

**Status:** Accepted, 0.1.0

The common case is reaching one VM port from a laptop. Anything gtunnel creates to do that
is a resource someone has to remember to delete.

An Access tunnel is a local process plus an IAP connection. No firewall rule, no public IP,
no agent on the VM, no resource with a lifetime.

Public mode cannot make the same promise — a URL needs a relay VM, a load balancer and a
certificate — so the two modes are documented with their costs side by side, and `relay
down` removes all of it. The asymmetry is deliberate, not an oversight.

## 3. Tunnels run as user services, not as a gtunnel daemon

**Status:** Accepted, 0.1.0

A tunnel has to outlive the terminal that started it, and something has to restart it when
ssh dies.

launchd on macOS and systemd `--user` on Linux supervise each tunnel, one job per tunnel,
with restart on failure and optional start at login. gtunnel writes the job definition and
then gets out of the way.

Writing a supervisor would have meant a daemon of our own to install, keep running, and
debug when it is the thing that broke. The cost is two platform back ends instead of one,
and the label `com.gtunnel.<account>.<name>` becoming part of the interface: rename a
tunnel and the job must follow, which is why `rename` is a command and not an edit to
`tunnels.toml`.

## 4. A local port is derived from the tunnel's name

**Status:** Accepted, 0.1.0

A port the user has to choose is a port they have to remember, and a port assigned in
sequence changes when an unrelated tunnel is deleted.

`name_hash` in `src/state.rs` maps the name into 15000-15999 for Access and 30000-30998 for
relay ports, then walks forward from there if the port is taken. The same name lands on the
same port on every machine.

The cost is that a collision makes one of the two tunnels land somewhere unexpected, and
that the ranges are now part of the contract: changing a base moves every port that was
not explicitly set.

## 5. A tunnel is named for what it reaches, never for the account

**Status:** Accepted, 0.1.0

Names are already scoped per account — `src/state.rs` rejects a duplicate only within an
account, and the service label carries the account — so an account segment in the name is
redundant everywhere it appears.

A name is `<platform>` or `<platform>-<service>`, lower case, up to 40 characters of
`[a-z0-9-]`. No `-dev`, `-prod` or `-staging`.

The payoff is that the same command works in both accounts. The cost is that two accounts
can only be told apart by their local ports, which is why those differ. Full rule:
[usage.md](usage.md).

## 6. One tunnel per target, not per destination behind it

**Status:** Accepted, 0.1.0

A tunnel forwards to one host, and some clients can pick their own destination once they
have a way in.

Where the client can choose — MongoDB Compass over an SSH tunnel, a SOCKS proxy — the
tunnel points at the bastion and the client holds the destination.

One tunnel per database would pin a shard hostname that is not guaranteed to be the
primary, which defeats replica-set failover. The cost is that the user has to know the
destination URI, which gtunnel does not store for them.

## 7. Documentation uses placeholders, never real configuration

**Status:** Accepted, 0.1.0

This repository is public. A set of individually harmless identifiers is an inventory of
someone's network.

Every project id, account, instance name, private address and internal hostname in
committed text is a placeholder: `example-dev-1a2b`, `you@example.com`, `bastion`,
`10.0.0.10`, `db.internal`. Zones and regions stay real, because a reader needs a valid one
to copy.

Real output for a doc is captured against a throwaway config through `GTUNNEL_CONFIG_DIR`,
and only the value under discussion is substituted — a transcript edited further is no
longer evidence. The cost is that examples read slightly abstract, and that a sweep is
required before every commit.

## 8. The relay's firewall belongs to the operator

**Status:** Accepted, 0.1.0

A public tunnel forwards with `ssh -R 0.0.0.0:<relay-port>:<target>`, so the port listens on
every interface of the relay VM. It has to: the load balancer reaches it from another host,
and a loopback bind would leave the URL unreachable.

gtunnel allocates the port. It never opens or closes a firewall rule for one.

Leaving `30000-30999` open to the internet lets anyone who knows the relay address and a
port reach the forwarded service directly, skipping the load balancer and, with
`--private`, the IAP sign-in in front of it. The alternative — gtunnel managing firewall
rules — would mean a tool that silently widens a VPC. Documented in
[usage.md](usage.md#relay).

## 9. Linux releases are static musl binaries

**Status:** Accepted, 0.1.0

A glibc-linked binary built on one distribution fails on an older one, and the error names
a symbol version rather than the real problem.

Both Linux targets build against musl and link statically. The macOS targets are ordinary
dynamic builds, because macOS has one libc.

The cost is a cross-compilation step in the release workflow and a larger binary.

## 10. The CI gate is six checks, and each one exists for a reason

**Status:** Accepted, 0.1.0

These binaries are what `gtunnel update` installs, so the gate protects users, not just the
branch.

| Check | Why it is separate |
|---|---|
| `cargo fmt --check` | a formatting-only diff must never ride along with a logic change |
| `cargo clippy --all-targets -- -D warnings` | carries the lint policy declared in `Cargo.toml` |
| `cargo test` | on ubuntu and macos, because the service back ends differ |
| `cargo audit --deny warnings` | a vulnerable crate reaches users as a binary they trust |
| `cargo check --locked` on 1.88 | a declared `rust-version` that nothing checks rots on the next bump |
| `zizmor --offline` | the workflows hold a release token, so an injection there is the most expensive mistake available |

Every action is pinned to a commit SHA with `persist-credentials: false`, the workflow
token is read-only, and only the release job is granted `contents: write`. The release
build uses no cache, so a pull request run cannot decide what a release is built from.

## 11. Release assets are named for the machine, not the target triple

**Status:** Accepted, 0.1.0 (2026-10-24)

`gtunnel-aarch64-unknown-linux-musl.tar.gz` is exact and tells a visitor nothing: `unknown`
is the vendor slot of a Rust target triple, and `x86_64` is what LLVM calls amd64. A reader
has to know both conventions before they can pick their own file.

The four assets are `gtunnel-macos-arm64`, `gtunnel-macos-intel`, `gtunnel-linux-amd64` and
`gtunnel-linux-arm64`. The triples still drive the build; only the published name changed.

The slug list now exists twice — as `slug` in the release matrix and as `platform_slug()`
in `src/update.rs` — and the updater fetches by it, so the two must change together. Both
sides carry a comment saying so. Renaming also broke `gtunnel update` for v0.1.0, which
shipped with triple names and no users.

## 12. Changes land through a pull request, squashed

**Status:** Accepted, 0.1.0 (2026-10-24)

The published history is what a reader of a public repository sees, and a branch's working
commits are not that.

The `main` ruleset requires a pull request, allows squash as the only merge method, and
requires all five CI checks with the branch up to date. Deletion and non-fast-forward
pushes are blocked.

The cost is that a one-line fix still needs a branch and a wait for CI. The benefit is that
every commit on `main` is one reviewed idea with a message that explains it, and that no
commit reaches `main` without the gate having run on that exact tree.

## 13. Documentation is a deliverable, not cleanup

**Status:** Accepted, 0.1.0

A feature whose behaviour is only discoverable by reading the source is not finished, and
documentation written later is written from memory.

`docs/` holds one file per category — usage, architecture, troubleshooting, design,
decisions — and a change that alters behaviour updates the relevant one in the same pull
request. Command output in the docs is captured from a real run, not written by hand.

The cost is that a code-only pull request is rare. The alternative is a README that
describes a program that no longer exists.

## 14. Releases are computed automatically and tagged by hand

**Status:** Accepted, 0.1.1 (2026-10-24)

Deciding a version by hand means deciding it from memory, and a changelog written at
release time is written from the diff rather than from the intent.

`release-plz` runs on every merge to `main` in `git_only` mode — crates.io is not
available to this project, so versions come from git tags rather than the registry. It
computes the next version from the conventional-commit subjects, rewrites `Cargo.toml`,
`Cargo.lock` and `CHANGELOG.md`, and keeps one open pull request holding the bump. A
maintainer merges it and pushes the tag; the tag starts `release.yml`, which builds, signs
and publishes.

The tag is the one manual step, and it is manual for a mechanical reason: a tag pushed
with `GITHUB_TOKEN` does not trigger another workflow, so an automated tag would publish a
release with no binaries in it. Closing that gap means a GitHub App or a long-lived
personal token with push rights, stored in this repository — a standing credential in a
project whose first decision was to hold none. One command is a cheaper price.

The cost is that the commit prefix is now load-bearing: a `feat` typed as a `chore` ships
without a version bump and without a changelog line.
