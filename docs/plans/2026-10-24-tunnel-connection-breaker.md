# Tunnel Connection Breaker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A tunnel that has stopped accepting connections is noticed within 30 seconds, restarted with a backoff that does not thrash, and — when restarting cannot fix it — stopped with a message naming the process responsible.

**Architecture:** Three independent pieces and one that binds them. The service units raise a file-descriptor ceiling nobody chose. `probe` learns to say *who* holds a tunnel's connections, not just how many. `diagnose_log_line` learns what `Too many open files` means. On top of those, a pure breaker state machine decides what to do, and a periodic `com.gtunnel.watchdog` agent runs `gtunnel watch --once` every 30 seconds to feed it. The state machine touches no I/O, so every policy decision is testable without a tunnel.

**Tech Stack:** Rust, tokio, serde, launchd and systemd user units.

**Spec:** none written; this plan is the spec. The failure it exists for is recorded in `docs/troubleshooting.md` (to be added by Task 8) and was observed on 2026-10-24: a jump tunnel wedged at 248 established connections, all but one held by a single leaking client, with `accept: Too many open files` repeating for three minutes while the ssh process stayed alive and launchd therefore never acted.

## Global Constraints

- **Nothing changes about how a tunnel is launched.** The breaker observes from outside; `service_spec`'s program stays `gcloud` and its argv keeps its current shape. The one exception is the new resource-limit key, which is a unit key and not an argument.
- **No new always-on process per tunnel.** One agent for the machine, not one per tunnel.
- **The default must not surprise.** A machine that never installs the watchdog behaves exactly as it does today.
- The breaker covers **Access and jump tunnels only**. Their health is a local TCP probe, which costs nothing and cannot be wrong about the network. A public tunnel's health lives in a GCP load balancer and is out of scope here; `watch` skips them and says so.
- No emoji. Documentation uses placeholders only, per `.claude/rules/public-repo-placeholders.md`. The observed incident's real host, project and client names never appear in committed text.
- Every task ends with `cargo fmt --check`, `cargo test` and `cargo clippy --all-targets -- -D warnings` clean.

## Two parts, two pull requests

Part 1 is useful on its own and carries no new moving parts. Part 2 depends on it.

| Part | Tasks | What ships |
|---|---|---|
| 1 | 1-3 | The fd ceiling, attribution, and the EMFILE hint. A wedged tunnel now explains itself even with nothing watching. |
| 2 | 4-8 | The breaker and the watchdog agent. A wedged tunnel now recovers itself. |

## What trips the breaker, and what it does

A check is a **failure** when either holds, and a **pass** otherwise:

| Condition | Why it means wedged, not merely idle |
|---|---|
| the service reports Running **and** the TCP probe to `127.0.0.1:<local_port>` fails | the process is alive and refusing connections, which is the state launchd cannot see |
| the log has gained a line matching `Too many open files` since the last check | the listener is out of descriptors; every new client is being rejected |

The state machine, with all timing in one place:

| State | On pass | On failure |
|---|---|---|
| `Closed` | stay `Closed`, reset the failure count | count it; at **2 consecutive** failures go `Open`, attempt 1 |
| `Open` (waiting) | — | when `now >= next_attempt`, restart the tunnel and go `HalfOpen` |
| `HalfOpen` | at **2 consecutive** passes go `Closed` and reset the attempt count | go `Open` with the attempt count raised |
| `Open`, attempt > 4 | — | stop the tunnel, go `Stopped`, report the holder |
| `Stopped` | — | nothing, ever, until a person runs `gtunnel start` |

Backoff is `10s * 2^(attempt - 1)`: 10, 20, 40, 80 seconds, then `Stopped`. Two consecutive
observations in both directions is what makes it smooth: one transient refusal during a
restart does not trip it, and one lucky probe right after a restart does not close it.

`Stopped` is deliberate. A tunnel whose client re-floods it the instant it comes back — the
observed case went from 1 to 234 connections in a single 5-second tick — cannot be fixed by
restarting it, and a tunnel that restarts forever is a tunnel whose log nobody can read. The
message is the fix, so the message names the process.

## Review Focus

- **The watchdog restarting a tunnel a person just stopped.** `gtunnel stop` must win: a
  stopped tunnel is not an unhealthy one, and nothing in `Closed` may restart a service whose
  status is not Running.
- **Two gtunnels acting at once.** The watchdog fires every 30 seconds while the TUI may also
  be restarting the same tunnel. Decide what happens when both act, and make the state file
  survive it rather than being overwritten with a half-written table.
- **A machine with no `lsof`.** Attribution is best-effort; the breaker must still trip,
  restart and stop without it, and must not claim a holder it did not find.
- **A tunnel that is slow, not wedged.** `TCP_TIMEOUT` decides this. A laptop waking from
  sleep will fail one probe; two consecutive is the guard, and the plan must not weaken it.
- **The state file growing without bound.** A tunnel that is deleted leaves a row behind;
  `watch` must forget rows whose tunnel no longer exists.
- **The watchdog's own failure.** A `gtunnel watch --once` that panics or hangs must not take
  the agent down permanently, and must not leave a tunnel in `Open` forever.

---

## Part 1

### Task 1: A file-descriptor ceiling the tunnel chose

**Files:**
- Modify: `src/daemon/launchd.rs` (the plist template), `src/daemon/systemd.rs` (the unit template)
- Test: `src/daemon/launchd.rs`, `src/daemon/systemd.rs`

**Interfaces:**
- Produces: `pub const TUNNEL_MAX_FILES: u64 = 4096;` in `src/daemon/mod.rs`, used by both unit writers.

The observed outage was EMFILE at 248 connections, because a launchd job inherits a soft
`RLIMIT_NOFILE` of 256 (`launchctl limit maxfiles` → `256 unlimited`) and gtunnel never raised
it. Each client of a forwarding tunnel costs one descriptor, so 256 is a ceiling on clients
that nobody chose and nothing documents. The hard limit is unlimited and
`kern.maxfilesperproc` is 61440, so 4096 is both safe and far more than a tunnel needs.

- [x] **Step 1: Write the failing tests**

```rust
// launchd.rs
#[test]
fn a_tunnel_is_not_capped_at_launchds_256_descriptors() {
    let plist = plist_for(&spec());
    assert!(plist.contains("<key>SoftResourceLimits</key>"), "{plist}");
    assert!(plist.contains("<key>NumberOfFiles</key>"), "{plist}");
    assert!(plist.contains(&format!("<integer>{TUNNEL_MAX_FILES}</integer>")), "{plist}");
}

// systemd.rs
#[test]
fn a_tunnel_is_not_capped_at_the_units_default_descriptors() {
    let unit = unit_for_spec(&spec());
    assert!(unit.contains(&format!("LimitNOFILE={TUNNEL_MAX_FILES}")), "{unit}");
}
```

- [x] **Step 2: Run them** — Expected: FAIL, neither key is written.
- [x] **Step 3: Add the key to both templates.** In the plist, `SoftResourceLimits` is a dict holding `NumberOfFiles`; it must not set a hard limit, which only root may raise. In the unit, `LimitNOFILE` goes in `[Service]`.
- [x] **Step 4: Run them** — Expected: PASS, and the whole suite green: the existing template tests assert on exact text and will need the new key accounted for.
- [x] **Step 5: Verify against a real service.** Install a tunnel under a throwaway `GTUNNEL_CONFIG_DIR` with an account name of its own, then read the limit off the running process rather than the file: `launchctl print gui/$UID/<label> | grep -A3 'soft resource limits'`. Expected: the file count is `TUNNEL_MAX_FILES`, not 256. Record the output in the commit body.
- [x] **Step 6: Commit** — `fix: a tunnel is not capped at launchd's 256 descriptors`

---

### Task 2: Who is holding a tunnel's connections

**Files:**
- Modify: `src/probe.rs`
- Test: `src/probe.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct Holder { pub pid: u32, pub command: String, pub count: u64 }
  /// Descending by count. Empty when `lsof` is absent or nothing is connected.
  pub fn holders_for_port(port: u16) -> Vec<Holder>;
  pub fn parse_lsof_holders(text: &str, port: u16, exclude: Option<u32>) -> Vec<Holder>;
  ```

`parse_lsof_by_port` already parses every established row into `(pid, local, peer)` and already
matches a client row to a server row by endpoint — that is how it excludes gtunnel's own probe.
It throws the client's pid away and keeps a count. The whole of this task is keeping it.

Two details the implementation must get right, both verified against the live failure:

| Detail | Why |
|---|---|
| the listing must use `lsof -nP +c 0` | without `+c 0` lsof truncates the command to 9 characters, so the culprit reads `mcp-serve` and a `pgrep` for it finds nothing |
| the row that names the holder is the **client** row | the server rows all belong to `ssh`; the holder is the pid whose *peer* is `127.0.0.1:<port>` |

- [x] **Step 1: Write the failing tests**

```rust
// A fixture in the real `lsof -nP +c 0 -iTCP -sTCP:ESTABLISHED` shape, with one
// ssh listener row per connection and the client rows that face them.
#[test]
fn the_holder_of_a_ports_connections_is_named_with_its_count() {
    let holders = parse_lsof_holders(FIXTURE, 8443, None);
    assert_eq!(holders[0].command, "mcp-server");
    assert_eq!(holders[0].pid, 19191);
    assert_eq!(holders[0].count, 3);
}

#[test]
fn holders_are_ordered_so_the_worst_offender_is_first() {}

// The probe connects to the very port it measures; naming gtunnel as the
// culprit would be both wrong and insulting
#[test]
fn gtunnels_own_probe_is_never_named_as_a_holder() {
    let holders = parse_lsof_holders(FIXTURE, 8443, Some(OUR_PID));
    assert!(holders.iter().all(|h| h.pid != OUR_PID), "{holders:?}");
}

// A machine with no lsof must degrade, not lie
#[test]
fn nothing_parseable_names_nobody() {
    assert!(parse_lsof_holders("", 8443, None).is_empty());
    assert!(parse_lsof_holders("lsof: command not found", 8443, None).is_empty());
}
```

- [x] **Step 2: Run them** — Expected: FAIL, `parse_lsof_holders` not defined.
- [x] **Step 3: Implement.** Reuse `established_rows`, extending it to carry the command column; keep `parse_lsof_by_port` working on the same rows rather than duplicating the parse.
- [x] **Step 4: Run them** — Expected: PASS.
- [x] **Step 5: Verify against the live machine.** Run `holders_for_port` against a port with real traffic and check the answer matches `lsof -nP +c 0 -iTCP:<port> -sTCP:ESTABLISHED | awk 'NR>1{print $1, $2}' | sort | uniq -c`. Record both in the commit body.
- [x] **Step 6: Commit** — `feat: a tunnel's connections can be attributed to the process holding them`

---

### Task 3: What `Too many open files` means

**Files:**
- Modify: `src/tunnel.rs` (`diagnose_log_line`)
- Test: `src/tunnel.rs`

- [x] **Step 1: Write the failing test**

```rust
#[test]
fn diagnose_log_line_explains_a_tunnel_out_of_descriptors() {
    let hint = diagnose_log_line("accept: Too many open files").expect("a hint");
    assert!(hint.contains("descriptor") || hint.contains("connections"), "{hint}");
    assert!(hint.contains("gtunnel doctor"), "{hint}");
}
```

- [x] **Step 2: Run it** — Expected: FAIL, no hint for that line.
- [x] **Step 3: Add the row.** The hint says the listener is out of descriptors, that a client is probably holding connections open, and points at `gtunnel doctor`, which Task 7 teaches to name the holder.
- [x] **Step 4: Run it** — Expected: PASS.
- [x] **Step 5: Commit** — `feat: the log says what a tunnel out of descriptors needs`

---

## Rulings made while executing Part 1

| Ruling | Why | Cost if wrong |
|---|---|---|
| `TUNNEL_MAX_FILES`' bounds are two `const _: () = assert!(...)` items, not a test | clippy rejects `assertions-on-constants` in a test body, and a compile-time assert is strictly stronger: a bad edit fails the build rather than a run | none; the check is harder to remove by accident |
| `holders_for_port` ships with `#[allow(dead_code)]` and a comment naming the watchdog | its consumer is Task 6. Wiring it into `doctor` early would widen Part 1 past what was approved, and the repo already carries this precedent in `daemon/mod.rs` | a dead function for one PR, removed or used in Part 2 |
| `active_connections`' own `lsof -iTCP:<port>` call does not take `+c 0` | it counts rows and never reads COMMAND, and the column width cannot change a count | none |
| The `+c 0` flag is a named constant with a test of its own | the live run found the truncation that the fixture test could not: a fixture holds whatever it was written to hold, so only the argv we hand lsof can pin the behaviour | none |

## Part 2

### Task 4: The breaker, as a state machine that touches nothing

**Files:**
- Create: `src/breaker.rs`
- Modify: `src/main.rs` (one `mod` line)
- Test: `src/breaker.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum Observation { Pass, Failed(Reason), NotRunning }
  pub enum Reason { ProbeRefused, DescriptorsExhausted }
  pub enum Action { Nothing, Restart, StopAndReport(Reason) }
  pub struct Breaker { state: State, failures: u8, passes: u8, attempt: u8, next_attempt: Option<SystemTime> }
  impl Breaker { pub fn observe(&mut self, obs: Observation, now: SystemTime) -> Action; }
  pub const MAX_ATTEMPTS: u8 = 4;
  pub const BASE_BACKOFF: Duration = Duration::from_secs(10);
  ```

Pure: no clock of its own, no process, no file. `now` is a parameter, which is what makes
every row of the table above a test that runs in microseconds.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn one_failed_probe_is_not_a_wedged_tunnel() {}
#[test] fn two_consecutive_failures_trip_it_and_schedule_a_restart() {}
#[test] fn a_restart_does_not_happen_before_its_backoff_has_elapsed() {}
#[test] fn each_attempt_waits_twice_as_long_as_the_one_before() {}
#[test] fn two_consecutive_passes_close_it_and_forget_the_attempts() {}
// The observed case: the client re-floods within seconds of every restart
#[test] fn a_tunnel_that_refloods_after_every_restart_is_stopped_not_restarted_forever() {}
#[test] fn a_stopped_breaker_never_acts_again() {}
// gtunnel stop must win
#[test] fn a_tunnel_a_person_stopped_is_not_a_tunnel_to_restart() {
    // Observation::NotRunning, from any state, yields Action::Nothing and
    // resets the counters: the next Running observation starts clean
}
```

- [ ] **Step 2: Run them** — Expected: FAIL, `Breaker` not defined.
- [ ] **Step 3: Implement the table above** and nothing beyond it.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Commit** — `feat: a breaker that decides when a wedged tunnel is worth restarting`

---

### Task 5: Remembering the breakers between runs

**Files:**
- Create: `src/breaker_state.rs`, or a `persist` module inside `src/breaker.rs`
- Test: with it

**Interfaces:**
- Consumes: `Breaker` from Task 4.
- Produces: `pub fn load() -> BreakerFile`, `pub fn save(&BreakerFile) -> Result<()>`, keyed `"<account>.<name>"`, stored beside `tunnels.toml` in the configuration directory as `breakers.toml`.

A 30-second agent is a fresh process every time, so the state machine's memory has to be on
disk or it has none.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_breaker_survives_the_process_that_wrote_it() {}
#[test] fn a_missing_file_is_every_breaker_closed_rather_than_an_error() {}
#[test] fn a_corrupt_file_is_an_error_that_names_the_file_and_does_not_delete_it() {}
// Review Focus: a deleted tunnel must not leave a row behind forever
#[test] fn rows_for_tunnels_that_no_longer_exist_are_forgotten_on_save() {}
// Review Focus: the TUI and the watchdog can write at the same moment
#[test] fn a_write_interrupted_partway_leaves_the_previous_file_intact() {
    // write to a sibling temp file in the same directory, then rename:
    // rename is atomic within a filesystem, a partial write is not
}
```

- [ ] **Step 2: Run them** — Expected: FAIL.
- [ ] **Step 3: Implement**, with the same owner-only permissions `save_config` sets.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Commit** — `feat: breaker state outlives the process that observed it`

---

### Task 6: `gtunnel watch --once`

**Files:**
- Create: `src/commands/watch.rs`
- Modify: `src/cli.rs`, `src/main.rs`
- Test: `src/commands/watch.rs`

**Interfaces:**
- Consumes: `Breaker`, the breaker file, `probe::tcp_probe`, `probe::holders_for_port`, `daemon::status`, `daemon::start`/`stop`.
- Produces: `pub async fn cmd_watch(once: bool) -> Result<()>`.

One pass over every Access and jump tunnel in every account: observe, feed the breaker, carry
out the action it returns, write the file back. Each action it takes writes a
`gtunnel: <message>` line into that tunnel's own log through `daemon::log_event`, because that
is where a person looks afterwards. `--once` runs a single pass and exits, which is what the
agent invokes; without it, `watch` loops in the foreground, which is what a person debugging
it runs.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn a_pass_over_a_healthy_tunnel_changes_nothing_and_writes_nothing() {}
#[tokio::test] async fn a_public_tunnel_is_skipped_and_said_to_be_skipped() {}
#[tokio::test] async fn the_log_line_for_a_trip_names_the_reason() {}
// The whole point of Task 2
#[tokio::test] async fn stopping_a_tunnel_names_the_process_holding_its_connections() {}
#[tokio::test] async fn a_machine_without_lsof_still_stops_the_tunnel_and_says_so_without_a_name() {}
```

- [ ] **Step 2: Run them** — Expected: FAIL.
- [ ] **Step 3: Implement.** Keep the pure decision in `Breaker`; this file does I/O and nothing else.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Verify live.** Against the real wedged tunnel if one can be reproduced, or by holding the local port's descriptors artificially. Show the log lines it wrote.
- [ ] **Step 6: Commit** — `feat: one pass of the watchdog over every tunnel`

---

### Task 7: The watchdog agent, and doctor noticing it

**Files:**
- Modify: `src/daemon/launchd.rs`, `src/daemon/systemd.rs` (an interval job, which neither writes today), `src/commands/watch.rs`, `src/cli.rs`, `src/doctor.rs`
- Test: with each

**Interfaces:**
- Produces: `gtunnel watch install`, `gtunnel watch uninstall`, `gtunnel watch status`; label `com.gtunnel.watchdog` on macOS, `gtunnel-watchdog.timer` on Linux.

The agent runs the gtunnel binary, not gcloud, so it needs the same `PATH` treatment
`daemon_env` already gives a tunnel. It must never ask for a terminal.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn the_watchdog_runs_every_thirty_seconds_and_at_load() {}
#[test] fn the_watchdog_runs_the_gtunnel_binary_with_watch_once() {}
// It is not a tunnel: a KeepAlive job would run it continuously
#[test] fn the_watchdog_is_an_interval_job_rather_than_a_keepalive_one() {}
#[test] fn doctor_reports_the_watchdog_as_not_installed_when_it_is_not() {}
```

- [ ] **Step 2: Run them** — Expected: FAIL.
- [ ] **Step 3: Implement**, including the `doctor` check and its `--fix`, which installs it.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Verify live.** Install it, wait one interval, and show `launchctl print gui/$UID/com.gtunnel.watchdog` reporting a completed run and the log line it wrote. Then uninstall it and show it gone.
- [ ] **Step 6: Commit** — `feat: a watchdog agent that notices a wedged tunnel within thirty seconds`

---

### Task 8: Documentation

**Files:**
- Modify: `docs/troubleshooting.md`, `docs/usage.md`, `docs/architecture.md`, `README.md`

- [ ] **Step 1: A `### A tunnel that stops accepting connections` section in `docs/troubleshooting.md`**, carrying the `Too many open files` row in the log table, what the descriptor ceiling is and was, and how to find the process holding the connections by hand. This is the one place that story lives.
- [ ] **Step 2: Document `watch` in `docs/usage.md`** — the three subcommands, the 30-second interval, the backoff schedule as a table, and what `Stopped` means and how to leave it.
- [ ] **Step 3: Add the watchdog to `docs/architecture.md`'s process model**, which today describes only per-tunnel services, and to its files table for `breakers.toml`.
- [ ] **Step 4: One line in `README.md`** under Configuration naming `breakers.toml`.
- [ ] **Step 5: Run the sweep** — `git ls-files | grep -v '^docs/plans/' | xargs grep -nIE 'holded|gonzalo|private-cluster|10\.3\.0\.13|a612|5749' 2>/dev/null` — Expected: no output. The incident that motivated this plan is described with placeholders.
- [ ] **Step 6: Commit** — `docs: what a wedged tunnel looks like and what the watchdog does about it`

## Self-review

**Spec coverage.** Each of the four pieces the scope named has a task: the ceiling is Task 1,
attribution Task 2, the EMFILE hint Task 3, the breaker Tasks 4-7.

**Interfaces.** `holders_for_port` is produced in Task 2 and consumed in Tasks 6 and 7 under
that name; `Breaker::observe` is produced in Task 4 and consumed in Task 6; `breakers.toml` is
produced in Task 5 and documented in Task 8.

**Review Focus coverage.** Person-stopped tunnels are pinned in Task 4's last test; concurrent
writers in Task 5's atomic-rename test; a missing `lsof` in Tasks 2 and 6; slow-not-wedged in
Task 4's first test; the growing state file in Task 5; the watchdog's own failure is the one
Review Focus item with no test, because it is a property of the agent configuration rather
than of code — Task 7 Step 5 verifies it by observation instead.

**Proportion.** Eight tasks over two pull requests, the first three of which are each a few
lines of template or parsing. The weight is in Tasks 4 to 7, which is where the behaviour is.
