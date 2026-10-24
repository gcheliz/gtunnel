# Startup Login Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When gtunnel starts, check that the selected account has usable gcloud credentials. If it does not, run `gcloud auth login` automatically and continue; if the user still is not logged in afterwards, exit non-zero.

**Architecture:** One function, `gcloud::ensure_logged_in`, runs the same token probe `doctor` already uses. On failure it runs `gcloud auth login` with inherited stdio, then probes again. `main()` calls it before the TUI or any GCP-touching subcommand starts, so login output never lands on the ratatui alternate screen.

**Tech Stack:** Rust, tokio `process`, existing `gcloud` wrapper in `src/gcp/gcloud.rs`.

**Spec:** No separate spec. This plan is its own spec, from the analysis in the session that produced it and the Decisions below.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| How to check | `gcloud auth print-access-token` with `gcloud_common` flags | Same probe as `doctor`; catches expired refresh tokens and reauth, which `gcloud auth list` reports as active |
| No terminal (stdin or stdout not a TTY) | No login attempt; exit 1 with "run `gcloud auth login`" | A browser flow cannot complete under cron, a pipe or CI |
| Accounts checked | Only the selected / `--account` one | Startup cost stays at one probe; switching accounts in the TUI keeps the existing lazy error path |
| Impersonation | Probe keeps `--impersonate-service-account`; login argv does not | `gcloud auth login` rejects impersonation; it fixes the user-side credentials the impersonation token is minted from |
| Login command | `gcloud auth login --configuration=<cfg>` | Stores the account in the configuration the tunnels use |
| Commands that skip the check | `account`, `reset`, `demo`, `update`, `doctor`, and `init` | None needs GCP (`doctor` reports auth as a finding and a login prompt would hide it; `init` has no account yet) |
| Failure | Exit 1, message "gtunnel needs a logged-in Google account" | Matches the request: the app must exit |
| Background services | Untouched | launchd and systemd run `gcloud` directly and never go through `main()` |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on macOS and Linux (Linux leg per `docs/plans/2026-10-23-tunnel-feedback.md` Global Constraints).
- No new dependency. TTY detection uses `std::io::IsTerminal`.
- Comments explain why, in the voice of the surrounding code.
- Never claim a login succeeded without a second successful probe.

## Review Focus

- The check must run before `ratatui::init()`; nothing may print to the terminal after raw mode starts.
- `gcloud auth login` must inherit stdin, stdout and stderr; the current `run_gcloud` nulls stdin and captures output, so it must not be reused for login.
- A Ctrl+C during login must exit non-zero, not fall through to the TUI.
- The exit code is 1 whenever the user is still not logged in, including a login that exits 0 but leaves the probe failing.

## Task 1: Login and ensure function

**Files:** modify `src/gcp/gcloud.rs`

- [ ] Write failing unit tests: `login_argv` yields `["auth", "login", "--configuration=<cfg>"]` and never contains `--impersonate-service-account` or `--project`; the decision helper returns `Proceed` when the probe passes, `Login` when it fails on a TTY, and `Fail` when it fails without a TTY.
- [ ] Add `pub fn login_argv(acct: &Account) -> Vec<String>`.
- [ ] Add `async fn run_login(acct) -> Result<()>`: `Command::new(require_gcloud()?)`, `.args(login_argv)`, stdio inherited, `.status().await`, error on non-zero.
- [ ] Add a pure `fn next_step(probe_ok: bool, interactive: bool) -> AuthStep` with `AuthStep::{Proceed, Login, Fail}`.
- [ ] Add `pub async fn ensure_logged_in(acct: &Account) -> Result<()>`: probe via `print_access_token`; on `Login` print one line saying why, run login, probe again; any remaining failure returns `bail!("gtunnel needs a logged-in Google account for '<account>'. Run `gcloud auth login`")`.
- [ ] Run the module's tests; all pass.

## Task 2: Wire into main

**Files:** modify `src/cli.rs`, `src/main.rs`

- [ ] Write a failing unit test in `cli.rs`: `Commands::needs_gcp_auth` is false for `Init`, `Account`, `Reset`, `Demo`, `Update`, `Doctor`, true for every other variant, and `None` (the TUI) is true.
- [ ] Add `needs_gcp_auth` on `Commands`, matching exhaustively so a new variant forces a decision.
- [ ] In `main()`, before the `match`, when the command needs auth: `let (_, acct) = commands::resolve(account)?;` then `gcloud::ensure_logged_in(&acct).await?;`.
- [ ] Confirm `main` returns the error through the existing `Result` so the process exits 1 and the message prints once.
- [ ] `cargo test` passes.

## Task 3: Integration tests with a stub gcloud

**Files:** create `tests/startup_login.rs`

- [ ] A stub `gcloud` shell script in a tempdir on `PATH`: `auth print-access-token` exits 1 until a marker file exists; `auth login` creates the marker (or exits 1 in the failing variant); everything else exits 0. `HOME` points at a tempdir holding a minimal config with one account (confirm how `config_dir()` resolves and set the right variable).
- [ ] Test "logged out, login succeeds": run `gtunnel list` with stdin and stdout attached to a pty-free harness that forces the interactive path (see Open Question below); assert exit 0 and the marker exists.
- [ ] Test "logged out, login fails": assert exit 1 and stderr contains "needs a logged-in Google account".
- [ ] Test "logged out, no TTY": assert exit 1, marker absent, login never invoked.
- [ ] Test "logged in": assert the stub's login was never invoked.
- [ ] Test "`gtunnel account list` skips the probe": assert the stub was never called.
- [ ] `cargo test` passes.

Open Question: the integration harness has no TTY, so the "login runs" cases need a seam. Preferred: an env var read only in test builds is not available to an external binary, so instead have `ensure_logged_in` take `interactive: bool` and let the integration tests cover only the no-TTY, logged-in and skip cases, with the login-runs cases covered by Task 1 unit tests plus the live evidence in Task 5. Confirm this split at execution time.

## Task 4: Docs

**Files:** modify `docs/usage.md`, `docs/troubleshooting.md`, `CHANGELOG.md`

- [ ] `docs/usage.md`: a "Startup" section with a table of commands that check and commands that skip, plus example shell output of the auto-login and the failure.
- [ ] `docs/troubleshooting.md`: a section for "gtunnel needs a logged-in Google account", covering the no-TTY case and impersonation.
- [ ] `CHANGELOG.md`: one entry under Unreleased.

## Task 5: Live evidence

- [ ] Real binary, real gcloud, logged in: run `gtunnel list`, show stdout and `EXIT=0`, and that no login was prompted.
- [ ] Real binary with a stub-broken credential: run with `CLOUDSDK_CONFIG` pointed at an empty tempdir so gcloud has no credentials and the interactive login starts; cancel it, show the message and `EXIT=1`.
- [ ] Same without a TTY (`< /dev/null`): show the no-TTY message and `EXIT=1`.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` output shown, on both platforms.

## Deferred

- `--no-launch-browser` when no display is detected (SSH sessions): add once someone runs gtunnel over SSH.
- Checking every configured account at startup: only worth it if lazy errors after an in-TUI account switch prove confusing.
