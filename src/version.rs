//! One reader for version text, shared by `update` and the service refresh, so
//! the two cannot disagree about what `0.3.21-rc.1` means.

pub(crate) type Version = (u64, u64, u64);

/// A version as three numbers, so `0.2.100` is above `0.2.67`. A leading `v`,
/// a pre-release and a build suffix are ignored, and anything that is not
/// three numbers is `None`: the refresh reads that as unstamped and overwrites
/// it, and `update` reads it as not newer.
pub(crate) fn parse_version(text: &str) -> Option<Version> {
    let core = text.trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(parse_version("0.2.67") < parse_version("0.2.100"));
        assert!(parse_version("1.0.0") > parse_version("0.99.99"));
    }

    #[test]
    fn a_pre_release_or_build_suffix_reads_as_its_release() {
        assert_eq!(parse_version("1.2.3-rc.1"), parse_version("1.2.3"));
        assert_eq!(parse_version("1.2.3+build"), parse_version("1.2.3"));
        assert_eq!(parse_version("v0.3.21-rc.1"), Some((0, 3, 21)));
    }

    #[test]
    fn a_tag_with_a_leading_v_reads_like_one_without() {
        assert_eq!(parse_version("v0.1.0"), Some((0, 1, 0)));
    }

    #[test]
    fn text_that_is_not_three_numbers_is_unreadable() {
        assert_eq!(parse_version("garbage"), None);
        assert_eq!(parse_version("main"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version(""), None);
    }
}
