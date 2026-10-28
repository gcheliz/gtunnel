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
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            window: Duration::from_secs(10 * 60),
            min_uptime: Duration::from_secs(60),
            base_backoff: Duration::from_secs(10),
            max_backoff: Duration::from_secs(5 * 60),
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

    pub fn len(&self) -> usize {
        self.0.len()
    }

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
}
