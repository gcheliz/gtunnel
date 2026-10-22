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
                        entry.history.record(m.latency_ms.unwrap_or(0));
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
        let running: Vec<String> = app
            .entries
            .iter()
            .filter(|e| e.status == crate::state::TunnelStatus::Running)
            .filter(|e| e.kind != TunnelKind::External)
            .map(|e| e.name().to_string())
            .collect();
        let tx = tx.clone();
        let account = account.clone();
        tokio::spawn(async move {
            let mut out = Vec::new();
            for name in running {
                if let Some(m) = ops::collect(&account, &name, include_lb).await {
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

    // A sparkline with no scale cannot be read: 60 bars of "some latency" say
    // nothing without the number the tallest one stands for
    #[test]
    fn the_sparkline_is_labelled_with_its_peak() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = app_with_two();
        for sample in [12u64, 31, 18, 44, 22] {
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
        assert!(text.contains("peak 44"), "{text}");
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

        let op = keys(&mut app, "a1db\n\n5432\n\n");
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
}
