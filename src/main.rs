// TODO: remove once every module is wired into a command (Task 18 checks this)
#![allow(dead_code)]

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
mod tunnel;

use anyhow::{bail, Result};
use clap::Parser;

use cli::{AccountCommands, Cli, Commands, DomainsCommands, RelayCommands, ZonesCommands};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    let account = args.account.as_deref();

    match args.command {
        None => bail!("not implemented: TUI"),
        Some(Commands::Init) => commands::account::cmd_init().await,
        Some(Commands::Add {
            name,
            target,
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
        Some(Commands::Reset { .. }) => bail!("not implemented: reset"),
        Some(Commands::Demo) => bail!("not implemented: demo"),
        Some(Commands::Update { .. }) => bail!("not implemented: update"),
    }
}
