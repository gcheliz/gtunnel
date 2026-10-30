// The whole module is CLI output, so printing is its job
#![allow(clippy::print_stdout, clippy::print_stderr)]

//! Bringing back the tunnels an expired login stopped, once the login is new.
//!
//! A supervisor stops on an expired login because restarting gcloud cannot
//! renew it, and leaves a marker saying why. Nothing used to notice when the
//! login was renewed, so the stop outlived its cause and every tunnel waited
//! for its own `gtunnel start`. The decision here is one question per account,
//! not per tunnel: can a token be minted now. Only `login-expired` is resumed.
//! An impersonation that was denied, a bad key and a missing configuration are
//! stopped for something a person has to fix, and a token fetch that works
//! says nothing about them.
//!
//! The probe and the start are injected through [`Resumer`], so the logic is
//! testable without a gcloud, a launchd or a network.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

use crate::daemon;
use crate::failure::Failure;
use crate::lifecycle::{Cause, Edge, State};
use crate::state::TunnelState;

/// A tunnel whose supervisor stopped on an auth marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parked {
    pub account: String,
    pub name: String,
    pub label: String,
}

/// What the markers say, split by whether a new login would help.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parking {
    /// Stopped on `login-expired`: a new login brings these back
    pub resumable: Vec<Parked>,
    /// Stopped on a failure a login does not change, with the one that did it
    pub left_alone: Vec<(Parked, Failure)>,
}

/// Every tunnel of `account` (or of all accounts) that has an auth marker.
pub fn parking(state: &TunnelState, dir: &Path, account: Option<&str>) -> Parking {
    let mut out = Parking::default();
    for tunnel in &state.tunnels {
        if account.is_some_and(|a| a != tunnel.account_name) {
            continue;
        }
        let label = daemon::platform_label(&tunnel.account_name, &tunnel.name);
        let Some(failure) = daemon::read_auth_marker(&daemon::auth_marker_in(dir, &label)) else {
            continue;
        };
        let parked = Parked {
            account: tunnel.account_name.clone(),
            name: tunnel.name.clone(),
            label,
        };
        if failure == Failure::LoginExpired {
            out.resumable.push(parked);
        } else {
            out.left_alone.push((parked, failure));
        }
    }
    out
}

/// What the resume needs from the world.
pub trait Resumer {
    /// Whether a token can be minted for the account now. Bounded by the
    /// implementation: a hung gcloud is a refusal, not a wait.
    async fn token_ok(&self, account: &str) -> bool;
    async fn start(&self, tunnel: &Parked) -> Result<()>;
    /// A pause before each start, so tunnels a login brings back do not start
    /// their gcloud processes in one second. Nothing for a person waiting.
    async fn settle(&self);
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// `account.name` of every tunnel that came back
    pub started: Vec<String>,
    /// `account.name` and the reason, for a start that was refused
    pub failed: Vec<(String, String)>,
    /// Accounts whose login is still not usable, with how many tunnels wait
    pub still_expired: Vec<(String, usize)>,
}

/// One token fetch per account, however many tunnels wait on it.
///
/// A fetch that fails changes nothing: the markers stay, so the next pass asks
/// again. One that works clears each marker and starts its tunnel; a start that
/// fails is reported and does not stop the others, since the cause is usually
/// that one tunnel's own.
pub async fn resume(dir: &Path, parked: Vec<Parked>, with: &impl Resumer) -> Outcome {
    let mut by_account: BTreeMap<String, Vec<Parked>> = BTreeMap::new();
    for tunnel in parked {
        by_account
            .entry(tunnel.account.clone())
            .or_default()
            .push(tunnel);
    }
    let mut outcome = Outcome::default();
    for (account, tunnels) in by_account {
        if !with.token_ok(&account).await {
            outcome.still_expired.push((account, tunnels.len()));
            continue;
        }
        for tunnel in tunnels {
            let id = format!("{}.{}", tunnel.account, tunnel.name);
            daemon::remove_auth_marker_in(dir, &tunnel.label);
            with.settle().await;
            match with.start(&tunnel).await {
                Ok(()) => outcome.started.push(id),
                Err(err) => outcome.failed.push((id, format!("{err:#}"))),
            }
        }
    }
    outcome
}

/// The watchdog's share: every account's login-expired tunnels, once.
///
/// A tunnel stopped on anything else stays stopped, whatever the token says.
pub async fn pass(state: &TunnelState, dir: &Path, with: &impl Resumer) -> Outcome {
    resume(dir, parking(state, dir, None).resumable, with).await
}

/// The real world: gcloud for the token, the service manager for the start.
pub struct Live {
    pub config: crate::config::Config,
    /// Spread the starts. The watchdog's pass sets it; a command or a tool
    /// call has a person waiting on it and does not.
    pub paced: bool,
}

impl Resumer for Live {
    async fn token_ok(&self, account: &str) -> bool {
        match self.config.get_account(Some(account)) {
            Ok(acct) => crate::gcp::gcloud::print_access_token(acct).await.is_ok(),
            Err(_) => false,
        }
    }

    async fn settle(&self) {
        if self.paced {
            tokio::time::sleep(crate::daemon::backoff::spread(
                crate::daemon::backoff::RESTART_SPREAD,
            ))
            .await;
        }
    }

    async fn start(&self, tunnel: &Parked) -> Result<()> {
        crate::commands::access::start_tunnel(Some(&tunnel.account), &tunnel.name)
            .await
            .map(|_| ())
    }
}

/// Edges for tunnels that stopped on an expired login, taken out of the list
/// and counted per account.
///
/// Four tunnels dying on one login are one fact, and the notification says it
/// once: "dev: login expired, 4 tunnels stopped" instead of four "stopped
/// unexpectedly" lines that each look like a crash.
pub fn fold_login_expired(
    edges: Vec<Edge>,
    parked: &[Parked],
) -> (Vec<Edge>, BTreeMap<String, usize>) {
    let mut counts = BTreeMap::new();
    let kept = edges
        .into_iter()
        .filter(|edge| {
            let stopped = matches!(edge.to, State::Stopped | State::Failed);
            let is_parked = parked
                .iter()
                .any(|p| p.account == edge.account && p.name == edge.tunnel);
            if edge.cause == Cause::Observed && stopped && is_parked {
                *counts.entry(edge.account.clone()).or_insert(0) += 1;
                false
            } else {
                true
            }
        })
        .collect();
    (kept, counts)
}

/// The one line said for an account.
pub fn login_expired_line(account: &str, count: usize) -> String {
    let noun = if count == 1 { "tunnel" } else { "tunnels" };
    format!("{account}: login expired, {count} {noun} stopped")
}

/// One notification per account, subject to the same switch as any failure.
pub fn announce_login_expired(
    notifier: &dyn crate::notify::Notifier,
    counts: &BTreeMap<String, usize>,
    cfg: &crate::config::NotifyConfig,
) {
    if !cfg.failed {
        return;
    }
    for (account, count) in counts {
        notifier.send("gtunnel", &login_expired_line(account, *count));
    }
}

/// What a person reads after `start --auth-expired`, one line each.
pub fn report(outcome: &Outcome, left_alone: &[(Parked, Failure)]) -> Vec<String> {
    let mut lines = Vec::new();
    for id in &outcome.started {
        lines.push(format!("started {id}"));
    }
    for (id, why) in &outcome.failed {
        lines.push(format!("could not start {id}: {why}"));
    }
    for (account, count) in &outcome.still_expired {
        lines.push(format!(
            "{account}: login still not usable, {count} waiting. Run `gcloud auth login`"
        ));
    }
    for (tunnel, failure) in left_alone {
        lines.push(format!(
            "left {}.{} stopped: {} needs fixing first, a new login does not help",
            tunnel.account,
            tunnel.name,
            failure.slug()
        ));
    }
    lines
}

/// `gtunnel start --auth-expired`. Fails when anything it tried did not come
/// back, so a script can tell.
pub async fn cmd_start_auth_expired(account: Option<&str>) -> Result<()> {
    let dir = crate::config::config_dir()?;
    let config = crate::config::load_config()?;
    if let Some(name) = account {
        config.get_account(Some(name))?;
    }
    let found = parking(&TunnelState::load()?, &dir, account);
    if found.resumable.is_empty() && found.left_alone.is_empty() {
        println!("No tunnel is stopped on an expired login");
        return Ok(());
    }
    let outcome = resume(
        &dir,
        found.resumable,
        &Live {
            config,
            paced: false,
        },
    )
    .await;
    for line in report(&outcome, &found.left_alone) {
        println!("{line}");
    }
    if outcome.failed.is_empty() && outcome.still_expired.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("not every tunnel stopped on an expired login came back")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec};
    use std::cell::RefCell;

    fn tunnel(account: &str, name: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.into(),
            account_name: account.into(),
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "bastion".into(),
                    zone: "europe-west1-b".into(),
                },
                remote_port: 443,
                local_port: 8443,
            },
            auto_start: false,
        }
    }

    fn mark(dir: &Path, account: &str, name: &str, slug: &str) {
        let label = daemon::platform_label(account, name);
        let path = daemon::auth_marker_in(dir, &label);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{slug}\n")).unwrap();
    }

    fn marked(dir: &Path, account: &str, name: &str) -> bool {
        daemon::auth_marker_in(dir, &daemon::platform_label(account, name)).exists()
    }

    fn parked(account: &str, name: &str) -> Parked {
        Parked {
            account: account.into(),
            name: name.into(),
            label: daemon::platform_label(account, name),
        }
    }

    /// Records every question it is asked
    struct Fake {
        login_works: bool,
        tokens: RefCell<Vec<String>>,
        starts: RefCell<Vec<String>>,
        settles: RefCell<u32>,
        refuse: Option<&'static str>,
    }

    impl Fake {
        fn new(login_works: bool) -> Self {
            Fake {
                login_works,
                tokens: RefCell::default(),
                starts: RefCell::default(),
                settles: RefCell::default(),
                refuse: None,
            }
        }
    }

    impl Resumer for Fake {
        async fn token_ok(&self, account: &str) -> bool {
            self.tokens.borrow_mut().push(account.to_string());
            self.login_works
        }
        async fn settle(&self) {
            *self.settles.borrow_mut() += 1;
        }
        async fn start(&self, tunnel: &Parked) -> Result<()> {
            if self.refuse == Some(tunnel.name.as_str()) {
                anyhow::bail!("port in use");
            }
            self.starts.borrow_mut().push(tunnel.name.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn each_tunnel_a_login_brings_back_is_started_after_its_own_pause() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["db", "cache", "k8s"] {
            mark(dir.path(), "dev", name, "login-expired");
        }
        let all = vec![
            parked("dev", "db"),
            parked("dev", "cache"),
            parked("dev", "k8s"),
        ];
        let fake = Fake::new(true);

        resume(dir.path(), all.clone(), &fake).await;
        assert_eq!(*fake.settles.borrow(), 3, "one pause per start");

        let refused = Fake::new(false);
        resume(dir.path(), all, &refused).await;
        assert_eq!(
            *refused.settles.borrow(),
            0,
            "a login that fails starts nothing"
        );
    }

    #[tokio::test]
    async fn a_new_login_clears_the_marker_and_starts_the_tunnel() {
        let dir = tempfile::tempdir().unwrap();
        mark(dir.path(), "dev", "db", "login-expired");
        let fake = Fake::new(true);

        let outcome = resume(dir.path(), vec![parked("dev", "db")], &fake).await;

        assert_eq!(outcome.started, vec!["dev.db".to_string()]);
        assert!(!marked(dir.path(), "dev", "db"), "the marker is cleared");
        assert_eq!(*fake.starts.borrow(), vec!["db".to_string()]);
    }

    #[tokio::test]
    async fn a_login_that_still_fails_changes_nothing_and_asks_once_per_account() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["db", "cache", "k8s"] {
            mark(dir.path(), "dev", name, "login-expired");
        }
        mark(dir.path(), "prod", "db", "login-expired");
        let all = vec![
            parked("dev", "db"),
            parked("dev", "cache"),
            parked("dev", "k8s"),
            parked("prod", "db"),
        ];
        let fake = Fake::new(false);

        let outcome = resume(dir.path(), all, &fake).await;

        assert!(outcome.started.is_empty());
        assert!(fake.starts.borrow().is_empty());
        assert!(marked(dir.path(), "dev", "cache"), "markers stay");
        assert_eq!(
            *fake.tokens.borrow(),
            vec!["dev".to_string(), "prod".to_string()],
            "one token fetch per account, not per tunnel"
        );
        assert_eq!(
            outcome.still_expired,
            vec![("dev".to_string(), 3), ("prod".to_string(), 1)]
        );
    }

    #[tokio::test]
    async fn one_refused_start_does_not_cost_the_others_their_resume() {
        let dir = tempfile::tempdir().unwrap();
        let all = vec![parked("dev", "db"), parked("dev", "cache")];
        let mut fake = Fake::new(true);
        fake.refuse = Some("db");

        let outcome = resume(dir.path(), all, &fake).await;

        assert_eq!(outcome.started, vec!["dev.cache".to_string()]);
        assert_eq!(outcome.failed.len(), 1);
        assert!(outcome.failed[0].1.contains("port in use"));
    }

    #[test]
    fn only_a_login_expired_marker_is_resumable() {
        let dir = tempfile::tempdir().unwrap();
        let state = TunnelState {
            tunnels: vec![
                tunnel("dev", "db"),
                tunnel("dev", "cache"),
                tunnel("dev", "k8s"),
                tunnel("dev", "up"),
                tunnel("prod", "db"),
            ],
            ..Default::default()
        };
        mark(dir.path(), "dev", "db", "login-expired");
        mark(dir.path(), "dev", "cache", "impersonation-denied");
        mark(dir.path(), "dev", "k8s", "1");
        mark(dir.path(), "prod", "db", "login-expired");

        let all = parking(&state, dir.path(), None);
        let names: Vec<_> = all.resumable.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["db", "k8s", "db"], "the legacy `1` is a login");
        assert_eq!(all.left_alone.len(), 1);
        assert_eq!(all.left_alone[0].0.name, "cache");
        assert_eq!(all.left_alone[0].1, Failure::ImpersonationDenied);

        let dev = parking(&state, dir.path(), Some("dev"));
        assert!(dev.resumable.iter().all(|p| p.account == "dev"));
    }

    #[tokio::test]
    async fn a_start_of_only_marked_tunnels_leaves_a_running_one_alone() {
        let dir = tempfile::tempdir().unwrap();
        let state = TunnelState {
            tunnels: vec![tunnel("dev", "db"), tunnel("dev", "up")],
            ..Default::default()
        };
        mark(dir.path(), "dev", "db", "login-expired");
        let fake = Fake::new(true);

        let found = parking(&state, dir.path(), None);
        resume(dir.path(), found.resumable, &fake).await;

        assert_eq!(*fake.starts.borrow(), vec!["db".to_string()]);
    }

    #[tokio::test]
    async fn a_watch_pass_with_a_working_login_resumes_only_the_login_stops() {
        let dir = tempfile::tempdir().unwrap();
        let state = TunnelState {
            tunnels: vec![
                tunnel("dev", "db"),
                tunnel("dev", "cache"),
                tunnel("dev", "up"),
            ],
            ..Default::default()
        };
        mark(dir.path(), "dev", "db", "login-expired");
        mark(dir.path(), "dev", "cache", "key-invalid");
        let fake = Fake::new(true);

        let outcome = pass(&state, dir.path(), &fake).await;

        assert_eq!(outcome.started, vec!["dev.db".to_string()]);
        assert!(
            marked(dir.path(), "dev", "cache"),
            "a bad key is not a login"
        );
        assert_eq!(*fake.tokens.borrow(), vec!["dev".to_string()]);
    }

    #[tokio::test]
    async fn a_watch_pass_with_nothing_parked_never_fetches_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let state = TunnelState {
            tunnels: vec![tunnel("dev", "db")],
            ..Default::default()
        };
        let fake = Fake::new(true);
        pass(&state, dir.path(), &fake).await;
        assert!(fake.tokens.borrow().is_empty());
    }

    #[test]
    fn four_stopped_tunnels_make_one_line_for_the_account() {
        let stopped = |name: &str| Edge {
            tunnel: name.into(),
            account: "dev".into(),
            from: State::Up,
            to: State::Stopped,
            cause: Cause::Observed,
        };
        let crashed = Edge {
            tunnel: "other".into(),
            ..stopped("other")
        };
        let edges = vec![
            stopped("a"),
            stopped("b"),
            stopped("c"),
            stopped("d"),
            crashed,
        ];
        let marked: Vec<Parked> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| parked("dev", n))
            .collect();

        let (kept, counts) = fold_login_expired(edges, &marked);

        assert_eq!(
            kept.len(),
            1,
            "a crash with no marker is still its own edge"
        );
        assert_eq!(counts.get("dev"), Some(&4));
        assert_eq!(
            login_expired_line("dev", 4),
            "dev: login expired, 4 tunnels stopped"
        );
        assert_eq!(
            login_expired_line("dev", 1),
            "dev: login expired, 1 tunnel stopped"
        );
    }

    #[test]
    fn the_account_notice_says_it_once_and_honours_the_failure_switch() {
        struct Recorder(RefCell<Vec<String>>);
        impl crate::notify::Notifier for Recorder {
            fn send(&self, _title: &str, body: &str) {
                self.0.borrow_mut().push(body.to_string());
            }
        }
        let counts = BTreeMap::from([("dev".to_string(), 4), ("prod".to_string(), 1)]);
        let rec = Recorder(RefCell::default());
        announce_login_expired(&rec, &counts, &crate::config::NotifyConfig::default());
        assert_eq!(
            *rec.0.borrow(),
            vec![
                "dev: login expired, 4 tunnels stopped".to_string(),
                "prod: login expired, 1 tunnel stopped".to_string()
            ]
        );

        let quiet = Recorder(RefCell::default());
        let off = crate::config::NotifyConfig {
            failed: false,
            ..Default::default()
        };
        announce_login_expired(&quiet, &counts, &off);
        assert!(quiet.0.borrow().is_empty());
    }
}
