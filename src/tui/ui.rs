use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Sparkline, Wrap,
};
use ratatui::Frame;

use super::app::{
    App, ConfigFile, ConfigView, HelpExtent, InputMode, TunnelEntry, TunnelKind, TunnelMode,
};
use super::theme;
use crate::probe::{HealthStatus, LB_WINDOW_MINUTES};
use crate::state::{TunnelSpec, TunnelStatus};
use crate::tunnel::diagnose_log_line;

pub const LOG_LINES: usize = 200;

/// A blank row, the lockup and the name beneath it, and the rule that closes
/// the header off from the panes: the rule is what keeps the header from
/// reading as another pane, so no blank row is spent between it and the name.
/// One size at every width: the design draws the same eleven cells on a
/// 1200-pixel board as on a narrow one, and a mark that grows with the
/// terminal is a second mark, not a responsive one.
pub const HEADER_HEIGHT_FULL: u16 = 4;
/// One row, for a terminal too narrow to hold the lockup without breaking it.
pub const HEADER_HEIGHT_COMPACT: u16 = 1;

/// One cell of air at each end of every full-width row, so the header, the
/// status line and the footer share the margin the panes between them have.
const MARGIN: usize = 1;

/// The lockup is eleven cells, not ten. `gtunnel` is seven, and 11 - 7 = 4
/// leaves two blank cells on each side; a ten-cell mark leaves three, and no
/// terminal can split a cell. The mark is mirror-symmetric about its centre
/// column, so the centring reads as deliberate rather than as a near miss.
pub const MARK_WIDTH: u16 = 11;

/// How many rows the mark occupies. One, and it does not change: see
/// `MARK_BRAILLE` for why the mark cannot be drawn at any other size.
pub const MARK_ROWS: usize = 1;

/// The name, padded to `MARK_WIDTH` so the two rows centre on one another.
pub const NAME_ROW: &str = "  gtunnel  ";

/// A cloud resting on a line. Braille because a braille cell carries 2x4
/// dots, which is the finest grid a terminal offers.
///
/// This art is fixed and must not be redrawn at another size. Scaling it is
/// arithmetically easy and visually fatal: a braille glyph draws its dots as
/// separated points, so doubling the raster doubles the gaps between them
/// too. The silhouette comes out identical on a dot grid and reads on screen
/// as a loose matrix of dots rather than a cloud -- and the word beneath it
/// cannot grow at all, because a terminal cannot scale type. One size is not
/// a limitation of the mark here; it is the mark.
const MARK_BRAILLE: [&str; MARK_ROWS] =
    ["\u{2824}\u{2824}\u{28a4}\u{28f6}\u{28fe}\u{28ff}\u{28f7}\u{28f6}\u{2864}\u{2824}\u{2824}"];

/// The same shape for a font with no braille coverage. Eleven cells again,
/// so a terminal that falls back here keeps the lockup's geometry.
///
/// Quadrant blocks give two rows where braille gives four, so each cell is
/// the honest reduction of its braille cell: a full block where the dots
/// reach the upper rows too, a half block where they reach only the lower
/// two, and `─` for a cell carrying nothing but the line, which a half block
/// would thicken into a slab. No cell may be an upper-half block: the shape
/// has none whose lower rows are empty, and one opens a hole under the cloud
/// instead of lifting it.
const MARK_FALLBACK: [&str; MARK_ROWS] =
    ["\u{2500}\u{2500}\u{2584}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2584}\u{2500}\u{2500}"];

/// Which hue each cell of the small mark carries. The line runs off both ends
/// in the same colour, and the four brand hues band the cloud left to right.
const MARK_BANDS: [Color; MARK_WIDTH as usize] = [
    theme::LOGO_LINE,
    theme::LOGO_LINE,
    theme::LOGO_BLUE,
    theme::LOGO_RED,
    theme::LOGO_RED,
    theme::LOGO_YELLOW,
    theme::LOGO_YELLOW,
    theme::LOGO_GREEN,
    theme::LOGO_GREEN,
    theme::LOGO_LINE,
    theme::LOGO_LINE,
];

/// The shortest a project id may be cut to, ellipsis included. Below it the
/// id says nothing a reader could match against a console, so it is dropped
/// whole instead of left as a stub.
const PROJECT_FLOOR: usize = 4;

/// Colour one row of art by cell index, merging consecutive cells that share a
/// hue so a row costs a handful of spans rather than one per cell.
fn band_spans(art: &str, bands: &[Color]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut hue: Option<Color> = None;
    for (cell, ch) in art.chars().enumerate() {
        let next = bands[cell];
        if hue != Some(next) {
            if let Some(previous) = hue {
                spans.push(Span::styled(
                    std::mem::take(&mut run),
                    Style::default().fg(previous),
                ));
            }
            hue = Some(next);
        }
        run.push(ch);
    }
    if let Some(previous) = hue {
        spans.push(Span::styled(run, Style::default().fg(previous)));
    }
    spans
}

/// The small mark as one row of coloured spans.
pub fn mark_spans(braille: bool, row: usize) -> Vec<Span<'static>> {
    let art = if braille { MARK_BRAILLE } else { MARK_FALLBACK };
    band_spans(art[row], &MARK_BANDS)
}

/// Every lit dot of the mark as `(x, y, colour)`, on the grid a row of braille
/// cells describes: two dots wide per cell, four tall, so `2 * MARK_WIDTH` by
/// four. Left to right, then top to bottom.
///
/// This is here so the README's banner can be the same drawing as the
/// terminal's rather than a second one. The art cannot be enlarged *as
/// braille* -- see `MARK_BRAILLE` for why -- but the dots behind it can be
/// re-plotted at any size, and plotting them from here is what keeps the two
/// in step. `the_readme_banner_is_the_mark_dot_for_dot` holds `docs/logo.svg`
/// to exactly what this returns. The four rows are a property of a braille
/// cell, not of the art; `MARK_ROWS` is pinned at one.
///
/// Test-only, because that test is its only caller: the TUI draws the glyphs
/// themselves and never needs the dots behind them. Keeping it out of the
/// release build is cheaper than an `allow(dead_code)`, which would silence a
/// lint the crate turns on deliberately.
#[cfg(test)]
pub fn mark_dots() -> Vec<(u16, u16, Color)> {
    // A braille cell numbers its dots down the left column and then down the
    // right, with the two fourth-row dots appended last: bit 0 is (0,0) and
    // bit 2 is (0,2), bit 3 is (1,0) and bit 5 is (1,2), then bit 6 is (0,3)
    // and bit 7 is (1,3).
    const PLACES: [(u16, u16); 8] = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    let mut dots = Vec::new();
    for y in 0..4 {
        for (cell, ch) in MARK_BRAILLE[0].chars().enumerate() {
            let bits = ch as u32 - 0x2800;
            for (bit, (dx, dy)) in PLACES.iter().enumerate() {
                if *dy == y && bits & (1 << bit) != 0 {
                    dots.push((2 * cell as u16 + dx, y, MARK_BANDS[cell]));
                }
            }
        }
    }
    dots
}

/// The rule that closes the header off from the panes, inset by the margin so
/// it starts and ends where the panes below it do.
fn rule_line(w: usize) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(MARGIN)),
        Span::styled(
            "\u{2500}".repeat(w.saturating_sub(MARGIN * 2)),
            Style::default().fg(theme::MUTED),
        ),
        Span::raw(" ".repeat(MARGIN.min(w))),
    ])
}

/// The divider between the lockup and the subtitle, with its two cells of air
/// each side. It runs the lockup's full height, so it is drawn on both rows.
fn divider_span() -> Span<'static> {
    Span::styled("  \u{2502}  ", Style::default().fg(theme::MUTED))
}

/// How many cells the divider and its air occupy.
const DIVIDER_WIDTH: usize = 5;

/// The two caption lines, one per lockup row: the platform beside the mark and
/// the subtitle beside the name. Two lines on a two-row lockup is the one
/// pairing a cell grid can centre exactly.
fn caption_spans(w: usize, mark_cells: usize, field: usize) -> Option<[Span<'static>; 2]> {
    const PLATFORM: &str = "Google Cloud Platform";
    const SUBTITLE: &str = "IAP tunnel manager";
    let used = MARGIN + mark_cells + field + MARGIN;

    // The captions appear only when the longer one fits whole, behind the
    // divider that separates them from the mark. A half-drawn caption is worse
    // than none.
    let longest = PLATFORM.chars().count().max(SUBTITLE.chars().count());
    (w >= used + DIVIDER_WIDTH + longest).then(|| {
        let muted = Style::default().fg(theme::MUTED);
        [Span::styled(PLATFORM, muted), Span::styled(SUBTITLE, muted)]
    })
}

/// The project id as it will be drawn: whole when it fits, cut with an
/// ellipsis when it does not, and gone when even the floor will not fit.
///
/// The id is the part that gives way because it is the longer and the less
/// urgent of the two lines: the account name is the state that changes what
/// every key acts on, so it is never cut while a field can still be drawn.
fn fit_project(project: &str, max_width: usize) -> String {
    let room = max_width.saturating_sub(2);
    let len = project.chars().count();
    if len <= room {
        return project.to_string();
    }
    if room < PROJECT_FLOOR {
        return String::new();
    }
    let kept: String = project.chars().take(room - 1).collect();
    format!("{kept}\u{2026}")
}

/// How many cells the field occupies: the longer line and a cell of padding on
/// each side, which is what makes it read as a field and not as more text.
fn account_field_width(account: &str, project: &str, max_width: usize) -> usize {
    let shown = fit_project(project, max_width);
    account.chars().count().max(shown.chars().count()) + 2
}

/// One row of the field: the text, left-aligned, on the fill out to `width`.
fn field_row(text: &str, width: usize, style: Style) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("{:<width$}", format!(" {text}")),
        style,
    )]
}

/// The two rows of the account field: the name, and the project id under it.
///
/// A filled block is enough to mark the field, so it carries no border and no
/// `ACCOUNT` label: a border needs a row above and below it, which would grow
/// the header to five rows, and the name is the only thing it could label. The
/// fill is `FIELD_BG`, not `SELECTED_BG`: the selected list row is a different
/// role and must not read as the same control. It is still an indexed colour,
/// so the terminal's own palette decides the shade. With no project the second
/// row is empty and the field is one row tall, rather than a blank line on the
/// fill.
fn account_field(account: &str, project: &str, max_width: usize) -> [Vec<Span<'static>>; 2] {
    let width = account_field_width(account, project, max_width);
    let shown = fit_project(project, max_width);
    let fill = Style::default().bg(theme::FIELD_BG);
    let name = field_row(
        account,
        width,
        fill.fg(Color::White).add_modifier(Modifier::BOLD),
    );
    let second = if shown.is_empty() {
        Vec::new()
    } else {
        field_row(&shown, width, fill.fg(theme::FIELD_SUBTITLE))
    };
    [name, second]
}

/// How many cells a row of spans occupies.
fn row_cells(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// The name under the mark, and the divider carried down beside it so the
/// separator runs the lockup's whole height rather than half of it.
fn name_spans(row: &'static str, caption: Option<Span<'static>>) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::raw(" ".repeat(MARGIN)),
        Span::styled(
            row,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(caption) = caption {
        spans.push(divider_span());
        spans.push(caption);
    }
    spans
}

/// The lockup, the subtitle behind its divider, the account field pinned
/// right, and the rule that closes the header off.
///
/// One size. What gives way as the terminal narrows is the subtitle, then the
/// project id, then the lockup itself: the account name has priority over all
/// of them, because it is the one piece of state that changes what every other
/// key acts on. When even the name will not fit beside the mark, the header
/// falls to a single line.
pub fn header_lines(width: u16, _height: u16, account: &str, project: &str) -> Vec<Line<'static>> {
    let w = width as usize;

    // What the field may occupy once the mark, the cell between them and the
    // two margins are paid for. The name alone is the narrowest field there is.
    let room = w.saturating_sub(MARGIN + MARK_WIDTH as usize + 1 + MARGIN);
    let narrowest = account.chars().count() + 2;

    if room < narrowest {
        let name: String = "gtunnel".chars().take(w).collect();
        let mut spans = vec![Span::styled(
            name.clone(),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )];
        // Two cells separate the name from the account; below that there is
        // room for the name alone, and the account waits for a wider terminal.
        let room = w.saturating_sub(name.chars().count());
        if room > 2 {
            let shown: String = account.chars().take(room - 2).collect();
            spans.push(Span::styled(
                format!("  {shown}"),
                Style::default().fg(theme::MUTED),
            ));
        }
        return vec![Line::from(spans)];
    }

    let field = account_field_width(account, project, room);

    // The divider belongs to the captions: with no room for them there is
    // nothing for it to separate, and it would point at empty space.
    let (platform, subtitle) = match caption_spans(w, MARK_WIDTH as usize, field) {
        Some([platform, subtitle]) => (Some(platform), Some(subtitle)),
        None => (None, None),
    };

    let mut top = vec![Span::raw(" ".repeat(MARGIN))];
    top.extend(mark_spans(true, 0));
    if let Some(platform) = platform {
        top.push(divider_span());
        top.push(platform);
    }

    // The field is two rows tall, the lockup is two, and they share rows 1 and
    // 2. Row 0 stays blank so the mark has air above it, and the rule sits
    // directly under the name on row 3.
    let [name_row, project_row] = account_field(account, project, room);
    let mut rows = [top, name_spans(NAME_ROW, subtitle)];
    for (row, segment) in rows.iter_mut().zip([name_row, project_row]) {
        if segment.is_empty() {
            continue;
        }
        let pad = w.saturating_sub(row_cells(row) + field + MARGIN);
        row.push(Span::raw(" ".repeat(pad)));
        row.extend(segment);
        row.push(Span::raw(" ".repeat(MARGIN)));
    }
    let [r1, r2] = rows;

    vec![
        Line::from(Vec::new()),
        Line::from(r1),
        Line::from(r2),
        rule_line(w),
    ]
}

/// How many rows the header takes here. Derived from `header_lines` rather than
/// recomputed, because a layout that reserves a different number of rows than
/// the header draws is a bug with no symptom until it clips.
pub fn header_height(width: u16, height: u16, account: &str, project: &str) -> u16 {
    match header_lines(width, height, account, project).len() {
        1 => HEADER_HEIGHT_COMPACT,
        _ => HEADER_HEIGHT_FULL,
    }
}

/// Where a key is listed on the help page. The four headings are the design's,
/// so a key's group says what it acts on rather than how it is typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Tunnel,
    List,
    UrlAndLog,
    Program,
}

impl Group {
    /// The order the help page stacks the groups in. Spelled out beside the enum
    /// and not derived from it, because the page's reading order is a design
    /// choice and a new variant should have to be placed on purpose.
    #[allow(dead_code)]
    pub const ALL: [Group; 4] = [Group::Tunnel, Group::List, Group::UrlAndLog, Group::Program];

    /// The heading as the design sets it: upper case and phrased as what the
    /// group acts on, so the page reads as a list of objects and not of verbs.
    pub fn heading(self) -> &'static str {
        match self {
            Group::Tunnel => "TUNNEL",
            Group::List => "THE LIST",
            Group::UrlAndLog => "THIS TUNNEL'S URL AND LOG",
            Group::Program => "THE PROGRAM",
        }
    }
}

/// One bound key. `label` is the sentence the help page prints; `footer` is the
/// word the footer prints beside it, or `None` for a key the footer never has
/// room for. They differ because a footer has to fit eight actions on a line and
/// a page does not, but both live here so neither can name a key the other has
/// never heard of.
pub struct KeyDoc {
    pub key: &'static str,
    pub label: &'static str,
    pub footer: Option<&'static str>,
    pub group: Group,
}

const fn doc(
    key: &'static str,
    footer: Option<&'static str>,
    label: &'static str,
    group: Group,
) -> KeyDoc {
    KeyDoc {
        key,
        label,
        footer,
        group,
    }
}

/// Every key `handle_key` binds, in the order the help page prints them. A test
/// presses each one, so a row here that nothing handles fails the build rather
/// than shipping on the help page.
pub const KEYMAP: &[KeyDoc] = &[
    doc(
        "s",
        Some("start"),
        "start the selected tunnel",
        Group::Tunnel,
    ),
    doc("S", Some("stop"), "stop it", Group::Tunnel),
    doc(
        "R",
        Some("restart"),
        "restart it (rewrites the service)",
        Group::Tunnel,
    ),
    doc(
        "A",
        Some("auto"),
        "toggle auto-start at login",
        Group::Tunnel,
    ),
    doc("h", None, "force a health check now", Group::Tunnel),
    doc("a", Some("add"), "add a tunnel", Group::List),
    doc("e", Some("edit"), "edit the selected one", Group::List),
    doc("d", Some("delete"), "delete it", Group::List),
    doc(
        "m",
        Some("import"),
        "import an ephemeral run as managed",
        Group::List,
    ),
    // On the help page the same four keys scroll it, since it has no selection
    doc(
        "up/k",
        None,
        "move the selection up, or scroll",
        Group::List,
    ),
    doc(
        "down/j",
        None,
        "move the selection down, or scroll",
        Group::List,
    ),
    doc("c", Some("copy"), "copy the tunnel URL", Group::UrlAndLog),
    doc("o", Some("open"), "open it in a browser", Group::UrlAndLog),
    doc(
        "L",
        Some("clear log"),
        "clear this tunnel's log",
        Group::UrlAndLog,
    ),
    doc(
        "r",
        None,
        "refresh the list, metrics and health",
        Group::UrlAndLog,
    ),
    doc(";", Some("account"), "cycle accounts", Group::Program),
    // `:` is the shift of `;`, so the key that cycles accounts and the key
    // that opens all of them are the same key twice. Its footer word is not
    // "accounts": beside `; account` that is a one-letter difference, which
    // is the trap the contextual footer exists to dissolve.
    doc(
        ":",
        Some("manage"),
        "open the accounts page",
        Group::Program,
    ),
    doc("?", Some("keys"), "this page", Group::Program),
    doc("q", Some("quit"), "quit", Group::Program),
    doc(
        "Esc",
        None,
        "close this, or cancel a prompt",
        Group::Program,
    ),
    // Bound on the page only: the list has no use for them, and the page needs
    // them because it is the one screen that can be taller than the terminal
    doc(
        "PgUp/Dn",
        None,
        "scroll this page a screen at a time",
        Group::Program,
    ),
    doc(
        "g/G",
        None,
        "jump to the top or end of this page",
        Group::Program,
    ),
    // The config page's own keys. `e` is a second row on purpose: the first one
    // edits the selected tunnel, and the footer finds its action by the first
    // row with the key, so that one must stay ahead of this
    doc("C", None, "open the config page", Group::Program),
    doc("Tab", None, "switch file, on that page", Group::Program),
    doc("e", None, "edit the file, on that page", Group::Program),
    doc("Ctrl+Z", None, "suspend to the shell", Group::Program),
    // The one key that quits from inside a prompt, where `q` is just a letter
    doc("Ctrl+C", None, "quit from anywhere", Group::Program),
];

/// One key and the word beside it in the footer.
pub struct Action {
    pub key: &'static str,
    pub label: &'static str,
    /// The one action the state is asking for. It is drawn in the warning
    /// colour so the next move is the loudest thing on the line.
    pub lead: bool,
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

impl Action {
    /// Looked up in `KEYMAP` at compile time, so a footer that names a key the
    /// table does not have, or one with no footer word, does not build.
    const fn of(key: &'static str) -> Self {
        let mut i = 0;
        while i < KEYMAP.len() {
            if same(KEYMAP[i].key, key) {
                return match KEYMAP[i].footer {
                    Some(label) => Action {
                        key: KEYMAP[i].key,
                        label,
                        lead: false,
                    },
                    None => panic!("a footer action needs a footer label in KEYMAP"),
                };
            }
            i += 1;
        }
        panic!("a footer action must be a KEYMAP key")
    }

    const fn lead(self) -> Self {
        Action { lead: true, ..self }
    }
}

// The old footer printed all fourteen keys at once, which needed 124 cells and
// so lost its tail on a 120-column terminal. Worse, it put `s` beside `S`, `a`
// beside `A` and `r` beside `R`: three pairs where case is the whole
// difference. Showing only what the selection can actually do dissolves most
// of that trap: `s`/`S` and `r`/`R` are never on screen together, because a
// tunnel cannot be both running and stopped. `a`/`A` is the exception, since
// `a add` is a program action and `A auto` a contextual one.
const ACTIONS_RUNNING: &[Action] = &[
    Action::of("S"),
    Action::of("R"),
    Action::of("c"),
    Action::of("o"),
    Action::of("L"),
];

const ACTIONS_STOPPED: &[Action] = &[
    Action::of("s"),
    Action::of("e"),
    Action::of("d"),
    Action::of("A"),
];

const ACTIONS_ERROR: &[Action] = &[
    Action::of("R").lead(),
    Action::of("e"),
    Action::of("d"),
    Action::of("L"),
];

const ACTIONS_EMPTY: &[Action] = &[Action::of("m")];

/// Someone else's `gcloud` process. Every tunnel action refuses on it, and the
/// running set offered five of them, so the footer named five keys that did
/// nothing. Nothing is left to offer here: `a` add now lives in the program
/// group, and `r` refresh has no footer word, being a key the screen never
/// advertises.
const ACTIONS_UNMANAGED: &[Action] = &[];

/// Always available, always flush right, and the last thing to shed. The
/// group sheds from the front, so its order is the order it is given up in:
/// `: manage` first, because a terminal too narrow for everything is not one
/// where someone is reorganising their accounts; then `a add`, which is true
/// of every row but is not what the row in front of them is; then
/// `; account`, with `?` and `q` left. `a add` is here rather than in a
/// contextual set because adding a tunnel is as valid on a running row as on
/// an empty list.
const ACTIONS_PROGRAM: &[Action] = &[
    Action::of(":"),
    Action::of("a"),
    Action::of(";"),
    Action::of("?"),
    Action::of("q"),
];

/// The program actions that outlast the row's own first action: `; account`,
/// `? keys` and `q quit`. Everything in front of them goes before it.
const PROGRAM_CORE: usize = 3;
/// And the two that never go at all.
const PROGRAM_FLOOR: usize = 2;

/// The most actions the footer ever shows at once, for the help page's count
/// line. Computed so a new action cannot leave the sentence wrong.
pub fn footer_most() -> usize {
    [
        ACTIONS_RUNNING,
        ACTIONS_STOPPED,
        ACTIONS_ERROR,
        ACTIONS_EMPTY,
        ACTIONS_UNMANAGED,
    ]
    .iter()
    .map(|set| set.len())
    .max()
    .unwrap_or(0)
        + ACTIONS_PROGRAM.len()
}

/// Draw a pane and return the rectangle its contents go in.
///
/// The name sits inside the box rather than in a gap cut out of its top edge.
/// A title on the border makes the frame carry two jobs at once — it is both
/// the pane's edge and its label — and the eye has to pick the words out of
/// the rule they interrupt. Set one row in, the name is just text, the border
/// is just a border, and upper case does the work that breaking the line used
/// to: it says "this is a heading" without competing with the values below it.
fn pane(f: &mut Frame, area: Rect, title: &str, border: Option<Style>) -> Rect {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    if let Some(style) = border {
        block = block.border_style(style);
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    // One cell of air inside the border, so the contents do not touch it
    let padded = Rect {
        x: inner.x.saturating_add(1),
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if padded.height == 0 || padded.width == 0 {
        return padded;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(padded);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title.to_uppercase(),
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    rows[1]
}

fn status_style(status: TunnelStatus) -> Style {
    match status {
        TunnelStatus::Running => Style::default().fg(theme::OK),
        TunnelStatus::Stopped => Style::default().fg(theme::MUTED),
        TunnelStatus::Error => Style::default().fg(theme::BAD),
    }
}

/// The terminal width below which the list stacks above the other panes. Side by
/// side the list is 46 percent of the width: 46 cells at 100 columns, 50 at 110,
/// 55 at 120. Measured at 24 rows, a 28-cell url was cut at 100 and 110 and a
/// 49-cell url at all three, so the list lost the url it exists to show across
/// the whole range. Stacked, it takes the full width and none of those is cut;
/// from 121 the right-hand column is wide enough for Details to carry in full
/// whatever the list still has to shorten. Stacking costs rows, not columns,
/// and a list loses a row more cheaply than a url.
const STACK_BELOW: u16 = 121;
/// The fewest rows a stacked list gets: its border, its title, its header and
/// two entries.
const LIST_MIN_ROWS: u16 = 5;

pub fn render(f: &mut Frame, app: &App) {
    // A terminal short enough that the header would cost the list its last row
    // keeps the list: the header says what the app is, the list is the app
    let header_height = if f.area().height >= 16 {
        header_height(f.area().width, f.area().height, &app.account, app.project())
    } else {
        0
    };
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    // The design closes the panes off from the footer the same way it closes
    // the header off from them, so the two rules bracket the work area.
    f.render_widget(Paragraph::new(rule_line(f.area().width as usize)), main[2]);

    if header_height > 0 {
        f.render_widget(
            Paragraph::new(header_lines(
                f.area().width,
                f.area().height,
                &app.account,
                app.project(),
            )),
            main[0],
        );
    }

    // Side by side, the list at 80 columns is 32 cells wide and the url is the
    // first thing it loses. Below `STACK_BELOW` the list takes the full width
    // above the other panes instead, trading rows for the columns it needs.
    let (list_area, right_area) = if f.area().width < STACK_BELOW {
        let total = main[1].height;
        let list_height = (total * 40 / 100).max(LIST_MIN_ROWS).min(total);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(list_height), Constraint::Min(0)])
            .split(main[1]);
        (rows[0], rows[1])
    } else {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
            .split(main[1]);
        (cols[0], cols[1])
    };

    render_tunnels(f, app, list_area);

    render_right(f, app, right_area);

    render_help_bar(f, main[3], app);

    match app.input_mode {
        InputMode::Normal => {}
        InputMode::Help => render_help_modal(f, app),
        InputMode::Config => render_config_modal(f, app),
        InputMode::Accounts => render_accounts(f, app),
        InputMode::ChooseMode => render_choose_mode(f),
        InputMode::AccountConfirm | InputMode::Confirm => {
            if let Some(message) = &app.confirm_message {
                render_confirm(f, message);
            }
        }
        InputMode::ChooseEdit => render_choose_edit(f),
        InputMode::AddInstance => render_instance_picker(f, app),
        InputMode::AddDomain => render_domain_picker(f, app),
        InputMode::MoveTo => render_account_picker(f, app),
        mode => render_prompt(f, app, mode),
    }
}

// Cells the list spends around the two columns that flex: the status symbol and
// its air, the air after the name, the mode and its air, the air after the url.
// Every mode is six characters.
const LEAD_CELLS: usize = 2;
const MODE_CELLS: usize = 6;
// One cell for the auto-start flag, one for the health warning. The flags used
// to follow the url, so they landed in a different column on every row; a fixed
// pair of cells is what makes the column a column.
const FLAG_CELLS: usize = 2;
const FIXED_CELLS: usize = LEAD_CELLS + 1 + MODE_CELLS + 1 + 1 + FLAG_CELLS;
/// Below this the name is cut to make room for the url, never further: a name
/// of eight cells still tells most rows apart, a url of none tells nothing.
const NAME_FLOOR: usize = 8;
/// The url the list defends: it is what the list exists to show, so the name
/// shrinks to leave it this much before the url starts to give way.
const URL_MIN: usize = 12;

/// How wide the two flexing columns are, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    pub name: usize,
    pub url: usize,
}

/// Splits a pane's width between the name and the url. Each gets what its
/// longest value needs while both fit; otherwise the name gives way first, down
/// to `NAME_FLOOR`, and only then the url. A cap on the name used to stand in
/// for this and cut names the pane had room for.
pub fn fit_columns(names: &[usize], urls: &[usize], width: usize) -> Columns {
    let name_want = names.iter().copied().max().unwrap_or(0).max("NAME".len());
    let url_want = urls.iter().copied().max().unwrap_or(0).max("URL".len());
    let room = width.saturating_sub(FIXED_CELLS);
    if name_want + url_want <= room {
        return Columns {
            name: name_want,
            url: url_want,
        };
    }
    let name = name_want
        .min(room.saturating_sub(URL_MIN.min(url_want)))
        .max(NAME_FLOOR.min(name_want))
        .min(room);
    Columns {
        name,
        url: url_want.min(room - name),
    }
}

/// Cuts `text` to `width` cells, marking the cut. A name or url that overflows
/// is clipped at the pane's edge otherwise, which reads as a shorter value.
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    match width {
        0 => String::new(),
        n => {
            let head: String = text.chars().take(n - 1).collect();
            format!("{head}\u{2026}")
        }
    }
}

fn render_tunnels(f: &mut Frame, app: &App, area: Rect) {
    let title = format!("Tunnels ({})", app.entries.len());
    let inner = pane(f, area, &title, None);
    if inner.height == 0 {
        return;
    }
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let names: Vec<usize> = app
        .entries
        .iter()
        .map(|e| e.display_name().chars().count())
        .collect();
    let urls: Vec<usize> = app
        .entries
        .iter()
        .map(|e| e.url_cell().chars().count())
        .collect();
    let cols = fit_columns(&names, &urls, inner.width as usize);

    let muted = Style::default().fg(theme::MUTED);
    let header = format!(
        "{:lead$}{:<name$} {:<mode$} {}",
        "",
        fit("NAME", cols.name),
        "MODE",
        fit("URL", cols.url),
        lead = LEAD_CELLS,
        name = cols.name,
        mode = MODE_CELLS,
    );
    f.render_widget(Paragraph::new(Span::styled(header, muted)), parts[0]);

    // The header alone over a blank pane reads as a draw that failed. The footer
    // already offers `a` and `m`, so the row only has to say the list is empty,
    // and only once a load has said so: before that, or after one that failed,
    // an empty list is just a list nothing has filled yet.
    if app.entries.is_empty() {
        let row = if app.loaded {
            "no tunnels in this account"
        } else if app.load_failed {
            "could not load tunnels"
        } else {
            "loading"
        };
        f.render_widget(Paragraph::new(Span::styled(row, muted)), parts[1]);
        return;
    }

    let items: Vec<ListItem> = app
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let mut spans = vec![
                Span::styled(
                    format!("{} ", entry.status.symbol()),
                    status_style(entry.status),
                ),
                Span::styled(
                    format!(
                        "{:<width$} ",
                        fit(&entry.display_name(), cols.name),
                        width = cols.name
                    ),
                    if i == app.selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!("{:<width$} ", entry.mode(), width = MODE_CELLS),
                    Style::default().fg(theme::ACCENT),
                ),
                Span::raw(format!(
                    "{:<width$} ",
                    fit(&entry.url_cell(), cols.url),
                    width = cols.url
                )),
                Span::styled(
                    if entry.tunnel.auto_start {
                        "\u{27f3}"
                    } else {
                        " "
                    },
                    Style::default().fg(theme::FLAG),
                ),
                Span::styled(
                    if entry.health == HealthStatus::Unhealthy {
                        "\u{26a0}"
                    } else {
                        " "
                    },
                    Style::default().fg(theme::WARN),
                ),
            ];
            // An unmanaged row already says so in the URL column
            if let Some(tag) = entry
                .tag()
                .filter(|_| entry.kind != crate::tui::TunnelKind::External)
            {
                spans.push(Span::styled(format!(" {tag}"), muted));
            }
            let style = if i == app.selected {
                Style::default().bg(theme::SELECTED_BG)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(spans).style(style))
        })
        .collect();

    // The list follows the selection through a state, so a selection past the
    // pane's last row scrolls into view. The offset is held on `App` and only
    // moved when the selection would leave the window, so a move inside it
    // moves the cursor and not the list. It is clamped to the last offset that
    // still fills the pane: a terminal that grew, or a list that shrank, would
    // otherwise leave blank rows under the last entry. Row styling stays on the
    // rows; the state is only asked to scroll.
    let height = parts[1].height as usize;
    let selected = app.selected.min(app.entries.len() - 1);
    let mut offset = app
        .list_offset
        .get()
        .min(app.entries.len().saturating_sub(height));
    if selected < offset {
        offset = selected;
    } else if height > 0 && selected >= offset + height {
        offset = selected + 1 - height;
    }
    app.list_offset.set(offset);
    let mut state = ListState::default()
        .with_offset(offset)
        .with_selected(Some(selected));
    f.render_stateful_widget(List::new(items), parts[1], &mut state);
}

/// Rows of border and title around the metrics figures, and the two the chart
/// wants under them.
const METRICS_CHROME: u16 = 3;
const SPARKLINE_ROWS: u16 = 2;
/// Fewest rows Logs keeps: its border, its title and one line. Fewer and the
/// pane that explains a failure is a frame with nothing inside it.
const LOGS_MIN: u16 = 4;
/// Fewest rows Metrics is drawn in: its frame, its title and one row of figures.
/// A pane of three would show only its own name, which is worse than the room.
const METRICS_MIN: u16 = 4;

/// Splits the right-hand column between Details, Logs and Metrics, each given
/// what it wants and the column's height as the limit.
///
/// The order they give way in is the order they matter in. Logs keeps
/// `LOGS_MIN` before anyone else is served, because a failing tunnel is
/// diagnosed there. Details is next and is cut only for that minimum, so a
/// value is never lost while another pane has rows to spare. Metrics gets the
/// remainder, and is dropped whole rather than drawn too short to show a figure.
/// Logs takes whatever is left, which is most of a tall terminal.
pub fn right_heights(avail: u16, details: u16, metrics: u16) -> (u16, u16, u16) {
    let logs_floor = LOGS_MIN.min(avail);
    let details = details.min(avail - logs_floor);
    let spare = avail - details - logs_floor;
    let metrics = if metrics.min(spare) >= METRICS_MIN {
        metrics.min(spare)
    } else {
        0
    };
    (details, avail - details - metrics, metrics)
}

/// Cells inside a pane: its border and the cell of air `pane` leaves each side.
fn pane_text_width(area_width: u16) -> usize {
    (area_width as usize).saturating_sub(4)
}

fn render_right(f: &mut Frame, app: &App, area: Rect) {
    let Some(entry) = app.selected_entry() else {
        render_logs(f, app, area);
        return;
    };
    let width = pane_text_width(area.width);
    let details = detail_lines(entry, app.project(), width).len() as u16 + DETAILS_CHROME;
    let metrics =
        METRICS_CHROME + wrapped_rows(&metrics_lines(entry, width), width as u16) + SPARKLINE_ROWS;
    let (details, logs, metrics) = right_heights(area.height, details, metrics);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(details),
            Constraint::Length(logs),
            Constraint::Length(metrics),
        ])
        .split(area);
    if details > 0 {
        render_details(f, app, right[0]);
    }
    render_logs(f, app, right[1]);
    if metrics > 0 {
        render_metrics(f, app, right[2]);
    }
}

/// The rows Details shows for an entry at `width` cells, long values wrapped. The
/// pane's height is derived from this count: a fixed height was sized for an Access tunnel and clipped `started by`
/// off every public one, and a `name` row would have clipped one more.
fn detail_lines(entry: &TunnelEntry, project: &str, width: usize) -> Vec<Line<'static>> {
    let mut rows: Vec<(&'static str, String)> = vec![
        // The list cuts long names and drops the account suffix; this row is
        // where the stored name, the one every command takes, can be read
        ("name", entry.name().to_string()),
        // The list already reads an unknown port as "-"; the details pane said
        // `localhost:0`, which reads as a port the tunnel actually holds
        ("url", entry.url()),
        ("target", entry.tunnel.display_target()),
    ];
    // The target names a host inside a project that nothing else on this pane
    // names: the header carries it as a property of the account, four panes
    // away from the host it qualifies and gone the moment the pane is the one
    // being read. A config with no project for the account says nothing here,
    // rather than printing a label over a blank.
    if !project.is_empty() {
        rows.push(("project", project.to_string()));
    }
    match &entry.tunnel.spec {
        // An Access tunnel adds nothing here. Its zone is already inside the
        // target line and its local port is already inside the url, so a row
        // for each repeated what the two rows above had just said — and the
        // rows that did carry new information, `started` and `started by`,
        // were the ones pushed off the bottom of the pane.
        TunnelSpec::Access { .. } => {}
        TunnelSpec::Expose {
            domain,
            relay_port,
            private,
            ..
        } => {
            rows.push((
                "relay",
                format!("{domain}{}", if *private { " (private)" } else { "" }),
            ));
            rows.push(("relay port", relay_port.to_string()));
        }
    }

    // Status says a tunnel is running; these two say since when, and whether
    // anything brought it up without being asked
    let metrics = entry.metrics.clone().unwrap_or_default();
    rows.push((
        "started",
        match metrics.uptime {
            Some(up) => format!("{} ago", format_uptime(up)),
            None => "-".to_string(),
        },
    ));
    rows.push((
        "started by",
        match (entry.kind, entry.status) {
            // `started_by` reads a restart count off the service manager, and
            // a stranger has no service: the zero it carries means nothing is
            // counting, not that nothing revived it. The pane said "gtunnel"
            // over a process gtunnel did not start and cannot stop.
            (TunnelKind::External, _) => "another process".to_string(),
            (_, TunnelStatus::Running) => started_by(metrics.restarts, entry.tunnel.auto_start),
            _ => "-".to_string(),
        },
    ));

    rows.iter()
        .flat_map(|(label, value)| wrap_detail(label, value, width))
        .collect()
}

/// Rows of border and title around the detail rows
const DETAILS_CHROME: u16 = 3;

fn render_details(f: &mut Frame, app: &App, area: Rect) {
    let Some(entry) = app.selected_entry() else {
        pane(f, area, "Details", None);
        return;
    };
    let inner = pane(f, area, "Details", None);
    f.render_widget(
        Paragraph::new(detail_lines(entry, app.project(), inner.width as usize)),
        inner,
    );
}

/// Who started the process that is running now. The service manager counts
/// runs, and the run gtunnel asked for is the first one: anything past it is
/// the manager reviving a tunnel that died, which the screen never showed.
pub fn started_by(restarts: u32, auto_start: bool) -> String {
    let flag = if auto_start {
        "auto-start on"
    } else {
        "auto-start off"
    };
    if restarts == 0 {
        format!("gtunnel ({flag})")
    } else {
        let manager = if cfg!(target_os = "macos") {
            "launchd"
        } else {
            "systemd"
        };
        format!("{manager}, after {restarts} restarts ({flag})")
    }
}

/// Cells the label column takes, so the value starts at the same cell on every
/// row and a continuation row can be indented to it.
const LABEL_CELLS: usize = 11;
/// A value column narrower than this is not worth wrapping into: it would break
/// every word. The row is cut with `…` instead, which at least says it is cut.
const VALUE_MIN: usize = 4;

fn detail(label: &'static str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<width$}", width = LABEL_CELLS),
            Style::default().fg(theme::MUTED),
        ),
        Span::raw(value.to_string()),
    ])
}

/// Greedy word wrap: a word moves to the next row rather than being split, and
/// is split only when it is longer than a row by itself, as a url usually is.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    for word in text.split_whitespace() {
        let cells = word.chars().count();
        if used > 0 && used + 1 + cells <= width {
            row.push(' ');
            row.push_str(word);
            used += 1 + cells;
            continue;
        }
        if used > 0 {
            rows.push(std::mem::take(&mut row));
        }
        let mut rest: Vec<char> = word.chars().collect();
        while rest.len() > width {
            rows.push(rest.drain(..width).collect());
        }
        used = rest.len();
        row = rest.into_iter().collect();
    }
    rows.push(row);
    rows
}

/// One labelled value as the rows it takes at `width` cells. Clipping hid the
/// end of a url, which is the part that tells two of them apart, so the value
/// wraps under itself instead, with the label column left blank beside it.
pub fn wrap_detail(label: &'static str, value: &str, width: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(LABEL_CELLS);
    if room < VALUE_MIN {
        return vec![detail(label, &fit(value, room))];
    }
    wrap_words(value, room)
        .iter()
        .enumerate()
        .map(|(i, row)| detail(if i == 0 { label } else { "" }, row))
        .collect()
}

/// What an empty pane means, which is three different things. A tunnel through
/// a jump host runs `ssh -N`: it prints nothing at all while it is working, so
/// for it an empty log is the healthy case. A tunnel in `Error` state with
/// nothing in its log is the opposite, and must not be told it is fine.
pub fn empty_log_note(spec: &TunnelSpec, status: TunnelStatus) -> &'static str {
    let jumps = matches!(
        spec,
        TunnelSpec::Access {
            target: crate::state::AccessTarget::Jump { .. },
            ..
        }
    );
    match (status, jumps) {
        (TunnelStatus::Error, _) => "The tunnel failed and wrote nothing. Press R to restart it.",
        (_, true) => {
            "No output. A tunnel through a jump host is silent while it is healthy; \
             errors appear here."
        }
        (_, false) => "No output yet.",
    }
}

/// What the log title should say about what gtunnel is doing now.
///
/// A finished operation says nothing. `App::status` holds the last result for
/// the rest of the session, so showing it would leave the title claiming an
/// operation was under way long after it finished; the title falling back to
/// the plain pane name is how the user learns it is done. A failure is the
/// exception and stays, because an error that clears itself is one nobody saw.
pub fn activity_text(
    running: Option<&str>,
    status: Option<&str>,
    status_error: bool,
) -> Option<String> {
    match (running, status_error) {
        (Some(running), _) => Some(running.to_string()),
        (None, true) => status.map(str::to_string),
        (None, false) => None,
    }
}

/// The log pane's title, carrying whatever gtunnel is doing right now.
///
/// Activity reads as an answer here rather than as a notice: "starting" beside
/// `k8s-dev` says which tunnel is starting, which a line at the far bottom of
/// the screen never did.
pub fn logs_title(selected: Option<&str>, activity: Option<&str>) -> String {
    let mut title = match selected {
        Some(name) => format!("Logs: {name}"),
        None => "Logs".to_string(),
    };
    if let Some(activity) = activity {
        title.push_str(" \u{2014} ");
        title.push_str(activity);
    }
    title
}

fn render_logs(f: &mut Frame, app: &App, area: Rect) {
    let running = app
        .spinner
        .is_running()
        .then(|| match &app.spinner.message {
            Some(message) => format!("{} {message}", app.spinner.frame()),
            None => app.spinner.frame().to_string(),
        });
    let activity = activity_text(running.as_deref(), app.status.as_deref(), app.status_error);
    let entry = app.selected_entry();
    let title = logs_title(entry.as_ref().map(|e| e.name()), activity.as_deref());

    let mut lines: Vec<Line> = Vec::new();
    for line in app.logs.iter().rev().take(LOG_LINES).collect::<Vec<_>>() {
        lines.push(Line::raw(line.clone()));
    }
    lines.reverse();
    // A hint belongs under the line that caused it, where it reads as an answer
    let mut annotated: Vec<Line> = Vec::new();
    for line in lines {
        let text = line.to_string();
        annotated.push(line);
        if let Some(hint) = diagnose_log_line(&text) {
            annotated.push(Line::styled(
                format!("  {hint}"),
                Style::default().fg(theme::WARN),
            ));
        }
    }

    if annotated.is_empty()
        && let Some(entry) = app.selected_entry()
    {
        annotated.push(Line::styled(
            empty_log_note(&entry.tunnel.spec, entry.status),
            Style::default().fg(theme::MUTED),
        ));
    }

    // The pane shows the newest lines, so it scrolls past everything that wraps
    // above them; without this a Paragraph renders from its first line and the
    // line the user is waiting for is the one cut off the bottom
    let inner = pane(
        f,
        area,
        &title,
        app.status_error.then(|| Style::default().fg(theme::BAD)),
    );
    let rows = wrapped_rows(&annotated, inner.width);
    let scroll = rows.saturating_sub(inner.height);
    f.render_widget(
        Paragraph::new(annotated)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        inner,
    );
}

/// The newest `width` samples. A `Sparkline` draws one cell per sample and
/// starts at the left edge, so a buffer shorter than the pane left the rest of
/// it blank and the chart read as one that had stopped. The buffer is longer
/// than any terminal instead, and the pane takes the tail it has room for.
pub fn sparkline_slice(samples: &[u64], width: u16) -> &[u64] {
    let width = width as usize;
    &samples[samples.len().saturating_sub(width)..]
}

/// How many rows these lines occupy once wrapped to `width`. An empty line
/// still takes one, and a pane too narrow to hold anything takes one per line.
///
/// Counted the way the widget wraps, not by dividing characters by the width: a
/// word that does not fit moves whole to the next row, and the cells it leaves
/// behind are lost. Dividing undercounts every line of words, and the pane then
/// scrolls too little and cuts off the newest line it exists to show.
pub fn wrapped_rows(lines: &[Line], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    lines
        .iter()
        .map(|line| rows_for(&line.to_string(), width as usize))
        .sum()
}

fn rows_for(text: &str, width: usize) -> u16 {
    let mut rows = 1usize;
    // cells on the current row, and whitespace waiting to see if a word follows
    // it on the same row. Trailing whitespace never earns a row of its own
    let mut used = 0usize;
    let mut pending = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            pending += 1;
            continue;
        }
        let mut word = 1usize;
        while chars.next_if(|c| !c.is_whitespace()).is_some() {
            word += 1;
        }
        if used + pending + word <= width {
            used += pending + word;
        } else {
            // `trim: false` leaves whitespace that fits on the row where it is,
            // swallows one cell of what does not as the break, and carries the
            // rest to the head of the new row, where it counts toward the word
            let carried = if used > 0 {
                rows += 1;
                (pending - pending.min(width - used)).saturating_sub(1)
            } else {
                pending
            };
            let needs = carried + word;
            rows += (needs - 1) / width;
            used = (needs - 1) % width + 1;
        }
        pending = 0;
    }
    rows as u16
}

/// The widest the four-figure row gets: a latency and a peak of `999 ms`, two
/// digits of connections and an uptime of `99d 23h`. A pane narrower than this
/// splits the row in two, decided by the worst case and not by this tick's
/// digits, so the pane does not change height as a latency gains one.
const GAUGE_CELLS: usize = 58;

/// The rows Metrics prints for an entry at `width` cells, before wrapping.
fn metrics_lines(entry: &TunnelEntry, width: usize) -> Vec<Line<'static>> {
    let metrics = entry.metrics.clone().unwrap_or_default();

    // The four figures worth a glance, on one line. Stacked over three rows
    // they read as a form to be filled in; side by side they read as a gauge,
    // which is what they are. Restarts and health keep their own row because
    // both are answers to "is something wrong", not numbers to watch.
    let latency = vec![
        inline("latency"),
        value(match metrics.latency {
            Some(rtt) => format_latency(rtt),
            None => "-".to_string(),
        }),
        inline("   active"),
        value(metrics.active_connections.to_string()),
    ];
    let uptime = vec![
        inline("uptime"),
        value(match metrics.uptime {
            Some(up) => format_uptime(up),
            None => "-".to_string(),
        }),
        inline("   peak"),
        value(if entry.history.is_empty() {
            "-".to_string()
        } else {
            format_latency(std::time::Duration::from_micros(entry.history.peak()))
        }),
    ];
    let mut lines = if width >= GAUGE_CELLS {
        let mut row = latency;
        row.push(Span::raw("   "));
        row.extend(uptime);
        vec![Line::from(row)]
    } else {
        vec![Line::from(latency), Line::from(uptime)]
    };
    lines.push(Line::from(vec![
        inline("restarts"),
        value(metrics.restarts.to_string()),
        inline("   health"),
        value(entry.health.symbol().to_string()),
    ]));

    if entry.tunnel.is_expose() {
        let (total, classes) = match &metrics.lb {
            Some(lb) => (
                lb.total.to_string(),
                lb.by_class
                    .iter()
                    .map(|(class, count)| format!("{class} {count}"))
                    .collect::<Vec<_>>()
                    .join("  "),
            ),
            None => ("-".to_string(), String::new()),
        };
        let label = format!("Requests ({LB_WINDOW_MINUTES}m)");
        lines.push(Line::from(vec![field(&label), Span::raw(total)]));
        lines.push(Line::from(vec![Span::raw(format!("  {classes}"))]));
    }
    lines
}

fn render_metrics(f: &mut Frame, app: &App, area: Rect) {
    let inner = pane(f, area, "Metrics", None);

    let Some(entry) = app.selected_entry() else {
        return;
    };
    let lines = metrics_lines(entry, inner.width as usize);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(wrapped_rows(&lines, inner.width)),
            Constraint::Min(0),
        ])
        .split(inner);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[0]);

    if !entry.history.is_empty() {
        let samples = app.selected_sparkline();
        f.render_widget(
            Sparkline::default()
                .data(sparkline_slice(&samples, chunks[1].width))
                .style(Style::default().fg(theme::ACCENT)),
            chunks[1],
        );
    }
}

// Milliseconds, with a decimal while the figure is small enough that rounding
// to a whole one would report a real measurement as nothing
pub fn format_latency(rtt: std::time::Duration) -> String {
    let ms = rtt.as_secs_f64() * 1000.0;
    if ms < 10.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{} ms", ms.round() as u64)
    }
}

fn field(label: &str) -> Span<'static> {
    Span::styled(format!("{label}: "), Style::default().fg(theme::LABEL))
}

/// A label on a row that holds several figures, where a colon after each one
/// would be four colons in a line of four readings. The space does the same
/// work and leaves the numbers as the only punctuation on the row.
fn inline(label: &str) -> Span<'static> {
    Span::styled(format!("{label} "), Style::default().fg(theme::LABEL))
}

/// The figure beside an `inline` label. It is what the row exists for, so it
/// is the only bright thing on it.
fn value(text: String) -> Span<'static> {
    Span::styled(text, Style::default().add_modifier(Modifier::BOLD))
}

fn format_uptime(up: std::time::Duration) -> String {
    let secs = up.as_secs();
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// What the selected tunnel can do, and what the program can always do.
pub fn footer_groups(
    selected: Option<(TunnelStatus, TunnelKind)>,
    any_tunnels: bool,
) -> (&'static [Action], &'static [Action]) {
    let contextual = match (selected, any_tunnels) {
        // A stranger's process answers to whoever started it, whatever state
        // it is in, so its kind decides the footer before its status does
        (Some((_, TunnelKind::External)), _) => ACTIONS_UNMANAGED,
        (Some((TunnelStatus::Running, _)), _) => ACTIONS_RUNNING,
        (Some((TunnelStatus::Stopped, _)), _) => ACTIONS_STOPPED,
        (Some((TunnelStatus::Error, _)), _) => ACTIONS_ERROR,
        // No tunnels at all: the only useful actions are the ones that make one
        (None, false) => ACTIONS_EMPTY,
        // Tunnels exist but nothing is selected, so nothing is contextual
        (None, true) => &[],
    };
    (contextual, ACTIONS_PROGRAM)
}

/// How many cells a group occupies, including the two between each pair.
fn group_cells(actions: &[Action]) -> usize {
    if actions.is_empty() {
        return 0;
    }
    actions
        .iter()
        .map(|a| a.key.chars().count() + 1 + a.label.chars().count())
        .sum::<usize>()
        + 2 * (actions.len() - 1)
}

fn group_spans(actions: &[Action], out: &mut Vec<Span<'static>>) {
    for (i, action) in actions.iter().enumerate() {
        if i > 0 {
            out.push(Span::raw("  "));
        }
        // A lead action trades the accent for the warning colour and its label
        // for white, which is what makes it read as the thing to press
        let (key_colour, label_colour) = if action.lead {
            (theme::WARN, Color::White)
        } else {
            (theme::ACCENT, theme::LABEL)
        };
        out.push(Span::styled(
            action.key,
            Style::default().fg(key_colour).add_modifier(Modifier::BOLD),
        ));
        out.push(Span::raw(" "));
        out.push(Span::styled(
            action.label,
            Style::default().fg(label_colour),
        ));
    }
}

/// The footer at this width: the selection's actions on the left, the program's
/// on the right, and nothing drawn that does not fit whole.
///
/// Shedding order: the contextual group goes from its end, because the actions
/// are listed in the order a user reaches for them, but stops at its first
/// entry. Then `a add` goes, which is true of every row, then that first entry,
/// the one the row exists for. Only once the group is empty do `; account`
/// and then `: manage` go, leaving the two that must never disappear: `?`
/// says what the keys are, and `q` is the way out.
pub fn footer_line(
    width: u16,
    selected: Option<(TunnelStatus, TunnelKind)>,
    any_tunnels: bool,
) -> Line<'static> {
    let (all_contextual, all_program) = footer_groups(selected, any_tunnels);
    let w = width as usize;

    let fits = |ctx: &[Action], prog: &[Action]| -> bool {
        // one cell of air at each end, and the gap only exists between groups
        let gap = if ctx.is_empty() { 0 } else { 2 };
        let needed = 2 + group_cells(ctx) + gap + group_cells(prog);
        needed <= w
    };

    let mut shown = all_contextual.len();
    let mut skipped = 0;
    let mut program = all_program;
    while !fits(&all_contextual[..shown], program) {
        if shown > 1 {
            // everything after the row's first action goes before `: manage`
            shown -= 1;
        } else if all_program.len() - skipped > PROGRAM_CORE {
            skipped += 1;
        } else if shown > 0 {
            // the row's own first action, `R restart` on a failure, outlasts
            // the program's extras but not `; account`
            shown = 0;
        } else if all_program.len() - skipped > PROGRAM_FLOOR {
            skipped += 1;
        } else {
            break;
        }
        program = &all_program[skipped..];
    }
    let contextual = &all_contextual[..shown];

    let mut spans = vec![Span::raw(" ")];
    group_spans(contextual, &mut spans);
    let used = 1 + group_cells(contextual) + group_cells(program) + 1;
    spans.push(Span::raw(
        " ".repeat(
            w.saturating_sub(used)
                .max(if contextual.is_empty() { 0 } else { 2 }),
        ),
    ));
    group_spans(program, &mut spans);
    spans.push(Span::raw(" "));

    Line::from(spans)
}

fn render_help_bar(f: &mut Frame, area: Rect, app: &App) {
    let selected = app.selected_entry().map(|e| (e.status, e.kind));
    f.render_widget(
        Paragraph::new(footer_line(area.width, selected, !app.entries.is_empty())),
        area,
    );
}

/// Inside the modal, the width from which the page lays out in two columns. A
/// column is `(width - gap) / 2` and has to hold the key cell and the longest
/// label, `refresh the list, metrics and health` at 44 cells in all, so 88 plus
/// the gap is the least that clips nothing; a test holds the figure to it.
pub const HELP_TWO_COLUMNS: usize = 92;
/// The key column's width: `Ctrl+Z` and `down/j` are the widest keys.
const HELP_KEY_CELLS: usize = 8;
const HELP_GAP: usize = 4;

const CASE_NOTE: &str = "Case is load-bearing in three places: s/S start and stop, a/A add and auto-start, r/R refresh and restart. The footer never pairs s/S or r/R; only a/A, in two groups - this page is where it has to be stated.";

/// How many keys are bound, counting `up/k` as the two keys it is. Counted
/// from the table, because a figure typed into a sentence is wrong by the next
/// key.
fn keys_bound() -> usize {
    KEYMAP.iter().map(|d| d.key.split('/').count()).sum()
}

fn help_heading(group: Group) -> Line<'static> {
    Line::styled(
        group.heading(),
        Style::default()
            .fg(theme::MUTED)
            .add_modifier(Modifier::BOLD),
    )
}

fn help_row(doc: &KeyDoc) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{:<width$}", doc.key, width = HELP_KEY_CELLS),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(doc.label, Style::default().fg(theme::LABEL)),
    ])
}

/// One group as rows: its heading, then a row per key.
fn help_group(group: Group) -> Vec<Line<'static>> {
    let mut rows = vec![help_heading(group)];
    rows.extend(KEYMAP.iter().filter(|d| d.group == group).map(help_row));
    rows
}

/// Groups stacked with a blank row between them.
fn help_stack(groups: &[Group]) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            rows.push(Line::raw(""));
        }
        rows.extend(help_group(*group));
    }
    rows
}

fn help_note(width: usize) -> Vec<Line<'static>> {
    wrap_words(CASE_NOTE, width)
        .into_iter()
        .map(|row| Line::styled(row, Style::default().fg(theme::LABEL)))
        .collect()
}

fn shorten_path(path: &std::path::Path, max_width: usize) -> String {
    let s = if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = path.strip_prefix(&home) {
            format!("~/{}", rel.display())
        } else {
            path.display().to_string()
        }
    } else {
        path.display().to_string()
    };

    if s.chars().count() <= max_width {
        s
    } else {
        let chars: Vec<char> = s.chars().collect();
        let keep = max_width.saturating_sub(4);
        let start = chars.len().saturating_sub(keep);
        format!(".../{}", chars[start..].iter().collect::<String>())
    }
}

fn help_paths(width: usize) -> Vec<Line<'static>> {
    let mut rows = vec![Line::styled(
        "SESSION PATHS",
        Style::default()
            .fg(theme::MUTED)
            .add_modifier(Modifier::BOLD),
    )];
    let config_p = crate::config::config_path().ok();
    let tunnels_p = crate::state::tunnels_path().ok();
    let logs_p = crate::config::config_dir().ok().map(|d| d.join("logs"));

    let items = [
        ("Config", config_p),
        ("Tunnels", tunnels_p),
        ("Logs", logs_p),
    ];

    let max_val_width = width.saturating_sub(HELP_KEY_CELLS);
    for (label, path) in items {
        let display_path = match path {
            Some(p) => shorten_path(&p, max_val_width),
            None => "-".to_string(),
        };
        rows.push(Line::from(vec![
            Span::styled(
                format!("{:<width$}", label, width = HELP_KEY_CELLS),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(display_path, Style::default().fg(theme::LABEL)),
        ]));
    }
    rows
}

/// The last row of the page: what the page counts on the left, how to leave on
/// the right. It falls to two rows when one cannot hold both, because the way
/// out matters more than sitting beside the count.
fn help_footer(width: usize) -> Vec<Line<'static>> {
    let count = format!(
        "{} keys bound - the footer shows at most {}",
        keys_bound(),
        footer_most()
    );
    let leave = |out: &mut Vec<Span<'static>>| {
        // `q` only closes the page here, so the page does not offer it as quit
        for (i, (key, label)) in [("Esc", "back"), ("q", "back")].into_iter().enumerate() {
            if i > 0 {
                out.push(Span::raw("  "));
            }
            out.push(Span::styled(
                key,
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ));
            out.push(Span::raw(" "));
            out.push(Span::styled(label, Style::default().fg(theme::LABEL)));
        }
    };
    let leave_cells = "Esc back  q back".chars().count();
    let count_span = Span::styled(count.clone(), Style::default().fg(theme::MUTED));
    let mut keys = Vec::new();
    leave(&mut keys);
    if count.chars().count() + 2 + leave_cells <= width {
        let pad = width - count.chars().count() - leave_cells;
        let mut row = vec![count_span, Span::raw(" ".repeat(pad))];
        row.extend(keys);
        vec![Line::from(row)]
    } else {
        vec![Line::from(count_span), Line::from(keys)]
    }
}

fn help_cadence() -> Vec<Line<'static>> {
    let muted = Style::default().fg(theme::MUTED);
    vec![
        Line::styled(
            format!(
                "Metrics every 5s, health every 30s, load balancer traffic every 60s ({LB_WINDOW_MINUTES}m window)"
            ),
            muted,
        ),
        Line::styled(
            "A tunnel going unhealthy raises a system notification",
            muted,
        ),
    ]
}

/// Two stacks side by side, the left one padded to its column so the right
/// starts at the same cell on every row.
fn help_columns(
    left: Vec<Line<'static>>,
    right: Vec<Line<'static>>,
    column: usize,
) -> Vec<Line<'static>> {
    let rows = left.len().max(right.len());
    (0..rows)
        .map(|i| {
            let mut spans = left.get(i).map(|l| l.spans.clone()).unwrap_or_default();
            let used = row_cells(&spans);
            spans.push(Span::raw(
                " ".repeat((column + HELP_GAP).saturating_sub(used)),
            ));
            spans.extend(right.get(i).map(|l| l.spans.clone()).unwrap_or_default());
            Line::from(spans)
        })
        .collect()
}

/// The keymap page as drawn at one size and scroll position, and how far it can
/// scroll at that size.
pub struct HelpPage {
    pub lines: Vec<Line<'static>>,
    /// The furthest the page scrolls; zero when it fits.
    pub max_scroll: usize,
    /// How many body rows the window shows.
    pub view: usize,
}

/// The keymap page, laid out from `KEYMAP` and cut to `height` rows.
///
/// A page that does not fit scrolls. It used to drop its notes and then its
/// last rows and count the keys that went, which left the program group and the
/// case note unreachable at 80x24: a keymap that cannot be read in full fails
/// its one job. The body (the groups, and in one column the case note) is the
/// scrolling part; the count line, `Esc back` and the blank row above them stay
/// put, so the way out is never scrolled off. The cadence lines are the one
/// thing still dropped, and only when they would not fit under the whole page,
/// since they are the least a reader came for. A cut page spends its last body
/// row on where the reader is in it, so a window is never mistaken for the page.
pub fn help_page(width: usize, height: usize, scroll: usize) -> HelpPage {
    let two = width >= HELP_TWO_COLUMNS;
    let body = if two {
        let column = (width - HELP_GAP) / 2;
        let mut left = help_stack(&[Group::Tunnel, Group::List]);
        left.push(Line::raw(""));
        left.extend(help_paths(column));
        let mut right = help_stack(&[Group::UrlAndLog, Group::Program]);
        right.push(Line::raw(""));
        right.extend(help_note(column));
        help_columns(left, right, column)
    } else {
        let mut rows = help_stack(&[Group::Tunnel, Group::List]);
        rows.push(Line::raw(""));
        rows.extend(help_paths(width));
        rows.push(Line::raw(""));
        rows.extend(help_stack(&[Group::UrlAndLog, Group::Program]));
        rows.push(Line::raw(""));
        rows.extend(help_note(width));
        rows
    };
    let footer = help_footer(width);
    let avail = height.saturating_sub(footer.len() + 1);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let (max_scroll, view) = if body.len() <= avail {
        let view = body.len();
        lines.extend(body);
        (0, view)
    } else {
        // One row of the window goes to the counter
        let view = avail.saturating_sub(1);
        let max_scroll = if view == 0 { 0 } else { body.len() - view };
        let top = scroll.min(max_scroll);
        let total = body.len();
        lines.extend(body.into_iter().skip(top).take(view));
        if avail > 0 {
            lines.push(help_counter(top, view, total, width));
        }
        (max_scroll, view)
    };
    lines.push(Line::raw(""));
    lines.extend(footer);
    if max_scroll == 0 && lines.len() + 2 <= height {
        lines.extend(help_cadence());
    }
    HelpPage {
        lines,
        max_scroll,
        view,
    }
}

/// `3-14 of 31 rows`, with the keys that move it when the row has room.
fn help_counter(top: usize, view: usize, total: usize, width: usize) -> Line<'static> {
    let place = if view == 0 {
        format!("{total} rows")
    } else {
        format!("{}-{} of {total} rows", top + 1, top + view)
    };
    let hinted = format!("{place}  j/k scroll");
    let text = if hinted.chars().count() <= width {
        hinted
    } else {
        place
    };
    Line::styled(text, Style::default().fg(theme::MUTED))
}

/// The modal's frame width on a screen this wide: a margin of two cells on each
/// side, and no wider than the two columns can use, since past that the labels
/// only drift away from the keys.
pub fn help_modal_width(screen_width: u16) -> u16 {
    screen_width.saturating_sub(4).min(112)
}

/// The frame's title at this modal width.
///
/// The version belongs here because nothing else on screen carries it: the
/// header's cells are spoken for by the account and the project, and the
/// footer sheds keys under width pressure rather than gaining text. Someone
/// filing a bug already opens this page to say which key they pressed.
///
/// ratatui truncates a title that does not fit without saying so, which would
/// leave ` Keys - gtunn` on a narrow terminal, so the title sheds instead: the
/// version goes, then the subject, in that order.
pub fn help_modal_title(modal_width: u16) -> String {
    // The two corners are not the title's to use
    let room = modal_width.saturating_sub(2) as usize;
    let full = format!(" Keys - gtunnel {} ", env!("CARGO_PKG_VERSION"));
    if full.chars().count() <= room {
        return full;
    }
    if HELP_TITLE_SHORT.chars().count() <= room {
        return HELP_TITLE_SHORT.to_string();
    }
    String::new()
}

/// The title with the version shed, and the widest one a tiny frame can hold.
const HELP_TITLE_SHORT: &str = " Keys ";

/// The width `help_page` lays out in, for a screen this wide: the frame less
/// two cells of border and two of air. Shared so a test measures the width the
/// renderer used and not its own copy of the arithmetic.
pub fn help_inner_width(screen_width: u16) -> usize {
    help_modal_width(screen_width).saturating_sub(4) as usize
}

fn render_help_modal(f: &mut Frame, app: &App) {
    let screen = f.area();
    let width = help_modal_width(screen.width);
    let room = screen.height.saturating_sub(2);
    let inner_width = help_inner_width(screen.width);
    let HelpPage {
        lines,
        max_scroll,
        view,
    } = help_page(
        inner_width,
        room.saturating_sub(2) as usize,
        app.help_scroll,
    );
    // The keys clamp to what this draw found, so a resize that changes the
    // page's length is corrected by the next press and never by a dead one
    app.help_extent.set(HelpExtent { max_scroll, view });
    // The frame hugs the page: the rows under it would be an empty box
    let height = (lines.len() as u16 + 2).min(room);
    let area = Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(help_modal_title(width))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);
    // One cell of air inside the border, as the panes keep
    let inner = Rect {
        x: inner.x.saturating_add(1),
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The config page as drawn at one size and scroll position.
pub struct ConfigPage {
    pub lines: Vec<Line<'static>>,
    /// The furthest the file scrolls; zero when it fits.
    pub max_scroll: usize,
    /// How many rows of the file the window shows.
    pub view: usize,
}

const CONFIG_NOTE: &str =
    "A saved edit reloads this list. Running tunnels keep their old settings until R restarts them.";
/// Fewest rows of the file worth sharing the page with the note.
const CONFIG_BODY_MIN: usize = 3;

fn config_missing(file: ConfigFile) -> &'static str {
    match file {
        ConfigFile::Config => "no config file yet: `gtunnel init` creates it",
        ConfigFile::Tunnels => {
            "no tunnels file yet: `gtunnel add` creates it, or press e to write one"
        }
    }
}

/// The keys the page answers to, shed from the left so `Esc` is last to go. A
/// failed edit has no `Tab`: moving to the other file would leave the edit
/// that was refused with nowhere to be reopened.
fn config_keys(width: usize, failed: bool) -> Vec<Span<'static>> {
    let mut keys: Vec<(&'static str, &'static str)> = vec![
        ("e", if failed { "edit again" } else { "edit" }),
        ("Tab", "files"),
        ("Esc", if failed { "discard" } else { "back" }),
    ];
    if failed {
        keys.remove(1);
    }
    let cells = |keys: &[(&str, &str)]| {
        keys.iter()
            .map(|(k, l)| k.chars().count() + 1 + l.chars().count())
            .sum::<usize>()
            + 2 * keys.len().saturating_sub(1)
    };
    // `Esc` is the last entry and never leaves; the others go from the front
    while keys.len() > 1 && cells(&keys) > width {
        // Tab goes before e: choosing a file matters less than the edit
        let at = keys.iter().position(|(k, _)| *k == "Tab").unwrap_or(0);
        keys.remove(at);
    }
    let mut out = Vec::new();
    for (i, (key, label)) in keys.into_iter().enumerate() {
        if i > 0 {
            out.push(Span::raw("  "));
        }
        out.push(Span::styled(
            key,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
        out.push(Span::raw(" "));
        out.push(Span::styled(label, Style::default().fg(theme::LABEL)));
    }
    out
}

/// The config file as a page, laid out in `height` rows.
///
/// What stays and what scrolls follows the help page: the file scrolls, and
/// the row of count and keys under it is pinned, so `Esc` is on screen at any
/// size that has a row for it. Under that, in the order they are given up when
/// the terminal is short: the reason an edit was refused, which is the one
/// thing the reader has to act on; then the restart note, which is dropped
/// whenever it would leave the file fewer than `CONFIG_BODY_MIN` rows and
/// whenever there is a message to read instead. The count sits in the pinned
/// row and not in the file's window, so where the reader is in a long file
/// costs no row of the file.
pub fn config_page(width: usize, height: usize, view: &ConfigView) -> ConfigPage {
    let width = width.max(1);
    let missing = view.text.is_none();
    let body: Vec<Line<'static>> = match view.text.as_deref() {
        None => wrap_words(config_missing(view.file), width)
            .into_iter()
            .map(|r| Line::styled(r, Style::default().fg(theme::MUTED)))
            .collect(),
        Some(text) => text
            .lines()
            .map(|l| Line::raw(fit(&l.replace('\t', "    "), width)))
            .collect(),
    };
    let total = view.text.as_deref().map_or(0, |t| t.lines().count());
    let failed = view.error.is_some();

    // One row if the widest count and the keys fit side by side, else two
    let keys = config_keys(width, failed);
    let keys_cells = row_cells(&keys);
    let widest = format!("{total}-{total} of {total} lines  j/k scroll");
    let two_rows = widest.chars().count() + 2 + keys_cells > width;
    let mut footer_rows = if two_rows { 2 } else { 1 };
    // The way out outranks the count when there is only room for one row
    footer_rows = footer_rows.min(height);
    let rest = height - footer_rows;

    // What the last edit came to: a refusal outranks a notice, and either
    // replaces the standing note, which would only repeat the restart warning
    // that a finished edit already carries
    let message: Option<(String, Color)> = match (view.error.as_deref(), view.notice.as_ref()) {
        (Some(error), _) => Some((format!("not saved: {error}"), theme::BAD)),
        (None, Some(n)) => Some((n.text.clone(), if n.ok { theme::OK } else { theme::BAD })),
        (None, None) => None,
    };
    let message_rows: Vec<Line<'static>> = match message.as_ref() {
        Some((text, colour)) if rest > 0 => {
            let cap = (rest / 2).clamp(1, 4);
            let mut rows = wrap_words(text, width);
            if rows.len() > cap {
                rows.truncate(cap);
                let last = rows.pop().unwrap_or_default();
                rows.push(fit(&format!("{last}\u{2026}"), width));
            }
            rows.into_iter()
                .map(|r| Line::styled(r, Style::default().fg(*colour)))
                .collect()
        }
        _ => Vec::new(),
    };
    let rest = rest - message_rows.len();
    let note_rows: Vec<Line<'static>> = if message.is_some() {
        Vec::new()
    } else {
        let rows = wrap_words(CONFIG_NOTE, width);
        if rest >= rows.len() + CONFIG_BODY_MIN.min(body.len()) {
            rows.into_iter()
                .map(|r| Line::styled(r, Style::default().fg(theme::MUTED)))
                .collect()
        } else {
            Vec::new()
        }
    };
    let mut window = rest - note_rows.len();
    // A blank row says where the file ends, and is the first thing given up
    // when the page is short: the file and the way out come before it
    let spacer = (!message_rows.is_empty() || !note_rows.is_empty())
        && window > CONFIG_BODY_MIN.min(body.len());
    if spacer {
        window -= 1;
    }

    // A file that fits is shown whole; one that does not scrolls in a window
    let max_scroll = body.len().saturating_sub(window);
    let top = view.scroll.min(max_scroll);
    let shown: Vec<Line<'static>> = body.into_iter().skip(top).take(window).collect();
    let view_rows = shown.len();

    let place = if missing {
        "not created yet".to_string()
    } else if max_scroll == 0 {
        format!("{total} lines")
    } else {
        format!("{}-{} of {total} lines", top + 1, top + view_rows)
    };
    let hinted = format!("{place}  j/k scroll");
    let count = if !two_rows && hinted.chars().count() + 2 + keys_cells <= width && max_scroll > 0 {
        hinted
    } else {
        place
    };
    let count_line = Line::styled(fit(&count, width), Style::default().fg(theme::MUTED));
    let footer: Vec<Line<'static>> = if footer_rows == 2 {
        vec![count_line, Line::from(keys)]
    } else if height >= 1 && !two_rows {
        let pad = width.saturating_sub(count.chars().count() + keys_cells);
        let mut row = vec![
            Span::styled(count, Style::default().fg(theme::MUTED)),
            Span::raw(" ".repeat(pad)),
        ];
        row.extend(keys);
        vec![Line::from(row)]
    } else {
        // Two rows were wanted and one is all there is: the keys win
        vec![Line::from(keys)]
    };

    let mut lines = shown;
    if spacer {
        lines.push(Line::raw(""));
    }
    lines.extend(message_rows);
    lines.extend(note_rows);
    lines.extend(footer);
    if height == 0 {
        lines.clear();
    }
    ConfigPage {
        lines,
        max_scroll,
        view: view_rows,
    }
}

/// The path for a title, cut from the front: the end is the file's name, and
/// which of the two files this is matters more than which directory it is in.
fn tail_fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    match width {
        0 => String::new(),
        n => {
            let tail: String = text.chars().skip(count - (n - 1)).collect();
            format!("\u{2026}{tail}")
        }
    }
}

fn render_config_modal(f: &mut Frame, app: &App) {
    let screen = f.area();
    let width = help_modal_width(screen.width);
    let height = screen.height.saturating_sub(2);
    let inner_width = help_inner_width(screen.width);
    let page = config_page(
        inner_width,
        height.saturating_sub(2) as usize,
        &app.config_view,
    );
    app.help_extent.set(HelpExtent {
        max_scroll: page.max_scroll,
        view: page.view,
    });
    // The frame hugs the page, as the keymap's does: rows under it would be an
    // empty box
    let height = (page.lines.len() as u16 + 2).min(height);
    let area = Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(
            " {} ",
            tail_fit(&app.config_view.path, (width as usize).saturating_sub(4))
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let inner = Rect {
        x: inner.x.saturating_add(1),
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    f.render_widget(Paragraph::new(page.lines), inner);
}

/// The frame a dialog gets on a screen: as big as it asked for and no bigger
/// than the terminal less a margin, centred. The dialogs used to be a percentage
/// of the screen, which is the wrong unit for text: a quarter of 24 rows is
/// less than the five a prompt needs, and 60 percent of 60 columns is less than
/// the question it asks. Sized to the text and then to the terminal, a dialog
/// keeps its words until the terminal cannot hold them, and never leaves it. The
/// floor of a border and one row of text is kept even on a terminal too small to
/// leave a margin, so there is always a row to draw the prompt in.
pub fn dialog_rect(screen: Rect, want_w: u16, want_h: u16) -> Rect {
    let width = want_w
        .min(screen.width.saturating_sub(DIALOG_MARGIN_X))
        .max(screen.width.min(DIALOG_FLOOR_W));
    let height = want_h
        .min(screen.height.saturating_sub(DIALOG_MARGIN_Y))
        .max(screen.height.min(DIALOG_FLOOR_H));
    Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    }
}

// Two cells each side and one row top and bottom, so the frame never touches
// the terminal's edge and the panes behind it still frame the dialog
const DIALOG_MARGIN_X: u16 = 4;
const DIALOG_MARGIN_Y: u16 = 2;
const DIALOG_FLOOR_W: u16 = 6;
const DIALOG_FLOOR_H: u16 = 3;
/// A dialog is never narrower than this when the terminal has the room, so a
/// two-word prompt still leaves a field to type in.
const DIALOG_MIN_WIDTH: usize = 40;
/// Nor wider: past this a long value is cut to its tail rather than stretching
/// the frame across the screen.
const DIALOG_MAX_WIDTH: usize = 72;

fn cells(text: &str) -> usize {
    text.chars().count()
}

/// The width a dialog asks for: its widest line, its title, and a floor, in
/// cells of frame (the border is added here).
fn dialog_width(widest: usize, title: &str) -> u16 {
    (widest
        .max(cells(title) + 4)
        .clamp(DIALOG_MIN_WIDTH, DIALOG_MAX_WIDTH)
        + 2) as u16
}

/// Draws a dialog whose lines depend on the width and height it is given. The
/// builder is handed the room inside the frame and returns the lines that fit
/// it, and the frame is then sized to them, so a short dialog is a short frame.
fn render_dialog(
    f: &mut Frame,
    title: &str,
    want_w: u16,
    build: impl FnOnce(usize, usize) -> Vec<Line<'static>>,
) {
    let screen = f.area();
    let room = dialog_rect(screen, want_w, u16::MAX);
    let inner_w = room.width.saturating_sub(2) as usize;
    let inner_h = room.height.saturating_sub(2) as usize;
    let lines = build(inner_w, inner_h);
    let area = dialog_rect(screen, want_w, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines), inner);
}

/// Sections stacked with a blank row between them when that fits, and without
/// when only that does; `None` when even the bare rows do not.
fn stacked(sections: &[Vec<Line<'static>>], rows: usize) -> Option<Vec<Line<'static>>> {
    let bare: usize = sections.iter().map(Vec::len).sum();
    let gaps = sections.len().saturating_sub(1);
    let with_blanks = bare + gaps <= rows;
    if !with_blanks && bare > rows {
        return None;
    }
    let mut out = Vec::new();
    for (i, section) in sections.iter().enumerate() {
        if i > 0 && with_blanks {
            out.push(Line::raw(""));
        }
        out.extend(section.iter().cloned());
    }
    Some(out)
}

/// `  text` in rows of at most `width` cells, continuation rows indented as the
/// first is. Indent is part of the width, so a wrapped row never runs over.
fn wrapped_indent(text: &str, indent: usize, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap_words(text, width.saturating_sub(indent))
        .into_iter()
        .map(|row| Line::styled(format!("{}{row}", " ".repeat(indent)), style))
        .collect()
}

/// What was typed and the cursor after it. A value wider than the dialog keeps
/// its tail: the end is where the next key lands, and a cursor pushed off the
/// right edge reads as a field that has stopped taking input.
fn input_row(input: &str, width: usize) -> Line<'static> {
    let room = width.saturating_sub(3);
    let skip = cells(input).saturating_sub(room);
    let tail: String = input.chars().skip(skip).collect();
    Line::from(vec![
        Span::raw(format!("  {tail}")),
        Span::styled("_", Style::default().fg(theme::ACCENT)),
    ])
}

/// A numbered chooser: one key per option and Esc to leave. Both of the
/// dialogs that open on a key rather than a field are drawn here, so an
/// option added to either reads the same way as the ones beside it.
fn render_chooser(f: &mut Frame, title: &str, choices: &[(&str, &str)]) {
    const LEAD: usize = 5;
    let widest = choices.iter().map(|o| LEAD + cells(o.1)).max().unwrap_or(0);
    render_dialog(f, title, dialog_width(widest, title), |width, rows| {
        let mut options = Vec::new();
        for (key, text) in choices {
            for (i, row) in wrap_words(text, width.saturating_sub(LEAD))
                .into_iter()
                .enumerate()
            {
                let lead = if i == 0 {
                    Span::styled(format!("  {key}  "), Style::default().fg(theme::ACCENT))
                } else {
                    Span::raw(" ".repeat(LEAD))
                };
                options.push(Line::from(vec![lead, Span::raw(row)]));
            }
        }
        let hint = vec![Line::styled(
            "  Esc to cancel",
            Style::default().fg(theme::MUTED),
        )];
        stacked(&[options.clone(), hint], rows).unwrap_or_else(|| {
            options.truncate(rows);
            options
        })
    });
}

fn render_choose_mode(f: &mut Frame) {
    render_chooser(
        f,
        "New tunnel",
        &[
            ("1", "Access - forward a remote port to localhost over IAP"),
            ("2", "Public - publish a local service at an HTTPS URL"),
        ],
    );
}

fn render_choose_edit(f: &mut Frame) {
    render_chooser(
        f,
        "Edit",
        &[
            ("1", "Target and ports - what it forwards, and where to"),
            ("2", "Rename - the same account, another name"),
            ("3", "Move - hand it to another account"),
        ],
    );
}

/// The keys the accounts page answers to, in the order a reader meets them.
const ACCOUNT_KEYS: [(&str, &str); 6] = [
    ("j/k", "move"),
    ("Enter", "select"),
    ("a", "add"),
    ("e", "edit"),
    ("d", "remove"),
    ("Esc", "back"),
];

/// The cells the whole hint wants on one row, indent included.
fn account_keys_cells() -> usize {
    ACCOUNT_KEYS
        .iter()
        .map(|(k, l)| cells(k) + 1 + cells(l))
        .sum::<usize>()
        + 2 * (ACCOUNT_KEYS.len() - 1)
        + 2
}

/// The hint as rows that fit `width`, a pair never split across two of them.
///
/// Not word-wrapped: wrapping collapses the two cells between pairs to one,
/// and the row then reads as a sentence - "Enter select a add e edit" - with
/// nothing to say where one key ends and the next begins.
fn account_key_rows(width: usize) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut used = 0;
    for (i, (key, label)) in ACCOUNT_KEYS.iter().enumerate() {
        let pair = cells(key) + 1 + cells(label);
        if i == 0 || used + 2 + pair > width {
            rows.push(vec![Span::raw("  ")]);
            used = 2;
        } else {
            rows.last_mut()
                .expect("a row to add to")
                .push(Span::raw("  "));
            used += 2;
        }
        let row = rows.last_mut().expect("a row to add to");
        row.push(Span::styled(
            *key,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
        row.push(Span::raw(" "));
        row.push(Span::styled(*label, Style::default().fg(theme::LABEL)));
        used += pair;
    }
    rows.into_iter().map(Line::from).collect()
}

/// One row per account: whether it is the one on screen, its name, its
/// project and its zone.
///
/// The project and the zone are what tell two accounts apart — the name is a
/// label this machine chose — so the row carries both rather than the name
/// alone. The columns are sized to the widest value and not capped here: the
/// dialog's own width decides what is cut, as the list's pane does.
pub fn account_rows(accounts: &[String], app: &App) -> Vec<String> {
    let widest = |values: &[String]| values.iter().map(|v| cells(v)).max().unwrap_or(0);
    let projects: Vec<String> = accounts
        .iter()
        .map(|a| app.projects.get(a).cloned().unwrap_or_default())
        .collect();
    let name_w = widest(accounts);
    let project_w = widest(&projects);
    accounts
        .iter()
        .zip(projects.iter())
        .map(|(name, project)| {
            // The account on screen, which is what every other pane is about
            let mark = if *name == app.account { "*" } else { " " };
            let zone = app.zones.get(name).cloned().unwrap_or_default();
            format!("{mark} {name:<name_w$}  {project:<project_w$}  {zone}")
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The accounts page. It is a dialog and not a full-screen page like the
/// config one, because its content is a handful of short rows: a frame sized
/// to them leaves the list and the header visible behind it, which is what
/// says the page is a detour and not another screen.
fn render_accounts(f: &mut Frame, app: &App) {
    let rows = account_rows(&app.accounts, app);
    let widest = rows
        .iter()
        .map(|r| cells(r) + 2)
        .max()
        .unwrap_or(0)
        .max(account_keys_cells());
    render_dialog(
        f,
        "Accounts",
        dialog_width(widest, "Accounts"),
        |w, room| {
            let keys = account_key_rows(w);
            let notice: Vec<Line<'static>> = app
                .account_notice
                .iter()
                .flat_map(|n| {
                    let colour = if n.ok { theme::OK } else { theme::BAD };
                    wrapped_indent(&n.text, 2, w, Style::default().fg(colour))
                })
                .collect();
            // The rows have the first claim on the frame, as the picker's do: a
            // page that shows its keys and no accounts has stopped being a list
            let visible = room.saturating_sub(keys.len() + notice.len()).max(1);
            let at = app.account_selected.min(rows.len().saturating_sub(1));
            let start = (at + 1).saturating_sub(visible);
            let listed: Vec<Line<'static>> = rows
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .map(|(i, row)| picker_row(&fit(row, w.saturating_sub(2)), i == at))
                .collect();
            let mut sections = vec![listed];
            if !notice.is_empty() {
                sections.push(notice);
            }
            sections.push(keys);
            stacked(&sections, room).unwrap_or_else(|| {
                let mut lines: Vec<Line<'static>> = sections.concat();
                lines.truncate(room);
                lines
            })
        },
    );
}

fn render_confirm(f: &mut Frame, message: &str) {
    render_dialog(
        f,
        "Confirm",
        dialog_width(cells(message), "Confirm"),
        |width, rows| {
            let mut lines: Vec<Line<'static>> = wrap_words(message, width)
                .into_iter()
                .map(Line::from)
                .collect();
            lines.truncate(rows);
            lines
        },
    );
}

const PROMPT_HINT: &str = "Enter to continue, Esc to cancel";

fn render_prompt(f: &mut Frame, app: &App, mode: InputMode) {
    let title = match app.draft.mode {
        TunnelMode::Access => "New access tunnel",
        TunnelMode::Public => "New public URL",
    };
    let title = match mode {
        InputMode::EditTarget => "Edit",
        InputMode::RenameTo => "Rename",
        InputMode::MoveRename => "Move",
        InputMode::AccountConfiguration | InputMode::AccountName => "New account",
        InputMode::AccountProject | InputMode::AccountZone => "Edit account",
        _ => title,
    };
    let prompt = mode.prompt();
    let widest = cells(prompt)
        .max(cells(&app.input) + 3)
        .max(cells(PROMPT_HINT) + 2);
    render_dialog(f, title, dialog_width(widest, title), |width, rows| {
        let ask = wrapped_indent(prompt, 0, width, Style::default().fg(theme::WARN));
        let mut field = vec![input_row(&app.input, width)];
        if let Some(error) = &app.draft_error {
            field.extend(wrapped_indent(
                error,
                0,
                width,
                Style::default().fg(theme::BAD),
            ));
        }
        let hint = wrapped_indent(PROMPT_HINT, 2, width, Style::default().fg(theme::MUTED));
        // What goes first when the terminal is short: the blank rows, then the
        // hint, then the end of a prompt that wraps. The field is never shed,
        // since the keys a user types must land somewhere they can see.
        stacked(&[ask.clone(), field.clone(), hint], rows)
            .or_else(|| stacked(&[ask.clone(), field.clone()], rows))
            .unwrap_or_else(|| {
                let mut lines: Vec<Line<'static>> =
                    ask.into_iter().take(rows.saturating_sub(1)).collect();
                lines.extend(field);
                lines
            })
    });
}

fn picker_row(row: &str, selected: bool) -> Line<'static> {
    let style = if selected {
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(theme::ACCENT)
    } else {
        Style::default()
    };
    Line::styled(format!("  {row}"), style)
}

struct Picker<'a> {
    title: &'a str,
    prompt: &'a str,
    filter: &'a str,
    rows: Vec<String>,
    selected: usize,
    // Why the dialog came back, drawn where the dialog covers the status line
    error: Option<&'a str>,
}

/// A dialog whose row list is taller than the dialog: the header is fixed and
/// the rows scroll just enough to keep the selected one on screen. The list
/// gets the first claim on the frame, because a picker that shows its prompt
/// and no choices has stopped being one; the header takes what is left, the
/// filter first, then the prompt, then the blank row that only spaces it.
fn render_picker(f: &mut Frame, p: Picker) {
    let Picker {
        title,
        prompt,
        filter,
        rows,
        selected,
        error,
    } = p;
    let total = rows.len();
    let counter = if total > 0 {
        format!("  {}/{}", selected.min(total - 1) + 1, total)
    } else {
        String::new()
    };
    let widest =
        (cells(prompt) + cells(&counter)).max(rows.iter().map(|r| cells(r) + 2).max().unwrap_or(0));
    render_dialog(f, title, dialog_width(widest, title), |width, room| {
        let mut ask = wrapped_indent(prompt, 0, width, Style::default().fg(theme::WARN));
        // The counter rides on the prompt's last row, or takes one of its own
        let muted = Style::default().fg(theme::MUTED);
        match ask.last_mut() {
            Some(last) if cells(&last.to_string()) + cells(&counter) <= width => {
                last.push_span(Span::styled(counter.clone(), muted));
            }
            _ if total > 0 => ask.push(Line::styled(counter.trim_start().to_string(), muted)),
            _ => {}
        }
        let mut budget = room.saturating_sub(1);
        let mut head: Vec<Line<'static>> = Vec::new();
        let keep_filter = budget > 0;
        budget = budget.saturating_sub(1);
        let asked = ask.len().min(budget);
        budget -= asked;
        head.extend(ask.into_iter().take(asked));
        if keep_filter {
            head.push(input_row(filter, width));
            if let Some(error) = error {
                let lines = wrapped_indent(error, 0, width, Style::default().fg(theme::BAD));
                let shown = lines.len().min(budget);
                budget -= shown;
                head.extend(lines.into_iter().take(shown));
            }
            if budget > 0 {
                head.push(Line::raw(""));
            }
        }

        let visible = room.saturating_sub(head.len());
        let mut lines = head;
        if total == 0 {
            lines.push(Line::styled("  nothing matches", muted));
        } else {
            let start = (selected.min(total - 1) + 1).saturating_sub(visible);
            lines.extend(
                rows.iter()
                    .enumerate()
                    .skip(start)
                    .take(visible)
                    .map(|(i, row)| picker_row(row, i == selected)),
            );
        }
        lines
    });
}

fn render_instance_picker(f: &mut Frame, app: &App) {
    let rows: Vec<String> = app
        .filtered_instances()
        .iter()
        .map(|i| format!("{:<30} {}", i.name, i.zone))
        .collect();
    render_picker(
        f,
        Picker {
            title: "Instance",
            prompt: InputMode::AddInstance.prompt(),
            filter: &app.input,
            rows,
            selected: app.instance_selected,
            error: app.draft_error.as_deref(),
        },
    );
}

/// Where a move may send the selected tunnel. A picker and not a field: the
/// accounts are a short list this process already holds, so an account that
/// does not exist cannot be asked for.
fn render_account_picker(f: &mut Frame, app: &App) {
    render_picker(
        f,
        Picker {
            title: "Move",
            prompt: InputMode::MoveTo.prompt(),
            filter: "",
            rows: app.other_accounts(),
            selected: app.move_selected,
            error: app.draft_error.as_deref(),
        },
    );
}

fn render_domain_picker(f: &mut Frame, app: &App) {
    render_picker(
        f,
        Picker {
            title: "Domain",
            prompt: InputMode::AddDomain.prompt(),
            filter: "",
            rows: app.domains.clone(),
            selected: app.domain_selected,
            error: app.draft_error.as_deref(),
        },
    );
}
