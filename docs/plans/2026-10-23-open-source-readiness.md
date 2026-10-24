# Open Source Readiness Roadmap

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement
> this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Get gtunnel to the state where the repository can be made public without leaking the
maintainer's agent configuration, without shipping a TUI that contradicts its own design, and
without an undeclared toolchain floor.

**Architecture:** Three independent phases. Phase 1 is a pointer to an already-written plan and
touches only `src/tui/` and `README.md`. Phase 0 touches no tracked file at all -- it writes to
two git-local locations and corrects one agent definition. Phase 2 touches `Cargo.toml`, adds
migrates the edition, and adds one CI job.

**Tech Stack:** Rust 2021 (migrating to 2024), ratatui 0.30, crossterm 0.29, GitHub Actions.

**Execution order:** 1, then 0, then 2. The phase numbers come from the findings table and are
kept so the choice stays traceable; they are deliberately not in numerical order.

| Phase | Why it sits here |
|---|---|
| 1 | The shipped binary currently contradicts the approved canvas. Nothing else is visible to a user. |
| 0 | Must land before the first `git add -A`, and phase 2's commits are the first ones after phase 1. |
| 2 | Lowest urgency; its MSRV job is the gate that later coverage work builds on. |

**Status (2026-10-24)**

| Phase | State |
|---|---|
| 1 | Shipped: the design landed through the TUI plans (header, list, resize, keymap page, account field) |
| 0 | Done on this machine: the global ignore (`~/.config/git/ignore`) and `.git/hooks/pre-commit` exist. Neither is in the repository, so a fresh clone needs the same two files |
| 2 | In progress by another agent: the MSRV half (`Cargo.toml`, `.github/workflows/ci.yml`). The edition migration is deferred, see below |

**Phases 3 and 4 of the findings (coverage gate, cargo-deny, CONTRIBUTING/SECURITY/CODE_OF_CONDUCT,
dual licence, README restructure) are out of scope for this plan.** See `## Deferred`.

---

## Global Constraints

- No emoji in any file this plan touches, including commit messages.
- No agent attribution in commits, PR bodies, or file contents.
- Terminal geometry is counted in cells: width assertions use `.chars().count()`, never `.len()`.
- `src/tui/theme.rs` colours stay named ANSI except where phase 1's own plan licenses an exception.
- macOS has no `timeout`; use `gtimeout`. The shell is zsh, so `PIPESTATUS` does not exist --
  redirect to a file and check `$?`.
- Never `git commit`, `add`, `push` or `checkout` outside a step that spells it out.
- MSRV floor is **1.88.0**, set by `ratatui 0.30.2`. No step may raise it without saying so.

---

## Review Focus

The five things most likely to bite that no task's tests exercise:

1. A terminal narrower than the 11-cell mark plus the `gtunnel` wordmark -- the header must shed
   parts rather than wrap or panic on a `u16` subtraction.
2. A `git add -A` run from a second machine, or from a shell where the global ignore is not read --
   the pre-commit hook is the only thing standing between that and a public `.claude/`.
3. An MSRV that can only be checked in CI: this machine has Homebrew's cargo and **no rustup**, so
   `cargo +1.88.0` does not exist and a local green build proves nothing about the floor.
4. `cargo fix --edition` changing behaviour, not just syntax -- the 2024 edition changes `unsafe`
   attribute rules and `impl Trait` capture, and this crate has seven `unsafe` sites.
5. An MSRV declared in `Cargo.toml` that no job ever checks, which rots on the next dependency bump.

---

## Phase 1 -- Ship the approved design

**This phase is `docs/plans/2026-10-23-tui-brand-layout.md`. Do not restate its tasks here.**

That plan carries six tasks, each already in RED-GREEN order:

| Task | Deliverable |
|---|---|
| 1 | The brand palette in `src/tui/theme.rs` as a documented truecolor exception |
| 2 | The compound logo replacing the block `LOGO`; `HEADER_HEIGHT_FULL` 4 to 2 |
| 3 | The header row with the right-aligned highlighted account field |
| 4 | The contextual footer: `Action`, `footer_groups`, `footer_line` |
| 5 | `keymap_sections()` documenting all 17 bound keys |
| 6 | `README.md` wears the 21-cell mark |

**Entry condition:** that plan has not been reviewed or approved. It must be read and approved
before Task 1 runs, because Task 1 changes the colour contract in `src/tui/theme.rs`.

**Working-tree note:** `src/tui/ui.rs` is modified and uncommitted -- the four-row README-derived
logo from the previous header commit. Task 2 replaces exactly that code. Decide before starting
whether to commit it first as its own change or let Task 2 subsume it; leaving it uncommitted
across phase 1 makes the task's diff unreadable.

**Exit condition:** the six tasks are complete under their own plan's verification, and the TUI
rendered in a real terminal matches the approved design canvas.

---

## Phase 0 -- Keep the agent configuration private

**Files:**
- Modify: `~/.config/git/ignore` (exists; currently two patterns)
- Modify: `.git/info/exclude` (repo-local, never pushed)
- Create: `.git/hooks/pre-commit` (repo-local, never pushed)
- Modify: `.claude/agents/rust-reviewer.md:67-68`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other phases read. The hook is a guard, not an API.

**State today, verified:**

| Fact | Value |
|---|---|
| Tracked agent config | none -- `git ls-files` returns nothing for `.claude/`, `CLAUDE*`, `GEMINI*` |
| `.gitignore` (committed) | `/target`, `/.superpowers` -- **`.claude/` absent** |
| `core.excludesFile` | unset, so git reads the XDG default `~/.config/git/ignore` |
| `~/.config/git/ignore` | `**/.claude/settings.local.json`, `**/CLAUDE.local.md` -- agents not covered |
| Installed git hooks | none |

The committed `.gitignore` is deliberately left alone. A public `.gitignore` naming `.claude/`
tells every reader which tools the maintainer uses; the git-local locations below do the same job
and are never pushed.

- [ ] **Step 1: Extend the global ignore**

Append to `~/.config/git/ignore`, keeping the two existing lines:

```gitignore
# Agent configuration is the maintainer's, not the project's. These stay out of
# every repository on this machine, so a public clone carries no trace of them.
.claude/
.gemini/
.superpowers/
CLAUDE.md
GEMINI.md
AGENTS.md
```

- [ ] **Step 2: Verify the global ignore catches the real files**

Run:
```bash
cd /Users/gonzalo/workspace/gtunnel
git check-ignore -v .claude/agents/rust-implementer.md .claude/agents/rust-reviewer.md
git status --porcelain | grep -E '^\?\? \.claude' && echo "STILL VISIBLE" || echo "ignored"
```
Expected: both paths attributed to `/Users/gonzalo/.config/git/ignore`, and `ignored`.

- [ ] **Step 3: Add the repo-local backstop**

Append the same seven lines to `.git/info/exclude`. This survives a missing or renamed global
ignore. It is not a duplicate for its own sake: step 2's attribution proves which file is doing
the work, and step 5 proves the hook catches what both miss.

- [ ] **Step 4: Write the pre-commit guard**

Create `.git/hooks/pre-commit`, mode 0755. It aborts the commit if any staged path matches a
private pattern, naming each one. Use `git diff --cached --name-only -z` and match on the path,
not on a `grep` of the whole diff.

```sh
#!/bin/sh
# A staged .claude/ or CLAUDE.md means an ignore file was missing or -f was used.
# Refuse rather than publish the maintainer's agent configuration.
set -eu
blocked=$(git diff --cached --name-only --diff-filter=AM \
  | grep -E '(^|/)(\.claude|\.gemini|\.superpowers)/|(^|/)(CLAUDE|GEMINI|AGENTS)(\.local)?\.md$' \
  || true)
if [ -n "$blocked" ]; then
  printf 'pre-commit: refusing to commit private agent configuration:\n' >&2
  printf '%s\n' "$blocked" | sed 's/^/  /' >&2
  printf 'Unstage these, or pass --no-verify if this is deliberate.\n' >&2
  exit 1
fi
```

- [ ] **Step 5: Prove the hook fires, without touching the real index**

Drive the hook against a throwaway index so the working index is untouched:

```bash
cd /Users/gonzalo/workspace/gtunnel
export GIT_INDEX_FILE=/private/tmp/claude-501/-Users-gonzalo-workspace-gtunnel/5486d292-8b89-4b2e-ba52-b76bf4859d44/scratchpad/guard-index
rm -f "$GIT_INDEX_FILE"
git read-tree HEAD
git add -f .claude/agents/rust-reviewer.md
.git/hooks/pre-commit; echo "EXIT=$?"
```
Expected: the hook prints `.claude/agents/rust-reviewer.md` and `EXIT=1`.

Then the green case:
```bash
rm -f "$GIT_INDEX_FILE"; git read-tree HEAD
.git/hooks/pre-commit; echo "EXIT=$?"
unset GIT_INDEX_FILE
rm -f /private/tmp/claude-501/-Users-gonzalo-workspace-gtunnel/5486d292-8b89-4b2e-ba52-b76bf4859d44/scratchpad/guard-index
```
Expected: no output and `EXIT=0`. Confirm with `git status --porcelain` that the real index is
unchanged.

- [ ] **Step 6: Correct the false `unsafe` claim in the reviewer agent**

`.claude/agents/rust-reviewer.md:67-68` currently reads "gtunnel has none today, so a block here
needs a stated safety invariant and a very good reason." That is wrong. Seven sites exist:

| File | Call |
|---|---|
| `src/ephemeral.rs:72`, `:463` | `libc::kill` |
| `src/daemon/launchd.rs:149` | `libc::getuid` |
| `src/daemon/mod.rs:552` | `libc::getuid` |
| `src/commands/maint.rs:60` | libc |
| `src/tui/mod.rs:361` | libc |
| `src/tui/ops.rs:418` | libc |

Replace the claim with the real posture: gtunnel calls `libc` from seven sites, every one needs a
`// SAFETY:` comment stating the invariant, and a new site outside that pattern is a finding.
Both agent files must stop implying `#![forbid(unsafe_code)]` is reachable for this crate.

- [ ] **Step 7: No commit**

Phase 0 produces no tracked change. `git status --porcelain` must show the same entries as before
step 1, minus `?? .claude/`.

**Open decision, needs a ruling before phase 2 commits:** `docs/plans/2026-10-22-gtunnel.md` is
tracked and will be public. Recommended line: plans stay public because they describe the
software, while `.claude/` and `.superpowers/` stay private because they configure the agent. The
alternative is ignoring `docs/plans/` wholesale, which costs the repository its design history.

---

## Phase 2 -- Declare the version policy

**Files:**
- Modify: `Cargo.toml` (add `rust-version`, change `edition`)
- Modify: `.github/workflows/ci.yml` (add an `msrv` job)
- Modify: whatever `cargo fix --edition` rewrites under `src/`

**Toolchain reality on this machine, verified:**

| Fact | Value |
|---|---|
| `rustup` | **not installed** |
| `cargo`, `rustc` | `/opt/homebrew/bin/`, rustc 1.99.1 |
| `cargo +stable --version` | `error: no such command: +stable` |
| `act` | 0.2.89, installed |

`+toolchain` selection and `rust-toolchain.toml` are both rustup features. Neither works here, so
**the MSRV is verified in CI via `act`, not locally**, and no `rust-toolchain.toml` is added. See
step 5 for the reasoning.

**Interfaces:**
- Consumes: nothing from phases 1 or 0.
- Produces: an MSRV of `1.88` that any later coverage or `cargo-deny` job must honour.

**Dependency floors, queried from crates.io:**

| Crate | Version | Declared `rust-version` |
|---|---|---|
| ratatui | 0.30.2 | **1.88.0** |
| reqwest | 0.13.5 | 1.85.0 |
| toml | 1.1.8 | 1.85 |
| clap | 4.6.7 | 1.85 |
| tokio | 1.54.1 | 1.85 |
| crossterm | 0.29.0 | 1.63.0 |
| dirs | 7.0.0 | unset |

ratatui binds the floor at 1.88.0. Edition 2024 needs 1.85, so it fits beneath that floor and
costs no MSRV.

- [x] **Step 1: Declare the MSRV**

Add to `[package]` in `Cargo.toml`, after `edition`:
```toml
# ratatui 0.30 sets this floor; every other dependency is at 1.85 or below.
rust-version = "1.88"
```

- [x] **Step 2: Add the MSRV job to CI**

The declaration cannot be checked locally, so the job that checks it is written in the same breath.
Append to `.github/workflows/ci.yml`, as a sibling of `test` and `audit`:

```yaml
  # A declared rust-version that nothing checks rots on the next dependency bump.
  # This job is the only thing that makes the Cargo.toml field a fact.
  msrv:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4
      - uses: dtolnay/rust-toolchain@b725eda525362e7cf34af9a7243cac1e3f01a7ec # stable
        with:
          toolchain: "1.88.0"
      - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2.9.3
        with:
          key: msrv
      - name: Check on the declared MSRV
        run: cargo check --locked --all-targets
```

- [x] **Step 3: Run the job locally with act -- this is the only MSRV evidence available**

```bash
act -j msrv --container-architecture linux/amd64 > /tmp/act-msrv.log 2>&1; echo "EXIT=$?"
grep -a '1\.88' /tmp/act-msrv.log | head
tail -c 3000 /tmp/act-msrv.log
```
Expected: `EXIT=0`, and the log naming 1.88.0 as the installed toolchain.
Log: `/tmp/act-msrv.log`. Read it with `! tail -c 3000 /tmp/act-msrv.log`.

A failure means the real floor is above 1.88 -- raise `rust-version` to whatever compiles rather
than deleting the field. Use `grep -a`: act's output carries ANSI progress bars that make plain
`grep` treat the log as binary and print nothing while exiting 0.

- [ ] **Step 4: Commit the declaration and its check together**

```bash
git add Cargo.toml .github/workflows/ci.yml
git commit -m "build: declare and check the 1.88 toolchain floor ratatui requires"
```

- [ ] **Step 5: Do not add rust-toolchain.toml -- record why**

A `rust-toolchain.toml` is tempting here and is the wrong call for this repository:

| Consequence | Effect |
|---|---|
| Locally | Inert. Homebrew's cargo is not a rustup shim and ignores the file entirely. |
| In CI | Authoritative. It overrides `dtolnay/rust-toolchain` in every job, including `msrv`, so the matrix would claim to test stable while testing the pinned version and the MSRV job would quietly stop checking the MSRV. |
| For contributors | Only those using rustup are affected, giving three different behaviours for one file. |

The declared `rust-version` plus the `msrv` job give the same guarantee with none of that. If
rustup is ever installed here, revisit this step: it becomes worth adding, and the `msrv` job then
needs an explicit override so the pin does not swallow it.

- [ ] **Step 6: Migrate to edition 2024**

```bash
cargo fix --edition --allow-staged > /tmp/edition.log 2>&1; echo "EXIT=$?"
tail -c 3000 /tmp/edition.log
```
Then set `edition = "2024"` in `Cargo.toml`.

The 2024 edition changes `unsafe` attribute syntax and `impl Trait` lifetime capture, and this
crate has seven `unsafe` sites -- read the diff rather than trusting the tool. Anything
`cargo fix` cannot rewrite is a real behaviour question, not a lint.

- [ ] **Step 7: Prove the migration changed nothing observable**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test > /tmp/edition-test.log 2>&1; echo "EXIT=$?"
tail -c 2000 /tmp/edition-test.log
act -j msrv --container-architecture linux/amd64 > /tmp/act-msrv.log 2>&1; echo "MSRV_EXIT=$?"
```
Expected: all clean, 210 tests passing, `MSRV_EXIT=0` (the edition floor is 1.85, below the 1.88 MSRV, so this must still pass). A test that fails here is a behaviour
change the edition introduced -- fix the cause, never the assertion.

- [ ] **Step 8: Commit the edition migration**

```bash
git add Cargo.toml src
git commit -m "build: move to the 2024 edition"
```

---

## Verification

Run at the end of all three phases, from a clean tree:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
act -j msrv --container-architecture linux/amd64
git check-ignore -v .claude/agents/rust-reviewer.md
git status --porcelain
```

| Check | Expected |
|---|---|
| fmt, clippy, test | clean; 210 tests pass |
| MSRV check (`act -j msrv`) | exit 0; the log names 1.88.0 |
| `check-ignore` | attributed to a git-local or global ignore file, never `.gitignore` |
| `status --porcelain` | no `.claude/`, no `.superpowers/`, no `CLAUDE.md` |
| The TUI in a real terminal | matches the approved canvas |

Phase 1 additionally carries its own Verification section; this one does not replace it.

---

## Deferred

From the findings, deliberately not in this plan:

| Item | Why deferred | What would pull it forward |
|---|---|---|
| Coverage gate (`cargo-llvm-cov`, 80 percent) | Coverage has never been measured; a blind threshold either breaks CI on day one or sits below reality. Must be measured, gated at the floor, then ratcheted. | Phase 1 landing, which changes the hardest-to-cover file in the repo |
| `cargo-deny` and `deny.toml` | `cargo audit` already gates advisories; deny adds licence, source and duplicate policy, which matters at first outside contribution | A dependency whose licence is not obviously compatible |
| SECURITY.md | Needs the self-updater's real threat model written down: `src/update.rs` verifies a sha256 served by the same host as the tarball, which is integrity against corruption, not against a compromised release host | Making the repository public |
| CONTRIBUTING.md, CODE_OF_CONDUCT.md | No outside contributors yet | The first external PR |
| MIT or Apache-2.0 dual licence | Cheapest now, while `gcheliz` is sole copyright holder; needs every contributor's consent afterwards | Any outside contribution -- this is the one item that gets harder with time |
| Signature verification for releases | Larger than a policy change; needs key management | Publishing to a wider audience than the maintainer |
| OpenSSF Scorecard | A workflow and a badge for a single-maintainer repo | Organisational adoption |
| Edition 2024 migration | Rewrites files another session edits; do after the identifier-rewrite work lands | The identifier-rewrite work landing |

- edition 2024 migration: rewrites files another session edits; do after the identifier-rewrite work lands (Phase 2 Step 6 stays open until then).
