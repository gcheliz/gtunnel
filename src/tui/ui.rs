use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Sparkline, Wrap,
};
use ratatui::Frame;

use super::app::{App, InputMode, TunnelMode};
use super::theme;
use crate::probe::{HealthStatus, LB_WINDOW_MINUTES};
use crate::state::{TunnelSpec, TunnelStatus};
use crate::tunnel::diagnose_log_line;

pub const LOG_LINES: usize = 200;

/// The lockup, the name beneath it, and the rule that closes the header off
/// from the panes. One size at every width: the design draws the same eleven
/// cells on a 1200-pixel board as on a narrow one, and a mark that grows with
/// the terminal is a second mark, not a responsive one.
pub const HEADER_HEIGHT_FULL: u16 = 5;
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
const MARK_FALLBACK: [&str; MARK_ROWS] =
    ["\u{2500}\u{2500}\u{2597}\u{2584}\u{259f}\u{2580}\u{2599}\u{2584}\u{2596}\u{2500}\u{2500}"];

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

/// The label beside the account value. Its spaces are part of the field.
const ACCOUNT_LABEL: &str = "ACCOUNT";

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

/// How many cells the account field occupies: the label, the name, and the cell
/// of padding on each side that makes it read as a field and not as more text.
fn account_field_width(account: &str) -> usize {
    account_box_width(account)
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

/// The subtitle and the right-aligned account field that sit on the mark's top
/// row, padded so the field's last cell lands on the final column.
fn caption_spans(w: usize, mark_cells: usize, account: &str) -> Vec<Span<'static>> {
    const SUBTITLE: &str = "IAP tunnel manager";
    let field = account_field_width(account);
    let used = MARGIN + mark_cells + field + MARGIN;

    // The subtitle appears only when it fits whole, behind the divider that
    // separates it from the mark. A half-drawn subtitle is worse than none.
    if w >= used + DIVIDER_WIDTH + SUBTITLE.chars().count() {
        vec![
            divider_span(),
            Span::styled(SUBTITLE, Style::default().fg(theme::MUTED)),
        ]
    } else {
        Vec::new()
    }
}

/// How wide the account box is: two borders, the label, the value, and the
/// air around them.
fn account_box_width(account: &str) -> usize {
    ACCOUNT_LABEL.chars().count() + account.chars().count() + 6
}

/// The three rows of the account box, top to bottom.
///
/// The account is drawn as a box rather than a filled chip because it is the
/// one control on the header: a block of solid colour reads as a status
/// light, something the program is telling you, while an outline reads as a
/// field — something you can change, which `;` does.
fn account_box(account: &str) -> [Vec<Span<'static>>; 3] {
    let w = account_box_width(account);
    let edge = Style::default().fg(theme::MUTED);
    let rule = "\u{2500}".repeat(w.saturating_sub(2));
    [
        vec![Span::styled(format!("\u{256d}{rule}\u{256e}"), edge)],
        vec![
            Span::styled("\u{2502} ", edge),
            Span::styled(ACCOUNT_LABEL, Style::default().fg(theme::ACCENT)),
            Span::raw("  "),
            Span::styled(
                account.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" \u{2502}", edge),
        ],
        vec![Span::styled(format!("\u{2570}{rule}\u{256f}"), edge)],
    ]
}

/// How many cells a row of spans occupies.
fn row_cells(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// The name under the mark, and the divider carried down beside it so the
/// separator runs the lockup's whole height rather than half of it.
fn name_spans(row: &'static str, divided: bool) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::raw(" ".repeat(MARGIN)),
        Span::styled(
            row,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if divided {
        spans.push(divider_span());
    }
    spans
}

/// The lockup, the subtitle behind its divider, the account field pinned
/// right, and the rule that closes the header off.
///
/// One size. What gives way as the terminal narrows is the subtitle, then the
/// lockup itself: the account field has priority over both, because it is the
/// one piece of state that changes what every other key acts on. When even it
/// will not fit beside the mark, the header falls to a single line.
pub fn header_lines(width: u16, _height: u16, account: &str) -> Vec<Line<'static>> {
    let w = width as usize;
    let field = account_field_width(account);

    if w < MARGIN + MARK_WIDTH as usize + 1 + field + MARGIN {
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

    let caption = caption_spans(w, MARK_WIDTH as usize, account);
    // The divider belongs to the subtitle: with no room for one there is
    // nothing for it to separate, and it would point at empty space.
    let divided = !caption.is_empty();

    let mut top = vec![Span::raw(" ".repeat(MARGIN))];
    top.extend(mark_spans(true, 0));
    top.extend(caption);

    // The box is three rows tall and the lockup is two, so it starts on the
    // blank row above the mark: that centres it on the mark rather than on
    // the word, and the mark is what the eye lands on first.
    let boxw = account_box_width(account);
    let mut rows = [Vec::new(), top, name_spans(NAME_ROW, divided)];
    for (row, segment) in rows.iter_mut().zip(account_box(account)) {
        let pad = w.saturating_sub(row_cells(row) + boxw + MARGIN);
        row.push(Span::raw(" ".repeat(pad)));
        row.extend(segment);
        row.push(Span::raw(" ".repeat(MARGIN)));
    }
    let [r0, r1, r2] = rows;

    // A blank row below the lockup. The header is the one part of the screen
    // that is not working data, and crowded against the panes it read as
    // another pane rather than as the program's name.
    vec![
        Line::from(r0),
        Line::from(r1),
        Line::from(r2),
        Line::default(),
        rule_line(w),
    ]
}

/// How many rows the header takes here. Derived from `header_lines` rather than
/// recomputed, because a layout that reserves a different number of rows than
/// the header draws is a bug with no symptom until it clips.
pub fn header_height(width: u16, height: u16, account: &str) -> u16 {
    match header_lines(width, height, account).len() {
        1 => HEADER_HEIGHT_COMPACT,
        _ => HEADER_HEIGHT_FULL,
    }
}

/// One key and what it does. The footer and the `?` overlay draw the same
/// shape, so they cannot drift apart.
pub struct Action {
    pub key: &'static str,
    pub label: &'static str,
}

// The old footer printed all fourteen keys at once, which needed 124 cells and
// so lost its tail on a 120-column terminal. Worse, it put `s` beside `S`, `a`
// beside `A` and `r` beside `R`: three pairs where case is the whole
// difference. Showing only what the selection can actually do dissolves that
// trap rather than annotating it — the two halves of a pair are never on
// screen together, because a tunnel cannot be both running and stopped.
const ACTIONS_RUNNING: &[Action] = &[
    Action {
        key: "S",
        label: "stop",
    },
    Action {
        key: "R",
        label: "restart",
    },
    Action {
        key: "c",
        label: "copy",
    },
    Action {
        key: "o",
        label: "open",
    },
    Action {
        key: "L",
        label: "clear log",
    },
];

const ACTIONS_STOPPED: &[Action] = &[
    Action {
        key: "s",
        label: "start",
    },
    Action {
        key: "e",
        label: "edit",
    },
    Action {
        key: "d",
        label: "delete",
    },
    Action {
        key: "A",
        label: "auto",
    },
];

const ACTIONS_ERROR: &[Action] = &[
    Action {
        key: "R",
        label: "restart",
    },
    Action {
        key: "e",
        label: "edit",
    },
    Action {
        key: "d",
        label: "delete",
    },
    Action {
        key: "L",
        label: "clear log",
    },
];

const ACTIONS_EMPTY: &[Action] = &[
    Action {
        key: "a",
        label: "add a tunnel",
    },
    Action {
        key: "m",
        label: "import",
    },
];

/// Always available, always flush right, and the last thing to shed.
const ACTIONS_PROGRAM: &[Action] = &[
    Action {
        key: ";",
        label: "account",
    },
    Action {
        key: "?",
        label: "keys",
    },
    Action {
        key: "q",
        label: "quit",
    },
];

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

pub fn render(f: &mut Frame, app: &App) {
    // A terminal short enough that the header would cost the list its last row
    // keeps the list: the header says what the app is, the list is the app
    let header_height = if f.area().height >= 16 {
        header_height(f.area().width, f.area().height, &app.account)
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
            Paragraph::new(header_lines(f.area().width, f.area().height, &app.account)),
            main[0],
        );
    }

    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(main[1]);

    render_tunnels(f, app, content[0]);

    let metrics_height = match app.selected_entry() {
        Some(entry) if entry.tunnel.is_expose() => 9,
        Some(_) => 7,
        None => 0,
    };
    if metrics_height == 0 {
        render_logs(f, app, content[1]);
    } else {
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                // Six rows of detail and two of border
                Constraint::Length(8),
                Constraint::Min(0),
                Constraint::Length(metrics_height),
            ])
            .split(content[1]);
        render_details(f, app, right[0]);
        render_logs(f, app, right[1]);
        render_metrics(f, app, right[2]);
    }

    render_help_bar(f, main[3], app);

    match app.input_mode {
        InputMode::Normal => {}
        InputMode::Help => render_help_modal(f),
        InputMode::ChooseMode => render_choose_mode(f),
        InputMode::Confirm => {
            if let Some(message) = &app.confirm_message {
                render_dialog(f, "Confirm", vec![Line::raw(message.clone())], 50, 20);
            }
        }
        InputMode::AddInstance => render_instance_picker(f, app),
        InputMode::AddDomain => render_domain_picker(f, app),
        mode => render_prompt(f, app, mode),
    }
}

fn render_tunnels(f: &mut Frame, app: &App, area: Rect) {
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
                    format!("{:<14}", entry.name()),
                    if i == app.selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!("{:<7}", entry.mode()),
                    Style::default().fg(theme::ACCENT),
                ),
                Span::raw(entry.url()),
            ];
            if entry.tunnel.auto_start {
                spans.push(Span::styled(" ⟳", Style::default().fg(theme::FLAG)));
            }
            if entry.health == HealthStatus::Unhealthy {
                spans.push(Span::styled(" ⚠", Style::default().fg(theme::WARN)));
            }
            if let Some(tag) = entry.tag() {
                spans.push(Span::styled(
                    format!(" {tag}"),
                    Style::default().fg(theme::MUTED),
                ));
            }
            let style = if i == app.selected {
                Style::default().bg(theme::SELECTED_BG)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(spans).style(style))
        })
        .collect();

    let title = format!("Tunnels ({})", app.entries.len());
    let inner = pane(f, area, &title, None);
    f.render_widget(List::new(items), inner);
}

fn render_details(f: &mut Frame, app: &App, area: Rect) {
    let Some(entry) = app.selected_entry() else {
        pane(f, area, "Details", None);
        return;
    };

    let mut lines = vec![
        // The list already reads an unknown port as "-"; the details pane said
        // `localhost:0`, which reads as a port the tunnel actually holds
        detail("url", &entry.url()),
        detail("target", &entry.tunnel.display_target()),
    ];
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
            lines.push(detail(
                "relay",
                &format!("{domain}{}", if *private { " (private)" } else { "" }),
            ));
            lines.push(detail("relay port", &relay_port.to_string()));
        }
    }

    // Status says a tunnel is running; these two say since when, and whether
    // anything brought it up without being asked
    let metrics = entry.metrics.clone().unwrap_or_default();
    lines.push(detail(
        "started",
        &match metrics.uptime {
            Some(up) => format!("{} ago", format_uptime(up)),
            None => "-".to_string(),
        },
    ));
    lines.push(detail(
        "started by",
        &match entry.status {
            TunnelStatus::Running => started_by(metrics.restarts, entry.tunnel.auto_start),
            _ => "-".to_string(),
        },
    ));

    let inner = pane(f, area, "Details", None);
    f.render_widget(Paragraph::new(lines), inner);
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

fn detail<'a>(label: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<11}"), Style::default().fg(theme::MUTED)),
        Span::raw(value.to_string()),
    ])
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

    if annotated.is_empty() {
        if let Some(entry) = app.selected_entry() {
            annotated.push(Line::styled(
                empty_log_note(&entry.tunnel.spec, entry.status),
                Style::default().fg(theme::MUTED),
            ));
        }
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
pub fn wrapped_rows(lines: &[Line], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    lines
        .iter()
        .map(|line| {
            let chars = line.to_string().chars().count().max(1) as u16;
            chars.div_ceil(width)
        })
        .sum()
}

fn render_metrics(f: &mut Frame, app: &App, area: Rect) {
    let inner = pane(f, area, "Metrics", None);

    let Some(entry) = app.selected_entry() else {
        return;
    };
    let metrics = entry.metrics.clone().unwrap_or_default();

    // The four figures worth a glance, on one line. Stacked over three rows
    // they read as a form to be filled in; side by side they read as a gauge,
    // which is what they are. Restarts and health keep their own row because
    // both are answers to "is something wrong", not numbers to watch.
    let mut lines = vec![
        Line::from(vec![
            inline("latency"),
            value(match metrics.latency {
                Some(rtt) => format_latency(rtt),
                None => "-".to_string(),
            }),
            inline("   active"),
            value(metrics.active_connections.to_string()),
            inline("   uptime"),
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
        ]),
        Line::from(vec![
            inline("restarts"),
            value(metrics.restarts.to_string()),
            inline("   health"),
            value(entry.health.symbol().to_string()),
        ]),
    ];

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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(lines.len() as u16), Constraint::Min(0)])
        .split(inner);
    f.render_widget(Paragraph::new(lines), chunks[0]);

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
    selected: Option<TunnelStatus>,
    any_tunnels: bool,
) -> (&'static [Action], &'static [Action]) {
    let contextual = match (selected, any_tunnels) {
        (Some(TunnelStatus::Running), _) => ACTIONS_RUNNING,
        (Some(TunnelStatus::Stopped), _) => ACTIONS_STOPPED,
        (Some(TunnelStatus::Error), _) => ACTIONS_ERROR,
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
        out.push(Span::styled(
            action.key,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
        out.push(Span::raw(" "));
        out.push(Span::styled(
            action.label,
            Style::default().fg(theme::LABEL),
        ));
    }
}

/// The footer at this width: the selection's actions on the left, the program's
/// on the right, and nothing drawn that does not fit whole.
///
/// Shedding order is from the end of the contextual group, because the actions
/// are listed in the order a user reaches for them. Only once that group is
/// empty does `; account` go, leaving the two that must never disappear: `?`
/// says what the keys are, and `q` is the way out.
pub fn footer_line(width: u16, selected: Option<TunnelStatus>, any_tunnels: bool) -> Line<'static> {
    let (all_contextual, all_program) = footer_groups(selected, any_tunnels);
    let w = width as usize;

    let fits = |ctx: &[Action], prog: &[Action]| -> bool {
        // one cell of air at each end, and the gap only exists between groups
        let gap = if ctx.is_empty() { 0 } else { 2 };
        let needed = 2 + group_cells(ctx) + gap + group_cells(prog);
        needed <= w
    };

    let mut shown = all_contextual.len();
    let mut program = all_program;
    while !fits(&all_contextual[..shown], program) {
        if shown > 0 {
            shown -= 1;
        } else if program.len() > 2 {
            program = &all_program[1..];
        } else {
            break;
        }
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
    let selected = app.selected_entry().map(|e| e.status);
    f.render_widget(
        Paragraph::new(footer_line(area.width, selected, !app.entries.is_empty())),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

const HELP_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "NAVIGATION",
        &[
            ("up/k", "Move selection up"),
            ("down/j", "Move selection down"),
            (";", "Cycle through accounts"),
            ("q", "Quit gtunnel"),
            ("Ctrl+Z", "Suspend"),
        ],
    ),
    (
        "TUNNELS",
        &[
            ("a", "Add a tunnel"),
            ("e", "Edit tunnel (target and zone)"),
            ("s", "Start selected tunnel"),
            ("S", "Stop selected tunnel"),
            ("R", "Restart tunnel (rewrites the service)"),
            ("d", "Delete selected tunnel"),
            ("m", "Import an ephemeral run as managed"),
            ("A", "Toggle auto-start on login (⟳ = enabled)"),
        ],
    ),
    (
        "INSPECT",
        &[
            ("c", "Copy tunnel URL to clipboard"),
            ("o", "Open tunnel URL in browser"),
            ("h", "Check tunnel health now"),
            ("L", "Clear this tunnel's log"),
            ("r", "Refresh list, metrics and health"),
        ],
    ),
];

fn render_help_modal(f: &mut Frame) {
    let area = centered_rect(70, 80, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Help - Press Esc to close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = Vec::new();
    for (heading, keys) in HELP_SECTIONS {
        lines.push(Line::styled(
            *heading,
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
        for (key, what) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<9}"), Style::default().fg(theme::ACCENT)),
                Span::raw(*what),
            ]));
        }
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled(
        format!(
            "  Metrics every 5s, health every 30s, load balancer traffic every 60s ({LB_WINDOW_MINUTES}m window)"
        ),
        Style::default().fg(theme::MUTED),
    ));
    lines.push(Line::styled(
        "  A tunnel going unhealthy raises a system notification",
        Style::default().fg(theme::MUTED),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_dialog(f: &mut Frame, title: &str, lines: Vec<Line>, width: u16, height: u16) {
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_choose_mode(f: &mut Frame) {
    render_dialog(
        f,
        "New tunnel",
        vec![
            Line::from(vec![
                Span::styled("  1  ", Style::default().fg(theme::ACCENT)),
                Span::raw("Access - forward a remote port to localhost over IAP"),
            ]),
            Line::from(vec![
                Span::styled("  2  ", Style::default().fg(theme::ACCENT)),
                Span::raw("Public - publish a local service at an HTTPS URL"),
            ]),
            Line::raw(""),
            Line::styled("  Esc to cancel", Style::default().fg(theme::MUTED)),
        ],
        60,
        25,
    );
}

fn render_prompt(f: &mut Frame, app: &App, mode: InputMode) {
    let title = match app.draft.mode {
        TunnelMode::Access => "New access tunnel",
        TunnelMode::Public => "New public URL",
    };
    let title = if mode == InputMode::EditTarget {
        "Edit"
    } else {
        title
    };
    render_dialog(
        f,
        title,
        vec![
            Line::styled(mode.prompt(), Style::default().fg(theme::WARN)),
            Line::raw(""),
            Line::from(vec![
                Span::raw(format!("  {}", app.input)),
                Span::styled("_", Style::default().fg(theme::ACCENT)),
            ]),
            Line::raw(""),
            Line::styled(
                "  Enter to continue, Esc to cancel",
                Style::default().fg(theme::MUTED),
            ),
        ],
        60,
        25,
    );
}

// The prompt, the filter being typed and a blank line, which stay put while
// the rows below them scroll
const PICKER_HEADER_LINES: u16 = 3;

fn picker_header<'a>(
    prompt: &'a str,
    filter: &str,
    selected: usize,
    total: usize,
) -> Vec<Line<'a>> {
    let mut head = vec![Span::styled(prompt, Style::default().fg(theme::WARN))];
    if total > 0 {
        // Says how far down a list that does not fit the selection is
        head.push(Span::styled(
            format!("  {}/{}", selected.min(total - 1) + 1, total),
            Style::default().fg(theme::MUTED),
        ));
    }
    vec![
        Line::from(head),
        Line::from(vec![
            Span::raw(format!("  {filter}")),
            Span::styled("_", Style::default().fg(theme::ACCENT)),
        ]),
        Line::raw(""),
    ]
}

fn picker_rows<'a>(rows: Vec<String>, selected: usize) -> Vec<Line<'a>> {
    if rows.is_empty() {
        return vec![Line::styled(
            "  nothing matches",
            Style::default().fg(theme::MUTED),
        )];
    }
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            let style = if i == selected {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(theme::ACCENT)
            } else {
                Style::default()
            };
            Line::styled(format!("  {row}"), style)
        })
        .collect()
}

struct Picker<'a> {
    title: &'a str,
    prompt: &'a str,
    filter: &'a str,
    rows: Vec<String>,
    selected: usize,
    width: u16,
    height: u16,
}

// A dialog whose row list is taller than the dialog: the header is fixed and
// the rows scroll just enough to keep the selected one on screen
fn render_picker(f: &mut Frame, p: Picker) {
    let Picker {
        title,
        prompt,
        filter,
        rows,
        selected,
        width,
        height,
    } = p;
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(PICKER_HEADER_LINES), Constraint::Min(0)])
        .split(inner);

    let total = rows.len();
    f.render_widget(
        Paragraph::new(picker_header(prompt, filter, selected, total)),
        parts[0],
    );

    let visible = parts[1].height as usize;
    let offset = selected.saturating_sub(visible.saturating_sub(1)) as u16;
    f.render_widget(
        Paragraph::new(picker_rows(rows, selected)).scroll((offset, 0)),
        parts[1],
    );
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
            width: 70,
            height: 60,
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
            width: 60,
            height: 40,
        },
    );
}
