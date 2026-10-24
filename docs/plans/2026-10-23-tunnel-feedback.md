# Tunnel Feedback and Presentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a tunnel say what happened to it, so that a healthy silent tunnel, a tunnel that never started and a tunnel someone else started are three different things on screen, and make the screen that says it read like one designed surface rather than seven panels that grew.

**Architecture:** gtunnel writes its own lifecycle lines into the same log file launchd and systemd already redirect the daemon's stdout and stderr into. Both writers append, so neither clobbers the other, and every reader gains the lines at once: `gtunnel logs`, the TUI pane, and the CLI's failure hint. The TUI stops rendering an empty file as an empty box, and the Details pane answers "who started this and when" from runtime facts the probe already collects.

**Tech Stack:** Rust, ratatui 0.30, tokio, launchd and systemd backends, `humantime` for RFC3339 timestamps.

**Spec:** No separate spec document. This plan is its own spec, from the analysis in `docs/plans/2026-10-22-gtunnel.md` and the decisions recorded under Decisions below. Rulings made during execution resolve against the Decisions section.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Where gtunnel's lines live | The daemon's own log file | One pane, no merge code, both writers append-only |
| Truncation on install | Kept as it is today | A run's log holds only that run; bounded without a size cap |
| Timestamps | RFC3339 UTC via `humantime`, suffixed `Z` | No hand-rolled civil-date arithmetic; Details shows relative time so the common case needs no timezone math |
| Scope | All seven tasks | The request was to improve this across the application, not only the pane |
| Colors | A `theme` module over the terminal's own 16 ANSI colors, not RGB | An RGB palette looks the same everywhere and therefore wrong somewhere: it ignores the user's terminal theme and fails on a light background. One named set, chosen once, stays legible in both |
| The chart's width | More history, sliced to the pane | A sparkline draws one cell per sample; stretching 60 samples across 110 columns invents resolution the probe never measured |
| `Active` | Count only the listener's side of each connection | Both ends of a loopback pair are on the same port, so the present count is exactly double whenever it is not zero |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on **both** macOS and Linux. The Linux leg runs in a container, because the binary target is where dead code surfaces:
  `docker run --rm -v "$PWD":/src:ro -v gtunnel-linux-target:/t -w /src -e CARGO_TARGET_DIR=/t rust:1-alpine sh -c 'apk add --no-cache build-base >/dev/null; rustup component add clippy rustfmt >/dev/null 2>&1; cargo fmt --check; cargo clippy --all-targets -- -D warnings; cargo test'`
- The suite is at 194 tests before this plan; it only grows.
- One new dependency, `humantime = "2"`, and no other.
- Comments explain why, in the voice of the surrounding code.
- `gtunnel` must never write a log line that claims an action it did not take.

## Review Focus

- A log file whose directory does not exist yet: the first `log_event` must create it rather than silently drop the line.
- A tunnel whose `log_path()` fails (unreadable config dir): a lifecycle event must not turn a working start into an error.
- A log line containing a newline or control characters from a target name: the pane must not break its own framing.
- Two writers on one file: a gtunnel line written while the daemon is mid-write must not interleave inside the daemon's line, which means one `write` of one complete line in append mode.
- A tunnel in `Error` state with an empty log: the pane must not claim the tunnel is "silent while healthy".
- A short terminal, 20 rows or fewer: the header must not take rows the tunnel list needs, and the list must keep at least one visible row.
- A narrow terminal, under 60 columns: the logo must step down to a single line rather than wrap into broken glyphs.
- A light-background terminal: every color the theme names must stay readable, which rules out `DarkGray` on white for anything load-bearing.

---

### Task 1: gtunnel writes its own lifecycle lines

**Files:**
- Modify: `src/daemon/mod.rs` (add `log_event`, add `description` to `ServiceSpec`, fill it in `service_spec`)
- Modify: `src/daemon/launchd.rs` (`install`), `src/daemon/systemd.rs` (`install`)
- Modify: `src/commands/access.rs` (`cmd_start`, `cmd_stop`, `cmd_restart`)
- Modify: `src/tui/ops.rs` (`start`, `Op::Stop`, `Op::Restart`, `clear_log`)
- Modify: `Cargo.toml` (`humantime = "2"`)
- Test: `src/daemon/mod.rs` `mod tests`

**Interfaces:**
- Consumes: `PersistentTunnel::log_path() -> Result<PathBuf>`, `PersistentTunnel::display_target() -> String` (both `src/state.rs`).
- Produces:
  - `daemon::log_event(path: &std::path::Path, message: &str) -> Result<()>` — appends one line `<rfc3339>Z gtunnel: <message>\n`, creating the parent directory if needed.
  - `daemon::ServiceSpec.description: String` — human-readable `"<name> -> <target>"`, filled by `service_spec`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_event_appends_one_line_and_creates_the_directory_it_needs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("nested/dev-db.log");
    log_event(&log, "starting dev-db -> 10.0.0.1:443").expect("first event");
    log_event(&log, "stopped by gtunnel").expect("second event");
    let text = std::fs::read_to_string(&log).expect("read");
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.lines().next().unwrap().ends_with("gtunnel: starting dev-db -> 10.0.0.1:443"), "{text}");
    assert!(text.ends_with('\n'), "{text}");
}

// The daemon appends too, so an event must never rewrite what is already there
#[test]
fn an_event_keeps_the_daemon_lines_already_in_the_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("dev-db.log");
    std::fs::write(&log, "Testing if tunnel connection works.\n").expect("seed");
    log_event(&log, "stopped by gtunnel").expect("event");
    let text = std::fs::read_to_string(&log).expect("read");
    assert!(text.starts_with("Testing if tunnel connection works.\n"), "{text}");
    assert_eq!(text.lines().count(), 2, "{text}");
}

// Review Focus 3: a target carrying a newline must not forge a second line
#[test]
fn an_event_writes_one_line_however_the_message_is_shaped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("dev-db.log");
    log_event(&log, "starting dev-db\nforged: line").expect("event");
    let text = std::fs::read_to_string(&log).expect("read");
    assert_eq!(text.lines().count(), 1, "{text}");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test daemon::tests::an_event`
Expected: FAIL, `cannot find function log_event in this scope`

- [ ] **Step 3: Implement `log_event(path: &std::path::Path, message: &str) -> Result<()>` in `src/daemon/mod.rs`**

Open with `create(true).append(true)`, after `create_dir_all` on the parent, and write one `format!` in a single `write_all` so the line cannot interleave with the daemon's. Replace any `\n` or `\r` in `message` with a space before formatting. Timestamp from `humantime::format_rfc3339_seconds(std::time::SystemTime::now())`.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test daemon::tests::an_event`
Expected: PASS, 3 tests

- [ ] **Step 5: Add `description` to `ServiceSpec` and fill it**

Add `pub description: String` to `ServiceSpec` in `src/daemon/mod.rs`, set in `service_spec` to `format!("{} -> {}", t.name, t.display_target())`. Update the `spec()` helper in `mod tests` and every other `ServiceSpec` construction the compiler names.

- [ ] **Step 6: Write the install-writes-a-header test**

```rust
// An edit or a plain install truncates the log; without a header the pane
// goes blank again and the tunnel looks like it was never there
#[tokio::test]
async fn installing_leaves_the_tunnel_named_in_its_own_log() {
    let _temp = crate::config::testing::TempConfigDir::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut s = spec();
    s.log_path = dir.path().join("dev-db.log");
    s.description = "dev-db -> 10.0.0.1:443".to_string();
    std::fs::write(&s.log_path, "stale line from the last run\n").expect("seed");
    write_log_header(&s).expect("header");
    let text = std::fs::read_to_string(&s.log_path).expect("read");
    assert!(!text.contains("stale"), "{text}");
    assert!(text.contains("dev-db -> 10.0.0.1:443"), "{text}");
}
```

- [ ] **Step 7: Run it, implement, run it again**

Run: `cargo test daemon::tests::installing_leaves`
Expected: FAIL (`write_log_header` not found), then PASS after implementing
`daemon::write_log_header(spec: &ServiceSpec) -> Result<()>` — `reset_log` then `log_event(&spec.log_path, &spec.description)` — and calling it from `launchd::install` and `systemd::install` in place of the bare `reset_log`.

- [ ] **Step 8: Write the lifecycle lines at the call sites that know the tunnel**

In `src/commands/access.rs` and `src/tui/ops.rs`, after the action succeeds:

| Call site | Line |
|---|---|
| `cmd_start`, `ops::start` | `starting <name> on <display_url>` |
| `cmd_stop`, `Op::Stop` | `stopped by gtunnel` |
| `cmd_restart`, `Op::Restart` | the stop line, then the start line |
| `ops::clear_log` | `log cleared` |

A failed `log_event` must not fail the operation: log the action's own result and ignore the event error, since a tunnel that started is still started.

- [ ] **Step 9: Verify the whole suite on both platforms**

Run: `cargo test` and the container command from Global Constraints
Expected: PASS on both, count above 194

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock src/daemon src/commands/access.rs src/tui/ops.rs
git commit -m "feat: a tunnel's log says what gtunnel did to it"
```

---

### Task 2: an empty pane explains itself

**Files:**
- Modify: `src/tui/ui.rs` (`render_logs`)
- Test: `src/tui/mod.rs` `mod tests`

**Interfaces:**
- Consumes: `TunnelEntry.tunnel.spec` (`TunnelSpec::Access { target: AccessTarget::Jump { .. } }` is the silent mode), `TunnelEntry.status`.
- Produces: `ui::empty_log_note(spec: &TunnelSpec, status: TunnelStatus) -> &'static str`.

- [ ] **Step 1: Write the failing test**

```rust
// A jump tunnel runs `ssh -N`, which prints nothing while it works, so an
// empty pane is the healthy case and must not read as a broken one
#[test]
fn an_empty_log_says_why_it_is_empty() {
    use crate::state::{AccessTarget, TunnelSpec, TunnelStatus};
    let jump = TunnelSpec::Access {
        target: AccessTarget::Jump {
            instance: "bastion".to_string(),
            zone: "europe-west1-b".to_string(),
            host: "10.3.0.13".to_string(),
        },
        remote_port: 443,
        local_port: 8443,
    };
    assert!(ui::empty_log_note(&jump, TunnelStatus::Running).contains("silent"));
    // Review Focus 5: a failed tunnel is not a quiet healthy one
    assert!(!ui::empty_log_note(&jump, TunnelStatus::Error).contains("silent"));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test tui::tests::an_empty_log`
Expected: FAIL, `cannot find function empty_log_note`

- [ ] **Step 3: Implement `empty_log_note` and use it in `render_logs`**

Three cases: a `Jump` target in any non-`Error` state returns a note that a tunnel through a jump host is silent while healthy and that errors appear here; an `Error` state returns a note that the tunnel failed with no output and names `gtunnel restart`; anything else returns a plain "no output yet". `render_logs` renders the note as the pane's only line when the tail is empty, in the dim style `detail` already uses.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test tui::tests::an_empty_log`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/tui/ui.rs src/tui/mod.rs
git commit -m "feat: an empty log pane says why it is empty"
```

---

### Task 3: Details answers who started this, and when

**Files:**
- Modify: `src/tui/ui.rs` (`render_details`)
- Test: `src/tui/mod.rs` `mod tests`

**Interfaces:**
- Consumes: `probe::TunnelMetrics.uptime: Option<Duration>`, `daemon::RuntimeInfo.restarts: u32`, `PersistentTunnel.auto_start: bool`, `ui::format_uptime`.
- Produces: `ui::started_by(restarts: u32, auto_start: bool) -> String`.

- [ ] **Step 1: Write the failing test**

```rust
// "Running" alone cannot answer the question the user actually asks, which is
// whether anything started this tunnel without being told to
#[test]
fn details_distinguish_a_launchd_revival_from_a_start() {
    assert_eq!(ui::started_by(1, false), "gtunnel (auto-start off)");
    assert_eq!(ui::started_by(1, true), "gtunnel (auto-start on)");
    // A second run of a job nobody re-started is launchd reviving a crash
    assert_eq!(ui::started_by(3, false), "launchd, after 2 restarts (auto-start off)");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test tui::tests::details_distinguish`
Expected: FAIL, `cannot find function started_by`

- [ ] **Step 3: Implement `started_by` and add two rows to `render_details`**

`runs <= 1` means the start gtunnel asked for; above that, launchd revived it `runs - 1` times. Add `detail("started", …)` using `format_uptime` on `metrics.uptime` rendered as "<uptime> ago", and `detail("started by", &started_by(..))`. An unknown uptime renders `-`, the way the port row already handles the unknown case.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test tui::tests::details_distinguish`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/tui/ui.rs src/tui/mod.rs
git commit -m "feat: details say when a tunnel started and what started it"
```

---

### Task 4: doctor reports auto-start drift, and the CLI can set it

**Files:**
- Modify: `src/doctor.rs` (`Finding`, `describe`, `fixable`, `account_findings`)
- Modify: `src/cli.rs` (an `AutoStart` command), `src/main.rs` (its arm), `src/commands/access.rs` (`cmd_auto_start`)
- Test: `src/doctor.rs` `mod tests`, `src/commands/access.rs` `mod tests`

**Interfaces:**
- Consumes: `daemon::launchd::agent_path(label) -> Result<PathBuf>`, `doctor::service_installed(label) -> bool`, `daemon::set_auto_start(spec) -> Result<()>`.
- Produces:
  - `Finding::AutoStartDrift { tunnel: String, record: bool, installed: bool }`
  - `access::cmd_auto_start(account: Option<&str>, name: &str, on: bool) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

```rust
// doctor reported "Nothing to report" while four tunnels were set to start at
// login, because it never compared the record against what launchd holds
#[test]
fn auto_start_drift_describes_both_sides() {
    let on_disk = Finding::AutoStartDrift {
        tunnel: "k8s-dev".to_string(),
        record: false,
        installed: true,
    };
    let text = on_disk.describe();
    assert!(text.contains("k8s-dev"), "{text}");
    assert!(text.contains("login"), "{text}");
    assert!(on_disk.fixable());
}
```

```rust
#[tokio::test]
async fn turning_auto_start_off_clears_the_record() {
    let _temp = crate::config::testing::TempConfigDir::new();
    // seed one tunnel with auto_start = true, call cmd_auto_start(.., false),
    // reload TunnelState and assert the record is false
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test doctor::tests::auto_start_drift commands::access::tests::turning_auto_start`
Expected: FAIL, `no variant named AutoStartDrift`

- [ ] **Step 3: Implement the finding**

Add the variant, a `describe` arm naming the tunnel and which side says what, `fixable() == true`, and a check in `account_findings` comparing `tunnel.auto_start` against `launchd::agent_path(&label).exists()` on macOS and the systemd equivalent elsewhere. The fix re-runs `daemon::set_auto_start` from the record, since the record is what the user edits.

- [ ] **Step 4: Implement the CLI command**

`gtunnel autostart <NAME> --on|--off` (no flag prints the current value), wired through `src/main.rs` to `cmd_auto_start`, which updates the record, saves, calls `daemon::set_auto_start`, and prints the resulting state.

- [ ] **Step 5: Run the suite on both platforms**

Run: `cargo test` and the container command from Global Constraints
Expected: PASS on both

- [ ] **Step 6: Update the docs**

`docs/usage.md` gains the `autostart` command beside `start` and `stop`, and a line saying a tunnel's log now opens with the tunnel's own name and target.

- [ ] **Step 7: Commit**

```bash
git add src/doctor.rs src/cli.rs src/main.rs src/commands/access.rs docs/usage.md
git commit -m "feat: doctor sees auto-start drift and the CLI can set it"
```

---

---

### Task 5: Active counts clients, not socket ends

**Files:**
- Modify: `src/probe.rs` (`parse_lsof_established`, `active_connections`)
- Test: `src/probe.rs` `mod tests`

**Interfaces:**
- Produces: `probe::parse_lsof_established(text: &str, port: u16) -> u64` — one per connection whose **local** side is `port`.

- [ ] **Step 1: Write the failing test**

```rust
// Both ends of a loopback connection carry the same port, so `lsof -iTCP:8443`
// returns two rows for one client and the count read double
#[test]
fn one_loopback_client_counts_once() {
    let real = "COMMAND   PID    USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME\n\
                ssh      2146 gonzalo    8u  IPv4 0x3a6c8252f144aa71      0t0  TCP 127.0.0.1:8443->127.0.0.1:64306 (ESTABLISHED)\n\
                Python  12253 gonzalo    3u  IPv4 0xc78928f83ee41eb7      0t0  TCP 127.0.0.1:64306->127.0.0.1:8443 (ESTABLISHED)\n";
    assert_eq!(parse_lsof_established(real, 8443), 1);
}

// A client on another machine reaching a tunnel bound to all interfaces has
// only one row, and it is still one connection
#[test]
fn a_remote_client_counts_once() {
    let text = "ssh 2146 gonzalo 8u IPv4 0x1 0t0 TCP 10.0.0.2:8443->10.0.0.9:51234 (ESTABLISHED)\n";
    assert_eq!(parse_lsof_established(text, 8443), 1);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test probe::tests::one_loopback_client probe::tests::a_remote_client`
Expected: FAIL, `this function takes 1 argument but 2 arguments were supplied`

- [ ] **Step 3: Implement the port-aware parser**

Take the `NAME` field (the last whitespace-separated token before `(ESTABLISHED)`), split on `->`, and count the row only when the left side's port equals `port`. Keep the existing requirement that the row contains `ESTABLISHED`, and update `active_connections` and the `ss` branch to pass the port through.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test probe::tests::`
Expected: PASS, including the existing parser tests

- [ ] **Step 5: Verify against a live tunnel**

Run, with `k8s-dev` running:
```bash
python3 -I -c "
import socket, subprocess
s = socket.create_connection(('127.0.0.1', 8443), 5)
print(subprocess.run(['lsof','-nP','-iTCP:8443','-sTCP:ESTABLISHED'], capture_output=True, text=True).stdout)
s.close()"
```
Expected: two rows, which the new parser reports as `1`.

- [ ] **Step 6: Commit**

```bash
git add src/probe.rs
git commit -m "fix: Active counts clients, not both ends of each socket"
```

---

### Task 6: the chart spans its pane

**Files:**
- Modify: `src/tui/app.rs` (`SPARKLINE_SAMPLES`), `src/tui/ui.rs` (`render_metrics`)
- Test: `src/tui/mod.rs` `mod tests`

**Interfaces:**
- Consumes: `MetricsHistory::samples() -> Vec<u64>` (`src/probe.rs`, unchanged).
- Produces: `ui::sparkline_slice(samples: &[u64], width: u16) -> &[u64]` — the newest `width` samples.

- [ ] **Step 1: Write the failing test**

```rust
// A sparkline draws one cell per sample, so 60 samples left two fifths of a
// wide pane empty and the chart read as a chart that had stopped
#[test]
fn the_chart_uses_every_column_it_is_given() {
    let samples: Vec<u64> = (0..240).collect();
    let shown = ui::sparkline_slice(&samples, 100);
    assert_eq!(shown.len(), 100);
    assert_eq!(shown.last(), samples.last(), "the newest sample must be drawn");
    // Fewer samples than columns is the first minute, not an error
    assert_eq!(ui::sparkline_slice(&samples[..10], 100).len(), 10);
    assert!(ui::sparkline_slice(&samples, 0).is_empty());
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test tui::tests::the_chart_uses_every_column`
Expected: FAIL, `cannot find function sparkline_slice`

- [ ] **Step 3: Implement it and raise the capacity**

`SPARKLINE_SAMPLES` becomes 240, which at one sample per refresh is four minutes of history and covers any terminal width. `render_metrics` passes `sparkline_slice(&samples, chunks[1].width)` to the `Sparkline`.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test tui::tests::the_chart_uses_every_column`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs src/tui/mod.rs
git commit -m "fix: the latency chart spans the pane it is drawn in"
```

---

### Task 7: a header, and one place that decides the colors

**Files:**
- Create: `src/tui/theme.rs`
- Modify: `src/tui/mod.rs` (declare the module), `src/tui/ui.rs` (`render`, `panel`, `status_style`, `field`, every `Color::` literal)
- Test: `src/tui/mod.rs` `mod tests`

**Interfaces:**
- Produces:
  - `theme::ACCENT`, `theme::MUTED`, `theme::OK`, `theme::WARN`, `theme::BAD`, `theme::SELECTED_BG` — the only `Color` values the TUI names.
  - `ui::header_lines(width: u16) -> Vec<Line<'static>>` — the logo and subtitle, or one compact line when `width` is under 60.
  - `ui::HEADER_HEIGHT_FULL`, `ui::HEADER_HEIGHT_COMPACT`.

- [ ] **Step 1: Write the failing tests**

```rust
// A logo that wraps is worse than no logo: the glyphs break mid-row and the
// subtitle lands inside them
#[test]
fn the_header_steps_down_before_it_wraps() {
    let wide = ui::header_lines(120);
    assert_eq!(wide.len() as u16, ui::HEADER_HEIGHT_FULL);
    for line in &wide {
        assert!(line.to_string().chars().count() <= 120, "{}", line.to_string());
    }
    let narrow = ui::header_lines(50);
    assert_eq!(narrow.len() as u16, ui::HEADER_HEIGHT_COMPACT);
    assert!(narrow[0].to_string().chars().count() <= 50);
}

// Review Focus: the header must never be the reason the list has no rows
#[test]
fn a_short_terminal_keeps_a_visible_tunnel_row() {
    let app = App::new_demo();
    let mut terminal = ratatui::Terminal::new(
        ratatui::backend::TestBackend::new(100, 20),
    ).expect("terminal");
    terminal.draw(|f| ui::render(f, &app)).expect("draw");
    let text = terminal.backend().to_string();
    assert!(text.contains("localhost:15432"), "{text}");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test tui::tests::the_header_steps_down tui::tests::a_short_terminal`
Expected: FAIL, `cannot find function header_lines`

- [ ] **Step 3: Write `src/tui/theme.rs`**

Six `pub const` colors over the terminal's own palette, so a user's theme still decides what cyan looks like: `ACCENT` cyan, `MUTED` dark gray, `OK` green, `WARN` yellow, `BAD` red, `SELECTED_BG` `Indexed(236)`. A doc comment at the top of the file states that rule, because the next person's instinct will be to reach for `Rgb`.

- [ ] **Step 4: Implement the header**

`header_lines` returns the block logo plus a subtitle line (`IAP tunnel manager` and the selected account) above 60 columns, and one line of `gtunnel - <account>` at or below it. `render` gains a first vertical constraint of the matching height, and the status line and help bar stay where they are. The logo is plain ASCII block characters, no emoji.

- [ ] **Step 5: Route every color through the theme**

Replace each `Color::` literal in `src/tui/ui.rs` with its `theme::` constant. `grep -c 'Color::' src/tui/ui.rs` ends at zero except for the `use` of the type itself.

- [ ] **Step 6: Run the tests and look at the result**

Run: `cargo test tui::` and then the app itself, `cargo run -- tui`, with a screenshot of the header at 120 columns and at 50
Expected: PASS, and a header that reads as one object at both widths

- [ ] **Step 7: Commit**

```bash
git add src/tui/theme.rs src/tui/mod.rs src/tui/ui.rs
git commit -m "feat: a header with the logo, and one module that decides the colors"
```

---

## Still deferred after this plan

- A log that survives the run that wrote it. Truncation on install stays by decision, so a `KeepAlive` revival still appends to the previous run's lines without a boundary. Task 3 makes the revival visible in Details (`started by: launchd, after N restarts`), which is the part the user could not see at all; the log itself remains a single run's record plus whatever a revival adds.
