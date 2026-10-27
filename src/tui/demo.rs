use std::time::Duration;

use super::app::{
    App, ConfigurationChoice, InstanceChoice, ProjectChoice, TunnelEntry, TunnelKind,
    SPARKLINE_SAMPLES,
};
use crate::gcp::monitoring::LbStats;
use crate::probe::{HealthStatus, MetricsHistory, TunnelMetrics};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};

// The shape `logfilter::format_line` writes: an RFC 3339 stamp, then either
// gcloud's own line or the one normalised `gtunnel: conn <id> ...` line that
// pairs a connection's open with its close and carries the client's source port
const DEMO_LOG: &[&str] = &[
    "2026-10-27T09:14:02Z Testing if tunnel connection works.",
    "2026-10-27T09:14:03Z Listening on port [15432].",
    "2026-10-27T09:14:03Z WARNING: `gcloud compute start-iap-tunnel` is in beta for this instance.",
    "2026-10-27T09:31:40Z gtunnel: conn iap-3 open from 127.0.0.1:52118",
    "2026-10-27T09:31:52Z gtunnel: conn iap-3 from 127.0.0.1:52118 closed after 12.4s",
    "2026-10-27T09:33:07Z gtunnel: conn iap-4 open from 127.0.0.1:52119",
    "2026-10-27T09:33:07Z gtunnel: conn iap-5 open from 127.0.0.1:52120",
];

const STAGING_LOG: &[&str] = &[
    "2026-10-27T08:02:11Z Testing if tunnel connection works.",
    "2026-10-27T08:02:12Z Listening on port [15379].",
    "2026-10-27T08:20:45Z gtunnel: conn iap-1 open from 127.0.0.1:61204",
];

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
        access("demo", "gt-3b2c10", "my-vm", 5432, 15900, false),
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
                ["bastion", "cache-vm", "db-vm"]
                    .map(|name| InstanceChoice {
                        name: name.to_string(),
                        zone: zone.clone(),
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
        self.logs = if staging { STAGING_LOG } else { DEMO_LOG }
            .iter()
            .map(|l| l.to_string())
            .collect();
        self.has_relay = !staging;
        self.domains = if staging {
            Vec::new()
        } else {
            vec!["demo.example.com".to_string()]
        };
    }
}
