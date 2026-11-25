//! A notification handed to the TUI that is open, instead of delivered by a
//! process that has no terminal.
//!
//! With `notifications.backend = "terminal"` the watchdog is the one that
//! notices a change, and it is never in a terminal. While a TUI is open it
//! beats a heartbeat here; the watchdog leaves its notification in a spool
//! instead of spawning a notifier, and the TUI shows it by escape sequence.
//! A notification nobody took within [`STALE_SECS`] means the TUI went away
//! first, and whoever looks next delivers it the ordinary way, so a hand-off
//! can delay an alert and cannot lose one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{Notification, Severity};

/// The TUI beats on every tick, a second apart: ten seconds is a TUI that is
/// still there and a busy one, not a closed one.
pub const FRESH_SECS: u64 = 10;
/// A watchdog pass runs every thirty seconds by default. Twice the freshness
/// window leaves a slow TUI room to take its entries first.
pub const STALE_SECS: u64 = 20;

fn beat_path(dir: &Path) -> PathBuf {
    dir.join("run").join("tui.heartbeat")
}

fn spool_dir(dir: &Path) -> PathBuf {
    dir.join("run").join("notify-spool")
}

/// The TUI is here, and will take what is left in the spool.
pub fn beat(dir: &Path, now: u64) -> Result<()> {
    let path = beat_path(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    crate::fsutil::write_atomic(&path, &format!("{now}\n"))
}

/// Whether a TUI has beaten recently enough to be handed a notification. A
/// clock that stepped puts the beat in the future; the distance is what counts.
pub fn tui_fresh(dir: &Path, now: u64) -> bool {
    std::fs::read_to_string(beat_path(dir))
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .is_some_and(|at| now.abs_diff(at) <= FRESH_SECS)
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub at: u64,
    pub severity: Severity,
    pub body: String,
    pub group: Option<String>,
}

impl Entry {
    pub fn into_notification(self) -> Notification {
        Notification {
            severity: self.severity,
            body: self.body,
            group: self.group,
        }
    }
}

/// Leaves a notification for the TUI. One file each, written whole, so a
/// reader never sees half of one.
pub fn write(dir: &Path, n: &Notification, now: u64) -> Result<()> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQUENCE: AtomicU32 = AtomicU32::new(0);
    let spool = spool_dir(dir);
    std::fs::create_dir_all(&spool)
        .with_context(|| format!("Failed to create {}", spool.display()))?;
    let name = format!(
        "{now:012}-{}-{}.json",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let entry = Entry {
        at: now,
        severity: n.severity,
        body: n.body.clone(),
        group: n.group.clone(),
    };
    crate::fsutil::write_atomic(&spool.join(name), &serde_json::to_string(&entry)?)
}

/// Takes what is in the spool and passes `keep` over each entry: the ones it
/// accepts are removed and returned oldest first, the rest are left. A file
/// that does not parse is removed, because nothing could ever deliver it.
fn take_where(dir: &Path, keep: impl Fn(&Entry) -> bool) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(spool_dir(dir)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut taken = Vec::new();
    for path in files {
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Entry>(&text).ok());
        match parsed {
            Some(entry) if !keep(&entry) => {}
            // Removed first: of two takers, the one whose remove succeeds
            // delivers, so an entry is shown once
            Some(entry) => {
                if std::fs::remove_file(&path).is_ok() {
                    taken.push(entry);
                }
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    taken
}

/// Everything left for the TUI
pub fn take_all(dir: &Path) -> Vec<Entry> {
    take_where(dir, |_| true)
}

/// What nobody took in time
pub fn take_stale(dir: &Path, now: u64) -> Vec<Entry> {
    take_where(dir, |e| now.saturating_sub(e.at) >= STALE_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(severity: Severity, body: &str) -> Notification {
        Notification::new(severity, body).about("dev/db")
    }

    #[test]
    fn a_tui_is_fresh_for_ten_seconds_after_its_beat_and_never_before_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!tui_fresh(dir.path(), 1000), "no beat, no TUI");
        beat(dir.path(), 1000).unwrap();
        assert!(tui_fresh(dir.path(), 1000));
        assert!(tui_fresh(dir.path(), 1010), "exactly the window");
        assert!(!tui_fresh(dir.path(), 1011));
        assert!(tui_fresh(dir.path(), 995), "a clock that stepped back");
        std::fs::write(beat_path(dir.path()), "garbage").unwrap();
        assert!(
            !tui_fresh(dir.path(), 1000),
            "an unreadable beat is no beat"
        );
    }

    #[test]
    fn what_is_spooled_comes_out_whole_oldest_first_and_once() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &note(Severity::Failure, "second"), 2000).unwrap();
        write(dir.path(), &note(Severity::Warning, "first"), 1999).unwrap();
        let taken = take_all(dir.path());
        let said: Vec<_> = taken.iter().map(|e| (e.at, e.body.as_str())).collect();
        assert_eq!(said, [(1999, "first"), (2000, "second")]);
        assert_eq!(taken[1].severity, Severity::Failure);
        assert_eq!(taken[1].group.as_deref(), Some("dev/db"));
        assert!(take_all(dir.path()).is_empty(), "taken once");
    }

    #[test]
    fn two_notifications_in_one_second_are_two_entries() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &note(Severity::Info, "a"), 5).unwrap();
        write(dir.path(), &note(Severity::Info, "b"), 5).unwrap();
        assert_eq!(take_all(dir.path()).len(), 2);
    }

    #[test]
    fn only_what_nobody_took_in_time_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &note(Severity::Failure, "old"), 100).unwrap();
        write(dir.path(), &note(Severity::Failure, "new"), 115).unwrap();
        // 19 s and 4 s old: both still the TUI's
        assert!(take_stale(dir.path(), 119).is_empty());
        // 20 s old is stale, 5 s old is not
        let stale = take_stale(dir.path(), 120);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].body, "old");
        // The one that was not stale is still there for the TUI
        assert_eq!(take_all(dir.path())[0].body, "new");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_removed_and_the_rest_still_come_out() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &note(Severity::Info, "fine"), 7).unwrap();
        std::fs::write(
            spool_dir(dir.path()).join("000000000001-1-1.json"),
            "{not json",
        )
        .unwrap();
        let taken = take_all(dir.path());
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].body, "fine");
        assert!(std::fs::read_dir(spool_dir(dir.path()))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn an_entry_becomes_the_notification_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let n = note(Severity::Recovered, "db is healthy again");
        write(dir.path(), &n, 9).unwrap();
        assert_eq!(take_all(dir.path()).remove(0).into_notification(), n);
    }

    #[test]
    fn a_missing_spool_is_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(take_all(dir.path()).is_empty());
        assert!(take_stale(dir.path(), 1).is_empty());
    }
}
