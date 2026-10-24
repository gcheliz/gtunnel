# Tunnel Log Activity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A running tunnel's log shows that it is carrying traffic, for every tunnel kind, without a gtunnel process watching it.

**Architecture:** Two independent sources, because neither alone is honest. The daemon's own child process is made to speak — unbuffered stdout, gcloud at `info`, ssh at `VERBOSE` — so the log fills whether or not anyone is looking. On top of that, gtunnel writes the facts only it knows, through the `log_event` writer that already exists: a change in the connection count it measures every 5s, and a health transition it already raises a desktop notification for.

**Tech Stack:** Rust, the existing `src/daemon` service writers, `src/tunnel.rs` argv builders, `src/probe.rs` metrics.

**Spec:** none. This plan implements a diagnosis, recorded below, not a written spec. Rulings made against it are provisional.

## The diagnosis this plan implements

Four verified findings, from reading gcloud 590.0.0 and the live logs:

| # | Finding | Evidence |
|---|---|---|
| 1 | 8 of 10 tunnels are jump tunnels running `gcloud compute ssh`, not `start-iap-tunnel` | `src/tunnel.rs:36-66` selects on the presence of `host` |
| 2 | That path has no local listener and no accept loop, so it can never print `Listening on port` or an accept line | `IapTunnelStdinHelper` (iap_tunnel.py:987); only `IapTunnelProxyServerHelper` (846-986) prints those |
| 3 | `New connection from` does not exist in gcloud 590.0.0 at all; the surviving per-connection lines are `log.info`, suppressed at the default `warning` verbosity | iap_tunnel.py:720, 830, 833 |
| 4 | gcloud's stdout block-buffers into a redirected file, so even what it does print does not appear | `dev-mongodb.log` holds only the line gcloud wrote to stderr |

The log line the user compared against came from `start-iap-tunnel` at a higher verbosity. It was never going to appear for a `--via` tunnel.

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- Public repo: every identifier in committed text is a placeholder per `.claude/rules/public-repo-placeholders.md`. GCP project `example-dev-1a2b`, account `you@example.com`, instance `bastion`, address `10.0.0.10`, hostname `db.internal`, username `you`. Zones stay real.
- Tunnel names follow `.claude/rules/tunnel-naming.md`: `<platform>` or `<platform>-<service>`, never an account segment.
- A log line gtunnel writes goes through `daemon::log_event`, never a second writer. Its contract holds: one `write_all` of one whole line, control characters stripped.
- `cargo fmt` and `cargo clippy -D warnings` pass before each commit. Crate `[lints]` makes dead code an error, so a test-only helper takes `#[cfg(test)]`, not `allow(dead_code)`.

## Review Focus

Five things no task's tests exercise, most likely to bite first:

1. **`--verbosity=info` on a long-lived tunnel floods the log.** gcloud logs per-poll at info on some paths; a tunnel left up for a week could fill the disk. There is no rotation today.
2. **A connection-count line written only while the TUI runs reads as a continuous record and is not one.** A reader seeing `connections: 0 -> 2` at 14:03 and nothing after 14:30 will conclude the tunnel went idle, when in fact gtunnel stopped watching.
3. **Two writers, one file.** The daemon's stdout and gtunnel's `log_event` both append. Under `PYTHONUNBUFFERED=1` gcloud writes smaller, more frequent chunks, which raises the chance of an interleave inside a line that was previously flushed whole.
4. **`-o LogLevel=VERBOSE` prints the remote host key and the auth method on every reconnect.** That is not a secret, but it is noise in a file the user pastes into issues.
5. **A tunnel that restarts in a loop now writes its whole verbose startup each time**, so the restart storm that was three lines becomes three hundred.

---

### Task 1: The daemon stops block-buffering gcloud's stdout

**Files:**
- Modify: `src/daemon/mod.rs` (`daemon_env`, around line 33)
- Test: `src/daemon/mod.rs` (the existing unit tests around line 300)

**Interfaces:**
- Consumes: nothing.
- Produces: `daemon_env` returns a third pair, `("PYTHONUNBUFFERED", "1")`. Tasks 2 and 3 depend on it: their extra output is worthless while it sits in a 4 KiB buffer.

- [ ] **Step 1: Write the failing test**

Extend the launchd and systemd env assertions that already exist near `src/daemon/mod.rs:303`, in their own test:

```rust
#[test]
fn the_child_does_not_block_buffer_its_output_into_the_log() {
    let unit = systemd::unit(&spec());
    assert!(
        unit.contains("Environment=PYTHONUNBUFFERED=1"),
        "gcloud is Python, and its stdout block-buffers into a redirected \
         file: a tunnel's lines would appear 4 KiB at a time or at exit\n{unit}"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -- the_child_does_not_block_buffer`
Expected: FAIL, the assertion message above.

- [ ] **Step 3: Add the pair to `daemon_env`**

One entry in the returned `vec!`, with a comment saying why: Python block-buffers a redirected stdout, and a tunnel that prints one line an hour would show nothing for a day.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -- the_child_does_not_block_buffer`
Expected: PASS.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test`
Expected: all pass. The launchd plist test at `src/daemon/mod.rs:198` and the systemd unit test at 298 both render the env; check neither pinned an exact count of entries.

- [ ] **Step 6: Commit**

```bash
git add src/daemon/mod.rs
git commit -m "fix: the daemon's child no longer block-buffers its log into silence"
```

---

### Task 2: gcloud is asked for the lines it already has

**Files:**
- Modify: `src/tunnel.rs` (`gcloud_common`, line 8)
- Test: `src/tunnel.rs` (the argv tests around line 319 and 381)

**Interfaces:**
- Consumes: Task 1's unbuffered stdout.
- Produces: every gcloud argv — access, jump and expose, because all three call `gcloud_common` — carries `--verbosity=info`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_gcloud_command_asks_for_its_connection_lines() {
    // The per-connection lines in iap_tunnel.py are log.info, and the default
    // verbosity is warning, so a working tunnel prints nothing at all
    for argv in [access_argv(&acct(), &instance_tunnel()).expect("access"),
                 access_argv(&acct(), &jump_tunnel()).expect("jump")] {
        assert!(
            argv.contains(&"--verbosity=info".to_string()),
            "{argv:?}"
        );
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -- every_gcloud_command_asks`
Expected: FAIL on the first argv.

- [ ] **Step 3: Add the flag in `gcloud_common`**

`--verbosity=info` beside `--configuration` and `--project`. The comment names what it buys: the accept lines at iap_tunnel.py:720 and 830 are `log.info`, invisible at the default `warning`.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -- every_gcloud_command_asks`
Expected: PASS.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test`
Expected: all pass. `gcloud_common` feeds the expose argv and the discovery fixtures too; an argv test that asserted an exact length will fail here and is the one to update.

- [ ] **Step 6: Commit**

```bash
git add src/tunnel.rs
git commit -m "feat: gcloud is asked for the connection lines it logs at info"
```

---

### Task 3: A jump tunnel's ssh says when it forwards

**Files:**
- Modify: `src/tunnel.rs` (the jump branch, around line 48-67)
- Test: `src/tunnel.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces: the jump argv carries `-o LogLevel=VERBOSE` among its other `-o` options.

A jump tunnel is `ssh`, so gcloud's verbosity does not reach it. ssh at `VERBOSE` prints the forward being established and the connection being accepted, which is the nearest honest equivalent of the line the user expected.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_jump_tunnel_asks_ssh_to_say_when_it_forwards() {
    let argv = access_argv(&acct(), &jump_tunnel()).expect("jump");
    let i = argv.iter().position(|a| a == "LogLevel=VERBOSE")
        .expect("ssh is silent by default, so a healthy --via tunnel logs nothing");
    assert_eq!(argv[i - 1], "-o", "{argv:?}");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -- a_jump_tunnel_asks_ssh`
Expected: FAIL with the `expect` message.

- [ ] **Step 3: Add the option**

`-o LogLevel=VERBOSE` in the existing `-o` run, before `-L`. One comment line: gcloud's `--verbosity` governs gcloud, not the ssh it execs, and `ExitOnForwardFailure` already proves the forward matters.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -- a_jump_tunnel_asks_ssh`
Expected: PASS.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/tunnel.rs
git commit -m "feat: a jump tunnel's ssh reports the forward it establishes"
```

---

### Task 4: gtunnel writes the connection count it already measures

**Files:**
- Modify: `src/tui/mod.rs` (beside `announce`, line 274)
- Modify: `src/tui/app.rs` (one field holding the last count per tunnel)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: `probe::TunnelMetrics::active_connections`, already collected every 5s at `src/tui/ops.rs:214`; `daemon::log_event(path, message)`.
- Produces: `fn connection_change(name: &str, was: u64, now: u64) -> Option<String>`, pure, returning the line to log or `None` when the count did not change. Task 5 follows the same shape.

Only a change is written. The count itself is on screen in Metrics; a line per poll would be 17,280 lines a day saying nothing.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn only_a_change_in_the_connection_count_is_worth_a_line() {
    assert_eq!(connection_change("kubernetes", 0, 2),
               Some("connections: 0 -> 2".to_string()));
    assert_eq!(connection_change("kubernetes", 2, 0),
               Some("connections: 2 -> 0".to_string()));
    assert_eq!(connection_change("kubernetes", 2, 2), None,
               "a line per poll is 17,280 a day that say nothing");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -- only_a_change_in_the_connection_count`
Expected: FAIL, `connection_change` not found.

- [ ] **Step 3: Implement `connection_change(name: &str, was: u64, now: u64) -> Option<String>` in `src/tui/mod.rs`**

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -- only_a_change_in_the_connection_count`
Expected: PASS.

- [ ] **Step 5: Wire it to the metrics arrival**

Where metrics land on `App`, compare against the stored previous count and, on `Some(line)`, call `daemon::log_event(&tunnel.log_path()?, &line)`, ignoring the error exactly as the existing call sites at `src/tui/ops.rs:149` and `:163` do. Store the new count.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add src/tui/mod.rs src/tui/app.rs
git commit -m "feat: a change in a tunnel's connection count reaches its log"
```

---

### Task 5: A health transition reaches the log, not only the desktop

**Files:**
- Modify: `src/tui/mod.rs` (`announce`, line 274)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: Task 4's `log_event` call shape.
- Produces: `announce` takes the log path and writes the same transition it notifies.

A desktop notification is gone the moment it is dismissed, and a notification raised while the laptop was asleep was never seen. The log is the durable copy of the one event the user most needs after the fact.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_health_transition_is_written_where_it_can_be_read_later() {
    assert_eq!(transition_line(HealthStatus::Healthy, HealthStatus::Unhealthy),
               Some("unhealthy".to_string()));
    assert_eq!(transition_line(HealthStatus::Unhealthy, HealthStatus::Healthy),
               Some("healthy again".to_string()));
    assert_eq!(transition_line(HealthStatus::Healthy, HealthStatus::Healthy), None);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -- a_health_transition_is_written`
Expected: FAIL, `transition_line` not found.

- [ ] **Step 3: Extract `transition_line(was, now) -> Option<String>` and have `announce` use it for both the notification body and a `log_event` call**

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -- a_health_transition_is_written`
Expected: PASS.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test`
Expected: all pass, including the design-doc citation tests.

- [ ] **Step 6: Commit**

```bash
git add src/tui/mod.rs
git commit -m "feat: a health transition is logged as well as notified"
```

---

### Task 6: Say what the log now holds

**Files:**
- Modify: `docs/troubleshooting.md` (a section on reading a tunnel's log)
- Modify: `docs/architecture.md` (the jump-tunnel section's note that nothing prints `Listening on port`)
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Document what each tunnel kind writes**

A table in `docs/troubleshooting.md`: for Access, jump and expose, which lines come from the daemon's child and which from gtunnel, with a real captured example per kind against a throwaway `GTUNNEL_CONFIG_DIR`.

- [ ] **Step 2: State the limitation plainly**

The connection-count lines are written by a running gtunnel, so a gap in them means nobody was watching, not that the tunnel was idle. A reader who does not know that will misread the file.

- [ ] **Step 3: Correct the architecture note**

`docs/architecture.md` says a jump tunnel prints nothing because there is no listener. Still true of `Listening on port`; no longer true of the file as a whole.

- [ ] **Step 4: CHANGELOG**

One bullet under Unreleased, naming the four causes and what each fix buys.

- [ ] **Step 5: Placeholder sweep**

Run:
```bash
git ls-files | grep -v '^docs/plans/' \
  | xargs grep -nIE 'holded|gonzalo|private-cluster|10\.3\.0\.13|a612|5749' 2>/dev/null
```
Expected: no output, exit 1.

- [ ] **Step 6: Commit**

```bash
git add docs/troubleshooting.md docs/architecture.md CHANGELOG.md
git commit -m "docs: what each tunnel kind writes to its log, and who writes it"
```
