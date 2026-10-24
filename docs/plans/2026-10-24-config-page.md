# Config Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show the loaded configuration inside the TUI and let the user edit it in their own editor, then validate it and reload without restarting. A `C` page lists `config.toml` and `tunnels.toml` with their paths; `e` on that page opens the visible file in `$VISUAL`, then `$EDITOR`, then `vi`.

**Architecture:** Four seams, each testable without a terminal. A pure `config_edit` module resolves the editor command, validates edited text for each file, and summarises what changed. An editor function field on `App` (the same seam as `clipboard` and `opener`) runs the editor, with a refusing stand-in under test. The input thread gains a pause flag so the editor owns the keyboard while it runs. A new `InputMode::Config` page renders the files and reuses the help page's scrolling.

**Tech Stack:** Rust 2024, ratatui 0.30, crossterm 0.29, `toml`. No new dependency.

**Spec:** This plan is its own spec. The facts behind it: config loads once at start; only the tunnel list reloads; `config.toml` is rewritten at mode 0600 on save; names are validated on `add` but not on load.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Files editable | Both `config.toml` and `tunnels.toml`, `Tab` switches | The user asked for "the .toml file loaded"; tunnels are the file edited by hand most |
| Key | `C` opens the page; inside it `e` edits, `Tab` switches, `Esc` or `q` closes | `C` is unbound; `e` already means edit-target on the list, so the page owns its own `e` |
| Editor | `$VISUAL`, then `$EDITOR`, then `vi`; split on whitespace so `code -w` works; no new config key | The conventional order; a setting is a second source of truth |
| Safe editing | Editor opens a temporary copy in the config directory; the real file is replaced only after validation | A half-valid `tunnels.toml` breaks the CLI and the daemon; `visudo` and `crontab -e` work this way |
| Failure | Show the parse or validation error with its line; `e` reopens the same edited copy, `Esc` discards it | The user's work is never thrown away silently |
| Permissions | Re-apply 0600 to `config.toml` after the replace | Editors that write by rename reset the mode |
| After success | Reload accounts, projects, relay and entries; keep the selected account if it still exists; status line `reloaded: N changed, N added, N removed` | The config is read once today, so a reload is the point of the feature |
| Running tunnels | The page and the status line say running tunnels keep their old settings until `R` | A launchd job is rewritten only by restart; saying it prevents the surprise |
| Terminal handoff | Leave raw mode and the alternate screen, pause the input thread, run the editor, restore, resume | The input thread polls stdin and would steal keystrokes from the editor |
| Windows | Not handled | gtunnel is macOS and Linux |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass before each task's commit; macOS and Linux.
- No new dependency; no new `Color::Rgb`; colours come from `theme.rs`.
- Comments explain why, in the voice of the surrounding code. Every `unsafe` block carries a `// SAFETY:` comment.
- Tests never run a real editor and never read the real config directory; they use `TempConfigDir` and the editor stand-in.
- Docs use placeholders only (`.claude/rules/public-repo-placeholders.md`); a screenshot or transcript uses a throwaway config.
- Widths count characters. The footer and help page both read `KEYMAP`; the new keys go there once.
- The CLI output does not change.

## Review Focus

- The input thread is paused for the whole editor run and resumed on every exit path, including a failed spawn and a non-zero editor exit.
- The terminal is restored on every exit path; a panic or `?` between leave and re-enter cannot leave the shell in raw mode.
- An invalid edit never reaches the real file; a valid one replaces it atomically and `config.toml` ends at 0600.
- The temporary copy is removed on discard and on success.
- Reload keeps the selection when the account survives and picks a sane one when it does not.
- At 80x24 and 40x12 the page does not draw past the terminal and `Esc` stays on screen.

## Task 1: The pure core

**Files:** create `src/tui/config_edit.rs`; modify `src/tui/mod.rs` (module line and tests)

- [x] Write failing tests: editor resolution (`VISUAL` wins over `EDITOR`, both empty falls back to `vi`, `code -w` splits into program and one argument, a quoted path is not mishandled); validation (`config.toml` text must parse as `Config` and name an existing `selected_account`; `tunnels.toml` text must parse, pass `validate_tunnel_name` per entry, reject a duplicate name within one account and a tunnel whose account is not in the config); error messages carry the line number from `toml`; change summary counts changed, added and removed tunnels by name and account.
- [x] Implement `resolve_editor(visual, editor) -> (String, Vec<String>)`, `validate_config(text) -> Result<Config>`, `validate_tunnels(text, &Config) -> Result<TunnelState>`, `summarise(old, new) -> Summary`.
- [x] `cargo test` passes.

## Task 2: Safe replace

**Files:** modify `src/tui/config_edit.rs`

- [x] Write failing tests with `TempConfigDir`: `stage(path)` copies the file to a temporary name in the same directory with owner-only mode; `commit(staged, path)` renames it over the original atomically and leaves `config.toml` at 0600; `discard(staged)` removes it; committing an invalid staged file is refused by the caller, never by rename.
- [x] Implement `stage`, `commit`, `discard` with `std::fs` only.
- [x] `cargo test` passes.

## Task 3: The editor seam and terminal handoff

**Files:** modify `src/tui/app.rs`, `src/tui/mod.rs`

- [x] Write failing tests: with the stand-in editor writing known text, `run_editor` returns the edited text and the input pause flag is false again afterwards; a stand-in that fails still resumes input and returns the error; under `cfg(test)` the default editor refuses, like `clipboard`.
- [x] Add `editor: fn(&Path) -> anyhow::Result<()>` to `App` with the real implementation (leave raw mode and the alternate screen, spawn the resolved command with the file path and inherited stdio, wait, re-enter) and an `AtomicBool` the input thread checks before `poll`.
- [x] Restore the terminal on every exit path with a guard value, not a trailing call.
- [x] `cargo test` passes.

## Task 4: The page

**Files:** modify `src/tui/ui.rs`, `src/tui/app.rs`, `src/tui/mod.rs`

- [x] Write failing render tests: `C` opens a page with the file's absolute path in its title, the TOML text below it, a line count, and the running-tunnels note; `Tab` switches files; the page scrolls with the help page's keys; at 80x24, 40x12 and 20x8 no row is wider than the terminal and `Esc` is on screen; a missing `tunnels.toml` shows `no tunnels file yet`.
- [x] Add `InputMode::Config`, `config_view: ConfigView { file, scroll, text, error }`, the `C`, `Tab` and `e` entries in `KEYMAP` (group THE PROGRAM, `footer: false`), and a `render_config_page` that reuses the help page's scroll extent report.
- [x] Update the KEYMAP count assertions and the handled-keys test.
- [x] `cargo test` passes.

## Task 5: Edit, validate, reload

**Files:** modify `src/tui/app.rs`, `src/tui/mod.rs`, `src/tui/ops.rs`

- [x] Write failing tests with the editor stand-in: a valid edit replaces the file, reloads accounts and projects, keeps the selected account, and sets the `reloaded: ...` status; an invalid edit leaves the real file byte for byte unchanged, shows the error with its line on the page, and `e` reopens the staged copy; `Esc` after a failure discards the copy; removing the selected account moves the selection to the first remaining one; an edit that renames a tunnel names the orphaned-plist risk in the summary.
- [x] Implement the flow `stage -> editor -> validate -> commit -> reload` and a `reload_config(&mut App)` that rebuilds `accounts`, `projects`, the relay and re-requests entries.
- [x] `cargo test` passes.

## Task 6: Docs

**Files:** modify `docs/usage.md`, `docs/design.md` only if a rule changes, `CHANGELOG.md`

- [x] `docs/usage.md`: a short section on `C`, the editor order, the validation step, and the restart note; a transcript from a throwaway config.
- [x] `CHANGELOG.md`: one entry under Unreleased.

## Task 7: Evidence

- [x] Render the page at 120x40 and 80x24 and paste the rows; render a validation failure.
- [x] Run the TUI in a pty against a scratch config with `EDITOR` set to a script that appends a tunnel, then to one that writes invalid TOML; show the status line both times and the file contents before and after.
- [x] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` output shown.

## Deferred

- An `editor =` key in `config.toml`: only if `$VISUAL` and `$EDITOR` prove insufficient.
- Watching the files and reloading on an outside change: only if someone edits while the TUI is open and asks for it.
- A diff view of the staged edit before it is committed.
