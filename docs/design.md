# How the TUI displays information

The rules below were derived from decisions already shipped, and each one names the code that obeys it. `src/tui/theme.rs` is the authority on which colours exist and why they are ANSI; this document states only what each colour, row and pane is for. Citations are file paths and symbols, never line numbers, so a refactor moves the code without invalidating the sentence. A test (`the_design_doc_cites_only_files_that_exist` in `src/tui/mod.rs`) fails the build when a cited file disappears, and a second (`the_design_doc_names_every_colour_the_theme_exports`) fails it when a colour has no stated job.

Scope: the TUI. `gtunnel list` and the other CLI output obey `## Tables` and nothing else here.

## Hierarchy

Three tiers and no more. The datum is the only bright thing on its row.

| Tier | Style | Used for | Code |
|---|---|---|---|
| Furniture | `theme::MUTED` | rules, pane names, column headers, the project id under the account, captions | `src/tui/theme.rs`, `src/tui/ui.rs` (`rule_line`, `divider_span`, `caption_spans`) |
| Label | `theme::LABEL` | the word that names a value, and the word beside a key in the footer | `src/tui/ui.rs` (`field`, `inline`, `detail`) |
| Datum | bold, default foreground | the value itself | `src/tui/ui.rs` (`value`) |

Why: a label that matches its value forces the eye to read both, and a row of equals has no entry point. Labels are one step brighter than furniture so they stay legible on their own, and still lose to the figure beside them.

The account field is the one filled element: `account_field` draws the account bold over its project id in `theme::MUTED`, on `theme::FIELD_BG`, with no border and no `ACCOUNT` label. A border would cost a fifth header row, and the name is the only thing a label could name. The account is the one piece of state that changes what every key acts on, so it is the brightest thing in the header.

The selection is a shade, not a colour: `theme::SELECTED_BG` plus a bold name in `render_tunnels`. `FIELD_BG` is a different shade on purpose, so the selected row never reads as the same control as the field above it.

## Density

A row is scarce. A row that repeats what an adjacent row already says is stolen from one that informs.

| Where | Rule | Code |
|---|---|---|
| Details, Access tunnel | the `TunnelSpec::Access` arm adds no rows: its zone is inside `target` and its local port is inside `url`. Rows that were pushed off the bottom, `started` and `started by`, are the ones that inform | `src/tui/ui.rs` (`detail_lines`) |
| Details, `project` | one row, only when the account has a project. It is the one fact the target line lacks, and the header's copy is four panes away and absent once the header is gone | `src/tui/ui.rs` (`detail_lines`) |
| Details height | derived from `detail_lines`, never fixed: a fixed height sized for one tunnel kind clipped another | `src/tui/ui.rs` (`render_right`, `DETAILS_CHROME`) |
| List | the auto-start and health flags own fixed cells (`FLAG_CELLS`) so each is a column, and an unmanaged row does not repeat its tag when the url column already says so | `src/tui/ui.rs` (`render_tunnels`) |
| Footer | the keys the selection can use now, not the whole keymap. The full keymap lives on the help page | `src/tui/ui.rs` (`footer_groups`, `KEYMAP`) |

## Colour

One colour, one question. A setting is not a verdict. `src/tui/theme.rs` states the ANSI contract and the one licensed exception; it is not repeated here.

| Constant | The one question it answers | Seen on |
|---|---|---|
| `ACCENT` | what did gtunnel measure or derive | the mode column, the footer's keys, modal borders, the compact header's name |
| `MUTED` | what is only furniture | rules, headers, captions, empty-list rows |
| `LABEL` | what names the value beside it | field labels, footer words |
| `OK` | is it running, healthy, done | a running status symbol, the config page's report of a saved edit |
| `WARN` | is it worth a look while nothing is broken | the health flag, a log hint, a prompt's question, the footer's lead action key |
| `BAD` | is it broken | a failed status symbol, a status-line error, the config page's refusal of an edit |
| `FLAG` | is a setting on, which is a choice and not a verdict | the auto-start flag |
| `SELECTED_BG` | which row has the cursor | the selected list row |
| `FIELD_BG` | what is the account field | the header's account plate |
| `LOGO_LINE`, `LOGO_BLUE`, `LOGO_RED`, `LOGO_YELLOW`, `LOGO_GREEN` | what is the mark | the mark and nothing else; the only exact hues |

Rules that follow: a status colour is never reused to decorate; `FLAG` is never `OK`; no `Color::Rgb` outside the `LOGO_*` constants. White, as a bold name or the footer's lead label, is emphasis and is not a palette entry.

## Narrowing

What a pane sheds as the terminal narrows is decided and tested, never emergent. Name the order; a half-drawn element is worse than an absent one.

| Surface | Sheds, in order | Code |
|---|---|---|
| Header | the captions, then the project id (cut with an ellipsis, then dropped below `PROJECT_FLOOR`), then the lockup. The account name is never cut while a field can still be drawn. When even it does not fit beside the mark the header falls to one row (`HEADER_HEIGHT_COMPACT`) | `src/tui/ui.rs` (`header_lines`, `caption_spans`, `fit_project`) |
| Header, short terminal | the header is dropped whole below 16 rows, because the list is the app and the header only says what it is | `src/tui/ui.rs` (`render`) |
| Layout | below `STACK_BELOW` (121 columns) the list stacks above the other panes at full width. Side by side the list lost the url it exists to show across 100 to 120 columns; stacking costs rows, and a list loses a row more cheaply than a url. `LIST_MIN_ROWS` is the least a stacked list gets | `src/tui/ui.rs` (`STACK_BELOW`, `render`) |
| Right column | Logs keeps `LOGS_MIN` first, because a failing tunnel is diagnosed there; Details is cut next and only for that minimum; Metrics gets the remainder and is dropped whole below `METRICS_MIN` rather than drawn too short to show a figure | `src/tui/ui.rs` (`right_heights`) |
| Footer | whole actions from the end of the contextual group, then `; account`; `?` and `q` are never shed | `src/tui/ui.rs` (`footer_line`) |
| Help page | one column below `HELP_TWO_COLUMNS`. A page that does not fit scrolls instead of dropping rows, because a keymap that cannot be read in full fails its one job; the count line and `Esc back` stay pinned so the way out never scrolls off. Only the cadence lines are dropped | `src/tui/ui.rs` (`help_page`, `HelpPage`) |
| Config page | the page is the keymap page's shape: the file scrolls, and one pinned row carries the count and the keys. Under the file, in the order they give way: the restart note (kept only while the file keeps three rows, and replaced by any message), the blank row above the message, then the file. The message about the last edit is never dropped, only cut to half the rows left, at most four. The keys shed `Tab`, then `e`, never `Esc`, and the count and keys fall to two rows before either is cut. The title is cut from the front so the file name survives. A finished edit is reported on the page itself, because the log title says nothing of a finished operation and the page covers it | `src/tui/ui.rs` (`config_page`, `config_keys`, `tail_fit`) |
| Dialogs | sized to their text, then to the terminal less a margin, centred, with a floor so there is always a row to draw in. Never a percentage of the screen, which is the wrong unit for text | `src/tui/ui.rs` (`dialog_rect`, `dialog_width`) |

## Artwork

The mark is fixed: 11 cells (`MARK_WIDTH`), one row (`MARK_ROWS`), never rescaled. Braille draws separated dots, so scaling the raster scales the gaps and the cloud turns into a dot matrix; the word beneath it cannot grow at all because a terminal cannot scale type. One size is the mark, not a limitation of it.

Code: `src/tui/ui.rs` (`MARK_BRAILLE`, `MARK_FALLBACK`, `NAME_ROW`). The test `the_mark_is_the_one_drawn_for_the_design_and_never_another` in `src/tui/mod.rs` pins the cells. The README carries a separate raster, `docs/logo.png`; it is documentation artwork and does not follow these rules.

## Tables

A column sizes to its content with a floor, and every row source shares one layout, including rows discovered after the first was formatted. Geometry is counted in cells, never bytes.

| Surface | How it sizes | Code |
|---|---|---|
| TUI list | each flexing column takes what its longest value needs while both fit; otherwise the name gives way first, down to `NAME_FLOOR`, and only then the url, which `URL_MIN` defends | `src/tui/ui.rs` (`fit_columns`, `fit`) |
| TUI list states | an empty list says why: `loading`, `could not load tunnels`, or `no tunnels in this account`, in `theme::MUTED`. The header over a blank pane reads as a failed draw | `src/tui/ui.rs` (`render_tunnels`) |
| TUI list scrolling | the offset is held on `App` (`list_offset`) and moves only when the selection would leave the window; it is clamped so a shrunk list or a taller terminal leaves no blank rows | `src/tui/ui.rs` (`render_tunnels`), `src/tui/app.rs` |
| CLI `gtunnel list` | grows to the widest value, never below its floors; rows are collected before any is printed because unmanaged tunnels, discovered last, have the longest names | `src/commands/mod.rs` (`ListLayout::fit`, `NAME_FLOOR`, `URL_FLOOR`) |

The two floors are different numbers on purpose: the CLI prints on an unbounded line and the TUI on a pane.

## Reading a layout

Three questions to ask of any pane, in order.

| Question | If it has no answer |
|---|---|
| What is the one thing this pane exists to say? | the pane is furniture or a duplicate |
| What does the reader lose first when it narrows, and was that decided? | the shedding order is emergent, which is a bug in waiting |
| Which row here repeats another? | that row displaces one that would inform |
