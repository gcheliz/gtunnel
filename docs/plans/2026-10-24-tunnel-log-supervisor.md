# Tunnel Log Supervisor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A tunnel's log carries the lines that say what the tunnel is doing, each with a timestamp, and nothing else — while ssh keeps running at `DEBUG`, which is the only level that emits those lines.

**Architecture:** The service manager runs `gtunnel` instead of `gcloud`. A hidden `__supervise` subcommand execs the gcloud argv it is handed, reads the child's stdout and stderr, drops the lines that carry no information, timestamps the rest, and writes them to its own stdout — which launchd and systemd already redirect into the tunnel's log. The supervisor knows nothing about tunnels, accounts or config: everything it needs is on its command line, so it is a pure filter with a process attached.

**Tech Stack:** Rust, clap, tokio (process, io), libc (signals).

**Spec:** none written; this plan is the spec. The behaviour it must preserve is recorded in `docs/architecture.md` ("Access mode", "Jump tunnels"), `docs/troubleshooting.md` ("Who writes what"), and the two comments that justify `-o LogLevel=DEBUG` — `src/tunnel.rs:70-75` for the jump argv and `src/tunnel.rs:150-154` for the expose argv.

## Global Constraints

- **ssh stays at `LogLevel=DEBUG` and gcloud at `--verbosity=info`.** The two lines that prove a tunnel works — `Local forwarding listening on` and one `Connection to port ... forwarding to ...` per client — are `debug()` calls in ssh. Lowering the level to quieten the log is the one fix this plan rejects: at `VERBOSE` ssh prints three lines for a whole session and nothing per connection.
- **`PYTHONUNBUFFERED=1` stays** (`src/daemon/mod.rs:48`) and is never conditional. gcloud is Python and block-buffers a non-terminal stdout; without it a line waits for 4 KiB or for the process to exit. The supervisor's own stdout must be flushed per line for the same reason.
- **The supervisor never reads config, state or `GTUNNEL_CONFIG_DIR`.** The plist's `EnvironmentVariables` does not carry it (`daemon_env`, `src/daemon/mod.rs:33`), so a supervisor that resolved a tunnel by name would read the wrong directory under a throwaway config and silently run the wrong tunnel.
- **Exit status is the child's.** launchd `KeepAlive` and systemd `Restart=` key off it; a supervisor that always exits 0 turns a crash-looping tunnel into a healthy-looking one.
- No emoji. Documentation uses placeholders only, per `.claude/rules/public-repo-placeholders.md`.
- Every task ends with `cargo fmt --check`, `cargo test` and `cargo clippy --all-targets -- -D warnings` clean.

## What is kept and what is dropped

The classifier is the whole design; everything else is plumbing.

| Line | Verdict | Why |
|---|---|---|
| `debug1: Local forwarding listening on 127.0.0.1 port 8443.` | keep | The forward came up. Without it a jump tunnel's log is silent until a client arrives |
| `debug1: Connection to port 8443 forwarding to 10.0.0.10 port 443` | keep | One per client. This is the line the Metrics pane's `active` count corresponds to |
| `debug1: channel 2: new direct-tcpip` | keep | ssh's own per-connection line; the pair above and this one are what a reader counts |
| `Listening on port [15432].` | keep | gcloud `start-iap-tunnel`'s equivalent, at `--verbosity=info` |
| `debug1: *` (everything else) | drop | Key exchange, host-key algorithms, channel bookkeeping, keepalives. Thousands of lines per session |
| `debug2: *`, `debug3: *` | drop | Only reachable at the future `debug` level; never informative at `info` |
| `WARNING:`, `ERROR:`, `error:` | keep | Always |
| Anything unmatched | **keep** | A filter that drops what it does not recognise hides the next unfamiliar failure. The default is to keep |

The last row is the rule that makes this safe to ship: the classifier has a deny-list, not an allow-list.

## Review Focus

Input classes no task's tests exercise that are most likely to bite someone, most likely first. Each has its test pinned to the task that owns the code.

1. **A child that writes a partial line and then exits** — the last line has no `\n`. It must still reach the log, not be swallowed by a line-oriented reader. (Task 1)
2. **A line longer than any sane buffer**, such as a gcloud stack trace on one line. Must not panic or truncate silently. (Task 1)
3. **Invalid UTF-8 from the child** — ssh can emit a raw byte in a host-key fingerprint error. Must be lossy, never a crash. (Task 1)
4. **SIGTERM to the supervisor while the child is mid-connection** — launchd stops a tunnel this way. The child must die too, or `gtunnel stop` leaves an orphaned gcloud holding the port. (Task 3)
5. **The child failing to exec at all** (gcloud removed mid-session) — the supervisor must write why and exit non-zero, not exit 0 silently. (Task 3)

---

### Task 1: The line classifier

A pure function over one line. No process, no IO, no tunnel.

**Files:**
- Create: `src/logfilter.rs`
- Modify: `src/main.rs` (add `mod logfilter;`)

**Interfaces:**
- Consumes: nothing
- Produces: `pub fn keep(line: &str) -> bool` and `pub fn format_line(line: &str) -> String`. The stamp is `humantime::format_rfc3339_seconds(std::time::SystemTime::now())`, which is what `log_event` (`src/daemon/mod.rs:220`) already writes — one log must not carry two timestamp formats. Sanitise control characters the same way `log_event` does, so a line from the child cannot forge a second one.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_lines_that_say_a_tunnel_works_are_kept() {
    assert!(keep("debug1: Local forwarding listening on 127.0.0.1 port 8443."));
    assert!(keep("debug1: Connection to port 8443 forwarding to 10.0.0.10 port 443"));
    assert!(keep("debug1: channel 2: new direct-tcpip [direct-tcpip] (inactive timeout: 0)"));
    assert!(keep("Listening on port [15432]."));
}

#[test]
fn ssh_bookkeeping_is_dropped() {
    assert!(!keep("debug1: kex: algorithm: curve25519-sha256"));
    assert!(!keep("debug1: Authenticating to compute.1234 as 'you'"));
    assert!(!keep("debug2: channel 2: window 2097152 sent adjust 65536"));
    assert!(!keep("debug3: send packet: type 98"));
}

#[test]
fn a_line_the_filter_does_not_recognise_is_kept() {
    assert!(keep("Something nobody has seen before"));
    assert!(keep("WARNING: your credentials expire in 5 minutes"));
    assert!(keep(""));
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --bin gtunnel logfilter`
Expected: FAIL, `cannot find function keep in this scope`.

- [ ] **Step 3: Implement `keep(line: &str) -> bool` in `src/logfilter.rs`**

A deny-list: a line starting `debug2:` or `debug3:` is dropped; a line starting `debug1:` is dropped unless it contains one of the three kept substrings; everything else is kept.

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test --bin gtunnel logfilter`
Expected: PASS.

- [ ] **Step 5: Add the Review Focus tests for this task**

A line with no trailing newline, a 1 MiB line, and a line holding invalid UTF-8 (built from `String::from_utf8_lossy(&[0xFF])`). Each asserts `keep` returns without panicking and gives the documented verdict.

- [ ] **Step 6: Commit**

```bash
git add src/logfilter.rs src/main.rs
git commit -m "feat: a classifier for the lines a tunnel's log is worth carrying"
```

---

### Task 2: Timestamping and the stream reader

Turning a child's two pipes into an ordered, stamped, filtered line stream.

**Files:**
- Modify: `src/logfilter.rs`
- Test: same file

**Interfaces:**
- Consumes: `keep` from Task 1
- Produces: `pub async fn pump<R: AsyncBufRead + Unpin, W: Write>(reader: R, out: &mut W) -> std::io::Result<()>` — reads lines, drops what `keep` rejects, writes `format_line` of the rest, flushing each.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn a_pump_keeps_stamps_and_flushes_only_the_informative_lines() {
    let input = "debug1: kex: algorithm: curve25519-sha256\n\
                 debug1: Local forwarding listening on 127.0.0.1 port 8443.\n\
                 debug2: send packet: type 98\n\
                 trailing line with no newline";
    let mut out = Vec::new();
    pump(input.as_bytes(), &mut out).await.unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.contains("Local forwarding listening on"), "{text}");
    assert!(text.contains("trailing line with no newline"), "{text}");
    assert!(!text.contains("kex:"), "{text}");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --bin gtunnel a_pump_keeps_stamps`
Expected: FAIL, `cannot find function pump`.

- [ ] **Step 3: Implement `pump`**

`tokio::io::AsyncBufReadExt::lines()` loses a trailing line without `\n` on some paths — verify against the test rather than assuming; if it does, read to end and split manually.

- [ ] **Step 4: Run it and watch it pass**

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/logfilter.rs
git commit -m "feat: stamp and flush each line a tunnel's child process writes"
```

---

### Task 3: The `__supervise` subcommand

**Files:**
- Modify: `src/cli.rs` (a hidden subcommand), `src/main.rs` (dispatch)
- Create: `src/commands/supervise.rs`
- Test: `src/commands/supervise.rs`

**Interfaces:**
- Consumes: `logfilter::pump`
- Produces: `pub async fn run(argv: Vec<String>) -> Result<std::process::ExitCode>`

The clap variant is `#[command(hide = true)] Supervise { #[arg(last = true)] argv: Vec<String> }`, invoked as `gtunnel __supervise -- /path/to/gcloud compute ssh ...`. Hidden because it is not a thing a person runs; `--help` must not grow a line for it.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn the_supervisor_exits_with_the_childs_status() {
    let code = run(vec!["/bin/sh".into(), "-c".into(), "exit 3".into()]).await.unwrap();
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(3)));
}

#[tokio::test]
async fn a_child_that_cannot_be_executed_says_so_and_fails() {
    let err = run(vec!["/nonexistent/gcloud".into()]).await.unwrap_err();
    assert!(err.to_string().contains("/nonexistent/gcloud"), "{err}");
}

#[tokio::test]
async fn both_pipes_reach_the_log() {
    // sh writing one informative line to stdout and one to stderr
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --bin gtunnel supervise`
Expected: FAIL, module not found.

- [ ] **Step 3: Implement `run`**

Spawn with `Stdio::piped()` on both, pump both concurrently with `tokio::join!`, wait, return the child's code. Put the child in its own process group (`libc::setsid` via `pre_exec`, which is `unsafe`) so Step 5's signal handling can take the whole group.

- [ ] **Step 4: Run them and watch them pass**

Expected: PASS.

- [ ] **Step 5: Add the signal test and handling** (Review Focus 4)

On SIGTERM or SIGINT the supervisor kills the child's process group, waits for it, and exits. The test spawns a `sleep 60` child, sends the supervisor SIGTERM, and asserts the child is gone within a bounded wait — never an unbounded one.

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs src/main.rs src/commands/supervise.rs
git commit -m "feat: gtunnel supervises the process behind a tunnel and filters its output"
```

---

### Task 4: Point the service managers at the supervisor

**Files:**
- Modify: `src/daemon/mod.rs` (`service_spec`)
- Test: `src/daemon/mod.rs`, and the launchd and systemd unit tests that assert on `program`

**Interfaces:**
- Consumes: the `__supervise` argv shape from Task 3
- Produces: a `ServiceSpec` whose `program` is the running gtunnel binary and whose `args` are `["__supervise", "--", <gcloud>, <gcloud args...>]`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_service_runs_gtunnel_which_runs_gcloud() {
    let spec = service_spec(&cfg, &jump_tunnel()).unwrap();
    assert!(spec.program.ends_with("gtunnel"), "{:?}", spec.program);
    assert_eq!(spec.args[0], "__supervise");
    assert_eq!(spec.args[1], "--");
    assert!(spec.args[2].ends_with("gcloud"), "{:?}", spec.args);
    assert!(spec.args.iter().any(|a| a == "LogLevel=DEBUG"),
            "ssh must still run at DEBUG");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --bin gtunnel a_service_runs_gtunnel`
Expected: FAIL — `program` is gcloud.

- [ ] **Step 3: Implement**

`std::env::current_exe()` is the program. Keep `daemon_env` exactly as it is: the child is still gcloud, so `PATH`, `PYTHONUNBUFFERED` and `CLOUDSDK_CORE_DISABLE_PROMPTS` are all still needed, now inherited through the supervisor.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test`
Expected: PASS. Existing launchd and systemd tests that assert the plist or unit names gcloud will fail here — they are asserting the old design and should be updated to the new one, not worked around.

- [ ] **Step 5: Re-install an existing tunnel and read its log**

Run, against a throwaway config dir:
```
GTUNNEL_CONFIG_DIR=$D gtunnel start <name>
```
Expected: the log holds stamped lines, `Local forwarding listening on` present, no `debug1: kex`. Capture the real output for Task 6.

- [ ] **Step 6: Commit**

```bash
git add src/daemon/
git commit -m "feat: the service manager runs gtunnel, which runs gcloud"
```

---

### Task 5: Write the kept tail before truncating, not after

Independent of the supervisor and worth its own commit.

`trim_log` (`src/daemon/mod.rs:175`) calls `set_len(0)`, then `seek(0)`, then `write_all(body)`. The child holds the same file in **append** mode, so between the truncation and the write its next line lands at offset 0 — and `write_all` then overwrites it from offset 0. The existing comment says "a line it writes between the read and the truncation is lost"; the real window is between the truncation and the write, and the effect is a corrupted line rather than a lost one. A failure of `write_all` in that window loses the whole log.

**Files:**
- Modify: `src/daemon/mod.rs`
- Test: `src/daemon/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_trim_never_leaves_the_log_empty() {
    // Write a log past LOG_CAP, hold a second handle open in append mode,
    // trim, and assert the file is non-empty at every observable point and
    // that the kept tail is intact and starts with the dropped-lines notice.
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --bin gtunnel a_trim_never_leaves`
Expected: FAIL.

- [ ] **Step 3: Implement**

Write the body to a sibling temp file in the same directory, then `rename` it over the log. Rename is atomic, so no reader sees an empty or half-written log. The cost is that a child holding the old inode in append mode keeps writing to the unlinked file and its subsequent lines are lost — which is the trade the current code already makes, now bounded to lines rather than to the whole file. State that in the function's doc comment, replacing the sentence that describes the window wrongly.

- [ ] **Step 4: Run it and watch it pass, then the whole suite**

Run: `cargo test --bin gtunnel a_trim_never_leaves && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/mod.rs
git commit -m "fix: a trimmed log is replaced atomically, never emptied first"
```

---

### Task 6: Documentation

**Files:**
- Modify: `docs/architecture.md` ("Access mode", "Jump tunnels", "Process model"), `docs/troubleshooting.md` ("Who writes what"), `CHANGELOG.md`

- [ ] **Step 1: Update the process model**

The service's child is now gtunnel, and gcloud is its grandchild. The Mermaid process-model diagram names the processes and is wrong as it stands; it stays Mermaid (a process flow) and gains the supervisor.

- [ ] **Step 2: Say what the log now holds**

"Who writes what" in `docs/troubleshooting.md` currently attributes every `debug1:` line to ssh. Replace with: ssh still writes them at `DEBUG`, gtunnel drops the ones that carry no information, and every surviving line is stamped. Include the real captured output from Task 4 Step 5, with placeholders substituted per `.claude/rules/public-repo-placeholders.md`.

- [ ] **Step 3: Note the one thing a user loses**

A reader who wants ssh's full `debug1:` stream no longer has it in the log. Say where it does live: running the same command by hand, which `docs/troubleshooting.md` already prints.

- [ ] **Step 4: Commit**

```bash
git add docs/ CHANGELOG.md
git commit -m "docs: what a tunnel's log holds now that gtunnel filters it"
```

---

## Interaction with the log-level plan

`docs/plans/2026-10-24-unified-log-level.md` is complementary and should land **after** this one. That plan makes the level configurable and preserves today's flags at `info`; this plan makes `info` readable. Done in the other order, the log-level work ships a `debug` level whose output is unusable, and its `info` default stays as noisy as it is today. Where they touch the same lines — `src/tunnel.rs:75` and `:154`, `src/daemon/mod.rs` env — this plan does not change the flags at all, so the other plan's tasks apply unchanged.

## Self-review

- **Spec coverage.** The four decisions you made — gtunnel supervises, gcloud runs at ssh `DEBUG`, connection lines kept and the rest dropped, every line timestamped — map to Tasks 3, 4, 1 and 2. The truncation-ordering defect maps to Task 5.
- **Step scan.** No step says "handle edge cases". Code blocks are test bodies and signatures; the two places that carry an algorithm (the deny-list in Task 1 Step 3, the rename in Task 5 Step 3) carry it because the signature does not determine it.
- **Type consistency.** `keep` and `pump` are named identically in Tasks 1, 2 and 3. The argv shape in Task 3 and Task 4 is the same four-element prefix.
- **Review Focus.** Five entries, each pinned to a task that owns the code: three to Task 1, two to Task 3.
- **Proportion.** Six tasks over roughly 300 lines of new code, most of it the classifier and its tests.
