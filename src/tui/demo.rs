use std::time::Duration;

use super::app::{App, TunnelEntry, TunnelKind, SPARKLINE_SAMPLES};
use crate::gcp::monitoring::LbStats;
use crate::probe::{HealthStatus, MetricsHistory, TunnelMetrics};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};

const LOG: &[&str] = &[
    "Testing if tunnel connection works.",
    "Listening on port [15432].",
    "WARNING: `gcloud compute start-iap-tunnel` is in beta for this instance.",
    "New connection from [127.0.0.1:52118]",
    "New connection from [127.0.0.1:52119]",
];

fn access(name: &str, instance: &str, remote: u16, local: u16, auto: bool) -> PersistentTunnel {
    PersistentTunnel {
        name: name.to_string(),
        account_name: "demo".to_string(),
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

fn history(values: &[u64]) -> MetricsHistory {
    let mut h = MetricsHistory::new(SPARKLINE_SAMPLES);
    for v in values {
        h.record(*v);
    }
    h
}

fn metrics(latency: u64, connections: u64, uptime_secs: u64, lb: Option<LbStats>) -> TunnelMetrics {
    TunnelMetrics {
        latency_ms: Some(latency),
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

impl App {
    // A fully populated screen that needs no config, no gcloud and no network
    pub fn new_demo() -> Self {
        let mut app = App::new("demo");
        app.accounts = vec!["demo".to_string(), "staging".to_string()];
        app.has_relay = true;
        app.domains = vec!["demo.example.com".to_string()];
        app.logs = LOG.iter().map(|l| l.to_string()).collect();
        app.set_status("demo data: nothing here touches GCP");

        let mut db = TunnelEntry::new(
            access("db", "postgres-1", 5432, 15432, true),
            TunnelStatus::Running,
            TunnelKind::Managed,
        );
        db.metrics = Some(metrics(14, 3, 9_120, None));
        db.history = history(&[11, 13, 12, 18, 14, 15, 21, 14, 12, 13, 14, 16, 14]);
        db.health = HealthStatus::Healthy;

        // Someone else's gcloud process, which gtunnel reports but never touches
        let mut redis = TunnelEntry::new(
            access("pid-48210", "cache-1", 6379, 15379, false),
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

        app.entries = vec![db, redis, api, preview];
        app
    }
}
