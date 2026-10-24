# Rust Lint and Security Checks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put a curated lint policy in `Cargo.toml` and add workflow linting to CI, so an `unsafe` block or a workflow change is checked before it merges. `cargo audit` stays the advisory gate. CodeQL is written but held until the repository is public. `cargo-deny` and `gitleaks` are deferred by choice.

**Architecture:** Two pieces. `[lints]` in `Cargo.toml` is the one source for rustc and clippy policy, so CI and editors agree. A `zizmor` CI job checks the workflows. A `codeql.yml` is added only when `gh repo view` reports the repository public.

**Tech Stack:** Rust 2024, GitHub Actions, `zizmor`, CodeQL.

**Spec:** This plan is its own spec. Findings behind it: the CI today runs `fmt`, `clippy -D warnings` on default lints, `test`, `cargo audit --deny warnings` and an MSRV check; Dependabot covers cargo and Actions; actions are pinned by commit SHA.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Lint policy | `[lints.rust]` and `[lints.clippy]` in `Cargo.toml`, errors in CI through the existing `-D warnings` | One declaration serves CI and editors; a flag in the workflow would not |
| Lints turned on | `unsafe_op_in_unsafe_fn`, `clippy::undocumented_unsafe_blocks`, `clippy::dbg_macro`, `clippy::todo`, `clippy::unimplemented` | Measured: 6 unsafe blocks lack a safety comment and nothing else fires, so the gate lands without churn |
| Full pedantic set | Off | 145 warning lines today; a blanket opt-in is a refactor, not a gate |
| `unwrap_used` | Not adopted here | Needs a count of non-test sites first; revisit as its own task |
| Workflow linting | `zizmor` | Checks template injection, permissions, unpinned actions and persisted credentials; `release.yml` holds a release token |
| CodeQL | Workflow written but only committed when the repo is public | On a private repo it needs a paid GitHub tier and the job fails; the Rust queries are generally available since October 2025 |
| `cargo-deny`, `gitleaks` | Deferred by the maintainer for now; `cargo audit --deny warnings` stays the advisory gate | Licence, ban and secret checks wait for a later decision |
| `cargo-vet` | Not adopted | Review-tracking for high-security teams; `cargo-deny` is enough for one maintainer |
| Verification | Run each tool locally and through `act` before committing | CI cannot be tested from here, as with the `msrv` job |

## Global Constraints

- No emojis anywhere: code, comments, docs, commit messages.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass before each task's commit.
- Actions are pinned by full commit SHA with the version in a comment, as the existing jobs are.
- Every new CI job declares `permissions: contents: read` and `persist-credentials: false` on checkout.
- Every `// SAFETY:` comment states the invariant that makes the call sound, never "this is fine".
- No new runtime dependency. Tools are installed in CI with a pinned version and `--locked`.
- Do not stage another session's edits; stage by hunk if the tree is mixed.

## Review Focus

- Task 1: a `// SAFETY:` comment that restates the call instead of the invariant.
- Task 2: a workflow finding silenced with an ignore and no comment.

## Task 1: Lint policy

**Files:** modify `Cargo.toml`, the files holding the 6 unsafe blocks

- [x] Write the failing gate first: add the `[lints]` table and run `cargo clippy --all-targets -- -D warnings`; expect 6 `undocumented_unsafe_blocks` errors.
- [x] Add a `// SAFETY:` comment to each of the 6 blocks, naming the invariant.
- [x] Keep the existing `collapsible_if` allow and its TODO.
- [x] Gate passes on the tree.

## Task 2: Workflow linting

**Files:** modify `.github/workflows/ci.yml`

- [x] Run `zizmor` locally on `.github/workflows/`; record the findings.
- [x] Fix each finding in the workflows; ignore none without a comment saying why.
- [x] Add a `zizmor` job with a pinned version.

## Task 3: CodeQL, held

**Files:** create `.github/workflows/codeql.yml` only when the repository is public

- [x] Check `gh repo view gcheliz/gtunnel --json visibility`. If private, write the workflow into the plan's Deferred section and stop. If public, add it with `languages: rust`, `build-mode: none`, a weekly schedule and `security-events: write` on that job only.

## Task 4: Docs

**Files:** modify `docs/design.md` is not touched; modify `CHANGELOG.md` and the readiness plan

- [x] `CHANGELOG.md`: one entry under Unreleased naming the new checks.
- [x] Note in `docs/plans/2026-10-23-open-source-readiness.md` that `cargo-deny` stays deferred.

## Task 5: Evidence

- [x] `cargo clippy --all-targets -- -D warnings` and `zizmor .github/workflows`, each with its exit code.
- [x] `act` run of the `zizmor` job, or the reason it cannot run.

## Deferred

- `cargo-deny` (licences, bans, sources): `deny.toml` plus a `deny` job; revisit when the repository goes public or a dependency's licence is in question.
- `gitleaks` over full history: a pinned binary in a `secrets` job; revisit before the repository goes public.
- CodeQL workflow: until the repository is public (`gh repo view gcheliz/gtunnel --json visibility` reports PRIVATE). When it is, add `.github/workflows/codeql.yml`: trigger on push to `main`, pull requests and a weekly `schedule`; one `analyze` job with `permissions: contents: read` plus `security-events: write` on that job only; `persist-credentials: false` on checkout; `github/codeql-action/init` with `languages: rust` and `build-mode: none`, then `analyze`; both actions pinned by full commit SHA.
- `unwrap_used` in non-test code: after counting the sites.
- Coverage (`cargo-llvm-cov`) and mutation testing: measure first, gate at the floor.
- `SECURITY.md` and OpenSSF Scorecard: tied to making the repository public.
