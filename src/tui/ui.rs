use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Sparkline};
use ratatui::Frame;

use super::app::{App, InputMode, TunnelMode};
use crate::probe::{HealthStatus, LB_WINDOW_MINUTES};
use crate::state::{TunnelSpec, TunnelStatus};
use crate::tunnel::diagnose_log_line;

pub const LOG_LINES: usize = 200;

const HELP_BAR: &str =
    " s start  S stop  R restart  d delete  a add  e edit  A auto  c copy  o open  ; account  r refresh  ? help  q quit";

fn panel(title: &str) -> Block<'_> {
    Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
}

fn status_style(status: TunnelStatus) -> Style {
    match status {
        TunnelStatus::Running => Style::default().fg(Color::Green),
        TunnelStatus::Stopped => Style::default().fg(Color::DarkGray),
        TunnelStatus::Error => Style::default().fg(Color::Red),
    }
}

pub fn render(f: &mut Frame, app: &App) {
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(main[0]);

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
                Constraint::Length(6),
                Constraint::Min(0),
                Constraint::Length(metrics_height),
            ])
            .split(content[1]);
        render_details(f, app, right[0]);
        render_logs(f, app, right[1]);
        render_metrics(f, app, right[2]);
    }

    render_status_line(f, app, main[1]);
    render_help_bar(f, main[2]);

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
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(entry.url()),
            ];
            if entry.tunnel.auto_start {
                spans.push(Span::styled(" ⟳", Style::default().fg(Color::Blue)));
            }
            if entry.health == HealthStatus::Unhealthy {
                spans.push(Span::styled(" ⚠", Style::default().fg(Color::Yellow)));
            }
            if let Some(tag) = entry.tag() {
                spans.push(Span::styled(
                    format!(" {tag}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            let style = if i == app.selected {
                Style::default().bg(Color::Indexed(236))
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
        detail("url", &entry.tunnel.display_url()),
        detail("target", &entry.tunnel.display_target()),
    ];
    match &entry.tunnel.spec {
        TunnelSpec::Access {
            target, local_port, ..
        } => {
            lines.push(detail("zone", &access_zone(target)));
            lines.push(detail("local port", &local_port.to_string()));
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

    f.render_widget(Paragraph::new(lines).block(panel("Details")), area);
}

fn access_zone(target: &crate::state::AccessTarget) -> String {
    match target {
        crate::state::AccessTarget::Instance { zone, .. } => zone.clone(),
        crate::state::AccessTarget::DestGroup { region, .. } => region.clone(),
    }
}

fn detail<'a>(label: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<11}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
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
                Style::default().fg(Color::Yellow),
            ));
        }
    }

    f.render_widget(Paragraph::new(annotated).block(panel(&title)), area);
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
            Span::raw(match metrics.latency_ms {
                Some(ms) => format!("{ms} ms"),
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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(lines.len() as u16), Constraint::Min(0)])
        .split(inner);
    f.render_widget(Paragraph::new(lines), chunks[0]);

    let samples = app.selected_sparkline();
    if !samples.is_empty() {
        f.render_widget(
            Sparkline::default()
                .data(samples)
                .style(Style::default().fg(Color::Cyan)),
            chunks[1],
        );
    }
}

fn field(label: &str) -> Span<'static> {
    Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray))
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
        Style::default().fg(Color::Black).bg(Color::Cyan),
    )];
    if app.spinner.is_running() {
        spans.push(Span::styled(
            format!(" {} ", app.spinner.frame()),
            Style::default().fg(Color::Cyan),
        ));
        if let Some(message) = &app.spinner.message {
            spans.push(Span::raw(message.clone()));
        }
    } else if let Some(status) = &app.status {
        spans.push(Span::raw(format!(" {status}")));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_help_bar(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::styled(HELP_BAR, Style::default().fg(Color::DarkGray))),
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
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = Vec::new();
    for (heading, keys) in HELP_SECTIONS {
        lines.push(Line::styled(
            *heading,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
        for (key, what) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<9}"), Style::default().fg(Color::Cyan)),
                Span::raw(*what),
            ]));
        }
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled(
        format!(
            "  Metrics every 5s, health every 30s, load balancer traffic every 60s ({LB_WINDOW_MINUTES}m window)"
        ),
        Style::default().fg(Color::DarkGray),
    ));
    lines.push(Line::styled(
        "  A tunnel going unhealthy raises a system notification",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_dialog(f: &mut Frame, title: &str, lines: Vec<Line>, width: u16, height: u16) {
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
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
                Span::styled("  1  ", Style::default().fg(Color::Cyan)),
                Span::raw("Access - forward a remote port to localhost over IAP"),
            ]),
            Line::from(vec![
                Span::styled("  2  ", Style::default().fg(Color::Cyan)),
                Span::raw("Public - publish a local service at an HTTPS URL"),
            ]),
            Line::raw(""),
            Line::styled("  Esc to cancel", Style::default().fg(Color::DarkGray)),
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
            Line::styled(mode.prompt(), Style::default().fg(Color::Yellow)),
            Line::raw(""),
            Line::from(vec![
                Span::raw(format!("  {}", app.input)),
                Span::styled("_", Style::default().fg(Color::Cyan)),
            ]),
            Line::raw(""),
            Line::styled(
                "  Enter to continue, Esc to cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ],
        60,
        25,
    );
}

fn picker_lines<'a>(
    prompt: &'a str,
    filter: &str,
    rows: Vec<String>,
    selected: usize,
) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::styled(prompt, Style::default().fg(Color::Yellow)),
        Line::from(vec![
            Span::raw(format!("  {filter}")),
            Span::styled("_", Style::default().fg(Color::Cyan)),
        ]),
        Line::raw(""),
    ];
    if rows.is_empty() {
        lines.push(Line::styled(
            "  nothing matches",
            Style::default().fg(Color::DarkGray),
        ));
    }
    for (i, row) in rows.into_iter().enumerate() {
        let style = if i == selected {
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan)
        } else {
            Style::default()
        };
        lines.push(Line::styled(format!("  {row}"), style));
    }
    lines
}

fn render_instance_picker(f: &mut Frame, app: &App) {
    let rows: Vec<String> = app
        .filtered_instances()
        .iter()
        .map(|i| format!("{:<30} {}", i.name, i.zone))
        .collect();
    render_dialog(
        f,
        "Instance",
        picker_lines(
            InputMode::AddInstance.prompt(),
            &app.input,
            rows,
            app.instance_selected,
        ),
        70,
        60,
    );
}

fn render_domain_picker(f: &mut Frame, app: &App) {
    render_dialog(
        f,
        "Domain",
        picker_lines(
            InputMode::AddDomain.prompt(),
            "",
            app.domains.clone(),
            app.domain_selected,
        ),
        60,
        40,
    );
}
