use crate::logfilter::{lock, Backend, Correlator};
use crate::restart_budget::{judge, judge_busy, Budget, Busy, History, Verdict};
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

/// Where a spent budget is recorded, and the budget that decides it.
pub struct Spend {
    pub marker: PathBuf,
    pub budget: Budget,
}

/// Run a child process, pumping its stdout and stderr through the log filter, then exit with its status.
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub async fn run(
    argv: Vec<String>,
    port: Option<u16>,
    ready_file: Option<PathBuf>,
    conns: Option<Backend>,
    auth_marker: Option<PathBuf>,
    crash_marker: Option<PathBuf>,
) -> Result<()> {
    // Read once, here, so a running supervisor keeps the budget it started
    // with. A bad value must not kill the supervisor: the service manager
    // would restart it into the same error forever, which is the loop this
    // budget exists to end. The default applies and the log says why.
    let budget = crate::config::load_config()
        .and_then(|c| c.restart_budget())
        .unwrap_or_else(|e| {
            eprintln!("restart budget: {e:#}. Using the defaults");
            Budget::default()
        });
    let spend = crash_marker.map(|marker| Spend { marker, budget });
    // Before anything is registered or spawned: a stop that lands in the wait
    // ends the process by its default action, and there is no child to orphan
    let asked = ready_file
        .as_deref()
        .is_some_and(|ready| take_asked(&asked_beside(ready)));
    tokio::time::sleep(start_delay(asked, crate::daemon::backoff::draw())).await;
    let code = supervise(argv, port, ready_file, conns, auth_marker, spend).await?;
    std::process::exit(code);
}

/// Run the child and return the code this process should exit with, once its
/// streams have drained.
///
/// That is the child's own, except when it died of expired credentials on its
/// own: restarting gcloud cannot fix a login, so the exit is the one the
/// service manager leaves alone, and a marker records why the job is down. A
/// stop the user asked for is not that, whatever the log says.
pub async fn supervise(
    argv: Vec<String>,
    port: Option<u16>,
    ready_file: Option<PathBuf>,
    conns: Option<Backend>,
    auth_marker: Option<PathBuf>,
    spend: Option<Spend>,
) -> Result<i32> {
    if argv.is_empty() {
        bail!("__supervise requires at least one argument (the program to run)");
    }
    let prog = &argv[0];
    let args = &argv[1..];

    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new(prog);
        c.process_group(0);
        c
    };
    #[cfg(not(unix))]
    let mut cmd = Command::new(prog);

    // A login that was renewed since the last run must not keep naming itself
    if let Some(file) = &auth_marker {
        let _ = std::fs::remove_file(file);
    }
    // Whoever starts this supervisor is starting the job again, so the reason
    // it was parked no longer holds. The count stays: it is what lets a loop
    // of restarts add up across supervisors.
    if let Some(spend) = &spend {
        let _ = std::fs::remove_file(&spend.marker);
    }
    // A ready file left by a previous run would say the listener is up before
    // this child has bound anything
    if let Some(file) = &ready_file {
        let _ = std::fs::remove_file(file);
    }
    let mut conn =
        Correlator::new(crate::loglevel::current()).with_settings(&crate::settings::current());
    // The probe ledger sits beside the ready file, so it is found by what the
    // installing process wrote down and not by this process's environment
    let probes = match (&ready_file, port) {
        (Some(file), Some(port)) => file.parent().map(|d| (d.join("probes"), port)),
        (None, Some(port)) => crate::probe::probes_dir().map(|d| (d, port)),
        _ => None,
    }
    .filter(|(dir, port)| crate::probe::watch_probes(dir, *port).is_ok());
    if let Some((dir, port)) = &probes {
        conn = conn.with_probes(dir.clone(), *port);
    }
    if let Some(backend) = conns {
        conn = conn.with_backend(backend);
    }
    if let Some(file) = &ready_file {
        conn = conn.with_ready(file.clone(), port);
    }
    let conn = conn.share();

    // Registered once, before the child exists, and kept to the end. tokio
    // leaves its handler installed for the process once a stream has been
    // made, so a stream that lives only inside one `select!` makes every
    // signal outside it vanish: during the pipe join and the restart backoff
    // a stop would be caught and read by nobody. Registering late has the
    // opposite fault, the default action killing us between spawn and the
    // handler and orphaning gcloud.
    let mut stops = Stops::new()?;

    // A supervisor that was SIGKILLed never swept its group
    let group_file = ready_file.as_ref().map(|f| group_file_beside(f));
    if let Some(file) = &group_file {
        sweep_stale_group(file).await;
    }

    let began = std::time::Instant::now();
    let mut child = cmd
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn {prog}"))?;

    #[cfg(unix)]
    if let (Some(file), Some(pid)) = (&group_file, child.id()) {
        record_group(file, pid);
    }

    let stdout = child.stdout.take().context("child stdout missing")?;
    let stderr = child.stderr.take().context("child stderr missing")?;

    let out_conn = conn.clone();
    let out_task = tokio::spawn(async move {
        let reader = tokio::io::BufReader::new(stdout);
        let mut out = std::io::stdout();
        crate::logfilter::pump(reader, &mut out, out_conn).await
    });

    let err_conn = conn.clone();
    let err_task = tokio::spawn(async move {
        let reader = tokio::io::BufReader::new(stderr);
        let mut out = std::io::stderr();
        crate::logfilter::pump(reader, &mut out, err_conn).await
    });

    // Opens are held back until they outlive a probe, and nothing arrives to
    // release them when the client just sits there, so time has to
    let tick_conn = conn.clone();
    let tick_task = tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_millis(500));
        loop {
            every.tick().await;
            let lines = lock(&tick_conn).tick(std::time::Instant::now());
            let mut err = std::io::stderr();
            for l in lines {
                let _ = err.write_all(l.as_bytes());
            }
        }
    });

    let mut asked_to_stop = false;
    #[cfg(unix)]
    let status = {
        let pgid = child.id().map(|id| id as i32);
        let status = tokio::select! {
            res = child.wait() => res.context("failed to wait on child")?,
            _ = stops.next() => {
                asked_to_stop = true;
                crate::proc::stop_group(&mut child, pgid, crate::proc::STOP_GRACE).await?
            }
        };
        // gcloud is a launcher: when it dies on its own, ssh or the IAP proxy
        // may still hold the port and the pipes. launchd only cleans up the
        // job's own group, and gcloud was moved out of it, so the sweep is
        // ours, as it is in `run`.
        if !asked_to_stop {
            tokio::select! {
                res = crate::proc::stop_group(&mut child, pgid, crate::proc::STOP_GRACE) => {
                    res?;
                }
                _ = stops.next() => {
                    asked_to_stop = true;
                    if let Some(pgid) = pgid {
                        crate::proc::signal_group(pgid, libc::SIGKILL);
                    }
                }
            }
        }
        status
    };

    #[cfg(not(unix))]
    let status = child.wait().await.context("failed to wait on child")?;

    // Bounded: a member that outlived the sweep, or sits in another group,
    // still holds the pipes open, and a join with no end is a supervisor that
    // never restarts the tunnel
    tokio::select! {
        _ = tokio::time::timeout(PIPE_DRAIN, async { tokio::join!(out_task, err_task) }) => {}
        _ = stops.next() => asked_to_stop = true,
    }
    tick_task.abort();
    let mut err = std::io::stderr();
    for l in lock(&conn).finish() {
        let _ = err.write_all(l.as_bytes());
    }
    if let Some(file) = &ready_file {
        let _ = std::fs::remove_file(file);
    }
    if let Some(file) = &group_file {
        let _ = std::fs::remove_file(file);
    }
    if let Some((dir, port)) = &probes {
        crate::probe::unwatch_probes(dir, *port);
    }
    let busy = lock(&conn).listener_busy(std::time::Instant::now());
    let stopping = lock(&conn).stopping_failure(std::time::Instant::now());
    let expired = stopping.is_some();
    if let Some(failure) = stopping
        && stops_for_credentials(status.success(), expired, asked_to_stop)
    {
        // The slug says which failure it was, so `list` can tell a login the
        // user renews from a key or an impersonation they have to fix
        if let Some(file) = &auth_marker {
            let _ = crate::fsutil::write_atomic(file, &format!("{}\n", failure.slug()));
        }
        return Ok(crate::daemon::auth_expired_exit(cfg!(target_os = "macos")));
    }
    let mut code = status.code().unwrap_or(1);
    if exited_clean_unasked(status.success(), asked_to_stop) {
        // Outside the budget and the park code on purpose: the tunnel is not
        // failing, it is gone, and only a non-zero exit gets it started again
        let _ = writeln!(std::io::stderr(), "gcloud exited 0 on its own, restarting");
        code = 1;
    }
    let outage = is_outage_exit(
        status.success(),
        asked_to_stop,
        lock(&conn).outage_failure(std::time::Instant::now()),
    );
    if outage && let Some(spend) = &spend {
        // Not the tunnel's fault and not the budget's business: wait, count
        // it, and let the service manager start gcloud again. Never parks,
        // however long the network stays away.
        let file = crate::daemon::net_failures_beside(&spend.marker);
        let mut fails = crate::restart_budget::parse_net_failures(
            &std::fs::read_to_string(&file).unwrap_or_default(),
        );
        let after = crate::restart_budget::network_wait(
            &spend.budget,
            &mut fails,
            began.elapsed(),
            crate::daemon::backoff::draw(),
        );
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = crate::fsutil::write_atomic(&file, &format!("{fails}\n"));
        tokio::select! {
            _ = tokio::time::sleep(after) => {}
            _ = stops.next() => {}
        }
        // The outage is the cause, so a busy streak would only be a second
        // clock running on the same fault: it starts over when the network is back
        let _ = std::fs::remove_file(crate::daemon::busy_beside(&spend.marker));
        return Ok(code);
    }
    // A relay that still holds the dead session's listener frees it within
    // about 30 s, so waiting is the cure and the budget is not spent. The
    // streak is kept in a file because each attempt is a new process.
    if let Some(spend) = &spend {
        let busy_file = crate::daemon::busy_beside(&spend.marker);
        if !(busy && spends_budget(status.success(), expired, asked_to_stop)) {
            let _ = std::fs::remove_file(&busy_file);
        } else {
            let since = std::fs::read_to_string(&busy_file)
                .ok()
                .and_then(|t| crate::restart_budget::parse_since(&t));
            let now = std::time::SystemTime::now();
            if let Busy::Wait { since, after } = judge_busy(
                &spend.budget,
                since,
                began.elapsed(),
                now,
                crate::daemon::backoff::draw(),
            ) {
                if let Some(parent) = busy_file.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = crate::fsutil::write_atomic(
                    &busy_file,
                    &crate::restart_budget::render_since(since),
                );
                tokio::select! {
                    _ = tokio::time::sleep(after) => {}
                    _ = stops.next() => {}
                }
                return Ok(code);
            }
        }
    }
    if let Some(spend) = &spend
        && spends_budget(status.success(), expired, asked_to_stop)
    {
        let history_file = crate::daemon::restart_history_beside(&spend.marker);
        let mut history =
            History::parse(&std::fs::read_to_string(&history_file).unwrap_or_default());
        let verdict = judge(
            &spend.budget,
            &mut history,
            began.elapsed(),
            std::time::SystemTime::now(),
            crate::daemon::backoff::draw(),
        );
        if let Some(parent) = history_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = crate::fsutil::write_atomic(&history_file, &history.render());
        if stops_for_budget(verdict) {
            let _ = crate::fsutil::write_atomic(&spend.marker, &format!("{status}\n"));
            return Ok(crate::daemon::auth_expired_exit(cfg!(target_os = "macos")));
        }
        if let Verdict::Restart { after } = verdict {
            // The service manager restarts at its own pace; this is the part
            // that grows, so a loop slows down before it is stopped
            tokio::select! {
                _ = tokio::time::sleep(after) => {}
                // A stop here is the one that matters most: the tunnel is
                // already down and `stop` or `delete` is waiting on us
                _ = stops.next() => {}
            }
        }
    }
    Ok(code)
}

/// How long the pipes get to drain once the group has been swept. Everything
/// that could hold them open is dead by then, so this is only the bound on a
/// member that escaped its group.
const PIPE_DRAIN: Duration = Duration::from_secs(2);

/// Every way this process is asked to stop, as one stream that outlives the
/// child. See the note at its creation for why it must not be made per wait.
struct Stops {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
}

impl Stops {
    #[cfg(unix)]
    fn new() -> Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
        })
    }

    #[cfg(not(unix))]
    fn new() -> Result<Self> {
        Ok(Self {})
    }

    #[cfg(unix)]
    async fn next(&mut self) {
        tokio::select! {
            _ = self.term.recv() => {}
            _ = self.int.recv() => {}
        }
    }

    #[cfg(not(unix))]
    async fn next(&mut self) {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Where the child's group is written down: beside the ready file, so the
/// next supervisor of the same job finds it by what the installer chose.
pub(crate) fn group_file_beside(ready: &std::path::Path) -> PathBuf {
    ready.with_extension("pgid")
}

/// Write the group id and the leader's start token. The id alone is not
/// enough to signal later: after a reboot the number may belong to anything.
#[cfg(unix)]
fn record_group(file: &std::path::Path, pgid: u32) {
    let token = crate::proc::start_token(pgid).unwrap_or_default();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::fsutil::write_atomic(file, &format!("{pgid} {token}\n"));
}

/// Kill the group a previous supervisor left behind, and forget it.
///
/// A leader that is still alive must carry the recorded token, or the number
/// was reused. A leader that is gone cannot be checked, but its group id
/// cannot be handed to another process while any member remains, so a group
/// that still answers is the old one.
async fn sweep_stale_group(file: &std::path::Path) {
    #[cfg(unix)]
    {
        let Ok(text) = std::fs::read_to_string(file) else {
            return;
        };
        let _ = std::fs::remove_file(file);
        let mut parts = text.split_whitespace();
        let Some(pgid) = parts.next().and_then(|p| p.parse::<i32>().ok()) else {
            return;
        };
        let recorded = parts.next();
        let leader = crate::proc::start_token(pgid as u32);
        let ours = match (&leader, recorded) {
            (Some(now), Some(then)) => now == then,
            (Some(_), None) => false,
            (None, _) => true,
        };
        if !ours || !crate::proc::group_alive(pgid) {
            return;
        }
        crate::proc::signal_group(pgid, libc::SIGTERM);
        let deadline = std::time::Instant::now() + crate::proc::STOP_GRACE;
        while crate::proc::group_alive(pgid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        crate::proc::signal_group(pgid, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = file;
}

/// A start asked for by a person or by the watchdog, written by `daemon::start`
/// just before it asks the service manager, and read once by the supervisor
/// that start produces.
///
/// A supervisor cannot tell who started it: a login or a wake loads every job
/// at once, and `gtunnel start` kickstarts one through the same manager. The
/// marker is the difference. Without it the first `gcloud` waits a random few
/// seconds so five jobs do not start together; with it nobody who asked for
/// this tunnel waits for a draw. It is beside the ready file, named by the
/// label, so the two sides agree on it without being told.
pub fn asked_beside(ready_file: &std::path::Path) -> PathBuf {
    ready_file.with_extension("asked")
}

/// How old a marker may be and still be the start that produced this
/// supervisor. One older than this is left over from a start that never
/// reached a supervisor, and must not excuse a later login from the draw.
const ASKED_FRESH: Duration = Duration::from_secs(60);

/// Whether a person or the watchdog asked for this start. The marker is
/// removed either way, so it excuses one supervisor and no more.
fn take_asked(marker: &std::path::Path) -> bool {
    let fresh = std::fs::metadata(marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| std::time::SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age <= ASKED_FRESH);
    let _ = std::fs::remove_file(marker);
    fresh
}

/// The wait before the first `gcloud`: none when someone asked for this start,
/// otherwise a draw from zero to [`crate::daemon::backoff::START_SPREAD`].
fn start_delay(asked: bool, unit: f64) -> Duration {
    if asked {
        Duration::ZERO
    } else {
        crate::daemon::backoff::spread_at(crate::daemon::backoff::START_SPREAD, unit)
    }
}

/// Whether this exit counts against the budget. Credentials have their own
/// stop, a clean exit is not a failure, and a stop the user asked for says
/// nothing about the tunnel.
fn spends_budget(succeeded: bool, expired: bool, asked_to_stop: bool) -> bool {
    !(succeeded || expired || asked_to_stop)
}

/// Whether gcloud died because the network was gone: it failed on its own and
/// the newest thing it said was a network or timeout line. Such an exit is
/// waited out rather than counted, because no restart count can fix it.
fn is_outage_exit(
    succeeded: bool,
    asked_to_stop: bool,
    last: Option<crate::failure::Failure>,
) -> bool {
    use crate::failure::Failure;
    !succeeded && !asked_to_stop && matches!(last, Some(Failure::Network | Failure::Timeout))
}

/// Whether the child quit with 0 and nobody asked it to. The service managers
/// treat 0 as finished, and on macOS 0 is also the deliberate park code, so
/// this exit has to be turned into a failure to bring the tunnel back.
fn exited_clean_unasked(succeeded: bool, asked_to_stop: bool) -> bool {
    succeeded && !asked_to_stop
}

/// Whether the verdict ends the job.
fn stops_for_budget(verdict: Verdict) -> bool {
    matches!(verdict, Verdict::Stop)
}

/// Whether this exit is the permanent kind. Only gcloud dying on its own
/// qualifies: a stop the user asked for, or a clean exit, says nothing about
/// the login whatever the log held.
fn stops_for_credentials(succeeded: bool, expired: bool, asked_to_stop: bool) -> bool {
    expired && !asked_to_stop && !succeeded
}

#[cfg(test)]
mod tests {

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_start_nobody_asked_for_waits_a_draw_from_zero_to_three_seconds() {
        assert_eq!(start_delay(false, 0.0), Duration::ZERO);
        assert_eq!(start_delay(false, 0.5), Duration::from_millis(1500));
        assert_eq!(start_delay(false, 1.0), secs(3));
    }

    #[test]
    fn a_start_a_person_or_the_watchdog_asked_for_does_not_wait() {
        assert_eq!(start_delay(true, 1.0), Duration::ZERO);
    }

    #[test]
    fn the_marker_is_found_beside_the_ready_file_by_the_name_the_installer_chose() {
        let dir = std::path::Path::new("/cfg");
        let ready = crate::daemon::ready_file_in(dir, "com.gtunnel.dev.db");
        assert_eq!(
            asked_beside(&ready),
            crate::daemon::asked_file_in(dir, "com.gtunnel.dev.db"),
            "a dotted label must not lose a segment"
        );
    }

    #[test]
    fn an_asked_marker_excuses_one_supervisor_and_is_gone_after() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = crate::daemon::asked_file_in(dir.path(), "com.gtunnel.dev.db");
        assert!(!take_asked(&marker), "no marker, no one asked");
        crate::daemon::note_asked_in(dir.path(), "com.gtunnel.dev.db");
        assert!(marker.exists());
        assert!(take_asked(&marker));
        assert!(!marker.exists(), "read once");
        assert!(!take_asked(&marker), "the next supervisor waits its draw");
    }

    #[test]
    fn a_marker_from_a_start_that_never_arrived_does_not_excuse_a_later_login() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.asked");
        std::fs::write(&marker, "").expect("write");
        let old = std::time::SystemTime::now() - secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&marker)
            .expect("open")
            .set_modified(old)
            .expect("mtime");
        assert!(!take_asked(&marker));
        assert!(!marker.exists(), "a stale marker is swept, not kept");
    }
    use super::*;

    #[test]
    fn a_stop_the_user_asked_for_is_never_an_auth_exit() {
        assert!(stops_for_credentials(false, true, false));
        assert!(!stops_for_credentials(false, true, true), "SIGTERM");
        assert!(!stops_for_credentials(true, true, false), "clean exit");
        assert!(
            !stops_for_credentials(false, false, false),
            "no credential line"
        );
    }

    #[test]
    fn stops_for_budget_when_judge_says_stop() {
        assert!(stops_for_budget(Verdict::Stop));
        assert!(!stops_for_budget(Verdict::Restart {
            after: Duration::ZERO
        }));
        // Only a failure of gcloud's own makes a run count
        assert!(spends_budget(false, false, false));
        assert!(!spends_budget(false, false, true), "SIGTERM");
        assert!(!spends_budget(true, false, false), "clean exit");
        assert!(!spends_budget(false, true, false), "credentials stop first");
    }

    fn tiny(max_restarts: u32, min_uptime: Duration) -> Budget {
        Budget {
            max_restarts,
            window: Duration::from_secs(600),
            min_uptime,
            base_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            busy_delay: Duration::from_millis(1),
            busy_window: Duration::from_secs(300),
        }
    }

    const BUSY_LINE: &str = "echo 'Error: remote port forwarding failed for listen port 30123' >&2";
    const BUSY: &str =
        "echo 'Error: remote port forwarding failed for listen port 30123' >&2; exit 255";

    // The relay keeps a dead session's listener until sshd reaps it, so every
    // restart inside that window fails the same way. None of them is the
    // tunnel's fault, and parking the job after six would make a laptop that
    // slept need a person
    #[tokio::test]
    async fn a_run_of_taken_relay_ports_never_parks_the_job_inside_its_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.crash");
        let budget = tiny(2, Duration::from_secs(60));
        for n in 0..12 {
            assert_eq!(run_once(dir.path(), BUSY, budget).await, 255, "attempt {n}");
        }
        assert!(!marker.exists(), "parked");
        assert!(
            !crate::daemon::restart_history_beside(&marker).exists(),
            "none of them spent the budget"
        );
        assert!(crate::daemon::busy_beside(&marker).exists());
    }

    #[tokio::test]
    async fn a_relay_that_never_lets_go_is_counted_once_the_window_is_spent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.crash");
        let budget = tiny(2, Duration::from_secs(60));
        std::fs::write(crate::daemon::busy_beside(&marker), "1000\n").expect("write");
        for _ in 0..3 {
            run_once(dir.path(), BUSY, budget).await;
        }
        assert!(marker.exists(), "the third spent failure parks it");
    }

    #[tokio::test]
    async fn any_other_exit_ends_the_streak() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.crash");
        let budget = tiny(5, Duration::from_secs(60));
        run_once(dir.path(), BUSY, budget).await;
        assert!(crate::daemon::busy_beside(&marker).exists());
        run_once(dir.path(), "exit 1", budget).await;
        assert!(!crate::daemon::busy_beside(&marker).exists());
    }

    async fn run_once(dir: &std::path::Path, script: &str, budget: Budget) -> i32 {
        supervise(
            fake_gcloud(script),
            None,
            None,
            None,
            None,
            Some(Spend {
                marker: dir.join("com.gtunnel.dev.db.crash"),
                budget,
            }),
        )
        .await
        .expect("run")
    }

    // The service manager starts one supervisor per attempt, so the count has
    // to live in a file: three runs here are three processes there
    #[tokio::test]
    async fn a_child_that_exits_instantly_forever_is_stopped_with_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.crash");
        let budget = tiny(2, Duration::from_secs(60));
        assert_eq!(run_once(dir.path(), "exit 1", budget).await, 1);
        assert_eq!(run_once(dir.path(), "exit 1", budget).await, 1);
        assert!(!marker.exists(), "still inside the budget");
        let want = if cfg!(target_os = "macos") {
            0
        } else {
            crate::daemon::AUTH_EXPIRED_EXIT
        };
        assert_eq!(run_once(dir.path(), "exit 1", budget).await, want);
        let reason = std::fs::read_to_string(&marker).expect("marker");
        assert!(reason.contains("exit status: 1"), "{reason}");
    }

    // `run/` is made by whoever wrote a ready file first, and a tunnel with
    // none must not lose its count to a directory that was never created
    #[tokio::test]
    async fn the_run_directory_is_created_when_the_count_has_to_be_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let run = dir.path().join("run");
        let budget = tiny(2, Duration::from_secs(60));
        run_once(&run, "exit 1", budget).await;
        assert!(run.join("com.gtunnel.dev.db.restarts").exists());
    }

    #[tokio::test]
    async fn a_run_past_min_uptime_resets_the_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let history = dir.path().join("com.gtunnel.dev.db.restarts");
        // A fast failure must stay under min_uptime even on a loaded runner, so
        // the bound is seconds wide, not milliseconds
        let budget = tiny(2, Duration::from_secs(2));
        run_once(dir.path(), "exit 1", budget).await;
        run_once(dir.path(), "exit 1", budget).await;
        assert_eq!(
            std::fs::read_to_string(&history)
                .expect("history")
                .lines()
                .count(),
            2
        );
        // It worked for longer than min_uptime, then died
        assert_eq!(run_once(dir.path(), "sleep 3; exit 1", budget).await, 1);
        assert_eq!(std::fs::read_to_string(&history).expect("history"), "");
        assert_eq!(
            run_once(dir.path(), "exit 1", budget).await,
            1,
            "fresh budget"
        );
        assert!(!dir.path().join("com.gtunnel.dev.db.crash").exists());
    }

    #[tokio::test]
    async fn a_zero_budget_never_stops_and_a_stale_marker_is_cleared_at_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.crash");
        std::fs::write(&marker, "old\n").expect("stale");
        let budget = tiny(0, Duration::from_secs(60));
        for _ in 0..4 {
            assert_eq!(run_once(dir.path(), "exit 1", budget).await, 1);
        }
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn runs_child_and_returns_exit_code() {
        let code = supervise(
            vec!["echo".to_string(), "hello".to_string()],
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("run echo");
        // Nobody asked it to stop, so even a clean exit is a restart
        assert_eq!(code, 1);
    }

    #[tokio::test]
    async fn returns_non_zero_exit_code_on_child_failure() {
        let code = supervise(vec!["false".to_string()], None, None, None, None, None)
            .await
            .expect("run false");
        assert_eq!(code, 1);
    }

    // The whole path: a child that prints what gcloud prints, a ready file the
    // supervisor owns, and a probe's close that must not reach the log.
    #[tokio::test]
    async fn the_ready_file_is_cleared_at_start_written_on_listening_and_removed_at_exit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("com.gtunnel.dev.db.ready");
        std::fs::write(&ready, "1\n").expect("stale");
        let code = supervise(
            vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "test ! -e '{0}' && echo 'Listening on port [18443].' && sleep 1 && test -e '{0}'",
                    ready.display()
                ),
            ],
            Some(18443),
            Some(ready.clone()),
            None,
            None,
            None,
        )
        .await
        .expect("run");
        // Gone before the child started, and back once it said it listens;
        // it then quit on its own, which is reported as a failure
        assert_eq!(code, 1);
        assert!(!ready.exists(), "removed when the supervisor exits");
        assert!(
            !dir.path().join("probes").join("18443").exists(),
            "the ledger is closed with the supervisor"
        );
    }

    fn fake_gcloud(script: &str) -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), script.to_string()]
    }

    // launchd (`SuccessfulExit:false`) and systemd (`Restart=on-failure`) read
    // 0 as done, so a child that quit by itself has to be reported as a failure
    #[tokio::test]
    async fn a_clean_exit_nobody_asked_for_exits_nonzero() {
        let code = supervise(fake_gcloud("exit 0"), None, None, None, None, None)
            .await
            .expect("run");
        assert_ne!(code, 0, "the service manager would call the tunnel done");
        assert_ne!(code, crate::daemon::AUTH_EXPIRED_EXIT, "that is the park");
        assert_eq!(code, 1);
    }

    #[test]
    fn a_clean_exit_is_restarted_only_when_nobody_asked_for_it() {
        assert!(exited_clean_unasked(true, false));
        assert!(!exited_clean_unasked(true, true), "the person asked");
        assert!(
            !exited_clean_unasked(false, false),
            "a failure has its own paths"
        );
        assert!(!exited_clean_unasked(false, true));
    }

    // A restart cannot fix an expired login, so the exit must be one the
    // service manager does not answer with another start
    #[tokio::test]
    async fn expired_credentials_end_in_the_exit_the_service_manager_does_not_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.auth");
        let code = supervise(
            fake_gcloud("echo 'ERROR: Reauthentication required.' >&2; exit 1"),
            None,
            None,
            None,
            Some(marker.clone()),
            None,
        )
        .await
        .expect("run");
        let want = if cfg!(target_os = "macos") {
            0
        } else {
            crate::daemon::AUTH_EXPIRED_EXIT
        };
        assert_eq!(code, want);
        assert!(marker.exists(), "the marker tells list why it is down");
    }

    // The issue's case: a denied impersonation used to burn the restart budget
    // and park as a crash loop with "check the log". It is a stop on the first
    // failure, with a marker that says which failure it was
    #[tokio::test]
    async fn a_denied_impersonation_stops_after_one_failure_with_its_own_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.auth");
        let crash = dir.path().join("com.gtunnel.dev.db.crash");
        let code = supervise(
            fake_gcloud("echo 'ERROR: Failed to impersonate [sa@example.invalid].' >&2; exit 1"),
            None,
            None,
            None,
            Some(marker.clone()),
            Some(Spend {
                marker: crash.clone(),
                budget: crate::restart_budget::Budget::default(),
            }),
        )
        .await
        .expect("run");
        let want = if cfg!(target_os = "macos") {
            0
        } else {
            crate::daemon::AUTH_EXPIRED_EXIT
        };
        assert_eq!(code, want, "an exit the service manager does not restart");
        assert_eq!(
            std::fs::read_to_string(&marker).expect("marker"),
            "impersonation-denied\n"
        );
        assert!(!crash.exists(), "no budget was spent");
        assert!(
            !crate::daemon::restart_history_beside(&crash).exists(),
            "nothing was counted against the restart budget"
        );
    }

    #[tokio::test]
    async fn an_expired_login_leaves_its_slug_in_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("t.auth");
        supervise(
            fake_gcloud("echo 'ERROR: Reauthentication required.' >&2; exit 1"),
            None,
            None,
            None,
            Some(marker.clone()),
            None,
        )
        .await
        .expect("run");
        assert_eq!(
            std::fs::read_to_string(&marker).expect("marker"),
            "login-expired\n"
        );
    }

    #[tokio::test]
    async fn a_failure_that_is_not_credentials_keeps_gcloud_exit_code_and_leaves_no_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("t.auth");
        let code = supervise(
            fake_gcloud("echo 'ERROR: [4033: not authorized]' >&2; exit 3"),
            None,
            None,
            None,
            Some(marker.clone()),
            None,
        )
        .await
        .expect("run");
        assert_eq!(code, 3);
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn a_marker_from_a_previous_run_is_cleared_when_the_supervisor_starts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("t.auth");
        std::fs::write(&marker, "1\n").expect("stale");
        let code = supervise(
            fake_gcloud("exit 0"),
            None,
            None,
            None,
            Some(marker.clone()),
            None,
        )
        .await
        .expect("run");
        // Nobody asked it to stop, so the clean exit is a restart, not a park
        assert_eq!(code, 1);
        assert!(!marker.exists());
    }

    // The #370 outage: the machine is offline, gcloud dies at once on every
    // start, and each run used to spend the budget until the job was parked
    // for good. Network exits wait and retry, however many there are.
    #[tokio::test]
    async fn ten_network_exits_in_a_row_do_not_park_the_job_and_the_wait_reaches_the_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.crash");
        let budget = tiny(2, Duration::from_secs(60));
        for n in 1..=10 {
            assert_eq!(
                run_once(
                    dir.path(),
                    "echo 'curl: (6) Could not resolve host: oauth2.googleapis.com' >&2; exit 1",
                    budget
                )
                .await,
                1,
                "exit {n} must still be one the service manager restarts"
            );
            assert!(!marker.exists(), "parked after network exit {n}");
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("com.gtunnel.dev.db.netfails"))
                .expect("count")
                .trim(),
            "10"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("com.gtunnel.dev.db.restarts"))
                .map_or(0, |t| t.lines().count()),
            0,
            "an outage spends none of the restart budget"
        );
        assert_eq!(
            crate::restart_budget::network_wait(&budget, &mut 9, Duration::ZERO, 1.0),
            budget.max_backoff
        );
    }

    const NET_LINE: &str = "echo 'curl: (6) Could not resolve host: oauth2.googleapis.com' >&2";

    // The newest classified line decides, as it does for an outage alone: a
    // network line after the busy one means the network went away again, and
    // the unbounded network wait is the right clock, not the 5 minute streak
    #[tokio::test]
    async fn a_network_line_after_a_busy_line_takes_the_network_wait() {
        let dir = tempfile::tempdir().expect("tempdir");
        let budget = tiny(2, Duration::from_secs(60));
        std::fs::write(
            crate::daemon::busy_beside(&dir.path().join("com.gtunnel.dev.db.crash")),
            "1000\n",
        )
        .expect("write");
        let script = format!("{BUSY_LINE}; {NET_LINE}; exit 255");
        run_once(dir.path(), &script, budget).await;
        assert!(dir.path().join("com.gtunnel.dev.db.netfails").exists());
        assert!(
            !dir.path().join("com.gtunnel.dev.db.busy").exists(),
            "the busy streak starts over"
        );
    }

    #[tokio::test]
    async fn a_busy_line_after_a_network_line_takes_the_busy_streak() {
        let dir = tempfile::tempdir().expect("tempdir");
        let budget = tiny(2, Duration::from_secs(60));
        let script = format!("{NET_LINE}; {BUSY_LINE}; exit 255");
        run_once(dir.path(), &script, budget).await;
        assert!(dir.path().join("com.gtunnel.dev.db.busy").exists());
        assert!(
            !dir.path().join("com.gtunnel.dev.db.netfails").exists(),
            "the network count is not touched"
        );
    }

    // No regression of #193: a login that expired is still the permanent kind,
    // even with the network line after it that an offline retry also prints
    #[tokio::test]
    async fn an_auth_failure_still_parks_after_one_failure_even_with_a_network_line_after_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("com.gtunnel.dev.db.auth");
        let code = supervise(
            fake_gcloud(
                "echo 'ERROR: Reauthentication required.' >&2; \
                 echo 'Could not resolve host: oauth2.googleapis.com' >&2; exit 1",
            ),
            None,
            None,
            None,
            Some(marker.clone()),
            Some(Spend {
                marker: dir.path().join("com.gtunnel.dev.db.crash"),
                budget: tiny(2, Duration::from_secs(60)),
            }),
        )
        .await
        .expect("run");
        let want = if cfg!(target_os = "macos") {
            0
        } else {
            crate::daemon::AUTH_EXPIRED_EXIT
        };
        assert_eq!(code, want);
        assert!(marker.exists(), "the auth marker names the login");
        assert!(
            !dir.path().join("com.gtunnel.dev.db.netfails").exists(),
            "a stop-class failure is not an outage"
        );
    }

    // A network line is not an alibi for a run the user stopped or for one
    // that succeeded: neither is an outage exit
    #[test]
    fn only_a_failed_run_that_nobody_asked_to_stop_is_an_outage_exit() {
        use crate::failure::Failure::{Network, Timeout};
        assert!(is_outage_exit(false, false, Some(Network)));
        assert!(is_outage_exit(false, false, Some(Timeout)));
        assert!(!is_outage_exit(true, false, Some(Network)), "clean exit");
        assert!(!is_outage_exit(false, true, Some(Network)), "SIGTERM");
        assert!(!is_outage_exit(false, false, None), "no network line");
    }
}
