pub mod app;
pub mod config_edit;
pub mod demo;
pub mod ops;
pub mod theme;
pub mod ui;

use anyhow::Result;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[allow(unused_imports)]
pub use app::{App, EditTunnel, NewTunnel, Op, OpResult, TunnelEntry, TunnelKind};

use crate::config::load_config;
use crate::lifecycle;
use crate::probe::HealthStatus;

const WATCHDOG_OFFER: &str =
    "notifications need the watchdog when the TUI is closed: gtunnel watch install";

const TICK: Duration = Duration::from_millis(120);

// What the event loop reacts to; everything slow arrives as a message
enum Msg {
    Key(crossterm::event::KeyEvent),
    Instances(Vec<app::InstanceChoice>),
    InstancesFailed(String),
    // The leading string is the account the list was fetched for
    Projects(String, Vec<app::ProjectChoice>),
    ProjectsFailed(String, String),
    Configurations(Vec<app::ConfigurationChoice>),
    ConfigurationsFailed(String),
    // The account and the project the zones were fetched for
    Zones(String, String, Vec<String>),
    ZonesFailed(String, String, String),
    Tick,
    // The leading number is the generation the round was begun under; a result
    // whose generation is no longer the poller's current one is dropped
    Entries(u64, Vec<TunnelEntry>, HashSet<u16>),
    EntriesFailed(u64, String),
    Metrics(u64, Vec<(String, crate::probe::TunnelMetrics)>),
    // One reading of every local port's established connections, far more
    // often than a metrics round, so a request shorter than a round is seen
    Samples(std::collections::HashMap<u16, u64>),
    Health(u64, Vec<(String, HealthStatus)>),
    // Whether the watchdog is installed, asked once per session
    Watchdog(bool),
    Logs(String, Vec<String>),
    OpDone(OpResult),
}

// A seam of its own so the failure path is testable: an `if let Ok` here once
// dropped the error and left an unreadable account looking merely empty
fn instances_msg(result: Result<Vec<app::InstanceChoice>>) -> Msg {
    match result {
        Ok(instances) => Msg::Instances(instances),
        Err(e) => Msg::InstancesFailed(format!("{e:#}")),
    }
}

// Same seam as `instances_msg`, and the account rides along so a slow list
// cannot land in a dialog opened for another one
fn projects_msg(account: &str, result: Result<Vec<app::ProjectChoice>>) -> Msg {
    match result {
        Ok(projects) => Msg::Projects(account.to_string(), projects),
        Err(e) => Msg::ProjectsFailed(account.to_string(), format!("{e:#}")),
    }
}

// Same seam as `instances_msg`
fn configurations_msg(result: Result<Vec<app::ConfigurationChoice>>) -> Msg {
    match result {
        Ok(list) => Msg::Configurations(list),
        Err(e) => Msg::ConfigurationsFailed(format!("{e:#}")),
    }
}

// Same seam as `projects_msg`, and the project rides along too: it is the one
// the dialog chose, which a step back can change before the list lands
fn zones_msg(account: &str, project: &str, result: Result<Vec<String>>) -> Msg {
    let (account, project) = (account.to_string(), project.to_string());
    match result {
        Ok(zones) => Msg::Zones(account, project, zones),
        Err(e) => Msg::ZonesFailed(account, project, format!("{e:#}")),
    }
}

pub async fn run_tui(initial_account: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let account = config.get_account(initial_account)?.name.clone();

    let mut app = App::new(&account);
    app.reload_config(&config);
    app.zones = config
        .accounts
        .iter()
        .map(|a| (a.name.clone(), a.default_zone.clone()))
        .collect();
    app.default_zone = app.zones.get(&account).cloned().unwrap_or_default();

    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    spawn_input(tx.clone(), app.input_gate.clone());
    spawn_ticker(tx.clone());

    // Once, before the screen exists: the check runs detached and writes the
    // cache, so what is shown is what an earlier run already learned
    crate::update::spawn_check_if_due();
    app.update = crate::update::available_update();

    // Once per session, off the event loop: the TUI only notifies while it is
    // open and no watchdog is running, so someone without one is told how to
    // get notifications that survive closing it. It never installs one unasked
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Msg::Watchdog(crate::daemon::watchdog_installed().await));
        });
    }

    // The first health check is due now, not after the first tick
    let mut timers = Timers::due();
    schedule(&mut app, &mut timers, &tx);

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &tx, &mut rx).await;
    ratatui::restore();
    result
}

pub async fn run_demo() -> Result<()> {
    let mut app = App::new_demo();
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    spawn_input(tx.clone(), app.input_gate.clone());
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
                        from_create: matches!(op, Op::Create(_)),
                        from_account: false,
                        name: None,
                    });
                    // No runtime sits behind the demo, so no round will come
                    // to end the pending state the key put on the row
                    for entry in &mut app.entries {
                        entry.pending = None;
                    }
                }
                app.answer_demo_lists();
                if app.should_quit {
                    break;
                }
            }
            Msg::Tick => {
                app.force_refresh = false;
                // A list being waited on draws a spinner frame too, so it must advance
                if app.spinner.is_running() || app.list_loading.is_some() {
                    app.spinner.tick();
                }
            }
            _ => {}
        }
        terminal.draw(|f| ui::render(f, app))?;
    }
    Ok(())
}

fn spawn_input(tx: mpsc::UnboundedSender<Msg>, gate: std::sync::Arc<app::InputGate>) {
    gate.mark_live();
    // Terminal reads block, so they live on their own thread
    std::thread::spawn(move || loop {
        // An editor owns the keyboard while the gate is up, and a poll here
        // would read its keystrokes
        if gate.hold() {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
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
        // After a suspend or a stalled loop, the default replays every missed
        // tick back to back: a burst of schedule passes that did no new work
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        }
    });
}

/// One slow poll: at most one round in flight, and a result is applied only
/// if it belongs to the round the poller last began.
///
/// Without the flight guard a round that takes longer than its period (a
/// `gcloud` call waiting on a login, a wedged `launchctl`) is started again on
/// every tick, and the subprocesses pile up. Without the generation, a round
/// begun for the previous account lands in the new account's list.
struct Poller {
    last: Instant,
    in_flight: bool,
    generation: u64,
}

impl Poller {
    // Due at once, so the first frame is not empty
    fn due_now() -> Self {
        Self {
            last: Instant::now() - Duration::from_secs(3600),
            in_flight: false,
            generation: 0,
        }
    }

    fn due(&self, every: Duration) -> bool {
        !self.in_flight && self.last.elapsed() >= every
    }

    fn begin(&mut self) -> u64 {
        self.in_flight = true;
        self.last = Instant::now();
        self.generation += 1;
        self.generation
    }

    /// Whether `generation` is the current round. Only that round frees the
    /// flight slot: an older one finishing must not free a newer round's.
    fn finish(&mut self, generation: u64) -> bool {
        let current = generation == self.generation;
        if current {
            self.in_flight = false;
        }
        current
    }

    /// Drop whatever is in flight: its result will no longer be accepted, and
    /// the next round may start at once.
    fn invalidate(&mut self) {
        self.generation += 1;
        self.in_flight = false;
    }

    // Make the next round due without disturbing one already running
    fn force(&mut self) {
        self.last = Instant::now() - Duration::from_secs(3600);
    }
}

struct Timers {
    entries: Poller,
    metrics: Poller,
    sample: Instant,
    health: Poller,
    lb: Instant,
}

impl Timers {
    // Every timer starts due, so the first frame is not empty
    fn due() -> Self {
        let past = Instant::now() - Duration::from_secs(3600);
        Self {
            entries: Poller::due_now(),
            metrics: Poller::due_now(),
            sample: past,
            health: Poller::due_now(),
            lb: past,
        }
    }

    // Not `*self = Self::due()`: that would restart the generations, and a
    // result still in flight could then match the number of a new round
    fn force(&mut self) {
        let past = Instant::now() - Duration::from_secs(3600);
        self.entries.force();
        self.metrics.force();
        self.sample = past;
        self.health.force();
        self.lb = past;
    }

    // What an account change does to every round begun for the old account
    fn account_changed(&mut self) {
        self.entries.invalidate();
        self.metrics.invalidate();
        self.health.invalidate();
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
    // The tunnel whose running op empties its log, held until the op is done
    let mut forget_after: Option<String> = None;

    terminal.draw(|f| ui::render(f, app))?;

    while let Some(msg) = rx.recv().await {
        match msg {
            Msg::Key(key) => {
                if let Some(op) = app.handle_key(key) {
                    // Applied when the op finishes rather than here: a
                    // metrics tick landing mid-op would put the count back
                    forget_after = empties_the_log(&op).map(String::from);
                    let account = app.account.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(Msg::OpDone(ops::execute(&account, op).await));
                    });
                }
                if app.should_quit {
                    break;
                }
                if app.clear_screen {
                    app.clear_screen = false;
                    terminal.clear()?;
                }
                if app.suspend {
                    app.suspend = false;
                    suspend(terminal)?;
                    timers.force();
                }
            }
            Msg::Tick => {
                // A list being waited on draws a spinner frame too, so it must advance
                if app.spinner.is_running() || app.list_loading.is_some() {
                    app.spinner.tick();
                }
                if app.force_refresh {
                    app.force_refresh = false;
                    timers.force();
                    logs_for = None;
                }
                if app.take_follow_up() {
                    timers.entries.force();
                }
                if app.want_instances {
                    app.want_instances = false;
                    let account = app.account.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(instances_msg(ops::load_instances(&account).await));
                    });
                }
                if app.want_configurations {
                    app.want_configurations = false;
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(configurations_msg(ops::load_configurations().await));
                    });
                }
                if app.want_zones {
                    app.want_zones = false;
                    let account = app.account_draft.name.clone();
                    let project = app.account_draft.project.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = ops::load_zones(&account, &project).await;
                        let _ = tx.send(zones_msg(&account, &project, result));
                    });
                }
                if app.want_projects {
                    app.want_projects = false;
                    let account = app.account_draft.name.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = ops::load_projects(&account).await;
                        let _ = tx.send(projects_msg(&account, result));
                    });
                }
                schedule(app, &mut timers, tx);
                // The logs panel follows the selection, so it reloads with it
                let selected = app.selected_entry().map(|e| e.tunnel.clone());
                if let Some(tunnel) = selected
                    && (logs_for.as_deref() != Some(tunnel.name.as_str())
                        || timers_logs_due(&timers))
                {
                    logs_for = Some(tunnel.name.clone());
                    let tx = tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let lines = ops::read_logs(&tunnel, ui::LOG_LINES);
                        let _ = tx.send(Msg::Logs(tunnel.name.clone(), lines));
                    });
                }
            }
            msg @ (Msg::Entries(..)
            | Msg::EntriesFailed(..)
            | Msg::Metrics(..)
            | Msg::Health(..)) => apply_polled(app, &mut timers, msg),
            Msg::Samples(samples) => app.record_samples(samples),
            Msg::Watchdog(installed) => {
                // Never over something the user is already reading
                if !installed && app.status.is_none() {
                    app.set_status(WATCHDOG_OFFER);
                }
            }
            Msg::Logs(name, lines) => {
                if app.selected_entry().map(|e| e.name().to_string()) == Some(name) {
                    app.logs = lines;
                }
            }
            Msg::Instances(instances) => app.instances_arrived(instances),
            Msg::InstancesFailed(reason) => app.instances_failed(&reason),
            Msg::Projects(account, projects) => app.projects_arrived(&account, projects),
            Msg::ProjectsFailed(account, reason) => app.projects_failed(&account, &reason),
            Msg::Configurations(list) => app.configurations_arrived(list),
            Msg::ConfigurationsFailed(reason) => app.configurations_failed(&reason),
            Msg::Zones(account, project, zones) => app.zones_arrived(&account, &project, zones),
            Msg::ZonesFailed(account, project, reason) => {
                app.zones_failed(&account, &project, &reason)
            }
            Msg::OpDone(result) => {
                // Rounds begun for the previous account must not land in the
                // new one's list
                if result.from_account {
                    timers.account_changed();
                }
                if let Some(name) = forget_after.take() {
                    app.forget_metrics(&name);
                }
                // An account op wrote config.toml, so what the header, the
                // accounts page and the Add dialog hold is a copy of a file
                // that has moved on. Read before `apply`, which is what puts
                // the result on the page the reload may have reshaped.
                if result.from_account
                    && let Ok(config) = load_config()
                {
                    app.reload_config(&config);
                }
                // A round begun before the op describes the tunnel as it was,
                // so it is dropped and the next one is begun at once. The
                // generation is what makes the drop certain: `force` alone
                // would leave the old round free to land after the op.
                let acted_on_a_row = result.name.is_some();
                if acted_on_a_row {
                    timers.entries.invalidate();
                    timers.entries.force();
                }
                app.apply(result);
                // The ops that mark a row print nothing, and a full clear
                // after each one flashes the screen. The others run CLI
                // commands that write to this terminal.
                if !acted_on_a_row {
                    terminal.clear()?;
                }
            }
        }
        terminal.draw(|f| ui::render(f, app))?;
    }
    Ok(())
}

fn timers_logs_due(timers: &Timers) -> bool {
    timers.metrics.last.elapsed() < Duration::from_millis(200)
}

// The result of a poller round, applied only when it is the round the poller
// last began. A round invalidated by an account switch, or overtaken by a
// newer one, is dropped whole.
fn apply_polled(app: &mut App, timers: &mut Timers, msg: Msg) {
    match msg {
        Msg::Entries(generation, entries, used_ports) => {
            if timers.entries.finish(generation) {
                app.replace_entries(entries, used_ports);
                // A tunnel that went from running to dead shows up here, and
                // no health round will ever say so: it is not probed
                announce_round(app);
            }
        }
        Msg::EntriesFailed(generation, error) => {
            if timers.entries.finish(generation) {
                app.load_failed(&error);
            }
        }
        Msg::Metrics(generation, metrics) => {
            if !timers.metrics.finish(generation) {
                return;
            }
            // Whoever the round was meant to measure and came back without a
            // verdict is owed a health probe; the others had theirs from it
            let covered: Vec<String> = ops::metric_targets(&app.entries)
                .into_iter()
                .map(|t| t.tunnel.name)
                .collect();
            for entry in app.entries.iter_mut() {
                let owed = covered.iter().any(|n| n == entry.name())
                    && !metrics
                        .iter()
                        .any(|(n, m)| n == entry.name() && m.health.is_some());
                entry.needs_health_probe = owed;
            }
            for (name, m) in metrics {
                if let Some(entry) = app.entries.iter_mut().find(|e| e.name() == name) {
                    // Microseconds: a fast hop is a real measurement, and
                    // whole milliseconds would record it as nothing
                    if let Some(d) = m.latency {
                        entry.history.record(d.as_micros() as u64);
                    }
                    entry.crash_looping = entry
                        .restart_watch
                        .observe(std::time::SystemTime::now(), m.restarts);
                    // The probe that timed the hop also says whether it
                    // answered; the health round skipped this tunnel for it
                    if let Some(health) = m.health {
                        entry.health = settled_health(entry.crash_looping, health);
                    }
                    entry.metrics = Some(m);
                }
            }
            announce_round(app);
        }
        Msg::Health(generation, results) => {
            if !timers.health.finish(generation) {
                return;
            }
            for (name, health) in results {
                let Some(entry) = app.entries.iter_mut().find(|e| e.name() == name) else {
                    continue;
                };
                entry.health = settled_health(entry.crash_looping, health);
            }
            announce_round(app);
        }
        _ => {}
    }
}

// A probe that passed or failed cannot say the service is restarting in a
// loop, and that is the more useful thing to tell the reader; expired
// credentials and the other verdicts are kept. One rule for both rounds that
// carry a verdict, or the same probe would read differently by who sent it.
fn settled_health(crash_looping: bool, probed: HealthStatus) -> HealthStatus {
    if crash_looping && matches!(probed, HealthStatus::Healthy | HealthStatus::Unhealthy) {
        HealthStatus::CrashLooping
    } else {
        probed
    }
}

// The ops that empty a tunnel's log: start and restart both reinstall the
// service, which truncates, and clear-log is the user asking for it. The
// count measured before one of these belongs to lines that are gone.
fn empties_the_log(op: &Op) -> Option<&str> {
    match op {
        Op::Start(name) | Op::Restart(name) | Op::ClearLog(name) => Some(name),
        _ => None,
    }
}

// What the TUI knows about notifications, in one place so a test can hand it
// a directory and a recorder in place of the real ones.
struct Observer<'a> {
    dir: &'a std::path::Path,
    notifier: &'a dyn crate::notify::Notifier,
    cfg: crate::config::NotifyConfig,
    // The watchdog notifies for every account, TUI open or not, so while it is
    // alive the TUI saying the same thing would be the double alert the ledger
    // exists to prevent
    watchdog_fresh: bool,
}

// A health round and a status change are the same question asked of one row:
// what state is it in now? The answer goes through the shared ledger, so what
// comes out is only what nobody has announced yet, for the account on screen,
// as one notification. The threshold is one: this is the TUI's own poll, which
// announced the first unhealthy reading before too.
fn observe_lifecycle(obs: &Observer, account: &str, entries: &[TunnelEntry]) {
    if obs.watchdog_fresh {
        return;
    }
    let mut edges = Vec::new();
    for entry in entries {
        // Only rows gtunnel owns, and not one it is in the middle of changing:
        // a start in flight reads as stopped until the job is loaded
        if entry.kind != TunnelKind::Managed || entry.pending.is_some() {
            continue;
        }
        let healthy = match entry.health {
            HealthStatus::Healthy => Some(true),
            HealthStatus::Unhealthy | HealthStatus::AuthExpired | HealthStatus::CrashLooping => {
                Some(false)
            }
            HealthStatus::Unknown | HealthStatus::Checking => None,
        };
        let failures = u32::from(entry.health.is_failing());
        let Some(to) = lifecycle::observe(entry.status, healthy, failures, 1) else {
            continue;
        };
        edges.extend(
            lifecycle::transition_in(
                obs.dir,
                account,
                entry.name(),
                to,
                lifecycle::Cause::Observed,
            )
            .ok()
            .flatten(),
        );
    }
    lifecycle::announce_with(obs.notifier, &edges, &obs.cfg);
}

#[cfg(not(test))]
fn announce_round(app: &App) {
    let Ok(dir) = crate::config::config_dir() else {
        return;
    };
    let cfg = load_config().map(|c| c.notifications).unwrap_or_default();
    observe_lifecycle(
        &Observer {
            dir: &dir,
            notifier: &crate::notify::Desktop,
            cfg,
            watchdog_fresh: crate::lifecycle::watchdog_is_fresh(),
        },
        &app.account,
        &app.entries,
    );
}

// Under test there is no config directory of its own and no desktop to
// notify; `observe_lifecycle` is what is tested, with both supplied
#[cfg(test)]
fn announce_round(_: &App) {}

fn schedule(app: &mut App, timers: &mut Timers, tx: &mpsc::UnboundedSender<Msg>) {
    let account = app.account.clone();

    if timers.entries.due(app::DISCOVERY_EVERY) {
        let generation = timers.entries.begin();
        // One small file read, so a check that finishes while the TUI is open
        // shows up within a round
        app.update = crate::update::available_update();
        let tx = tx.clone();
        let account = account.clone();
        tokio::spawn(async move {
            let _ = tx.send(match ops::load_entries(&account).await {
                Ok((entries, used_ports)) => Msg::Entries(generation, entries, used_ports),
                Err(e) => Msg::EntriesFailed(generation, format!("{e:#}")),
            });
        });
    }

    // Blocking: `lsof` is a process, and it runs ten times per metrics round
    if timers.sample.elapsed() >= app::SAMPLE_EVERY && ports_worth_sampling(app) {
        timers.sample = Instant::now();
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::Samples(crate::probe::established_by_port()));
        });
    }

    let polls = plan_probes(app, timers);
    if let Some(round) = polls.metrics {
        let tx = tx.clone();
        tokio::spawn(async move {
            let include_lb = round.include_lb;
            let peaks = std::sync::Arc::new(round.peaks);
            let targets = round
                .targets
                .into_iter()
                .map(|t| (t.tunnel.name.clone(), t));
            let out = collect_round(targets, move |target| {
                let peaks = peaks.clone();
                async move { ops::collect(&target, include_lb, Some(&peaks)).await }
            })
            .await;
            let _ = tx.send(Msg::Metrics(round.generation, out));
        });
    }

    let targets = polls.health;
    if !targets.is_empty() {
        let generation = timers.health.begin();
        let tx = tx.clone();
        tokio::spawn(async move {
            let probes = targets
                .into_iter()
                .map(|(name, tunnel)| async move { (name, ops::probe_health(&tunnel).await) });
            let results = futures_join(probes).await;
            let _ = tx.send(Msg::Health(generation, results));
        });
    }
}

// The probing one tick starts, decided before any of it runs
struct Probes {
    metrics: Option<MetricsRound>,
    health: Vec<(String, crate::state::PersistentTunnel)>,
}

/// Which rounds start this tick, and for whom.
///
/// A probe costs one IAP connection to the VM, and the metrics probe already
/// carries a verdict (`TunnelMetrics::health`), so a tunnel the metrics round
/// starts this tick is left out of the health round instead of being asked
/// twice. Everything the metrics round does not cover keeps the health probe on
/// its own clock: a tunnel with no local port of its own, a stopped one is not
/// probed at all, and a metrics round still in flight leaves every tunnel to
/// the health round rather than leaving the rows without a verdict.
fn plan_probes(app: &mut App, timers: &mut Timers) -> Probes {
    let metrics = metrics_round(app, timers);
    let mut health = health_round(app, timers);
    if let Some(round) = &metrics {
        let covered: std::collections::HashSet<&str> = round
            .targets
            .iter()
            .map(|t| t.tunnel.name.as_str())
            .collect();
        let owed: std::collections::HashSet<&str> = app
            .entries
            .iter()
            .filter(|e| e.needs_health_probe)
            .map(|e| e.name())
            .collect();
        health.retain(|(name, _)| !covered.contains(name.as_str()) || owed.contains(name.as_str()));
    }
    Probes { metrics, health }
}

// Whether any port on screen can have clients at all. The sampler runs ten
// times per metrics round, and a screen of stopped tunnels, or of public ones
// that have no local port, is no reason to spawn `lsof` twice a second.
fn ports_worth_sampling(app: &App) -> bool {
    app.entries.iter().any(|e| {
        e.status == crate::state::TunnelStatus::Running
            && matches!(
                e.tunnel.spec,
                crate::state::TunnelSpec::Access { local_port, .. } if local_port != 0
            )
    })
}

// What one metrics round measures, and the sampler window it reports
struct MetricsRound {
    generation: u64,
    targets: Vec<ops::MetricTarget>,
    include_lb: bool,
    peaks: std::collections::HashMap<u16, u64>,
}

/// The round due now, or `None` when the clock has not come round yet.
///
/// Taking the peaks is what ends the sampler's window: `active` is the most
/// clients seen since the last round, so a round that left them in place would
/// report the same burst every five seconds until a larger one replaced it.
fn metrics_round(app: &mut App, timers: &mut Timers) -> Option<MetricsRound> {
    if !timers.metrics.due(app::METRICS_EVERY) {
        return None;
    }
    let generation = timers.metrics.begin();
    let include_lb = timers.lb.elapsed() >= app::LB_EVERY;
    if include_lb {
        timers.lb = Instant::now();
    }
    Some(MetricsRound {
        generation,
        targets: ops::metric_targets(&app.entries),
        include_lb,
        peaks: app.take_peaks(),
    })
}

/// The tunnels to probe this round, empty when the round is not due or has
/// nothing to probe.
///
/// The clock is spent only when there is something to spend it on. Every timer
/// starts due, but entries arrive asynchronously, so the first tick sees an
/// empty list; resetting the timer there bought a whole period of `?` for rows
/// that were about to appear. `health_stale` covers the same hole after `;`,
/// which clears the list, and after a tunnel is added under a new name.
///
/// A row still waiting for its first verdict is marked `Checking`, so `?` goes
/// back to meaning the probe had nothing to measure — a tunnel with no local
/// port of its own. A verdict already on screen stands until the next one
/// lands, rather than blinking back to `…` every round.
fn health_round(
    app: &mut App,
    timers: &mut Timers,
) -> Vec<(String, crate::state::PersistentTunnel)> {
    // A round already out keeps its slot; a stale row stays owed until it lands
    if timers.health.in_flight || (!timers.health.due(app::HEALTH_EVERY) && !app.health_stale) {
        return Vec::new();
    }
    let running = |e: &&mut TunnelEntry| e.status == crate::state::TunnelStatus::Running;
    // Every running tunnel is probed at once; one slow host must not delay the
    // rest
    let mut targets = Vec::new();
    for entry in app.entries.iter_mut().filter(running) {
        if entry.health == HealthStatus::Unknown {
            entry.health = HealthStatus::Checking;
        }
        targets.push((entry.name().to_string(), entry.tunnel.clone()));
    }
    // The debt is settled by looking, but the clock only moves when there was
    // something to probe: an account whose tunnels are all stopped must not
    // re-ask every tick, and a list that has not loaded yet must not lose its
    // turn.
    app.health_stale = false;
    if !targets.is_empty() {
        timers.health.last = Instant::now();
    }
    targets
}

/// One metrics round: every target measured at once, in target order, with a
/// target that yields nothing left out.
///
/// The round is as slow as its slowest tunnel, not the sum of them. Measured
/// one after another, a single tunnel whose probe stalled held back the
/// numbers of every other row for as long as it took to time out.
async fn collect_round<T, R, F, Fut>(
    targets: impl IntoIterator<Item = (String, T)>,
    collect: F,
) -> Vec<(String, R)>
where
    F: Fn(T) -> Fut,
    Fut: std::future::Future<Output = Option<R>> + Send + 'static,
    R: Send + 'static,
{
    let measured = targets.into_iter().map(|(name, target)| {
        let measuring = collect(target);
        async move { measuring.await.map(|m| (name, m)) }
    });
    futures_join(measured).await.into_iter().flatten().collect()
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
mod accounts_page_tests;
#[cfg(test)]
mod config_page_tests;
#[cfg(test)]
mod tests;
