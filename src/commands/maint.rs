// The whole module is CLI output, so printing is its job
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::commands::confirm;
use crate::config::{config_dir, load_config};
use crate::daemon;
use crate::ephemeral::list_ephemeral;
use crate::state::TunnelState;

// A run tears down its route and removes its record after gcloud exits; a
// record without an owner cannot say when that is done, so it gets the settle
const RUN_STOP_WAIT: Duration = Duration::from_secs(10);
const RUN_SETTLE: Duration = Duration::from_secs(1);

// Removes everything gtunnel put on this machine, and nothing it put in GCP
pub async fn cmd_reset(assume_yes: bool) -> Result<()> {
    let dir = config_dir()?;
    if !dir.exists() {
        println!("Nothing to remove: {} does not exist", dir.display());
        return Ok(());
    }

    // A file that cannot be parsed still gets deleted, so its services would be
    // left running with nothing left to stop them by: say so instead of
    // reporting zero
    let state = match TunnelState::load() {
        Ok(state) => state,
        Err(e) => {
            eprintln!("Could not read the tunnel list, so its services cannot be removed: {e:#}");
            eprintln!("Remove them by hand after this, or fix the file and run reset again.");
            TunnelState::default()
        }
    };
    let accounts = match load_config() {
        Ok(config) => config.accounts.len(),
        Err(e) => {
            eprintln!("Could not read the account list: {e:#}");
            0
        }
    };

    // An ephemeral run holds its ports until it exits, and its record is about
    // to be deleted with the rest of the directory. Read before anything is
    // changed: runs that cannot be listed cannot be stopped, and deleting
    // their records would leave nothing to find them by.
    let runs =
        list_ephemeral().context("Could not list the running tunnels, so nothing was removed")?;

    println!(
        "This removes {} local tunnel(s), {accounts} account(s) and {}",
        state.tunnels.len(),
        dir.display()
    );
    if !confirm("Remove all local gtunnel configuration?", assume_yes)? {
        println!("Cancelled");
        return Ok(());
    }

    for tunnel in &state.tunnels {
        let label = daemon::platform_label(&tunnel.account_name, &tunnel.name);
        let _ = daemon::stop(&label).await;
        if let Err(e) = daemon::uninstall(&label).await {
            eprintln!("  could not remove the service for {}: {e:#}", tunnel.name);
        }
    }

    let live: Vec<_> = runs
        .into_iter()
        .filter_map(|(record, alive)| alive.then_some(record))
        .collect();
    for record in &live {
        println!("  stopping run {} (pid {})", record.name, record.pid);
        record.terminate();
    }
    // A dying run removes its own record, so it writes into the directory as
    // it goes: deleting under it left the directory behind or failed with
    // "Directory not empty". Wait for each to finish first.
    for record in &live {
        if !record.wait_for_handover(RUN_STOP_WAIT, RUN_SETTLE).await {
            bail!(
                "Run {} (pid {}) did not stop within {}s, so {} was kept. Stop it, then run \
                 reset again",
                record.name,
                record.pid,
                RUN_STOP_WAIT.as_secs(),
                dir.display()
            );
        }
    }

    std::fs::remove_dir_all(&dir)?;
    println!("Removed {}", dir.display());
    println!(
        "Cloud resources were not deleted. Run gtunnel relay down first if you want them gone."
    );
    Ok(())
}
