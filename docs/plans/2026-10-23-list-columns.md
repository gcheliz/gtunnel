# TUI Tunnel List Columns Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the TUI tunnel list a table whose columns line up on every row: a header row, a name column capped at 12 cells, a fixed flags column, and names shown without the `-dev` / `-prod` suffix their account already says.

**Architecture:** A pure `display_name(name, account)` decides what the list shows: it drops a trailing `-<account>` and cuts anything still over 12 characters to 11 plus an ellipsis. `render_tunnels` lays the row out in fixed columns and draws a header above them. Stored names, the CLI and `gtunnel list` are untouched; the Details pane gains a `name` row so the full name is always one keypress away.

**Tech Stack:** Rust, ratatui 0.30. No new dependency.

**Spec:** No separate spec. This plan is its own spec, from the analysis in the session that produced it and the Decisions below.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| What "remove -dev / -prod" means | Display only; stored names and CLI commands keep the suffix | A rename touches `tunnels.toml`, launchd and systemd labels, logs and agents; that is a migration with live services behind it and gets its own plan if wanted |
| Where the cut applies | TUI list only | `gtunnel list` sizes to the longest name on purpose (`76c6e0e`), and people copy names from it into `gtunnel start NAME` |
| Suffix rule | Strip `-<account>` only when it equals the entry's own account and something remains | Generic, and never touches a tunnel that is not suffixed by its own account |
| Name cap | 12 cells; longer is 11 characters plus `…` | The request; every cut name in the real config stays unique within its account |
| Full name | A `name` row at the top of Details | The cut is harmless only if the whole name is reachable |
| Columns | Header row `NAME MODE URL`; the flags get a fixed 2-cell column | The `⟳` and `⚠` flags follow the URL today, so they land at a different column on every row |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on macOS and Linux (Linux leg per `docs/plans/2026-10-23-tunnel-feedback.md` Global Constraints).
- No new dependency.
- Comments explain why, in the voice of the surrounding code.
- The CLI `gtunnel list` output does not change.
- Widths count characters, not bytes. Managed tunnel names are ASCII (`validate_tunnel_name`), so a character is a cell.

## Review Focus

- A name of exactly 12 characters is shown whole; 13 is cut to 11 plus `…`. Off-by-one at the boundary.
- A tunnel named exactly `-dev` under account `dev`, or `dev` under account `dev`, must not display as an empty string.
- An unmanaged row (`pid-24083`, an ephemeral tunnel) uses the same column and the same cap; the suffix rule applies only when its account matches.
- A narrow pane: the URL column shrinks before the name column does, and no row wraps into the next.
- The header row must not shift the selection: `app.selected` indexes entries, not screen rows.

## Task 1: `display_name`

**Files:** modify `src/tui/app.rs`

- [ ] (Superseded in part by `2026-10-24-resize-layout.md` Task 1: the 12-cell cut was removed, so the cut-name assertions no longer hold; the suffix assertions shipped.) Write failing unit tests: `k8s-dev` under `dev` gives `k8s`; `k8s-prod` under `dev` stays `k8s-prod`; `redis-dev` gives `redis`; `dev` under `dev` stays `dev`; `-dev` stays `-dev`; a 12-character name is unchanged; `mongo-activitylog-prod` under `prod` gives `mongo-activ…`; `mongo-production-prod` gives `mongo-produ…`; the eleven real stripped names from the user's config are all distinct within their account.
- [ ] (Superseded: `NAME_CELLS` was removed by the resize plan; `display_name` shipped without the cut.) Add `pub const NAME_CELLS: usize = 12;` and `pub fn display_name(name: &str, account: &str) -> String`.
- [x] Add `TunnelEntry::display_name(&self) -> String` using `self.tunnel.account_name`.
- [x] Run the module's tests; all pass.

## Task 2: Columns and header

**Files:** modify `src/tui/ui.rs`, `src/tui/mod.rs` (tests only)

- [x] Write failing render tests with `TestBackend`: for a set of entries including `mongo-activitylog-prod` and `k8s-prod`, every row's mode column starts at the same cell; the flags cell is at the same cell on a row with `⟳`, one with `⚠`, and one with neither; the header's `NAME`, `MODE` and `URL` start at the same cells as the values beneath them; the selected row is still the entry at `app.selected`.
- [x] In `render_tunnels`, split the pane's inner area into a one-row header and the list; draw `NAME`, `MODE`, `URL` muted.
- [x] Replace `{:<14}` with the `display_name` padded to `NAME_CELLS`, then one cell of air; mode padded to its own width; then the URL padded to the longest URL on screen, shrinking before the name when the pane is narrow; then a 2-cell flags column (`⟳`, `⚠`, each in its own cell); then the tag.
- [x] Update the existing list assertions in `mod.rs` that expect the old layout.
- [x] `cargo test` passes.

## Task 3: Name row in Details

**Files:** modify `src/tui/ui.rs`

- [x] Write a failing test: Details for a tunnel whose name is cut in the list shows its full name on its first row.
- [x] Add `detail("name", entry.name())` as the first row of `render_details`.
- [x] Check the pane still shows `started` and `started by` at its minimum height; if not, drop the `name` row when the pane is shorter than the rows it would push off.
- [x] `cargo test` passes.

## Task 4: Docs

**Files:** modify `docs/usage.md`, `CHANGELOG.md`

- [x] `docs/usage.md`: a short note under the TUI section that the list hides a suffix equal to the account and cuts names to 12 cells, that Details shows the full name, and that `gtunnel list` and every command keep using full names.
- [x] `CHANGELOG.md`: one entry under Unreleased.

## Task 5: Evidence

- [ ] Render the list at 100 columns against a copy of the user's real `tunnels.toml` (scratch `HOME`, never the real file) and paste the rows.
- [ ] Same at 80 columns, to show the URL column shrinking first.
- [ ] `gtunnel list` output before and after is byte-identical (diff shown).
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` output shown, on both platforms.

## Deferred

- Renaming stored tunnels to drop the suffix (the migration in the first Decision): worth doing only if the CLI should also read `gtunnel start k8s`; needs `doctor` to cope with the old labels.
- Showing the account suffix again when two accounts are listed together: only if a combined multi-account list is ever built.
