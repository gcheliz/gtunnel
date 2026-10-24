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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
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

/// Read through `FromStr` rather than by derive, so `config.toml` accepts the
/// same spellings the flag and the environment do.
///
/// A derived `Deserialize` matches the variant names exactly, which made
/// `log_level = "Info"` a parse error - and because it is the whole file that
/// fails to parse, every command died, including the `--log-level` that was
/// supposed to be the way out of a bad value in the file.
impl<'de> Deserialize<'de> for LogLevel {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// The level this process resolved at startup.
///
/// A value rather than a parameter because the argv builders sit behind a
/// dozen call sites - `service_spec` alone has twelve - that have no business
/// knowing about verbosity. The builders themselves still take the level
/// explicitly, so a test can pin any level without touching this.
static CURRENT: OnceLock<LogLevel> = OnceLock::new();

/// What the `--log-level` flag or `GTUNNEL_LOG_LEVEL` forced, if either did.
///
/// Held apart from [`CURRENT`] because the two answer different questions.
/// `CURRENT` is a snapshot of startup, which is the whole life of a CLI
/// invocation but not of a TUI session; this is the part of that snapshot
/// that stays true for a process that runs for hours, because a flag and an
/// environment variable cannot change under it while a config file can.
static OVERRIDE: OnceLock<Option<LogLevel>> = OnceLock::new();

/// Record what `main` resolved, and which part of it was an override.
/// The first call wins; later ones are ignored, so no code path can quietly
/// change the level mid-run.
pub fn set_current(level: LogLevel, from_flag_or_env: Option<LogLevel>) {
    let _ = CURRENT.set(level);
    let _ = OVERRIDE.set(from_flag_or_env);
}

/// The level in force for work whose output this process captures and
/// discards - a `gcloud auth print-access-token`, a foreground `gtunnel run`.
/// `info` before `main` has resolved one, which is what every test and every
/// argv built outside the binary's startup path gets.
pub fn current() -> LogLevel {
    CURRENT.get().copied().unwrap_or_default()
}

/// The level for an argv that is about to be written into a service file.
///
/// Takes the level from the `Config` in hand instead of the startup snapshot,
/// because a service file outlives the command that wrote it and the TUI
/// rewrites one hours into a session - by which time `config.toml` may say
/// something the snapshot does not. An override still wins, so
/// `--log-level debug` reaches an installed tunnel exactly as before.
pub fn effective(from_config: LogLevel) -> LogLevel {
    resolve_effective(OVERRIDE.get().copied().flatten(), from_config)
}

/// The rule [`effective`] applies, without the process-wide slot, so the
/// precedence is testable in parallel with everything else.
pub fn resolve_effective(over: Option<LogLevel>, from_config: LogLevel) -> LogLevel {
    over.unwrap_or(from_config)
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

    // The flag and the environment both lower-case what they are given, so a
    // config file that did not would make the same word valid in two places
    // and fatal in the third - and fatal for every command, not just the ones
    // that care about logging, because the file fails to parse at all.
    #[test]
    fn a_level_in_a_config_file_is_read_whatever_its_case() {
        #[derive(Deserialize)]
        struct Holder {
            log_level: LogLevel,
        }
        for spelling in ["\"Info\"", "\"INFO\"", "\"info\"", "\" Info \""] {
            let h: Holder = toml::from_str(&format!("log_level = {spelling}"))
                .unwrap_or_else(|e| panic!("{spelling} should parse: {e}"));
            assert_eq!(h.log_level, LogLevel::Info, "{spelling}");
        }
    }

    // One bad value, one message, whichever of the three sources carried it
    #[test]
    fn a_bad_level_in_a_config_file_names_the_three_that_exist() {
        #[derive(Debug, Deserialize)]
        struct Holder {
            #[allow(dead_code)]
            log_level: LogLevel,
        }
        let err = toml::from_str::<Holder>("log_level = \"verbose\"")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("quiet") && err.contains("info") && err.contains("debug"),
            "{err}"
        );
        assert!(
            err.contains("verbose"),
            "the error names what was typed: {err}"
        );
    }

    // The file is re-read whenever a service is installed, so an edit on the
    // TUI's config page has to take effect without relaunching gtunnel.
    #[test]
    fn the_file_decides_when_neither_the_flag_nor_the_environment_did() {
        assert_eq!(resolve_effective(None, LogLevel::Debug), LogLevel::Debug);
        assert_eq!(resolve_effective(None, LogLevel::Quiet), LogLevel::Quiet);
    }

    #[test]
    fn an_override_beats_the_file_every_time_the_file_is_read() {
        assert_eq!(
            resolve_effective(Some(LogLevel::Quiet), LogLevel::Debug),
            LogLevel::Quiet
        );
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
