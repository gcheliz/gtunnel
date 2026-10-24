# Open Items Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every item left open after the TUI layout work (header, list, resize, account field, keymap page), and give each open item from the earlier plans an owner and an order, so nothing waits only in a plan's Deferred section.

**Architecture:** Part A is new work in the TUI, each task independent and small. Part B does not repeat other plans: it names the plan that already specifies the work, says what state it is in, and what has to be confirmed before running it.

**Tech Stack:** Rust, ratatui 0.30. No new dependency.

**Spec:** This plan is its own spec for Part A. Part B defers to the plans it names.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Order | A1 to A4 first, then A5 to A8, then Part B | A1 and A2 are visible defects a user can hit at 80x24; the rest is polish or evidence |
| Help page at small sizes | Scroll with `j`/`k`, `PgUp`/`PgDn` and the arrows; the page shows a position counter instead of the "N more keys" row | The cut notice is a stopgap; a keymap page that cannot be read in full fails its one job |
| Picker and dialog width | Dialogs size to their text and the terminal (`min(text, width - 4)`), never a fixed 50 | Fixed width overflows below 54 columns |
| List scroll offset | Held on `App`, moved only when the selection leaves the window | Rebuilding it each draw sticks the selected row to the bottom once scrolled |
| Account field fill | A `FIELD_BG` constant in `theme.rs`, one step off `SELECTED_BG` | The field and the selected row read as the same control today |
| Stacked layout at 110-120 columns | Measure first; change only if 100-column URLs are cut on real data | A layout change with no observed failure is churn |
| Load failure on first load | A muted `could not load tunnels` row beside the status error, not `loading` forever | `loading` for a permanently broken config reads as a hang |
| Plans in git | Strip any artifact link before a plan is committed | The global rule forbids Claude links in anything that lands in a repo, and two plans carry one |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on macOS and Linux (Linux leg per `docs/plans/2026-10-23-tunnel-feedback.md` Global Constraints).
- No new dependency; no new `Color::Rgb` beyond `FIELD_BG` if A5 is taken.
- Comments explain why, in the voice of the surrounding code.
- The CLI `gtunnel list` output does not change.
- Never stage another session's uncommitted edits (`src/cli.rs`, `src/probe.rs`, `src/tunnel.rs` and identifier rewrites): stage by hunk and build the staged tree in a scratch copy before committing.

## Review Focus

- A1: at 80x24 every one of the 23 keys is reachable on the help page.
- A2: no dialog or picker draws a row wider than the terminal at 10x4, 20x8, 40x12 and 48x20.
- A3: the selected row stays put while moving inside the window; it scrolls only at the edge.
- A7: the failed-load row appears only when the load failed, never during the first load.

## Part A: work in this repository

### Task A1: Scrollable help page

**Files:** modify `src/tui/ui.rs`, `src/tui/app.rs`, `src/tui/mod.rs` (tests)

- [x] Write failing tests: at 80x24, after scrolling to the end, the THE PROGRAM group and the case note are on screen; the scroll position never exceeds the page; `Esc`, `?` and `q` still close the page and reset the position.
- [x] Add `help_scroll: usize` to `App`; handle `j`, `k`, arrows, `PgUp`, `PgDn`, `g`, `G` in `InputMode::Help` only.
- [x] Replace the `N more keys` row with a position counter (`3-14 of 31 rows`) drawn only when the page is cut; two-column mode scrolls both columns together.
- [x] Add the scroll keys to `KEYMAP` under THE PROGRAM so the page documents itself; update the literal count assertion.
- [x] `cargo test` passes.

### Task A2: Dialogs and pickers at narrow sizes

**Files:** modify `src/tui/ui.rs`, `src/tui/mod.rs` (tests)

- [ ] Read-only pass first: render the add, edit, confirm and import dialogs and the instance and domain pickers at 10x4, 20x8, 40x12, 48x20 and 60x24; list what overflows, clips or panics, as a table in this section.
- [x] Write a failing test per defect found; fix with `min(text, width - 4)` sizing and wrapped prompt text.
- [x] A size-matrix test asserts no panic and no row wider than the terminal for every dialog and picker.
- [x] `cargo test` passes.

### Task A3: Persistent list scroll offset

**Files:** modify `src/tui/app.rs`, `src/tui/ui.rs`, `src/tui/mod.rs` (tests)

- [x] Write a failing test: 20 entries, a pane of 8 rows, select 10 then 9; the second draw leaves the window where it was instead of moving the selected row to the bottom.
- [x] Hold `list_offset` on `App`; `render_tunnels` clamps it so the selection is inside the window and otherwise leaves it alone; a resize clamps it too.
- [x] `cargo test` passes.

### Task A4: Load failure on the first load

**Files:** modify `src/tui/app.rs`, `src/tui/ui.rs`, `src/tui/mod.rs` (tests)

- [x] Write a failing test: after `Msg::EntriesFailed` on an app that has not loaded, the list shows `could not load tunnels`, not `loading`; a later successful load replaces it.
- [x] Track `load_failed: bool` beside `loaded`; the row follows it.
- [x] `cargo test` passes.

### Task A5: A field background of its own

**Files:** modify `src/tui/theme.rs`, `src/tui/ui.rs`

- [x] Add `FIELD_BG` (an indexed colour one step from `SELECTED_BG`) and use it for the account field; the theme test that names every exported colour gains it.
- [ ] Render the header beside a selected row and check by eye in a real terminal that the two read as different things; if they still do not, say so and stop.

### Task A6: Weak handlers in the keymap test

**Files:** modify `src/tui/app.rs`, `src/tui/mod.rs` (tests)

- [x] `c` and `o` reach the real clipboard and browser for any selected entry. Put the clipboard and opener behind a small trait or a function field `App` can replace in tests, then make the handled-keys test press both on a selected entry and assert the status line.
- [x] `cargo test` passes.

### Task A7: Measure before touching the 100-column layout

Measured with scratch data (a 28-cell url, a 49-cell url and an unmanaged row) at 24 rows,
side by side, before the change:

| Width | List cells | `https://api.demo.example.com` (28) | 49-cell url | `[not managed by gtunnel]` |
|---|---|---|---|---|
| 100 | 46 | cut (`https://api.demo.ex…`) | cut (`https://checkout-ca…`) | cut (`[not managed by gtu…`) |
| 110 | 50 | cut (`https://api.demo.example…`) | cut (`https://checkout-callbac…`) | whole |
| 120 | 55 | whole | cut (`https://checkout-callback.sa…`) | whole |

- [x] With the real tunnel names (scratch data, never the real config), render 100, 110 and 120 columns at 24 rows and record which URLs are cut.
- [x] If none is cut, close this item with the table and change nothing. If one is, write the change as its own task here before touching code. Cuts found; approved option 1: `STACK_BELOW` 100 to 121.

After the change, at 100x24 and 120x24 the list is the full width, 7 rows tall (border, title, header, three entries), and every url and the unmanaged note are whole. The cost is height: Details shows 4 rows (name, url, target, relay; `relay port` and below are cut), Logs keeps its title and newest line, and Metrics is hidden at 24 rows. Tests: `from_100_to_120_columns_the_list_shows_every_url_uncut`, `from_121_columns_the_panes_are_side_by_side`, `at_100x24_the_stacked_list_keeps_rows_and_the_panes_stay_usable`.

### Task A8: Evidence that was never run

- [x] Linux gate leg: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` on Linux (CI run or container); show the output. Done via `act -j test` in a scratch copy: 324 + 6 tests pass. The first run failed one test only because act's container has no init process to reap orphans; with `--container-options "--init"` it is green.
- [ ] Metrics sparkline against real metrics data: run the TUI against a scratch config with a stub that returns latency samples, and paste the Metrics pane at 120x40.
- [x] A real browser run of `gcloud auth login` through the startup check, per `docs/plans/2026-10-23-startup-login.md` Task 5: cancel it, show the message and `EXIT=1`. Run on a pty with a scratch config and an empty gcloud config dir: the interrupt sent to the gcloud child only gave the message and `EXIT=1`. Ctrl-C on the terminal reaches gtunnel too and kills it by signal (exit -2) with no message; that is the shell, not a cancel.
- [x] Run by the user (a browser login cannot be driven from here). Build first with `cargo build --release`. In a terminal, so stdin and stdout are TTYs, with a scratch config and an empty gcloud config dir so the real ones are untouched. The no-TTY case was already run and gave the message with `EXIT=1`.
  - With a TTY, cancel the login (close the browser tab, then press Ctrl-C at the prompt): `S=$(mktemp -d); mkdir $S/cfg $S/gc; printf 'selected_account = "dev"\n[[accounts]]\nname = "dev"\ngcloud_configuration = "default"\nproject = "example-dev-1a2b"\ndefault_zone = "europe-west1-b"\n' > $S/cfg/config.toml; GTUNNEL_CONFIG_DIR=$S/cfg CLOUDSDK_CONFIG=$S/gc ./target/release/gtunnel list; echo EXIT=$?`
  - Expected: `gcloud has no usable credentials for 'dev'. Starting gcloud auth login.`, then the login's own prompt, then after the cancel `Error: gtunnel needs a logged-in Google account for 'dev'. Run gcloud auth login` and `EXIT=1`.
  - Without a TTY: `GTUNNEL_CONFIG_DIR=$S/cfg CLOUDSDK_CONFIG=$S/gc ./target/release/gtunnel list < /dev/null; echo EXIT=$?` prints only the last message, `EXIT=1`.
  - If the `!` prefix gives the command no TTY, the first command takes the no-TTY path; run it in a separate terminal window instead.

## Part B: open work that already has a plan

| Item | Plan | State found | Before running it |
|---|---|---|---|
| README wears the logo | `2026-10-23-tui-brand-layout.md` Task 6 | Task 5 shipped with the keymap page; Task 6 superseded by the README logo raster | Superseded: the README carries `docs/logo.png`, so the text-mark test no longer applies |
| One doc for how the TUI displays information | `2026-10-24-design-doctrine.md` | `docs/design.md` written against the shipped code, with its two rot-guard tests; not yet committed | Done: the plan and the doc were corrected against the shipped header, account field, list, resize and keymap work |
| Public-readiness phases (agent config privacy, version policy, SECURITY.md, coverage, `cargo-deny`) | `2026-10-23-open-source-readiness.md` | `SECURITY.md` and `CONTRIBUTING.md` are absent; `ci.yml` and `release.yml` exist | Check each phase against the repo; the identifier rewrites another session is making are part of Phase 0 and should land through that plan |
| `doctor` noticing orphaned logs; a stale log surviving a `KeepAlive` restart | `2026-10-22-gtunnel.md` Deferred | Not started | Decide whether the second needs launching the daemon through gtunnel itself, which changes what launchd runs |
| `--no-launch-browser` over SSH; checking every account at startup | `2026-10-23-startup-login.md` Deferred | Not started | Only when someone runs gtunnel over SSH or reports a confusing lazy error |
| Plan files tracked or not; checkboxes never ticked | All plans | Four plans untracked; no checkbox in any plan is ticked | Decide tracked or local; if tracked, strip artifact links from `2026-10-24-account-field.md` and `2026-10-24-design-gaps.md` first, and tick the boxes of shipped plans |

## Status

A1 to A7 are verified against the code. A2's read-only pass table was never recorded in this plan, so that box stays open (the size-matrix test it led to exists). A5's by-eye check, A8 and Part B are open. FIELD_BG, `list_offset`, the clipboard seam, `STACK_BELOW` 121 and the A4 row were in the working tree, not in a commit, when these boxes were ticked.

## Deferred

- Renaming the plan checkboxes to a status table: only if ticking them by hand proves to rot.
- A scrollable list for the instance picker's two-column form: only if A2 shows the picker unusable below 60 columns.
