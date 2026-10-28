//! The command dispatch, in the library crate so the binary stays a shell
//! around it: `main` owns the process (the runtime, the exit code, what an
//! error prints) and this owns what each subcommand calls.

use anyhow::Result;

use crate::cli::{
    AccountCommands, Cli, Commands, DomainsCommands, RelayCommands, WatchCommands, ZonesCommands,
};
pub use crate::commands::completions::answer_if_asked;
use crate::{commands, config, doctor, ephemeral, gcp, loglevel, mcp, tui, update};

/// Prints a failure the way the caller asked for it and ends the process
/// with the code its kind maps to.
pub fn exit_with(e: &anyhow::Error, json: bool) -> ! {
    // The command already printed this failure; printing it again would
    // double it on stderr
    if e.downcast_ref::<commands::access::Printed>().is_some() {
        std::process::exit(1);
    }
    let kind = crate::error::kind_of(e);
    if json {
        println!("{}", crate::error::to_json(e));
    } else {
        eprintln!("Error: {e:?}");
    }
    std::process::exit(kind.exit_code());
}

// A bare `gtunnel` on a machine that has never been set up walks through `init`
// instead of ending in an error. It needs a person to answer, so a script or a
// pipe keeps the plain error, and it needs the file to be absent: one that
// exists and fails to load is reported as such, never overwritten.
fn first_run_needed(no_command: bool, config_exists: bool, interactive: bool) -> bool {
    no_command && !config_exists && interactive
}

pub async fn run(args: Cli) -> Result<()> {
    // Before anything builds an argv or calls gcloud, and before the config
    // is needed for anything else: a bad GTUNNEL_LOG_LEVEL should fail here,
    // naming the variable, rather than somewhere downstream. A config that
    // does not load yet - `gtunnel init` has not run - is not an error at
    // this point, so its contribution falls back to the default.
    let from_file = config::load_config()
        .map(|c| c.log_level)
        .unwrap_or_default();
    // The override is kept apart from the resolved level: it is the half of
    // the answer that stays true for a session that outlives this moment,
    // while `config.toml` may be edited under a running TUI.
    let over = config::resolve_log_override(
        args.log_level,
        std::env::var("GTUNNEL_LOG_LEVEL").ok().as_deref(),
    )?;
    loglevel::set_current(loglevel::resolve_effective(over, from_file), over);

    let account = args.account.as_deref();
    // The hint reads a cache file; `update` does the real check itself
    let quiet = matches!(
        args.command,
        None | Some(Commands::Update { .. } | Commands::Completions { .. })
    );

    // Ahead of the gate below, which needs an account to exist; a failure or an
    // interrupted prompt returns here, so the TUI never starts on a half setup
    if first_run_needed(
        args.command.is_none(),
        config::config_path()?.exists(),
        gcp::gcloud::is_interactive(),
    ) {
        println!("gtunnel is not set up yet. Pick a gcloud configuration to use:");
        commands::account::cmd_init().await?;
    }

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
            ttl,
        }) => {
            let ttl = ttl.as_deref().map(ephemeral::parse_ttl).transpose()?;
            ephemeral::cmd_run(
                account,
                &run_args,
                zone.as_deref(),
                local_port,
                public,
                domain.as_deref(),
                ttl,
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
        Some(Commands::List { json }) => commands::access::cmd_list(account, json).await,
        Some(Commands::Delete { name }) => commands::access::cmd_delete(account, &name).await,
        Some(Commands::Rename { old, new }) => {
            commands::relocate::cmd_rename(account, &old, &new).await
        }
        Some(Commands::Move { name, to, rename }) => {
            commands::relocate::cmd_move(&name, account, &to, rename.as_deref()).await
        }
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
            None => commands::account::cmd_account_list(false),
            Some(AccountCommands::List { json }) => commands::account::cmd_account_list(json),
            Some(AccountCommands::Select { name }) | Some(AccountCommands::Default { name }) => {
                commands::account::cmd_account_select(&name)
            }
            Some(AccountCommands::Add {
                name,
                configuration,
                project,
                zone,
                credential_file,
            }) => {
                let opts = commands::account::AddOptions {
                    project,
                    zone,
                    credential_file,
                };
                commands::account::cmd_account_add(&configuration, &name, opts).await
            }
            Some(AccountCommands::Edit {
                name,
                project,
                zone,
                credential_file,
            }) => commands::account::cmd_account_edit(
                &name,
                project.as_deref(),
                zone.as_deref(),
                credential_file.as_deref(),
            ),
            Some(AccountCommands::Remove { name, yes }) => {
                commands::account::cmd_account_remove(&name, yes)
            }
        },
        Some(Commands::Watch {
            command,
            once,
            verbose,
        }) => match command {
            Some(WatchCommands::Install) => commands::watch::cmd_watch_install().await,
            Some(WatchCommands::Uninstall) => commands::watch::cmd_watch_uninstall().await,
            Some(WatchCommands::Status) => commands::watch::cmd_watch_status().await,
            None => commands::watch::cmd_watch(once, verbose).await,
        },
        Some(Commands::Doctor {
            fix,
            yes,
            all,
            json,
        }) => doctor::cmd_doctor(account, fix, yes, all, json).await,
        Some(Commands::Reset { yes }) => commands::maint::cmd_reset(yes).await,
        Some(Commands::Demo) => tui::run_demo().await,
        Some(Commands::Update { check }) => update::cmd_update(check).await,
        Some(Commands::Mcp { validate }) => {
            if validate {
                mcp::validator::run_validate()
            } else {
                mcp::server::run().await
            }
        }
        Some(Commands::Completions { shell }) => commands::completions::cmd_completions(shell),
        Some(Commands::Supervise {
            port,
            ready_file,
            conns,
            auth_marker,
            argv,
        }) => commands::supervise::run(argv, port, ready_file, conns, auth_marker).await,
    };

    if !quiet {
        update::maybe_print_update_hint();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::first_run_needed;

    #[test]
    fn first_run_needed_is_true_for_a_bare_command_with_no_file_on_a_terminal() {
        assert!(first_run_needed(true, false, true));
    }

    #[test]
    fn first_run_needed_is_false_when_the_file_exists() {
        // A file that exists but does not parse must reach the load error, never
        // `init`: saving over it would delete every account it holds
        assert!(!first_run_needed(true, true, true));
    }

    #[test]
    fn first_run_needed_is_false_without_a_terminal() {
        // A prompt nobody can answer would hang a script or a cron job
        assert!(!first_run_needed(true, false, false));
    }

    #[test]
    fn first_run_needed_is_false_for_a_subcommand() {
        assert!(!first_run_needed(false, false, true));
    }
}
