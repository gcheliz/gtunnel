use std::time::Duration;

use super::app::{
    App, ConfigurationChoice, InstanceChoice, ProjectChoice, TunnelEntry, TunnelKind,
    SPARKLINE_SAMPLES,
};
use crate::gcp::monitoring::LbStats;
use crate::probe::{HealthStatus, MetricsHistory, TunnelMetrics};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};

// Every row of the demo has a log of its own, written in the shapes the real
// files hold: gtunnel's `gtunnel: ...` events, which `daemon::log_event` stamps,
// then gcloud's own lines, then the normalised `gtunnel: conn <id> ...` lines
// that `logfilter` pairs per connection. The first stamp of a row is a fixed
// date, so the lines read the same in every recording and no clock is involved.
const LOG_START: &str = "2026-10-27T09:14:02Z";

// How long each demo connection lasts, in tenths of a second, cycling. Fixed so
// the lines a recording shows do not depend on when it was made.
const CONN_TENTHS: [u64; 4] = [32, 124, 18, 75];

// Ticks between one connection event and the next: 25 of `TICK`, about 3 seconds
const EVENT_EVERY: u32 = 25;

fn access(
    account: &str,
    name: &str,
    instance: &str,
    remote: u16,
    local: u16,
    auto: bool,
) -> PersistentTunnel {
    PersistentTunnel {
        name: name.to_string(),
        account_name: account.to_string(),
        auto_start: auto,
        spec: TunnelSpec::Access {
            target: AccessTarget::Instance {
                instance: instance.to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: remote,
            local_port: local,
        },
    }
}

// A tunnel through a jump host: the instance is the hop, the host is the goal
fn jump(name: &str, host: &str, remote: u16, local: u16) -> PersistentTunnel {
    PersistentTunnel {
        name: name.to_string(),
        account_name: "demo".to_string(),
        auto_start: false,
        spec: TunnelSpec::Access {
            target: AccessTarget::Jump {
                instance: "bastion".to_string(),
                zone: "europe-west1-b".to_string(),
                host: host.to_string(),
            },
            remote_port: remote,
            local_port: local,
        },
    }
}

// A tunnel to a destination group, which has no instance to name
fn dest_group(name: &str, host: &str, remote: u16, local: u16) -> PersistentTunnel {
    PersistentTunnel {
        name: name.to_string(),
        account_name: "demo".to_string(),
        auto_start: false,
        spec: TunnelSpec::Access {
            target: AccessTarget::DestGroup {
                host: host.to_string(),
                region: "europe-west1".to_string(),
                group: "prod-hosts".to_string(),
                network: "default".to_string(),
            },
            remote_port: remote,
            local_port: local,
        },
    }
}

fn public(name: &str, host: &str, target: &str, port: u16, private: bool) -> PersistentTunnel {
    PersistentTunnel {
        name: name.to_string(),
        account_name: "demo".to_string(),
        auto_start: false,
        spec: TunnelSpec::Expose {
            local_target: target.to_string(),
            domain: "demo.example.com".to_string(),
            hostname: host.to_string(),
            relay_port: port,
            private,
        },
    }
}

// Samples are written in milliseconds for the eye and recorded in the
// microseconds the history holds, the same unit a live probe records
fn history(millis: &[u64]) -> MetricsHistory {
    let mut h = MetricsHistory::new(SPARKLINE_SAMPLES);
    for ms in millis {
        h.record(ms * 1_000);
    }
    h
}

fn metrics(latency: u64, connections: u64, uptime_secs: u64, lb: Option<LbStats>) -> TunnelMetrics {
    TunnelMetrics {
        latency: Some(Duration::from_millis(latency)),
        active_connections: connections,
        uptime: Some(Duration::from_secs(uptime_secs)),
        restarts: 0,
        lb,
        available: true,
        health: None,
    }
}

fn lb_stats(total: u64) -> LbStats {
    let mut by_class = std::collections::BTreeMap::new();
    by_class.insert("2xx".to_string(), total - 3);
    by_class.insert("4xx".to_string(), 2);
    by_class.insert("5xx".to_string(), 1);
    LbStats { total, by_class }
}

// The account with a relay: access, public, ephemeral and unmanaged tunnels
fn demo_entries() -> Vec<TunnelEntry> {
    let mut db = TunnelEntry::new(
        access("demo", "db", "postgres-1", 5432, 15432, true),
        TunnelStatus::Running,
        TunnelKind::Managed,
    );
    db.metrics = Some(metrics(14, 3, 9_120, None));
    db.history = history(&[11, 13, 12, 18, 14, 15, 21, 14, 12, 13, 14, 16, 14]);
    db.health = HealthStatus::Healthy;

    let mut cache = TunnelEntry::new(
        jump("redis", "cache.example.internal", 6379, 15381),
        TunnelStatus::Running,
        TunnelKind::Managed,
    );
    cache.metrics = Some(metrics(19, 2, 6_480, None));
    cache.history = history(&[17, 19, 22, 18, 19, 20, 17, 19, 23, 19, 18, 19]);
    cache.health = HealthStatus::Healthy;

    let mut hosts = TunnelEntry::new(
        dest_group("pg-hosts", "10.0.0.5", 5432, 15433),
        TunnelStatus::Running,
        TunnelKind::Managed,
    );
    hosts.metrics = Some(metrics(27, 1, 2_760, None));
    hosts.history = history(&[25, 28, 27, 31, 26, 27, 29, 25, 27, 28, 27, 26]);
    hosts.health = HealthStatus::Healthy;

    // What an agent opened with `gtunnel_run_tunnel`: it lives 25 minutes from
    // now, so the row and its details always show a stop time still ahead
    let mut run = TunnelEntry::new(
        access("demo", "gt-3b2c10", "bastion", 5432, 15900, false),
        TunnelStatus::Running,
        TunnelKind::Ephemeral,
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    run.expires_at = Some(now + 25 * 60);
    run.metrics = Some(metrics(16, 1, 300, None));
    run.history = history(&[15, 17, 16, 14, 18, 16, 15, 16, 17, 16]);
    run.health = HealthStatus::Healthy;

    // Someone else's gcloud process, which gtunnel reports but never touches
    let mut redis = TunnelEntry::new(
        access("demo", "cache-1:6379", "cache-1", 6379, 15379, false),
        TunnelStatus::Running,
        TunnelKind::External,
    );
    redis.health = HealthStatus::Unknown;

    let mut api = TunnelEntry::new(
        public(
            "api",
            "api.demo.example.com",
            "localhost:3000",
            30001,
            false,
        ),
        TunnelStatus::Running,
        TunnelKind::Managed,
    );
    api.metrics = Some(metrics(38, 11, 4_300, Some(lb_stats(1_284))));
    api.history = history(&[31, 44, 38, 35, 52, 41, 37, 39, 36, 38, 40, 38]);
    api.health = HealthStatus::Healthy;

    let mut preview = TunnelEntry::new(
        public(
            "gt-7f3a91",
            "gt-7f3a91.demo.example.com",
            "localhost:8080",
            30002,
            true,
        ),
        TunnelStatus::Running,
        TunnelKind::Ephemeral,
    );
    preview.health = HealthStatus::Unhealthy;

    vec![db, cache, hosts, run, redis, api, preview]
}

// A second account, with no relay: Access tunnels only, one of them stopped
fn staging_entries() -> Vec<TunnelEntry> {
    let mut cache = TunnelEntry::new(
        access("staging", "cache", "cache-2", 6379, 15379, false),
        TunnelStatus::Running,
        TunnelKind::Managed,
    );
    cache.metrics = Some(metrics(22, 1, 1_805, None));
    cache.history = history(&[24, 19, 23, 27, 21, 22, 20, 25, 22, 21, 23, 22]);
    cache.health = HealthStatus::Healthy;

    let bastion = TunnelEntry::new(
        access("staging", "bastion", "bastion-2", 22, 15022, true),
        TunnelStatus::Stopped,
        TunnelKind::Managed,
    );

    vec![cache, bastion]
}

impl App {
    // A fully populated screen that needs no config, no gcloud and no network
    pub fn new_demo() -> Self {
        let mut app = App::new("demo");
        app.demo = true;
        app.accounts = vec!["demo".to_string(), "staging".to_string()];
        app.projects = [
            ("demo".to_string(), "gtunnel-demo-000000".to_string()),
            ("staging".to_string(), "gtunnel-staging-000000".to_string()),
        ]
        .into();
        app.zones = [
            ("demo".to_string(), "europe-west1-b".to_string()),
            ("staging".to_string(), "europe-west4-a".to_string()),
        ]
        .into();
        app.default_zone = "europe-west1-b".to_string();
        // Both accounts', as a real load carries: ports are the laptop's, so the
        // Add dialog must not suggest the other account's
        app.used_ports = demo_entries()
            .into_iter()
            .chain(staging_entries())
            .filter_map(|e| match e.tunnel.spec {
                TunnelSpec::Access { local_port, .. } => Some(local_port),
                _ => None,
            })
            .collect();
        app.load_demo_account();
        // The header's rule carries an available update; a fixed version, so the
        // demo shows one whatever release it is built from
        app.update = Some(crate::update::UpdateNotice {
            latest: "9.9.9".to_string(),
            command: "gtunnel update",
        });
        app.set_status("demo data: nothing here touches GCP");
        app
    }

    // What gcloud would have answered for each list a form asks for. The demo
    // loop calls it after every key, so a picker opens as it would for real;
    // one configuration has no project, so the refusal can be shown too.
    pub fn answer_demo_lists(&mut self) {
        if std::mem::take(&mut self.want_instances) {
            let zone = self.default_zone.clone();
            self.instances_arrived(
                [
                    // name, hidden, iap
                    ("bastion", false, true),
                    ("cache-1", false, false),
                    ("db-1", false, false),
                    ("batch-old", true, false),
                    ("gke-demo-pool-1a2b", true, false),
                ]
                .map(|(name, hidden, iap)| InstanceChoice {
                    name: name.to_string(),
                    zone: zone.clone(),
                    hidden,
                    iap,
                })
                .to_vec(),
            );
        }
        if std::mem::take(&mut self.want_configurations) {
            self.configurations_arrived(vec![
                ConfigurationChoice {
                    name: "demo".to_string(),
                    project: Some("gtunnel-demo-000000".to_string()),
                    is_active: true,
                },
                ConfigurationChoice {
                    name: "scratch".to_string(),
                    project: None,
                    is_active: false,
                },
                ConfigurationChoice {
                    name: "staging".to_string(),
                    project: Some("gtunnel-staging-000000".to_string()),
                    is_active: false,
                },
            ]);
        }
        if std::mem::take(&mut self.want_projects) {
            let account = self.account_draft.name.clone();
            self.projects_arrived(
                &account,
                [
                    ("gtunnel-demo-000000", "Demo"),
                    ("gtunnel-staging-000000", "Staging"),
                ]
                .map(|(id, name)| ProjectChoice {
                    project_id: id.to_string(),
                    name: name.to_string(),
                })
                .to_vec(),
            );
        }
        if std::mem::take(&mut self.want_zones) {
            let (account, project) = (
                self.account_draft.name.clone(),
                self.account_draft.project.clone(),
            );
            let zones = [
                "europe-west1-b",
                "europe-west1-c",
                "europe-west1-d",
                "europe-west4-a",
                "europe-west4-b",
            ];
            self.zones_arrived(&account, &project, zones.map(str::to_string).to_vec());
        }
    }

    // What the runtime would have loaded for the selected account, had there
    // been one. `;` calls this, so cycling accounts keeps the screen filled
    pub fn load_demo_account(&mut self) {
        let staging = self.account == "staging";
        self.loaded = true;
        self.entries = if staging {
            staging_entries()
        } else {
            demo_entries()
        };
        // The other account's rows are other tunnels, so nothing carries over
        self.demo_logs = DemoLogs::default();
        self.clear_logs();
        self.has_relay = !staging;
        self.domains = if staging {
            Vec::new()
        } else {
            vec!["demo.example.com".to_string()]
        };
        self.sync_demo_logs();
    }

    /// Puts the selected row's log in the pane when the selection or the account
    /// has changed, as the real loop refetches on a change of name. The pane is
    /// a copy of the row's file, which lives in the cache: moving the selection
    /// clears the pane before this runs, so the pane cannot be what is kept.
    pub fn sync_demo_logs(&mut self) {
        let key = self
            .selected_entry()
            .map(|e| format!("{}/{}", e.tunnel.account_name, e.name()));
        if key == self.demo_logs.shown {
            return;
        }
        self.log_scroll.follow();
        self.logs = match (&key, self.entries.get(self.selected)) {
            (Some(key), Some(entry)) => {
                if !self.demo_logs.cache.contains_key(key) {
                    let (lines, next) = demo_log(entry);
                    self.demo_logs.cache.insert(key.clone(), lines);
                    self.demo_logs.flow.insert(key.clone(), next);
                }
                self.demo_logs.cache[key].clone()
            }
            _ => Vec::new(),
        };
        self.demo_logs.shown = key;
    }

    /// One tick of the demo's activity: every few seconds the selected row gains
    /// a connection open, and on the next beat its close. Only a row whose log
    /// carries connection lines in real life gets them, so a jump tunnel and a
    /// public URL stay as quiet as they are.
    pub fn tick_demo_logs(&mut self) {
        self.demo_logs.ticks += 1;
        if !self.demo_logs.ticks.is_multiple_of(EVENT_EVERY) {
            return;
        }
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.status != TunnelStatus::Running || !has_connection_lines(&entry.tunnel) {
            return;
        }
        let Some(key) = self.demo_logs.shown.clone() else {
            return;
        };
        let Some(next) = self.demo_logs.flow.get_mut(&key) else {
            return;
        };
        let Some(lines) = self.demo_logs.cache.get_mut(&key) else {
            return;
        };
        let last = lines
            .last()
            .and_then(|l| l.split(' ').next())
            .and_then(|stamp| humantime::parse_rfc3339(stamp).ok());
        let Some(last) = last else {
            return;
        };
        let line = match next.open.take() {
            None => {
                let id = next.id;
                next.id += 1;
                let port = source_port(id);
                next.open = Some((id, port));
                let at = last + Duration::from_secs(6);
                format!(
                    "{} gtunnel: conn iap-{id} open from 127.0.0.1:{port}",
                    humantime::format_rfc3339_seconds(at)
                )
            }
            Some((id, port)) => {
                let tenths = CONN_TENTHS[id as usize % CONN_TENTHS.len()];
                let at = last + Duration::from_secs(tenths.div_ceil(10));
                format!(
                    "{} gtunnel: conn iap-{id} from 127.0.0.1:{port} closed after {}.{}s",
                    humantime::format_rfc3339_seconds(at),
                    tenths / 10,
                    tenths % 10
                )
            }
        };
        lines.push(line);
        let excess = lines.len().saturating_sub(super::ui::LOG_LINES);
        lines.drain(..excess);
        let fresh = lines.clone();
        self.set_logs(fresh);
    }

    /// What the runtime's reload would show once an op had landed. The demo has
    /// no runtime, so without this a stop leaves the row running as it was, and
    /// the Metrics line of a row the reader has just stopped reads healthy.
    pub fn land_demo_op(&mut self, op: &super::app::Op) {
        use super::app::Op;
        let (name, status, health) = match op {
            Op::Stop(name) | Op::StopRun(name) => {
                (name, TunnelStatus::Stopped, HealthStatus::Unknown)
            }
            Op::Start(name) => (name, TunnelStatus::Running, HealthStatus::Healthy),
            _ => return,
        };
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name() == name) {
            entry.status = status;
            entry.health = health;
        }
    }
}

/// What the pane keeps between rows and between ticks.
#[derive(Debug, Clone, Default)]
pub struct DemoLogs {
    // `account/name` of the row whose log is in `App::logs`
    shown: Option<String>,
    // The logs of the rows not on screen
    cache: std::collections::HashMap<String, Vec<String>>,
    // The next connection id and the connection waiting for its close, per row
    flow: std::collections::HashMap<String, Flow>,
    ticks: u32,
}

#[derive(Debug, Clone, Copy)]
struct Flow {
    id: u32,
    open: Option<(u32, u16)>,
}

// The client's port, which is what tells two connections apart on screen
fn source_port(id: u32) -> u16 {
    52_117 + id as u16
}

// A jump tunnel's ssh logs no connection of its own, and an expose tunnel runs
// without `--conns`: only an access tunnel straight to an instance or a
// destination group has the `gtunnel: conn` lines.
fn has_connection_lines(tunnel: &PersistentTunnel) -> bool {
    matches!(
        &tunnel.spec,
        TunnelSpec::Access {
            target: AccessTarget::Instance { .. } | AccessTarget::DestGroup { .. },
            ..
        }
    )
}

/// The log file a row would have, and the id its next connection takes. An
/// unmanaged row has none: gtunnel never installed it, so there is no file to
/// read and the real pane says so too.
fn demo_log(entry: &TunnelEntry) -> (Vec<String>, Flow) {
    let quiet = Flow { id: 1, open: None };
    if entry.kind == TunnelKind::External {
        return (Vec::new(), quiet);
    }
    let t = &entry.tunnel;
    // The two lines every install writes, whatever the tunnel is
    let mut lines = vec![
        format!("{LOG_START} gtunnel: {} -> {}", t.name, t.display_target()),
        format!(
            "{LOG_START} gtunnel: starting {} on {}",
            t.name,
            t.display_url()
        ),
    ];
    if entry.status != TunnelStatus::Running {
        lines.push("2026-10-27T09:40:19Z gtunnel: stopped by gtunnel".to_string());
        return (lines, quiet);
    }
    if !has_connection_lines(t) {
        return (lines, quiet);
    }
    let TunnelSpec::Access { local_port, .. } = &t.spec else {
        return (lines, quiet);
    };
    lines.push("2026-10-27T09:14:03Z Testing if tunnel connection works.".to_string());
    lines.push(format!(
        "2026-10-27T09:14:03Z Listening on port [{local_port}]."
    ));
    // One finished connection, then as many still open as the metrics count,
    // so the pane and the Metrics line tell the same story
    lines.push(format!(
        "2026-10-27T09:31:40Z gtunnel: conn iap-1 open from 127.0.0.1:{}",
        source_port(1)
    ));
    lines.push(format!(
        "2026-10-27T09:31:52Z gtunnel: conn iap-1 from 127.0.0.1:{} closed after 12.4s",
        source_port(1)
    ));
    let held = entry
        .metrics
        .as_ref()
        .map_or(0, |m| m.active_connections)
        .min(2) as u32;
    for id in 2..2 + held {
        lines.push(format!(
            "2026-10-27T09:33:07Z gtunnel: conn iap-{id} open from 127.0.0.1:{}",
            source_port(id)
        ));
    }
    (
        lines,
        Flow {
            id: 2 + held,
            open: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::Op;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        app.sync_demo_logs();
    }

    fn select(app: &mut App, name: &str) {
        app.selected = app
            .entries
            .iter()
            .position(|e| e.name() == name)
            .expect("a row of that name");
        app.sync_demo_logs();
    }

    fn ticks(app: &mut App, n: u32) {
        for _ in 0..n {
            app.tick_demo_logs();
        }
    }

    #[test]
    fn moving_the_selection_fills_the_pane_with_that_rows_log() {
        let mut app = App::new_demo();
        let first = app.logs.clone();
        assert!(!first.is_empty());
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected_entry().expect("row").name(), "redis");
        assert!(
            !app.logs.is_empty(),
            "a jump tunnel still has its install lines"
        );
        assert_ne!(app.logs, first);
        press(&mut app, KeyCode::Down);
        assert!(
            app.logs.iter().any(|l| l.contains("10.0.0.5:5432")),
            "{:?}",
            app.logs
        );
    }

    #[test]
    fn every_managed_row_in_both_accounts_has_a_log() {
        let mut app = App::new_demo();
        for account in ["demo", "staging"] {
            app.account = account.to_string();
            app.load_demo_account();
            for i in 0..app.entries.len() {
                app.selected = i;
                app.sync_demo_logs();
                let entry = app.selected_entry().expect("row");
                if entry.kind == TunnelKind::External {
                    assert!(app.logs.is_empty(), "an unmanaged row has no file");
                } else {
                    assert!(!app.logs.is_empty(), "{}", entry.name());
                }
            }
        }
    }

    #[test]
    fn a_jump_tunnel_stays_silent_and_an_unmanaged_row_has_no_log() {
        let mut app = App::new_demo();
        select(&mut app, "redis");
        let before = app.logs.clone();
        ticks(&mut app, EVENT_EVERY * 4);
        assert_eq!(app.logs, before);
        assert!(!app.logs.iter().any(|l| l.contains("conn ")));
        select(&mut app, "cache-1:6379");
        assert!(app.logs.is_empty());
    }

    #[test]
    fn every_line_has_the_shape_the_binary_writes() {
        let mut app = App::new_demo();
        for name in ["db", "pg-hosts", "api", "gt-7f3a91", "gt-3b2c10"] {
            select(&mut app, name);
            ticks(&mut app, EVENT_EVERY * 2);
            for line in &app.logs {
                let (stamp, rest) = line.split_once(' ').expect("a stamp then text");
                humantime::parse_rfc3339(stamp).unwrap_or_else(|_| panic!("{line}"));
                assert!(!rest.is_empty());
            }
        }
    }

    #[test]
    fn ticks_open_a_connection_then_close_it() {
        let mut app = App::new_demo();
        let before = app.logs.len();
        ticks(&mut app, EVENT_EVERY - 1);
        assert_eq!(app.logs.len(), before, "nothing arrives before the beat");
        ticks(&mut app, 1);
        assert_eq!(app.logs.len(), before + 1);
        assert!(app
            .logs
            .last()
            .expect("line")
            .contains(" open from 127.0.0.1:"));
        ticks(&mut app, EVENT_EVERY);
        let close = app.logs.last().expect("line");
        assert!(close.contains(" closed after "), "{close}");
        let id = app.logs[before].split("conn ").nth(1).expect("id");
        let id = id.split(' ').next().expect("id");
        assert!(close.contains(&format!("conn {id} from ")), "{close}");
    }

    #[test]
    fn the_stamps_only_move_forward() {
        let mut app = App::new_demo();
        ticks(&mut app, EVENT_EVERY * 12);
        let stamps: Vec<_> = app
            .logs
            .iter()
            .map(|l| humantime::parse_rfc3339(l.split(' ').next().expect("stamp")).expect("parse"))
            .collect();
        assert!(stamps.windows(2).all(|w| w[0] <= w[1]), "{:?}", app.logs);
    }

    #[test]
    fn activity_never_grows_the_pane_past_the_cap() {
        let mut app = App::new_demo();
        ticks(
            &mut app,
            EVENT_EVERY * (super::super::ui::LOG_LINES as u32 + 50),
        );
        assert_eq!(app.logs.len(), super::super::ui::LOG_LINES);
    }

    #[test]
    fn a_row_keeps_its_new_lines_when_the_reader_comes_back() {
        let mut app = App::new_demo();
        ticks(&mut app, EVENT_EVERY * 3);
        let grown = app.logs.clone();
        // By the keys, which clear the pane before the demo loop refills it
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_ne!(app.logs, grown);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.logs, grown);
    }

    #[test]
    fn the_same_ticks_always_write_the_same_lines() {
        let run = || {
            let mut app = App::new_demo();
            ticks(&mut app, EVENT_EVERY * 6);
            app.logs
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn cycling_the_account_loads_the_new_rows_log() {
        let mut app = App::new_demo();
        app.next_account();
        app.load_demo_account();
        assert_eq!(app.account, "staging");
        assert!(
            app.logs.iter().any(|l| l.contains("cache")),
            "{:?}",
            app.logs
        );
    }

    #[test]
    fn a_stopped_row_gains_no_activity_and_reads_stopped() {
        let mut app = App::new_demo();
        app.land_demo_op(&Op::Stop("db".to_string()));
        let before = app.logs.len();
        ticks(&mut app, EVENT_EVERY * 3);
        assert_eq!(app.logs.len(), before);
        assert_eq!(app.entries[0].status, TunnelStatus::Stopped);
    }

    #[test]
    fn stopping_in_the_demo_shows_the_stopped_circle() {
        let mut app = App::new_demo();
        let mut seen = None;
        if let Some(op) = app.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE)) {
            app.land_demo_op(&op);
            seen = Some(op);
        }
        assert_eq!(seen, Some(Op::Stop("db".to_string())));
        assert_eq!(app.entries[0].status, TunnelStatus::Stopped);
        assert_eq!(app.entries[0].health, HealthStatus::Unknown);
    }
}
