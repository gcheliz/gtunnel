use crate::logfilter::{lock, Backend, Correlator};
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

/// How long a stopping child gets between SIGTERM and SIGKILL. It has to stay
/// under launchd's ExitTimeOut (20 s by default) and systemd's TimeoutStopSec
/// (90 s), or the init system kills the supervisor first and the SIGKILL below
/// never reaches the group. A plain const: no caller needs to tune it.
#[cfg(unix)]
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Run a child process, pumping its stdout and stderr through the log filter, then exit with its status.
pub async fn run(
    argv: Vec<String>,
    port: Option<u16>,
    ready_file: Option<PathBuf>,
    conns: Option<Backend>,
) -> Result<()> {
    let status = run_inner(argv, port, ready_file, conns).await?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Stop the child the way a terminal would: SIGTERM to its whole group, a
/// bounded wait, then SIGKILL to the whole group. gcloud closes its IAP
/// websocket and releases the port in its SIGTERM handler, and a SIGKILL sent
/// at once leaves that to the server's idle timeout. The group is signalled,
/// not just the child, because gcloud is a launcher with children of its own.
///
/// Takes the child and its group, not the signal, so a test can stop a child
/// of its own without signalling the test process.
#[cfg(unix)]
async fn stop_child(
    child: &mut tokio::process::Child,
    pgid: Option<i32>,
    grace: Duration,
) -> Result<std::process::ExitStatus> {
    if let Some(pgid) = pgid {
        // SAFETY: pgid is a valid process group id spawned above.
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(status) => status.context("failed to wait on child after a stop signal"),
        Err(_) => {
            if let Some(pgid) = pgid {
                // SAFETY: the child is not yet reaped, so its group id is still ours.
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
            }
            let _ = child.kill().await;
            child
                .wait()
                .await
                .context("failed to wait on child after SIGKILL")
        }
    }
}

/// Run child process and return its ExitStatus once streams have drained.
pub async fn run_inner(
    argv: Vec<String>,
    port: Option<u16>,
    ready_file: Option<PathBuf>,
    conns: Option<Backend>,
) -> Result<std::process::ExitStatus> {
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

    #[cfg(unix)]
    let status = {
        let pgid = child.id().map(|id| id as i32);
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            res = child.wait() => res.context("failed to wait on child")?,
            _ = sigterm.recv() => stop_child(&mut child, pgid, STOP_GRACE).await?,
            _ = tokio::signal::ctrl_c() => stop_child(&mut child, pgid, STOP_GRACE).await?,
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
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_child_and_returns_exit_code() {
        let status = run_inner(
            vec!["echo".to_string(), "hello".to_string()],
            None,
            None,
            None,
        )
        .await
        .expect("run echo");
        assert!(status.success());
    }

    #[tokio::test]
    async fn returns_non_zero_exit_code_on_child_failure() {
        let status = run_inner(vec!["false".to_string()], None, None, None)
            .await
            .expect("run false");
        assert!(!status.success());
    }

    // The whole path: a child that prints what gcloud prints, a ready file the
    // supervisor owns, and a probe's close that must not reach the log.
    #[tokio::test]
    async fn the_ready_file_is_cleared_at_start_written_on_listening_and_removed_at_exit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("com.gtunnel.dev.db.ready");
        std::fs::write(&ready, "1\n").expect("stale");
        let status = run_inner(
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
        )
        .await
        .expect("run");
        // Gone before the child started, and back once it said it listens
        assert!(status.success());
        assert!(!ready.exists(), "removed when the supervisor exits");
        assert!(
            !dir.path().join("probes").join("18443").exists(),
            "the ledger is closed with the supervisor"
        );
    }

    #[cfg(unix)]
    mod stopping {
        use super::super::*;
        use std::os::unix::process::ExitStatusExt;

        const SHORT: Duration = Duration::from_millis(600);

        fn spawn_sh(script: &str) -> (tokio::process::Child, Option<i32>) {
            let mut c = Command::new("sh");
            c.args(["-c", script]).process_group(0);
            let child = c.spawn().expect("spawn sh");
            let pgid = child.id().map(|id| id as i32);
            (child, pgid)
        }

        #[tokio::test]
        async fn a_child_that_exits_on_sigterm_is_not_sigkilled() {
            let (mut child, pgid) = spawn_sh(r#"trap "sleep 0.3; exit 0" TERM; sleep 30 & wait"#);
            // Let sh install its trap before the signal arrives
            tokio::time::sleep(Duration::from_millis(300)).await;
            let status = stop_child(&mut child, pgid, SHORT).await.expect("stop");
            assert_eq!(
                status.signal(),
                None,
                "a clean exit, not a kill: {status:?}"
            );
            assert_eq!(status.code(), Some(0));
        }

        #[tokio::test]
        async fn a_child_that_ignores_sigterm_is_killed_after_the_grace() {
            let (mut child, pgid) = spawn_sh(r#"trap "" TERM; while :; do sleep 1; done"#);
            tokio::time::sleep(Duration::from_millis(300)).await;
            let started = std::time::Instant::now();
            let status = stop_child(&mut child, pgid, SHORT).await.expect("stop");
            let took = started.elapsed();
            assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
            assert!(took >= SHORT, "waited out the grace first: {took:?}");
            assert!(took < SHORT * 4, "and did not hang: {took:?}");
        }
    }
}
