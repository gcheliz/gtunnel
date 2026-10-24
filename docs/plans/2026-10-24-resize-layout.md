# TUI Resize Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every pane of the TUI adapt to the terminal size: the list shows full names and scrolls with the selection, nothing the user needs is clipped mid-word or hidden at ordinary sizes, and the Logs pane never disappears at 24 rows.

**Architecture:** Layout decisions become pure functions that take a size and return cell counts, so each is tested without a terminal: a column fitter for the list, a height budget for the right-hand column, and a wrapper for Details values. `render` chooses a stacked or side-by-side layout from the width and hands each pane its rectangle. The list renders through a `ListState` so ratatui scrolls it.

**Tech Stack:** Rust, ratatui 0.30. No new dependency.

**Spec:** No separate spec. This plan is its own spec, from the resize analysis in the session that produced it and the Decisions below.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Name cap | Removed. The name column is as wide as the longest displayed name; the suffix hiding stays | The 12-cell cap was a stopgap; the CLI list already sizes to the longest name |
| When a name is still cut | Only when the pane cannot hold name, mode, flags and a 12-cell URL; the name then shrinks to make room, floor 8 cells, with `…` | The URL is the information the list exists to show |
| Narrow layout | Below 121 columns (`STACK_BELOW`; first drafted as 100, moved after measuring cut URLs at 100 to 120, see open-items A7) the list stacks above the right-hand panes | The list gets the full width, so URLs fit; side by side leaves it 32 cells at 80 columns |
| Height priority in the right column | Details first, then Logs with a 4-row minimum, then Metrics | Logs vanished at 100x24 because Details and Metrics took fixed heights first |
| Long Details values | Wrapped under the label, continuation rows indented to the value column; the pane's height follows the wrapped rows | Clipping hid the end of a URL; the request was for wrapped information |
| Metrics | The gauge row splits onto two rows below 58 cells; Metrics shrinks, then hides, before Logs loses its minimum | Four figures on one row are clipped at 60 columns |
| List scrolling | A `ListState` following `app.selected` | A stateless `List` cannot show a selected row past the pane's height |
| Resize events | Unchanged | Redraw rides the 120 ms tick and ratatui re-reads the size on every draw; no defect found |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on macOS and Linux (Linux leg per `docs/plans/2026-10-23-tunnel-feedback.md` Global Constraints).
- No new dependency; no new `Color::Rgb`.
- Comments explain why, in the voice of the surrounding code.
- The CLI `gtunnel list` output does not change.
- The header keeps its 4 rows (`HEADER_HEIGHT_FULL`) and its tests; it still hides below 16 rows.
- Widths count characters; managed tunnel names are ASCII.

## Review Focus

- The selected entry is on screen at every size at least 12 rows tall, for lists longer than the pane.
- The Logs title and the newest log line are on screen at 100x24 for an access tunnel and for a public tunnel.
- No rendered row is wider than the terminal; no panic at 10x4, 20x8 or 1x1.
- Moving across the 100-column boundary changes the layout without losing the selection or the scroll position.
- A wrapped Details value never overwrites the label column.
- The wrapped-row count for Logs agrees with how the widget really wraps; a test uses a log line of many short words, where character division undercounts.

## Task 1: Column fitter, no name cap

**Files:** modify `src/tui/app.rs`, `src/tui/ui.rs`

- [x] Write failing tests for a pure `fit_columns(names: &[usize], urls: &[usize], width: usize) -> Columns { name, url }`: wide pane gives the longest name and longest URL in full; a pane too narrow for both shrinks the name first to a floor of 8 and leaves the URL at least 12; below that the URL gives way.
- [x] Remove the 12-cell cut from `display_name` (keep the suffix rule) and delete `NAME_CELLS`; update the `display_name` tests that asserted the cut.
- [x] Replace the fixed `NAME_CELLS` in `render_tunnels` and its header with the fitted widths; `fit` still cuts with `…` when the fitted width is smaller than the text.
- [x] `cargo test` passes.

## Task 2: Scrolling list

**Files:** modify `src/tui/ui.rs`

- [x] Write a failing render test: 20 entries, `selected = 19`, a pane of 8 rows; the selected row is on screen. Repeat with `selected = 0` after, and the list scrolls back.
- [x] Render with `render_stateful_widget` and a `ListState` whose selection is `app.selected`; the list style no longer paints the selected row by hand if ratatui's highlight does it, otherwise keep the existing row style.
- [x] `cargo test` passes.

## Task 3: Stacked layout below 121 columns

**Files:** modify `src/tui/ui.rs`

- [x] Write a failing test: at 80x30 the list's rows start at column 1 of the pane and span the full width (a 20-cell URL is shown whole); at 120x30 the list is the left 46%.
- [x] Add `const STACK_BELOW: u16 = 121;` (shipped as 100 first). Below it, `render` splits the content area vertically: list takes `Percentage(40)` with a 5-row minimum, the right-hand panes take the rest; at or above it, the current 46/54 horizontal split.
- [x] `cargo test` passes.

## Task 4: Right-column budget and wrapped Details

**Files:** modify `src/tui/ui.rs`

- [x] Write failing tests for a pure `right_heights(avail, details, metrics) -> (details, logs, metrics)`: with ample height every pane gets what it wants; at 17 available rows Logs gets at least 4; Metrics shrinks first, then hides below 3 rows, before Logs goes under 4; Details is never cut while another pane still has rows to give.
- [x] Write failing tests for `wrap_detail(label, value, width) -> Vec<Line>`: a value that fits is one row; a longer one wraps with continuation rows indented to the value column; a width below the label column plus 4 degrades to one cut row.
- [x] Implement both; `detail_lines` takes the pane width, and the Details height is its row count plus `DETAILS_CHROME`.
- [x] Split the Metrics gauge row in two below 58 cells.
- [x] Render-level test: at 100x24, access and public, the Logs title and the newest log line are on screen; at 60 columns no Details or Metrics row is cut mid-word.
- [x] `cargo test` passes.

## Task 5: Logs wrap count

**Files:** modify `src/tui/ui.rs`

- [x] Write a failing test: a log line of many short words in a narrow pane, where word wrapping needs more rows than `chars / width`; the newest line must still be on screen.
- [x] If it fails, make `wrapped_rows` count the way the widget wraps (greedy on word boundaries, hard break on a word longer than the width). If it already passes, keep the test and change nothing.

## Task 6: Size-matrix guard

**Files:** modify `src/tui/mod.rs` (tests)

- [x] One test renders an app of 20 long-named tunnels at 10x4, 20x8, 40x12, 60x20, 80x24, 100x24, 120x40 and 200x50 and asserts: no panic, no row wider than the terminal, and where the height is at least 12 the selected row is on screen.

## Task 7: Docs

**Files:** modify `docs/usage.md`, `CHANGELOG.md`

- [x] `docs/usage.md`: the 100-column layout switch, that the list scrolls, and that long Details values wrap.
- [x] `CHANGELOG.md`: one entry under Unreleased.

## Task 8: Evidence

- [ ] Render the real tunnel names (scratch data, never the real config) at 60, 80, 100 and 120 columns and 24 rows; paste the rows.
- [ ] Show the Logs title and newest line at 100x24 for access and public.
- [ ] `gtunnel list` output byte-identical before and after.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` output shown, on both platforms.

## Deferred

- The instance and domain pickers, the dialogs (fixed width 50) and the help modal were not analysed at narrow sizes: do that read-only pass next, and fix only what it shows broken.
- A user-set split between the list and the right-hand panes: only if two fixed layouts prove too rigid.
