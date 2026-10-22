// TODO: remove once every module is wired into a command (Task 18 checks this)
#![allow(dead_code)]

mod cli;
mod config;
mod gcp;
mod state;
mod tunnel;

use anyhow::{bail, Result};
use clap::Parser;

use cli::{AccountCommands, Cli, Commands, DomainsCommands, RelayCommands, ZonesCommands};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    // Consumed by every command implementation from Task 10 onward
    let _account = args.account.as_deref();

    match args.command {
        None => bail!("not implemented: TUI"),
        Some(Commands::Init) => bail!("not implemented: init"),
        Some(Commands::Add { .. }) => bail!("not implemented: add"),
        Some(Commands::Expose { .. }) => bail!("not implemented: expose"),
        Some(Commands::Run { .. }) => bail!("not implemented: run"),
        Some(Commands::Start { .. }) => bail!("not implemented: start"),
        Some(Commands::Stop { .. }) => bail!("not implemented: stop"),
        Some(Commands::Restart { .. }) => bail!("not implemented: restart"),
        Some(Commands::Logs { .. }) => bail!("not implemented: logs"),
        Some(Commands::List) => bail!("not implemented: list"),
        Some(Commands::Delete { .. }) => bail!("not implemented: delete"),
        Some(Commands::Zones { command }) => match command {
            None => bail!("not implemented: zones list"),
            Some(ZonesCommands::Default { .. }) => bail!("not implemented: zones default"),
        },
        Some(Commands::Domains { command }) => match command {
            None => bail!("not implemented: domains list"),
            Some(DomainsCommands::Default { .. }) => bail!("not implemented: domains default"),
            Some(DomainsCommands::Add { .. }) => bail!("not implemented: domains add"),
        },
        Some(Commands::Relay { command }) => match command {
            RelayCommands::Up { .. } => bail!("not implemented: relay up"),
            RelayCommands::Down { .. } => bail!("not implemented: relay down"),
            RelayCommands::Status => bail!("not implemented: relay status"),
        },
        Some(Commands::Account { command }) => match command {
            None => bail!("not implemented: account list"),
            Some(AccountCommands::List) => bail!("not implemented: account list"),
            Some(AccountCommands::Select { .. }) | Some(AccountCommands::Default { .. }) => {
                bail!("not implemented: account select")
            }
            Some(AccountCommands::Remove { .. }) => bail!("not implemented: account remove"),
        },
        Some(Commands::Doctor { .. }) => bail!("not implemented: doctor"),
        Some(Commands::Reset { .. }) => bail!("not implemented: reset"),
        Some(Commands::Demo) => bail!("not implemented: demo"),
        Some(Commands::Update { .. }) => bail!("not implemented: update"),
    }
}
