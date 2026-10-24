# Account Field Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the bordered `ACCOUNT dev` box in the header with a filled two-row field: the account name on the first row, the account's GCP project id as a subtitle on the second. The header stays four rows.

**Architecture:** `header_lines` takes the project id beside the account name. `account_box` becomes `account_field`, two rows of filled cells that sit on the same two rows as the lockup and its captions (rows 1 and 2); row 0 is blank. `App` carries the project per account so `;` switches both together.

**Tech Stack:** Rust, ratatui 0.30. No new dependency.

**Spec:** The Design canvas `gtunnel account header`, option A, board Main, and the Decisions below.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Field shape | Filled, two rows, no border, no `ACCOUNT` label | A bordered box needs four rows and grows the header to five; the fill already marks it as the mode |
| Fill colour | Shipped as `theme::FIELD_BG`, an indexed colour one step lighter than `SELECTED_BG` (first drafted as a tint of `theme::ACCENT`) | Indexed, so no new `Color::Rgb`; a shade of its own so the field and the selected row never read as one control |
| Subtitle | `Account.project` from the config | Already stored; nothing new to read |
| Width | The longer of the two lines plus one cell of padding each side | Matches the board |
| Narrowing | Captions go first (already so), then the project id is cut with `…`, floor 4 cells; the account name is never cut until the existing single-line fallback | The account is the state that changes what every key acts on |
| Switching accounts | `;` updates the project id with the account | The subtitle is wrong the moment it lags |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on macOS and Linux.
- No new dependency; no new `Color::Rgb`.
- Comments explain why, in the voice of the surrounding code.
- The header keeps its 4 rows, hides below 16 rows, and falls to one line when the field does not fit beside the mark.
- Widths count characters.

## Review Focus

- The field's two rows line up with the lockup's two rows at every width where the captions show.
- A project id longer than the field's maximum is cut, never wrapped, and never pushes the field into the lockup.
- `;` changes name and project id in the same draw.
- An account with an empty `project` shows the name alone, on one row, without an empty second line.

## Task 1: Carry the project per account

**Files:** modify `src/tui/app.rs`, `src/tui/mod.rs`, `src/tui/demo.rs`

- [x] Write a failing test: with two accounts and their projects set, `;` leaves `app.project()` equal to the new account's project.
- [x] Add `projects: HashMap<String, String>` to `App` and `pub fn project(&self) -> &str` (empty when unknown).
- [x] Fill it in `run_tui` from `config.accounts`; give the demo account a placeholder project.
- [x] `cargo test` passes.

## Task 2: The field

**Files:** modify `src/tui/ui.rs`

- [x] Write failing tests on `header_lines(width, height, account, project)`: rows 1 and 2 carry the name and the project id in the same column; row 0 has no box characters; the rule stays on row 3; a 40-character project id is cut with `…` and the row is no wider than the terminal; an empty project leaves row 2 without field fill.
- [x] Replace `account_box`, `account_box_width` and `ACCOUNT_LABEL` with `account_field(account, project, max_width)` returning two rows, and a width helper.
- [x] Update `render` and the existing header tests (the box-closing-row assertions) to the new shape.
- [x] `cargo test` passes.

## Task 3: Docs

**Files:** modify `docs/usage.md`, `CHANGELOG.md`

- [x] `docs/usage.md`: the header shows the account and its GCP project id; `;` switches both.
- [x] `CHANGELOG.md`: one entry under Unreleased.

## Task 4: Evidence

- [ ] Render the header at 120, 100, 80 and 48 columns with scratch data and paste the rows.
- [ ] Render a long project id and an empty one.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` output shown.

## Deferred

- Renaming `k8s-*` tunnels to `kubernetes`: a delete and add per account with live services behind it (see `.claude/rules/tunnel-naming.md`); decide separately from this layout change.
- The failed-state lead key, the empty-state hint and the `?` help rewrite from the design review.
