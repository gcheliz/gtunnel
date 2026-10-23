use std::time::Duration;

use super::app::{App, TunnelEntry, TunnelKind, SPARKLINE_SAMPLES};
use crate::gcp::monitoring::LbStats;
use crate::probe::{HealthStatus, MetricsHistory, TunnelMetrics};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};

const DEMO_LOG: &[&str] = &[
    "Testing if tunnel connection works.",
    "Listening on port [15432].",
    "WARNING: `gcloud compute start-iap-tunnel` is in beta for this instance.",
    "New connection from [127.0.0.1:52118]",
    "New connection from [127.0.0.1:52119]",
];

const STAGING_LOG: &[&str] = &[
    "Testing if tunnel connection works.",
    "Listening on port [15379].",
    "New connection from [127.0.0.1:61204]",
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

    // Someone else's gcloud process, which gtunnel reports but never touches
    let mut redis = TunnelEntry::new(
        access("demo", "pid-48210", "cache-1", 6379, 15379, false),
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

    vec![db, redis, api, preview]
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
        app.load_demo_account();
        app.set_status("demo data: nothing here touches GCP");
        app
    }

    // What the runtime would have loaded for the selected account, had there
    // been one. `;` calls this, so cycling accounts keeps the screen filled
    pub fn load_demo_account(&mut self) {
        let staging = self.account == "staging";
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
