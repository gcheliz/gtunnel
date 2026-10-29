use crate::logfilter::{lock, Backend, Correlator};
use crate::restart_budget::{judge, Budget, History, Verdict};
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

    let began = std::time::Instant::now();
    let mut child = cmd
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn {prog}"))?;

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
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            res = child.wait() => res.context("failed to wait on child")?,
            _ = sigterm.recv() => {
                asked_to_stop = true;
                crate::proc::stop_group(&mut child, pgid, crate::proc::STOP_GRACE).await?
            }
            _ = tokio::signal::ctrl_c() => {
                asked_to_stop = true;
                crate::proc::stop_group(&mut child, pgid, crate::proc::STOP_GRACE).await?
            }
        }
    };

    #[cfg(not(unix))]
    let status = child.wait().await.context("failed to wait on child")?;

    let _ = tokio::join!(out_task, err_task);
    tick_task.abort();
    let mut err = std::io::stderr();
    for l in lock(&conn).finish() {
        let _ = err.write_all(l.as_bytes());
    }
    if let Some(file) = &ready_file {
        let _ = std::fs::remove_file(file);
    }
    if let Some((dir, port)) = &probes {
        crate::probe::unwatch_probes(dir, *port);
    }
    let expired = lock(&conn).credentials_expired(std::time::Instant::now());
    if stops_for_credentials(status.success(), expired, asked_to_stop) {
        if let Some(file) = &auth_marker {
            let _ = crate::fsutil::write_atomic(file, "1\n");
        }
        return Ok(crate::daemon::auth_expired_exit(cfg!(target_os = "macos")));
    }
    let code = status.code().unwrap_or(1);
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
            tokio::time::sleep(after).await;
        }
    }
    Ok(code)
}

/// Whether this exit counts against the budget. Credentials have their own
/// stop, a clean exit is not a failure, and a stop the user asked for says
/// nothing about the tunnel.
fn spends_budget(succeeded: bool, expired: bool, asked_to_stop: bool) -> bool {
    !(succeeded || expired || asked_to_stop)
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
        }
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
        assert_eq!(code, 0);
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
        // Gone before the child started, and back once it said it listens
        assert_eq!(code, 0);
        assert!(!ready.exists(), "removed when the supervisor exits");
        assert!(
            !dir.path().join("probes").join("18443").exists(),
            "the ledger is closed with the supervisor"
        );
    }

    fn fake_gcloud(script: &str) -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), script.to_string()]
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
        assert_eq!(code, 0);
        assert!(!marker.exists());
    }
}
