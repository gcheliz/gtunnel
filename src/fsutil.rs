// Every file gtunnel writes goes through here, so the three guarantees are
// stated once: the write is atomic, the file is owner-only (the contents name
// accounts, projects and hosts), and a read-modify-write cycle can be
// serialised across processes with an advisory lock.

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

// Long enough for another gtunnel to finish a save, short enough that a
// wedged holder is reported rather than waited on
pub const LOCK_DEADLINE: Duration = Duration::from_secs(5);

// A holder releases and re-takes the lock within microseconds, so a waiter
// that always sleeps the full cap almost never wakes to a free lock. Starting
// short and doubling lets a waiter in after a short hold, and the cap bounds
// polling during a long one
const FIRST_POLL: Duration = Duration::from_millis(1);
const MAX_POLL: Duration = Duration::from_millis(20);

fn next_poll(wait: Duration) -> Duration {
    (wait * 2).min(MAX_POLL)
}

fn open_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Writes to a sibling temp file and renames it into place. Rename is atomic
/// within a filesystem and truncate-then-write is not, so a write that fails
/// partway leaves the previous file whole instead of an empty one that reads
/// as "nothing configured".
///
/// The temp name carries the pid: `gtunnel watch` in a terminal and the
/// agent's `watch --once` can write the same file at once, and one fixed name
/// would let the second writer truncate the first one's temp before either
/// rename.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let temp = path.with_extension(format!("{}.new", std::process::id()));
    let written = open_private(&temp).and_then(|mut file| {
        file.set_len(0)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("Failed to write {}", temp.display()));
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("Failed to replace {}", path.display()));
    }
    Ok(())
}

/// Holds an advisory lock on a file until dropped. The lock is on a file of
/// its own, never on the data file, because `write_atomic` replaces the data
/// file's inode and a lock on the old inode would protect nothing.
#[derive(Debug)]
pub struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        // The kernel releases the lock when the descriptor closes anyway
        let _ = self.0.unlock();
    }
}

/// Polls rather than blocks, so a holder that never lets go becomes an error
/// naming the lock file instead of a frozen TUI.
pub fn lock_exclusive(path: &Path, deadline: Duration) -> Result<Lock> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    let file = open_private(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let started = Instant::now();
    let mut wait = FIRST_POLL;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Lock(file)),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("Failed to lock {}", path.display()));
            }
        }
        if started.elapsed() >= deadline {
            bail!("{} is held by another gtunnel process", path.display());
        }
        std::thread::sleep(wait);
        wait = next_poll(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_atomic_write_leaves_the_old_file_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tunnels.toml");
        std::fs::write(&path, "before").unwrap();
        // A directory where the temp file should go makes the write fail
        let blocker = path.with_extension(format!("{}.new", std::process::id()));
        std::fs::create_dir(&blocker).unwrap();
        assert!(write_atomic(&path, "after").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
    }

    #[cfg(unix)]
    #[test]
    fn an_atomic_write_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tunnels.toml");
        write_atomic(&path, "x").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn update_gives_up_after_the_deadline_and_names_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("tunnels.lock");
        let _held = lock_exclusive(&lock_path, LOCK_DEADLINE).unwrap();
        let err = lock_exclusive(&lock_path, Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("tunnels.lock"), "{err}");
    }

    #[test]
    fn the_poll_interval_starts_short_and_doubles_to_a_cap() {
        let mut wait = FIRST_POLL;
        let mut seen = vec![wait];
        for _ in 0..8 {
            wait = next_poll(wait);
            seen.push(wait);
        }
        let ms: Vec<u128> = seen.iter().map(Duration::as_millis).collect();
        assert_eq!(ms, [1, 2, 4, 8, 16, 20, 20, 20, 20]);
    }
}
