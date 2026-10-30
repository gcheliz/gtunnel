//! How long to wait before trying again, and how far apart to space things
//! that would otherwise all happen at one instant.
//!
//! A wake, a login or the end of an outage fails every tunnel at the same
//! moment, and a doubling that is the same for all of them keeps them failing
//! together: N `gcloud` processes start at +0, +10, +30, +70 s, each one a
//! Python start that reads the credential store and asks Google for a token.
//! "Full jitter", a draw from zero to the ceiling, spreads them over the whole
//! interval, which is what breaks the lockstep. The ceiling is still the
//! doubling, so a lone tunnel's worst case is what it always was.
//!
//! The draw is a parameter of every `_at` function, so a test fixes it and
//! asserts exact values; the functions without the suffix draw it.

use rand::RngExt;
use std::time::Duration;

/// A supervisor that launchd or systemd started waits up to this long before
/// its first `gcloud`, so a login that loads five jobs does not start five
/// together.
pub const START_SPREAD: Duration = Duration::from_secs(3);

/// The watchdog waits up to this long before each restart in a pass, for the
/// same reason one level up: a pass that finds five wedged tunnels would
/// otherwise restart them back to back.
pub const RESTART_SPREAD: Duration = Duration::from_secs(2);

/// The longest a retry can wait: `base * 2^(attempt - 1)`, held at `cap`.
/// Attempt 0 is read as the first, and the shift is bounded so a count this
/// function cannot represent saturates rather than panics.
pub fn ceiling(attempt: u32, base: Duration, cap: Duration) -> Duration {
    let doublings = attempt.saturating_sub(1).min(30);
    base.saturating_mul(1 << doublings).min(cap)
}

/// A wait drawn from `[0, ceiling]`, with `unit` the draw in `[0, 1]`.
pub fn backoff_at(attempt: u32, base: Duration, cap: Duration, unit: f64) -> Duration {
    ceiling(attempt, base, cap).mul_f64(clamp_unit(unit))
}

/// The one retry wait in gtunnel: the supervisor's budget and network waits and
/// the watchdog's breaker all come here, so none of them keeps a doubling of
/// its own.
pub fn backoff(attempt: u32, base: Duration, cap: Duration) -> Duration {
    backoff_at(attempt, base, cap, draw())
}

/// A wait drawn from `[0, max]`, with `unit` the draw in `[0, 1]`.
pub fn spread_at(max: Duration, unit: f64) -> Duration {
    max.mul_f64(clamp_unit(unit))
}

pub fn spread(max: Duration) -> Duration {
    spread_at(max, draw())
}

/// One uniform draw in `[0, 1)`.
pub fn draw() -> f64 {
    rand::rng().random::<f64>()
}

// A NaN would make `mul_f64` panic, and a panic here takes the supervisor or
// the whole watch pass with it
fn clamp_unit(unit: f64) -> f64 {
    if unit.is_nan() {
        1.0
    } else {
        unit.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_secs(10);
    const CAP: Duration = Duration::from_secs(300);

    #[test]
    fn a_thousand_draws_fall_within_the_cap_and_are_not_all_equal() {
        let draws: Vec<Duration> = (0..1000).map(|_| backoff(20, BASE, CAP)).collect();
        assert!(draws.iter().all(|d| *d <= CAP), "a draw exceeded the cap");
        let first = draws[0];
        assert!(draws.iter().any(|d| *d != first), "no spread at all");
        let top = draws.iter().max().unwrap();
        let bottom = draws.iter().min().unwrap();
        assert!(
            *top - *bottom > CAP / 2,
            "1000 draws covered only {:?}",
            *top - *bottom
        );
    }

    #[test]
    fn the_ceiling_doubles_to_the_cap() {
        let secs: Vec<u64> = (0..8).map(|n| ceiling(n, BASE, CAP).as_secs()).collect();
        assert_eq!(secs, [10, 10, 20, 40, 80, 160, 300, 300]);
    }

    #[test]
    fn a_huge_attempt_saturates_instead_of_panicking() {
        assert_eq!(ceiling(u32::MAX, BASE, CAP), CAP);
        assert_eq!(ceiling(3, Duration::MAX, Duration::MAX), Duration::MAX);
    }

    #[test]
    fn the_draw_scales_the_ceiling() {
        assert_eq!(backoff_at(3, BASE, CAP, 0.0), Duration::ZERO);
        assert_eq!(backoff_at(3, BASE, CAP, 0.5), Duration::from_secs(20));
        assert_eq!(backoff_at(3, BASE, CAP, 1.0), Duration::from_secs(40));
    }

    #[test]
    fn a_draw_outside_zero_to_one_cannot_escape_the_ceiling() {
        assert_eq!(backoff_at(3, BASE, CAP, 7.0), Duration::from_secs(40));
        assert_eq!(backoff_at(3, BASE, CAP, -1.0), Duration::ZERO);
        assert_eq!(backoff_at(3, BASE, CAP, f64::NAN), Duration::from_secs(40));
    }

    #[test]
    fn a_spread_stays_inside_its_bound() {
        let draws: Vec<Duration> = (0..1000).map(|_| spread(START_SPREAD)).collect();
        assert!(draws.iter().all(|d| *d <= START_SPREAD));
        assert!(draws.iter().any(|d| *d > Duration::ZERO));
        assert!(draws.iter().any(|d| *d != draws[0]));
        assert_eq!(spread_at(RESTART_SPREAD, 0.5), Duration::from_secs(1));
    }
}
