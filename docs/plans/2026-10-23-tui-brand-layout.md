# TUI Brand Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the shipped TUI render the approved layout exactly — the compound cloud-over-name logo in brand hues, the account pinned top-right in a highlighted field, and a footer that shows only the keys the selection can actually use.

**Architecture:** Three seams, all inside `src/tui/`. `theme.rs` gains a documented truecolor exception for the logo only; status colors stay named-ANSI. `ui.rs` replaces the 4-row block `LOGO` with an 11-cell braille lockup and replaces the static `HELP_BAR` string with a function of selection state and width. `README.md` carries the same 11-cell mark, so the documentation and the program draw one logo and not two.

**Tech Stack:** Rust, ratatui, crossterm.

**Spec:** the approved canvas — https://claude.ai/artifact/TwZjfGhQ2ZuNLcMdJMkYgJ (boards: `Main` the layout, `States` every state and width, `Keys` the full keymap). The boards are the binding reference for colour, grouping and shedding order.

## Global Constraints

- The logo is the only place in the TUI permitted to name an exact hue. Every status, label and selection colour stays a named ANSI colour, per the existing contract in `src/tui/theme.rs`.
- Brand hues, exact: line `#4DD0E1`, blue `#4285F4`, red `#EA4335`, yellow `#FBBC04`, green `#34A853`.
- The one-row mark is 11 cells and the name row is `"  gtunnel  "`, also 11 cells. The widths must stay equal or the lockup stops centring.
- **The mark is fixed artwork and is never redrawn at another size.** It is 11 cells and one
  row, everywhere it appears. Scaling it is arithmetically easy and visually fatal: a braille
  glyph draws its dots as separated points, so doubling the raster doubles the gaps between
  them and the cloud reads as a loose dot matrix, while the word beneath it cannot grow at all
  because a terminal cannot scale type. A header that needs more height gains blank rows, not
  a bigger mark.
- Every action string in the footer and in the `?` overlay must correspond to a key actually bound in `src/tui/app.rs`. No key is printed that is not bound; no bound key is absent from `?`.
- No emoji anywhere. No Claude attribution in commits or PR text.
- `cargo fmt` and `cargo clippy` clean, `cargo test` green, before each task's commit.

## Review Focus

Input classes the spec implies but no task's tests exercise, most likely to bite first:

1. A terminal narrower than the 11-cell lockup plus the account field — the account must not overlap the logo or wrap. (Test added to Task 3.)
2. A terminal whose width lands exactly on a shedding boundary, where an action is half-drawn. (Test added to Task 4.)
3. An account name long enough to push the field off the right edge. (Test added to Task 3.)
4. An empty account — no tunnels at all, so there is no selection to derive the contextual group from. (Test added to Task 4.)
5. A terminal with no braille glyphs in its font, where the mark must fall back without changing the lockup's width. (Test added to Task 2.)

---

### Task 1: The brand palette

**Files:**
- Modify: `src/tui/theme.rs`
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub const LOGO_LINE: Color`, `LOGO_BLUE`, `LOGO_RED`, `LOGO_YELLOW`, `LOGO_GREEN`, all `Color::Rgb`; `pub const LABEL: Color` (= `Color::Gray`), the colour of a field's name, one step brighter than the `MUTED` furniture so it reads on its own while still losing to its value.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_logo_is_the_only_truecolor_in_the_theme() {
    assert_eq!(theme::LOGO_BLUE, Color::Rgb(0x42, 0x85, 0xF4));
    assert_eq!(theme::LOGO_RED, Color::Rgb(0xEA, 0x43, 0x35));
    assert_eq!(theme::LOGO_YELLOW, Color::Rgb(0xFB, 0xBC, 0x04));
    assert_eq!(theme::LOGO_GREEN, Color::Rgb(0x34, 0xA8, 0x53));
    assert_eq!(theme::LOGO_LINE, Color::Rgb(0x4D, 0xD0, 0xE1));
    // and nothing else: every colour that answers a question about state
    // stays a named ANSI colour the terminal's theme gets to reinterpret
    for c in [theme::ACCENT, theme::MUTED, theme::LABEL, theme::OK,
              theme::WARN, theme::BAD, theme::FLAG] {
        assert!(!matches!(c, Color::Rgb(..)), "{c:?} names an exact hue");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test the_logo_is_the_only_truecolor_in_the_theme`
Expected: FAIL, `no constant LOGO_BLUE in theme`.

- [ ] **Step 3: Add the constants to `src/tui/theme.rs`**

Extend the module doc comment with the exception and its reason: the logo is fixed artwork whose hues carry brand meaning, so it names them and accepts that a 16-colour terminal approximates them; everything that answers a question about state stays ANSI so the terminal's own theme decides the shade.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test the_logo_is_the_only_truecolor_in_the_theme`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tui/theme.rs src/tui/mod.rs
git commit -m "feat: the theme names the brand hues, for the logo only"
```

---

### Task 2: The compound logo

**Files:**
- Modify: `src/tui/ui.rs:17-39` (the `LOGO` block and its width constants)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: Task 1's `LOGO_*` constants.
- Produces: `pub const MARK_WIDTH: u16 = 11`; `pub const MARK_ROWS: usize = 1`; `pub fn mark_spans(braille: bool, row: usize) -> Vec<Span<'static>>` (coloured per band); `pub const NAME_ROW: &str = "  gtunnel  "`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_lockup_rows_are_the_same_width() {
    for row in 0..ui::MARK_ROWS {
        let mark: String = ui::mark_spans(true, row).iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(mark.chars().count(), ui::MARK_WIDTH as usize);
        let fallback: String = ui::mark_spans(false, row).iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(fallback.chars().count(), ui::MARK_WIDTH as usize,
            "the fallback must not change the lockup width");
    }
    assert_eq!(ui::NAME_ROW.chars().count(), ui::MARK_WIDTH as usize);
}
```

And the test that keeps the mark from being redrawn. It is not decoration: the
mark has been enlarged three times and come out wrong three times, for the
reason in the Global Constraints. Pinning the exact cells makes the fourth
attempt fail here instead of on screen.

```rust
#[test]
fn the_mark_is_the_one_drawn_for_the_design_and_never_another() {
    assert_eq!(ui::MARK_ROWS, 1, "the mark is one row and stays one row");
    assert_eq!(ui::MARK_WIDTH, 11);
    let drawn: Vec<u32> = ui::mark_spans(true, 0).iter()
        .flat_map(|s| s.content.chars()).map(|c| c as u32).collect();
    assert_eq!(drawn, vec![
        0x2824, 0x2824, 0x28a4, 0x28f6, 0x28fe, 0x28ff, 0x28f7, 0x28f6, 0x2864, 0x2824, 0x2824,
    ], "the mark must be the cloud from the design, cell for cell");
    assert_eq!(ui::NAME_ROW, "  gtunnel  ");
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test the_lockup_rows_are_the_same_width the_mark_is_the_one_drawn`
Expected: FAIL, `no function mark_spans in ui`.

- [ ] **Step 3: Replace the block logo in `src/tui/ui.rs`**

Delete `LOGO`, `LOGO_WIDTH`, `HEADER_MIN_WIDTH` and `SUBTITLE_COLUMN` as they stand. The one-row mark is `⠤⠤⢤⣶⣾⣿⣷⣶⡤⠤⠤`, banded by cell index: 0-1 `LOGO_LINE`, 2 `LOGO_BLUE`, 3-4 `LOGO_RED`, 5-6 `LOGO_YELLOW`, 7-8 `LOGO_GREEN`, 9-10 `LOGO_LINE`. The no-braille fallback is `──▗▄▟▀▙▄▖──`, same bands, same 11 cells.

`mark_spans` takes a row index although there is only one row, so the header's
layout code does not have to change if the art ever gains a row of its own.
`MARK_ROWS` is the single place that number lives.

Comment why 11 and not 10: `gtunnel` is 7 cells, 11 − 7 = 4, two blank cells each side. A 10-cell mark leaves 3, which no terminal can split. Comment on `MARK_BRAILLE` why it is never redrawn larger, so the next reader finds the reason before the test does.

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test the_lockup_rows_are_the_same_width the_mark_is_the_one_drawn`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tui/ui.rs src/tui/mod.rs
git commit -m "feat: the logo is a cloud over the name, eleven cells wide"
```

---

### Task 3: The header row

**Files:**
- Modify: `src/tui/ui.rs` (`header_lines`, `header_height`)
- Test: `src/tui/mod.rs:560-594` (the two existing header tests change)

**Interfaces:**
- Consumes: Task 2's `mark_spans`, `NAME_ROW`, `MARK_WIDTH`, `MARK_ROWS`; Task 1's `MUTED` and `LABEL`.
- Produces: `pub fn header_lines(width: u16, height: u16, account: &str) -> Vec<Line<'static>>` returning `HEADER_HEIGHT_FULL` (6) lines when the lockup fits and 1 when it does not; `header_height` returns `6`/`1` to match, derived from `header_lines` rather than recomputed.

Six rows:

| Row | Contents |
|---|---|
| 0 | blank |
| 1 | the mark, the divider, then the account box's top `╭─╮` flush right |
| 2 | `NAME_ROW` in `Color::White` bold, the divider, the subtitle `IAP tunnel manager` in `MUTED`, then `│ ACCOUNT  dev │` flush right |
| 3 | the account box's bottom `╰─╯` flush right |
| 4 | blank |
| 5 | a rule across the width |

The box is three rows tall and the lockup is two, and a cell grid has no row between two rows,
so one of them has to be off by half a row. The box's text sits on the name's row: the name is
the heaviest thing in the lockup, so the subtitle and the account read as one line with it, and
the box opens on the mark's row and closes on the row below the name. `ACCOUNT` is in `LABEL`
and the account itself is bold.

The account is a box and not a filled chip because it is the one control on the header: a
block of solid colour reads as a status light, something the program is telling you, while an
outline reads as a field — something you can change, which `;` does.

The subtitle appears only when it fits whole; the account box has priority over it and is
dropped only when even it will not fit, at which point the header falls to the compact
one-line form. The mark does not grow to fill a taller header — see the Global Constraints —
so `height` only decides full against compact.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_account_sits_flush_right_and_never_touches_the_logo() {
    let lines = ui::header_lines(120, 24, "dev");
    assert_eq!(lines.len() as u16, ui::HEADER_HEIGHT_FULL);
    assert_eq!(line_text(&lines[0]), "");
    let top = line_text(&lines[1]);
    assert!(top.trim_end().ends_with('\u{256e}'), "{top:?}");
    assert!(line_text(&lines[3]).trim_end().ends_with('\u{256f}'));
    assert_eq!(line_text(&lines[4]), "");
    let row0 = line_text(&lines[2]);
    // One cell of margin at each end, the same the footer and the panes have.
    assert!(row0.ends_with(" ACCOUNT  dev \u{2502} "), "{row0:?}");
    assert_eq!(row0.chars().count(), 120);
    assert!(row0.contains(ui::NAME_ROW), "{row0:?}");
    assert!(row0.contains("\u{2502}  IAP tunnel manager"), "{row0:?}");
    // The divider runs the lockup's whole height, and belongs to the lockup
    // rather than to the box. Cells, not bytes: a braille cell is three
    // bytes, so `find` would put the divider past the mark on the mark's row.
    for row in [1 + ui::MARK_ROWS - 1, 1 + ui::MARK_ROWS] {
        let text = line_text(&lines[row]);
        let divider = text.chars().position(|c| c == '\u{2502}')
            .expect("the divider runs every row");
        assert!(divider < ui::MARK_WIDTH as usize + 6, "{text:?}");
    }
}
```

```rust
#[test]
fn a_long_account_name_does_not_overflow_the_row() {
    let lines = ui::header_lines(60, 40, "platform-production-emea");
    assert!(lines.iter().all(|l| line_text(l).chars().count() <= 60));
}

#[test]
fn a_terminal_too_narrow_for_the_lockup_gets_one_line() {
    let lines = ui::header_lines(20, 40, "dev");
    assert_eq!(lines.len(), 1);
    assert_eq!(ui::header_height(20, 40, "dev"), ui::HEADER_HEIGHT_COMPACT);
}
```

Add the `line_text` helper beside them if it does not already exist: concatenate every span's content.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test header -- --nocapture`
Expected: FAIL — the old tests assert the 4-row block logo, the new ones assert functions that do not exist yet.

- [ ] **Step 3: Rewrite `header_lines` and `header_height`**

Pad between the subtitle and the account box so the box's last cell lands on `width - 1`, measuring every span in cells rather than bytes. Replace the two superseded tests (`the_header_steps_down_before_it_wraps`, and the logo assertion inside `a_short_terminal_keeps_a_visible_tunnel_row`) rather than leaving them asserting the old art.

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test header`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tui/ui.rs src/tui/mod.rs
git commit -m "feat: the account is pinned top right in a highlighted field"
```

---

### Task 4: The contextual footer

**Files:**
- Modify: `src/tui/ui.rs:88-89` (`HELP_BAR`) and `:551` (its render site)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: `TunnelStatus` from `crate::state`.
- Produces:

```rust
pub struct Action { pub key: &'static str, pub label: &'static str }
pub fn footer_groups(selected: Option<TunnelStatus>, any_tunnels: bool)
    -> (&'static [Action], &'static [Action]);
pub fn footer_line(width: u16, selected: Option<TunnelStatus>, any_tunnels: bool)
    -> Line<'static>;
```

The groups, exactly as the `States` board draws them:

| Selection | Contextual group, in order |
|---|---|
| Running | `S stop`, `R restart`, `c copy`, `o open`, `L clear log` |
| Stopped | `s start`, `e edit`, `d delete`, `A auto` |
| Error | `R restart`, `e edit`, `d delete`, `L clear log` |
| No tunnels | `a add a tunnel`, `m import` |

The program group is always `; account`, `? keys`, `q quit`, flush right. Keys render in `ACCENT` bold, labels in the default foreground. Shedding: drop actions from the **end** of the contextual group until the whole line fits; the program group never sheds until the contextual group is empty, then `; account` goes, leaving `? keys  q quit`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_footer_offers_only_what_the_selection_can_do() {
    let (ctx, _) = ui::footer_groups(Some(TunnelStatus::Running), true);
    let keys: Vec<_> = ctx.iter().map(|a| a.key).collect();
    assert_eq!(keys, ["S", "R", "c", "o", "L"]);
    assert!(!keys.contains(&"s"), "a running tunnel cannot be started");

    let (ctx, _) = ui::footer_groups(Some(TunnelStatus::Stopped), true);
    let keys: Vec<_> = ctx.iter().map(|a| a.key).collect();
    assert_eq!(keys, ["s", "e", "d", "A"]);
    assert!(!keys.contains(&"S"), "s and S are never on screen together");
}

#[test]
fn an_empty_account_offers_a_way_to_fill_it() {
    let (ctx, _) = ui::footer_groups(None, false);
    assert_eq!(ctx.iter().map(|a| a.key).collect::<Vec<_>>(), ["a", "m"]);
}

#[test]
fn the_footer_sheds_whole_actions_and_never_half_of_one() {
    for width in 20u16..=140 {
        let text = line_text(&ui::footer_line(width, Some(TunnelStatus::Running), true));
        assert!(text.chars().count() <= width as usize, "overflows at {width}");
        for action in ["stop", "restart", "clear log", "account", "keys", "quit"] {
            let starts = text.matches(&format!(" {action}")).count();
            assert!(starts <= 1, "{action} drawn twice at {width}");
        }
        assert!(!text.ends_with("cle") && !text.ends_with("accou"),
            "an action was cut mid-word at {width}: {text:?}");
    }
}

#[test]
fn the_way_out_survives_the_narrowest_terminal() {
    let text = line_text(&ui::footer_line(24, Some(TunnelStatus::Running), true));
    assert!(text.contains("keys") && text.contains("quit"));
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test footer`
Expected: FAIL, `no function footer_groups in ui`.

- [ ] **Step 3: Implement the three items and rewire the render site**

Thread the selected tunnel's status and the list length from `App` to the footer render call at `src/tui/ui.rs:551`.

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test footer`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add src/tui/ui.rs src/tui/mod.rs
git commit -m "feat: the footer shows the keys the selection can use"
```

---

### Task 5: The keymap behind `?`

**Files:**
- Modify: `src/tui/ui.rs` (the help overlay)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: Task 4's `Action`.
- Produces: `pub fn keymap_sections() -> &'static [(&'static str, &'static [Action])]` — four sections titled `TUNNEL`, `THE LIST`, `THIS TUNNEL'S URL AND LOG`, `THE PROGRAM`, matching the `Keys` board.

The complete set, every one bound in `app.rs`: `s S R A h` / `a e d m j k` / `c o L r` / `; ? q Esc`. `m` and `h` are the two that no footer has ever printed.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_bound_key_is_documented_behind_question_mark() {
    let documented: Vec<&str> = ui::keymap_sections()
        .iter().flat_map(|(_, acts)| acts.iter().map(|a| a.key)).collect();
    for key in ["s", "S", "R", "A", "h", "a", "e", "d", "m", "c", "o", "L", "r", ";", "?", "q"] {
        assert!(documented.contains(&key), "{key} is bound in app.rs but undocumented");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test every_bound_key_is_documented_behind_question_mark`
Expected: FAIL, `no function keymap_sections in ui`.

- [ ] **Step 3: Implement `keymap_sections` and render the overlay from it**

Close the overlay with the note the `Keys` board carries: case is load-bearing in three places, `s`/`S`, `a`/`A`, `r`/`R`.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test every_bound_key_is_documented_behind_question_mark`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tui/ui.rs src/tui/mod.rs
git commit -m "feat: ? documents every bound key, m and h included"
```

---

### Task 6: The README wears the same mark

> **Superseded.** The README now carries `docs/logo.png` (done outside this plan), so the text-mark test below no longer applies and no step of this task should be run. Kept as history.

**Files:**
- Modify: `README.md:2-7`
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: Task 2's `mark_spans` and `NAME_ROW`.
- Produces: nothing for other tasks.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_readme_draws_the_same_mark_as_the_program() {
    let readme = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    for row in 0..ui::MARK_ROWS {
        let mark: String = ui::mark_spans(true, row).iter()
            .map(|s| s.content.as_ref()).collect();
        assert!(readme.contains(&mark), "README is missing a row of the mark: {mark:?}");
    }
    assert!(readme.contains(ui::NAME_ROW.trim_end()));
    assert!(!readme.contains("\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}"),
        "the old block logo is still in the README");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test the_readme_draws_the_same_mark_as_the_program`
Expected: FAIL — the README still holds the six-row block logo.

- [ ] **Step 3: Replace `README.md:2-7` with the mark**

The one mark row plus the name row, inside the existing fenced block. The README gets the
same 11 cells the program draws, not a larger variant of them: a README that shows a bigger
logo than the program has is the same mistake as a program that tries to draw one.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test the_readme_draws_the_same_mark_as_the_program`
Expected: PASS.

- [ ] **Step 5: Run everything and commit**

```bash
cargo fmt --check && cargo clippy -- -D warnings && cargo test
git add README.md src/tui/mod.rs
git commit -m "docs: the README wears the mark the program draws"
```

---

## Verification

The suite is not evidence that the layout is right — it is evidence that the strings and widths are. Before calling this done, run the program and look at it:

```bash
cargo run -- demo
```

Check against the `Main` board: the cloud sits over the name, the name is centred under it, the account is flush right in its outlined box, and the footer shows five actions on the left and three on the right. Then narrow the terminal to 80 and to 48 columns and check against the `States` board.

## Deferred

- The header settled at five rows, not the two this plan first specified: the lockup, a blank
  row and a rule, with the account box spanning the first three. The extra height is whitespace
  and furniture around a mark that stays 11 cells by one row.
- Three attempts were made to enlarge the mark for the taller header, and all three were
  reverted. The reason is now a Global Constraint and a test; do not spend a fourth.
- Task 6 is superseded (the README carries `docs/logo.png`); Task 5 is open here but shipped with the keymap page. Task 4 shipped, and the account indicator it duplicated in the footer
  was removed there rather than here.
