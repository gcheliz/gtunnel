# TUI Brand Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the shipped TUI render the approved layout exactly — the compound cloud-over-name logo in brand hues, the account pinned top-right in a highlighted field, and a footer that shows only the keys the selection can actually use.

**Architecture:** Three seams, all inside `src/tui/`. `theme.rs` gains a documented truecolor exception for the logo only; status colors stay named-ANSI. `ui.rs` replaces the 4-row block `LOGO` with an 11-cell braille lockup and replaces the static `HELP_BAR` string with a function of selection state and width. `README.md` carries the 21-cell large form so the documentation and the program draw the same mark.

**Tech Stack:** Rust, ratatui, crossterm.

**Spec:** the approved canvas — https://claude.ai/artifact/TwZjfGhQ2ZuNLcMdJMkYgJ (boards: `Main` the layout, `States` every state and width, `Keys` the full keymap). The boards are the binding reference for colour, grouping and shedding order.

## Global Constraints

- The logo is the only place in the TUI permitted to name an exact hue. Every status, label and selection colour stays a named ANSI colour, per the existing contract in `src/tui/theme.rs`.
- Brand hues, exact: line `#4DD0E1`, blue `#4285F4`, red `#EA4335`, yellow `#FBBC04`, green `#34A853`.
- The one-row mark is 11 cells and the name row is `"  gtunnel  "`, also 11 cells. The widths must stay equal or the lockup stops centring.
- The large form is 21 cells over 3 rows, name row `"       gtunnel       "`, also 21.
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
- Produces: `pub const LOGO_LINE: Color`, `LOGO_BLUE`, `LOGO_RED`, `LOGO_YELLOW`, `LOGO_GREEN`, all `Color::Rgb`; `pub const ACCOUNT_BG: Color` (= `ACCENT`) and `ACCOUNT_FG: Color` (= `ON_ACCENT`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_logo_is_the_only_truecolor_in_the_theme() {
    assert_eq!(theme::LOGO_BLUE, Color::Rgb(0x42, 0x85, 0xF4));
    assert_eq!(theme::LOGO_RED, Color::Rgb(0xEA, 0x43, 0x35));
    assert_eq!(theme::LOGO_YELLOW, Color::Rgb(0xFB, 0xBC, 0x04));
    assert_eq!(theme::LOGO_GREEN, Color::Rgb(0x34, 0xA8, 0x53));
    assert_eq!(theme::LOGO_LINE, Color::Rgb(0x4D, 0xD0, 0xE1));
    // the account field reuses the ANSI pair, it is not brand artwork
    assert_eq!(theme::ACCOUNT_BG, theme::ACCENT);
    assert_eq!(theme::ACCOUNT_FG, theme::ON_ACCENT);
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
- Produces: `const MARK_WIDTH: u16 = 11`; `pub fn mark_spans(braille: bool) -> Vec<Span<'static>>` (one row, coloured per band); `pub const NAME_ROW: &str = "  gtunnel  "`; `pub const LARGE_MARK: [&str; 3]` and `pub const LARGE_NAME_ROW: &str`. `HEADER_HEIGHT_FULL` becomes `2`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_lockup_rows_are_the_same_width() {
    let mark: String = ui::mark_spans(true).iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(mark.chars().count(), 11);
    assert_eq!(ui::NAME_ROW.chars().count(), 11);
    let fallback: String = ui::mark_spans(false).iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(fallback.chars().count(), 11, "the fallback must not change the lockup width");
    for row in ui::LARGE_MARK {
        assert_eq!(row.chars().count(), 21);
    }
    assert_eq!(ui::LARGE_NAME_ROW.chars().count(), 21);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test the_lockup_rows_are_the_same_width`
Expected: FAIL, `no function mark_spans in ui`.

- [ ] **Step 3: Replace the block logo in `src/tui/ui.rs`**

Delete `LOGO`, `LOGO_WIDTH`, `HEADER_MIN_WIDTH` and `SUBTITLE_COLUMN` as they stand. The one-row mark is `⠤⠤⢤⣶⣾⣿⣷⣶⡤⠤⠤`, banded by cell index: 0-1 `LOGO_LINE`, 2 `LOGO_BLUE`, 3-4 `LOGO_RED`, 5-6 `LOGO_YELLOW`, 7-8 `LOGO_GREEN`, 9-10 `LOGO_LINE`. The no-braille fallback is `──▗▄▟▀▙▄▖──`, same bands, same 11 cells. `LARGE_MARK` is:

```
⠀⠀⠀⠀⠀⠀⠀⠀⣠⣴⣶⣦⣄⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⢀⣴⣾⣶⣿⣿⣿⣿⣿⣶⣷⣦⡀⠀⠀⠀⠀
⠤⠤⠤⠤⣿⣿⡏⠉⠉⠉⠉⠉⠉⠉⢹⣿⣿⠤⠤⠤⠤
```

Comment why 11 and not 10: `gtunnel` is 7 cells, 11 − 7 = 4, two blank cells each side. A 10-cell mark leaves 3, which no terminal can split.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test the_lockup_rows_are_the_same_width`
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
- Consumes: Task 2's `mark_spans`, `NAME_ROW`, `MARK_WIDTH`; Task 1's `ACCOUNT_BG`/`ACCOUNT_FG`.
- Produces: `pub fn header_lines(width: u16, account: &str) -> Vec<Line<'static>>` returning 2 lines when the lockup fits and 1 when it does not; `header_height` returns `2`/`1` to match.

Row 0 is the mark, then the subtitle `IAP tunnel manager` in `MUTED`, then the account field flush right: `" account "` in `MUTED` followed by `format!(" {account} ")` styled `bg(ACCOUNT_BG).fg(ACCOUNT_FG)` bold. Row 1 is `NAME_ROW` alone, in `#F1F3F4`-equivalent (`Color::White`) bold. The subtitle appears only when it fits whole; the account field has priority over it and is dropped only when even it will not fit, at which point the header falls to the compact one-line form.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_account_sits_flush_right_and_never_touches_the_logo() {
    let lines = ui::header_lines(120, "dev");
    assert_eq!(lines.len(), 2);
    let row0 = line_text(&lines[0]);
    assert!(row0.trim_end().ends_with("dev "), "{row0:?}");
    assert_eq!(row0.chars().count(), 120);
    assert!(row0[..11].contains('\u{2824}'), "the mark keeps the first 11 cells");
}

#[test]
fn a_long_account_name_does_not_overflow_the_row() {
    let lines = ui::header_lines(60, "platform-production-emea");
    assert!(lines.iter().all(|l| line_text(l).chars().count() <= 60));
}

#[test]
fn a_terminal_too_narrow_for_the_lockup_gets_one_line() {
    let lines = ui::header_lines(20, "dev");
    assert_eq!(lines.len(), 1);
    assert_eq!(ui::header_height(20), 1);
}
```

Add the `line_text` helper beside them if it does not already exist: concatenate every span's content.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test header -- --nocapture`
Expected: FAIL — the old tests assert the 4-row block logo, the new ones assert functions that do not exist yet.

- [ ] **Step 3: Rewrite `header_lines` and `header_height`**

Pad between the subtitle and the account field so the field's last cell lands on `width - 1`. Replace the two superseded tests (`the_header_steps_down_before_it_wraps`, and the logo assertion inside `a_short_terminal_keeps_a_visible_tunnel_row`) rather than leaving them asserting the old art.

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

**Files:**
- Modify: `README.md:2-7`
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: Task 2's `LARGE_MARK` and `LARGE_NAME_ROW`.
- Produces: nothing for other tasks.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_readme_draws_the_same_mark_as_the_program() {
    let readme = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    for row in ui::LARGE_MARK {
        assert!(readme.contains(row), "README is missing a row of the mark: {row:?}");
    }
    assert!(readme.contains(ui::LARGE_NAME_ROW.trim_end()));
    assert!(!readme.contains("\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}"),
        "the old block logo is still in the README");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test the_readme_draws_the_same_mark_as_the_program`
Expected: FAIL — the README still holds the six-row block logo.

- [ ] **Step 3: Replace `README.md:2-7` with the large form**

Three rows of mark plus the name row, inside the existing fenced block.

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

Check against the `Main` board: the cloud sits over the name, the name is centred under it, the account is flush right in a cyan field, and the footer shows five actions on the left and three on the right. Then narrow the terminal to 80 and to 48 columns and check against the `States` board.

## Deferred

- The two-row header costs the panes one row against today's four-row logo, so this is a net gain of two rows. If the lockup ever needs breathing room between the cloud and the name, that is a third row and a separate decision.
- `src/tui/ui.rs` currently has uncommitted changes (the four-row README-derived logo). Task 2 replaces that work; decide whether to commit it first for history or discard it.
