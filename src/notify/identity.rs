//! What makes a rich notification look like gtunnel's and not a stranger's:
//! its icon, and the id of the banner it replaces.
//!
//! Both are used only with `notifications.style = "rich"`, and only where the
//! notifier takes them: the freedesktop service does, `osascript` has no way
//! to carry either.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The icon, drawn by hand so nothing is fetched or decoded to show it
const ICON: &str = include_str!("gtunnel.svg");

/// Writes the icon where a notification server can read it, once, and returns
/// the path. A server reads the file itself, so it has to be on disk and stay
/// there; a copy that differs from this one (an older gtunnel's) is replaced.
pub fn ensure_icon_in(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("gtunnel.svg");
    if std::fs::read_to_string(&path).is_ok_and(|text| text == ICON) {
        return Ok(path);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    crate::fsutil::write_atomic(&path, ICON)?;
    Ok(path)
}

/// Where the icon lives: the user's data directory, as an application's would
pub fn ensure_icon() -> Result<PathBuf> {
    let base = dirs::data_dir().context("no data directory")?;
    ensure_icon_in(&base.join("gtunnel"))
}

/// A file name for a group: readable, and distinct for groups that differ only
/// in the characters a file name cannot hold (`dev/db` and `dev_db`).
fn id_file(dir: &Path, group: &str) -> PathBuf {
    let readable: String = group
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    // FNV-1a, because the name must be the same in every process and run
    let hash = group.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    dir.join("run")
        .join("notify-ids")
        .join(format!("{readable}-{hash:016x}"))
}

/// The id of the last banner shown for `group`, or 0, which the specification
/// reads as "a new one". An id the server has forgotten is a new banner too.
pub fn last_id_in(dir: &Path, group: &str) -> u32 {
    std::fs::read_to_string(id_file(dir, group))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

pub fn remember_id_in(dir: &Path, group: &str, id: u32) -> Result<()> {
    let path = id_file(dir, group);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    crate::fsutil::write_atomic(&path, &format!("{id}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_an_svg_written_once_and_replaced_when_it_differs() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_icon_in(dir.path()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("<svg "), "{text}");
        assert!(text.contains("</svg>"));
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(ensure_icon_in(dir.path()).unwrap(), path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            written,
            "an icon that is already there is not written again"
        );
        std::fs::write(&path, "an older gtunnel's").unwrap();
        ensure_icon_in(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn a_group_remembers_its_last_banner_and_a_stranger_has_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(last_id_in(dir.path(), "dev/db"), 0);
        remember_id_in(dir.path(), "dev/db", 41).unwrap();
        assert_eq!(last_id_in(dir.path(), "dev/db"), 41);
        remember_id_in(dir.path(), "dev/db", 42).unwrap();
        assert_eq!(last_id_in(dir.path(), "dev/db"), 42, "the newest wins");
        assert_eq!(last_id_in(dir.path(), "dev/api"), 0);
    }

    #[test]
    fn groups_that_differ_only_in_punctuation_do_not_share_a_banner() {
        let dir = tempfile::tempdir().unwrap();
        remember_id_in(dir.path(), "dev/db", 1).unwrap();
        remember_id_in(dir.path(), "dev_db", 2).unwrap();
        assert_eq!(last_id_in(dir.path(), "dev/db"), 1);
        assert_eq!(last_id_in(dir.path(), "dev_db"), 2);
        // Whatever the characters, the name stays a file name
        let name = id_file(dir.path(), "../../etc/passwd\0 x");
        assert_eq!(name.parent(), id_file(dir.path(), "a").parent());
    }

    #[test]
    fn an_unreadable_id_is_a_new_banner() {
        let dir = tempfile::tempdir().unwrap();
        remember_id_in(dir.path(), "g", 5).unwrap();
        std::fs::write(id_file(dir.path(), "g"), "not a number").unwrap();
        assert_eq!(last_id_in(dir.path(), "g"), 0);
    }
}
