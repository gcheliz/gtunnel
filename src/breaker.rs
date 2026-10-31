//! When a wedged tunnel is worth restarting, and when restarting it is
//! making things worse.
//!
//! The decision lives here and nothing else does: no clock, no process, no
//! file. `now` is a parameter, so every row of the plan's table is a test
//! that runs in microseconds instead of in minutes of real waiting. The
//! watchdog in `commands::watch` supplies the observations and carries out
//! the actions.

use std::time::{Duration, SystemTime};

/// Restarts a tunnel is allowed before the breaker concludes that restarting
/// is not the fix.
pub const MAX_ATTEMPTS: u8 = 4;

/// The ceiling of the first wait. Each attempt doubles it: 10, 20, 40, 80
/// seconds, and the wait is drawn from zero up to that.
pub const BASE_BACKOFF: Duration = Duration::from_secs(10);

/// The last wait, which the ceiling stops at. With [`MAX_ATTEMPTS`] of 4 the
/// doubling never reaches it; it is the cap `backoff` asks for, not a second
/// limit.
const MAX_BACKOFF: Duration = Duration::from_secs(10 * (1 << (MAX_ATTEMPTS as u64 - 1)));

/// How many observations in a row it takes to change the breaker's mind, in
/// either direction. One transient refusal during a restart must not trip it,
/// and one lucky probe right after a restart must not close it.
const RUN: u8 = 2;

/// What a single check of one tunnel saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    Pass,
    Failed(Reason),
    /// The service is not running, so there is nothing to judge. A person who
    /// ran `gtunnel stop` outranks the breaker.
    NotRunning,
    /// The check could not tell. The network is down, the service manager
    /// timed out, or the tunnel was started a moment ago and is not listening
    /// yet. None of those is a verdict on the tunnel, so the breaker must not
    /// count it as a failure and must not read it as a pass either.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Running, and refusing connections on its own local port.
    ProbeRefused,
    /// The log gained a `Too many open files` line since the last check.
    DescriptorsExhausted,
}

/// What the watchdog should do about it. Everything that touches a process
/// lives on this side of the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Restart,
    /// Stop the tunnel and say why, naming the process holding its
    /// connections. Restarting it again would only bury that message.
    StopAndReport(Reason),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    #[default]
    Closed,
    /// Tripped, waiting for `next_attempt` to come round.
    Open,
    /// Just restarted, and not yet believed.
    HalfOpen,
    /// Out of attempts. Only a person starting the tunnel clears this.
    Stopped,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Breaker {
    state: State,
    failures: u8,
    passes: u8,
    attempt: u8,
    next_attempt: Option<SystemTime>,
    /// When the failure count last grew. Without it a failure stored before
    /// the laptop slept is still counted after it wakes, and pairs with the
    /// first refused probe of the new session to trip a healthy tunnel.
    /// Absent in files written before it existed, which load as `None`.
    last_failure: Option<SystemTime>,
}

impl Breaker {
    /// [`Breaker::observe_with`] with the wait at its ceiling, so a schedule
    /// can be read as the plain doubling. The watchdog draws; tests do not.
    #[cfg(test)]
    pub fn observe(&mut self, obs: Observation, now: SystemTime, stale_after: Duration) -> Action {
        self.observe_with(obs, now, stale_after, 1.0)
    }

    /// Feed it one check and learn what to do. `now` is a parameter so the
    /// whole schedule is testable without waiting for it.
    ///
    /// `stale_after` is how old a counted failure may be and still pair with
    /// a new one. The watchdog passes twice its pass interval: a run of two
    /// is two consecutive passes, and anything older is a gap, not a run.
    pub fn observe_with(
        &mut self,
        obs: Observation,
        now: SystemTime,
        stale_after: Duration,
        unit: f64,
    ) -> Action {
        // Before anything else: a tunnel that is not running is not a tunnel
        // with a problem. This is also how `Stopped` normally ends - the
        // breaker the watchdog just stopped sees `NotRunning` on its next
        // pass and goes quiet, clean for whenever a person starts it again.
        if obs == Observation::NotRunning {
            *self = Self::default();
            return Action::Nothing;
        }
        // Changes nothing, not even to forget a stale failure: what the
        // pass could not see is not evidence of anything
        if obs == Observation::Unknown {
            return Action::Nothing;
        }
        // `Stopped` is written when the watchdog stops a tunnel, and the
        // `NotRunning` above is how it normally ends. A person who starts the
        // tunnel again before the next pass skips that step, and a tunnel
        // found running is the proof: no one but a person brings it back. The
        // breaker starts over rather than staying deaf to a tunnel that was
        // never watched again.
        if self.state == State::Stopped {
            *self = Self::default();
        }

        match obs {
            Observation::Pass => self.pass(),
            Observation::Failed(reason) => self.failure(reason, now, stale_after, unit),
            Observation::NotRunning | Observation::Unknown => unreachable!("handled above"),
        }
    }

    fn pass(&mut self) -> Action {
        self.failures = 0;
        self.last_failure = None;
        if self.state == State::Closed {
            return Action::Nothing;
        }
        // Open and HalfOpen both close on a run of passes. A tunnel that
        // recovered on its own while the breaker was waiting to restart it
        // does not need restarting.
        self.passes += 1;
        if self.passes >= RUN {
            *self = Self::default();
        }
        Action::Nothing
    }

    fn failure(
        &mut self,
        reason: Reason,
        now: SystemTime,
        stale_after: Duration,
        unit: f64,
    ) -> Action {
        self.passes = 0;
        match self.state {
            State::Closed => {
                // A clock that stepped backwards reads as fresh, not stale:
                // forgetting a failure is the lenient mistake, but only an
                // elapsed time that is known to be long may make it
                if self
                    .last_failure
                    .and_then(|then| now.duration_since(then).ok())
                    .is_some_and(|age| age > stale_after)
                {
                    self.failures = 0;
                }
                self.failures += 1;
                self.last_failure = Some(now);
                if self.failures >= RUN {
                    self.trip(1, now, unit);
                }
                Action::Nothing
            }
            // Waiting. The restart happens on the first check at or after the
            // scheduled moment, not on a clock of the breaker's own.
            State::Open => {
                if self
                    .next_attempt
                    .is_some_and(|due| still_waiting(due, now, backoff(self.attempt)))
                {
                    return Action::Nothing;
                }
                self.state = State::HalfOpen;
                Action::Restart
            }
            // The restart did not take.
            State::HalfOpen => {
                if self.attempt >= MAX_ATTEMPTS {
                    *self = Self {
                        state: State::Stopped,
                        ..Self::default()
                    };
                    return Action::StopAndReport(reason);
                }
                self.trip(self.attempt + 1, now, unit);
                Action::Nothing
            }
            State::Stopped => Action::Nothing,
        }
    }

    // Open, with the next restart due within one backoff from now. The moment
    // is drawn once, here, and stored: `still_waiting` reads it back on every
    // later pass, so a redraw per pass would never settle.
    fn trip(&mut self, attempt: u8, now: SystemTime, unit: f64) {
        self.state = State::Open;
        self.failures = 0;
        self.attempt = attempt;
        self.next_attempt = Some(
            now + crate::daemon::backoff::backoff_at(
                attempt.into(),
                BASE_BACKOFF,
                MAX_BACKOFF,
                unit,
            ),
        );
    }
}

/// Whether a scheduled restart is still ahead, read by a clock that may have
/// stepped between passes.
///
/// `due` was written on an earlier pass, so the two times can come from
/// different clocks: an NTP correction or a laptop resuming with a bad clock
/// moves `now` backwards, and a plain `now < due` then stays true for the
/// whole length of the skew - the wedged tunnel that tripped the breaker
/// waits out a clock error rather than being restarted. This breaker never
/// schedules anything further out than one backoff, so a `due` beyond that is
/// a stepped clock and the wait is over.
/// The restart happens on the first check *at or after* the scheduled moment,
/// so a remaining of zero is due, not waiting.
fn still_waiting(due: SystemTime, now: SystemTime, backoff: Duration) -> bool {
    due.duration_since(now)
        .is_ok_and(|remaining| !remaining.is_zero() && remaining <= backoff)
}

/// The longest the wait for `attempt` can be, which is what `still_waiting`
/// bounds a stored moment by: the draw only ever shortens it.
fn backoff(attempt: u8) -> Duration {
    crate::daemon::backoff::ceiling(attempt.into(), BASE_BACKOFF, MAX_BACKOFF)
}

/// Where a breaker's memory lives between runs. A thirty-second agent is a
/// fresh process every time, so a state machine with no file has no memory
/// at all.
pub mod persist {
    use super::Breaker;
    use anyhow::{Context, Result};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// One tunnel's row: its breaker, and how long its log was when the
    /// watchdog last looked.
    ///
    /// The log length is what makes "the log *gained* a `Too many open
    /// files` line since the last check" answerable at all. Without it a
    /// line from last week reads as news on every pass, and the breaker
    /// would trip on a tunnel that has been healthy for days.
    #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(default)]
    pub struct Row {
        pub breaker: Breaker,
        pub log_len: u64,
        /// Set before the watchdog stops a tunnel to restart it and cleared
        /// after the start, holding the moment the decision was made.
        ///
        /// A restart is two service-manager calls, and a pass that dies
        /// between them (logout, `watch uninstall`, a kill) leaves the tunnel
        /// down with nothing but this to say the watchdog did it. The next
        /// pass reads it and starts the tunnel, instead of treating a stopped
        /// tunnel as a person's choice. Absent when nothing is in flight, so
        /// files written before it existed load as not restarting.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub restarting: Option<std::time::SystemTime>,
    }

    /// Every row, keyed `"<account>.<name>"`. Ordered, so the file does not
    /// churn between saves that changed nothing.
    pub type BreakerFile = BTreeMap<String, Row>;

    pub fn path() -> Result<PathBuf> {
        Ok(crate::config::config_dir()?.join("breakers.toml"))
    }

    /// Every breaker as it was left. A file that is not there is every
    /// breaker closed, which is what a first run should see; a file that is
    /// there and unreadable is an error, because silently starting over
    /// would restart tunnels a previous run had deliberately stopped.
    ///
    /// Only *not there* is a fresh start. A file we were refused, or that
    /// the disk could not give us, might be the one saying a tunnel was
    /// stopped on purpose, and reading that as "every breaker closed" is
    /// the single wrong answer.
    pub fn load() -> Result<BreakerFile> {
        let path = path()?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BreakerFile::new()),
            Err(err) => {
                return Err(anyhow::Error::new(err))
                    .with_context(|| format!("Failed to read {}", path.display()))
            }
        };
        toml::from_str(&text).with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Move an unusable file out of the way and report where it went.
    ///
    /// [`load`] refusing to guess is right, but a refusal repeated every
    /// thirty seconds is the watchdog going silent, which is worse than the
    /// watchdog forgetting. The caller recovers by calling this, saying so,
    /// and starting from closed. The file stays on disk as evidence.
    pub fn set_aside() -> Result<PathBuf> {
        let path = path()?;
        let moved = path.with_extension("toml.bad");
        std::fs::rename(&path, &moved)
            .with_context(|| format!("Failed to move {} to {}", path.display(), moved.display()))?;
        Ok(moved)
    }

    /// Writes one tunnel's row now, ahead of the pass's own save at its end.
    ///
    /// The `restarting` entry has to be on disk before the stop it guards, or
    /// a pass that dies there leaves no trace. Every other row is kept as the
    /// file has it; the pass holds the lock from its read to its write, so
    /// this read-modify-write cannot lose another pass's decision.
    pub fn checkpoint(key: &str, row: &Row) -> Result<()> {
        let mut breakers = load()?;
        breakers.insert(key.to_string(), row.clone());
        let live: Vec<String> = breakers.keys().cloned().collect();
        save(&breakers, &live)
    }

    /// Writes the breakers of tunnels that still exist, and only those: a
    /// deleted tunnel's row would otherwise sit in the file forever.
    ///
    /// The write is atomic: a save that fails partway leaves the previous
    /// file whole rather than an empty one that reads as every breaker closed.
    pub fn save(breakers: &BreakerFile, live: &[String]) -> Result<()> {
        let path = path()?;
        let dir = path.parent().context("breakers.toml has no directory")?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;

        let kept: BreakerFile = breakers
            .iter()
            .filter(|(key, _)| live.iter().any(|k| k == *key))
            .map(|(key, row)| (key.clone(), row.clone()))
            .collect();
        let text = toml::to_string_pretty(&kept).context("Failed to serialize the breakers")?;

        crate::fsutil::write_atomic(&path, &text)
    }
}

#[cfg(test)]
mod persist_tests {
    use super::persist::{self, BreakerFile, Row};
    use super::{Breaker, Observation, Reason};
    use crate::config::testing::TempConfigDir;
    use std::time::SystemTime;

    // A breaker that has counted something, so a round trip has something to
    // lose
    fn armed() -> Breaker {
        let mut breaker = Breaker::default();
        let now = SystemTime::UNIX_EPOCH;
        breaker.observe(
            Observation::Failed(Reason::ProbeRefused),
            now,
            std::time::Duration::MAX,
        );
        breaker.observe(
            Observation::Failed(Reason::DescriptorsExhausted),
            now,
            std::time::Duration::MAX,
        );
        breaker
    }

    fn one(key: &str, breaker: Breaker) -> BreakerFile {
        BreakerFile::from([(key.to_string(), row(breaker))])
    }

    fn row(breaker: Breaker) -> Row {
        Row {
            breaker,
            log_len: 0,
            restarting: None,
        }
    }

    // The agent is a fresh process every thirty seconds, so this is the
    // entire memory of the state machine
    #[test]
    fn a_breaker_survives_the_process_that_wrote_it() {
        let _dir = TempConfigDir::new();
        let key = "dev.kubernetes".to_string();
        persist::save(&one(&key, armed()), std::slice::from_ref(&key)).expect("save");

        let back = persist::load().expect("load");
        assert_eq!(
            back.get(&key),
            Some(&row(armed())),
            "a round trip lost state"
        );
    }

    // A restart in flight is the one thing a dying pass must leave behind, so
    // it is written ahead of the pass's own save, and beside the other rows
    // rather than over them
    #[test]
    fn a_restart_in_flight_is_written_ahead_of_the_pass_and_beside_the_other_rows() {
        let _dir = TempConfigDir::new();
        let (kept, mine) = ("dev.cache".to_string(), "dev.kubernetes".to_string());
        persist::save(&one(&kept, armed()), std::slice::from_ref(&kept)).expect("save");

        let since = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let restarting = Row {
            restarting: Some(since),
            ..row(Breaker::default())
        };
        persist::checkpoint(&mine, &restarting).expect("checkpoint");

        let back = persist::load().expect("load");
        assert_eq!(back.get(&mine), Some(&restarting));
        assert_eq!(back.get(&kept), Some(&row(armed())), "the other row stays");
        assert_eq!(
            persist::load().expect("load")[&kept].restarting,
            None,
            "an idle row writes no entry"
        );
        let text = std::fs::read_to_string(persist::path().unwrap()).unwrap();
        assert_eq!(text.matches("restarting").count(), 1, "{text}");
    }

    #[test]
    fn a_missing_file_is_every_breaker_closed_rather_than_an_error() {
        let _dir = TempConfigDir::new();
        assert!(!persist::path().expect("path").exists());
        assert_eq!(
            persist::load().expect("a first run is not an error").len(),
            0
        );
    }

    // Starting over silently would restart tunnels a previous run stopped on
    // purpose, so this is an error - and the file stays put for a person to
    // look at
    #[test]
    fn a_corrupt_file_is_an_error_that_names_the_file_and_does_not_delete_it() {
        let _dir = TempConfigDir::new();
        let path = persist::path().expect("path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not toml {{{").unwrap();

        let err = persist::load().expect_err("a corrupt file is an error");
        assert!(
            format!("{err:#}").contains("breakers.toml"),
            "the error must name the file: {err:#}"
        );
        assert!(path.exists(), "a corrupt file is evidence, not litter");
    }

    // An error every pass forever is the watchdog going silent, which is
    // worse than the watchdog forgetting. There has to be a way out that
    // keeps the evidence
    #[test]
    fn a_file_that_cannot_be_parsed_can_be_moved_aside_and_started_over() {
        let _dir = TempConfigDir::new();
        let path = persist::path().expect("path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not toml {{{").unwrap();

        let moved = persist::set_aside().expect("set aside");
        assert!(!path.exists(), "the unparseable file is out of the way");
        assert!(moved.exists(), "and still on disk to look at");
        assert_eq!(
            std::fs::read_to_string(&moved).unwrap(),
            "this is not toml {{{"
        );
        assert_eq!(persist::load().expect("load").len(), 0);
    }

    // `read_to_string` fails for more reasons than a missing file, and the
    // others are not "every breaker closed": a file we cannot read might
    // say a tunnel was stopped on purpose
    #[test]
    fn a_file_that_exists_but_cannot_be_read_is_an_error_not_a_fresh_start() {
        let _dir = TempConfigDir::new();
        let path = persist::path().expect("path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();

        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            // root reads anything, so there is nothing to assert there
            if std::fs::read_to_string(&path).is_err() {
                let err = persist::load().expect_err("an unreadable file is an error");
                assert!(
                    format!("{err:#}").contains("breakers.toml"),
                    "the error must name the file: {err:#}"
                );
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn rows_for_tunnels_that_no_longer_exist_are_forgotten_on_save() {
        let _dir = TempConfigDir::new();
        let mut file = BreakerFile::new();
        file.insert("dev.kubernetes".to_string(), row(armed()));
        file.insert("dev.deleted-last-week".to_string(), row(armed()));
        persist::save(&file, &["dev.kubernetes".to_string()]).expect("save");

        let back = persist::load().expect("load");
        assert_eq!(back.len(), 1, "{back:?}");
        assert!(back.contains_key("dev.kubernetes"));
    }

    // Truncate-then-write loses the old file the moment it opens the new
    // one; write-then-rename cannot. The discriminator is a directory the
    // temp file cannot be created in: the atomic save fails and leaves the
    // previous file whole, where a direct write would have emptied it.
    #[test]
    fn a_write_interrupted_partway_leaves_the_previous_file_intact() {
        {
            use std::os::unix::fs::PermissionsExt;
            let _dir = TempConfigDir::new();
            let key = "dev.kubernetes".to_string();
            persist::save(&one(&key, armed()), std::slice::from_ref(&key)).expect("the first save");
            let path = persist::path().expect("path");
            let before = std::fs::read_to_string(&path).expect("read");

            let parent = path.parent().unwrap().to_path_buf();
            let saved = std::fs::metadata(&parent).unwrap().permissions();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();
            // root ignores the mode, and so would make this prove nothing
            let writable = std::fs::write(parent.join(".probe"), b"x").is_ok();
            let outcome = persist::save(&BreakerFile::new(), &[]);
            let after = std::fs::read_to_string(&path).ok();
            std::fs::set_permissions(&parent, saved).unwrap();

            if writable {
                let _ = std::fs::remove_file(parent.join(".probe"));
                return;
            }
            assert!(outcome.is_err(), "a save that cannot write must say so");
            assert_eq!(
                after.as_deref(),
                Some(before.as_str()),
                "the previous file must survive a failed save"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn epoch() -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    // A tunnel is only wedged if it stays wedged: one refused probe is what a
    // tunnel in the middle of its own restart looks like
    #[test]
    fn one_failed_probe_is_not_a_wedged_tunnel() {
        let mut breaker = Breaker::default();
        assert_eq!(
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                epoch(),
                std::time::Duration::MAX
            ),
            Action::Nothing
        );
    }

    #[test]
    fn two_consecutive_failures_trip_it_and_schedule_a_restart() {
        let mut breaker = Breaker::default();
        for _ in 0..2 {
            assert_eq!(
                breaker.observe(
                    Observation::Failed(Reason::ProbeRefused),
                    epoch(),
                    std::time::Duration::MAX
                ),
                Action::Nothing,
                "tripping schedules the restart, it does not perform it"
            );
        }
        assert_eq!(
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                epoch() + secs(10),
                std::time::Duration::MAX
            ),
            Action::Restart
        );
    }

    // Two tunnels that tripped on the same pass, one pass apart from a wake,
    // must not come due on the same later pass
    #[test]
    fn the_restart_is_due_somewhere_inside_the_backoff_not_always_at_its_end() {
        let fail = Observation::Failed(Reason::ProbeRefused);
        let due_after = |unit: f64| {
            let mut breaker = Breaker::default();
            for _ in 0..2 {
                breaker.observe_with(fail, epoch(), Duration::MAX, unit);
            }
            (0..=10).find(|s| {
                let mut probe = breaker.clone();
                probe.observe_with(fail, epoch() + secs(*s), Duration::MAX, unit) == Action::Restart
            })
        };
        assert_eq!(due_after(0.0), Some(0), "the shortest draw is due at once");
        assert_eq!(due_after(0.5), Some(5), "half of a 10s backoff");
        assert_eq!(due_after(1.0), Some(10), "the ceiling is the old schedule");
    }

    #[test]
    fn a_restart_does_not_happen_before_its_backoff_has_elapsed() {
        let mut breaker = Breaker::default();
        for _ in 0..2 {
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                epoch(),
                std::time::Duration::MAX,
            );
        }
        for early in [0, 1, 9] {
            assert_eq!(
                breaker.observe(
                    Observation::Failed(Reason::ProbeRefused),
                    epoch() + secs(early),
                    std::time::Duration::MAX
                ),
                Action::Nothing,
                "{early}s into a 10s backoff"
            );
        }
    }

    // 10, 20, 40, 80: a client that refloods instantly gets four chances, and
    // each one costs twice the quiet of the last
    #[test]
    fn each_attempt_waits_twice_as_long_as_the_one_before() {
        let mut breaker = Breaker::default();
        for _ in 0..2 {
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                epoch(),
                std::time::Duration::MAX,
            );
        }
        let mut now = epoch();
        for wait in [10u64, 20, 40, 80] {
            let too_early = now + secs(wait - 1);
            assert_eq!(
                breaker.observe(
                    Observation::Failed(Reason::ProbeRefused),
                    too_early,
                    std::time::Duration::MAX
                ),
                Action::Nothing,
                "a restart {wait}s out must not happen at {}s",
                wait - 1
            );
            now += secs(wait);
            assert_eq!(
                breaker.observe(
                    Observation::Failed(Reason::ProbeRefused),
                    now,
                    std::time::Duration::MAX
                ),
                Action::Restart,
                "the restart due after {wait}s"
            );
            // The restart lands in HalfOpen; the failure that follows raises
            // the attempt and schedules the next one - except after the last,
            // where there is no next one to schedule
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now,
                std::time::Duration::MAX,
            );
        }
    }

    #[test]
    fn two_consecutive_passes_close_it_and_forget_the_attempts() {
        let mut breaker = Breaker::default();
        for _ in 0..2 {
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                epoch(),
                std::time::Duration::MAX,
            );
        }
        breaker.observe(
            Observation::Failed(Reason::ProbeRefused),
            epoch() + secs(10),
            std::time::Duration::MAX,
        );
        for _ in 0..2 {
            assert_eq!(
                breaker.observe(
                    Observation::Pass,
                    epoch() + secs(10),
                    std::time::Duration::MAX
                ),
                Action::Nothing
            );
        }
        // Closed and forgetful: the next trip waits 10s again, not 20s
        let mut now = epoch() + secs(100);
        for _ in 0..2 {
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now,
                std::time::Duration::MAX,
            );
        }
        now += secs(10);
        assert_eq!(
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now,
                std::time::Duration::MAX
            ),
            Action::Restart,
            "a closed breaker starts its backoff over"
        );
    }

    // The observed case: 1 connection to 234 in a single five-second tick,
    // every time the tunnel came back. Restarting it forever only buries the
    // log line that explains it
    #[test]
    fn a_tunnel_that_refloods_after_every_restart_is_stopped_not_restarted_forever() {
        let mut breaker = Breaker::default();
        let mut now = epoch();
        for _ in 0..2 {
            breaker.observe(
                Observation::Failed(Reason::DescriptorsExhausted),
                now,
                std::time::Duration::MAX,
            );
        }
        let mut restarts = 0;
        let mut verdict = Action::Nothing;
        for wait in [10u64, 20, 40, 80] {
            now += secs(wait);
            if breaker.observe(
                Observation::Failed(Reason::DescriptorsExhausted),
                now,
                std::time::Duration::MAX,
            ) == Action::Restart
            {
                restarts += 1;
            }
            // Still wedged the moment it comes back
            verdict = breaker.observe(
                Observation::Failed(Reason::DescriptorsExhausted),
                now,
                std::time::Duration::MAX,
            );
        }
        assert_eq!(restarts, MAX_ATTEMPTS, "four chances, then the truth");
        assert_eq!(
            verdict,
            Action::StopAndReport(Reason::DescriptorsExhausted),
            "the failure after the last restart is a report, not a fifth restart"
        );
    }

    // `next_attempt` is an absolute time written on a previous pass, and the
    // clock moves between passes: an NTP correction, or a laptop resuming
    // with a bad clock, steps `now` backwards. A plain `now < due` then holds
    // for the whole length of the skew, and the wedged tunnel the breaker
    // tripped on waits out a clock error instead of being restarted
    #[test]
    fn a_clock_that_steps_backwards_does_not_park_a_tripped_breaker() {
        let mut breaker = Breaker::default();
        let now = epoch() + secs(86_400);
        for _ in 0..RUN {
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now,
                std::time::Duration::MAX,
            );
        }

        // The clock jumps back an hour; the restart was due ten seconds out
        let stepped = now - secs(3_600);
        assert_eq!(
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                stepped,
                std::time::Duration::MAX
            ),
            Action::Restart,
            "a restart due further ahead than one backoff is a stepped clock, not a wait"
        );
    }

    // The guard must not fire on an ordinary wait, or every backoff collapses
    // to nothing and the ladder restarts four times in four passes
    #[test]
    fn an_ordinary_wait_is_still_a_wait() {
        let mut breaker = Breaker::default();
        let now = epoch();
        for _ in 0..RUN {
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now,
                std::time::Duration::MAX,
            );
        }
        assert_eq!(
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now + secs(1),
                std::time::Duration::MAX
            ),
            Action::Nothing,
            "one second into a ten-second backoff is still waiting"
        );
    }

    // Stopped is the watchdog's own verdict. While the pass cannot tell, it
    // holds, however long that lasts
    #[test]
    fn a_stopped_breaker_holds_while_a_pass_cannot_tell() {
        let mut breaker = stopped();
        let mut now = epoch() + secs(1_000);
        for _ in 0..20 {
            now += secs(3_600);
            assert_eq!(
                breaker.observe(Observation::Unknown, now, std::time::Duration::MAX),
                Action::Nothing
            );
        }
        assert_eq!(breaker, stopped(), "an unknown pass changes nothing");
    }

    // The watchdog stops a wedged tunnel, the person starts it again before the
    // next pass, and the pass finds it running. `NotRunning` never comes, so
    // the breaker stayed `Stopped` and the tunnel was never watched again
    #[test]
    fn a_stopped_breaker_closes_when_a_person_has_started_the_tunnel_again() {
        for running in [Observation::Pass, Observation::Failed(Reason::ProbeRefused)] {
            let mut breaker = stopped();
            let first = breaker.observe(running, epoch() + secs(10), std::time::Duration::MAX);
            assert_eq!(
                first,
                Action::Nothing,
                "{running:?} reads the person's start"
            );
            assert_ne!(breaker, stopped(), "{running:?} must leave Stopped");

            // Watched again: a later wedge is counted, trips, and is restarted
            let fail = Observation::Failed(Reason::ProbeRefused);
            let mut now = epoch() + secs(20);
            let mut restarted = false;
            for _ in 0..4 {
                now += secs(30);
                restarted |=
                    breaker.observe(fail, now, std::time::Duration::MAX) == Action::Restart;
            }
            assert!(restarted, "after {running:?} a later wedge counts again");
        }
    }

    // `gtunnel stop` has to win. A tunnel a person took down is not a tunnel
    // with a problem, and the breaker must not bring it back
    #[test]
    fn a_tunnel_a_person_stopped_is_not_a_tunnel_to_restart() {
        let mut armed = Breaker::default();
        armed.observe(
            Observation::Failed(Reason::ProbeRefused),
            epoch(),
            std::time::Duration::MAX,
        );
        armed.observe(
            Observation::Failed(Reason::ProbeRefused),
            epoch(),
            std::time::Duration::MAX,
        );

        for mut breaker in [Breaker::default(), armed, stopped()] {
            assert_eq!(
                breaker.observe(
                    Observation::NotRunning,
                    epoch() + secs(10),
                    std::time::Duration::MAX
                ),
                Action::Nothing
            );
            // Clean afterwards: one failure on the way back up is still just
            // one failure, whatever the breaker had counted before
            assert_eq!(
                breaker.observe(
                    Observation::Failed(Reason::ProbeRefused),
                    epoch() + secs(20),
                    std::time::Duration::MAX
                ),
                Action::Nothing
            );
        }
    }

    // A breaker that reached its limit, by the only route that reaches it
    fn stopped() -> Breaker {
        let mut breaker = Breaker::default();
        let mut now = epoch();
        for _ in 0..2 {
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now,
                std::time::Duration::MAX,
            );
        }
        let mut verdict = Action::Nothing;
        for wait in [10u64, 20, 40, 80] {
            now += secs(wait);
            breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now,
                std::time::Duration::MAX,
            );
            verdict = breaker.observe(
                Observation::Failed(Reason::ProbeRefused),
                now,
                std::time::Duration::MAX,
            );
        }
        assert_eq!(verdict, Action::StopAndReport(Reason::ProbeRefused));
        breaker
    }

    // Offline, every probe fails on the same pass. The watchdog calls that
    // pass `Unknown`, and a breaker that moved on it would climb the ladder
    // to a stop for a fault that is not the tunnel's.
    #[test]
    fn a_pass_the_network_gate_vetoed_leaves_every_breaker_unchanged() {
        let fail = Observation::Failed(Reason::ProbeRefused);
        let mut one_failure = Breaker::default();
        one_failure.observe(fail, epoch(), Duration::MAX);
        let mut tripped = one_failure.clone();
        tripped.observe(fail, epoch(), Duration::MAX);
        let mut half_open = tripped.clone();
        half_open.observe(fail, epoch() + secs(10), Duration::MAX);
        for start in [
            Breaker::default(),
            one_failure,
            tripped,
            half_open,
            stopped(),
        ] {
            let mut breaker = start.clone();
            for pass in 1..=20 {
                assert_eq!(
                    breaker.observe(Observation::Unknown, epoch() + secs(pass * 30), secs(60)),
                    Action::Nothing
                );
            }
            assert_eq!(breaker, start, "an unknown pass changed the breaker");
        }
    }

    // A failure stored before the laptop slept is not the first half of a
    // trip after it wakes: the pair has to be one run, not two moments
    #[test]
    fn a_stored_failure_older_than_two_intervals_does_not_count_toward_a_trip() {
        let stale_after = secs(60);
        let fail = Observation::Failed(Reason::ProbeRefused);
        let mut breaker = Breaker::default();
        breaker.observe(fail, epoch(), stale_after);
        let woke = epoch() + secs(3600);
        assert_eq!(breaker.observe(fail, woke, stale_after), Action::Nothing);
        assert_eq!(
            breaker.observe(fail, woke + secs(30), stale_after),
            Action::Nothing,
            "the stale failure was forgotten, so the woken one is only the first of a pair"
        );
        assert_eq!(
            breaker.observe(fail, woke + secs(60), stale_after),
            Action::Restart,
            "a fresh pair still trips it"
        );
    }

    #[test]
    fn a_failure_within_two_intervals_still_counts() {
        let stale_after = secs(60);
        let fail = Observation::Failed(Reason::ProbeRefused);
        let mut breaker = Breaker::default();
        breaker.observe(fail, epoch(), stale_after);
        assert_eq!(
            breaker.observe(fail, epoch() + secs(30), stale_after),
            Action::Nothing,
            "the second of a pair trips it, and the restart waits for its backoff"
        );
        assert_eq!(
            breaker.observe(fail, epoch() + secs(40), stale_after),
            Action::Restart
        );
    }

    // A breaker file written by v0.2.62 has no `last_failure`
    #[test]
    fn a_breaker_written_before_failures_were_timed_still_loads() {
        let old = "state = \"closed\"\nfailures = 1\npasses = 0\nattempt = 0\n";
        let breaker: Breaker = toml::from_str(old).expect("an old file loads");
        assert_eq!(breaker.failures, 1);
        assert_eq!(breaker.last_failure, None);
    }
}
