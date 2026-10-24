use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

/// How much a tunnel says about itself.
///
/// One level governs every process gtunnel starts and every gcloud call it
/// makes. It exists because gcloud and ssh scale verbosity differently and
/// the same word means different things to each: `info` to gcloud is the
/// per-connection line, while `INFO` to ssh is three lines for a whole
/// session. Call sites ask this type for the flag rather than carrying a
/// string, so the two scales stay in step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Failures only. A working tunnel says nothing.
    Quiet,
    /// The forward coming up, and one line per client connecting.
    #[default]
    Info,
    /// The above plus gcloud's and ssh's own internals.
    Debug,
}

impl LogLevel {
    /// What `--verbosity` this level is to gcloud.
    pub fn gcloud_verbosity(&self) -> &'static str {
        match self {
            // Not `error`: a failed IAP handshake and an expiring credential
            // are warnings, and they are the whole reason to read a quiet
            // tunnel's log
            LogLevel::Quiet => "warning",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        }
    }

    /// What `-o LogLevel=` this level is to ssh.
    ///
    /// ssh's levels run `QUIET < FATAL < ERROR < INFO < VERBOSE < DEBUG`, and
    /// both lines a user means by "tell me it is working" - the forward being
    /// established, and one per client - are `debug()` calls. So `info` maps
    /// to `DEBUG`, one step further than the name suggests; at `VERBOSE` ssh
    /// prints three lines for a whole session and nothing per connection.
    pub fn ssh_log_level(&self) -> &'static str {
        match self {
            // `ERROR`, not `QUIET`: `QUIET` suppresses the failure messages
            // that are the only reason to open a quiet tunnel's log.
            //
            // `ERROR` is enough only because every tunnel gtunnel builds sets
            // `ExitOnForwardFailure=yes`, which is what turns a failed remote
            // forward from OpenSSH's `logit("Warning: ...")`, suppressed below
            // `INFO`, into `fatal("Error: ...")`, which survives here. Drop
            // that option and `quiet` starts hiding the one line that explains
            // why a public tunnel will not come up.
            LogLevel::Quiet => "ERROR",
            LogLevel::Info => "DEBUG",
            LogLevel::Debug => "DEBUG3",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LogLevel::Quiet => "quiet",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        })
    }
}

/// What a value that is not a level produces.
///
/// A named type rather than a string so clap can take `LogLevel` directly as
/// an argument type, which needs `FromStr::Err: std::error::Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLogLevelError(String);

impl fmt::Display for ParseLogLevelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a log level: use quiet, info or debug",
            self.0
        )
    }
}

impl std::error::Error for ParseLogLevelError {}

impl FromStr for LogLevel {
    type Err = ParseLogLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "quiet" => Ok(LogLevel::Quiet),
            "info" => Ok(LogLevel::Info),
            "debug" => Ok(LogLevel::Debug),
            _ => Err(ParseLogLevelError(s.to_string())),
        }
    }
}

/// The level this process resolved at startup.
///
/// A value rather than a parameter because the argv builders sit behind a
/// dozen call sites - `service_spec` alone has twelve - that have no business
/// knowing about verbosity. The builders themselves still take the level
/// explicitly, so a test can pin any level without touching this.
static CURRENT: OnceLock<LogLevel> = OnceLock::new();

/// Record the level `main` resolved. The first call wins; later ones are
/// ignored, so no code path can quietly change the level mid-run.
pub fn set_current(level: LogLevel) {
    let _ = CURRENT.set(level);
}

/// The level in force. `info` before `main` has resolved one, which is what
/// every test and every argv built outside the binary's startup path gets.
pub fn current() -> LogLevel {
    CURRENT.get().copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The default has to be a no-op: this work introduces the knob, not a
    // change of behaviour, so `info` must spell out today's two flags.
    #[test]
    fn the_default_level_is_the_flags_the_tunnels_ship_with_today() {
        let l = LogLevel::default();
        assert_eq!(l, LogLevel::Info);
        assert_eq!(l.gcloud_verbosity(), "info");
        assert_eq!(l.ssh_log_level(), "DEBUG");
    }

    #[test]
    fn every_level_maps_to_both_tools() {
        let table = [
            (LogLevel::Quiet, "warning", "ERROR"),
            (LogLevel::Info, "info", "DEBUG"),
            (LogLevel::Debug, "debug", "DEBUG3"),
        ];
        for (level, verbosity, ssh) in table {
            assert_eq!(level.gcloud_verbosity(), verbosity, "{level}");
            assert_eq!(level.ssh_log_level(), ssh, "{level}");
        }
    }

    #[test]
    fn a_level_is_read_whatever_its_case() {
        assert_eq!("INFO".parse::<LogLevel>().unwrap(), LogLevel::Info);
        assert_eq!("Debug".parse::<LogLevel>().unwrap(), LogLevel::Debug);
        assert_eq!("quiet".parse::<LogLevel>().unwrap(), LogLevel::Quiet);
        assert_eq!(" info ".parse::<LogLevel>().unwrap(), LogLevel::Info);
    }

    #[test]
    fn an_unknown_level_names_the_three_that_exist() {
        let err = "verbose".parse::<LogLevel>().unwrap_err().to_string();
        assert!(
            err.contains("quiet") && err.contains("info") && err.contains("debug"),
            "{err}"
        );
        assert!(
            err.contains("verbose"),
            "the error names what was typed: {err}"
        );
    }

    #[test]
    fn quiet_still_lets_ssh_report_a_failure() {
        assert_eq!(LogLevel::Quiet.ssh_log_level(), "ERROR");
    }

    // A level round-trips through the config file, so what Display writes
    // FromStr has to read back.
    #[test]
    fn a_level_round_trips_through_its_own_spelling() {
        for level in [LogLevel::Quiet, LogLevel::Info, LogLevel::Debug] {
            assert_eq!(level.to_string().parse::<LogLevel>().unwrap(), level);
        }
    }
}
