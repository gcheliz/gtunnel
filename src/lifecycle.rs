//! One lifecycle per tunnel, and the one ledger every observer shares.
//!
//! Notification used to be a side effect of one viewer's health poll, so a
//! crash, a failed start, a closed TUI or the other account were all silent.
//! Here a notification is the answer to a different question: did the
//! tunnel's state change since anyone last looked? The ledger holds "since
//! anyone last looked", so the watchdog, the TUI and the CLI can all observe
//! and a change is still announced once, by whoever stores it first.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::NotifyConfig;
use crate::notify::Notifier;
use crate::state::TunnelStatus;

/// How old the watchdog's heartbeat may be before the TUI stops trusting it to
/// be watching. Three of its thirty-second passes: one missed pass is a slow
/// pass, three is a watchdog that is not running.
pub const HEARTBEAT_FRESH_SECS: u64 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Stopped,
    Starting,
    Up,
    Degraded,
    Failed,
}

/// Who made the edge. A person who ran `stop` does not need to be told that
/// the tunnel stopped, so a user edge is never announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    User,
    Observed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub tunnel: String,
    pub account: String,
    pub from: State,
    pub to: State,
    pub cause: Cause,
}

/// What a status and a probe say the state is, or `None` when they say
/// nothing new.
///
/// `None` is "keep the ledger's state": a running tunnel nobody probed this
/// pass, or one that failed a probe but not often enough to be called
/// degraded, has not changed. Guessing `Up` for either would announce a
/// recovery that never happened.
pub fn observe(
    status: TunnelStatus,
    healthy: Option<bool>,
    failures: u32,
    threshold: u32,
) -> Option<State> {
    match status {
        TunnelStatus::Stopped => Some(State::Stopped),
        TunnelStatus::Error => Some(State::Failed),
        TunnelStatus::Running => match healthy {
            Some(true) => Some(State::Up),
            Some(false) if failures >= threshold => Some(State::Degraded),
            _ => None,
        },
    }
}

enum Kind {
    Unhealthy,
    Recovered,
    Up,
}

/// What an edge is worth saying, and how it counts when several are said at
/// once.
fn classify(edge: &Edge, cfg: &NotifyConfig) -> Option<(Kind, String)> {
    use State::*;
    if edge.cause == Cause::User {
        return None;
    }
    let name = format!("{} ({})", edge.tunnel, edge.account);
    match (edge.from, edge.to) {
        (Starting, Up) => cfg.up.then(|| (Kind::Up, format!("{name} is up"))),
        (Starting, Failed) => cfg
            .failed
            .then(|| (Kind::Unhealthy, format!("{name} failed to start"))),
        (Starting, Degraded) => cfg.degraded.then(|| {
            (
                Kind::Unhealthy,
                format!("{name} started but is not answering"),
            )
        }),
        // The start is still in flight: a job that has not been loaded yet
        // reads as stopped, and that is not a crash
        (Starting, Stopped) => None,
        // Still unhealthy: said once already
        (Degraded, Degraded) | (Failed, Degraded) => None,
        (_, Degraded) => cfg
            .degraded
            .then(|| (Kind::Unhealthy, format!("{name} is unhealthy"))),
        (Degraded | Failed, Up) => cfg
            .recovered
            .then(|| (Kind::Recovered, format!("{name} is healthy again"))),
        (_, Failed) => cfg
            .failed
            .then(|| (Kind::Unhealthy, format!("{name} failed"))),
        (Up | Degraded, Stopped) => cfg
            .failed
            .then(|| (Kind::Unhealthy, format!("{name} stopped unexpectedly"))),
        _ => None,
    }
}

/// The words for one edge, or `None` when it is not worth interrupting for.
#[cfg(test)]
pub fn policy(edge: &Edge, cfg: &NotifyConfig) -> Option<String> {
    classify(edge, cfg).map(|(_, message)| message)
}

/// One pass's edges, one notification.
///
/// The pass is the unit: a laptop waking from sleep finds every tunnel
/// changed at once, and five alerts in a row say less than one that counts.
pub fn announce_with(notifier: &dyn Notifier, edges: &[Edge], cfg: &NotifyConfig) {
    let said: Vec<(Kind, String)> = edges.iter().filter_map(|e| classify(e, cfg)).collect();
    match said.as_slice() {
        [] => {}
        [(_, message)] => notifier.send("gtunnel", message),
        many => {
            let count = |want: fn(&Kind) -> bool| many.iter().filter(|(k, _)| want(k)).count();
            let parts: Vec<String> = [
                (count(|k| matches!(k, Kind::Unhealthy)), "unhealthy"),
                (count(|k| matches!(k, Kind::Recovered)), "recovered"),
                (count(|k| matches!(k, Kind::Up)), "up"),
            ]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
            notifier.send(
                "gtunnel",
                &format!("{} tunnels: {}", many.len(), parts.join(", ")),
            );
        }
    }
}

pub fn announce(edges: &[Edge]) {
    if edges.is_empty() {
        return;
    }
    let cfg = crate::config::load_config()
        .map(|c| c.notifications)
        .unwrap_or_default();
    announce_with(&crate::notify::Desktop, edges, &cfg);
}

#[derive(Debug, Serialize, Deserialize)]
struct Row {
    state: State,
    at: u64,
}

type Ledger = BTreeMap<String, Row>;

fn key(account: &str, name: &str) -> String {
    format!("{account}/{name}")
}

fn ledger_path(dir: &Path) -> PathBuf {
    dir.join("lifecycle.json")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// An unreadable ledger is an empty one: the cost is one silent baseline pass,
// where refusing to run would cost every notification until someone noticed
fn read(dir: &Path) -> Ledger {
    std::fs::read_to_string(ledger_path(dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn update(dir: &Path, change: impl FnOnce(&mut Ledger) -> Option<Edge>) -> Result<Option<Edge>> {
    let _lock =
        crate::fsutil::lock_exclusive(&dir.join("lifecycle.lock"), crate::fsutil::LOCK_DEADLINE)?;
    let mut ledger = read(dir);
    let edge = change(&mut ledger);
    let text = serde_json::to_string_pretty(&ledger).context("Failed to serialize lifecycle")?;
    crate::fsutil::write_atomic(&ledger_path(dir), &text)?;
    Ok(edge)
}

/// Stores `to` and returns the edge, only when the stored state differed.
///
/// This is the dedup: two observers that see the same change both call it, and
/// the second finds the state already stored. A tunnel never seen before is
/// stored without an edge, because the first pass after installing the
/// watchdog meets tunnels in every state and none of that is news.
pub fn transition_in(
    dir: &Path,
    account: &str,
    name: &str,
    to: State,
    cause: Cause,
) -> Result<Option<Edge>> {
    update(dir, |ledger| {
        let row = Row {
            state: to,
            at: now_secs(),
        };
        let from = ledger.insert(key(account, name), row)?.state;
        (from != to).then(|| Edge {
            tunnel: name.to_string(),
            account: account.to_string(),
            from,
            to,
            cause,
        })
    })
}

/// The state the ledger holds, for an observer that was told nothing new.
#[cfg(test)]
pub fn stored_in(dir: &Path, account: &str, name: &str) -> Option<State> {
    read(dir).get(&key(account, name)).map(|row| row.state)
}

pub fn forget_in(dir: &Path, account: &str, name: &str) -> Result<()> {
    update(dir, |ledger| {
        ledger.remove(&key(account, name));
        None
    })
    .map(|_| ())
}

fn config_dir() -> Option<PathBuf> {
    crate::config::config_dir().ok()
}

pub fn transition(account: &str, name: &str, to: State, cause: Cause) -> Option<Edge> {
    transition_in(&config_dir()?, account, name, to, cause)
        .ok()
        .flatten()
}

/// A person's own operation. Best effort and silent: the ledger being
/// unwritable must not fail a `stop`, and the edge it returns is never one
/// anyone announces.
pub fn record_user(account: &str, name: &str, to: State) {
    let _ = transition(account, name, to, Cause::User);
}

pub fn forget(account: &str, name: &str) {
    if let Some(dir) = config_dir() {
        let _ = forget_in(&dir, account, name);
    }
}

fn heartbeat_path(dir: &Path) -> PathBuf {
    dir.join("run").join("watchdog.heartbeat")
}

pub fn write_heartbeat_in(dir: &Path, now: u64) -> Result<()> {
    let path = heartbeat_path(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    crate::fsutil::write_atomic(&path, &format!("{now}\n"))
}

pub fn write_heartbeat() -> Result<()> {
    write_heartbeat_in(&config_dir().context("no config directory")?, now_secs())
}

/// Seconds since the last watchdog pass finished, if there ever was one.
pub fn heartbeat_age_in(dir: &Path, now: u64) -> Option<u64> {
    let text = std::fs::read_to_string(heartbeat_path(dir)).ok()?;
    let at: u64 = text.trim().parse().ok()?;
    // A clock that stepped puts the beat in the future; the distance is still
    // the honest measure of how far from "now" it is
    Some(now.abs_diff(at))
}

pub fn heartbeat_fresh_in(dir: &Path, now: u64) -> bool {
    heartbeat_age_in(dir, now).is_some_and(|age| age <= HEARTBEAT_FRESH_SECS)
}

#[cfg_attr(test, allow(dead_code))]
pub fn watchdog_is_fresh() -> bool {
    config_dir().is_some_and(|dir| heartbeat_fresh_in(&dir, now_secs()))
}

pub fn heartbeat_age() -> Option<u64> {
    heartbeat_age_in(&config_dir()?, now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Recorder(RefCell<Vec<String>>);

    impl Notifier for Recorder {
        fn send(&self, _title: &str, body: &str) {
            self.0.borrow_mut().push(body.to_string());
        }
    }

    fn edge(from: State, to: State, cause: Cause) -> Edge {
        Edge {
            tunnel: "kubernetes".into(),
            account: "dev".into(),
            from,
            to,
            cause,
        }
    }

    fn said(from: State, to: State) -> Option<String> {
        policy(&edge(from, to, Cause::Observed), &NotifyConfig::default())
    }

    #[test]
    fn a_crash_is_announced() {
        let text = said(State::Up, State::Stopped).expect("announced");
        assert!(text.contains("stopped unexpectedly"), "{text}");
    }

    #[test]
    fn a_failure_while_running_is_announced() {
        let text = said(State::Up, State::Failed).expect("announced");
        assert!(text.contains("failed"), "{text}");
    }

    #[test]
    fn going_unhealthy_is_announced() {
        let text = said(State::Up, State::Degraded).expect("announced");
        assert!(text.contains("unhealthy"), "{text}");
    }

    #[test]
    fn recovering_is_announced() {
        let text = said(State::Degraded, State::Up).expect("announced");
        assert!(text.contains("healthy again"), "{text}");
        assert!(said(State::Failed, State::Up).is_some());
    }

    #[test]
    fn a_failed_start_is_announced() {
        let text = said(State::Starting, State::Failed).expect("announced");
        assert!(text.contains("failed to start"), "{text}");
    }

    #[test]
    fn a_start_that_comes_up_unhealthy_is_announced() {
        assert!(said(State::Starting, State::Degraded).is_some());
    }

    #[test]
    fn a_start_that_works_is_quiet_by_default() {
        assert_eq!(said(State::Starting, State::Up), None);
    }

    #[test]
    fn a_start_that_works_is_announced_when_asked_for() {
        let cfg = NotifyConfig {
            up: true,
            ..NotifyConfig::default()
        };
        let text = policy(&edge(State::Starting, State::Up, Cause::Observed), &cfg);
        assert!(text.expect("announced").contains("is up"));
    }

    #[test]
    fn a_user_stop_is_never_announced() {
        let all_on = NotifyConfig {
            up: true,
            ..NotifyConfig::default()
        };
        for (from, to) in [
            (State::Up, State::Stopped),
            (State::Starting, State::Failed),
            (State::Up, State::Degraded),
            (State::Starting, State::Up),
        ] {
            assert_eq!(policy(&edge(from, to, Cause::User), &all_on), None);
        }
    }

    #[test]
    fn a_job_not_yet_loaded_is_not_a_crash() {
        assert_eq!(said(State::Starting, State::Stopped), None);
    }

    #[test]
    fn a_tunnel_started_outside_gtunnel_is_not_announced() {
        assert_eq!(said(State::Stopped, State::Up), None);
    }

    #[test]
    fn staying_unhealthy_is_not_announced_again() {
        assert_eq!(said(State::Degraded, State::Degraded), None);
        assert_eq!(said(State::Failed, State::Stopped), None);
    }

    #[test]
    fn each_class_can_be_switched_off() {
        let off = NotifyConfig {
            up: false,
            degraded: false,
            recovered: false,
            failed: false,
        };
        for (from, to) in [
            (State::Up, State::Degraded),
            (State::Degraded, State::Up),
            (State::Up, State::Failed),
            (State::Up, State::Stopped),
            (State::Starting, State::Failed),
        ] {
            assert_eq!(policy(&edge(from, to, Cause::Observed), &off), None);
        }
    }

    #[test]
    fn a_stopped_status_is_stopped_and_an_error_is_failed() {
        assert_eq!(
            observe(TunnelStatus::Stopped, Some(true), 0, 2),
            Some(State::Stopped)
        );
        assert_eq!(
            observe(TunnelStatus::Error, None, 0, 2),
            Some(State::Failed)
        );
    }

    #[test]
    fn a_running_tunnel_is_degraded_only_after_the_threshold() {
        let at = |failures| observe(TunnelStatus::Running, Some(false), failures, 2);
        assert_eq!(at(1), None);
        assert_eq!(at(2), Some(State::Degraded));
        assert_eq!(
            observe(TunnelStatus::Running, Some(true), 0, 2),
            Some(State::Up)
        );
    }

    #[test]
    fn a_running_tunnel_nobody_probed_keeps_its_state() {
        assert_eq!(observe(TunnelStatus::Running, None, 0, 2), None);
    }

    #[test]
    fn the_second_observer_of_the_same_change_gets_no_edge() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert_eq!(
            transition_in(d, "dev", "kubernetes", State::Up, Cause::Observed).unwrap(),
            None,
            "a first sighting is a baseline, not news"
        );
        let first = transition_in(d, "dev", "kubernetes", State::Failed, Cause::Observed)
            .unwrap()
            .expect("the first observer sees the change");
        assert_eq!((first.from, first.to), (State::Up, State::Failed));
        let second = transition_in(d, "dev", "kubernetes", State::Failed, Cause::Observed).unwrap();
        assert_eq!(second, None);
    }

    #[test]
    fn two_accounts_keep_their_own_state_for_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        transition_in(d, "dev", "kubernetes", State::Up, Cause::Observed).unwrap();
        transition_in(d, "prod", "kubernetes", State::Up, Cause::Observed).unwrap();
        let edge = transition_in(d, "prod", "kubernetes", State::Failed, Cause::Observed).unwrap();
        assert!(edge.is_some());
        assert_eq!(stored_in(d, "dev", "kubernetes"), Some(State::Up));
    }

    #[test]
    fn the_ledger_is_json_keyed_by_account_and_name() {
        let dir = tempfile::tempdir().unwrap();
        transition_in(dir.path(), "dev", "kubernetes", State::Up, Cause::User).unwrap();
        let text = std::fs::read_to_string(dir.path().join("lifecycle.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["dev/kubernetes"]["state"], "up");
        assert!(json["dev/kubernetes"]["at"].is_u64(), "{text}");
    }

    #[test]
    fn a_forgotten_tunnel_leaves_no_row_so_a_new_one_of_that_name_starts_clean() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        transition_in(d, "dev", "kubernetes", State::Failed, Cause::Observed).unwrap();
        forget_in(d, "dev", "kubernetes").unwrap();
        assert_eq!(stored_in(d, "dev", "kubernetes"), None);
    }

    #[test]
    fn an_unreadable_ledger_is_treated_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lifecycle.json"), "{ not json").unwrap();
        let edge = transition_in(dir.path(), "dev", "k", State::Up, Cause::Observed).unwrap();
        assert_eq!(edge, None);
        assert_eq!(stored_in(dir.path(), "dev", "k"), Some(State::Up));
    }

    #[test]
    fn several_edges_in_one_pass_are_one_notification() {
        let rec = Recorder::default();
        let edges = [
            edge(State::Up, State::Failed, Cause::Observed),
            edge(State::Up, State::Degraded, Cause::Observed),
            edge(State::Degraded, State::Up, Cause::Observed),
            edge(State::Up, State::Stopped, Cause::User),
        ];
        announce_with(&rec, &edges, &NotifyConfig::default());
        assert_eq!(
            rec.0.borrow().as_slice(),
            ["3 tunnels: 2 unhealthy, 1 recovered"]
        );
    }

    #[test]
    fn one_edge_is_its_own_sentence_and_none_is_silence() {
        let rec = Recorder::default();
        announce_with(&rec, &[], &NotifyConfig::default());
        announce_with(
            &rec,
            &[edge(State::Up, State::Stopped, Cause::User)],
            &NotifyConfig::default(),
        );
        assert!(rec.0.borrow().is_empty());
        announce_with(
            &rec,
            &[edge(State::Up, State::Failed, Cause::Observed)],
            &NotifyConfig::default(),
        );
        assert_eq!(rec.0.borrow().as_slice(), ["kubernetes (dev) failed"]);
    }

    #[test]
    fn a_heartbeat_is_fresh_for_ninety_seconds_and_not_after() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert!(!heartbeat_fresh_in(d, 1_000), "no heartbeat is not fresh");
        write_heartbeat_in(d, 1_000).unwrap();
        assert!(heartbeat_fresh_in(d, 1_090));
        assert!(!heartbeat_fresh_in(d, 1_091));
        assert_eq!(heartbeat_age_in(d, 1_030), Some(30));
    }
}
