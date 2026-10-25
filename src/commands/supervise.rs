use anyhow::{bail, Context, Result};
use tokio::process::Command;

/// Run a child process, pumping its stdout and stderr through the log filter, then exit with its status.
pub async fn run(argv: Vec<String>) -> Result<()> {
    let status = run_inner(argv).await?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Run child process and return its ExitStatus once streams have drained.
pub async fn run_inner(argv: Vec<String>) -> Result<std::process::ExitStatus> {
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

    let mut child = cmd
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn {prog}"))?;

    let stdout = child.stdout.take().context("child stdout missing")?;
    let stderr = child.stderr.take().context("child stderr missing")?;

    let out_task = tokio::spawn(async move {
        let reader = tokio::io::BufReader::new(stdout);
        let mut out = std::io::stdout();
        crate::logfilter::pump(reader, &mut out).await
    });

    let err_task = tokio::spawn(async move {
        let reader = tokio::io::BufReader::new(stderr);
        let mut out = std::io::stderr();
        crate::logfilter::pump(reader, &mut out).await
    });

    #[cfg(unix)]
    let status = {
        let pgid = child.id().map(|id| id as i32);
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            res = child.wait() => res.context("failed to wait on child")?,
            _ = sigterm.recv() => {
                if let Some(pgid) = pgid {
                    // SAFETY: pgid is a valid process group id spawned above.
                    unsafe { libc::kill(-pgid, libc::SIGTERM) };
                }
                let _ = child.kill().await;
                child.wait().await.context("failed to wait on child after SIGTERM")?
            }
            _ = tokio::signal::ctrl_c() => {
                if let Some(pgid) = pgid {
                    // SAFETY: pgid is a valid process group id spawned above.
                    unsafe { libc::kill(-pgid, libc::SIGTERM) };
                }
                let _ = child.kill().await;
                child.wait().await.context("failed to wait on child after SIGINT")?
            }
        }
    };

    #[cfg(not(unix))]
    let status = child.wait().await.context("failed to wait on child")?;

    let _ = tokio::join!(out_task, err_task);
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_child_and_returns_exit_code() {
        let status = run_inner(vec!["echo".to_string(), "hello".to_string()])
            .await
            .expect("run echo");
        assert!(status.success());
    }

    #[tokio::test]
    async fn returns_non_zero_exit_code_on_child_failure() {
        let status = run_inner(vec!["false".to_string()])
            .await
            .expect("run false");
        assert!(!status.success());
    }
}
