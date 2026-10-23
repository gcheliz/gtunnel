pub mod app;
pub mod demo;
pub mod ops;
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
        assert!(text.contains("Peak: 44 ms"), "{text}");
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
        for wanted in ["Tunnels (2)", "localhost:15432", "https://api.example.com"] {
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
