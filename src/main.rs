mod cli;
mod commands;
mod config;
mod daemon;
mod discovery;
mod doctor;
mod ephemeral;
mod expose;
mod gcp;
mod notify;
mod probe;
mod state;
mod tui;
mod tunnel;
mod update;

use anyhow::Result;
use clap::Parser;

use cli::{AccountCommands, Cli, Commands, DomainsCommands, RelayCommands, ZonesCommands};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    let account = args.account.as_deref();
    // The hint reads a cache file; `update` does the real check itself
    let quiet = matches!(args.command, None | Some(Commands::Update { .. }));

    // Before the TUI takes the screen or any command touches GCP, so a login
    // prints to a normal terminal and a missing one ends the process here
    if args.needs_gcp_auth() {
        let (_, acct) = commands::resolve(account)?;
        gcp::gcloud::ensure_logged_in(&acct).await?;
    }

    let result = match args.command {
        None => tui::run_tui(account).await,
        Some(Commands::Init) => commands::account::cmd_init().await,
        Some(Commands::Add {
            name,
            target,
            via,
            zone,
            local_port,
            dest_group,
            region,
            network,
            start,
        }) => {
            commands::access::cmd_add(
                account,
                &name,
                &target,
                via.as_deref(),
                zone.as_deref(),
                local_port,
                dest_group.as_deref(),
                region.as_deref(),
                network.as_deref(),
                start,
            )
            .await
        }
        Some(Commands::Expose {
            name,
            target,
            domain,
            private,
            start,
        }) => {
            commands::expose::cmd_expose(account, &name, &target, domain.as_deref(), private, start)
                .await
        }
        Some(Commands::Run {
            args: run_args,
            zone,
            local_port,
            public,
            domain,
        }) => {
            ephemeral::cmd_run(
                account,
                &run_args,
                zone.as_deref(),
                local_port,
                public,
                domain.as_deref(),
            )
            .await
        }
        Some(Commands::Start { name }) => commands::access::cmd_start(account, &name).await,
        Some(Commands::Stop { name }) => commands::access::cmd_stop(account, &name).await,
        Some(Commands::Restart { name }) => commands::access::cmd_restart(account, &name).await,
        Some(Commands::Autostart { name, on, off }) => {
            let wanted = if on {
                Some(true)
            } else if off {
                Some(false)
            } else {
                None
            };
            commands::access::cmd_auto_start(account, &name, wanted).await
        }
        Some(Commands::Logs {
            name,
            follow,
            lines,
        }) => commands::access::cmd_logs(account, &name, follow, lines).await,
        Some(Commands::List) => commands::access::cmd_list(account).await,
        Some(Commands::Delete { name }) => commands::access::cmd_delete(account, &name).await,
        Some(Commands::Zones { command }) => match command {
            None => commands::access::cmd_zones(account, None).await,
            Some(ZonesCommands::Default { zone }) => {
                commands::access::cmd_zones(account, Some(&zone)).await
            }
        },
        Some(Commands::Domains { command }) => match command {
            None => commands::expose::cmd_domains(account, None, None).await,
            Some(DomainsCommands::Default { domain }) => {
                commands::expose::cmd_domains(account, Some(&domain), None).await
            }
            Some(DomainsCommands::Add { managed_zone }) => {
                commands::expose::cmd_domains(account, None, Some(&managed_zone)).await
            }
        },
        Some(Commands::Relay { command }) => match command {
            RelayCommands::Up {
                zone,
                network,
                managed_zone,
                yes,
            } => {
                commands::expose::cmd_relay_up(
                    account,
                    zone.as_deref(),
                    &network,
                    managed_zone.as_deref(),
                    yes,
                )
                .await
            }
            RelayCommands::Down { force, yes } => {
                commands::expose::cmd_relay_down(account, force, yes).await
            }
            RelayCommands::Status => commands::expose::cmd_relay_status(account).await,
        },
        Some(Commands::Account { command }) => match command {
            None | Some(AccountCommands::List) => commands::account::cmd_account_list(),
            Some(AccountCommands::Select { name }) | Some(AccountCommands::Default { name }) => {
                commands::account::cmd_account_select(&name)
            }
            Some(AccountCommands::Remove { name, yes }) => {
                commands::account::cmd_account_remove(&name, yes)
            }
        },
        Some(Commands::Doctor { fix, yes, all }) => {
            doctor::cmd_doctor(account, fix, yes, all).await
        }
        Some(Commands::Reset { yes }) => commands::maint::cmd_reset(yes).await,
        Some(Commands::Demo) => tui::run_demo().await,
        Some(Commands::Update { check }) => update::cmd_update(check).await,
    };

    if !quiet {
        update::maybe_print_update_hint();
    }
    result
}
