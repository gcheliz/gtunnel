//! One lock per account's relay, so a teardown and a route being built cannot
//! overlap. It is its own file and not `tunnels.lock`: that one is global to
//! `tunnels.toml`, held for a read-modify-write and never across a network
//! call, while a teardown awaits GCP for minutes and would freeze every other
//! writer, the TUI included.

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::GtunnelError;
use crate::fsutil::{lock_deadline, lock_exclusive, Lock};

#[cfg(test)]
thread_local! {
    // Tests never reach the real config directory; one that needs a lock of
    // its own sets this, and the rest get one per thread under the temp dir
    pub static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn lock_path(account: &str) -> Result<PathBuf> {
    #[cfg(test)]
    {
        // Per thread, because tests run in parallel and a lock shared between
        // two of them would make one refuse the other
        let dir = TEST_DIR.with(|d| d.borrow().clone()).unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "gtunnel-test-relay-locks-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ))
        });
        Ok(dir.join(format!("relay-{account}.lock")))
    }
    #[cfg(not(test))]
    Ok(crate::config::config_dir()?.join(format!("relay-{account}.lock")))
}

fn is_held(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("is held by another")
}

/// Taken around the step that builds a route or a relay resource. It does not
/// wait: a relay being deleted is not coming back, so waiting only delays the
/// same refusal.
pub fn route_guard(account: &str) -> Result<Lock> {
    match lock_exclusive(&lock_path(account)?, Duration::ZERO) {
        Ok(lock) => Ok(lock),
        Err(e) if is_held(&e) => Err(GtunnelError::usage(format!(
            "the relay for '{account}' is being torn down; try again after relay down finishes"
        ))
        .into()),
        Err(e) => Err(e),
    }
}

/// Held by `relay down` from its first check to its last delete. It waits the
/// usual lock deadline, so a route that is mid-build finishes first.
pub fn teardown_guard(account: &str) -> Result<Lock> {
    match lock_exclusive(&lock_path(account)?, lock_deadline()) {
        Ok(lock) => Ok(lock),
        Err(e) if is_held(&e) => Err(GtunnelError::usage(format!(
            "a route or `relay up` for '{account}' is still being built; try again in a moment"
        ))
        .into()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_teardown_refuses_a_route_with_the_relay_name_in_the_message() {
        let dir = tempfile::tempdir().unwrap();
        TEST_DIR.with(|d| *d.borrow_mut() = Some(dir.path().to_path_buf()));
        let _teardown = teardown_guard("dev").unwrap();
        let err = route_guard("dev").unwrap_err();
        assert_eq!(
            err.to_string(),
            "the relay for 'dev' is being torn down; try again after relay down finishes"
        );
        // Another account's relay is not in the way
        assert!(route_guard("prod").is_ok());
    }
}
