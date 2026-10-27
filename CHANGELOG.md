# Changelog

Notable changes per release. Dates are the release date; an unreleased section is
what is on `main` and not yet tagged.

## [0.1.26](https://github.com/gcheliz/gtunnel/compare/v0.1.25...v0.1.26) - 2026-10-27

### Fixed

- *(state)* write tunnels.toml, config.toml and run records atomically under a lock ([#74](https://github.com/gcheliz/gtunnel/pull/74))

## [0.1.25](https://github.com/gcheliz/gtunnel/compare/v0.1.24...v0.1.25) - 2026-10-27

### Fixed

- *(mcp)* keep stdout pure and report failed starts as failures ([#71](https://github.com/gcheliz/gtunnel/pull/71))

## [0.1.24](https://github.com/gcheliz/gtunnel/compare/v0.1.23...v0.1.24) - 2026-10-27

### Fixed

- *(daemon)* treat a systemd unit that is not loaded as already stopped

## [0.1.23](https://github.com/gcheliz/gtunnel/compare/v0.1.22...v0.1.23) - 2026-10-27

### Documentation

- document installing Rust and a C compiler for the cargo routes ([#72](https://github.com/gcheliz/gtunnel/pull/72))

## [0.1.22](https://github.com/gcheliz/gtunnel/compare/v0.1.21...v0.1.22) - 2026-10-27

### Documentation

- link the landing page from the README and require conventional PR titles ([#70](https://github.com/gcheliz/gtunnel/pull/70))

## [0.1.21](https://github.com/gcheliz/gtunnel/compare/v0.1.20...v0.1.21) - 2026-10-27

### Documentation

- re-record the TUI demo with the edit, add and accounts flows ([#67](https://github.com/gcheliz/gtunnel/pull/67))

## [0.1.20](https://github.com/gcheliz/gtunnel/compare/v0.1.19...v0.1.20) - 2026-10-27

### Fixed

- *(discovery)* stop listing a tunnel gtunnel just started as unmanaged ([#66](https://github.com/gcheliz/gtunnel/pull/66))

## [0.1.19](https://github.com/gcheliz/gtunnel/compare/v0.1.18...v0.1.19) - 2026-10-27

### Fixed

- *(release)* ship lockfile-only dependency updates ([#64](https://github.com/gcheliz/gtunnel/pull/64))

## [0.1.18](https://github.com/gcheliz/gtunnel/compare/v0.1.17...v0.1.18) - 2026-10-27

### Documentation

- sync version references to 0.1.17 and check them in CI ([#52](https://github.com/gcheliz/gtunnel/pull/52))

## [0.1.17](https://github.com/gcheliz/gtunnel/compare/v0.1.16...v0.1.17) - 2026-10-26

### Added

- *(mcp)* add native mcp server, agent skill, and json cli output ([#50](https://github.com/gcheliz/gtunnel/pull/50))

## [0.1.16](https://github.com/gcheliz/gtunnel/compare/v0.1.15...v0.1.16) - 2026-10-26

### Added

- *(cli)* allow all config commands unauthenticated and deduplicate workflows ([#48](https://github.com/gcheliz/gtunnel/pull/48))

## [0.1.15](https://github.com/gcheliz/gtunnel/compare/v0.1.14...v0.1.15) - 2026-10-26

### Added

- *(update)* allow self-update to replace cargo-installed binaries directly

### Documentation

- clarify that cargo installs self-update directly

## [0.1.14](https://github.com/gcheliz/gtunnel/compare/v0.1.13...v0.1.14) - 2026-10-26

### Added

- *(ci)* automate release tagging and include version in release assets ([#45](https://github.com/gcheliz/gtunnel/pull/45))

## [0.1.13](https://github.com/gcheliz/gtunnel/compare/v0.1.12...v0.1.13) - 2026-10-25

### Added

- *(cli)* standardize command error messages and document interactive prompt flows ([#43](https://github.com/gcheliz/gtunnel/pull/43))

## [0.1.12](https://github.com/gcheliz/gtunnel/compare/v0.1.11...v0.1.12) - 2026-10-25

### Fixed

- improve health check responsiveness and log filtering ([#42](https://github.com/gcheliz/gtunnel/pull/42))

## [0.1.10](https://github.com/gcheliz/gtunnel/compare/v0.1.9...v0.1.10) - 2026-10-25

### Fixed

- *(tui)* eliminate latency sparkline gaps, log origin endpoints, and add copy toast ([#36](https://github.com/gcheliz/gtunnel/pull/36))

## [0.1.9](https://github.com/gcheliz/gtunnel/compare/v0.1.8...v0.1.9) - 2026-10-25

### Documentation

- synchronize commands, keybindings, paths, and output references ([#34](https://github.com/gcheliz/gtunnel/pull/34))

## [0.1.8](https://github.com/gcheliz/gtunnel/compare/v0.1.7...v0.1.8) - 2026-10-25

### Added

- *(tui)* bind l to copy tunnel log to clipboard and quiet jump probe noise ([#32](https://github.com/gcheliz/gtunnel/pull/32))

## [0.1.7](https://github.com/gcheliz/gtunnel/compare/v0.1.6...v0.1.7) - 2026-10-25

### Fixed

- *(logfilter)* drop iap websocket teardown tracebacks and routine connection info lines ([#30](https://github.com/gcheliz/gtunnel/pull/30))

## [0.1.6](https://github.com/gcheliz/gtunnel/compare/v0.1.5...v0.1.6) - 2026-10-25

### Fixed

- *(tui)* guard s and S keybindings by status and ignore stdin proxy commands ([#28](https://github.com/gcheliz/gtunnel/pull/28))

## [0.1.5](https://github.com/gcheliz/gtunnel/compare/v0.1.4...v0.1.5) - 2026-10-25

### Added

- *(daemon)* supervise tunnel processes with output filtering and timestamping ([#26](https://github.com/gcheliz/gtunnel/pull/26))

## [0.1.4](https://github.com/gcheliz/gtunnel/compare/v0.1.3...v0.1.4) - 2026-10-25

### Added

- ~/.gtunnel configuration directory path with backwards-compatible fallback ([#22](https://github.com/gcheliz/gtunnel/pull/22))
- SESSION PATHS section in the `?` help modal displaying active config, tunnels, and logs paths ([#22](https://github.com/gcheliz/gtunnel/pull/22))
- interactive confirmation prompt before launching `gcloud auth login` on unauthenticated startup ([#22](https://github.com/gcheliz/gtunnel/pull/22))

## [0.1.3](https://github.com/gcheliz/gtunnel/compare/v0.1.2...v0.1.3) - 2026-10-24

### Added

- a watchdog that restarts a wedged tunnel, and gives up before it thrashes ([#20](https://github.com/gcheliz/gtunnel/pull/20))
- tunnels get descriptor headroom and explain it when they run out ([#19](https://github.com/gcheliz/gtunnel/pull/19))
- one log level for the tunnels and the gcloud calls ([#18](https://github.com/gcheliz/gtunnel/pull/18))

## [0.1.2](https://github.com/gcheliz/gtunnel/compare/v0.1.1...v0.1.2) - 2026-10-24

### Added

- the help frame names the running version, so a bug report can quote it
  ([#15](https://github.com/gcheliz/gtunnel/pull/15)). Press `?`; the title reads
  `Keys - gtunnel 0.1.2`, and sheds the version below 28 columns rather than clipping it

### Documentation

- the version table described a 1.0 project, and this one is not ([#17](https://github.com/gcheliz/gtunnel/pull/17))
- the cargo uninstall recovery hits an ambiguity, so show it ([#16](https://github.com/gcheliz/gtunnel/pull/16))
- installation recovery, a full config reference, and a release tag gate ([#15](https://github.com/gcheliz/gtunnel/pull/15))
- say what gh attestation verify actually prints ([#11](https://github.com/gcheliz/gtunnel/pull/11))

## v0.1.1 - 2026-10-24

### Installing and proving what you installed

- **Every release asset carries a build attestation** - a `.sha256` proves the download
  arrived intact, not who produced it, since whoever could swap the tarball could swap the
  checksum beside it. Each asset is now signed by GitHub with a short-lived certificate
  tied to this repository's Release workflow and recorded in a public transparency log.
  `gh attestation verify <file> -o gcheliz` checks it. Attestations begin with v0.1.1.
- **`gtunnel update` no longer sends a cargo install to crates.io** - the `gtunnel` name
  there belongs to an unrelated crate by another author, so the old hint would have
  installed a stranger's binary over this one. It now names the repository:
  `cargo install --git https://github.com/gcheliz/gtunnel --force`.
- **Installing is documented for four routes** - a published binary through a URL that
  carries no version, `cargo install --git`, `cargo install --path`, and `gtunnel update`.

### Project documentation

- **`CONTRIBUTING.md`** - what to install, the six CI gate commands to run before pushing,
  the placeholder rule for a public repository and how to capture real output without
  breaking it, commit and pull-request conventions, and which doc a given change updates.
- **`docs/decisions.md`** - thirteen records covering the choices that would otherwise be
  re-litigated: no stored credential, Access mode creating nothing in GCP, user services
  instead of a daemon, ports derived from the tunnel name, naming, the operator owning the
  relay firewall, static musl builds, the shape of the CI gate, and squash-only merges.
  Each says what it costs, not only what was chosen.

### Accounts, renaming and moving

- **`gtunnel rename` renames the tunnel, not just the record** - a name is also the
  service label and the log filename, so editing `tunnels.toml` by hand left an orphaned
  plist and a log nothing would ever write to again. The command stops the tunnel,
  rewrites the record, carries the log across and reinstalls the service under the new
  label. A rename that fails partway says where the tunnel actually is rather than
  leaving the caller to guess.
- **`gtunnel move` hands an Access tunnel to another account** - the global `--account`
  names the source and `--to` the destination, so neither is inferred from the default.
  It verifies the destination before the first write: an instance that does not exist
  over there costs nothing and the error says `nothing was changed`. Public tunnels are
  refused, because a URL, its certificate and its load-balancer rules belong to the
  account that created them, and a half-moved URL serves a 503 to real visitors.
- **A name collision asks instead of cancelling** - names are scoped per account, so a
  move can land on one the destination already holds. The CLI takes `--rename` up front;
  the TUI does not require it to be predicted, and advances to a rename prompt with the
  reason on screen at the moment the clash is found.
- **An account that still holds tunnels cannot be removed** - and the refusal names both
  ways out, `gtunnel delete` and `gtunnel move`, rather than offering a force flag. The
  flag would strand installed services whose account no longer exists, which is the one
  state nothing in gtunnel can clean up afterwards. The TUI's accounts page answers to
  the same rule: its own confirmation does not override it.
- **Accounts are managed from the TUI** - `:`, the shift of `;`, opens a page that adds,
  edits, selects and removes them. `account add` reads the project and default zone from
  an existing gcloud configuration, so an account is never a second place to type what
  gcloud already knows.
- **`e` chooses what to edit** - target and ports, rename, or move, rather than spending
  two more top-level keys on a keymap that already binds 31. Both new options refuse an
  unmanaged row and an ephemeral run that has not been imported with `m`.

### Logs

- **A running tunnel's log now shows that it is running** - four causes, all fixed.
  gcloud is Python and block-buffered its stdout into the redirected log, so lines
  arrived 4 KiB at a time or at exit; `PYTHONUNBUFFERED=1` ends that. gcloud's
  per-connection lines are `log.info` and the default verbosity is `warning`, so a tunnel
  carrying traffic said nothing; every command now passes `--verbosity=info`. The `--via`
  and public kinds run `gcloud compute ssh`, which gcloud's verbosity does not govern and
  which is silent once connected, so both also pass `-o LogLevel=DEBUG` - not `VERBOSE`,
  which is one level short of every line worth having. `New connection from`, the line
  this was measured against, does not exist anywhere in gcloud 590.0.0.
- **gtunnel writes what only gtunnel knows** - the connection count it already measured
  every 5 seconds reached the Metrics pane and nothing else. A change in it is now a log
  line, and so is a health transition, which until now existed only as a desktop
  notification that was gone once dismissed. Both are written only while a gtunnel
  process runs, and `docs/troubleshooting.md` says so: a gap in the counts means nobody
  was watching, not that the tunnel was idle. The count is an Access and jump fact - a
  public tunnel has no local port of its own to count - and the same document says that
  too. An op that empties a log also drops the count that belonged to it, so a restart
  no longer reports a drop that nobody caused.
- **A talkative log is a bounded one** - past 8 MiB gtunnel keeps the last 2 MiB and
  says on the first line what it dropped, and the TUI reads the end of a log rather
  than all of it to show the last 200 lines.

### TUI

- **A config page** - `C` shows `config.toml` and `tunnels.toml` with their paths, and `e`
  edits the one on screen in `$VISUAL`, then `$EDITOR`, then `vi`. The editor works on a
  private copy, which replaces the real file only after it parses and passes the rules
  `gtunnel add` enforces; a refused edit keeps its copy for `e` to reopen and `Esc` to
  discard. A saved edit reloads accounts, projects, the relay and the list without
  restarting, and says what changed. Running tunnels keep their old settings until `R`.

- **`;` switches the relay with the account** - the Public flow and the domain picker
  followed the account the TUI started on, so an account without a relay offered one and an
  account with one refused it. Both now change with the name and the project.

### Fixed

- **A recorded pid of 0 is never signalled** - `kill(0)` signals the caller's own process
  group. A run record holding pid 0, or one too large for a pid, now reads as not running
  and `reset` and the import handover skip it; a run whose `gcloud` has no pid fails to
  start instead of recording one.

### Build

- **A lint policy in `Cargo.toml`** - `unsafe_op_in_unsafe_fn`,
  `clippy::undocumented_unsafe_blocks`, `dbg_macro`, `todo` and `unimplemented` are
  declared once under `[lints]`, so CI and editors agree. Every `unsafe` block now carries
  the invariant that makes it sound.
- **Workflow linting in CI** - a `zizmor` job checks the workflows for template injection,
  broad permissions, unpinned actions and persisted credentials. Checkouts no longer keep
  the token, CI jobs are read-only, and the release build no longer restores a build cache.

### Documentation

- **The README points at `docs/` instead of restating it** - it carried the full keymap, the
  full command map and the full error table, three tables that `docs/usage.md` and
  `docs/troubleshooting.md` already own, and each copy drifted on its own schedule. The
  README now orients a newcomer and links; every table it dropped exists in exactly one file
  under `docs/`. 243 lines down to 149.
- **Four diagrams for what the prose only described** - `docs/architecture.md` gains the jump
  tunnel's second hop (`compute ssh --tunnel-through-iap`, which is a different command from
  `start-iap-tunnel` and has no local listener, so nothing prints `Listening on port`), what
  `relay up` creates against what `relay down` removes and in which order, the service
  lifecycle across launchd and systemd with the two asymmetric auto-start switches, and the
  account model that lets `kubernetes` exist once per account.
- **The banner keeps the mark's gaps** - `docs/logo.svg` drew each braille dot as a
  rounded square as wide as the pitch, so the squares met, the full-width row merged into
  the cloud, and the mark read as one bottom-heavy blob. It is one circle per dot again, as
  braille separates them: the gaps are what keep that row reading as a line behind the
  cloud. The silhouette is unchanged - all 48 dots and their hues are still pinned to
  `mark_dots()` - and `docs/logo.png` is re-rendered from it.
- **The fallback lost its notch** - `MARK_FALLBACK` carried `▀` at its centre, an
  upper-half block in a shape that has no cell with empty lower rows, so it drew a hole
  under the cloud on every terminal without braille coverage. Each cell is now the honest
  reduction of its braille cell, and the test bars `▀` by name.
- **A security policy** - `SECURITY.md` names the supported version, routes reports through
  GitHub's private vulnerability reporting rather than a public issue, and says what gtunnel
  holds: no stored credential, a fresh token per call from `gcloud auth print-access-token`,
  and nothing secret in `config.toml` or `tunnels.toml`.
- **Badges that a private repository can back** - the CI badge is gone. GitHub's image proxy
  fetches anonymously, so every dynamic badge against a private repository renders as a broken
  image; the workflow badge returned 404 while the static ones returned 200. In its place are
  static badges for the version, the Rust floor, the edition, the security policy and the
  licence, and a line naming what CI actually runs. **The version badge is written by hand and
  has to be bumped in `README.md` as part of cutting a release.** The CI badge can come back
  the day the repository goes public.

## v0.1.0 - 2026-10-24

First release. gtunnel is a port of [ytunnel](https://yetidevworks.com/ytunnel) to GCP
Identity-Aware Proxy: the same two modes and the same TUI-first shape, with IAP and a
GCP relay in place of Cloudflare Tunnel.

### Access mode

- **IAP port forwarding as a managed service** - `gtunnel add db my-vm:5432` gives a
  local port that reaches a VM inside the VPC, supervised by launchd or systemd so it
  survives a crash and, optionally, a reboot.
- **Destination groups** - `--dest-group` targets an IAP destination group, which
  reaches a host that is not a compute instance.
- **Stable local ports** - the port is derived from the tunnel name, so the same tunnel
  gets the same port on every machine unless it is taken.
- **Nothing is created in GCP** - Access mode makes no API call beyond reading, so a
  tunnel costs nothing and leaves nothing behind.

### Public mode

- **A public HTTPS URL for a local service** - `gtunnel expose web 3000` publishes
  `https://web.<domain>` through a relay VM, a global load balancer and Cloud DNS.
- **One relay per account** - the VM, load balancer, certificate map and firewall rules
  are created once by `gtunnel relay up` and shared by every tunnel on the account.
- **Managed certificates** - `gtunnel domains add` requests a certificate for the zone
  and its wildcard and writes the DNS authorization record.
- **IAP in front of the load balancer** - `expose --private` requires a Google sign-in
  before a visitor reaches the tunnel.
- **Self-healing routes** - starting a public tunnel re-creates its backend, host rule,
  named port and DNS record if any of them went missing, so `restart` fixes a tunnel
  that comes up serving a 503.

### TUI

- **Live dashboard** - tunnel list, details, logs and metrics in one screen, with
  latency, connection counts and load-balancer request classes.
- **Full lifecycle from the keyboard** - add, edit, start, stop, restart, delete,
  auto-start, copy and open, without leaving the TUI.
- **Unmanaged tunnels** - a `gcloud start-iap-tunnel` process started outside gtunnel
  is listed and read-only. It is named for the target and port it holds rather than
  for its pid, which is the one fact about it nothing can act on; the pid returns only
  to tell two strangers on the same target apart. Every action on such a row is
  refused, the footer offers none of them, the refusal is on screen rather than
  silent, and Details credits the process to whoever started it instead of to gtunnel.
- **A mark and a header** - a cloud resting on a line, drawn in braille and banded in
  the four Google Cloud hues, with the platform beside the mark and the product beside
  the name. The art is one fixed size, because a braille glyph scaled up reads as a loose
  matrix of dots rather than a cloud. One module decides every colour the TUI uses, in
  named ANSI so it follows the terminal's own palette; the mark's hues are the documented
  exception.
- **Details** - beside the stored name, url and target, the pane says which GCP project
  the target is in, when the tunnel started, and whether anything brought it up without
  being asked - a launchd revival after a crash reads differently from a start.
- **List columns** - the tunnel list has a `NAME  MODE  URL` header and fixed columns,
  including one for the auto-start and health flags. It hides a name suffix equal to the
  tunnel's account; Details shows the full name, and `gtunnel list` is unchanged.
- **Resizing** - every pane follows the terminal. The name column is as wide as the
  longest name and shrinks only when the URL would otherwise be cut; below 121 columns
  the list stacks above the other panes instead of sharing a row with them; the list
  scrolls only when the selection leaves the window; long Details values wrap; and Logs
  keeps a minimum height, so it is still on screen at 24 rows.
- **Stacked from 121 columns down** - at 100 to 120 columns the list now takes the full
  width above the other panes, because its 46 percent share cut a 28-cell URL at 100 and
  110 columns and a 49-cell URL at 120.
- **Account field** - the header shows the active account and its GCP project id as a
  two-row filled field, so the project a key will act on is on screen. `;` switches both
  together. A long project id is cut with a `…` before the account name is ever shortened,
  and an account with no project shows the name alone.
- **Field colour** - the account field is filled with a shade of its own, one step lighter
  than the selected list row, so the header no longer reads as a second selection.
- **Keymap page and states** - `?` opens every bound key in four groups, two columns when
  the terminal is wide and one when it is not, with a note on the three case-sensitive
  pairs. A failed tunnel's footer leads with `R restart` in the warning colour, and an
  empty account says `no tunnels in this account` under the list header, and `loading`
  until its first load has answered, or `could not load tunnels` if that load failed. A
  page taller than the terminal scrolls with `j`/`k`, the arrows, `PgUp`/`PgDn` and
  `g`/`G`, and says where the reader is in it.
- **Dialogs that fit the terminal** - the add, edit, delete and mode dialogs and the
  instance and domain pickers are sized to their text and the terminal instead of a
  percentage of it, so a prompt wraps instead of being cut, the `Esc to cancel` row is on
  screen at 80x24, a long value keeps its end and the cursor in view, and a picker always
  shows the selected row.
- **Health notifications** - a tunnel going unhealthy raises a system notification.
- **`gtunnel demo`** - the whole dashboard filled with synthetic data, touching no GCP
  project and no config file.

### Operations

- **Ephemeral runs** - `gtunnel run` holds a tunnel in the foreground and tears down
  every route it created on exit, including on Ctrl+C and on a crash.
- **`gtunnel doctor`** - reports stale records, dead runs, orphaned relay resources,
  missing IAP firewall rules, broken credentials and a gcloud Python without NumPy.
  `--fix` removes only what is tagged `gtunnel:<account>` and unclaimed.
- **Ownership by tag, not by name** - every resource gtunnel creates carries
  `description: gtunnel:<account>`, so a hand-made resource whose name matches the
  scheme is never deleted.
- **Multiple accounts** - one gcloud configuration, project, zone and relay per account.
- **Self-update** - `gtunnel update` verifies the published SHA-256 before replacing the
  binary, and defers to Homebrew or cargo when that is how it was installed.

### Startup

- **Login check** - gtunnel probes the selected account's gcloud credentials before the
  TUI or any GCP command starts, runs `gcloud auth login` on a terminal when they are
  missing or expired, and exits 1 if the account is still not logged in afterwards.
  Without a terminal it exits 1 instead of prompting. `doctor`, `account`, `init`,
  `reset`, `demo` and `update` skip it.

### Platforms

- **macOS** - launchd user agents. Auto-start is the presence of the plist in
  `~/Library/LaunchAgents`; the definition gtunnel loads and unloads itself lives
  outside that directory, because launchd runs a `KeepAlive` job the moment it is
  loaded.
- **Linux** - systemd `--user` units. `loginctl enable-linger` is what makes a tunnel
  survive a logout.
- **Release binaries** - `macos-arm64`, `macos-intel`, `linux-amd64`, `linux-arm64`. The
  assets are named for the machine rather than the Rust target triple, so the file a
  visitor wants is the one they can recognise. The Linux pair are static musl builds.
