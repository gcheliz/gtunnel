# One Mark, and a README That Stops Repeating Itself — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** gtunnel has one mark, derived from the eleven braille cells the TUI draws, used in the app, the docs and the README alike; the README stops duplicating `docs/`; the four missing architecture diagrams exist; and every badge states a fact the repository can back.

**Architecture:** The mark in `src/tui/ui.rs` is the source of truth. The README banner becomes a vector enlargement of those same cells rather than a separate drawing, and a test recomputes the expected dots from `MARK_BRAILLE` and `MARK_BANDS` so the two cannot drift. Everything else is documentation surgery: move duplicated depth out of `README.md` into the file that owns it, add the diagrams `architecture.md` is missing, and replace a badge that 404s with badges that do not depend on the repository being public.

**Tech Stack:** Rust (the mark and its test), hand-written SVG, Mermaid, Markdown.

**Spec:** none separate. The decisions this plan implements were taken in session on 2026-10-24: option A for the logo, the code's fallback over the canvas's, drop the CI badge and add language/security/version badges, and the six-file documentation split approved as proposed.

## Global Constraints

- No emojis anywhere: code, comments, commit messages, documentation, PR text.
- No Claude attribution in any commit message, PR body or file content.
- Public-repo placeholders only, per `.claude/rules/public-repo-placeholders.md`: `example-dev-1a2b`, `you@example.com`, `bastion`, `my-vm`, `10.0.0.10`. Real zones such as `europe-west1-b` stay real. `docs/plans/` is excluded from the sweep.
- Documentation is tables and diagrams over prose; one topic, one file.
- Every badge must state something the repository can back without being public. No badge may assert a live status it cannot prove.
- The gate before every commit: `cargo fmt --check`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, all clean.
- Tests are written first and seen to fail before the implementation.

## Review Focus

Five things no task's tests exercise directly, most likely to bite first:

1. A terminal whose font has no braille coverage draws `MARK_FALLBACK`; a reader comparing that screen to the README banner must still recognise one mark. Pinned in Task 1's test by asserting both rows are eleven cells and the fallback is the code's, not the canvas's.
2. The SVG is hand-written, so a single mistyped circle silently changes the cloud. Task 1's test recomputes every dot from the codepoints rather than trusting the file.
3. A reader on a phone sees the README banner at roughly 200px wide; 22 dots across becomes a smear. Task 1 checks the committed PNG's rendered width against the dot pitch.
4. Dropping the CI badge removes the only signal that CI exists at all. Task 4 keeps that discoverable in text rather than leaving a gap.
5. A link moved from `README.md` to a `docs/` file breaks every bookmark to the old anchor. Task 6 greps for internal anchors before cutting anything.

---

### Task 1: The README banner is the mark, enlarged

**Files:**
- Create: `docs/logo.svg` (replacing the hand-drawn one), `docs/logo.png` (regenerated from it)
- Modify: `src/tui/ui.rs` (expose the dot geometry for the test), `src/tui/mod.rs` (the test)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: `MARK_BRAILLE`, `MARK_FALLBACK`, `MARK_BANDS`, `MARK_WIDTH`, `NAME_ROW` from `src/tui/ui.rs`.
- Produces: `pub fn mark_dots() -> Vec<(u16, u16, Color)>` in `src/tui/ui.rs`, returning one entry per lit braille dot as `(x, y, colour)` on a 22x4 grid, left to right then top to bottom.

A braille cell is `U+2800` plus a bit per dot, two columns by four rows. The bit order is not the reading order: bit 0 is (col 0, row 0), bit 1 (0,1), bit 2 (0,2), bit 3 (1,0), bit 4 (1,1), bit 5 (1,2), bit 6 (0,3), bit 7 (1,3). Cell `n` of the mark occupies grid columns `2n` and `2n+1`, so eleven cells give a 22 by 4 dot grid. Each dot takes the colour `MARK_BANDS[n]` of the cell it belongs to.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_readme_banner_is_the_mark_dot_for_dot() {
    // every <circle> in docs/logo.svg matches mark_dots(): same count,
    // same grid positions, same colour per dot, nothing extra
}

#[test]
fn the_fallback_is_the_one_the_code_draws_not_the_canvas() {
    // mark_spans(false, 0) is ──▗▄▟▀▙▄▖── and is MARK_WIDTH cells
}
```

- [ ] **Step 2: Run them** — Expected: FAIL, `mark_dots` not defined.
- [ ] **Step 3: Implement `mark_dots()`** in `src/tui/ui.rs`, decoding each cell of `MARK_BRAILLE` by the bit order above and pairing it with its band colour.
- [ ] **Step 4: Run them** — Expected: FAIL on the SVG comparison, because the committed `docs/logo.svg` is the old hand-drawn art.
- [ ] **Step 5: Write `docs/logo.svg`** from `mark_dots()`: one `<circle>` per lit dot on the 22x4 grid, the wordmark `gtunnel` centred beneath on the same centre column, `#0E0E10` ground, the five hues already in `src/tui/theme.rs`. Give it a `role="img"` and an `aria-label` of `gtunnel`.
- [ ] **Step 6: Run them** — Expected: PASS.
- [ ] **Step 7: Regenerate `docs/logo.png`** from the SVG at a width that keeps the dot pitch legible at the README's `width="420"`, and confirm the file is smaller than the 68K it replaces.
- [ ] **Step 8: Commit** — `feat: the README banner is the TUI mark, dot for dot`

---

### Task 2: The design canvas agrees with the code

**Files:**
- Modify: the `gtunnel TUI Layout` canvas, artboards `Cloud.dc.html` and `Variants.dc.html`

**Interfaces:**
- Consumes: Task 1's confirmed fallback cells.

This task touches no repository file. The canvas at `https://claude.ai/artifact/ETxxPpTtXUAjzm7xq1UUNe` carries two drawings the code does not: a three-row large form labelled "README and `--version`", which option A retires, and a non-braille fallback `──▗▞▀▀▚▖▄──` that disagrees with the code's `──▗▄▟▀▙▄▖──`.

- [ ] **Step 1: Read** `project/canvas.json`, `project/Cloud.dc.html` and `project/Variants.dc.html` before editing.
- [ ] **Step 2: In `Cloud.dc.html`,** remove the large-form panel and its "The large form — README and `--version`" heading, and replace that half of the artboard with the enlarged SVG from Task 1, labelled as the README banner.
- [ ] **Step 3: In `Variants.dc.html`,** change the no-braille row from `──▗▞▀▀▚▖▄──` to `──▗▄▟▀▙▄▖──`.
- [ ] **Step 4: Publish** both artboards in one call, index only if the layout changed.

---

### Task 3: `docs/design.md` states the single-mark rule

**Files:**
- Modify: `docs/design.md`

- [ ] **Step 1: Delete the carve-out** at `docs/design.md:72` — "The README carries a separate raster, `docs/logo.png`; it is documentation artwork and does not follow these rules." It is no longer true and was the licence for the drift.
- [ ] **Step 2: Replace it** with the rule: one mark, eleven cells, defined by `MARK_BRAILLE`; the README banner is that mark enlarged, generated from `mark_dots()` and pinned by `the_readme_banner_is_the_mark_dot_for_dot`; the non-braille fallback is `MARK_FALLBACK` and carries the same geometry. Name the test, as the surrounding text already does for `the_mark_is_the_one_drawn_for_the_design_and_never_another`.
- [ ] **Step 3: Commit** — `docs: one mark, and where it is pinned`

---

### Task 4: Badges that do not need a public repository

**Files:**
- Create: `SECURITY.md`
- Modify: `README.md` (the badge block, lines 5-9)

**Interfaces:**
- Consumes: `Cargo.toml` `version = "0.1.0"`, `rust-version = "1.88"`, `license = "MIT"`.

Every dynamic GitHub badge 404s while the repository is private, exactly as the CI badge does today — verified by fetching it anonymously. So each badge below is static and states a fact from `Cargo.toml` or from a file in the repository. None asserts a passing build or a clean audit, because neither can be proven from a private repository and a badge that claims one would be a lie.

| Badge | Text | Links to |
|---|---|---|
| Language | `rust-1.88+-orange` | `https://www.rust-lang.org` |
| Edition | `edition-2024-orange` | `Cargo.toml` |
| Version | `version-0.1.0-blue` | `CHANGELOG.md` |
| Security | `security-policy-green` | `SECURITY.md` |
| License | `license-MIT-blue` (unchanged) | `LICENSE` |
| Platform | `platform-macOS | Linux-lightgrey` (unchanged) | — |

- [ ] **Step 1: Remove the CI badge** from `README.md`.
- [ ] **Step 2: Add a line of prose** under the badges naming what CI runs — fmt, clippy with `-D warnings`, `cargo audit --deny warnings`, and an MSRV job on 1.88.0 — with a link to `.github/workflows/ci.yml`. The badge went; the signal must not.
- [ ] **Step 3: Write `SECURITY.md`** — supported versions, how to report a vulnerability privately, what gtunnel does and does not hold (it stores no credential; every token comes from `gcloud auth print-access-token`), and the response expectation. Keep it under 40 lines.
- [ ] **Step 4: Add the four badges** above.
- [ ] **Step 5: Verify every badge URL** returns 200 fetched anonymously, and paste the status codes into the report. A badge that 404s is the defect this task exists to fix.
- [ ] **Step 6: Note in `CHANGELOG.md`** that the version badge is hand-written and bumps at release.
- [ ] **Step 7: Commit** — `docs: badges that state facts a private repo can back`

---

### Task 5: The four missing architecture diagrams

**Files:**
- Modify: `docs/architecture.md`

`docs/architecture.md` has three Mermaid diagrams: access mode, public mode, and the process model. This repository has no GCP-styled diagram pipeline, so Mermaid is correct here. Vertical layout (`TB`) in full documentation; these are not PR bodies.

- [ ] **Step 1: Jump tunnels.** A diagram for `--via <instance>`: client, local port, the `gcloud` process, IAP, the bastion, and the private host behind it that only the bastion can reach. The existing access diagram stops at the VM and never shows the second hop.
- [ ] **Step 2: Service lifecycle.** One diagram covering both platforms: `add` writes the record, `install` writes the plist or unit, the manager starts the process, failure restarts it, and auto-start is the presence of the plist in `~/Library/LaunchAgents` or `loginctl enable-linger`. Name both paths in one diagram rather than two.
- [ ] **Step 3: The account and config model.** `config.toml` holding accounts, default account, relay and domains per account; `tunnels.toml` holding tunnels keyed by name and account; `agents/` and `logs/` named for `<account>-<name>`; and the launchd label `com.gtunnel.<account>.<name>`. This is what makes two accounts able to hold the same tunnel name.
- [ ] **Step 4: What `relay up` creates.** Every GCP resource, who owns it, and what `relay down` removes: relay VM, backend service per tunnel, URL map host rule, named port, certificate map, DNS record, firewall rules — each tagged `gtunnel:<account>` except the DNS record, which carries no description.
- [ ] **Step 5: Check the placeholder sweep** over the new text, per `.claude/rules/public-repo-placeholders.md`.
- [ ] **Step 6: Commit** — `docs: jump tunnels, service lifecycle, config model and relay resources`

---

### Task 6: The README stops repeating `docs/`

**Files:**
- Modify: `README.md`, `docs/usage.md`

Roughly 90 of the README's 236 lines restate a `docs/` file: the CLI command table restates `usage.md`, the troubleshooting table restates `troubleshooting.md`, and the TUI keymap table restates what `usage.md` and `design.md` own. Duplication is what goes stale, and it already has: the README says "below 100 columns the list stacks", which the code changed to 121.

- [ ] **Step 1: Grep for internal anchors** — `grep -rn "README.md#" . --include="*.md"` and the same for each `docs/` file — and record which anchors must survive. Breaking a bookmark silently is worse than a long README.
- [ ] **Step 2: Move the full keymap table** into `docs/usage.md` under a `## The TUI` section. The README keeps one line saying `?` opens all bindings.
- [ ] **Step 3: Cut the CLI command table** to the six commands a newcomer runs first, with a link to `docs/usage.md` for the rest.
- [ ] **Step 4: Cut the troubleshooting table** to three rows and link to `docs/troubleshooting.md`.
- [ ] **Step 5: Fix the stale 100** to 121 wherever it survives, and re-read the remaining README text against the code rather than against itself.
- [ ] **Step 6: Confirm the README is under 120 lines** and that every table it dropped exists in exactly one `docs/` file.
- [ ] **Step 7: Update `CHANGELOG.md`** under the unreleased section: one bullet for the single mark, one for the documentation split.
- [ ] **Step 8: Commit** — `docs: the README points at docs/ instead of restating it`

---

## Self-Review

**Spec coverage.** Four decisions were taken: option A (Task 1, with Tasks 2 and 3 removing the rival drawings and the rule that permitted them), the code's fallback (Tasks 1 and 2), drop the CI badge and add language, security and version badges (Task 4), and the approved six-file split (Tasks 5 and 6). All four are covered.

**Step scan.** Task 2 has no test because it edits an artifact, not the repository; its correctness is the eleven cells matching Task 1's, which Task 1 does test. Tasks 3 to 6 are documentation and carry verification steps rather than unit tests: a fetched status code in Task 4, a line count and a grep in Task 6.

**Type consistency.** `mark_dots()` is defined once in Task 1 and consumed only by its own test and, indirectly, by the SVG a human writes from it. No later task redefines it.

**Review Focus.** Items 1 to 3 have tests or checks in Task 1; items 4 and 5 are steps in Tasks 4 and 6. None is left as an intention.

**Proportion.** Six tasks, one of which touches Rust. The plan is longer than the diff will be for Tasks 3 to 6, which is right for documentation work, where what to cut is the whole decision and the cutting itself is trivial.
