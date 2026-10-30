// The whole module is CLI output, so printing is its job
#![allow(clippy::print_stdout, clippy::print_stderr)]

//! The command dispatch, in the library crate so the binary stays a shell
//! around it: `main` owns the process (the runtime, the exit code, what an
//! error prints) and this owns what each subcommand calls.

use anyhow::Result;

use crate::cli::{
    AccountCommands, Cli, Commands, DomainsCommands, RelayCommands, WatchCommands, ZonesCommands,
};
pub use crate::commands::completions::answer_if_asked;
use crate::{commands, config, doctor, ephemeral, gcp, loglevel, mcp, settings, tui, update};

/// Ends the process quietly when the reader of stdout or stderr has gone, as
/// `head -1` or a pager quit with `q` does: the reader chose to stop, so there
/// is nothing to report. Rust ignores SIGPIPE and `println!` panics on the
/// resulting EPIPE, and restoring the default signal instead would let a write
/// to a closed tunnel socket kill the supervisor. Every other panic goes on to
/// the default hook.
pub fn exit_quietly_on_broken_pipe() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if is_broken_pipe_panic(message) {
            std::process::exit(0);
        }
        default(info);
    }));
}

// The message std panics with, `failed printing to stdout: <error>`. The
// error's text comes from the platform, so it is built here rather than spelled
fn is_broken_pipe_panic(message: &str) -> bool {
    let epipe = std::io::Error::from_raw_os_error(libc::EPIPE).to_string();
    ["stdout", "stderr"]
        .iter()
        .any(|stream| message == format!("failed printing to {stream}: {epipe}"))
}

/// Prints a failure the way the caller asked for it and ends the process
/// with the code its kind maps to.
pub fn exit_with(e: &anyhow::Error, json: bool) -> ! {
    // The command already said why and chose its own status
    if let Some(exited) = e.downcast_ref::<crate::error::Exited>() {
        std::process::exit(exited.0);
    }
    let kind = crate::error::kind_of(e);
    // The command already printed this failure; printing it again would
    // double it on stderr. Its kind still decides the code: a start that
    // stopped for expired credentials is `auth`, not a bug
    if e.downcast_ref::<commands::access::Printed>().is_some() {
        std::process::exit(kind.exit_code());
    }
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

/// Whether a command brings installed services up to date first. Not the
/// ones that are themselves a service or run unattended, nor the ones that
/// must work before anything is set up.
fn wants_refresh(command: &Option<Commands>) -> bool {
    !matches!(
        command,
        Some(
            Commands::Supervise { .. }
                | Commands::Watch { .. }
                | Commands::Mcp { .. }
                | Commands::Completions { .. }
                | Commands::Update { .. }
                | Commands::Init
        )
    )
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

    // The same early failure for GTUNNEL_RESTART_*, against the file's own
    // values, so a typo is caught by whichever command is typed next and not
    // only when a tunnel is failing. The supervisor is exempt: it falls back
    // to the defaults, because dying here would restart it into this error.
    // The same goes for the settings of `settings.rs`: the file is checked at
    // load, and here the environment is checked against it, so a bad
    // GTUNNEL_START_DEADLINE fails by name before a command half-runs.
    if !matches!(args.command, Some(Commands::Supervise { .. })) {
        let cfg = config::load_config().unwrap_or_default();
        cfg.restart_budget()?;
        settings::resolve(&cfg, &|name| std::env::var(name).ok())?;
    }

    // An upgrade replaces the binary and leaves every installed service on the
    // arguments it was written with, so the first command after one brings
    // them up to date. Not the commands that are themselves a service or run
    // unattended (a reload there would restart the job that is running it),
    // nor the ones that must work before anything is set up.
    if wants_refresh(&args.command) {
        crate::daemon::refresh::refresh_installed().await;
    }

    let account = args.account.as_deref();
    // The hint reads a cache file; `update` does the real check itself
    let quiet = matches!(
        args.command,
        None | Some(
            Commands::Update { .. } | Commands::Completions { .. } | Commands::Reset { .. }
        )
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
            crash_marker,
            argv,
        }) => {
            commands::supervise::run(argv, port, ready_file, conns, auth_marker, crash_marker).await
        }
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
    fn only_a_print_to_a_closed_pipe_is_a_broken_pipe_panic() {
        let epipe = std::io::Error::from_raw_os_error(libc::EPIPE).to_string();
        let quiet = super::is_broken_pipe_panic;
        assert!(quiet(&format!("failed printing to stdout: {epipe}")));
        assert!(quiet(&format!("failed printing to stderr: {epipe}")));
        // A full disk or a closed descriptor is a real failure and keeps its panic
        let enospc = std::io::Error::from_raw_os_error(libc::ENOSPC).to_string();
        assert!(!quiet(&format!("failed printing to stdout: {enospc}")));
        assert!(!quiet("called `Option::unwrap()` on a `None` value"));
        assert!(!quiet(&epipe));
    }

    #[test]
    fn only_commands_that_are_not_services_or_setup_refresh_definitions() {
        use crate::cli::Cli;
        use clap::Parser;
        let wants = |argv: &[&str]| {
            let mut v = vec!["gtunnel"];
            v.extend_from_slice(argv);
            super::wants_refresh(&Cli::try_parse_from(v).expect("parses").command)
        };
        assert!(wants(&[]));
        assert!(wants(&["list"]));
        assert!(wants(&["start", "k8s"]));
        for quiet in [
            &["init"][..],
            &["completions", "zsh"],
            &["watch", "--once"],
            &["mcp"],
            &["update", "--check"],
        ] {
            assert!(!wants(quiet), "{quiet:?}");
        }
    }

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
