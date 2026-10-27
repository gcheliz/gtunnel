use crate::logfilter::{lock, Backend, Correlator};
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

/// Run a child process, pumping its stdout and stderr through the log filter, then exit with its status.
pub async fn run(
    argv: Vec<String>,
    port: Option<u16>,
    ready_file: Option<PathBuf>,
    conns: Option<Backend>,
    auth_marker: Option<PathBuf>,
) -> Result<()> {
    let code = supervise(argv, port, ready_file, conns, auth_marker).await?;
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
    // A ready file left by a previous run would say the listener is up before
    // this child has bound anything
    if let Some(file) = &ready_file {
        let _ = std::fs::remove_file(file);
    }
    let mut conn = Correlator::new(crate::loglevel::current());
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
    let expired = lock(&conn).credentials_expired();
    if expired && !asked_to_stop && !status.success() {
        if let Some(file) = &auth_marker {
            let _ = crate::fsutil::write_atomic(file, "1\n");
        }
        return Ok(crate::daemon::auth_expired_exit(cfg!(target_os = "macos")));
    }
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_child_and_returns_exit_code() {
        let code = supervise(
            vec!["echo".to_string(), "hello".to_string()],
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
        let code = supervise(vec!["false".to_string()], None, None, None, None)
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
        )
        .await
        .expect("run");
        assert_eq!(code, 0);
        assert!(!marker.exists());
    }
}
