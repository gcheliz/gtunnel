// One backoff policy for every place gtunnel retries a GCP call: the client's
// transient-error loop and the fingerprint (412) loop in `expose::routes`.
// Google's client guidance is exponential backoff with jitter, and "full"
// jitter (a uniform draw from zero to the ceiling) is what spreads the retries
// of two gtunnel processes that failed at the same instant. A fixed or
// ceiling-only delay would send them back together.

use rand::RngExt;
use std::time::Duration;

/// Attempts per request, the first one included.
pub const ATTEMPTS: u32 = 4;

/// A server that asks for a long wait gets at most this: a `Retry-After` of an
/// hour is a command that looks hung, and failing is the more useful answer.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);

/// The ceiling stops doubling here, so a high attempt count cannot sleep for
/// minutes.
const CEILING_CAP: Duration = Duration::from_secs(8);

// A unit test would otherwise sleep half a second per retry for nothing
#[cfg(not(test))]
const BASE: Duration = Duration::from_millis(500);
#[cfg(test)]
const BASE: Duration = Duration::from_millis(1);

/// How long to wait before attempt `attempt + 1`, where `attempt` counts the
/// tries already made (1 after the first).
pub fn delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    delay_from(BASE, attempt, retry_after)
}

/// The same with the base spelled out, so a test can assert the real numbers
/// that the test build shrinks.
pub fn delay_from(base: Duration, attempt: u32, retry_after: Option<Duration>) -> Duration {
    let ceiling = ceiling(base, attempt);
    let ceiling_ms = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
    let jitter = Duration::from_millis(rand::rng().random_range(0..=ceiling_ms));
    match retry_after {
        // The server's word is a floor, never something to jitter below
        Some(asked) => asked.min(RETRY_AFTER_CAP).max(jitter),
        None => jitter,
    }
}

pub fn ceiling(base: Duration, attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(16);
    base.saturating_mul(1u32 << doublings).min(CEILING_CAP)
}

/// `Retry-After` as delta-seconds. The HTTP-date form is not read: Google's
/// APIs send seconds, and an unreadable value falls back to the jitter.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_doubles_from_500_ms_and_stops_at_eight_seconds() {
        let base = Duration::from_millis(500);
        let got: Vec<u128> = (1..=7).map(|n| ceiling(base, n).as_millis()).collect();
        assert_eq!(got, vec![500, 1000, 2000, 4000, 8000, 8000, 8000]);
    }

    #[test]
    fn a_delay_is_a_draw_between_zero_and_the_ceiling() {
        let base = Duration::from_millis(500);
        let draws: Vec<Duration> = (0..200).map(|_| delay_from(base, 2, None)).collect();
        assert!(draws.iter().all(|d| *d <= Duration::from_millis(1000)));
        // Full jitter spreads; a constant would mean the draw is not random
        assert!(draws.iter().any(|d| *d != draws[0]), "{draws:?}");
    }

    #[test]
    fn a_retry_after_is_a_floor_capped_at_thirty_seconds() {
        let base = Duration::from_millis(500);
        let asked = Duration::from_secs(3);
        assert!(delay_from(base, 1, Some(asked)) >= asked);
        assert_eq!(
            delay_from(base, 1, Some(Duration::from_secs(3600))),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn retry_after_reads_seconds_and_ignores_anything_else() {
        assert_eq!(parse_retry_after(" 7 "), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after(""), None);
    }
}
