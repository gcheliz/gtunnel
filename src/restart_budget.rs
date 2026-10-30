//! When a supervisor should stop restarting a gcloud that keeps dying.
//!
//! Like `breaker`, the decision lives here and nothing else does: no clock, no
//! file, no process. `now` is a parameter, so every row of the plan's table is
//! a test that runs in microseconds. The supervisor in `commands::supervise`
//! keeps the history between runs and carries out the verdict.
//!
//! The count is of runs that died before `min_uptime`. A run that lived that
//! long was working, whatever killed it afterwards, so it clears the count:
//! that is also why a wake from sleep, which ends a long run, never adds up
//! to a stop. The length of a run is the larger of the monotonic and the wall
//! clock ([`run_length`]), because the monotonic clock stops during sleep on
//! macOS; a run that began just before the lid closed still costs one history
//! entry and one backoff step if it dies before `min_uptime` of either clock.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many fast failures a tunnel is allowed, and how it waits between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Fast failures tolerated inside `window`. 0 turns the budget off.
    pub max_restarts: u32,
    /// Failures older than this are forgotten.
    pub window: Duration,
    /// A run at least this long clears the count.
    pub min_uptime: Duration,
    /// The ceiling of the wait after the first failure; each next one doubles
    /// it, and the wait itself is drawn from zero up to that ceiling.
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    /// The fixed wait after a run that died because the relay still holds the
    /// listener of its dead predecessor.
    pub busy_delay: Duration,
    /// How long such deaths may go on before they count like any other.
    pub busy_window: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            window: Duration::from_secs(10 * 60),
            min_uptime: Duration::from_secs(60),
            base_backoff: Duration::from_secs(10),
            max_backoff: Duration::from_secs(5 * 60),
            // Two missed keepalives on the relay is about 30 s, so a retry
            // every 15 s finds the port free on the second or third try
            busy_delay: Duration::from_secs(15),
            busy_window: Duration::from_secs(5 * 60),
        }
    }
}

/// When each recent fast failure happened. Persisted between supervisor
/// processes, because the service manager starts a new one per attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History(Vec<SystemTime>);

impl History {
    /// One unix second per line. A line that is not a number is dropped: a
    /// damaged file must cost the count, never the tunnel.
    pub fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .filter_map(|l| l.trim().parse::<u64>().ok())
                .map(|s| UNIX_EPOCH + Duration::from_secs(s))
                .collect(),
        )
    }

    pub fn render(&self) -> String {
        self.0
            .iter()
            .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| format!("{}\n", d.as_secs()))
            .collect()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Wait this long, then let the service manager start it again.
    Restart { after: Duration },
    /// Spent: stay down until a person starts it.
    Stop,
}

/// How long a run lived, given both clocks.
///
/// `Instant` is `CLOCK_UPTIME_RAW` on macOS and does not advance while the
/// machine sleeps, so a run that spanned a sleep reads as seconds long and
/// would be judged a fast failure. The wall clock does advance, so the larger
/// of the two is taken. `wall` is the result of `SystemTime::duration_since`,
/// which fails when the clock was set back; then the instant is all there is.
pub fn run_length(
    instant: Duration,
    wall: Result<Duration, std::time::SystemTimeError>,
) -> Duration {
    wall.map_or(instant, |w| w.max(instant))
}

/// Judge one failed run. `ran_for` is how long the child lived.
///
/// `unit` is the draw in `[0, 1]` that places the wait inside its backoff;
/// see [`crate::daemon::backoff`] for why it is drawn at all.
pub fn judge(
    b: &Budget,
    h: &mut History,
    ran_for: Duration,
    now: SystemTime,
    unit: f64,
) -> Verdict {
    // A budget of 0 is the old behaviour: restart at once, forever, keeping
    // nothing that could grow
    if b.max_restarts == 0 {
        return Verdict::Restart {
            after: Duration::ZERO,
        };
    }
    if ran_for >= b.min_uptime {
        h.0.clear();
        return Verdict::Restart {
            after: Duration::ZERO,
        };
    }
    // An entry stamped in the future (the clock moved back) has an age of
    // zero and is kept: forgetting it would let a loop outlive its budget
    h.0.retain(|t| now.duration_since(*t).map_or(true, |age| age <= b.window));
    h.0.push(now);
    let n = h.0.len();
    if n > b.max_restarts as usize {
        return Verdict::Stop;
    }
    let attempt = u32::try_from(n).unwrap_or(u32::MAX);
    let after = crate::daemon::backoff::backoff_at(attempt, b.base_backoff, b.max_backoff, unit);
    Verdict::Restart { after }
}

/// What to do with a run that died on `remote port forwarding failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    /// Wait `after`, then start again. `since` is when this streak began and
    /// is what the supervisor writes down for the next process.
    Wait { since: SystemTime, after: Duration },
    /// The relay never let go: count it like any other fast failure.
    Spent,
}

/// Judge a run that died because the relay still holds the old session's
/// listener. Nothing here is the tunnel's fault and a restart is the cure, so
/// it does not spend the budget; but a relay that never lets go must not be
/// retried forever, hence the window. A run that lived `min_uptime` was
/// working, so what killed it afterwards starts a new streak.
///
/// The wait is half `busy_delay` plus a draw over the other half (`unit` in
/// `[0, 1]`): the tunnels one wake cut retry apart, and none comes back before
/// sshd has had half the delay to reap the old session.
pub fn judge_busy(
    b: &Budget,
    since: Option<SystemTime>,
    ran_for: Duration,
    now: SystemTime,
    unit: f64,
) -> Busy {
    let since = match since {
        Some(t) if ran_for < b.min_uptime => t,
        _ => now,
    };
    // A start stamped in the future (the clock moved back) has an age of
    // zero, as in `judge`: the streak is kept, and so stays bounded
    let age = now.duration_since(since).unwrap_or(Duration::ZERO);
    if age > b.busy_window {
        return Busy::Spent;
    }
    let half = b.busy_delay / 2;
    Busy::Wait {
        since,
        after: half + crate::daemon::backoff::spread_at(b.busy_delay - half, unit),
    }
}

/// The streak's start as `History` keeps its entries: one unix second.
pub fn parse_since(text: &str) -> Option<SystemTime> {
    let s = text.trim().parse::<u64>().ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(s))
}

pub fn render_since(t: SystemTime) -> String {
    let s = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("{s}\n")
}

/// How long to wait after a run that died of the network, and the count to
/// keep for the next one.
///
/// A network exit says nothing about the tunnel, so it never reaches [`judge`]
/// and can never stop the job: an outage that lasts an hour is a loop of
/// retries, not a verdict. The wait uses the budget's own backoff, 10 s doubling
/// to 5 min and drawn from zero up to it, so an offline laptop is not hammered
/// and a house of them does not retry on the same second. `fails` is the count of
/// network exits in a row; a run that lived `min_uptime` was working, so it
/// starts the count over, as it does for the budget.
pub fn network_wait(b: &Budget, fails: &mut u32, ran_for: Duration, unit: f64) -> Duration {
    if ran_for >= b.min_uptime {
        *fails = 0;
    }
    *fails = fails.saturating_add(1);
    crate::daemon::backoff::backoff_at(*fails, b.base_backoff, b.max_backoff, unit)
}

/// The persisted network count: one integer. Anything else is zero, because a
/// damaged file must cost the count and never the tunnel.
pub fn parse_net_failures(text: &str) -> u32 {
    text.trim().parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn quick() -> Duration {
        Duration::from_secs(1)
    }

    // The ceiling is the draw of 1.0, so every wait below is the doubling the
    // table names; the jitter has its own tests
    fn judge(b: &Budget, h: &mut History, ran_for: Duration, now: SystemTime) -> Verdict {
        super::judge(b, h, ran_for, now, 1.0)
    }

    fn network_wait(b: &Budget, fails: &mut u32, ran_for: Duration) -> Duration {
        super::network_wait(b, fails, ran_for, 1.0)
    }

    #[test]
    fn a_restart_wait_is_drawn_from_zero_to_its_backoff() {
        let b = Budget::default();
        let wait = |unit| match super::judge(&b, &mut History::default(), quick(), at(1000), unit) {
            Verdict::Restart { after } => after,
            Verdict::Stop => panic!("stopped"),
        };
        assert_eq!(wait(0.0), Duration::ZERO);
        assert_eq!(wait(0.5), b.base_backoff / 2);
        assert_eq!(wait(1.0), b.base_backoff);
    }

    #[test]
    fn a_network_wait_is_drawn_from_zero_to_its_backoff() {
        let b = Budget::default();
        let wait = |unit| super::network_wait(&b, &mut 2, quick(), unit);
        assert_eq!(wait(0.0), Duration::ZERO);
        assert_eq!(wait(0.5), b.base_backoff * 2);
        assert_eq!(wait(1.0), b.base_backoff * 4);
    }

    // The issue's second acceptance item: two copies of the doubling is how
    // the supervisor and the breaker drifted apart in the first place
    #[test]
    fn the_supervisor_and_the_breaker_use_the_shared_backoff_and_double_nothing_themselves() {
        for (file, source) in [
            ("restart_budget.rs", include_str!("restart_budget.rs")),
            ("breaker.rs", include_str!("breaker.rs")),
        ] {
            let production = source.split("#[cfg(test)]\nmod ").next().unwrap();
            assert!(production.contains("backoff::"), "{file} does not call it");
            assert!(
                !production.contains("saturating_mul(1"),
                "{file} writes the doubling again"
            );
        }
    }

    #[test]
    fn instant_exits_stop_at_the_limit() {
        let b = Budget::default();
        let mut h = History::default();
        for n in 1..=b.max_restarts {
            let v = judge(&b, &mut h, quick(), at(1000 + u64::from(n)));
            assert!(matches!(v, Verdict::Restart { .. }), "failure {n}: {v:?}");
        }
        let v = judge(&b, &mut h, quick(), at(1100));
        assert_eq!(v, Verdict::Stop, "one past the budget");
    }

    #[test]
    fn a_run_that_spanned_a_sleep_clears_the_count() {
        let b = Budget::default();
        let mut h = History::default();
        for n in 0..3 {
            judge(&b, &mut h, quick(), at(1000 + n));
        }
        assert_eq!(h.len(), 3);
        let eight_hours = Duration::from_secs(8 * 3600);
        let ran_for = run_length(Duration::from_secs(30), Ok(eight_hours));
        assert_eq!(ran_for, eight_hours);
        let v = judge(&b, &mut h, ran_for, at(1010));
        assert!(h.is_empty(), "the sleep counted as time the run was up");
        assert_eq!(
            v,
            Verdict::Restart {
                after: Duration::ZERO
            }
        );
    }

    #[test]
    fn a_wall_clock_that_went_backwards_falls_back_to_the_instant() {
        let earlier = at(100);
        let later = at(200);
        let backwards = earlier.duration_since(later);
        assert!(backwards.is_err());
        let thirty = Duration::from_secs(30);
        assert_eq!(run_length(thirty, backwards), thirty);
        assert_eq!(run_length(thirty, Ok(Duration::from_secs(5))), thirty);
    }

    #[test]
    fn a_long_run_resets_the_count() {
        let b = Budget::default();
        let mut h = History::default();
        for n in 0..b.max_restarts {
            judge(&b, &mut h, quick(), at(1000 + u64::from(n)));
        }
        assert_eq!(h.len(), b.max_restarts as usize);
        // The next death comes after a run that was working
        let v = judge(&b, &mut h, b.min_uptime, at(1010));
        assert!(h.is_empty(), "the long run cleared the history");
        assert_eq!(
            v,
            Verdict::Restart {
                after: Duration::ZERO
            },
            "a run that worked restarts at once"
        );
        let v = judge(&b, &mut h, quick(), at(1011));
        assert_eq!(
            v,
            Verdict::Restart {
                after: b.base_backoff
            },
            "the budget starts fresh"
        );
    }

    #[test]
    fn backoff_doubles_to_the_cap() {
        let b = Budget {
            max_restarts: 20,
            ..Budget::default()
        };
        let mut h = History::default();
        let mut waits = Vec::new();
        for n in 0..8 {
            match judge(&b, &mut h, quick(), at(1000 + n)) {
                Verdict::Restart { after } => waits.push(after.as_secs()),
                Verdict::Stop => panic!("stopped early"),
            }
        }
        assert_eq!(waits, [10, 20, 40, 80, 160, 300, 300, 300]);
    }

    #[test]
    fn zero_max_never_stops() {
        let b = Budget {
            max_restarts: 0,
            ..Budget::default()
        };
        let mut h = History::default();
        for n in 0..100 {
            let v = judge(&b, &mut h, quick(), at(1000 + n));
            assert_eq!(
                v,
                Verdict::Restart {
                    after: Duration::ZERO
                }
            );
        }
        assert!(h.is_empty(), "a disabled budget keeps no history");
    }

    #[test]
    fn exits_outside_the_window_are_forgotten() {
        let b = Budget::default();
        let mut h = History::default();
        for n in 0..b.max_restarts {
            judge(&b, &mut h, quick(), at(1000 + u64::from(n)));
        }
        // Past the window the five old failures no longer count
        let later = 1000 + b.window.as_secs() + 60;
        let v = judge(&b, &mut h, quick(), at(later));
        assert_eq!(
            v,
            Verdict::Restart {
                after: b.base_backoff
            }
        );
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn the_history_survives_a_round_trip_through_its_file() {
        let b = Budget::default();
        let mut h = History::default();
        judge(&b, &mut h, quick(), at(1000));
        judge(&b, &mut h, quick(), at(1005));
        assert_eq!(History::parse(&h.render()), h);
        assert_eq!(History::parse("junk\n\n42\n").len(), 1, "junk is dropped");
    }

    #[test]
    fn a_relay_that_still_holds_the_old_listener_is_retried_without_spending_the_budget() {
        let b = Budget::default();
        let first = at(1000);
        let mut since = None;
        for n in 0..20u64 {
            let now = at(1000 + n * 15);
            match judge_busy(&b, since, quick(), now, 1.0) {
                Busy::Wait { since: s, after } => {
                    assert_eq!(after, Duration::from_secs(15), "attempt {n}");
                    since = Some(s);
                }
                Busy::Spent => panic!("spent at attempt {n}, {}s in", n * 15),
            }
        }
        assert_eq!(since, Some(first), "the streak keeps its first start");
    }

    #[test]
    fn a_relay_that_never_lets_go_is_counted_after_the_window() {
        let b = Budget::default();
        let v = judge_busy(&b, Some(at(1000)), quick(), at(1000) + b.busy_window, 1.0);
        assert!(matches!(v, Busy::Wait { .. }), "{v:?}");
        let v = judge_busy(&b, Some(at(1000)), quick(), at(1001) + b.busy_window, 1.0);
        assert_eq!(v, Busy::Spent);
    }

    #[test]
    fn a_run_that_worked_starts_a_new_streak() {
        let b = Budget::default();
        let now = at(1000) + b.busy_window * 3;
        let v = judge_busy(&b, Some(at(1000)), b.min_uptime, now, 1.0);
        assert_eq!(
            v,
            Busy::Wait {
                since: now,
                after: b.busy_delay
            }
        );
    }

    #[test]
    fn a_busy_relay_wait_is_drawn_from_half_its_delay_to_all_of_it() {
        let b = Budget::default();
        let wait = |unit| match judge_busy(&b, None, quick(), at(1000), unit) {
            Busy::Wait { after, .. } => after,
            Busy::Spent => panic!("a new streak is never spent"),
        };
        assert_eq!(wait(0.0), Duration::from_millis(7500));
        assert_eq!(wait(0.5), Duration::from_millis(11250));
        assert_eq!(wait(1.0), Duration::from_secs(15));
    }

    fn ladder(b: &Budget, n: u32) -> Duration {
        let mut fails = n - 1;
        network_wait(b, &mut fails, quick())
    }

    // The same 10 s to 5 min the budget already uses between fast failures,
    // so a network outage backs off as a crash loop does and never stops
    #[test]
    fn the_network_wait_doubles_from_ten_seconds_to_five_minutes_and_stays() {
        let b = Budget::default();
        let secs = |n| Duration::from_secs(n);
        let got: Vec<Duration> = (1..=8).map(|n| ladder(&b, n)).collect();
        assert_eq!(
            got,
            [10, 20, 40, 80, 160, 300, 300, 300].map(secs),
            "10, 20, 40, 80, 160, then the 300 s cap"
        );
        assert_eq!(
            ladder(&b, 1000),
            b.max_backoff,
            "an unbounded count is safe"
        );
    }

    #[test]
    fn the_streak_start_survives_the_file_it_is_kept_in() {
        let t = at(1234);
        assert_eq!(parse_since(&render_since(t)), Some(t));
        assert_eq!(parse_since("not a number"), None);
    }

    #[test]
    fn a_run_that_lived_min_uptime_starts_the_network_wait_over() {
        let b = Budget::default();
        let mut fails = 9;
        assert_eq!(network_wait(&b, &mut fails, b.min_uptime), b.base_backoff);
        assert_eq!(fails, 1, "the long run was working, so the count restarts");
        assert_eq!(network_wait(&b, &mut fails, quick()), 2 * b.base_backoff);
        assert_eq!(fails, 2);
    }

    #[test]
    fn the_network_count_survives_a_round_trip_through_its_file() {
        assert_eq!(parse_net_failures("7\n"), 7);
        assert_eq!(parse_net_failures(""), 0);
        assert_eq!(
            parse_net_failures("junk"),
            0,
            "a damaged file costs the count"
        );
    }
}
