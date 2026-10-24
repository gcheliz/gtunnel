# Design Gaps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three places the shipped TUI still differs from the footer design (`gtunnel footer action menu` canvas): the failed state does not lead with restart, an empty account shows a blank list, and `?` opens a one-column list that does not match the design's keymap page. Then clear the leftovers the earlier plans deferred.

**Architecture:** The keymap becomes one table that the footer, the help page and a test all read, so the three cannot drift. The help page is laid out from that table in one or two columns by width. The two small gaps are local to `footer_line` and `render_tunnels`.

**Tech Stack:** Rust, ratatui 0.30. No new dependency.

**Spec:** The design canvas boards `Main`, `States` and `Keys` and the Decisions below.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Failed footer | `R restart` leads: key in `theme::WARN` bold, label in white; the other actions keep the normal style | The board's "restart leads"; the failed tunnel's next move should be the loudest thing in the footer |
| Empty list | Under the `NAME MODE URL` header, one muted row `no tunnels in this account` | The board's empty state; the footer already names `a` and `m` |
| Help layout | Groups TUNNEL, THE LIST, THIS TUNNEL'S URL AND LOG, THE PROGRAM; two columns at 90 cells or more inside the modal, one below | The board; a single column overflows short terminals less when it is the fallback |
| Help wording | Describe what the code does, not what the board says: `m` is "import an ephemeral run as managed", `;` is "cycle accounts" | The board's `m` text ("from a file") and `;` text ("switch") do not match the handlers |
| Help extras | Keep `up/k`, `down/j` and `Ctrl+Z`; add `?` and `Esc` | They are bound; the board lists `j k` and `Esc` only because it predates them |
| Case note | One paragraph stating that `s/S`, `a/A` and `r/R` differ by case | The footer never shows both halves, so the help page is where the trap is stated |
| Count line | `N keys bound - the footer shows at most M`, both computed from the table | A hard-coded count rots on the next key |
| Single source | One `KEYMAP` table: key, label, group, and where it shows (footer state or none) | The doc comment on `Action` already claims footer and overlay share one shape; today they do not |
| 48-column footer | Unchanged: it keeps `; account` | The board's caption says the app group never moves; the way out is the safer behaviour |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on macOS and Linux (Linux leg per `docs/plans/2026-10-23-tunnel-feedback.md` Global Constraints).
- No new dependency; no new `Color::Rgb`.
- Comments explain why, in the voice of the surrounding code.
- The footer keeps its shedding rules and its existing tests.
- The CLI `gtunnel list` output does not change.
- Do not commit the other session's identifier rewrites in `src/cli.rs`, `src/probe.rs`, `src/tunnel.rs`, `docs/usage.md` or the three bastion hunks of `src/tui/mod.rs`.

## Review Focus

- Every key the help page lists is handled: a test feeds each one through `handle_key` and asserts it changes state, returns an op or sets a status.
- Every footer action appears in the help page.
- The help modal never draws a row wider than the terminal and never panics at 10x4, 20x8 or 1x1.
- At 60x24 the one-column help page scrolls or truncates without hiding `Esc`.
- The failed footer at 48 columns still keeps `R restart` as its first action.

## Task 1: Failed state leads with restart

**Files:** modify `src/tui/ui.rs`, `src/tui/mod.rs` (tests)

- [x] Write a failing test: for a failed selection the first footer span holding `R` is styled `WARN` and bold and its label white; the same footer for a running selection has no `WARN` span.
- [x] Add `lead: bool` to `Action`; set it on `R` in `ACTIONS_ERROR`; draw a lead action with the lead style in `group_spans`.
- [x] `cargo test` passes.

## Task 2: Empty-state row

**Files:** modify `src/tui/ui.rs`, `src/tui/mod.rs` (tests)

- [x] Write a failing render test: with no entries the list pane shows `no tunnels in this account` on the row under the header, and the pane title reads `TUNNELS (0)`.
- [x] Render the row in `render_tunnels` when `app.entries` is empty.
- [x] `cargo test` passes.

## Task 3: One keymap table

**Files:** modify `src/tui/ui.rs`, `src/tui/app.rs` (tests only)

- [x] Write failing tests: every `Action` in the four footer sets has an entry in `KEYMAP`; every `KEYMAP` key is handled by `handle_key` in `InputMode::Normal` (the test drives each and asserts a visible effect).
- [x] Add `KEYMAP: &[KeyDoc { key, label, group }]`, groups as an enum, and derive the footer labels from it so a label is written once.
- [x] `cargo test` passes.

## Task 4: The help page

**Files:** modify `src/tui/ui.rs`, `src/tui/mod.rs` (tests)

- [x] Write failing render tests: at 120x40 the modal shows the four group headings in two columns, the case note, the count line and an `Esc back  q quit` footer; at 60x24 it is one column and `Esc` is still on screen; a size matrix (10x4, 20x8, 40x12, 60x24, 120x40) never panics and no row is wider than the terminal.
- [x] Replace `HELP_SECTIONS` and `render_help_modal` with a layout built from `KEYMAP`; the metrics-cadence lines move under the count line.
- [x] `cargo test` passes.

## Task 5: Docs

**Files:** modify `docs/usage.md`, `CHANGELOG.md`

- [x] `docs/usage.md`: the keymap table follows the help page groups; one sentence on the failed-state lead and the empty-state row.
- [x] `CHANGELOG.md`: one entry under Unreleased.

## Task 6: Evidence

- [ ] Render the failed footer at 120 and 48 columns, the empty list at 120x12, and the help page at 120x40 and 60x24 from a scratch copy; paste the rows.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` output shown, on both platforms.

## Deferred

- Shipped: the instance and domain pickers and the confirmation dialogs now size through `dialog_rect` (see `2026-10-24-open-items.md` A2).
- Shipped: the persistent list scroll offset, held on `App` as `list_offset` (open-items A3).
- Shipped: `FIELD_BG` in `src/tui/theme.rs` (open-items A5).
- Metrics sparkline against real data: not seen in the scratch renders.
- Shipped: `STACK_BELOW` is 121 after the measurement in open-items A7.
- A real browser run of `gcloud auth login` and the Linux gate leg: never run.
