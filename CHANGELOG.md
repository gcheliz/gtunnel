# Changelog

Notable changes per release. Dates are the release date; an unreleased section is
what is on `main` and not yet tagged.

## Unreleased

### TUI

- **A config page** - `C` shows `config.toml` and `tunnels.toml` with their paths, and `e`
  edits the one on screen in `$VISUAL`, then `$EDITOR`, then `vi`. The editor works on a
  private copy, which replaces the real file only after it parses and passes the rules
  `gtunnel add` enforces; a refused edit keeps its copy for `e` to reopen and `Esc` to
  discard. A saved edit reloads accounts, projects, the relay and the list without
  restarting, and says what changed. Running tunnels keep their old settings until `R`.

### Build

- **A lint policy in `Cargo.toml`** - `unsafe_op_in_unsafe_fn`,
  `clippy::undocumented_unsafe_blocks`, `dbg_macro`, `todo` and `unimplemented` are
  declared once under `[lints]`, so CI and editors agree. Every `unsafe` block now carries
  the invariant that makes it sound.
- **Workflow linting in CI** - a `zizmor` job checks the workflows for template injection,
  broad permissions, unpinned actions and persisted credentials. Checkouts no longer keep
  the token, CI jobs are read-only, and the release build no longer restores a build cache.

## v0.1.0 - unreleased

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
- **Release binaries** - `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`.
