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
    fn the_timeout_message_names_the_bound_in_whole_seconds() {
        let err: anyhow::Error = Timeout {
            what: "launchctl print".to_string(),
            bound: SERVICE_BOUND,
        }
        .into();
        assert_eq!(err.to_string(), "launchctl print timed out after 10s");
    }
}
