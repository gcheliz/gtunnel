# Design Doctrine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give gtunnel one committed document that states how the TUI displays
information, and teach the implementer and reviewer agents to apply it, so
layout judgement stops being rediscovered once per feature.

**Architecture:** One new file, `docs/design.md`, is the single source. It states
each rule, the reason behind it, and the code that already obeys it. Two tests in
`src/tui/mod.rs` keep it from rotting: one asserts every source file it cites
exists, one asserts it names every colour `theme.rs` exports. The two agent specs
in `.claude/agents/` gain a short section each that points at the doc and says
what to do with it — they stay thin, because duplicating the doctrine into two
ignored files is how it drifts.

**Tech Stack:** Markdown, Rust (`std::fs` in tests), ratatui.

**Spec:** this plan is its own spec. The doctrine it encodes was derived in
session from decisions already shipped; each rule in Task 1 names the code that
demonstrates it, and that citation is the evidence the rule is real rather than
invented here.

## Global Constraints

- No emoji, anywhere: doc, code, comments, tests, commit messages.
- No Claude attribution in commits or any file that lands in the repo.
- **Cite symbols and file paths, never bare line numbers.** `docs/plans/2026-10-23-tui-brand-layout.md`
  pinned exact line numbers and exact constants and drifted from the code twice
  in one day; `HEADER_HEIGHT_FULL` alone went 5 to 6 to 4. A citation reads
  `src/tui/ui.rs` plus the item's name.
- Tables and short prose over bullet lists and paragraphs, per the repo's
  documentation rule. One topic, one file: everything about display goes in
  `docs/design.md` and nowhere else.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
  `cargo test` all clean before each task's commit.
- Other sessions edit `src/tui/*.rs`, `CHANGELOG.md`, `docs/usage.md`,
  `README.md`, `Cargo.toml` and `.github/` concurrently. Task 1 adds tests to
  `src/tui/mod.rs`; append them, change nothing already in the file, and touch
  none of the others.

## Review Focus

Failure modes this plan's own tests do not cover, most likely to bite first:

1. A rule stated in `docs/design.md` that the code contradicts today — the doc
   becomes a lie the moment it ships. (Task 1 Step 5 checks each rule against its
   cited code by reading it.)
2. A reviewer applying a display rule as a blocking finding on code the rule was
   never meant to govern — the CLI's `gtunnel list`, not the TUI. (Task 2 scopes
   the section explicitly.)
3. The doc growing a second copy of the colour contract that `theme.rs` already
   states in its module comment, so the two drift. (Task 1 Step 3: the doc points
   at `theme.rs` for the contract and states only the display consequence.)
4. An agent treating the doctrine as style preference and raising Minor findings
   on every diff, drowning the real ones. (Task 2 gives each rule a severity
   anchor: a display rule is Important only when a person running gtunnel is
   misled or loses information.)
5. `.claude/` is git-ignored, so Task 2's edits exist on one machine only and a
   fresh clone has agents that never heard of the doc. (Task 2 Step 4 records
   this openly; the doc itself is committed and is the durable half.)

---

### Task 1: The design doctrine

**Files:**
- Create: `docs/design.md`
- Modify: `README.md` (the documentation table, the rows listing `docs/*.md`)
- Test: `src/tui/mod.rs` (append to the existing `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `docs/design.md`, cited by Task 2. Its section headings are the
  stable anchors Task 2 links to: `## Hierarchy`, `## Density`, `## Colour`,
  `## Narrowing`, `## Artwork`, `## Tables`, `## Reading a layout`.

- [x] **Step 1: Write the failing tests**

```rust
/// The doc cites code. A citation that no longer resolves is worse than no
/// citation: it sends the next reader to a file that does not exist and
/// quietly teaches them the rule is stale too.
#[test]
fn the_design_doc_cites_only_files_that_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = std::fs::read_to_string(root.join("docs/design.md")).unwrap();
    let mut checked = 0;
    for token in doc.split(|c: char| !(c.is_alphanumeric() || "/._-".contains(c))) {
        if token.starts_with("src/") && token.ends_with(".rs") {
            assert!(root.join(token).exists(), "docs/design.md cites {token}, which does not exist");
            checked += 1;
        }
    }
    assert!(checked >= 6, "the doctrine should cite the code it describes, found {checked}");
}

/// Every colour the theme exports has a documented job. A colour the doc does
/// not name is a colour with no stated meaning, which is how a palette turns
/// into decoration.
#[test]
fn the_design_doc_names_every_colour_the_theme_exports() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = std::fs::read_to_string(root.join("docs/design.md")).unwrap();
    let theme = std::fs::read_to_string(root.join("src/tui/theme.rs")).unwrap();
    for line in theme.lines() {
        let Some(rest) = line.strip_prefix("pub const ") else { continue };
        let name = rest.split(':').next().unwrap().trim();
        assert!(doc.contains(name), "docs/design.md never mentions theme::{name}");
    }
}
```

Shipped form: the second test also asserts it saw at least 13 constants, so a
refactor of `theme.rs` that stops matching `pub const ` cannot pass it vacuously.

- [x] **Step 2: Run them and watch them fail**

Run: `cargo test design_doc`
Expected: FAIL on both, from `unwrap()` on a missing `docs/design.md`.

- [x] **Step 3: Write `docs/design.md`**

Six rule sections, in this order, each stating the rule, the reason, and the code
that already obeys it. The reason is the part that travels: a rule without one
is a preference, and the next person overrules it.

| Section | The rule it states | Code it cites |
|---|---|---|
| `## Hierarchy` | Three tiers and no more: `MUTED` furniture, `LABEL` names a value, bold is the value itself. The datum is the only bright thing on its row. The account field is the one filled element (`account_field`, `FIELD_BG`, no border, no label); the selection is a shade (`SELECTED_BG`), and `FIELD_BG` is a different shade so the two never read as one control | `src/tui/theme.rs` (`MUTED`, `LABEL`, `FIELD_BG`), `src/tui/ui.rs` (`field`, `inline`, `value`, `account_field`) |
| `## Density` | A row is scarce. A row repeating what an adjacent row already says is stolen from one that informs. Details gains a `project` row only because the target line lacks it; the footer shows what the selection can do and the full keymap lives on the help page | `src/tui/ui.rs` (`detail_lines`: the `TunnelSpec::Access` arm is deliberately empty and the `project` row is conditional; `footer_groups`, `KEYMAP`) |
| `## Colour` | One colour, one question. A setting is not a verdict. `theme.rs` owns the ANSI contract; this section states only the display consequence and does not restate it. Every exported constant is named, which the second test enforces: `ACCENT`, `MUTED`, `LABEL`, `OK`, `WARN`, `BAD`, `FLAG`, `SELECTED_BG`, `FIELD_BG` and the five `LOGO_*` | `src/tui/theme.rs` |
| `## Narrowing` | What a pane sheds as the terminal narrows is decided and tested, never emergent. Name the shedding order; a half-drawn element is worse than an absent one. A table of surfaces: header, `STACK_BELOW` (121) stacking, right column, footer, help page (scrolls, pinned way out), dialogs | `src/tui/ui.rs` (`header_lines`, `caption_spans`, `fit_project`, `HEADER_HEIGHT_COMPACT`, `STACK_BELOW`, `right_heights`, `footer_line`, `help_page`, `dialog_rect`) |
| `## Artwork` | The mark is fixed: 11 cells, one row, never rescaled, because braille draws separated dots so scaling the raster scales the gaps, and a terminal cannot scale type at all. The README logo raster is documentation artwork and outside the rule | `src/tui/ui.rs` (`MARK_BRAILLE`, `MARK_ROWS`), `src/tui/mod.rs` (`the_mark_is_the_one_drawn_for_the_design_and_never_another`) |
| `## Tables` | A column sizes to its content with a floor, and every row source shares one layout — including rows discovered after the first was formatted. Geometry is counted in cells, never bytes. The TUI list has its own fit (`fit_columns`: the name gives way to `NAME_FLOOR`, then the url, which `URL_MIN` defends), states for an empty list (`loading`, `could not load tunnels`, `no tunnels in this account`) and a persistent scroll offset on `App` | `src/commands/mod.rs` (`ListLayout::fit`, `NAME_FLOOR`, `URL_FLOOR`), `src/tui/ui.rs` (`fit_columns`, `render_tunnels`) |

Open with two sentences saying what the document is for and that `theme.rs` is
the authority on which colours exist. Close with a short `## Reading a layout`
section: the three questions to ask of any pane — what is the one thing this
pane exists to say, what does the reader lose first when it narrows, and which
row here repeats another.

- [x] **Step 4: Run them and watch them pass**

Run: `cargo test design_doc`
Expected: PASS, both.

- [x] **Step 5: Check each rule against the code it cites**

For each of the six rows above (done against the code as shipped; the corrections are the table cells above and `docs/design.md` itself), open the cited item and confirm the code does
what the rule says. A rule the code contradicts is corrected in the doc, not
asserted over the code — the code shipped and the doc has not. Report any
correction as a `Ruling:` line.

Expected: six confirmations, or a corrected doc plus a `Ruling:` for each change.

- [ ] **Step 6: Add the row to the README documentation table** (not done in the first run: `README.md` was owned by another session; do it when that session lands)

Add `| [docs/design.md](docs/design.md) | How the TUI displays information: hierarchy, density, colour, narrowing |`
to the existing table, after the `docs/architecture.md` row.

- [ ] **Step 7: Run the gate and commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
git add docs/design.md README.md src/tui/mod.rs   # README.md once Step 6 is done
git commit -m "docs: one place that says how the TUI displays information"
```

---

### Task 2: The agents apply it

**Files:**
- Modify: `.claude/agents/rust-implementer.md` (the `## This codebase's conventions` section)
- Modify: `.claude/agents/rust-reviewer.md` (the `## What to check` section, after `**This codebase's contracts**`)
- Test: none possible — see Step 4.

**Interfaces:**
- Consumes: Task 1's `docs/design.md` and its six section headings.
- Produces: nothing for other tasks.

- [x] **Step 1: Add the design section to the reviewer**

A new `**Display and information design**` block inside `## What to check`,
immediately after `**This codebase's contracts**`. It says: read `docs/design.md`
before reviewing any diff that changes what the user sees; then names the four
checks that catch most of it.

| Check | The finding it produces |
|---|---|
| A row, label or field that repeats what an adjacent one already says | the rows it displaces are the finding, named |
| A new colour, or an existing one used for a second meaning | which question the colour already answers |
| A layout that narrows without a decided shedding order, or sheds by truncating mid-element | the width at which it breaks, and what the user sees there |
| Artwork redrawn at another size | `## Artwork`, and the test that already forbids it |

And the severity anchor, verbatim, so the agent does not inflate: *A display
finding is Important when a person running gtunnel is misled or loses
information they had before. It is Minor when the layout is merely less elegant.
Taste is not a finding.*

- [x] **Step 2: Add the matching section to the implementer**

A bullet in `## This codebase's conventions`, in that section's existing register
(observed fact, then the reason). It says `docs/design.md` is read before
changing anything the user sees, names the three questions from its
`## Reading a layout` section, and states that a new display rule discovered
while implementing belongs in `docs/design.md` in the same commit — not in a
code comment where the next feature will not find it.

- [x] **Step 3: Verify both edits landed and resolve**

```bash
cd /Users/gonzalo/workspace/gtunnel
grep -c 'docs/design.md' .claude/agents/rust-implementer.md .claude/agents/rust-reviewer.md
for h in Hierarchy Density Colour Narrowing Artwork Tables; do
  grep -q "^## $h" docs/design.md || echo "MISSING SECTION: $h"
done
echo "SECTIONS_OK"
```
Expected: a count of at least 1 for each agent file, no `MISSING SECTION` line,
and `SECTIONS_OK`.

- [x] **Step 4: Record what this task cannot guarantee**

`.claude/` is matched by `~/.config/git/ignore` and `.git/info/exclude`, so
neither agent file is committed and a fresh clone has neither. State this in the
task report, not in the files: the agents are one machine's configuration, and
`docs/design.md` is the half that travels. Do not "fix" it by committing
`.claude/` — that decision was made deliberately and is enforced by the
pre-commit hook.

- [x] **Step 5: Commit**

Only `docs/` and `README.md` changes from Task 1 are committable; this task's two
files are ignored. There is nothing to commit here. Confirm with:

```bash
git status --porcelain
```
Expected: no `.claude/` entry.

---

## Verification

The tests prove the doc's citations resolve and that it names every colour. They
cannot prove the doctrine is right. Before calling this done, use it once:

```bash
cargo run -- demo
```

Take one pane and answer the three questions from `## Reading a layout` out loud
against what is on screen. If a question cannot be answered from the doc, the doc
is missing a rule, and that is the finding.

## Deferred

- The doctrine is derived from the TUI. `gtunnel list` and the other CLI output
  obey `## Tables` and nothing else in the document; whether the CLI deserves its
  own section is a separate decision, taken once there is a second CLI surface to
  generalise from.
- `docs/plans/2026-10-23-tui-brand-layout.md` drifted from the code twice today
  and was corrected once. Once `docs/design.md` exists, the brand plan's Global
  Constraints duplicate it; folding them in and leaving the plan as history is a
  follow-up, not part of this plan.
