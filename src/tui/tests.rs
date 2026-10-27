//! Tests for the event loop in `mod.rs`: pollers, timers, lifecycle
//! announcements and the way messages land on the app. Kept beside the module
//! and not inside it so `super::*` still reaches its private items while the
//! file stays short enough to read.

use super::app::*;
use super::*;
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(super) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn a_poller_in_flight_is_not_due() {
    let mut p = Poller::due_now();
    assert!(p.due(Duration::from_secs(5)));
    let g = p.begin();
    assert!(!p.due(Duration::ZERO));
    assert!(p.finish(g));
    p.last = Instant::now() - Duration::from_secs(6);
    assert!(p.due(Duration::from_secs(5)));
}

#[test]
fn a_result_from_an_older_round_is_dropped() {
    let mut p = Poller::due_now();
    let old = p.begin();
    p.invalidate();
    assert!(!p.finish(old));
    let new = p.begin();
    assert!(
        !p.finish(old),
        "the old round must not free the new one's slot"
    );
    assert!(p.in_flight);
    assert!(p.finish(new));
}

#[test]
fn forcing_the_timers_leaves_a_round_in_flight_alone() {
    let mut timers = Timers::due();
    let g = timers.entries.begin();
    timers.force();
    assert!(
        !timers.entries.due(Duration::ZERO),
        "a second round started"
    );
    assert!(timers.entries.finish(g));
    assert!(
        timers.entries.due(app::DISCOVERY_EVERY),
        "the force was lost"
    );
}

#[test]
fn a_round_for_the_previous_account_is_dropped() {
    let mut app = App::new("dev");
    let mut timers = Timers::due();
    let old = timers.entries.begin();
    // What an account switch does when its op completes
    timers.account_changed();
    let stale = vec![TunnelEntry::new(
        access("db", 15432),
        TunnelStatus::Running,
        TunnelKind::Managed,
    )];
    apply_polled(
        &mut app,
        &mut timers,
        Msg::Entries(old, stale, HashSet::new()),
    );
    assert!(app.entries.is_empty(), "the old account's rows landed");

    let new = timers.entries.begin();
    apply_polled(
        &mut app,
        &mut timers,
        Msg::Entries(new, Vec::new(), HashSet::new()),
    );
    assert!(app.loaded, "the current round was refused");
}

#[test]
fn a_metrics_round_in_flight_is_not_started_twice() {
    let mut app = app_with_two();
    let mut timers = Timers::due();
    assert!(metrics_round(&mut app, &mut timers).is_some());
    timers.metrics.last = Instant::now() - app::METRICS_EVERY;
    assert!(metrics_round(&mut app, &mut timers).is_none());
}

// The sampler spawns a process ten times per round, so it only runs while
// there is a port for the count to be about
#[test]
fn nothing_listening_means_nothing_to_sample() {
    let mut app = app_with_two();
    assert!(ports_worth_sampling(&app));
    app.entries[0].status = TunnelStatus::Stopped;
    assert!(
        !ports_worth_sampling(&app),
        "a stopped tunnel has no clients to count, and the expose row has no local port"
    );
}

// What the round reports is the sampler's window, and reporting it ends
// it: carried over, a burst would be re-reported every five seconds until
// a larger one replaced it
#[test]
fn a_metrics_round_carries_the_peaks_and_starts_the_window_again() {
    let mut app = app_with_two();
    app.record_samples(std::collections::HashMap::from([(15432u16, 3u64)]));
    let mut timers = Timers::due();

    let round = metrics_round(&mut app, &mut timers).expect("every timer starts due");
    assert_eq!(round.peaks.get(&15432).copied(), Some(3));
    assert!(
        app.take_peaks().is_empty(),
        "the window still holds a count the round has already reported"
    );
    assert!(
        metrics_round(&mut app, &mut timers).is_none(),
        "the round did not reset its own clock"
    );
}

// Starting a tunnel empties its log, but the count measured during the
// run before it survives in memory. Without this, a restart writes
// `connections: 2 -> 0` into a fresh log, which reads as two clients
// having been here on this run and left.
#[test]
fn the_ops_that_empty_a_log_also_drop_the_count_it_belonged_to() {
    assert_eq!(empties_the_log(&Op::Start("db".to_string())), Some("db"));
    assert_eq!(empties_the_log(&Op::Restart("db".to_string())), Some("db"));
    assert_eq!(empties_the_log(&Op::ClearLog("db".to_string())), Some("db"));
    assert_eq!(
        empties_the_log(&Op::Stop("db".to_string())),
        None,
        "a stop leaves the log alone, and its clients really did go"
    );
}

#[test]
fn a_forgotten_count_makes_the_next_one_a_first_sight() {
    let mut app = app_with_two();
    app.entries[0].metrics = Some(crate::probe::TunnelMetrics {
        active_connections: 2,
        ..Default::default()
    });
    app.forget_metrics("db");
    assert!(
        app.entries[0].metrics.is_none(),
        "the count from the run before belongs to a log that is gone"
    );
}

pub(super) fn access(name: &str, local_port: u16) -> PersistentTunnel {
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

pub(super) fn expose(name: &str, hostname: &str) -> PersistentTunnel {
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

pub(super) fn app_with_two() -> App {
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

struct Said(std::cell::RefCell<Vec<String>>);

impl crate::notify::Notifier for Said {
    fn send(&self, _title: &str, body: &str) {
        self.0.borrow_mut().push(body.to_string());
    }
}

// Both rows were last seen Up, then the world changed under them
pub(super) fn seeded(dir: &std::path::Path) {
    use crate::lifecycle::{transition_in, Cause, State};
    for name in ["db", "api"] {
        transition_in(dir, "dev", name, State::Up, Cause::Observed).expect("seed");
    }
}

fn observe_with(dir: &std::path::Path, fresh: bool, app: &App) -> Vec<String> {
    let said = Said(Default::default());
    observe_lifecycle(
        &Observer {
            dir,
            notifier: &said,
            cfg: crate::config::NotifyConfig::default(),
            watchdog_fresh: fresh,
        },
        &app.account,
        &app.entries,
    );
    said.0.into_inner()
}

#[test]
fn the_tui_stays_quiet_while_the_watchdog_heartbeat_is_fresh() {
    let dir = tempfile::tempdir().expect("tempdir");
    seeded(dir.path());
    let mut app = app_with_two();
    app.entries[0].status = TunnelStatus::Error;

    assert!(observe_with(dir.path(), true, &app).is_empty());
    assert_eq!(
        crate::lifecycle::stored_in(dir.path(), "dev", "db"),
        Some(crate::lifecycle::State::Up),
        "the ledger is the watchdog's to move while it is watching"
    );
}

#[test]
fn without_a_watchdog_the_tui_announces_a_tunnel_that_died() {
    let dir = tempfile::tempdir().expect("tempdir");
    seeded(dir.path());
    let mut app = app_with_two();
    app.entries[0].status = TunnelStatus::Error;

    let said = observe_with(dir.path(), false, &app);
    assert_eq!(said, ["db (dev) failed"]);
    assert!(
        observe_with(dir.path(), false, &app).is_empty(),
        "the next round finds the change already stored"
    );
}

#[test]
fn a_tunnel_that_goes_unhealthy_while_running_is_announced_by_the_tui() {
    let dir = tempfile::tempdir().expect("tempdir");
    seeded(dir.path());
    let mut app = app_with_two();
    app.entries[1].health = HealthStatus::Unhealthy;

    assert_eq!(
        observe_with(dir.path(), false, &app),
        ["api (dev) is unhealthy"]
    );
}

#[test]
fn two_changes_in_one_health_round_are_one_notification() {
    let dir = tempfile::tempdir().expect("tempdir");
    seeded(dir.path());
    let mut app = app_with_two();
    app.entries[0].health = HealthStatus::Unhealthy;
    app.entries[1].health = HealthStatus::Unhealthy;

    assert_eq!(
        observe_with(dir.path(), false, &app),
        ["2 tunnels: 2 unhealthy"]
    );
}

#[test]
fn a_row_with_an_operation_in_flight_is_not_judged() {
    let dir = tempfile::tempdir().expect("tempdir");
    seeded(dir.path());
    let mut app = app_with_two();
    app.entries[0].status = TunnelStatus::Stopped;
    app.entries[0].pending = Some(PendingOp {
        verb: "start",
        since: Instant::now(),
    });

    assert!(observe_with(dir.path(), false, &app).is_empty());
}

#[test]
fn a_tunnel_the_user_stopped_is_not_announced_by_the_tui() {
    let dir = tempfile::tempdir().expect("tempdir");
    seeded(dir.path());
    crate::lifecycle::transition_in(
        dir.path(),
        "dev",
        "db",
        crate::lifecycle::State::Stopped,
        crate::lifecycle::Cause::User,
    )
    .expect("record");
    let mut app = app_with_two();
    app.entries[0].status = TunnelStatus::Stopped;

    assert!(observe_with(dir.path(), false, &app).is_empty());
}

// Every timer starts due, but entries arrive asynchronously, so the first
// tick finds no entries at all. Spending the health slot there left every
// row reading `?` for a full period, while latency — on a five-second
// period — filled in almost at once.
#[test]
fn an_empty_list_does_not_spend_the_health_slot() {
    let mut app = App::new("dev");
    let mut timers = Timers::due();
    assert!(health_round(&mut app, &mut timers).is_empty());

    app.entries = vec![TunnelEntry::new(
        access("db", 15432),
        TunnelStatus::Running,
        TunnelKind::Managed,
    )];
    let targets = health_round(&mut app, &mut timers);
    assert_eq!(
        targets.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["db"],
        "the slot was still there when the entries arrived"
    );
}

// A round in flight is not a round with nothing to say: `?` means the probe
// found nothing to measure, so a row still waiting for its first answer
// says `…` instead, and a verdict already on screen stays until the next
// one lands.
#[test]
fn a_first_round_says_checking_and_a_later_one_leaves_the_verdict_alone() {
    let mut app = app_with_two();
    let mut timers = Timers::due();
    let _ = health_round(&mut app, &mut timers);
    assert!(
        app.entries
            .iter()
            .all(|e| e.health == HealthStatus::Checking),
        "{:?}",
        app.entries.iter().map(|e| e.health).collect::<Vec<_>>()
    );

    app.entries[0].health = HealthStatus::Healthy;
    timers.health.last = Instant::now() - app::HEALTH_EVERY;
    let _ = health_round(&mut app, &mut timers);
    assert_eq!(app.entries[0].health, HealthStatus::Healthy);
}

// `;` clears the list, so the next load is all new names with no health to
// carry. Without this the account just switched to reads `?` for a period.
#[test]
fn entries_that_bring_no_health_make_the_next_round_due() {
    let mut app = app_with_two();
    let mut timers = Timers::due();
    let _ = health_round(&mut app, &mut timers);
    assert!(
        health_round(&mut app, &mut timers).is_empty(),
        "a spent slot does not come back on its own"
    );

    app.replace_entries(
        vec![TunnelEntry::new(
            access("other", 15999),
            TunnelStatus::Running,
            TunnelKind::Managed,
        )],
        HashSet::new(),
    );
    assert_eq!(
        health_round(&mut app, &mut timers).len(),
        1,
        "a name with no carried health is probed at once"
    );
}

// Health carried across a stop meant the next start read as "healthy
// again" or "still unhealthy" by how the last run happened to end, not by
// what this one did
#[test]
fn health_resets_when_a_tunnel_stops() {
    let mut app = app_with_two();
    app.entries[0].health = HealthStatus::Unhealthy;
    app.entries[1].health = HealthStatus::Healthy;
    let round = |status| {
        vec![
            TunnelEntry::new(access("db", 15432), status, TunnelKind::Managed),
            TunnelEntry::new(
                expose("api", "api.example.com"),
                TunnelStatus::Running,
                TunnelKind::Managed,
            ),
        ]
    };

    app.replace_entries(round(TunnelStatus::Stopped), HashSet::new());
    assert_eq!(app.entries[0].health, HealthStatus::Unknown);
    assert_eq!(
        app.entries[1].health,
        HealthStatus::Healthy,
        "a tunnel that is still running keeps what it was last measured as"
    );

    app.entries[0].health = HealthStatus::Healthy;
    app.replace_entries(round(TunnelStatus::Error), HashSet::new());
    assert_eq!(app.entries[0].health, HealthStatus::Unknown);
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
fn s_lowercase_on_a_running_tunnel_is_a_noop() {
    let mut app = app_with_two();
    assert_eq!(app.handle_key(key(KeyCode::Char('s'))), None);
    let status = app.status.clone().expect("status message");
    assert!(status.contains("already running"), "{status}");
}

#[test]
fn s_uppercase_on_a_stopped_tunnel_is_a_noop() {
    let mut app = app_with_two();
    app.entries[0].status = TunnelStatus::Stopped;
    assert_eq!(app.handle_key(key(KeyCode::Char('S'))), None);
    let status = app.status.clone().expect("status message");
    assert!(status.contains("not running"), "{status}");
}

#[test]
fn s_lowercase_starts_a_stopped_tunnel() {
    let mut app = app_with_two();
    app.entries[0].status = TunnelStatus::Stopped;
    assert_eq!(
        app.handle_key(key(KeyCode::Char('s'))),
        Some(Op::Start("db".to_string()))
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

// Up on the first row keeps the row it is on, so the panel keeps the log it
// is already showing. Clearing it there emptied a pane the loop then had no
// reason to refill, because it refetches on a change of selected name and
// the name had not changed: the tunnel read as silent until the log timer
// came round. The last row is the same clamp at the other end.
#[test]
fn a_selection_that_cannot_move_keeps_the_log_on_screen() {
    let mut app = app_with_two();
    app.logs = vec!["2026-10-23T23:16:59Z gtunnel: starting".to_string()];
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.selected, 0);
    assert!(!app.logs.is_empty(), "{:?}", app.logs);

    app.selected = app.entries.len() - 1;
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.selected, app.entries.len() - 1);
    assert!(!app.logs.is_empty(), "{:?}", app.logs);

    // A move that does land on another tunnel still drops the stale pane
    app.selected = 0;
    app.handle_key(key(KeyCode::Down));
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
        from_create: false,
        from_account: false,
        name: None,
    });
    assert_eq!(app.status.as_deref(), Some("permission denied"));
    assert!(app.status_error, "a failure must be marked");

    app.apply(OpResult {
        message: "db starting".to_string(),
        ok: true,
        reload: true,
        from_create: false,
        from_account: false,
        name: None,
    });
    assert!(!app.status_error, "success must clear the mark");
}

#[test]
fn the_demo_is_always_loaded() {
    let mut app = App::new_demo();
    assert!(app.loaded);
    app.handle_key(key(KeyCode::Char(';')));
    assert!(app.loaded, "the demo carries its own data across accounts");
}

// The demo has no gcloud behind it, so every list a form asks for is answered
// from placeholder data; otherwise its pickers never open and a recording of
// the forms shows only the text fields they fall back to
#[test]
fn the_demo_answers_every_list_a_form_asks_for() {
    let mut app = App::new_demo();
    app.handle_key(key(KeyCode::Char(':')));
    app.handle_key(key(KeyCode::Char('a')));
    app.answer_demo_lists();
    assert_eq!(app.input_mode, InputMode::AccountConfigurationPick);
    assert!(!app.want_configurations, "the request is answered once");
    let active = &app.filtered_configurations()[app.configuration_selected];
    assert_eq!(active.name, "demo");
    assert!(
        app.configurations_list.iter().any(|c| c.project.is_none()),
        "one row shows the refusal"
    );

    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Char('e')));
    app.answer_demo_lists();
    assert_eq!(app.input_mode, InputMode::AccountProjectPick);
    app.handle_key(key(KeyCode::Enter));
    app.answer_demo_lists();
    assert_eq!(app.input_mode, InputMode::AccountZonePick);
    assert_eq!(app.filtered_zones()[app.zone_selected], "europe-west1-b");

    let mut app = App::new_demo();
    app.handle_key(key(KeyCode::Char('a')));
    app.handle_key(key(KeyCode::Char('1')));
    for c in "postgres".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.input_mode, InputMode::AddInstance);
    app.answer_demo_lists();
    assert!(!app.instances.is_empty(), "the instance picker has rows");
}

#[test]
fn a_failed_load_reaches_the_status_line_and_leaves_the_list_loading() {
    let mut app = App::new("dev");
    app.load_failed("tunnels.toml is not valid");
    let status = app.status.clone().expect("a status message");
    assert!(status.contains("tunnels.toml is not valid"), "{status}");
    assert!(app.status_error, "a failure must be marked");
    assert!(!app.loaded, "a failed load is not an answer");
}

pub(super) fn keymap_fixture() -> App {
    let mut app = App::new("dev");
    app.accounts = vec!["dev".to_string(), "prod".to_string()];
    app.entries = vec![
        TunnelEntry::new(
            access("db", 15432),
            TunnelStatus::Running,
            TunnelKind::Managed,
        ),
        TunnelEntry::new(
            access("cache", 16379),
            TunnelStatus::Running,
            TunnelKind::Managed,
        ),
        TunnelEntry::new(
            access("queue", 15672),
            TunnelStatus::Running,
            TunnelKind::Managed,
        ),
    ];
    app.entries[1].health = HealthStatus::Healthy;
    app.selected = 1;
    app
}

// Stand-ins for the clipboard and the browser. `App::new` already refuses
// both under test, so a test that forgets to install these fails loudly
// instead of copying to the clipboard of the machine running it.
pub(super) fn copy_ok(_: &str) -> anyhow::Result<()> {
    Ok(())
}

pub(super) fn open_ok(_: &str) -> anyhow::Result<()> {
    Ok(())
}

fn copy_fails(_: &str) -> anyhow::Result<()> {
    anyhow::bail!("no clipboard")
}

fn open_fails(_: &str) -> anyhow::Result<()> {
    anyhow::bail!("no browser")
}

#[test]
fn c_and_o_report_a_clipboard_or_browser_that_failed() {
    let mut app = keymap_fixture();
    app.clipboard = copy_fails;
    app.opener = open_fails;
    app.logs = vec!["line 1".to_string()];
    app.handle_key(key(KeyCode::Char('c')));
    assert_eq!(app.status.as_deref(), Some("could not copy: no clipboard"));
    app.handle_key(key(KeyCode::Char('l')));
    assert_eq!(
        app.status.as_deref(),
        Some("could not copy log: no clipboard")
    );
    app.handle_key(key(KeyCode::Char('o')));
    assert_eq!(
        app.status.as_deref(),
        Some("could not open http://localhost:16379: no browser")
    );
}

#[test]
fn l_on_an_empty_log_says_so() {
    let mut app = keymap_fixture();
    app.clipboard = copy_ok;
    app.logs.clear();
    app.handle_key(key(KeyCode::Char('l')));
    assert_eq!(app.status.as_deref(), Some("log for cache is empty"));
}

#[test]
fn l_copies_the_log_and_triggers_a_flash_message() {
    let mut app = keymap_fixture();
    app.clipboard = copy_ok;
    app.logs = vec![
        "sample log line 1".to_string(),
        "sample log line 2".to_string(),
    ];
    app.handle_key(key(KeyCode::Char('l')));
    assert_eq!(app.status.as_deref(), Some("copied 2 log lines for cache"));
    assert_eq!(app.flash_message(), Some("copied 2 log lines for cache"));
}

#[test]
fn c_and_o_do_nothing_without_a_selected_tunnel() {
    let mut app = App::new("dev");
    app.clipboard = copy_ok;
    app.opener = open_ok;
    assert!(app.handle_key(key(KeyCode::Char('c'))).is_none());
    assert!(app.handle_key(key(KeyCode::Char('l'))).is_none());
    assert!(app.handle_key(key(KeyCode::Char('o'))).is_none());
    assert_eq!(app.status, None);
}

// The scroll keys are the help page's alone: in the list `j` and `k` move the
// selection and the rest are not bound, and none may leak a position into a
// page that is closed
#[test]
fn the_scroll_keys_leave_the_help_position_alone_outside_the_page() {
    let mut app = app_with_two();
    for code in [
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::PageDown,
        KeyCode::PageUp,
        KeyCode::Char('g'),
        KeyCode::Char('G'),
    ] {
        app.handle_key(key(code));
        assert_eq!(app.help_scroll, 0, "{code:?}");
        assert_eq!(app.input_mode, InputMode::Normal, "{code:?}");
    }
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

// The subtitle under the account name is wrong the moment it lags the name,
// so both must come from the same switch.
#[test]
fn semicolon_carries_the_project_with_the_account() {
    let mut app = app_with_two();
    app.accounts = vec!["dev".to_string(), "prod".to_string()];
    app.projects = [
        ("dev".to_string(), "sample-dev-1".to_string()),
        ("prod".to_string(), "sample-prod-2".to_string()),
    ]
    .into();
    app.account = "dev".to_string();
    assert_eq!(app.project(), "sample-dev-1");
    app.handle_key(key(KeyCode::Char(';')));
    assert_eq!(app.account, "prod");
    assert_eq!(app.project(), "sample-prod-2");
    app.projects.clear();
    assert_eq!(app.project(), "", "an unknown account has no project");
}

// The relay was read once for the account on screen, so `;` left the next
// account with the first one's Public flow and domain picker.
#[test]
fn semicolon_carries_the_relay_with_the_account() {
    use crate::config::{Account, Config, DomainConfig, ExposeConfig};
    let account = |name: &str, expose: Option<ExposeConfig>| Account {
        name: name.to_string(),
        gcloud_configuration: name.to_string(),
        project: format!("sample-{name}"),
        default_zone: "europe-west1-b".to_string(),
        impersonate_service_account: None,
        expose,
    };
    let relay = ExposeConfig {
        relay_instance: "relay".to_string(),
        relay_zone: "europe-west1-b".to_string(),
        network: "default".to_string(),
        default_domain: "app.example.test".to_string(),
        domains: vec![DomainConfig {
            domain: "app.example.test".to_string(),
            managed_zone: "app-zone".to_string(),
        }],
    };
    let config = Config {
        selected_account: "dev".to_string(),
        accounts: vec![account("dev", Some(relay)), account("prod", None)],
        ..Default::default()
    };
    let mut app = App::new("dev");
    app.reload_config(&config);
    assert!(app.has_relay, "dev has a relay");
    assert_eq!(app.domains, vec!["app.example.test".to_string()]);

    app.handle_key(key(KeyCode::Char(';')));
    assert_eq!(app.account, "prod");
    assert!(!app.has_relay, "prod has none, the name alone moved");
    assert!(app.domains.is_empty(), "{:?}", app.domains);
    assert_eq!(keys(&mut app, "a2"), None);
    let status = app.status.clone().expect("a status message");
    assert!(status.contains("relay up"), "{status}");

    app.input_mode = InputMode::Normal;
    app.handle_key(key(KeyCode::Char(';')));
    assert_eq!(app.account, "dev");
    assert!(app.has_relay, "coming back restores the relay");
    assert_eq!(app.domains, vec!["app.example.test".to_string()]);
}

// Cells, not text: the list is a table only if every row's columns start
// where the header's do, which a substring search cannot say
pub(super) fn screen(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

pub(super) fn keys(app: &mut App, sequence: &str) -> Option<Op> {
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

pub(super) fn at_the_name_prompt() -> App {
    let mut app = app_with_two();
    keys(&mut app, "a1");
    assert_eq!(app.input_mode, InputMode::AddName);
    app
}

#[test]
fn a_name_the_account_already_holds_is_refused_in_the_dialog() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "db\n");
    assert_eq!(app.input_mode, InputMode::AddName, "advanced past a clash");
    assert_eq!(app.input, "db", "the typed text was discarded");
    let error = app.draft_error.as_deref().expect("no message");
    assert!(error.contains("'db'"), "does not name the clash: {error}");
}

#[test]
fn a_name_that_collides_with_an_unmanaged_row_is_refused() {
    let mut app = at_the_name_prompt();
    app.entries.push(TunnelEntry::new(
        access("cache", 16379),
        TunnelStatus::Running,
        TunnelKind::External,
    ));
    keys(&mut app, "cache\n");
    assert_eq!(app.input_mode, InputMode::AddName);
}

#[test]
fn an_invalid_name_is_refused_in_the_dialog() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "Db!\n");
    assert_eq!(app.input_mode, InputMode::AddName);
    assert_eq!(app.input, "Db!");
    let error = app.draft_error.as_deref().expect("no message");
    assert!(error.contains("lowercase"), "reports no rule: {error}");
}

#[test]
fn an_instance_list_that_could_not_be_read_says_so() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "cache\n");
    assert_eq!(app.input_mode, InputMode::AddInstance);
    app.instances_failed("gcloud exited 1");
    assert!(app.status_error);
    let status = app.status.as_deref().expect("no status");
    assert!(status.contains("gcloud exited 1"), "no reason: {status}");
    let error = app
        .draft_error
        .as_deref()
        .expect("nothing inside the dialog");
    assert!(error.contains("gcloud exited 1"), "no reason: {error}");
}

#[test]
fn an_instance_the_picker_never_offered_can_still_be_typed() {
    let mut app = at_the_name_prompt();
    app.default_zone = "europe-west1-b".to_string();
    keys(&mut app, "cache\n");
    app.instances_failed("gcloud exited 1");
    assert!(app.instances.is_empty());
    keys(&mut app, "bastion\n");
    assert_eq!(app.input_mode, InputMode::AddJumpHost);
    assert_eq!(app.draft.instance, "bastion");
    assert_eq!(app.draft.zone, "europe-west1-b");
    assert!(app.draft_error.is_none(), "an accepted name kept the error");
}

#[test]
fn the_suggested_port_is_the_one_the_cli_would_allocate() {
    use crate::state::{allocate_port, ACCESS_PORT_BASE, ACCESS_PORT_SPAN};
    let mut app = app_with_two();
    let first = allocate_port(
        "cache",
        ACCESS_PORT_BASE,
        ACCESS_PORT_SPAN,
        &Default::default(),
        |_| true,
    )
    .expect("a free port");
    assert_eq!(app.suggested_local_port("cache"), first);

    // Another account holds the name's first choice, which `entries` (the
    // selected account only) cannot show
    app.used_ports = [first].into();
    let next = allocate_port(
        "cache",
        ACCESS_PORT_BASE,
        ACCESS_PORT_SPAN,
        &app.used_ports,
        |_| true,
    )
    .expect("a free port");
    assert_ne!(next, first);
    assert_eq!(app.suggested_local_port("cache"), next);
}

#[test]
fn a_typed_instance_takes_the_zone_of_the_account_switched_to() {
    let mut app = app_with_two();
    app.accounts = vec!["dev".to_string(), "prod".to_string()];
    app.zones = [
        ("dev".to_string(), "europe-west1-b".to_string()),
        ("prod".to_string(), "europe-west4-a".to_string()),
    ]
    .into();
    app.default_zone = "europe-west1-b".to_string();
    keys(&mut app, ";");
    assert_eq!(app.account, "prod");
    keys(&mut app, "a1cache\nbastion\n");
    assert_eq!(app.input_mode, InputMode::AddJumpHost);
    assert_eq!(app.draft.zone, "europe-west4-a");
}

#[test]
fn a_filter_that_matches_none_of_the_listed_instances_accepts_what_was_typed() {
    let mut app = at_the_name_prompt();
    app.default_zone = "europe-west1-b".to_string();
    app.instances = vec![InstanceChoice {
        name: "my-vm".to_string(),
        zone: "europe-west4-a".to_string(),
    }];
    keys(&mut app, "cache\nbastion\n");
    assert_eq!(app.input_mode, InputMode::AddJumpHost);
    assert_eq!(app.draft.instance, "bastion");
    assert_eq!(app.draft.zone, "europe-west1-b");
}

#[test]
fn accepting_a_typed_instance_clears_the_failed_list_error_from_the_status_line() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "cache\n");
    app.instances_failed("gcloud exited 1");
    keys(&mut app, "bastion\n");
    assert!(!app.status_error, "a standing error about the list");
    let status = app.status.as_deref().unwrap_or("");
    assert!(!status.contains("gcloud exited 1"), "{status}");
}

#[test]
fn a_failed_instance_list_becomes_a_message_carrying_the_reason() {
    let failed = instances_msg(Err(anyhow::anyhow!("gcloud exited 1")));
    match failed {
        Msg::InstancesFailed(reason) => assert_eq!(reason, "gcloud exited 1"),
        _ => panic!("the error was dropped"),
    }
    assert!(matches!(instances_msg(Ok(Vec::new())), Msg::Instances(_)));
}

#[test]
fn enter_on_an_empty_instance_step_asks_for_a_name_inside_the_dialog() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "cache\n\n");
    assert_eq!(app.input_mode, InputMode::AddInstance);
    assert!(app.draft_error.is_some());
}

#[test]
fn a_valid_new_name_advances_to_the_instance_step() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "cache\n");
    assert_eq!(app.input_mode, InputMode::AddInstance);
    assert_eq!(app.draft.name, "cache");
    assert!(app.draft_error.is_none());
}

#[test]
fn typing_again_clears_the_refusal() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "db\n");
    keys(&mut app, "2");
    assert!(app.draft_error.is_none());
}

#[test]
fn adding_an_access_tunnel_walks_the_prompts_and_creates_it() {
    let mut app = app_with_two();
    app.instances = vec![InstanceChoice {
        name: "my-vm".to_string(),
        zone: "europe-west1-b".to_string(),
    }];

    // a, mode 1, name, instance, host (empty: the instance itself), ports
    let op = keys(&mut app, "a1cache\n\n\n5432\n\n");
    assert_eq!(
        op,
        Some(Op::Create(NewTunnel {
            account: "dev".to_string(),
            name: "cache".to_string(),
            mode: TunnelMode::Access,
            instance: "my-vm".to_string(),
            zone: "europe-west1-b".to_string(),
            remote_port: 5432,
            local_port: 15870,
            ..NewTunnel::default()
        }))
    );
    assert_eq!(app.input_mode, InputMode::Normal);
}

pub(super) fn failed(message: &str) -> OpResult {
    OpResult {
        message: message.to_string(),
        ok: false,
        reload: false,
        from_create: true,
        from_account: false,
        name: None,
    }
}

fn unrelated(ok: bool, message: &str) -> OpResult {
    OpResult {
        message: message.to_string(),
        ok,
        reload: false,
        from_create: false,
        from_account: false,
        name: None,
    }
}

pub(super) fn a_stopped_app() -> App {
    let mut app = app_with_two();
    app.entries[0].status = TunnelStatus::Stopped;
    app
}

#[test]
fn a_second_start_on_a_pending_row_is_refused() {
    let mut app = a_stopped_app();
    assert!(app.handle_key(key(KeyCode::Char('s'))).is_some());
    for k in ['s', 'S', 'R', 'A', 'd'] {
        assert_eq!(app.handle_key(key(KeyCode::Char(k))), None, "{k}");
        assert_eq!(
            app.status.as_deref(),
            Some("db: start already in progress"),
            "{k}"
        );
    }
    assert_eq!(app.input_mode, InputMode::Normal);
}

#[test]
fn a_round_begun_before_an_op_finished_is_not_applied() {
    let mut app = a_stopped_app();
    let mut timers = Timers::due();
    let begun_before = timers.entries.begin();
    // What `OpDone` does before it applies the result
    timers.entries.invalidate();
    timers.entries.force();
    let stale = vec![TunnelEntry::new(
        access("db", 15432),
        TunnelStatus::Running,
        TunnelKind::Managed,
    )];
    apply_polled(
        &mut app,
        &mut timers,
        Msg::Entries(begun_before, stale, HashSet::new()),
    );
    assert_eq!(app.entries.len(), 2, "the stale round must not replace");
    assert_eq!(app.entries[0].status, TunnelStatus::Stopped);
    assert!(timers.entries.due(Duration::from_secs(5)));
}

#[test]
fn pending_clears_on_the_first_round_after_the_op() {
    let mut app = a_stopped_app();
    app.handle_key(key(KeyCode::Char('s')));
    let fresh = |status| {
        vec![TunnelEntry::new(
            access("db", 15432),
            status,
            TunnelKind::Managed,
        )]
    };
    // A round that lands while the op still runs keeps the row pending
    app.replace_entries(fresh(TunnelStatus::Stopped), HashSet::new());
    assert!(app.entries[0].pending.is_some());
    app.apply(OpResult {
        message: "db running".to_string(),
        ok: true,
        reload: true,
        from_create: false,
        from_account: false,
        name: Some("db".to_string()),
    });
    assert!(app.entries[0].pending.is_some(), "not until a round lands");
    app.replace_entries(fresh(TunnelStatus::Running), HashSet::new());
    assert!(app.entries[0].pending.is_none());
    assert!(!app.entries[0].pending_settle);
}

pub(super) fn submitted_access_draft() -> App {
    let mut app = app_with_two();
    app.instances = vec![InstanceChoice {
        name: "my-vm".to_string(),
        zone: "europe-west1-b".to_string(),
    }];
    keys(&mut app, "a1cache\n\n\n5432\n\n");
    assert_eq!(app.input_mode, InputMode::Normal);
    app
}

#[test]
fn a_create_that_failed_brings_the_draft_back() {
    let mut app = submitted_access_draft();
    app.apply(failed("instance not found"));

    assert_eq!(app.input_mode, InputMode::AddLocalPort);
    assert_eq!(app.draft.name, "cache");
    assert_eq!(app.draft.instance, "my-vm");
    assert_eq!(app.draft.remote_port, 5432);
    assert_eq!(app.draft.local_port, 15870);
    assert_eq!(app.input, "15870");
    assert_eq!(app.draft_error.as_deref(), Some("instance not found"));
}

#[test]
fn another_ops_failure_does_not_reopen_the_creates_draft() {
    let mut app = submitted_access_draft();
    app.apply(unrelated(false, "no such job"));
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.draft_error.is_none());
    app.apply(failed("instance not found"));
    assert_eq!(
        app.input_mode,
        InputMode::AddLocalPort,
        "the draft was lost"
    );
    assert_eq!(app.draft_error.as_deref(), Some("instance not found"));
}

#[test]
fn another_ops_success_does_not_swallow_the_creates_draft() {
    let mut app = submitted_access_draft();
    app.apply(unrelated(true, "db stopped"));
    app.apply(failed("instance not found"));
    assert_eq!(
        app.input_mode,
        InputMode::AddLocalPort,
        "the draft was lost"
    );
    assert_eq!(app.draft.name, "cache");
}

#[test]
fn a_failed_create_does_not_overwrite_a_second_add_in_progress() {
    let mut app = submitted_access_draft();
    keys(&mut app, "a1half");
    app.apply(failed("instance not found"));
    assert_eq!(app.input_mode, InputMode::AddName);
    assert_eq!(app.input, "half");
    assert!(app.draft_error.is_none());
    assert_eq!(app.status.as_deref(), Some("instance not found"));
}

#[test]
fn resubmitting_a_name_the_failed_create_already_saved_is_refused_in_the_dialog() {
    let mut app = submitted_access_draft();
    app.apply(OpResult {
        reload: true,
        ..failed("could not install the job")
    });
    assert!(app.force_refresh, "the saved record must be reloaded");
    // the reload brings in the record the failed create had saved
    let mut saved = app.entries[0].clone();
    saved.tunnel.name = "cache".to_string();
    app.entries.push(saved);
    let op = keys(&mut app, "\n");
    assert_eq!(op, None, "the clash went to the daemon");
    assert_eq!(app.input_mode, InputMode::AddLocalPort);
    let error = app.draft_error.clone().expect("a refusal");
    assert!(error.contains("already used"), "{error}");
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

// What the create op sends to `add`: a jump tunnel names the host as its
// target and the instance as the way to it, a plain one names the instance
#[test]
fn a_draft_with_a_host_is_created_through_its_instance() {
    let mut draft = NewTunnel {
        instance: "bastion".to_string(),
        remote_port: 443,
        ..Default::default()
    };
    assert_eq!(
        ops::add_arguments(&draft),
        ("bastion:443".to_string(), None)
    );
    draft.host = "10.0.0.10".to_string();
    assert_eq!(
        ops::add_arguments(&draft),
        ("10.0.0.10:443".to_string(), Some("bastion".to_string()))
    );
}

// Two accounts on the machine and a `db` already on the other one, so a
// move from `dev` has somewhere to go and something to collide with
pub(super) fn with_a_second_account() -> App {
    let mut app = app_with_two();
    app.accounts = vec!["dev".to_string(), "prod".to_string()];
    app
}

// The name is unique per account, so the clash is knowable before the
// dialog closes; letting it reach `rename` makes it a failure afterwards
#[test]
fn renaming_from_the_tui_refuses_a_taken_name_in_the_dialog() {
    let mut app = with_a_second_account();
    keys(&mut app, "e2");
    for _ in 0.."db".len() {
        app.handle_key(key(KeyCode::Backspace));
    }
    assert_eq!(keys(&mut app, "api\n"), None, "api is taken");
    assert_eq!(app.input_mode, InputMode::RenameTo, "the dialog stays");
    assert_eq!(app.input, "api", "the typed text was discarded");
    let said = app.draft_error.clone().expect("a reason beside the field");
    assert!(said.contains("'api'"), "{said}");

    for _ in 0.."api".len() {
        app.handle_key(key(KeyCode::Backspace));
    }
    assert_eq!(
        keys(&mut app, "postgres\n"),
        Some(Op::Rename(RenameTunnel {
            account: "dev".to_string(),
            name: "db".to_string(),
            new_name: "postgres".to_string(),
        }))
    );
    assert_eq!(app.input_mode, InputMode::Normal);
}

// A rename rewrites a record in `tunnels.toml` and a move hands one to
// another account. A stranger's process has no record, and a run's lives
// elsewhere until `m` imports it, so neither has anything to rewrite.
#[test]
fn moving_an_unmanaged_or_ephemeral_row_is_refused() {
    let mut app = with_a_second_account();
    app.entries.push(TunnelEntry::new(
        access("cache:6379", 16379),
        TunnelStatus::Running,
        TunnelKind::External,
    ));
    app.selected = app.entries.len() - 1;
    assert_eq!(keys(&mut app, "e"), None);
    assert_eq!(app.input_mode, InputMode::Normal, "no dialog opened");
    let status = app.status.clone().expect("a refusal is said out loud");
    assert!(status.contains("not managed by gtunnel"), "{status}");
    assert!(app.status_error);

    let mut app = with_a_second_account();
    app.entries.push(TunnelEntry::new(
        access("run", 16380),
        TunnelStatus::Running,
        TunnelKind::Ephemeral,
    ));
    app.selected = app.entries.len() - 1;
    assert_eq!(keys(&mut app, "e3"), None);
    assert_eq!(app.input_mode, InputMode::Normal, "the dialog is gone");
    let status = app.status.clone().expect("a refusal is said out loud");
    assert!(status.contains("run"), "{status}");
    assert!(
        status.contains('m'),
        "it says what makes it movable: {status}"
    );
    assert!(app.status_error);
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
    assert_eq!(app.entries.len(), 7);
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
fn the_demo_shows_a_jump_a_dest_group_and_a_run_with_a_stop_time() {
    use crate::state::AccessTarget;
    let app = App::new_demo();
    let target = |name: &str| {
        let e = app.entries.iter().find(|e| e.name() == name).unwrap();
        match &e.tunnel.spec {
            TunnelSpec::Access { target, .. } => target.clone(),
            other => panic!("{name} is not an Access tunnel: {other:?}"),
        }
    };
    assert!(matches!(target("redis"), AccessTarget::Jump { .. }));
    assert!(matches!(target("pg-hosts"), AccessTarget::DestGroup { .. }));
    let run = app
        .entries
        .iter()
        .find(|e| e.name() == "gt-3b2c10")
        .unwrap();
    assert_eq!(run.kind, TunnelKind::Ephemeral);
    assert!(run.tag().unwrap().starts_with("[run, stops at "));
    let ports: Vec<u16> = app
        .used_ports
        .iter()
        .copied()
        .filter(|p| (15000..16000).contains(p))
        .collect();
    assert_eq!(
        ports.len(),
        app.used_ports.len(),
        "a port outside 15000-15999"
    );
    assert!(app.update.is_some(), "the header shows no update");
}

#[test]
fn the_demo_log_is_in_the_normalised_connection_format() {
    let app = App::new_demo();
    let conn: Vec<&String> = app
        .logs
        .iter()
        .filter(|l| l.contains("gtunnel: conn"))
        .collect();
    assert!(!conn.is_empty());
    for line in &app.logs {
        // RFC 3339 stamp to the second, as format_line writes it
        assert_eq!(line.as_bytes()[4], b'-', "{line}");
        assert_eq!(&line[19..21], "Z ", "{line}");
    }
    for line in conn {
        assert!(line.contains(" from 127.0.0.1:"), "{line}");
    }
    assert!(app.logs.iter().any(|l| l.contains(" closed after ")));
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

/// The doc cites code. A citation that no longer resolves is worse than no
/// citation: it sends the next reader to a file that does not exist and
/// quietly teaches them the rule is stale too.
#[test]
fn the_design_doc_cites_only_files_that_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = std::fs::read_to_string(root.join("docs/design.md")).unwrap();
    let mut checked = 0;
    for token in doc.split(|c: char| !(c.is_alphanumeric() || "/._-".contains(c))) {
        if token.starts_with("src/") && token.ends_with(".rs") {
            assert!(
                root.join(token).exists(),
                "docs/design.md cites {token}, which does not exist"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 6,
        "the doctrine should cite the code it describes, found {checked}"
    );
}

#[test]
fn a_crash_loop_is_named_even_when_the_probe_just_passed() {
    let mut app = app_with_two();
    let mut timers = Timers::due();
    let metrics = |restarts| {
        vec![(
            "db".to_string(),
            crate::probe::TunnelMetrics {
                restarts,
                ..Default::default()
            },
        )]
    };
    for restarts in [0, 1, 3] {
        let generation = timers.metrics.begin();
        apply_polled(
            &mut app,
            &mut timers,
            Msg::Metrics(generation, metrics(restarts)),
        );
    }
    let generation = timers.health.begin();
    let results = vec![("db".to_string(), HealthStatus::Healthy)];
    apply_polled(&mut app, &mut timers, Msg::Health(generation, results));
    assert_eq!(app.entries[0].health, HealthStatus::CrashLooping);
    assert_eq!(app.entries[1].health, HealthStatus::Unknown);
}

#[test]
fn expired_credentials_outrank_a_crash_loop_and_both_count_as_failing() {
    let mut app = app_with_two();
    let mut timers = Timers::due();
    app.entries[0].crash_looping = true;
    let generation = timers.health.begin();
    let results = vec![("db".to_string(), HealthStatus::AuthExpired)];
    apply_polled(&mut app, &mut timers, Msg::Health(generation, results));
    assert_eq!(app.entries[0].health, HealthStatus::AuthExpired);
    assert!(HealthStatus::AuthExpired.is_failing());
    assert!(HealthStatus::CrashLooping.is_failing());
    assert!(!HealthStatus::Checking.is_failing());
}

// The accounts page's edit dialog, at its first step with the cursor on dev
pub(super) fn at_the_project_step() -> App {
    let mut app = app_with_two();
    app.accounts = vec!["dev".to_string(), "prod".to_string()];
    app.projects = [
        ("dev".to_string(), "example-dev-1a2b".to_string()),
        ("prod".to_string(), "example-prod-3c4d".to_string()),
    ]
    .into();
    app.zones = [("dev".to_string(), "europe-west1-b".to_string())].into();
    keys(&mut app, ":e");
    assert_eq!(app.input_mode, InputMode::AccountProject);
    app
}

pub(super) fn listed_projects(ids: &[&str]) -> Vec<ProjectChoice> {
    ids.iter()
        .map(|id| ProjectChoice {
            project_id: (*id).to_string(),
            name: String::new(),
        })
        .collect()
}

#[test]
fn the_project_step_opens_a_picker_once_the_list_arrives() {
    let mut app = at_the_project_step();
    assert!(app.want_projects, "the loop was never asked for the list");
    app.projects_arrived(
        "dev",
        listed_projects(&["example-dev-1a2b", "example-prod-3c4d"]),
    );
    assert_eq!(app.input_mode, InputMode::AccountProjectPick);
    assert_eq!(app.filtered_projects().len(), 2);
}

#[test]
fn enter_keeps_the_current_project() {
    let mut app = at_the_project_step();
    app.projects_arrived(
        "dev",
        listed_projects(&["example-a-0000", "example-dev-1a2b", "example-prod-3c4d"]),
    );
    assert_eq!(app.project_selected, 1, "the current row is not selected");
    keys(&mut app, "\n");
    assert_eq!(app.account_draft.project, "example-dev-1a2b");
    assert_eq!(app.input_mode, InputMode::AccountZone);
    assert_eq!(app.input, "europe-west1-b");
}

#[test]
fn a_current_project_the_list_lacks_is_still_kept_by_enter() {
    let mut app = at_the_project_step();
    app.projects_arrived("dev", listed_projects(&["example-prod-3c4d"]));
    keys(&mut app, "\n");
    assert_eq!(app.account_draft.project, "example-dev-1a2b");
    assert_eq!(app.input_mode, InputMode::AccountZone);
}

#[test]
fn a_row_chosen_with_the_arrows_replaces_the_current_project() {
    let mut app = at_the_project_step();
    app.projects_arrived(
        "dev",
        listed_projects(&["example-dev-1a2b", "example-prod-3c4d"]),
    );
    keys(&mut app, "\u{2193}\n");
    assert_eq!(app.account_draft.project, "example-prod-3c4d");
    assert_eq!(app.input_mode, InputMode::AccountZone);
}

#[test]
fn a_filter_that_matches_nothing_is_taken_as_the_project() {
    let mut app = at_the_project_step();
    app.projects_arrived("dev", listed_projects(&["example-dev-1a2b"]));
    keys(&mut app, "example-prod-3c4d\n");
    assert_eq!(app.account_draft.project, "example-prod-3c4d");
    assert_eq!(app.input_mode, InputMode::AccountZone);
}

#[test]
fn a_failed_list_falls_back_to_the_text_field_prefilled_with_the_current_project() {
    let mut app = at_the_project_step();
    app.projects_failed("dev", "GCP API error 403: denied");
    assert_eq!(app.input_mode, InputMode::AccountProject);
    assert_eq!(app.input, "example-dev-1a2b");
    assert!(app.status_error);
    let status = app.status.as_deref().expect("no status");
    assert!(status.starts_with("Could not list projects: "), "{status}");
    assert!(status.contains("403"), "{status}");
    keys(&mut app, "\n");
    assert_eq!(app.input_mode, InputMode::AccountZone);
    assert!(!app.status_error, "the list's error outlived the field");
}

#[test]
fn an_empty_list_falls_back_to_the_text_field() {
    let mut app = at_the_project_step();
    app.projects_arrived("dev", Vec::new());
    assert_eq!(app.input_mode, InputMode::AccountProject);
    assert_eq!(app.input, "example-dev-1a2b");
    let status = app.status.as_deref().expect("no status");
    assert!(status.starts_with("Could not list projects: "), "{status}");
}

#[test]
fn a_list_for_another_account_or_a_closed_dialog_is_ignored() {
    let mut app = at_the_project_step();
    app.projects_arrived("prod", listed_projects(&["example-prod-3c4d"]));
    assert_eq!(app.input_mode, InputMode::AccountProject);
    app.handle_key(key(KeyCode::Esc));
    app.projects_arrived("dev", listed_projects(&["example-dev-1a2b"]));
    assert_eq!(app.input_mode, InputMode::Accounts);
    app.projects_failed("dev", "late");
    assert!(app.status.is_none(), "a stale error reached the line");
}

#[test]
fn a_list_does_not_replace_a_project_the_person_is_already_typing() {
    let mut app = at_the_project_step();
    keys(&mut app, "x");
    app.projects_arrived("dev", listed_projects(&["example-dev-1a2b"]));
    assert_eq!(app.input_mode, InputMode::AccountProject);
    assert_eq!(app.input, "example-dev-1a2bx");
}

#[test]
fn escape_in_the_project_picker_returns_to_the_accounts_page() {
    let mut app = at_the_project_step();
    app.projects_arrived("dev", listed_projects(&["example-dev-1a2b"]));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.input_mode, InputMode::Accounts);
}

#[test]
fn a_failed_project_list_becomes_a_message_carrying_the_reason() {
    match projects_msg("dev", Err(anyhow::anyhow!("denied"))) {
        Msg::ProjectsFailed(account, reason) => {
            assert_eq!(account, "dev");
            assert_eq!(reason, "denied");
        }
        _ => panic!("the error was dropped"),
    }
    assert!(matches!(
        projects_msg("dev", Ok(Vec::new())),
        Msg::Projects(..)
    ));
}

#[test]
fn a_steady_round_trip_does_not_fill_the_chart() {
    let samples: Vec<u64> = (0..60).map(|i| 120_000 + (i % 5) * 1_000).collect();
    let (drawn, top) = ui::chart_scale(&samples);
    assert!(drawn.iter().all(|d| *d < top / 2), "{drawn:?} of {top}");
    assert!(drawn.iter().all(|d| *d > 0));
}

#[test]
fn one_spike_reaches_the_top_and_leaves_the_rest_above_the_floor() {
    let mut samples = vec![120_000u64; 59];
    samples.push(400_000);
    let (drawn, top) = ui::chart_scale(&samples);
    assert_eq!(drawn[59], top);
    assert!(drawn[..59].iter().all(|d| *d > 0 && *d < top));
}

#[test]
fn identical_samples_draw_no_full_height_column() {
    let (drawn, top) = ui::chart_scale(&[40_000; 30]);
    assert!(drawn.iter().all(|d| *d < top));
}

#[test]
fn a_two_millisecond_tunnel_still_draws_a_chart() {
    let samples = [1_500u64, 2_000, 2_500, 9_000, 2_000];
    let (drawn, top) = ui::chart_scale(&samples);
    assert_eq!(top, 10_000);
    assert!(drawn.iter().max() > drawn.iter().min());
}

#[test]
fn every_form_field_has_a_description() {
    for mode in [
        InputMode::AccountConfiguration,
        InputMode::AccountConfigurationPick,
        InputMode::AccountName,
        InputMode::AccountProject,
        InputMode::AccountProjectPick,
        InputMode::AccountZone,
        InputMode::AccountZonePick,
        InputMode::AddName,
        InputMode::AddInstance,
        InputMode::AddJumpHost,
        InputMode::AddRemotePort,
        InputMode::AddLocalPort,
        InputMode::AddLocalTarget,
        InputMode::AddDomain,
        InputMode::EditTarget,
    ] {
        assert!(!mode.describe().is_empty(), "{mode:?} has no description");
    }
}

#[test]
fn the_add_form_counts_its_steps_per_mode() {
    assert_eq!(InputMode::AddName.step(TunnelMode::Access), Some((1, 5)));
    assert_eq!(
        InputMode::AddLocalPort.step(TunnelMode::Access),
        Some((5, 5))
    );
    assert_eq!(InputMode::AddName.step(TunnelMode::Public), Some((1, 3)));
    assert_eq!(InputMode::AddDomain.step(TunnelMode::Public), Some((3, 3)));
    assert_eq!(InputMode::RenameTo.step(TunnelMode::Access), None);
}

#[test]
fn the_name_prompt_shows_its_step_and_what_a_name_is() {
    let app = at_the_name_prompt();
    let text = rows_text(&screen(&app, 100, 30));
    assert!(text.contains("New access tunnel 1/5"), "{text}");
    assert!(text.contains("never the account"), "{text}");
}

#[test]
fn a_short_terminal_sheds_the_description_before_the_field() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "zz");
    let text = rows_text(&screen(&app, 80, 7));
    assert!(text.contains("zz"), "the field is gone:\n{text}");
    assert!(!text.contains("never the account"), "{text}");
}

#[test]
fn the_edit_dialog_names_the_value_it_edits() {
    let mut app = app_with_two();
    keys(&mut app, "e1");
    let text = rows_text(&screen(&app, 100, 30));
    assert!(text.contains("Ports (remote:local):"), "{text}");

    let mut app = app_with_two();
    keys(&mut app, "\u{2193}e1");
    let text = rows_text(&screen(&app, 100, 30));
    assert!(text.contains("Local target:"), "{text}");
    assert!(!text.contains("Ports (remote:local)"), "{text}");
}

fn rows_text(buffer: &ratatui::buffer::Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn frame(app: &App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal.draw(|f| ui::render(f, app)).expect("draw");
    rows_text(terminal.backend().buffer())
}

// An empty list, whether still loading after `;` or loaded and empty, has no
// row for `m import` to act on, so the footer never offers it there: it was a
// dead key in an empty account and flashed on every account switch
#[test]
fn an_empty_list_never_offers_import_loading_or_loaded() {
    let mut app = App::new("dev");
    assert!(!app.loaded);
    let waiting = frame(&app, 120, 30);
    assert!(waiting.contains("loading"), "{waiting}");
    assert!(!waiting.contains("m import"), "{waiting}");

    app.loaded = true;
    let empty = frame(&app, 120, 30);
    assert!(empty.contains("no tunnels in this account"), "{empty}");
    assert!(!empty.contains("m import"), "{empty}");
    assert!(empty.contains("a add"), "{empty}");
}

// Before the instances arrive the picker is waiting, not empty: `nothing
// matches` read as a project with no instances
#[test]
fn the_instance_picker_says_loading_before_the_list_and_nothing_matches_after() {
    let mut app = App::new("dev");
    app.handle_key(key(KeyCode::Char('a')));
    app.handle_key(key(KeyCode::Char('1')));
    for c in "postgres".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.input_mode, InputMode::AddInstance);
    let waiting = frame(&app, 120, 30);
    assert!(waiting.contains("loading instances"), "{waiting}");
    assert!(!waiting.contains("nothing matches"), "{waiting}");
    app.instances_arrived(Vec::new());
    let answered = frame(&app, 120, 30);
    assert!(answered.contains("nothing matches"), "{answered}");
    assert!(!answered.contains("loading instances"), "{answered}");
}

// A run is in the ephemeral list and not in `tunnels.toml`, so the op that
// stops a managed tunnel would look it up and fail with not found
#[test]
fn stopping_a_run_sends_the_op_that_ends_a_run() {
    let mut app = app_with_two();
    app.entries[0].kind = TunnelKind::Ephemeral;
    assert_eq!(
        app.handle_key(key(KeyCode::Char('S'))),
        Some(Op::StopRun("db".to_string()))
    );
}

// A run has no service to reinstall, so there is nothing to restart
#[test]
fn restarting_a_run_is_refused_and_says_to_import_it() {
    let mut app = app_with_two();
    app.entries[0].kind = TunnelKind::Ephemeral;
    assert_eq!(app.handle_key(key(KeyCode::Char('R'))), None);
    assert_eq!(
        app.status.as_deref(),
        Some("db is a run: import it with m before restarting it")
    );
    assert!(app.status_error);
    assert!(app.entries[0].pending.is_none());
}
