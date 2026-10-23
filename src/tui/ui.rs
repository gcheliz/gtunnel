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

/// The mark, and the name beneath it. Two rows where the block lettering took
/// four: a header is a label, not a splash screen, and every row it takes comes
/// off the panes below.
pub const HEADER_HEIGHT_FULL: u16 = 2;
/// One row, for a terminal too narrow to hold the lockup without breaking it.
pub const HEADER_HEIGHT_COMPACT: u16 = 1;

/// The lockup is eleven cells, not ten. `gtunnel` is seven, and 11 - 7 = 4
/// leaves two blank cells on each side; a ten-cell mark leaves three, and no
/// terminal can split a cell. The mark is mirror-symmetric about its centre
/// column, so the centring reads as deliberate rather than as a near miss.
pub const MARK_WIDTH: u16 = 11;

/// The name, padded to `MARK_WIDTH` so the two rows centre on one another.
pub const NAME_ROW: &str = "  gtunnel  ";

/// A cloud resting on a line. Braille because a braille cell carries 2x4 dots,
/// which is the only way to draw a curve inside a single terminal row.
const MARK_BRAILLE: &str = "⠤⠤⢤⣶⣾⣿⣷⣶⡤⠤⠤";

/// The same shape for a font with no braille coverage. Eleven cells again, so
/// a missing glyph changes how the mark looks but never where the name sits.
const MARK_FALLBACK: &str = "──▗▄▟▀▙▄▖──";

/// The large form, where the mark is the subject rather than a label. The
/// program never draws it — only the README does — so it lives here as the
/// single copy both are checked against, and the test is what ties them
/// together. Three rows, 21 cells.
#[cfg(test)]
pub const LARGE_MARK: [&str; 3] = [
    "⠀⠀⠀⠀⠀⠀⠀⠀⣠⣴⣶⣦⣄⠀⠀⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⢀⣴⣾⣶⣿⣿⣿⣿⣿⣶⣷⣦⡀⠀⠀⠀⠀",
    "⠤⠤⠤⠤⣿⣿⡏⠉⠉⠉⠉⠉⠉⠉⢹⣿⣿⠤⠤⠤⠤",
];

/// The large form's name row, padded to its 21 cells.
#[cfg(test)]
pub const LARGE_NAME_ROW: &str = "       gtunnel       ";

/// Which hue each cell of the mark carries. The line runs off both ends in the
/// same colour, and the four brand hues band the cloud left to right.
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
const ACCOUNT_LABEL: &str = " account ";

/// The mark as one row of coloured spans. Consecutive cells sharing a hue are
/// merged so a row costs five spans rather than eleven.
pub fn mark_spans(braille: bool) -> Vec<Span<'static>> {
    let art = if braille { MARK_BRAILLE } else { MARK_FALLBACK };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut hue: Option<Color> = None;
    for (cell, ch) in art.chars().enumerate() {
        let next = MARK_BANDS[cell];
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

/// How many cells the account field occupies: the label, the name, and the cell
/// of padding on each side that makes it read as a field and not as more text.
fn account_field_width(account: &str) -> usize {
    ACCOUNT_LABEL.chars().count() + account.chars().count() + 2
}

/// The lockup with the subtitle and the account beside it, or one line of the
/// same facts when the terminal is too narrow for the lockup to survive.
///
/// The account field has priority over the subtitle: it is the one piece of
/// state that changes what every other key acts on, so it is the last thing to
/// go, and when even it will not fit the header falls to the compact form.
pub fn header_lines(width: u16, account: &str) -> Vec<Line<'static>> {
    let w = width as usize;
    let field = account_field_width(account);

    if w < MARK_WIDTH as usize + 1 + field {
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

    const SUBTITLE: &str = "IAP tunnel manager";
    let mut spans = mark_spans(true);
    let mut used = MARK_WIDTH as usize + field;

    // The subtitle appears only when it fits whole, with two cells of air on
    // each side. A half-drawn subtitle is worse than none.
    let subtitle_cost = SUBTITLE.chars().count() + 4;
    if w >= used + subtitle_cost {
        spans.push(Span::styled(
            format!("  {SUBTITLE}"),
            Style::default().fg(theme::MUTED),
        ));
        used += SUBTITLE.chars().count() + 2;
    }

    // Whatever is left pushes the field flush against the right edge.
    spans.push(Span::raw(" ".repeat(w - used)));
    spans.push(Span::styled(
        ACCOUNT_LABEL,
        Style::default().fg(theme::MUTED),
    ));
    spans.push(Span::styled(
        format!(" {account} "),
        Style::default()
            .bg(theme::ACCOUNT_BG)
            .fg(theme::ACCOUNT_FG)
            .add_modifier(Modifier::BOLD),
    ));

    vec![
        Line::from(spans),
        Line::from(Span::styled(
            NAME_ROW,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
    ]
}

/// How many rows the header takes at this width. Derived from `header_lines`
/// rather than recomputed, because a layout that reserves a different number of
/// rows than the header draws is a bug with no symptom until it clips.
pub fn header_height(width: u16, account: &str) -> u16 {
    match header_lines(width, account).len() {
        1 => HEADER_HEIGHT_COMPACT,
        _ => HEADER_HEIGHT_FULL,
    }
}

const HELP_BAR: &str =
    " s start  S stop  R restart  d delete  a add  e edit  A auto  c copy  o open  L clear log  ; account  r refresh  ? help  q quit";

fn panel(title: &str) -> Block<'_> {
    Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
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
        header_height(f.area().width, &app.account)
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

    if header_height > 0 {
        f.render_widget(
            Paragraph::new(header_lines(f.area().width, &app.account)),
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

    render_status_line(f, app, main[2]);
    render_help_bar(f, main[3]);

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
            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();

    let title = format!("Tunnels ({})", app.entries.len());
    f.render_widget(List::new(items).block(panel(&title)), area);
}

fn render_details(f: &mut Frame, app: &App, area: Rect) {
    let Some(entry) = app.selected_entry() else {
        f.render_widget(panel("Details"), area);
        return;
    };

    let mut lines = vec![
        // The list already reads an unknown port as "-"; the details pane said
        // `localhost:0`, which reads as a port the tunnel actually holds
        detail("url", &entry.url()),
        detail("target", &entry.tunnel.display_target()),
    ];
    match &entry.tunnel.spec {
        TunnelSpec::Access {
            target, local_port, ..
        } => {
            lines.push(detail("zone", &access_zone(target)));
            lines.push(detail(
                "local port",
                &match local_port {
                    0 => "-".to_string(),
                    port => port.to_string(),
                },
            ));
        }
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

    f.render_widget(Paragraph::new(lines).block(panel("Details")), area);
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

fn access_zone(target: &crate::state::AccessTarget) -> String {
    match target {
        crate::state::AccessTarget::Instance { zone, .. } => zone.clone(),
        crate::state::AccessTarget::Jump { zone, .. } => zone.clone(),
        crate::state::AccessTarget::DestGroup { region, .. } => region.clone(),
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

fn render_logs(f: &mut Frame, app: &App, area: Rect) {
    let title = match app.selected_entry() {
        Some(entry) => format!("Logs: {}", entry.name()),
        None => "Logs".to_string(),
    };

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
    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2);
    let rows = wrapped_rows(&annotated, inner_width);
    let scroll = rows.saturating_sub(inner_height);
    f.render_widget(
        Paragraph::new(annotated)
            .block(panel(&title))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
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
    let block = panel("Metrics");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(entry) = app.selected_entry() else {
        return;
    };
    let metrics = entry.metrics.clone().unwrap_or_default();

    let mut lines = vec![
        Line::from(vec![
            field("Latency"),
            Span::raw(match metrics.latency {
                Some(rtt) => format_latency(rtt),
                None => "-".to_string(),
            }),
            field("  Active"),
            Span::raw(metrics.active_connections.to_string()),
        ]),
        Line::from(vec![
            field("Uptime"),
            Span::raw(match metrics.uptime {
                Some(up) => format_uptime(up),
                None => "-".to_string(),
            }),
            field("  Restarts"),
            Span::raw(metrics.restarts.to_string()),
            field("  Health"),
            Span::raw(entry.health.symbol().to_string()),
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

    // A sparkline carries no scale of its own, so the tallest bar is named
    // here rather than on a row of its own, which the bars need
    if !entry.history.is_empty() {
        lines.push(Line::from(vec![
            field("Peak"),
            Span::styled(
                format_latency(std::time::Duration::from_micros(entry.history.peak())),
                Style::default().fg(theme::MUTED),
            ),
        ]));
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
    Span::styled(format!("{label}: "), Style::default().fg(theme::MUTED))
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

fn render_status_line(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        format!(" {} ", app.account),
        Style::default().fg(theme::ON_ACCENT).bg(theme::ACCENT),
    )];
    if app.spinner.is_running() {
        spans.push(Span::styled(
            format!(" {} ", app.spinner.frame()),
            Style::default().fg(theme::ACCENT),
        ));
        if let Some(message) = &app.spinner.message {
            spans.push(Span::raw(message.clone()));
        }
    } else if let Some(status) = &app.status {
        let style = if app.status_error {
            Style::default().fg(theme::BAD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(format!(" {status}"), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_help_bar(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::styled(HELP_BAR, Style::default().fg(theme::MUTED))),
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
