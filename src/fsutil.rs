// Every file gtunnel writes goes through here, so the three guarantees are
// stated once: the write is atomic, the file is owner-only (the contents name
// accounts, projects and hosts), and a read-modify-write cycle can be
// serialised across processes with an advisory lock.

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a writer waits for the lock another gtunnel holds: `gcp.lock_deadline`,
/// read at the wait and not cached, because the supervisor and the TUI outlive
/// an edit of the file.
pub fn lock_deadline() -> Duration {
    crate::settings::current().gcp_lock_deadline
}

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
/// The temp name carries the pid and a process-wide counter: `gtunnel watch`
/// in a terminal and the agent's `watch --once` can write the same file at
/// once, and so can two threads of one process. A pid alone is unique per
/// process, not per writer, so the second writer would truncate the first
/// one's temp before either rename.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("{}.{n}.new", std::process::id()));
    write_via(path, text, &temp)
}

fn write_via(path: &Path, text: &str, temp: &Path) -> Result<()> {
    let written = open_private(temp).and_then(|mut file| {
        file.set_len(0)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(temp);
        return Err(e).with_context(|| format!("Failed to write {}", temp.display()));
    }
    if let Err(e) = std::fs::rename(temp, path) {
        let _ = std::fs::remove_file(temp);
        return Err(e).with_context(|| format!("Failed to replace {}", path.display()));
    }
    sync_parent(path);
    Ok(())
}

// The rename is durable only once the directory entry is, so a power cut right
// after it can otherwise bring back the old file. The new file is already
// whole and in place, so a failure here (a filesystem that cannot fsync a
// directory) is not worth failing the write for
pub(crate) fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(handle) = File::open(dir) {
            let _ = handle.sync_all();
        }
    }
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
        let blocker = path.with_extension("blocked.new");
        std::fs::create_dir(&blocker).unwrap();
        assert!(write_via(&path, "after", &blocker).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
    }

    #[test]
    fn two_threads_writing_one_path_both_succeed_and_leave_one_whole_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");
        let values = ["a".repeat(64 * 1024), "b".repeat(64 * 1024)];
        for _ in 0..50 {
            std::thread::scope(|scope| {
                let handles: Vec<_> = values
                    .iter()
                    .map(|v| scope.spawn(|| write_atomic(&path, v)))
                    .collect();
                for h in handles {
                    h.join().unwrap().unwrap();
                }
            });
            let got = std::fs::read_to_string(&path).unwrap();
            assert!(values.contains(&got), "a torn write: {} bytes", got.len());
        }
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "a temp file was left behind");
    }

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
        let _held = lock_exclusive(&lock_path, lock_deadline()).unwrap();
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
