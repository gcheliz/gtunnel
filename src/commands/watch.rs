//! One pass over every tunnel: look, decide, act, remember.
//!
//! The deciding lives in [`crate::breaker`] and stays pure. This file is the
//! half that touches the world - the probe, the log, the service manager -
//! and the few pure functions that turn a decision into the words a person
//! reads afterwards.

use anyhow::Result;
use std::time::{Duration, SystemTime};

use crate::breaker::persist::{self, Row};
use crate::breaker::{Action, Observation, Reason};
use crate::daemon;
use crate::probe::{self, Holder};
use crate::state::{PersistentTunnel, TunnelState, TunnelStatus};

/// What a listener out of file descriptors writes before it starts refusing
/// everything. The same words `diagnose_log_line` keys on.
const DESCRIPTOR_ERROR: &str = "Too many open files";

/// How long a healthy tunnel has to answer its own local port. Generous: a
/// slow answer is a working tunnel, and a false trip costs a restart.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// What one check of one tunnel saw.
///
/// `Running` and refusing is the state launchd cannot see and the only one
/// worth acting on. Anything else is a tunnel nobody is currently running,
/// which the breaker treats as a reset.
fn observe(status: TunnelStatus, probe_ok: bool, gained_error: bool) -> Observation {
    if status != TunnelStatus::Running {
        return Observation::NotRunning;
    }
    if probe_ok {
        return Observation::Pass;
    }
    // The log does not decide whether this failed, only what to call it: a
    // listener out of descriptors and one that died are both refusing
    // connections, and a person needs to know which
    Observation::Failed(if gained_error {
        Reason::DescriptorsExhausted
    } else {
        Reason::ProbeRefused
    })
}

/// Where in the log this pass starts reading: the offset the last pass
/// stopped at, as a position in the file.
///
/// A file shorter than `seen` was emptied by a restart, so all of it is new.
///
/// This has to be a file offset read with [`daemon::read_from`], not an index
/// into a tail window: the window is at most [`daemon::TAIL_WINDOW`] bytes and
/// is lossily decoded, so on any longer log the two numbers disagree, the
/// whole window is rescanned every pass, and a descriptor error from hours ago
/// is read as news - which blames every later failure of that tunnel on
/// descriptors.
fn unread_from(len: u64, seen: u64) -> u64 {
    if len < seen {
        0
    } else {
        seen
    }
}

fn reason_text(reason: Reason) -> &'static str {
    match reason {
        Reason::ProbeRefused => "it is running but refusing connections",
        Reason::DescriptorsExhausted => "it ran out of file descriptors",
    }
}

/// The one line a pass writes into a tunnel's own log, or nothing at all.
///
/// Nothing is the common case and has to stay silent: a line every thirty
/// seconds would bury the lines that matter.
/// What to write after trying to carry an action out. `failed` is the error
/// the service manager gave, when it refused.
///
/// The line describes what happened rather than what was decided: a `stop`
/// that failed leaves the tunnel running and still refusing connections, and
/// a log saying it was stopped sends the person hunting for a tunnel that is
/// right there in `gtunnel list`.
fn action_line(
    action: Action,
    reason: Option<Reason>,
    holders: &[Holder],
    failed: Option<&str>,
) -> Option<String> {
    let Some(err) = failed else {
        return log_line(action, reason, holders);
    };
    match action {
        Action::Nothing => None,
        Action::Restart => Some(format!("could not restart it: {err}")),
        Action::StopAndReport(reason) => Some(format!(
            "could not stop it after {} restarts that did not hold: {err}. It is still \
             running and still failing: {}",
            crate::breaker::MAX_ATTEMPTS,
            reason_text(reason),
        )),
    }
}

fn log_line(action: Action, reason: Option<Reason>, holders: &[Holder]) -> Option<String> {
    match action {
        Action::Nothing => None,
        Action::Restart => Some(format!(
            "restarting it: {}",
            reason.map_or("it failed its last two checks", reason_text)
        )),
        Action::StopAndReport(reason) => Some(format!(
            "stopped after {} restarts that did not hold: {}. {} Start it again with `gtunnel start` once that is dealt with.",
            crate::breaker::MAX_ATTEMPTS,
            reason_text(reason),
            holder_text(holders),
        )),
    }
}

/// Who to go and talk to, or an honest admission that it is not known.
fn holder_text(holders: &[Holder]) -> String {
    match holders.first() {
        Some(worst) => format!(
            "{} (pid {}) holds {} of its connections.",
            worst.command, worst.pid, worst.count
        ),
        None => "The process holding its connections could not be identified.".to_string(),
    }
}

/// The tunnels a pass covers, and a note for every one it leaves alone.
///
/// A public tunnel's health lives in a GCP load balancer; a TCP probe to a
/// local port would be answering a different question, so `watch` says it is
/// skipping them rather than quietly reporting them healthy.
fn targets(state: &TunnelState) -> (Vec<&PersistentTunnel>, Vec<String>) {
    let mut watched = Vec::new();
    let mut skipped = Vec::new();
    for tunnel in &state.tunnels {
        if tunnel.is_expose() {
            skipped.push(format!(
                "{}.{}: public tunnels are not watched, their health is the load balancer's",
                tunnel.account_name, tunnel.name
            ));
        } else {
            watched.push(tunnel);
        }
    }
    (watched, skipped)
}

/// The local address a tunnel answers on, for Access and jump tunnels.
fn probe_addr(tunnel: &PersistentTunnel) -> Option<String> {
    match &tunnel.spec {
        crate::state::TunnelSpec::Access { local_port, .. } => {
            Some(format!("127.0.0.1:{local_port}"))
        }
        crate::state::TunnelSpec::Expose { .. } => None,
    }
}

fn key(tunnel: &PersistentTunnel) -> String {
    format!("{}.{}", tunnel.account_name, tunnel.name)
}

/// One pass over every watched tunnel.
///
/// Reads the breaker file once, visits each tunnel, writes it back once. A
/// pass that cannot read the file stops rather than starting over, because
/// starting over would restart tunnels a previous pass deliberately stopped.
pub async fn one_pass(verbose: bool) -> Result<()> {
    let state = TunnelState::load()?;
    let (watched, skipped) = targets(&state);
    if verbose {
        for note in &skipped {
            println!("skipped {note}");
        }
    }

    // A file that cannot be read would abort every pass before a single
    // tunnel was looked at, forever, with nothing but a line in the
    // watchdog's own log to say so. Keep it as evidence, say where it went,
    // and start from closed.
    let mut rows = match persist::load() {
        Ok(rows) => rows,
        Err(err) => {
            let moved = persist::set_aside()?;
            eprintln!("{err:#}. Moved it to {} and started over", moved.display());
            persist::BreakerFile::new()
        }
    };
    let live: Vec<String> = watched.iter().map(|t| key(t)).collect();

    for tunnel in watched {
        if let Err(err) = visit(tunnel, &mut rows, SystemTime::now(), verbose).await {
            // One tunnel's failure must not cost every other tunnel its check
            eprintln!("{}: {err:#}", key(tunnel));
        }
    }

    persist::save(&rows, &live)?;
    Ok(())
}

/// Look at one tunnel, feed the breaker, and carry out what it decided.
async fn visit(
    tunnel: &PersistentTunnel,
    rows: &mut persist::BreakerFile,
    now: SystemTime,
    verbose: bool,
) -> Result<()> {
    let Some(addr) = probe_addr(tunnel) else {
        return Ok(());
    };
    let label = daemon::platform_label(&tunnel.account_name, &tunnel.name);
    let status = daemon::status(&label).await;
    let probe_ok = probe::tcp_probe(&addr, PROBE_TIMEOUT).await.is_some();

    let log_path = tunnel.log_path()?;
    let len = std::fs::metadata(&log_path).map_or(0, |m| m.len());

    let row = rows.entry(key(tunnel)).or_insert_with(|| Row {
        // A tunnel seen for the first time starts from the end of its log:
        // everything already in it is history, not news
        log_len: len,
        ..Row::default()
    });
    let gained =
        daemon::read_from(&log_path, unread_from(len, row.log_len)).contains(DESCRIPTOR_ERROR);
    row.log_len = len;

    let obs = observe(status, probe_ok, gained);
    let reason = match obs {
        Observation::Failed(reason) => Some(reason),
        _ => None,
    };
    // Kept so a decision the service manager refuses can be undone
    let before = row.breaker.clone();
    let action = row.breaker.observe(obs, now);
    if verbose {
        println!("{}: {obs:?} -> {action:?}", key(tunnel));
    }

    let holders = match action {
        // Only asked for when it is about to be printed: one lsof listing
        // per pass per wedged tunnel, never on the healthy path
        Action::StopAndReport(_) => probe::holders_for_port(local_port(tunnel).unwrap_or(0)),
        _ => Vec::new(),
    };
    // The action first, then the line that describes it. A breaker that
    // moved on a restart the service manager refused would spend the whole
    // ladder on restarts that never happened, and park in Stopped reporting
    // a tunnel that is still up.
    let outcome = match action {
        Action::Nothing => Ok(()),
        // Stop first, exactly as `gtunnel restart` does. Starting alone is
        // not a restart: `launchctl kickstart` without `-k` and `systemctl
        // start` are both no-ops on a service that is already running, and
        // a tunnel that is running is the only kind the breaker ever trips
        // on. Verified by watching the pid: without the stop it does not
        // change, and four restarts cost four nothings. Unlike `gtunnel
        // restart`, a failed stop is fatal here rather than ignored, because
        // nobody is watching the start that would quietly do nothing.
        Action::Restart => match daemon::stop(&label).await {
            Ok(()) => daemon::start(&label).await,
            Err(err) => Err(err),
        },
        Action::StopAndReport(_) => daemon::stop(&label).await,
    };

    let failed = outcome.as_ref().err().map(|e| format!("{e:#}"));
    if failed.is_some() {
        // The decision assumed the action took. Put the breaker back so the
        // next pass tries again rather than living in a state it never
        // actually reached.
        *row = Row {
            breaker: before,
            log_len: row.log_len,
        };
    }
    if let Some(line) = action_line(action, reason, &holders, failed.as_deref()) {
        let _ = daemon::log_event(&log_path, &line);
    }
    outcome
}

fn local_port(tunnel: &PersistentTunnel) -> Option<u16> {
    match &tunnel.spec {
        crate::state::TunnelSpec::Access { local_port, .. } => Some(*local_port),
        crate::state::TunnelSpec::Expose { .. } => None,
    }
}

/// How long between passes when `watch` runs in the foreground. The agent
/// runs `--once` on its own thirty-second interval instead.
const LOOP_INTERVAL: Duration = Duration::from_secs(30);

/// `gtunnel watch`. One pass and out, which is what the agent invokes, or a
/// foreground loop, which is what a person debugging it runs.
pub async fn cmd_watch_install() -> Result<()> {
    // The agent runs this very binary, so the path has to be the one running
    // now rather than whatever is on PATH at install time
    let program = std::env::current_exe()?;
    daemon::install_watchdog(&program).await?;
    println!(
        "Watchdog installed: {} every {}s",
        program.display(),
        daemon::WATCHDOG_INTERVAL
    );
    println!("Log: {}", daemon::watchdog_log()?.display());
    Ok(())
}

pub async fn cmd_watch_uninstall() -> Result<()> {
    daemon::uninstall_watchdog().await?;
    println!("Watchdog removed");
    Ok(())
}

pub async fn cmd_watch_status() -> Result<()> {
    if daemon::watchdog_installed().await {
        println!(
            "Watchdog installed, looking at every tunnel every {}s",
            daemon::WATCHDOG_INTERVAL
        );
        println!("Log: {}", daemon::watchdog_log()?.display());
    } else {
        println!("Watchdog not installed. Run `gtunnel watch install`");
    }
    Ok(())
}

pub async fn cmd_watch(once: bool, verbose: bool) -> Result<()> {
    if once {
        return one_pass(verbose).await;
    }
    println!(
        "Watching every {}s. Ctrl-C to stop.",
        LOOP_INTERVAL.as_secs()
    );
    loop {
        // A pass that failed is reported and the next one still happens: the
        // watchdog going quiet is worse than one bad pass
        if let Err(err) = one_pass(verbose).await {
            eprintln!("pass failed: {err:#}");
        }
        tokio::time::sleep(LOOP_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breaker::{Action, Reason};
    use crate::probe::Holder;
    use crate::state::TunnelStatus;

    // The offset a pass remembers is a position in the FILE. Reading a
    // tail window and indexing it with that offset compares two different
    // numbers, and on any log past the window it silently rescans the whole
    // window: an error from hours ago is then read as news on every pass,
    // and every probe failure of that tunnel is blamed on descriptors
    #[test]
    fn a_descriptor_error_the_last_pass_already_saw_is_not_read_as_news() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("dev-k8s.log");

        // Bigger than one tail window, with the error well inside it
        let filler = "debug1: forwarding a connection\n".repeat(5000);
        let old = format!("{filler}error: accept: Too many open files\n{filler}");
        assert!(old.len() as u64 > crate::daemon::TAIL_WINDOW);
        std::fs::write(&log, &old).expect("write");
        let seen = old.len() as u64;

        // The pass after it: the log gained a line, but not that one
        std::fs::write(&log, format!("{old}debug1: another connection\n")).expect("write");
        let len = std::fs::metadata(&log).expect("metadata").len();

        let fresh = crate::daemon::read_from(&log, unread_from(len, seen));
        assert!(
            !fresh.contains("Too many open files"),
            "read {} bytes of history as new: {fresh:.120}",
            fresh.len()
        );

        // A log emptied by a restart is shorter than the offset, and then
        // everything in it really is new
        std::fs::write(&log, "error: accept: Too many open files\n").expect("write");
        let shorter = std::fs::metadata(&log).expect("metadata").len();
        assert_eq!(unread_from(shorter, seen), 0);
        assert!(crate::daemon::read_from(&log, unread_from(shorter, seen))
            .contains("Too many open files"));
    }

    fn holder(command: &str, pid: u32, count: u64) -> Holder {
        Holder {
            pid,
            command: command.to_string(),
            count,
        }
    }

    // A healthy tunnel is the overwhelmingly common case, thirty seconds at
    // a time, forever. It must leave no trace in the log at all
    #[test]
    fn a_pass_over_a_healthy_tunnel_changes_nothing_and_writes_nothing() {
        assert_eq!(
            observe(TunnelStatus::Running, true, false),
            Observation::Pass
        );
        assert_eq!(log_line(Action::Nothing, None, &[]), None);
    }

    #[test]
    fn a_tunnel_running_and_refusing_connections_is_the_wedged_case() {
        assert_eq!(
            observe(TunnelStatus::Running, false, false),
            Observation::Failed(Reason::ProbeRefused)
        );
    }

    // The log is what says which of the two failures this is, and the
    // message a person reads depends on getting it right
    #[test]
    fn a_log_that_gained_a_descriptor_error_names_that_as_the_reason() {
        assert_eq!(
            observe(TunnelStatus::Running, false, true),
            Observation::Failed(Reason::DescriptorsExhausted)
        );
    }

    // A tunnel nobody started is not a tunnel with a problem
    #[test]
    fn a_tunnel_that_is_not_running_is_not_judged() {
        for status in [TunnelStatus::Stopped, TunnelStatus::Error] {
            assert_eq!(observe(status, false, true), Observation::NotRunning);
        }
    }

    #[test]
    fn the_log_line_for_a_trip_names_the_reason() {
        let line = log_line(Action::Restart, Some(Reason::DescriptorsExhausted), &[])
            .expect("a restart is worth a line");
        assert!(line.contains("restart"), "{line}");
        assert!(
            line.contains("file descriptors") || line.contains("descriptors"),
            "{line}"
        );
    }

    // The whole point of attributing the connections: the message that ends
    // the restarts has to say who to go and talk to
    #[test]
    fn stopping_a_tunnel_names_the_process_holding_its_connections() {
        let line = log_line(
            Action::StopAndReport(Reason::DescriptorsExhausted),
            None,
            &[holder("mcp-server", 19191, 215), holder("psql", 400, 2)],
        )
        .expect("a stop is worth a line");
        assert!(line.contains("mcp-server"), "{line}");
        assert!(line.contains("19191"), "{line}");
        assert!(line.contains("215"), "{line}");
        assert!(line.contains("stopped"), "{line}");
    }

    // Linux has no lsof in the minimal case, and a listing can fail for
    // reasons of its own. Not knowing the name is not a reason to say nothing
    #[test]
    fn a_machine_without_lsof_still_stops_the_tunnel_and_says_so_without_a_name() {
        let line = log_line(Action::StopAndReport(Reason::ProbeRefused), None, &[])
            .expect("a stop is worth a line even unattributed");
        assert!(line.contains("stopped"), "{line}");
        assert!(
            line.contains("could not") || line.contains("no process"),
            "the line must admit it cannot name the holder: {line}"
        );
    }

    // A public tunnel's health lives in a load balancer, not in a local
    // port, so a TCP probe here would be answering a different question
    #[test]
    fn a_public_tunnel_is_skipped_and_said_to_be_skipped() {
        let state = crate::state::TunnelState {
            tunnels: vec![access("kubernetes"), expose("docs")],
        };
        let (watched, skipped) = targets(&state);

        assert_eq!(watched.len(), 1);
        assert_eq!(watched[0].name, "kubernetes");
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(skipped[0].contains("docs"), "{:?}", skipped[0]);
    }

    // The log must describe what happened, not what was decided. A
    // `launchctl bootout` that fails leaves the tunnel running and still
    // refusing connections, and a log line saying it was stopped sends the
    // person looking for a tunnel that is right there in `gtunnel list`
    #[test]
    fn an_action_the_service_manager_refused_is_not_logged_as_done() {
        let refused = action_line(
            Action::Restart,
            Some(Reason::ProbeRefused),
            &[],
            Some("launchctl bootout failed: Operation not permitted"),
        )
        .expect("a line");
        assert!(!refused.contains("restarting it"), "{refused}");
        assert!(refused.contains("Operation not permitted"), "{refused}");

        let unstopped = action_line(
            Action::StopAndReport(Reason::DescriptorsExhausted),
            None,
            &[],
            Some("launchctl bootout failed: Operation not permitted"),
        )
        .expect("a line");
        assert!(!unstopped.contains("stopped after"), "{unstopped}");
        assert!(unstopped.contains("still running"), "{unstopped}");

        // Nothing to do is still nothing to say, however it went
        assert_eq!(action_line(Action::Nothing, None, &[], Some("boom")), None);
    }

    // Reads the log the way a pass does: from a file, at a file offset
    fn read_since(log: &std::path::Path, seen: u64) -> bool {
        let len = std::fs::metadata(log).expect("metadata").len();
        crate::daemon::read_from(log, unread_from(len, seen)).contains(DESCRIPTOR_ERROR)
    }

    // A line already in the log when the watchdog first looks is history,
    // not news: tripping on it would stop a tunnel that is working
    #[test]
    fn a_descriptor_error_already_in_the_log_is_not_news() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("dev-k8s.log");
        let text = "accept: Too many open files\n";
        std::fs::write(&log, text).expect("write");
        assert!(!read_since(&log, text.len() as u64));
        assert!(read_since(&log, 0));
    }

    // The log is read from where the last pass stopped, so a line that
    // arrived since is the only kind that counts
    #[test]
    fn only_the_part_of_the_log_that_is_new_is_read_for_the_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("dev-k8s.log");
        let old = "starting kubernetes on localhost:8443\n";
        std::fs::write(&log, old).expect("write");
        assert!(!read_since(&log, old.len() as u64));

        std::fs::write(&log, format!("{old}accept: Too many open files\n")).expect("write");
        assert!(read_since(&log, old.len() as u64));
    }

    fn access(name: &str) -> crate::state::PersistentTunnel {
        crate::state::PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: crate::state::TunnelSpec::Access {
                target: crate::state::AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 443,
                local_port: 8443,
            },
        }
    }

    fn expose(name: &str) -> crate::state::PersistentTunnel {
        crate::state::PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: crate::state::TunnelSpec::Expose {
                local_target: "localhost:3000".to_string(),
                domain: "example.com".to_string(),
                hostname: "docs.example.com".to_string(),
                relay_port: 30123,
                private: false,
            },
        }
    }
}
