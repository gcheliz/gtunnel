# Unified Log Level Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** One log level, set in one place, that governs every process gtunnel starts and every gcloud call it makes, defaulting to `info`.

**Architecture:** A `LogLevel` enum is the single source of truth. One function turns it into the flags each underlying tool understands, because gcloud and ssh use different scales and the same literal string means different things to each. Every call site asks that function instead of carrying a string of its own.

**Tech Stack:** Rust, clap, serde.

**Spec:** none written; this plan is the spec. The behaviour it preserves is recorded in `docs/troubleshooting.md` ("Who writes what") and in the comments at `src/tunnel.rs:11-15` and `:66-75`.

## Global Constraints

- The default is `info`, and `info` must produce **exactly today's flags**: gcloud `--verbosity=info`, ssh `-o LogLevel=DEBUG`. No level change ships as part of this work.
- `PYTHONUNBUFFERED=1` (`src/daemon/mod.rs:48`) is not a verbosity and is never conditional. It is a buffering fix; without it a line waits for 4 KiB or for the process to exit at any level.
- No emoji. Documentation uses placeholders only, per `.claude/rules/public-repo-placeholders.md`.
- Every task ends with `cargo fmt --check`, `cargo test` and `cargo clippy --all-targets -- -D warnings` clean.

## The mapping, which is the whole design

| gtunnel level | gcloud `--verbosity` | ssh `-o LogLevel` | What the user gets |
|---|---|---|---|
| `quiet` | `warning` | `ERROR` | Failures only. A working tunnel says nothing |
| `info` (default) | `info` | `DEBUG` | The listener or forward coming up, and one line per client connecting |
| `debug` | `debug` | `DEBUG3` | The above plus gcloud's and ssh's own internals, for diagnosing a tunnel that will not carry traffic |

`info` maps to ssh `DEBUG`, one step further than the name suggests, and that is deliberate. ssh's
levels run `QUIET < FATAL < ERROR < INFO < VERBOSE < DEBUG`, and both lines a user means by
"tell me it is working" — the forward coming up and one per client — are `debug()` calls. At
`VERBOSE` ssh prints three lines for a whole session and nothing per connection. Mapping
`info` to ssh `INFO` would make jump and public tunnels silent again.

`quiet` maps to ssh `ERROR`, not `QUIET`, because `QUIET` suppresses the failure messages that
are the only reason to read a quiet tunnel's log at all.

## Where the value comes from

Precedence, highest first: `--log-level <level>` on the command line, then `GTUNNEL_LOG_LEVEL`
in the environment, then `log_level` in `config.toml`, then `info`.

The level is global, not per account and not per tunnel: it answers "how much do I want to see
right now", which is a property of the moment rather than of a tunnel. A tunnel that needs more
detail gets it by restarting under a higher level, which is already how a changed setting
reaches a running service.

## Review Focus

- An unparseable level in `config.toml` or `GTUNNEL_LOG_LEVEL`: a typo must not make every
  command fail, nor silently become `info` with no word to the user.
- Case and spelling: `INFO`, `Info` and `info` are the same level; `verbose` is not a level and
  must say so, naming the three that are.
- A running tunnel started under the old level: its service carries the old flags until it is
  restarted, and nothing on screen should imply otherwise.
- `quiet` must not suppress the lines `diagnose_log_line` matches on (`4003`, `4033`,
  `Reauthentication required`), or the TUI's log annotations go blank exactly when they matter.
- The level reaching `gcloud auth print-access-token` (`src/gcp/gcloud.rs:126`): at `debug` that
  call prints request detail on every token fetch, which is noisy but correct; at `quiet` it
  must still surface an auth failure.

---

### Task 1: The LogLevel type and its mapping

**Files:**
- Create: `src/loglevel.rs`
- Modify: `src/main.rs` (one `mod` line)
- Test: `src/loglevel.rs`

**Interfaces:**
- Produces: `pub enum LogLevel { Quiet, Info, Debug }` with `Default` = `Info`,
  `FromStr` (case-insensitive), `Display`, `serde::Serialize`/`Deserialize`,
  `pub fn gcloud_verbosity(&self) -> &'static str` and `pub fn ssh_log_level(&self) -> &'static str`.

- [x] **Step 1: Write the failing tests**

```rust
#[test] fn the_default_level_is_the_flags_the_tunnels_ship_with_today() {
    let l = LogLevel::default();
    assert_eq!(l, LogLevel::Info);
    assert_eq!(l.gcloud_verbosity(), "info");
    assert_eq!(l.ssh_log_level(), "DEBUG");
}
#[test] fn every_level_maps_to_both_tools() { /* the table above, all three rows */ }
#[test] fn a_level_is_read_whatever_its_case() {
    assert_eq!("INFO".parse::<LogLevel>().unwrap(), LogLevel::Info);
    assert_eq!("Debug".parse::<LogLevel>().unwrap(), LogLevel::Debug);
}
#[test] fn an_unknown_level_names_the_three_that_exist() {
    let err = "verbose".parse::<LogLevel>().unwrap_err().to_string();
    assert!(err.contains("quiet") && err.contains("info") && err.contains("debug"), "{err}");
}
#[test] fn quiet_still_lets_ssh_report_a_failure() {
    assert_eq!(LogLevel::Quiet.ssh_log_level(), "ERROR");
}
```

- [x] **Step 2: Run them** — Expected: FAIL, `LogLevel` not defined.
- [x] **Step 3: Implement the enum and both mapping functions** exactly as the table above gives them. The `FromStr` error names the three valid levels.
- [x] **Step 4: Run them** — Expected: PASS.
- [x] **Step 5: Commit** — `feat: one log level, mapped to each tool's own scale`

---

### Task 2: Where the value comes from

**Files:**
- Modify: `src/config.rs` (the `Config` struct), `src/cli.rs` (the `Cli` struct)
- Test: `src/config.rs`

**Interfaces:**
- Consumes: `LogLevel` from Task 1.
- Produces: `Config.log_level: LogLevel` (`#[serde(default)]`), `Cli.log_level: Option<LogLevel>`
  as a global `--log-level` argument, and
  `pub fn resolve_log_level(flag: Option<LogLevel>, env: Option<&str>, cfg: LogLevel) -> Result<LogLevel>`.

- [x] **Step 1: Write the failing tests**

```rust
#[test] fn the_flag_beats_the_environment_which_beats_the_file() {}
#[test] fn nothing_set_anywhere_is_info() {}
#[test] fn a_config_without_the_field_still_loads_and_is_info() {}
#[test] fn a_bad_value_in_the_environment_is_an_error_naming_the_variable() {}
```

- [x] **Step 2: Run them** — Expected: FAIL, `resolve_log_level` not defined.
- [x] **Step 3: Implement.** `resolve_log_level` is pure and takes the environment string rather than reading it, so the precedence is testable without touching the process environment. A malformed `GTUNNEL_LOG_LEVEL` is an error that names the variable; a malformed `config.toml` value is already a serde error at load.
- [x] **Step 4: Run them** — Expected: PASS.
- [x] **Step 5: Commit** — `feat: the log level comes from the flag, the environment or the file`

---

### Task 3: Every call site asks for the level

**Files:**
- Modify: `src/tunnel.rs` (`gcloud_common` at :8, the jump branch at :75, `expose_argv` at :154), `src/gcp/gcloud.rs:126`
- Test: `src/tunnel.rs`

**Interfaces:**
- Consumes: `LogLevel`, resolved once at startup and threaded to the argv builders.

- [x] **Step 1: Write the failing tests**

```rust
#[test] fn an_access_tunnel_carries_the_levels_verbosity() {}
#[test] fn a_jump_tunnel_carries_the_levels_ssh_level_after_the_separator() {}
#[test] fn a_public_tunnel_carries_the_levels_ssh_level_after_the_separator() {}
#[test] fn the_default_level_leaves_every_argv_byte_identical_to_today() {}
```

The last one is the guard this task exists for: build each argv at `LogLevel::Info` and assert
the exact vectors the current tests assert at `src/tunnel.rs:335-348` and `:372`, so a mapping
slip cannot ship as a silent behaviour change.

- [x] **Step 2: Run them** — Expected: FAIL to compile, the builders take no level.
- [x] **Step 3: Implement.** Replace the three hardcoded strings with `level.gcloud_verbosity()` and `level.ssh_log_level()`. Keep the `-o LogLevel=` option in its current position, after the `--` separator, which an existing test asserts. Update the existing assertions at `:345`, `:372` and `:393` to the level they now pass rather than deleting them.
- [x] **Step 4: Run them** — Expected: PASS, and the whole suite green.
- [x] **Step 5: Commit** — `feat: the tunnels and the gcloud calls take the configured level`

---

### Task 4: Documentation

**Files:**
- Modify: `docs/architecture.md` (both captured command blocks), `docs/troubleshooting.md` ("Who writes what", "How large a log gets"), `docs/usage.md` (the new flag), `README.md` (if it lists configuration), `CHANGELOG.md`

- [x] **Step 1: Write the mapping table into `docs/troubleshooting.md`**, as the table above, with the two sentences explaining why `info` maps to ssh `DEBUG` and `quiet` to ssh `ERROR`. This is the one place the table lives; every other document links to it.
- [x] **Step 2: Make the captured commands in `docs/architecture.md` show the default level** and say that both flags follow the configured level, pointing at the table.
- [x] **Step 3: Say in "How large a log gets" what each level costs.** Today's figure, around 90 lines per ssh handshake, is the `info` figure; `debug` is substantially more and `quiet` is almost nothing. The 8 MiB cap and 2 MiB tail bound all three and do not change.
- [x] **Step 4: Document `--log-level`, `GTUNNEL_LOG_LEVEL` and `log_level` in `docs/usage.md`**, with the precedence, and note that a running tunnel keeps the level it started under until `gtunnel restart`.
- [x] **Step 5: Run the sweep** — `git ls-files | grep -v '^docs/plans/' | xargs grep -nIE '<the alternation in .claude/rules/public-repo-placeholders.md>' 2>/dev/null` — Expected: no output.
- [x] **Step 6: Commit** — `docs: the log level, its mapping, and what each level costs`

---

## Rulings made while executing

| # | Ruling | Why | Cost if wrong |
|---|---|---|---|
| 1 | The level reaches the argv builders through a process-wide value, not through a parameter on every caller | `service_spec` alone sits behind twelve call sites that have no business knowing about verbosity; the builders still take the level explicitly, so tests pin any level | A call site could build an argv before the level resolves. Checked: every builder needs an `Account`, which only exists after `load_config`, so none can |
| 2 | An explicit `--log-level` returns before `GTUNNEL_LOG_LEVEL` is parsed, so a bad value in the environment is not an error when the flag is set | The flag is the way out of a typo in a shell profile; validating the thing being overridden would defeat it | A typo in the environment goes unreported for that one invocation |
| 3 | The process-wide value was split in review into a startup snapshot and an override, and `service_spec` resolves against the `Config` it is handed | A TUI session outlives the snapshot: a config edit has to reach the service file it installs an hour later. A flag and an environment variable cannot change under a running process; a file can | A transient `gcloud auth print-access-token` still follows the startup level rather than a mid-session edit. Its output is captured and discarded, so nothing persists |
| 4 | `CHANGELOG.md` is listed in Task 4's Files and was deliberately not touched | `release-plz.toml` sets `changelog_update = true`, so the changelog is generated from commit subjects since the last tag; a hand-written entry would be duplicated or overwritten | Nothing, provided the commit subjects carry their conventional prefixes. They do |
