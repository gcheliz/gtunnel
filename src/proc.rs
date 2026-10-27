//! Every subprocess gtunnel waits on goes through here, so a hung one is an
//! error the caller can show instead of a task that never returns.
//!
//! A poller that awaits a `launchctl print` stuck behind a wedged launchd, or
//! a `gcloud` waiting on a browser login nobody will finish, holds its round
//! open for ever; with one round in flight per poller, that row of the TUI
//! then never updates again. The bound turns "never" into a message.

use anyhow::Result;
use std::fmt;
use std::time::Duration;

/// `gcloud` talks to the network and may refresh a token, so it gets the longer
/// bound.
pub const GCLOUD_BOUND: Duration = Duration::from_secs(30);

/// `launchctl` and `systemctl` answer from the local service manager; ten
/// seconds without a reply means it is wedged, not slow.
pub const SERVICE_BOUND: Duration = Duration::from_secs(10);

/// `ps`, `lsof`, `ss`, the clipboard and the opener answer from the local
/// machine in milliseconds; five seconds is wedged, and these run on the key
/// handler or inside a poller round that must not stall.
pub const LOCAL_TOOL_BOUND: Duration = Duration::from_secs(5);

/// A notifier that has not returned in five seconds is not going to show
/// anything useful; the next one in the chain gets its turn.
pub const NOTIFY_BOUND: Duration = Duration::from_secs(5);

const SERVICE_BOUND_ENV: &str = "GTUNNEL_SERVICE_BOUND_MS";

/// `SERVICE_BOUND`, or the milliseconds in `GTUNNEL_SERVICE_BOUND_MS`. The
/// override exists so a test of the timeout path does not wait ten seconds,
/// and so a slow machine can be diagnosed without a rebuild.
pub fn service_bound() -> Duration {
    std::env::var(SERVICE_BOUND_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(SERVICE_BOUND)
}

/// A typed error, not message text: `delete` must tell "the stop timed out,
/// the job may still be running" from "the job was not loaded", and a string
/// match on the former would break the first time the wording changed.
#[derive(Debug)]
pub struct Timeout {
    what: String,
    bound: Duration,
}

impl fmt::Display for Timeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} timed out after {}s", self.what, self.bound.as_secs())
    }
}

impl std::error::Error for Timeout {}

/// Whether `err`, however many contexts wrap it, is a `Timeout`.
pub fn is_timeout(err: &anyhow::Error) -> bool {
    err.downcast_ref::<Timeout>().is_some()
}

/// Run `cmd` to completion, or fail after `bound`. The child is killed when
/// the future is dropped, so a timeout does not leave the process behind.
pub async fn output_within(
    mut cmd: tokio::process::Command,
    bound: Duration,
    what: &str,
) -> Result<std::process::Output> {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(bound, cmd.output()).await {
        Ok(out) => Ok(out?),
        Err(_) => Err(Timeout {
            what: what.to_string(),
            bound,
        }
        .into()),
    }
}

/// Run `cmd` to completion on the calling thread, or fail after `bound`.
///
/// The sibling of `output_within` for the callers that are not async: the key
/// handler and the blocking pool. Both used to call `.output()`, so a wedged
/// `ps` or `pbcopy` froze a keypress or held a pool thread for ever.
pub fn output_within_blocking(
    cmd: std::process::Command,
    bound: Duration,
    what: &str,
) -> Result<std::process::Output> {
    output_within_blocking_with_input(cmd, None, bound, what)
}

/// As `output_within_blocking`, feeding `input` to the child's stdin.
///
/// Stdin, stdout and stderr are each served by a thread of their own. Waiting
/// first and reading after deadlocks on a child whose output fills the pipe
/// (`ps` on a busy machine does), and writing on the waiting thread would let a
/// clipboard tool that never reads hold the bound hostage. On expiry the child
/// is killed and reaped; the helper threads are left to finish on their own,
/// because a grandchild holding a pipe open must not hold the caller with it.
pub fn output_within_blocking_with_input(
    mut cmd: std::process::Command,
    input: Option<&[u8]>,
    bound: Duration,
    what: &str,
) -> Result<std::process::Output> {
    use std::io::{Read, Write};
    use std::process::Stdio;

    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let input = input.to_vec();
        // A write error is a child that stopped reading; its exit status says so
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = std::time::Instant::now() + bound;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Timeout {
                what: what.to_string(),
                bound,
            }
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // The child has exited, so its pipes close unless a grandchild kept them;
    // that case is cut off at the same bound instead of waiting for it
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    let left = left.max(Duration::from_millis(200));
    Ok(std::process::Output {
        status,
        stdout: stdout.recv_timeout(left).unwrap_or_default(),
        stderr: stderr.recv_timeout(left).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_subprocess_past_its_bound_is_an_error_naming_the_command() {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("5");
        let err = output_within(cmd, Duration::from_millis(100), "sleep 5")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("sleep 5 timed out"), "{err}");
        assert!(is_timeout(&err));
    }

    #[tokio::test]
    async fn a_subprocess_within_its_bound_returns_its_output() {
        let mut cmd = tokio::process::Command::new("echo");
        cmd.arg("hi");
        let out = output_within(cmd, Duration::from_secs(5), "echo")
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }

    #[tokio::test]
    async fn a_missing_program_is_not_mistaken_for_a_timeout() {
        let cmd = tokio::process::Command::new("gtunnel-no-such-program");
        let err = output_within(cmd, Duration::from_secs(5), "nothing")
            .await
            .unwrap_err();
        assert!(!is_timeout(&err), "{err}");
    }

    #[test]
    fn a_blocking_call_past_its_bound_is_a_timeout_and_leaves_no_child() {
        let dir = std::env::temp_dir().join(format!("gtunnel-proc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        // `exec` keeps the pid the shell reported: the sleep replaces the
        // shell instead of becoming a child that outlives the kill
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(format!("echo $$ > {}; exec sleep 30", pidfile.display()));
        let err = output_within_blocking(cmd, Duration::from_secs(1), "sleep 30").unwrap_err();
        assert!(is_timeout(&err), "{err}");
        let pid = std::fs::read_to_string(&pidfile).unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "pid {} survived its timeout", pid.trim());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_blocking_call_within_its_bound_returns_its_output() {
        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hi");
        let out = output_within_blocking(cmd, Duration::from_secs(5), "echo").unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }

    #[test]
    fn a_blocking_call_drains_output_larger_than_a_pipe_buffer() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("yes | head -c 400000");
        let out = output_within_blocking(cmd, Duration::from_secs(5), "yes").unwrap();
        assert_eq!(out.stdout.len(), 400_000);
    }

    #[test]
    fn a_blocking_call_feeds_its_input_to_the_child() {
        let cmd = std::process::Command::new("cat");
        let out =
            output_within_blocking_with_input(cmd, Some(b"fed"), Duration::from_secs(5), "cat")
                .unwrap();
        assert_eq!(out.stdout, b"fed");
    }

    #[test]
    fn the_timeout_message_names_the_bound_in_whole_seconds() {
        let err: anyhow::Error = Timeout {
            what: "launchctl print".to_string(),
            bound: SERVICE_BOUND,
        }
        .into();
        assert_eq!(err.to_string(), "launchctl print timed out after 10s");
    }
}
