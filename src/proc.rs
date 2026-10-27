//! Every subprocess gtunnel waits on goes through here, so a hung one is an
//! error the caller can show instead of a task that never returns. This is
//! also where a long-lived child is stopped and identified, so the supervisor
//! and `run` end gcloud the same way.
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

/// How long a stopping child gets between SIGTERM and SIGKILL. It has to stay
/// under launchd's ExitTimeOut (20 s by default) and systemd's TimeoutStopSec
/// (90 s), or the init system kills the supervisor first and the SIGKILL below
/// never reaches the group. A plain const: no caller needs to tune it.
#[cfg(unix)]
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// Send `signal` to every member of process group `pgid`. The one
/// `kill(-pgid, sig)` site, so the guard against a non-positive id (which
/// would signal every process of the user, or our own group) lives once.
/// ESRCH is ignored: a group that is gone is the outcome being asked for.
#[cfg(unix)]
pub fn signal_group(pgid: i32, signal: libc::c_int) {
    if pgid <= 0 {
        return;
    }
    // SAFETY: pgid is positive, so -pgid names one group and never "every
    // process"; the call reads no memory of ours.
    unsafe { libc::kill(-pgid, signal) };
}

#[cfg(unix)]
fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; pgid is checked positive by the caller.
    pgid > 0 && unsafe { libc::kill(-pgid, 0) } == 0
}

/// Stop the child the way a terminal would: SIGTERM to its whole group, a
/// bounded wait, then SIGKILL to the whole group. gcloud closes its IAP
/// websocket and releases the port in its SIGTERM handler, and a SIGKILL sent
/// at once leaves that to the server's idle timeout. The group is signalled,
/// not just the child, because gcloud is a launcher with children of its own.
///
/// A leader that exits on TERM does not mean the group is gone: ssh may ignore
/// it and keep the port. A reaped leader leaves its id with the surviving
/// members, so `-pgid` still names only this group and the sweep can finish
/// the job within the same grace.
///
/// Takes the child and its group, not the signal, so a test can stop a child
/// of its own without signalling the test process. Correct after the caller's
/// own `child.wait()` already reaped the child: the status is cached.
#[cfg(unix)]
pub async fn stop_group(
    child: &mut tokio::process::Child,
    pgid: Option<i32>,
    grace: Duration,
) -> Result<std::process::ExitStatus> {
    use anyhow::Context;
    let deadline = std::time::Instant::now() + grace;
    if let Some(pgid) = pgid {
        signal_group(pgid, libc::SIGTERM);
    }
    let status = match tokio::time::timeout(grace, child.wait()).await {
        Ok(status) => status.context("failed to wait on child after a stop signal"),
        Err(_) => {
            if let Some(pgid) = pgid {
                signal_group(pgid, libc::SIGKILL);
            }
            let _ = child.kill().await;
            child
                .wait()
                .await
                .context("failed to wait on child after SIGKILL")
        }
    };
    if let Some(pgid) = pgid {
        while group_alive(pgid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if group_alive(pgid) {
            signal_group(pgid, libc::SIGKILL);
        }
    }
    status
}

/// Who `pid` is, as an opaque string that changes when the kernel hands the
/// same number to another process. A pid alone is reused after a reboot or a
/// wrap; pid and start together name one process. `None` when the pid is not
/// signalable, the process is gone, or this platform cannot say. Callers on
/// macOS and Linux treat `None` for a process they just spawned as "it already
/// exited".
///
/// Neither source is the wall clock: macOS stamps the start at fork and Linux
/// counts ticks since boot, qualified by the boot id. A clock step must not
/// make a live run read as dead, which `/proc/stat` `btime` would do.
pub fn start_token(pid: u32) -> Option<String> {
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    start_token_of(pid)
}

#[cfg(target_os = "macos")]
fn start_token_of(pid: i32) -> Option<String> {
    use std::mem::{size_of, MaybeUninit};
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is a zeroed proc_bsdinfo and the size passed is its
    // own, so the kernel cannot write past it; the pid is positive.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    // SAFETY: the call filled the whole struct (got == size), and zeroed is a
    // valid value for it besides.
    let info = unsafe { info.assume_init() };
    Some(format!(
        "{}.{:06}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(target_os = "linux")]
fn start_token_of(pid: i32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = linux_start_ticks(&stat)?;
    match std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        Ok(boot) => Some(format!("{}/{ticks}", boot.trim())),
        Err(_) => Some(ticks.to_string()),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn start_token_of(_pid: i32) -> Option<String> {
    None
}

/// Field 22 of `/proc/<pid>/stat`, the start time in ticks since boot. The
/// command name is field 2, in parentheses, and may itself hold `) `, so the
/// fields are counted from the last `)`. Compiled everywhere so the parser is
/// tested on the machine the author has.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_start_ticks(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    // The text starts at field 3, so field 22 is index 19
    after.split_whitespace().nth(19)?.parse().ok()
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

    #[test]
    fn a_live_process_has_a_stable_start_token() {
        let a = start_token(std::process::id()).expect("our own start");
        assert_eq!(start_token(std::process::id()), Some(a));
    }

    #[test]
    fn a_missing_or_group_pid_has_no_start_token() {
        assert_eq!(start_token(999_999), None);
        assert_eq!(start_token(0), None);
        assert_eq!(start_token(u32::MAX), None);
    }

    // The command name is in parentheses and may itself hold ") ", so the fields
    // are counted from the last ')'
    #[test]
    fn the_start_ticks_are_field_22_even_when_the_command_name_holds_a_parenthesis() {
        let stat = "4242 (a) b) S 1 4242 4242 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 987654 1 2 3";
        assert_eq!(linux_start_ticks(stat), Some(987_654));
        assert_eq!(linux_start_ticks("4242 (sh) S 1"), None);
    }

    #[test]
    fn a_macos_start_token_is_seconds_and_six_digit_microseconds() {
        if cfg!(target_os = "macos") {
            let token = start_token(std::process::id()).unwrap();
            let (secs, micros) = token.split_once('.').expect("sec.usec");
            assert!(secs.parse::<u64>().unwrap() > 1_000_000_000, "{token}");
            assert_eq!(micros.chars().count(), 6, "{token}");
        }
    }

    #[cfg(unix)]
    mod stopping {
        use super::super::*;
        use std::os::unix::process::ExitStatusExt;
        use tokio::process::Command;

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
            let dir = tempfile::tempdir().unwrap();
            let ready = dir.path().join("ready");
            let (mut child, pgid) = spawn_sh(&format!(
                r#"trap "sleep 0.1; exit 0" TERM; : > {}; sleep 30 & wait"#,
                ready.display()
            ));
            // The file appears only once the trap is installed; a fixed sleep
            // lost that race on a loaded machine
            for _ in 0..100 {
                if ready.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(ready.exists(), "sh never installed its trap");
            let status = stop_group(&mut child, pgid, SHORT).await.expect("stop");
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
            let status = stop_group(&mut child, pgid, SHORT).await.expect("stop");
            let took = started.elapsed();
            assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
            assert!(took >= SHORT, "waited out the grace first: {took:?}");
            assert!(took < SHORT * 4, "and did not hang: {took:?}");
        }

        #[tokio::test]
        async fn a_child_that_outlives_its_leader_is_killed_by_the_sweep() {
            // The leader exits on TERM at once; its child ignores TERM
            let dir = tempfile::tempdir().unwrap();
            let pidfile = dir.path().join("child");
            let (mut child, pgid) = spawn_sh(&format!(
                "(trap '' TERM; exec sleep 30) & echo $! > {}; wait",
                pidfile.display()
            ));
            // The shell writes the pid once its subshell has forked; wait for it
            let mut grandchild = 0;
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(&pidfile)
                    && let Ok(pid) = text.trim().parse()
                {
                    grandchild = pid;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(grandchild > 0, "the grandchild never reported its pid");
            let started = std::time::Instant::now();
            stop_group(&mut child, pgid, SHORT).await.expect("stop");
            assert!(
                started.elapsed() < SHORT * 4,
                "bounded: {:?}",
                started.elapsed()
            );
            // Reparented to init and reaped there, so poll, bounded
            let mut gone = false;
            for _ in 0..40 {
                // SAFETY: signal 0 sends nothing; the pid is positive
                if unsafe { libc::kill(grandchild, 0) } != 0 {
                    gone = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(gone, "pid {grandchild} survived the group stop");
        }
    }
}
