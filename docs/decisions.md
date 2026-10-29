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

A credential file is referenced by path in `config.toml` and passed to gcloud per call; gtunnel never copies, stores or logs it, and `doctor` reads it only to check its type and mode.

The cost is a hard dependency on a working `gcloud` on `PATH` and a shell-out per
connection. In exchange, a stolen `tunnels.toml` is worth nothing, and revoking access is
something the user does in Google, not in gtunnel.

## 2. An Access tunnel creates nothing in GCP

**Status:** Accepted, 0.1.0

The common case is reaching one GCP instance port from a laptop. Anything gtunnel creates to do that
is a resource someone has to remember to delete.

An Access tunnel is a local process plus an IAP connection. No public IP, no port open to the
internet, no agent on the instance, no resource with a lifetime. The only ingress is a rule for
IAP's range, which `gtunnel doctor` prints the command for.

Expose mode cannot make the same promise — a URL needs a relay instance, a load balancer and a
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
[Naming a tunnel](commands.md#naming-a-tunnel).

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
every interface of the relay instance. It has to: the load balancer reaches it from another host,
and a loopback bind would leave the URL unreachable.

gtunnel allocates the port. It never opens or closes a firewall rule for one.

Leaving `30000-30999` open to the internet lets anyone who knows the relay address and a
port reach the forwarded service directly, skipping the load balancer and, with
`--private`, the IAP sign-in in front of it. The alternative — gtunnel managing firewall
rules — would mean a tool that silently widens a VPC. Documented in
[commands.md](commands.md#the-relays-firewall-is-yours-to-close).

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
| `cargo deny --locked check` | a vulnerable or unlicensed crate reaches users as a binary they trust |
| `cargo check --locked` on 1.89 | a declared `rust-version` that nothing checks rots on the next bump |
| `zizmor --offline` | the workflows hold a release token, so an injection there is the most expensive mistake available |

Every action is pinned to a commit SHA with `persist-credentials: false`, the workflow
token is read-only, and only the release job is granted `contents: write`. The release
build uses no cache, so a pull request run cannot decide what a release is built from.

## 11. Release assets are named for the machine, not the target triple

**Status:** Accepted, 0.1.0 (2026-10-24)

`gtunnel-aarch64-unknown-linux-musl.tar.gz` is exact and tells a visitor nothing: `unknown`
is the vendor slot of a Rust target triple, and `x86_64` is what LLVM calls amd64. A reader
has to know both conventions before they can pick their own file.

The four assets are `gtunnel-macos-arm64-v<version>.tar.gz`, `gtunnel-macos-intel-v<version>.tar.gz`,
`gtunnel-linux-amd64-v<version>.tar.gz` and `gtunnel-linux-arm64-v<version>.tar.gz`. Including the version at
the end prevents collisions on repeated browser downloads while preserving plain-language platform names.

The slug list now exists twice — as `slug` in the release matrix and as `platform_slug()`
in `src/update.rs` — and the updater fetches by it, so the two must change together. Both
sides carry a comment saying so. Renaming also broke `gtunnel update` for v0.1.0, which
shipped with triple names and no users.

## 12. Changes land through a pull request, squashed

**Status:** Accepted, 0.1.0 (2026-10-24)

The published history is what a reader of a public repository sees, and a branch's working
commits are not that.

The `main` ruleset requires a pull request, allows squash as the only merge method, and
requires six checks with the branch up to date: `test` on macOS and Ubuntu, `audit`,
`msrv`, `zizmor` (the gate of [decision 10](#10-the-ci-gate-is-six-checks-and-each-one-exists-for-a-reason))
and `pr-title`. A squash takes the pull request title as its commit message and the release train reads that
message, so the title is checked too. Deletion and non-fast-forward pushes are blocked.

The cost is that a one-line fix still needs a branch and a wait for CI. The benefit is that
every commit on `main` is one reviewed idea with a message that explains it, and that no
commit reaches `main` without the gate having run on that exact tree.

## 13. Documentation is a deliverable, not cleanup

**Status:** Accepted, 0.1.0

A feature whose behaviour is only discoverable by reading the source is not finished, and
documentation written later is written from memory.

`docs/` holds one file per category — usage, commands, configuration, gcp, architecture,
troubleshooting, design, decisions — and a change that alters behaviour updates the relevant one in the same pull
request. Command output in the docs is captured from a real run, not written by hand.

The cost is that a code-only pull request is rare. The alternative is a README that
describes a program that no longer exists.

## 14. Releases are computed and published automatically

**Status:** Accepted, 0.1.1 (2026-10-24), amended 0.1.14 (2026-10-26), amended 2026-10-27, amended 2026-10-28 (docs no longer release)

Deciding a version by hand means deciding it from memory, and a changelog written at
release time is written from the diff rather than from the intent.

One merge to `main` is one release. `release.yml` runs a train on the merge commit:

| Job | What it does |
|---|---|
| plan | `release-plz update` in `git_only` mode computes the next version from the conventional-commit subjects and rewrites `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`; nothing to release ends the run |
| ci | calls `ci.yml`, so the merged tree is tested once |
| draft | opens a draft release with the changelog section as notes |
| tag | commits the bump as `chore(release): vX.Y.Z` and pushes it with the annotated tag in one atomic push |
| verify | runs on the tag: it must point at a commit on `main` and name the version that commit declares |
| build, publish | four targets, a `.sha256` each, an attestation each, then the draft is published as latest |

Every merge that changes the binary, `feat`, `fix`, `perf` or `refactor`, cuts a release
(`release_commits` in `release-plz.toml`). A `docs` merge writes a changelog line but ships
identical binaries, so it cuts none: its line ships in the notes of the next release. A
merge of tests, CI or chores alone writes no line and changes no version. A merge that
changes only `Cargo.lock` is invisible to that filter, because release-plz drops
lockfile-only commits from the history it reads; it releases instead through the commit
release-plz adds when the binary's locked dependencies differ from the last tag, which the
filter names. A later merge on top of an unreleased lockfile change hides it again, so the
update then waits for the next release.

The bump is pushed with `RELEASE_TOKEN`, a fine-grained token with Contents: write on
this repository only. A push made with the workflow's own token starts no workflows, so
the tag would never build, and a pull request opened with it waits for a manual approval
of its CI. That approval and the second merge it led to are what the release pull request
cost, and why it is gone.

The Homebrew tap is another repository, `gcheliz/homebrew-tap`, so its formula is bumped by
a last `tap` job that runs once the release is public. It reads the four `.sha256` files the
build jobs wrote, rewrites the url and hash pairs in `Formula/gtunnel.rb` and pushes with
`HOMEBREW_TAP_TOKEN`, a second fine-grained token with Contents: write on the tap only, so
neither token reaches the other repository. A failed bump leaves the release as published and
the formula on the previous version, which `brew upgrade` reads as no update; re-running the
job retries it.

A tag is a request to sign and publish, so two checks stand in front of it. The
`release-tags` ruleset lets only an admin create, move or delete a `v*` tag, and the tag
run publishes only a tag that points at a commit on `main`, names the version that commit
declares, and has the draft the train opened. A tag pushed any other way builds nothing
that reaches users.

The costs: the commit prefix is load-bearing, so a `feat` typed as a `chore` ships nothing;
and a standing secret must be rotated before it expires, or the train fails at the tag job
with an error naming it.

## 15. A command is a core that returns a result and a printer that prints it

**Status:** Accepted, 2026-10-27

`start`, `stop`, `restart`, `add` and `delete` were functions that printed their outcome and
returned `Ok`. The MCP server called them, so their `println!` landed on the JSON-RPC stdout
and broke the client's parser, and a start that failed printed "failed to start" and returned
`Ok`, so the CLI exited 0, MCP reported success and the TUI said "starting".

Each now has a core in `commands/access.rs` that returns a typed result or an error, and a
`cmd_*` that prints it. The CLI, the MCP server and the TUI call the core, so the three
surfaces cannot disagree about what happened. A start that did not come up is the error
`StartFailed`, whose text is the same everywhere. `add` with a start saves the record first and
returns that error afterwards, so the record stays, as it did on the CLI.

The costs: a start now takes the three-second settle in the TUI too, and the CLI keeps the
failure lines on stdout but now exits 1, so a script that ignored the status starts seeing
failures it used to miss.

## 16. State files are written atomically under an advisory lock

**Status:** Accepted, 2026-10-27

`tunnels.toml` and `config.toml` were written with truncate-then-write and changed by
load, edit, save. A crash mid-write left an empty file that reads as "nothing configured", and
two gtunnels (the TUI and a CLI command, or the watchdog) that each loaded, edited and saved
dropped one another's change. Only some files were owner-only.

Every write goes through `fsutil::write_atomic`: a sibling temp file with mode 0600, then a
rename. Every change goes through `TunnelState::update` or `config::update_config`, which take
an exclusive `File::try_lock` on `tunnels.lock` or `config.lock`, load, run the closure and
save. The lock is polled for 5 seconds and then fails naming the lock file, so a wedged holder
is an error a person can act on and not a frozen TUI. The lock is a file of its own because
the rename replaces the data file's inode, and a lock on the old one would guard nothing.

The closure never awaits. A command that talks to gcloud or a service manager does that first
and changes the record afterwards: `delete` stops and uninstalls, then removes the record; `add`
verifies the instance, then saves. The cost of that order is a window in which the world has
changed and the record has not, which every one of those commands already had.

The minimum Rust version is 1.89 because `File::lock`, `try_lock` and `unlock` are stable from
there; the alternative was a dependency for one syscall. The costs: a package that builds with
an older compiler must update, and the lock is advisory, so an editor writing `tunnels.toml`
directly is not stopped by it.

## 17. The release binary is built for size

**Status:** Accepted, 2026-10-27

The release binary was 11.5 MB, and the release tarballs and `gtunnel update` download it
whole. Four profile keys and one TLS backend choice cut it to 4.2 MB, all under
`[profile.release]` and `Cargo.toml`, so `cargo install` builds the same binary.

| Step | Size, macOS arm64 |
|---|---|
| Before | 11.54 MB |
| `strip = "symbols"` | 9.42 MB |
| `lto = "fat"`, `codegen-units = 1` | 6.72 MB |
| `panic = "abort"` | 5.64 MB |
| rustls on `ring`, not `aws-lc-rs` | 4.20 MB |

TLS uses `ring`: reqwest is built with `rustls-no-provider` and `src/http.rs` installs the
provider, once, for every client. `ring` is 1.4 MB smaller than `aws-lc-rs`. The cost is no
post-quantum key exchange, which `aws-lc-rs` offers; gtunnel talks to Google and GitHub
endpoints that negotiate classical key exchange today.

`panic = "abort"` is safe here: nothing in the release build calls `catch_unwind` or inspects
a panic. The one guard that does, the `JoinError` fallback in `spawn_round` (`src/tui/mod.rs`),
runs only in test and debug builds, which unwind; in the release profile a panic ends the
process before it can fire. The panic hook that `ratatui::init` installs restores the
terminal before the process ends, and launchd and systemd restart a daemon that dies. A panic is now a crash, not an unwind.

`opt-level` stays 3. `"s"` would shrink the binary further, but the relay and probe paths are
not benchmarked, so the speed given up is unknown.

The costs: a clean release build takes about 166 s where it took 57 s (fat LTO and one
codegen unit), and a client built without `crate::http::client_builder` panics, which under
abort ends the process. `clippy.toml` forbids `reqwest::Client::new`, `Client::builder` and
`ClientBuilder::new`, so CI rejects that door.

## 18. A run is stopped like a service and known by its start time

**Status:** Accepted, 2026-10-27

The run record named gcloud by pid alone, `run` ended it with a lone SIGKILL that could orphan
ssh and the IAP proxy, and its TTL slept on the monotonic clock, which stops while the machine
is suspended. After a reboot a recycled pid read as a live run and could be sent a signal
(#114). `run` also accepted a name or local port already in use (#115).

| Change | Why |
|---|---|
| The supervisor's group stop is shared as `crate::proc::stop_group`, and it also sweeps a group that outlives its leader | One way to end gcloud for the daemon and for `run`: SIGTERM to the group, a 2 s grace, then SIGKILL; an ssh that ignores TERM after gcloud exits is still killed |
| The TTL compares the wall clock with `expires_at` every second | Time asleep counts, so a laptop that wakes past the stop time ends the run within a second |
| The record carries a start token read from the kernel; a pid whose token differs is dead and is never signalled | A pid alone is a number the kernel hands out again. macOS reads the fork time with `proc_pidinfo`, Linux reads `/proc/<pid>/stat` field 22 with the boot id, neither from the wall clock |
| Name, ports, spawn and record are claimed under `tunnels.lock` | Two launches cannot pick one port or one name. A public run therefore creates its route after gcloud starts |

The costs: gcloud's reverse forward is up for a few seconds before the hostname resolves,
which serves nothing; a record written by 0.1.42 or older has no token and is judged by its
pid alone until it ends; and the TTL lives in the run process, so a run process killed by
SIGKILL still leaves gcloud with nothing to end it.

## 19. A joined account-and-name string belongs to one tunnel

**Status:** Accepted, 2026-10-27

Log files, run records and the systemd unit are named `<account>-<name>`. Account `prod` with
tunnel `eu-api` and account `prod-eu` with tunnel `api` both join to `prod-eu-api`, so two
different tunnels would write one file (#132).

| Choice | Reason |
|---|---|
| Refuse the second name, for declared tunnels and for runs | The refusal costs one rename and changes no existing file |
| Keep `-` as the separator | A new separator renames every log and systemd unit on upgrade |
| Build every such name through `state::label_of` and `ephemeral::record_path` | One join, so the check and the writer cannot disagree |

The launchd label joins with `.` and is unaffected. A dead run does not block the name,
because it holds nothing.

## 20. Restarts belong to launchd and systemd, and readiness is a file

**Status:** Accepted, 2026-10-28, amended 2026-10-28 (the supervisor stops a job that spends its restart budget, #229)

A tunnel's supervisor runs gcloud once and exits with gcloud's status. Restarting it is the
service manager's job, with fixed settings, and gtunnel does not speak `sd_notify` (#209).

| Platform | Settings | What they do |
|---|---|---|
| macOS | `KeepAlive {SuccessfulExit false}`, `ThrottleInterval 10` | Restarts every non-zero exit, 10 s apart |
| Linux | `Type=simple`, `Restart=on-failure`, `RestartSec=5`, `RestartPreventExitStatus=78` | Restarts every failure, 5 s apart, except exit 78 |

| Question | Answer |
|---|---|
| A failure that restarting cannot fix | Expired credentials exit 0 on macOS and 78 on Linux, the one status each manager leaves alone, and write `run/<label>.auth` (#193) |
| A transient failure, such as a dropped network or VPN | Retried indefinitely; 5 s never reaches systemd's default start limit of 5 starts in 10 s |
| When the tunnel is ready | `run/<label>.ready`, written once gcloud logs that it is listening; `start` waits for it and `list` reads it on both platforms |

`Type=notify` was not adopted. It would show readiness only to someone reading raw
`systemctl --user status`, it does nothing on macOS, and it adds a failure: a gcloud that
never logs its listener is killed at `TimeoutStartSec` instead of staying up. It is worth
reopening when a client reads systemd readiness rather than the ready file.

The cost: a failure that is permanent but not about credentials, such as a wrong instance
name, is retried every 5 or 10 s until the user stops the tunnel.

### Amendment: the supervisor stops a job that spends its restart budget

Restarting is still the service manager's job, but a tunnel whose gcloud keeps failing for a
reason gtunnel does not recognise no longer loops forever. The supervisor counts fast
failures and, over budget, stops the job the way an expired credential does: it writes
`run/<label>.crash` with the last exit reason and exits 0 on macOS or 78 on Linux. The
tunnel then stays stopped until `gtunnel start`, which clears the marker and the count.
The supervisor never retries by itself after it stops, and `gtunnel watch` skips a job that
carries the marker. Credential expiry still wins: its marker and exit are unchanged.

| Setting under `[restart]` in `config.toml` | Default | Meaning |
|---|---|---|
| `max_restarts` | 5 | Fast failures tolerated inside `window`; `0` disables the budget |
| `window` | 10m | Failures older than this are forgotten |
| `min_uptime` | 60s | A run at least this long clears the count |
| `base_backoff` | 10s | Wait after the first failure; doubles on each next one |
| `max_backoff` | 5m | Cap on the wait |

| Question | Answer |
|---|---|
| Why not systemd `StartLimitBurst` | The supervisor owns the budget on both platforms. systemd's default (5 starts in 10 s) cannot trip at `RestartSec=5`, and macOS has no equivalent |
| Are wake restarts excluded | No. A wake restart follows a run longer than `min_uptime`, which clears the count, so they never accumulate (#230) |
| Is a user stop counted | No. SIGTERM is never counted and never writes the marker |

The cost: a tunnel that needs longer than `min_uptime` to come up and keeps failing is
judged fast-failing, so `min_uptime` must stay above gcloud's normal startup. The "retried
indefinitely" and "retried every 5 or 10 s" statements above hold for transient failures
that recover; a job that keeps failing now stops after `max_restarts`.
