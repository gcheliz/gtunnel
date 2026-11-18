// Durations and timestamps as people type and read them, on jiff. These keep
// the forms humantime gave before RUSTSEC-2026-0542 marked it unmaintained, so
// a config value or log line written by an older gtunnel still reads the same.

use jiff::fmt::friendly::{Designator, SpanParser, SpanPrinter};
use jiff::{SignedDuration, Span, SpanRelativeTo, SpanRound, Timestamp, Unit};
use std::time::{Duration, SystemTime};

const PARSER: SpanParser = SpanParser::new();

/// `90s` as `1m 30s`. Days stay days rather than piling up as hours; nothing
/// larger, since a month or a year has no fixed length.
pub fn format_duration(d: Duration) -> String {
    let span = Span::try_from(d)
        .and_then(|s| s.round(SpanRound::new().largest(Unit::Day).days_are_24_hours()));
    match span {
        Ok(span) => SpanPrinter::new()
            .designator(Designator::HumanTime)
            .span_to_string(&span),
        // Past what a Span holds (centuries): plain seconds still say how long
        Err(_) => format!("{}s", d.as_secs()),
    }
}

/// `30s`, `1h30m` or `2 days`. A day counts as 24 hours: a timeout is
/// elapsed time, not a calendar step across a DST change.
pub fn parse_duration(raw: &str) -> Result<Duration, jiff::Error> {
    let span: Span = PARSER.parse_span(raw)?;
    let d: SignedDuration = span.to_duration(SpanRelativeTo::days_are_24_hours())?;
    Duration::try_from(d)
}

/// RFC 3339 in UTC to the second, `2026-10-27T09:14:02Z`, the stamp every
/// gtunnel log line starts with.
pub fn format_rfc3339_seconds(at: SystemTime) -> String {
    match Timestamp::try_from(at) {
        Ok(ts) => ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        // Only a clock set past year 9999 gets here; the epoch says so plainly
        Err(_) => Timestamp::UNIX_EPOCH
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    }
}

/// Any RFC 3339 stamp, with `Z` or a numeric offset: GCP and gcloud write both.
pub fn parse_rfc3339(stamp: &str) -> Result<SystemTime, jiff::Error> {
    SystemTime::try_from(stamp.parse::<Timestamp>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_print_in_the_units_people_type() {
        let shown = |secs: u64| format_duration(Duration::from_secs(secs));
        assert_eq!(shown(0), "0s");
        assert_eq!(shown(30), "30s");
        assert_eq!(shown(90), "1m 30s");
        assert_eq!(shown(5_400), "1h 30m");
        assert_eq!(shown(6 * 3_600), "6h");
        assert_eq!(shown(3 * 86_400 + 3_600), "3d 1h");
        assert_eq!(format_duration(Duration::from_millis(1_500)), "1s 500ms");
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
    }

    #[test]
    fn durations_parse_with_or_without_spaces_and_long_unit_names() {
        let parsed = |raw: &str| parse_duration(raw).expect(raw);
        assert_eq!(parsed("30s"), Duration::from_secs(30));
        assert_eq!(parsed("1h30m"), Duration::from_secs(5_400));
        assert_eq!(parsed("1h 30m"), Duration::from_secs(5_400));
        assert_eq!(parsed("2 min"), Duration::from_secs(120));
        assert_eq!(parsed("1.5s"), Duration::from_millis(1_500));
        assert_eq!(parsed("500ms"), Duration::from_millis(500));
        assert_eq!(parsed("1d"), Duration::from_secs(86_400));
        assert_eq!(parsed("2days"), Duration::from_secs(2 * 86_400));
        assert_eq!(parsed("1w"), Duration::from_secs(7 * 86_400));
    }

    #[test]
    fn a_duration_with_no_fixed_length_or_no_unit_is_refused() {
        for raw in ["10", "abc", "", "-1s", "1y", "1 month"] {
            assert!(parse_duration(raw).is_err(), "{raw} parsed");
        }
    }

    #[test]
    fn a_printed_duration_parses_back_to_itself() {
        for secs in [1, 59, 61, 3_599, 3_601, 86_399, 90_061] {
            let d = Duration::from_secs(secs);
            assert_eq!(parse_duration(&format_duration(d)).expect("parse"), d);
        }
    }

    #[test]
    fn stamps_print_to_the_second_and_parse_back() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_793_000_000_999);
        let shown = format_rfc3339_seconds(at);
        assert_eq!(shown, "2026-10-26T07:33:20Z");
        assert_eq!(
            parse_rfc3339(&shown).expect("parse"),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_000_000)
        );
    }

    #[test]
    fn stamps_parse_with_fractions_and_numeric_offsets() {
        let utc = parse_rfc3339("2026-10-30T12:00:00Z").expect("utc");
        assert_eq!(
            parse_rfc3339("2026-10-30T14:00:00+02:00").expect("offset"),
            utc
        );
        assert_eq!(
            parse_rfc3339("2026-10-30T12:00:00.500Z").expect("fraction"),
            utc + Duration::from_millis(500)
        );
        assert!(parse_rfc3339("2026-10-30").is_err());
        assert!(parse_rfc3339("not a stamp").is_err());
    }
}
