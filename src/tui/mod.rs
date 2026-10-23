pub mod app;
pub mod demo;
pub mod ops;
pub mod theme;
pub mod ui;

use anyhow::Result;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[allow(unused_imports)]
pub use app::{App, EditTunnel, NewTunnel, Op, OpResult, TunnelEntry, TunnelKind};

use crate::config::load_config;
use crate::probe::HealthStatus;

const TICK: Duration = Duration::from_millis(120);

// What the event loop reacts to; everything slow arrives as a message
enum Msg {
    Key(crossterm::event::KeyEvent),
    Instances(Vec<app::InstanceChoice>),
    Tick,
    Entries(Vec<TunnelEntry>),
    Metrics(Vec<(String, crate::probe::TunnelMetrics)>),
    Health(Vec<(String, HealthStatus)>),
    Logs(String, Vec<String>),
    OpDone(OpResult),
}

pub async fn run_tui(initial_account: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let account = config.get_account(initial_account)?.name.clone();

    let mut app = App::new(&account);
    app.accounts = config.accounts.iter().map(|a| a.name.clone()).collect();
    set_relay(&mut app, &config);

    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    spawn_input(tx.clone());
    spawn_ticker(tx.clone());

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &tx, &mut rx).await;
    ratatui::restore();
    result
}

// The Public flow and the domain picker only make sense with a relay
fn set_relay(app: &mut App, config: &crate::config::Config) {
    let expose = config
        .accounts
        .iter()
        .find(|a| a.name == app.account)
        .and_then(|a| a.expose.as_ref());
    app.has_relay = expose.is_some();
    app.domains = expose
        .map(|e| e.domains.iter().map(|d| d.domain.clone()).collect())
        .unwrap_or_default();
}

pub async fn run_demo() -> Result<()> {
    let mut app = App::new_demo();
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    spawn_input(tx.clone());
    spawn_ticker(tx.clone());

    let mut terminal = ratatui::init();
    let result = demo_loop(&mut terminal, &mut app, &mut rx).await;
    ratatui::restore();
    result
}

// The demo draws and reacts to keys, but never schedules work
async fn demo_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
) -> Result<()> {
    terminal.draw(|f| ui::render(f, app))?;
    while let Some(msg) = rx.recv().await {
        match msg {
            Msg::Key(key) => {
                if let Some(op) = app.handle_key(key) {
                    app.apply(OpResult {
                        message: format!("demo: {op:?} does nothing here"),
                        ok: true,
                        reload: false,
                    });
                }
                if app.should_quit {
                    break;
                }
            }
            Msg::Tick => {
                app.force_refresh = false;
                if app.spinner.is_running() {
                    app.spinner.tick();
                }
            }
            _ => {}
        }
        terminal.draw(|f| ui::render(f, app))?;
    }
    Ok(())
}

fn spawn_input(tx: mpsc::UnboundedSender<Msg>) {
    // Terminal reads block, so they live on their own thread
    std::thread::spawn(move || loop {
        if crossterm::event::poll(Duration::from_millis(200)).unwrap_or(false) {
            match crossterm::event::read() {
                Ok(crossterm::event::Event::Key(key))
                    if key.kind == crossterm::event::KeyEventKind::Press =>
                {
                    if tx.send(Msg::Key(key)).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
}

fn spawn_ticker(tx: mpsc::UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        loop {
            interval.tick().await;
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        }
    });
}

struct Timers {
    entries: Instant,
    metrics: Instant,
    health: Instant,
    lb: Instant,
}

impl Timers {
    // Every timer starts due, so the first frame is not empty
    fn due() -> Self {
        let past = Instant::now() - Duration::from_secs(3600);
        Self {
            entries: past,
            metrics: past,
            health: past,
            lb: past,
        }
    }

    fn force(&mut self) {
        *self = Self::due();
    }
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    tx: &mpsc::UnboundedSender<Msg>,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
) -> Result<()> {
    let mut timers = Timers::due();
    let mut logs_for: Option<String> = None;

    terminal.draw(|f| ui::render(f, app))?;

    while let Some(msg) = rx.recv().await {
        match msg {
            Msg::Key(key) => {
                if let Some(op) = app.handle_key(key) {
                    let account = app.account.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(Msg::OpDone(ops::execute(&account, op).await));
                    });
                }
                if app.should_quit {
                    break;
                }
                if app.suspend {
                    app.suspend = false;
                    suspend(terminal)?;
                    timers.force();
                }
            }
            Msg::Tick => {
                if app.spinner.is_running() {
                    app.spinner.tick();
                }
                if app.force_refresh {
                    app.force_refresh = false;
                    timers.force();
                    logs_for = None;
                }
                if app.want_instances {
                    app.want_instances = false;
                    let account = app.account.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        if let Ok(instances) = ops::load_instances(&account).await {
                            let _ = tx.send(Msg::Instances(instances));
                        }
                    });
                }
                schedule(app, &mut timers, tx);
                // The logs panel follows the selection, so it reloads with it
                let selected = app.selected_entry().map(|e| e.tunnel.clone());
                if let Some(tunnel) = selected {
                    if logs_for.as_deref() != Some(tunnel.name.as_str()) || timers_logs_due(&timers)
                    {
                        logs_for = Some(tunnel.name.clone());
                        let tx = tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let lines = ops::read_logs(&tunnel, ui::LOG_LINES);
                            let _ = tx.send(Msg::Logs(tunnel.name.clone(), lines));
                        });
                    }
                }
            }
            Msg::Entries(entries) => app.replace_entries(entries),
            Msg::Metrics(metrics) => {
                for (name, m) in metrics {
                    if let Some(entry) = app.entries.iter_mut().find(|e| e.name() == name) {
                        // Microseconds: a fast hop is a real measurement, and
                        // whole milliseconds would record it as nothing
                        entry
                            .history
                            .record(m.latency.map_or(0, |d| d.as_micros() as u64));
                        entry.metrics = Some(m);
                    }
                }
            }
            Msg::Health(results) => {
                for (name, health) in results {
                    let Some(entry) = app.entries.iter_mut().find(|e| e.name() == name) else {
                        continue;
                    };
                    announce(entry.name(), entry.health, health);
                    entry.health = health;
                }
            }
            Msg::Logs(name, lines) => {
                if app.selected_entry().map(|e| e.name().to_string()) == Some(name) {
                    app.logs = lines;
                }
            }
            Msg::Instances(instances) => {
                app.instances = instances;
                app.instance_selected = 0;
            }
            Msg::OpDone(result) => {
                app.apply(result);
                // The CLI commands this ran print to the same terminal
                terminal.clear()?;
            }
        }
        terminal.draw(|f| ui::render(f, app))?;
    }
    Ok(())
}

fn timers_logs_due(timers: &Timers) -> bool {
    timers.metrics.elapsed() < Duration::from_millis(200)
}

// A tunnel that just went bad, or just recovered, is worth interrupting for
fn announce(name: &str, was: HealthStatus, now: HealthStatus) {
    match (was, now) {
        (HealthStatus::Healthy, HealthStatus::Unhealthy) => {
            crate::notify::notify("gtunnel", &format!("{name} is unhealthy"));
        }
        (HealthStatus::Unhealthy, HealthStatus::Healthy) => {
            crate::notify::notify("gtunnel", &format!("{name} is healthy again"));
        }
        _ => {}
    }
}

fn schedule(app: &App, timers: &mut Timers, tx: &mpsc::UnboundedSender<Msg>) {
    let account = app.account.clone();

    if timers.entries.elapsed() >= app::DISCOVERY_EVERY {
        timers.entries = Instant::now();
        let tx = tx.clone();
        let account = account.clone();
        tokio::spawn(async move {
            if let Ok(entries) = ops::load_entries(&account).await {
                let _ = tx.send(Msg::Entries(entries));
            }
        });
    }

    if timers.metrics.elapsed() >= app::METRICS_EVERY {
        timers.metrics = Instant::now();
        let include_lb = timers.lb.elapsed() >= app::LB_EVERY;
        if include_lb {
            timers.lb = Instant::now();
        }
        let targets = ops::metric_targets(&app.entries);
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut out = Vec::new();
            for target in targets {
                let name = target.tunnel.name.clone();
                if let Some(m) = ops::collect(&target, include_lb).await {
                    out.push((name, m));
                }
            }
            let _ = tx.send(Msg::Metrics(out));
        });
    }

    if timers.health.elapsed() >= app::HEALTH_EVERY {
        timers.health = Instant::now();
        // Every running tunnel is probed at once; one slow host must not delay
        // the rest
        let targets: Vec<_> = app
            .entries
            .iter()
            .filter(|e| e.status == crate::state::TunnelStatus::Running)
            .map(|e| (e.name().to_string(), e.tunnel.clone()))
            .collect();
        let tx = tx.clone();
        tokio::spawn(async move {
            let probes = targets
                .into_iter()
                .map(|(name, tunnel)| async move { (name, ops::probe_health(&tunnel).await) });
            let results = futures_join(probes).await;
            let _ = tx.send(Msg::Health(results));
        });
    }
}

// All probes in flight together, without pulling in a futures crate
async fn futures_join<F, T>(tasks: impl IntoIterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handles: Vec<_> = tasks.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::new();
    for handle in handles {
        if let Ok(value) = handle.await {
            out.push(value);
        }
    }
    out
}

#[cfg(unix)]
fn suspend(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    ratatui::restore();
    // SAFETY: raising SIGTSTP on our own process group is what Ctrl+Z does
    unsafe {
        libc::raise(libc::SIGTSTP);
    }
    *terminal = ratatui::init();
    terminal.clear()?;
    Ok(())
}

#[cfg(not(unix))]
fn suspend(_terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::app::*;
    use super::*;
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn access(name: &str, local_port: u16) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "my-vm".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port,
            },
        }
    }

    fn expose(name: &str, hostname: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Expose {
                local_target: "localhost:3000".to_string(),
                domain: "example.com".to_string(),
                hostname: hostname.to_string(),
                relay_port: 30001,
                private: false,
            },
        }
    }

    fn app_with_two() -> App {
        let mut app = App::new("dev");
        app.entries = vec![
            TunnelEntry::new(
                access("db", 15432),
                TunnelStatus::Running,
                TunnelKind::Managed,
            ),
            TunnelEntry::new(
                expose("api", "api.example.com"),
                TunnelStatus::Running,
                TunnelKind::Managed,
            ),
        ];
        app
    }

    #[test]
    fn j_moves_down_and_stop_acts_on_the_selected_tunnel() {
        let mut app = app_with_two();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('S'))),
            Some(Op::Stop("db".to_string()))
        );
        assert_eq!(app.handle_key(key(KeyCode::Char('j'))), None);
        assert_eq!(app.selected, 1);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('S'))),
            Some(Op::Stop("api".to_string()))
        );
    }

    // A log that filled with a previous run's gcloud tracebacks is noise the
    // user can clear from the row it belongs to, and the panel empties with it
    // rather than showing lines the file no longer holds
    #[test]
    fn l_clears_the_selected_tunnels_log_and_the_panel_with_it() {
        let mut app = app_with_two();
        app.logs = vec!["ERROR: [243] Connection reset by peer".to_string()];
        assert_eq!(
            app.handle_key(key(KeyCode::Char('L'))),
            Some(Op::ClearLog("db".to_string()))
        );
        assert!(app.logs.is_empty(), "{:?}", app.logs);
    }

    // An ephemeral run writes to the terminal that started it, so there is no
    // file to clear and saying so beats a silent no-op
    #[test]
    fn clearing_the_log_of_a_run_says_it_has_none() {
        let mut app = app_with_two();
        app.entries[0].kind = TunnelKind::Ephemeral;
        assert_eq!(app.handle_key(key(KeyCode::Char('L'))), None);
        let status = app.status.clone().expect("a status message");
        assert!(status.contains("log"), "{status}");
    }

    #[test]
    fn an_external_tunnel_refuses_every_action_and_says_why() {
        let mut app = app_with_two();
        app.entries[0].kind = TunnelKind::External;
        assert_eq!(app.handle_key(key(KeyCode::Char('s'))), None);
        let status = app.status.clone().expect("a status message");
        assert!(
            status.contains("not managed by gtunnel") && status.contains("start"),
            "{status}"
        );
    }

    // A failed operation and a successful one both end up on the status line,
    // so the line has to say which one it is
    #[test]
    fn a_failed_operation_marks_the_status_line() {
        let mut app = app_with_two();
        app.apply(OpResult {
            message: "permission denied".to_string(),
            ok: false,
            reload: false,
        });
        assert_eq!(app.status.as_deref(), Some("permission denied"));
        assert!(app.status_error, "a failure must be marked");

        app.apply(OpResult {
            message: "db starting".to_string(),
            ok: true,
            reload: true,
        });
        assert!(!app.status_error, "success must clear the mark");
    }

    // A hop that answers in under a millisecond is a measurement, not a zero:
    // whole milliseconds reported every local tunnel as 0 ms, which left the
    // chart with nothing to draw and the peak reading 0
    #[test]
    fn a_sub_millisecond_round_trip_is_not_reported_as_zero() {
        assert_eq!(ui::format_latency(Duration::from_micros(400)), "0.4 ms");
        assert_eq!(ui::format_latency(Duration::from_micros(1500)), "1.5 ms");
        assert_eq!(ui::format_latency(Duration::from_millis(340)), "340 ms");
    }

    // A gcloud error is a paragraph, not a line: clipped at the pane's width it
    // loses the half that names the cause, so the pane wraps and the count of
    // rows a wrapped line takes is what scrolls it to the newest one
    #[test]
    fn a_log_line_too_wide_for_the_pane_counts_as_the_rows_it_wraps_to() {
        use ratatui::text::Line;
        let lines = vec![Line::raw("0123456789"), Line::raw(""), Line::raw("abc")];
        assert_eq!(ui::wrapped_rows(&lines, 4), 3 + 1 + 1);
        // A pane with no width to speak of must not divide by zero
        assert_eq!(ui::wrapped_rows(&lines, 0), 3);
    }

    // "Running" alone cannot answer the question the user actually asks, which is
    // whether anything started this tunnel without being told to
    #[test]
    fn details_distinguish_a_launchd_revival_from_a_start() {
        // RuntimeInfo already reports runs minus the one gtunnel asked for
        assert_eq!(ui::started_by(0, false), "gtunnel (auto-start off)");
        assert_eq!(ui::started_by(0, true), "gtunnel (auto-start on)");
        // Any run past the first is the service manager reviving a crash
        let revived = ui::started_by(2, false);
        assert!(revived.contains("after 2 restarts"), "{revived}");
        assert!(revived.contains("auto-start off"), "{revived}");
    }

    // A jump tunnel runs `ssh -N`, which prints nothing while it works, so an
    // empty pane is the healthy case and must not read as a broken one
    #[test]
    fn an_empty_log_says_why_it_is_empty() {
        use crate::state::{AccessTarget, TunnelSpec, TunnelStatus};
        let jump = TunnelSpec::Access {
            target: AccessTarget::Jump {
                instance: "bastion".to_string(),
                zone: "europe-west1-b".to_string(),
                host: "10.3.0.13".to_string(),
            },
            remote_port: 443,
            local_port: 8443,
        };
        assert!(ui::empty_log_note(&jump, TunnelStatus::Running).contains("silent"));
        // Review Focus 5: a failed tunnel is not a quiet healthy one
        assert!(!ui::empty_log_note(&jump, TunnelStatus::Error).contains("silent"));
    }

    // The brand hues are the one exception to the named-ANSI contract. The
    // account field's half of that contract is asserted where the field is
    // introduced, in the header tests.
    #[test]
    fn the_logo_is_the_only_truecolor_in_the_theme() {
        use ratatui::style::Color;
        assert_eq!(theme::LOGO_BLUE, Color::Rgb(0x42, 0x85, 0xF4));
        assert_eq!(theme::LOGO_RED, Color::Rgb(0xEA, 0x43, 0x35));
        assert_eq!(theme::LOGO_YELLOW, Color::Rgb(0xFB, 0xBC, 0x04));
        assert_eq!(theme::LOGO_GREEN, Color::Rgb(0x34, 0xA8, 0x53));
        assert_eq!(theme::LOGO_LINE, Color::Rgb(0x4D, 0xD0, 0xE1));
    }

    // The mark is fixed artwork, not a parameter. It has been redrawn larger
    // three times and come out wrong three times, for a reason that does not
    // go away: braille draws separated dots, so scaling the raster scales the
    // gaps with it and the cloud reads as a dot matrix, while the word under
    // it cannot scale at all. This test pins the exact cells so the next
    // attempt fails here instead of on screen.
    #[test]
    fn the_mark_is_the_one_drawn_for_the_design_and_never_another() {
        assert_eq!(ui::MARK_ROWS, 1, "the mark is one row and stays one row");
        assert_eq!(ui::MARK_WIDTH, 11);
        let drawn: Vec<u32> = ui::mark_spans(true, 0)
            .iter()
            .flat_map(|s| s.content.chars())
            .map(|c| c as u32)
            .collect();
        assert_eq!(
            drawn,
            vec![
                0x2824, 0x2824, 0x28a4, 0x28f6, 0x28fe, 0x28ff, 0x28f7, 0x28f6, 0x2864, 0x2824,
                0x2824,
            ],
            "the mark must be the cloud from the design, cell for cell"
        );
        assert_eq!(ui::NAME_ROW, "  gtunnel  ");
    }

    // The lockup only centres if the mark and the name are the same number of
    // cells. A terminal has no half cells, so this is an equality, not a target.
    #[test]
    fn the_lockup_rows_are_the_same_width() {
        for row in 0..ui::MARK_ROWS {
            let mark: String = ui::mark_spans(true, row)
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(mark.chars().count(), ui::MARK_WIDTH as usize, "row {row}");
        }
        assert_eq!(ui::NAME_ROW.chars().count(), ui::MARK_WIDTH as usize);
        let fallback: String = ui::mark_spans(false, 0)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            fallback.chars().count(),
            ui::MARK_WIDTH as usize,
            "the fallback must not change the lockup width"
        );
    }

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    // Activity used to sit on a row of its own above the footer, where it
    // crowded the keymap and said nothing about which tunnel it concerned. It
    // belongs on the log it describes.
    #[test]
    fn activity_rides_on_the_log_pane_title() {
        assert_eq!(ui::logs_title(Some("k8s-dev"), None), "Logs: k8s-dev");
        assert_eq!(
            ui::logs_title(Some("k8s-dev"), Some("\u{280b} starting")),
            "Logs: k8s-dev \u{2014} \u{280b} starting"
        );
        // with nothing selected there is still a pane, and still activity
        assert_eq!(ui::logs_title(None, None), "Logs");
        assert_eq!(
            ui::logs_title(None, Some("importing")),
            "Logs \u{2014} importing"
        );
    }

    // `App::status` is set when an operation finishes and never cleared, so a
    // title that showed it kept saying "start k8s-dev" long after k8s-dev had
    // started. A title is standing chrome, not a log line.
    #[test]
    fn the_log_title_stops_saying_starting_once_it_has_started() {
        // while the operation is in flight
        assert_eq!(
            ui::activity_text(Some("\u{280b} start k8s-dev"), None, false).as_deref(),
            Some("\u{280b} start k8s-dev")
        );
        // it finished: the title going back to naming the pane is the signal
        assert_eq!(
            ui::activity_text(None, Some("started k8s-dev"), false),
            None
        );
        // a failure stays, because an error that clears itself is one nobody saw
        assert_eq!(
            ui::activity_text(None, Some("could not start k8s-dev"), true).as_deref(),
            Some("could not start k8s-dev")
        );
        // and the spinner wins over a stale error while the next op runs
        assert_eq!(
            ui::activity_text(Some("\u{280b} stop k8s-dev"), Some("it failed"), true).as_deref(),
            Some("\u{280b} stop k8s-dev")
        );
    }

    // The account is the one piece of state that changes what every other key
    // acts on, so it is pinned to the corner where nothing else can push it.
    #[test]
    fn the_account_sits_flush_right_and_never_touches_the_logo() {
        let lines = ui::header_lines(120, 24, "dev");
        assert_eq!(lines.len() as u16, ui::HEADER_HEIGHT_FULL);
        // The box is three rows tall and opens on the row above the mark, so
        // it is centred on the lockup rather than hanging off its top.
        let top = line_text(&lines[0]);
        assert!(top.trim_end().ends_with('\u{256e}'), "{top:?}");
        // The two caption lines are as tall as the lockup: the platform on the
        // mark's row, the subtitle on the name's row, both in the same column.
        let platform = line_text(&lines[1]);
        let subtitle = line_text(&lines[2]);
        let col = |t: &str, needle: &str| t.find(needle).map(|i| t[..i].chars().count());
        assert!(
            col(&platform, "Google Cloud Platform").is_some(),
            "{platform:?}"
        );
        assert_eq!(
            col(&platform, "Google Cloud Platform"),
            col(&subtitle, "IAP tunnel manager"),
            "{platform:?} / {subtitle:?}"
        );
        assert!(line_text(&lines[2]).trim_end().ends_with('\u{256f}'));
        // a blank row of padding below the lockup
        assert_eq!(line_text(&lines[1 + ui::MARK_ROWS + 1]), "");
        let row0 = line_text(&lines[1]);
        // One cell of margin at each end, the same the footer and the panes
        // have, so the field stops a cell short of the edge rather than on it.
        assert!(row0.ends_with(" ACCOUNT  dev \u{2502} "), "{row0:?}");
        assert!(row0.starts_with(' '), "{row0:?}");
        assert_eq!(row0.chars().count(), 120);
        // The captions are separated from the mark by a divider, not by air
        assert!(row0.contains("\u{2502}  Google Cloud Platform"), "{row0:?}");
        // and that divider runs the lockup's whole height: the mark's lower
        // row and the name both carry it, so it does not stop halfway.
        for row in [1 + ui::MARK_ROWS - 1, 1 + ui::MARK_ROWS] {
            let text = line_text(&lines[row]);
            // Cells, not bytes: a braille cell is three bytes, so `find`
            // would put the divider past the mark on the row that has one.
            let divider = text
                .chars()
                .position(|c| c == '\u{2502}')
                .expect("the divider runs every row");
            assert!(
                divider < ui::MARK_WIDTH as usize + 6,
                "the divider belongs to the lockup, not the account box: {text:?}"
            );
        }
        // the name sits under the mark, letterspaced to the mark's own width
        assert!(line_text(&lines[1 + ui::MARK_ROWS]).contains(ui::NAME_ROW));
        // the rule closes the header off, inset to the same margin
        let rule = line_text(&lines[ui::HEADER_HEIGHT_FULL as usize - 1]);
        assert_eq!(rule.chars().count(), 120);
        assert_eq!(rule.matches('\u{2500}').count(), 118);
    }

    // Review Focus 3: an account name long enough to push the field off the edge
    #[test]
    fn a_long_account_name_does_not_overflow_the_row() {
        let lines = ui::header_lines(60, 24, "platform-production-emea");
        assert!(lines.iter().all(|l| line_text(l).chars().count() <= 60));
    }

    // Review Focus 1: narrower than the lockup plus the field, the header sheds
    // a row rather than wrapping the mark mid-glyph
    #[test]
    fn a_terminal_too_narrow_for_the_lockup_gets_one_line() {
        let lines = ui::header_lines(20, 24, "dev");
        assert_eq!(lines.len(), 1);
        assert_eq!(ui::header_height(20, 24, "dev"), 1);
        assert!(line_text(&lines[0]).chars().count() <= 20);
    }

    // The design draws the same eleven cells on a 1200-pixel board as on a
    // narrow one. A mark that scales with the terminal breaks the lockup: the
    // cloud grows and the word under it cannot, so the two stop being one mark.
    #[test]
    fn the_lockup_is_one_size_at_every_width() {
        for (w, h) in [(80, 24), (100, 30), (160, 40), (200, 60)] {
            let lines = ui::header_lines(w, h, "dev");
            assert_eq!(
                lines.len() as u16,
                ui::HEADER_HEIGHT_FULL,
                "{w}x{h} must draw the mark, the name and the rule"
            );
            assert_eq!(ui::header_height(w, h, "dev"), ui::HEADER_HEIGHT_FULL);
            for row in 0..ui::MARK_ROWS {
                let drawn: String = line_text(&lines[1 + row])
                    .chars()
                    .skip(1)
                    .take(ui::MARK_WIDTH as usize)
                    .collect();
                let expected: String = ui::mark_spans(true, row)
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect();
                assert_eq!(drawn, expected, "mark row {row} differs at {w}x{h}");
            }
            assert!(line_text(&lines[1 + ui::MARK_ROWS]).contains(ui::NAME_ROW));
            for line in &lines {
                assert!(line_text(line).chars().count() <= w as usize);
            }
        }
    }

    // Only the mark names exact hues. Everything that answers a question about
    // what gtunnel found stays on the terminal's own palette, so it is legible
    // against whatever background the user chose.
    #[test]
    fn only_the_logo_names_an_exact_hue() {
        let judgements = [
            ("ACCENT", theme::ACCENT),
            ("MUTED", theme::MUTED),
            ("LABEL", theme::LABEL),
            ("OK", theme::OK),
            ("WARN", theme::WARN),
            ("BAD", theme::BAD),
            ("FLAG", theme::FLAG),
        ];
        for (name, colour) in judgements {
            assert!(
                !matches!(colour, ratatui::style::Color::Rgb(..)),
                "{name} names an exact hue; only the LOGO_* constants may"
            );
        }
        // And the mark's own colours are the documented exception, so the
        // test fails if someone quietly demotes them to ANSI.
        for (name, colour) in [
            ("LOGO_LINE", theme::LOGO_LINE),
            ("LOGO_BLUE", theme::LOGO_BLUE),
            ("LOGO_RED", theme::LOGO_RED),
            ("LOGO_YELLOW", theme::LOGO_YELLOW),
            ("LOGO_GREEN", theme::LOGO_GREEN),
        ] {
            assert!(
                matches!(colour, ratatui::style::Color::Rgb(..)),
                "{name} is part of the mark and must name its hue"
            );
        }
    }

    // Case is load-bearing in three pairs, and the old bar printed both halves
    // of each. Showing only what the selection can do dissolves the trap
    // instead of annotating it.
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

    // Review Focus 4: no tunnels at all, so there is no selection to derive a
    // contextual group from. The footer still has to offer a way forward.
    #[test]
    fn an_empty_account_offers_a_way_to_fill_it() {
        let (ctx, _) = ui::footer_groups(None, false);
        assert_eq!(ctx.iter().map(|a| a.key).collect::<Vec<_>>(), ["a", "m"]);
    }

    // Review Focus 2: a width landing exactly on a shedding boundary, where an
    // action would be drawn half-way
    #[test]
    fn the_footer_sheds_whole_actions_and_never_half_of_one() {
        for width in 20u16..=140 {
            let text = line_text(&ui::footer_line(width, Some(TunnelStatus::Running), true));
            assert!(
                text.chars().count() <= width as usize,
                "overflows at {width}"
            );
            for action in ["stop", "restart", "clear log", "account", "keys", "quit"] {
                let drawn = text.matches(&format!(" {action}")).count();
                assert!(drawn <= 1, "{action} drawn twice at {width}");
            }
            // every label that appears, appears whole
            for action in [
                "stop",
                "restart",
                "copy",
                "open",
                "clear log",
                "account",
                "keys",
                "quit",
            ] {
                let prefix = &action[..action.len() - 1];
                if text.contains(prefix) {
                    assert!(
                        text.contains(action),
                        "{action} was cut mid-word at {width}: {text:?}"
                    );
                }
            }
        }
    }

    // The program group sheds last and `q quit` sheds never: a user who cannot
    // read the footer must still be able to leave.
    #[test]
    fn the_way_out_survives_the_narrowest_terminal() {
        let text = line_text(&ui::footer_line(24, Some(TunnelStatus::Running), true));
        assert!(text.contains("keys") && text.contains("quit"), "{text:?}");
    }

    // A lockup that wraps is worse than no lockup: the mark breaks mid-glyph
    // and the name lands inside it. The compact form is the escape hatch, and
    // it still has to carry the account.
    #[test]
    fn the_header_steps_down_before_it_wraps() {
        let wide = ui::header_lines(120, 24, "dev");
        assert_eq!(wide.len() as u16, ui::HEADER_HEIGHT_FULL);
        for line in &wide {
            assert!(
                line_text(line).chars().count() <= 120,
                "{}",
                line_text(line)
            );
        }
        let narrow = ui::header_lines(18, 24, "dev");
        assert_eq!(narrow.len() as u16, ui::HEADER_HEIGHT_COMPACT);
        assert!(line_text(&narrow[0]).chars().count() <= 18);
        assert!(line_text(&narrow[0]).contains("dev"), "{:?}", narrow[0]);
    }

    // Review Focus: the header must never be the reason the list has no rows
    #[test]
    fn a_short_terminal_keeps_a_visible_tunnel_row() {
        let app = App::new_demo();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).expect("terminal");
        terminal.draw(|f| ui::render(f, &app)).expect("draw");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("localhost:15432"), "{text}");
    }

    // A sparkline draws one cell per sample, so 60 samples left two fifths of a
    // wide pane empty and the chart read as a chart that had stopped
    #[test]
    fn the_chart_uses_every_column_it_is_given() {
        let samples: Vec<u64> = (0..240).collect();
        let shown = ui::sparkline_slice(&samples, 100);
        assert_eq!(shown.len(), 100);
        assert_eq!(
            shown.last(),
            samples.last(),
            "the newest sample must be drawn"
        );
        // Fewer samples than columns is the first minute, not an error
        assert_eq!(ui::sparkline_slice(&samples[..10], 100).len(), 10);
        assert!(ui::sparkline_slice(&samples, 0).is_empty());
    }

    // A sparkline with no scale cannot be read: 60 bars of "some latency" say
    // nothing without the number the tallest one stands for
    #[test]
    fn the_sparkline_is_labelled_with_its_peak() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = app_with_two();
        for sample in [12_000u64, 31_000, 18_000, 44_000, 22_000] {
            app.entries[0].history.record(sample);
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 38)).unwrap();
        terminal.draw(|f| ui::render(f, &app)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("peak 44 ms"), "{text}");
    }

    #[test]
    fn semicolon_cycles_to_the_next_account() {
        let mut app = app_with_two();
        app.accounts = vec!["dev".to_string(), "prod".to_string()];
        app.handle_key(key(KeyCode::Char(';')));
        assert_eq!(app.account, "prod");
        app.handle_key(key(KeyCode::Char(';')));
        assert_eq!(app.account, "dev");
    }

    #[test]
    fn the_frame_shows_the_list_and_both_kinds_of_url() {
        let app = app_with_two();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui::render(f, &app)).unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for wanted in ["TUNNELS (2)", "localhost:15432", "https://api.example.com"] {
            assert!(rendered.contains(wanted), "missing {wanted:?}");
        }
    }

    fn render_to_string(app: &App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| ui::render(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn app_with_many_instances(selected: usize) -> App {
        let mut app = app_with_two();
        app.instances = (0..36)
            .map(|i| InstanceChoice {
                name: format!("instance-{i:02}"),
                zone: "europe-west1-b".to_string(),
            })
            .collect();
        app.input_mode = InputMode::AddInstance;
        app.instance_selected = selected;
        app
    }

    #[test]
    fn the_instance_picker_scrolls_to_keep_the_selection_visible() {
        let rendered = render_to_string(&app_with_many_instances(30), 120, 40);
        assert!(
            rendered.contains("instance-30"),
            "the selected instance is off screen"
        );
        assert!(rendered.contains("31/36"), "no position counter");
    }

    #[test]
    fn the_instance_picker_starts_at_the_top() {
        let rendered = render_to_string(&app_with_many_instances(0), 120, 40);
        assert!(
            rendered.contains("instance-00"),
            "the list is scrolled past"
        );
        assert!(
            !rendered.contains("instance-35"),
            "the last row cannot fit with the first"
        );
        assert!(rendered.contains("1/36"), "no position counter");
    }

    fn keys(app: &mut App, sequence: &str) -> Option<Op> {
        let mut last = None;
        for c in sequence.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\u{2193}' => KeyCode::Down,
                other => KeyCode::Char(other),
            };
            if let Some(op) = app.handle_key(key(code)) {
                last = Some(op);
            }
        }
        last
    }

    #[test]
    fn adding_an_access_tunnel_walks_the_prompts_and_creates_it() {
        let mut app = app_with_two();
        app.instances = vec![InstanceChoice {
            name: "my-vm".to_string(),
            zone: "europe-west1-b".to_string(),
        }];

        // a, mode 1, name, instance, host (empty: the instance itself), ports
        let op = keys(&mut app, "a1db\n\n\n5432\n\n");
        assert_eq!(
            op,
            Some(Op::Create(NewTunnel {
                account: "dev".to_string(),
                name: "db".to_string(),
                mode: TunnelMode::Access,
                instance: "my-vm".to_string(),
                zone: "europe-west1-b".to_string(),
                remote_port: 5432,
                local_port: 15000,
                ..NewTunnel::default()
            }))
        );
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn publishing_a_url_without_a_relay_says_to_run_relay_up() {
        let mut app = app_with_two();
        app.has_relay = false;
        assert_eq!(keys(&mut app, "a2"), None);
        assert_eq!(app.input_mode, InputMode::Normal);
        let status = app.status.clone().expect("a status message");
        assert!(status.contains("relay up"), "{status}");
    }

    #[test]
    fn delete_asks_first_and_n_cancels() {
        let mut app = app_with_two();
        assert_eq!(keys(&mut app, "d"), None);
        assert_eq!(app.input_mode, InputMode::Confirm);
        assert_eq!(keys(&mut app, "n"), None);
        assert_eq!(app.input_mode, InputMode::Normal);

        assert_eq!(keys(&mut app, "d"), None);
        assert_eq!(keys(&mut app, "y"), Some(Op::Delete("db".to_string())));
    }

    // The chart's samples and the latency reading are one measurement in one
    // unit. The demo seeded milliseconds into a history the live path fills in
    // microseconds, so a 21 ms peak was labelled `0.0 ms` under a chart of bars
    // Adding a tunnel to a private endpoint has to be possible from the TUI,
    // not only from the CLI: the dialog asks what the chosen instance reaches,
    // and an empty answer means the instance itself
    #[test]
    fn the_add_dialog_asks_what_the_instance_reaches() {
        let mut app = App::new_demo();
        app.draft.instance = "private-cluster-bastion".to_string();
        app.draft.zone = "europe-west1-b".to_string();
        app.input_mode = InputMode::AddJumpHost;
        app.input = "10.3.0.13".to_string();
        assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
        assert_eq!(app.draft.host, "10.3.0.13");
        assert_eq!(app.input_mode, InputMode::AddRemotePort);
    }

    // What the create op sends to `add`: a jump tunnel names the host as its
    // target and the instance as the way to it, a plain one names the instance
    #[test]
    fn a_draft_with_a_host_is_created_through_its_instance() {
        let mut draft = NewTunnel {
            instance: "private-cluster-bastion".to_string(),
            remote_port: 443,
            ..Default::default()
        };
        assert_eq!(
            ops::add_arguments(&draft),
            ("private-cluster-bastion:443".to_string(), None)
        );
        draft.host = "10.3.0.13".to_string();
        assert_eq!(
            ops::add_arguments(&draft),
            (
                "10.3.0.13:443".to_string(),
                Some("private-cluster-bastion".to_string())
            )
        );
    }

    #[test]
    fn the_demo_peak_reads_in_the_same_unit_as_its_latency() {
        let app = App::new_demo();
        for entry in &app.entries {
            let Some(latency) = entry.metrics.as_ref().and_then(|m| m.latency) else {
                continue;
            };
            let peak = Duration::from_micros(entry.history.peak());
            assert!(
                peak >= latency / 2 && peak <= latency * 4,
                "{}: peak {peak:?} is not the same unit as latency {latency:?}",
                entry.name()
            );
        }
    }

    #[test]
    fn the_demo_needs_no_config_and_shows_every_kind_of_tunnel() {
        let app = App::new_demo();
        assert_eq!(app.entries.len(), 4);
        assert_eq!(
            app.entries
                .iter()
                .filter(|e| e.kind == TunnelKind::External)
                .count(),
            1
        );
        assert_eq!(
            app.entries.iter().filter(|e| e.tunnel.is_expose()).count(),
            2
        );
        assert!(!app.logs.is_empty());
        assert!(app.entries.iter().any(|e| e.metrics.is_some()));
    }

    #[test]
    fn cycling_accounts_in_the_demo_keeps_showing_tunnels() {
        let mut app = App::new_demo();
        let demo_entries = app.entries.len();

        assert_eq!(keys(&mut app, ";"), None);
        assert_eq!(app.account, "staging");
        assert!(!app.entries.is_empty(), "staging shows no tunnels");
        assert!(!app.logs.is_empty(), "staging shows no log");
        assert!(
            !app.force_refresh,
            "the demo has no state to refresh from, so the request would clear the screen"
        );

        assert_eq!(keys(&mut app, ";"), None);
        assert_eq!(app.account, "demo");
        assert_eq!(app.entries.len(), demo_entries);
    }
}
