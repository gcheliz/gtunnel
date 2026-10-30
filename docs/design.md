# How the TUI displays information

The rules below were derived from decisions already shipped, and each one names the code that obeys it. `src/tui/theme.rs` is the authority on which colours exist and why they are ANSI; this document states only what each colour, row and pane is for. Citations are file paths and symbols, never line numbers, so a refactor moves the code without invalidating the sentence. A test (`the_design_doc_cites_only_files_that_exist` in `src/tui/tests.rs`) fails the build when a cited file disappears, and a second (`the_design_doc_names_every_colour_the_theme_exports` in `src/tui/ui/tests.rs`) fails it when a colour has no stated job.

Scope: the TUI. `gtunnel list` and the other CLI output obey `## Tables` and nothing else here.

## Hierarchy

Three tiers and no more. The datum is the only bright thing on its row.

| Tier | Style | Used for | Code |
|---|---|---|---|
| Furniture | `theme::MUTED` | rules, pane names, column headers, the project id under the account, captions | `src/tui/theme.rs`, `src/tui/ui.rs` (`rule_line`, `divider_span`, `caption_spans`) |
| Label | `theme::LABEL` | the word that names a value, and the word beside a key in the footer | `src/tui/ui.rs` (`field`, `inline`, `detail`) |
| Datum | bold, default foreground | the value itself | `src/tui/ui.rs` (`value`) |

Why: a label that matches its value forces the eye to read both, and a row of equals has no entry point. Labels are one step brighter than furniture so they stay legible on their own, and still lose to the figure beside them.

The account field is the one filled element: `account_field` draws the account bold over its project id in `theme::FIELD_SUBTITLE`, on `theme::FIELD_BG`, with no border and no `ACCOUNT` label. A border would cost a fifth header row, and the name is the only thing a label could name. The account is the one piece of state that changes what every key acts on, so it is the brightest thing in the header.

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
| `MUTED` | what is only furniture | rules, headers, captions, empty-list rows, the stopped circle in Metrics |
| `LABEL` | what names the value beside it | field labels, footer words |
| `OK` | is it running, healthy, done | a running status symbol, the config page's report of a saved edit |
| `WARN` | is it worth a look while nothing is broken | the health flag, a pending row's glyph and verb, a log hint, a prompt's question, the footer's lead action key, the version in an update notice |
| `BAD` | is it broken | a failed status symbol, any failing health (unhealthy, auth expired, crash loop: the row's flag is the same warning for all three and the Metrics symbol and `fix` row name which), a status-line error, the config page's refusal of an edit |
| `FLAG` | is a setting on, which is a choice and not a verdict | the auto-start flag |
| `SELECTED_BG` | which row has the cursor | the selected list row |
| `FIELD_BG` | what is the account field | the header's account plate |
| `FIELD_SUBTITLE` | what is the text under the account name | the project id on the account plate, lighter than `MUTED` so it reads on the fill |
| `LOGO_LINE`, `LOGO_BLUE`, `LOGO_RED`, `LOGO_YELLOW`, `LOGO_GREEN` | what is the mark | the mark and nothing else; the only exact hues |

Rules that follow: a status colour is never reused to decorate; `FLAG` is never `OK`; no `Color::Rgb` outside the `LOGO_*` constants. White, as a bold name or the footer's lead label, is emphasis and is not a palette entry.

## Narrowing

What a pane sheds as the terminal narrows is decided and tested, never emergent. Name the order; a half-drawn element is worse than an absent one.

| Surface | Sheds, in order | Code |
|---|---|---|
| Header | the captions, then the project id (cut with an ellipsis, then dropped below `PROJECT_FLOOR`), then the lockup. The account name is never cut while a field can still be drawn. When even it does not fit beside the mark the header falls to one row (`HEADER_HEIGHT_COMPACT`) | `src/tui/ui.rs` (`header_lines`, `caption_spans`, `fit_project`) |
| Header, short terminal | the header is dropped whole below 16 rows, because the list is the app and the header only says what it is | `src/tui/ui.rs` (`render`) |
| Update notice | the command, then the version, each only while `RULE_LEAD` dashes stay ahead of it. When the last check failed and no update shows, `update check failed` takes the same place and sheds whole under the same rule; an update outranks it. It lives in the header's rule and moves to the footer's when the header is gone or one row, never both, so it costs no row of its own | `src/tui/ui.rs` (`rule_line_with`, `rule_line_noting`, `render`) |
| Layout | below `STACK_BELOW` (121 columns) the list stacks above the other panes at full width. Side by side the list lost the url it exists to show across 100 to 120 columns; stacking costs rows, and a list loses a row more cheaply than a url. `LIST_MIN_ROWS` is the least a stacked list gets | `src/tui/ui.rs` (`STACK_BELOW`, `render`) |
| Stacked list | sized to its entries (`LIST_CHROME` plus a row each), at most `LIST_MAX_PERCENT` of the height and at least `LIST_MIN_ROWS`; past that it scrolls and a scrollbar on its border says so. The list shows that rows are hidden because a list that silently shows 3 of 7 reads as the whole account | `src/tui/ui.rs` (`stacked_heights`, `render_tunnels`) |
| Stacked summary | Details folds into the list's bottom border as its title, left-aligned with a cell of air after each corner, so the pane it describes frames it and it costs no row (a loose line between two panes read as stray output). The scrollbar's arrows sit on the right border above that row and the line never reaches them. Whole parts go from the end: auto-start, then uptime, then the target; the name, the status and the url are clipped with `…` only when not even they fit | `src/tui/ui.rs` (`summary_line`) |
| Stacked Logs and Metrics | from `BESIDE_FROM` (80) columns side by side, 60 and 40 percent, both the full remaining height; narrower, Logs takes the width and Metrics is one row (`metrics_strip`). The chart is the last thing to go: it keeps the rows left after the figures, at least one, and the figures are cut from the end first | `src/tui/ui.rs` (`bottom_split`, `metrics_split`, `metrics_strip`) |
| Right column | Logs keeps `LOGS_MIN` first, because a failing tunnel is diagnosed there; Details is cut next and only for that minimum; Metrics gets the remainder and is dropped whole below `METRICS_MIN` rather than drawn too short to show a figure | `src/tui/ui.rs` (`right_heights`) |
| Footer | whole actions from the end of the contextual group (`L clear log`, then `PgUp logs`, which never outranks `c copy` or `o open`), then `; account`; `?` and `q` are never shed | `src/tui/ui.rs` (`footer_line`) |
| Help page | one column below `HELP_TWO_COLUMNS`. A page that does not fit scrolls instead of dropping rows, because a keymap that cannot be read in full fails its one job; the count line and `Esc back` stay pinned so the way out never scrolls off. Only the cadence lines are dropped | `src/tui/ui.rs` (`help_page`, `HelpPage`) |
| Config page | the page is the keymap page's shape: the file scrolls, and one pinned row carries the count and the keys. Under the file, in the order they give way: the restart note (kept only while the file keeps three rows, and replaced by any message), the blank row above the message, then the file. The message about the last edit is never dropped, only cut to half the rows left, at most four. The keys shed `Tab`, then `e`, never `Esc`, and the count and keys fall to two rows before either is cut. The title is cut from the front so the file name survives. A finished edit is reported on the page itself, because the log title says nothing of a finished operation and the page covers it | `src/tui/ui.rs` (`config_page`, `config_keys`, `tail_fit`) |
| Dialogs | sized to their text, then to the terminal less a margin, centred, with a floor so there is always a row to draw in. Never a percentage of the screen, which is the wrong unit for text | `src/tui/ui.rs` (`dialog_rect`, `dialog_width`) |
| Pickers | the first column (an instance, project or configuration name) takes the room the other columns leave, up to its widest value, and is cut with an ellipsis through `fit`; the zone or project beside it keeps its width, so a long name never pushes it off the row. `PICKER_NAME_FLOOR` is the least a name keeps before the other columns are cut instead. Under the list one muted detail line gives the highlighted row in full, only while some row is cut (otherwise it would repeat the row), padded to the tallest a row would wrap so the list does not move with the selection. It is dropped before any list row, as the description is, while `PICKER_ROWS_KEPT` rows remain. A picker that hides rows by default says how many in its description, with the key that shows them, because the list cannot say what it leaves out; the instance picker's `iap` is a third column that is empty on most rows, a mark and not a value | `src/tui/ui.rs` (`picker_columns`, `render_picker`) |

What the reader sees at each threshold:

| Surface | On screen |
|---|---|
| Header | the subtitle beside the logo goes first; the project id is cut with `…`, never below 4 cells (`PROJECT_FLOOR`), and dropped whole below that; the account name goes last. An account with no project shows the name alone |
| Update notice | `↑ v<latest> · <command>` loses the command, then the version; a step shows only while 8 dashes (`RULE_LEAD`) stay before it, and a string is never cut |
| Layout | from 121 columns the list is the left 46% and Details, Logs and Metrics stack on the right; below 121 the list takes the full width above them |
| Stacked panes | at 80 columns and up the list, a one-line summary of the selection, then Logs and Metrics side by side; under 80 columns Metrics is the last row, `14 ms  peak 21  active 3  ✓` and a one-row chart. Details is only in the side-by-side layout, where it has the room |
| Right column | Logs keeps at least 4 rows (`LOGS_MIN`); Metrics shrinks, then disappears, before Logs does. A Details value too long for its row (a URL, a target, how the tunnel was started) wraps under its label instead of being cut |
| List | the name column is as wide as the longest name and shrinks, with `…` and to no fewer than 8 cells (`NAME_FLOOR`), only when the pane could not otherwise hold a 12-cell URL (`URL_MIN`). A trailing `-<account>` suffix is hidden (`display_name` in `src/tui/app.rs`); Details shows the stored name. The list scrolls only when the selection would leave the window |
| Form dialogs | on a short terminal the line under a field's label goes first, so the field always fits |
| Help page | two columns from 92 cells inside its frame (`HELP_TWO_COLUMNS`), one below. Too short a terminal scrolls it: `j` `k` and the arrows move a row, `PgUp` `PgDn` a screen, `g` `G` the top and the end, and `3-14 of 31 rows` says where the window is. Both columns scroll together, the count and `Esc back  q back` lines stay put, closing resets to the top, and a page that fits shows no counter |

## Artwork

The mark is fixed: 11 cells (`MARK_WIDTH`), one row (`MARK_ROWS`), never rescaled. Braille draws separated dots, so scaling the raster scales the gaps and the cloud turns into a dot matrix; the word beneath it cannot grow at all because a terminal cannot scale type. One size is the mark, not a limitation of it.

There is one mark, and print does not get a second one. The eleven cells of `MARK_BRAILLE` are the drawing; `docs/logo.svg` is those same dots re-plotted, one circle per lit braille dot on the 22 by 4 lattice a row of braille cells describes, in the same five hues. `docs/logo.png` is rendered from that SVG, and the README embeds it. The non-braille `MARK_FALLBACK` is the same silhouette again in quadrant blocks, for a font with no braille coverage.

The gaps between the dots are load-bearing, so print keeps them. The silhouette's widest row is the one below the line, and the line itself spans all 22 columns: separated, that reads as a line running behind a cloud, which is the mark. Closed up — dots drawn as squares as wide as the pitch — the line merges into the body and the whole thing reads as one bottom-heavy blob.

The fallback has the matching trap. Quadrant blocks give two rows for the braille's four, so each cell reduces to a full block, a half block, or `─` where the cell carries only the line. An upper-half block is never right: no cell in the shape has empty lower rows, so one punches a hole under the cloud.

| | |
|---|---|
| Code | `src/tui/ui.rs` (`MARK_BRAILLE`, `MARK_FALLBACK`, `NAME_ROW`, `mark_dots`) |
| The cells are pinned by | `the_mark_is_the_one_drawn_for_the_design_and_never_another` in `src/tui/ui/tests.rs` |
| The banner is pinned to them by | `the_readme_banner_is_the_mark_dot_for_dot`, which reads every circle out of `docs/logo.svg` and compares it to `mark_dots()` |
| The fallback is pinned by | `the_fallback_is_the_one_the_code_draws_not_the_canvas` |

Regenerating the banner means editing `docs/logo.svg` and re-rendering `docs/logo.png` from it; the test fails until the two agree again.

## Tables

A column sizes to its content with a floor, and every row source shares one layout, including rows discovered after the first was formatted. Geometry is counted in cells, never bytes.

| Surface | How it sizes | Code |
|---|---|---|
| TUI list | each flexing column takes what its longest value needs while both fit; otherwise the name gives way first, down to `NAME_FLOOR`, and only then the url, which `URL_MIN` defends | `src/tui/ui.rs` (`fit_columns`, `fit`) |
| TUI status messages | every message a key leaves is drawn in the Logs title: a plain one (`<name> is already running`, `opened <url>`, `account <name>`) for three seconds, then the title returns to the pane name, so it never claims an operation is still under way; an error (red border, `set_error`) until the next result replaces it. Order in the title: copy flash, fresh plain status, running spinner or error, then a standing offer. The one offer that outlasts three seconds is the watchdog (`set_offer`), because its next step is a command typed elsewhere; a spinner or an error replaces it, and so do the next status and an installed watchdog. A new message that is only stored and never drawn fails `every_status_a_key_sets_is_in_the_logs_title`. The footer is untouched: only the copy flash, a confirmation, draws a check mark there | `src/tui/app.rs` (`set_status`, `set_error`, `set_offer`), `src/tui/ui.rs` (`render_logs`) |
| TUI list states | an empty list says why: `loading`, `could not load tunnels`, or `no tunnels in this account`, in `theme::MUTED`. The header over a blank pane reads as a failed draw | `src/tui/ui.rs` (`render_tunnels`) |
| TUI list, pending row | a row with an op running replaces its status symbol with `PENDING_GLYPH` and, in the tag's place, says `<verb>ing… <secs>s` (auto-start reads `enabling auto-start` or `disabling auto-start`, whichever the key will do), both in `theme::WARN`, on the frame of the key press. The row exists to say that the key was heard, so it is not muted whatever the status was. Narrowing clips the words first and the glyph last, because the glyph alone still says something is under way. The state ends on the first entries round begun after the op returned, never on the op's own return, so the row never shows the state from before the op. A key that would start a second op on the row is refused on the status line, and so are rename, move and edit, which mark the row too: `e` on a busy row is refused before the dialog opens, and the submit checks again in case an op started while it was typed. The marker is kept by account and name, so a switch of account and back does not lose it | `src/tui/ui.rs` (`render_tunnels`, `PENDING_GLYPH`), `src/tui/app.rs` (`PendingOp`, `refuse_pending`) |
| TUI log scrolling | the view is a count of lines newer than the last one drawn (`LogScroll`), so a line that wraps is one step and the title's `N newer` is checkable against the file. A line arriving while scrolled back raises the count by the same amount and the view holds; zero is following. The count is clamped to the last draw's measure, so a taller terminal or a shorter log leaves no blank rows. A selection or account change, and a read that shares no line with the last, return to following | `src/tui/app.rs` (`LogScroll`, `set_logs`, `lines_arrived`), `src/tui/ui.rs` (`render_logs`, `log_extent`) |
| TUI mouse | only the wheel is read, and only on the plain list screen: over TUNNELS it moves the selection a row, as `j` and `k` do, because the list scrolls only with its selection; over LOGS it scrolls three lines. Only button reports are requested (`?1000h`, `?1006h`), never motion (`?1003h`): a report for every move is likely to arrive split across two reads, and crossterm types the pieces as Esc and characters. Capture is on so the wheel works and is paid for with the terminal's own selection, which Shift brings back, said on the keymap page. It is released on exit, on a panic and for the editor | `src/tui/app.rs` (`handle_mouse`), `src/tui/mod.rs` (`enter_terminal`, `leave_terminal`) |
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
