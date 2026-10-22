use anyhow::Result;

use crate::commands::confirm;
use crate::config::{config_dir, load_config};
use crate::daemon;
use crate::ephemeral::list_ephemeral;
use crate::state::TunnelState;

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

    // An ephemeral run holds its ports until it exits, and its record is about
    // to be deleted with the rest of the directory
    for (record, alive) in list_ephemeral().unwrap_or_default() {
        if alive {
            println!("  stopping run {} (pid {})", record.name, record.pid);
            #[cfg(unix)]
            unsafe {
                libc::kill(record.pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }

    std::fs::remove_dir_all(&dir)?;
    println!("Removed {}", dir.display());
    println!(
        "Cloud resources were not deleted. Run gtunnel relay down first if you want them gone."
    );
    Ok(())
}
