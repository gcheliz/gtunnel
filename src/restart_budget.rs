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
//! to a stop.

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
    /// The wait after the first failure; each next one doubles it.
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

/// Judge one failed run. `ran_for` is how long the child lived.
pub fn judge(b: &Budget, h: &mut History, ran_for: Duration, now: SystemTime) -> Verdict {
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
    // 2^(n-1), with the shift bounded so a large budget cannot overflow
    let doublings = u32::try_from(n - 1).unwrap_or(u32::MAX).min(30);
    let after = b
        .base_backoff
        .saturating_mul(1 << doublings)
        .min(b.max_backoff);
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
pub fn judge_busy(
    b: &Budget,
    since: Option<SystemTime>,
    ran_for: Duration,
    now: SystemTime,
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
    Busy::Wait {
        since,
        after: b.busy_delay,
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
/// to 5 min, so an offline laptop is not hammered. `fails` is the count of
/// network exits in a row; a run that lived `min_uptime` was working, so it
/// starts the count over, as it does for the budget.
pub fn network_wait(b: &Budget, fails: &mut u32, ran_for: Duration) -> Duration {
    if ran_for >= b.min_uptime {
        *fails = 0;
    }
    *fails = fails.saturating_add(1);
    b.base_backoff
        .saturating_mul(1 << (*fails - 1).min(30))
        .min(b.max_backoff)
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
    fn a_relay_that_still_holds_the_old_listener_is_retried_on_a_fixed_delay() {
        let b = Budget::default();
        let first = at(1000);
        let mut since = None;
        for n in 0..20u64 {
            let now = at(1000 + n * 15);
            match judge_busy(&b, since, quick(), now) {
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
        let v = judge_busy(&b, Some(at(1000)), quick(), at(1000) + b.busy_window);
        assert!(matches!(v, Busy::Wait { .. }), "{v:?}");
        let v = judge_busy(&b, Some(at(1000)), quick(), at(1001) + b.busy_window);
        assert_eq!(v, Busy::Spent);
    }

    #[test]
    fn a_run_that_worked_starts_a_new_streak() {
        let b = Budget::default();
        let now = at(1000) + b.busy_window * 3;
        let v = judge_busy(&b, Some(at(1000)), b.min_uptime, now);
        assert_eq!(
            v,
            Busy::Wait {
                since: now,
                after: b.busy_delay
            }
        );
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
