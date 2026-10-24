use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "gtunnel")]
#[command(about = "TUI-first CLI for GCP IAP tunnels and public URLs", long_about = None)]
#[command(version)]
pub struct Cli {
    // Use a specific account (overrides the default selected account)
    #[arg(
        long,
        global = true,
        help = "Use a specific account (overrides the default)"
    )]
    pub account: Option<String>,

    // How much the tunnels and the gcloud calls say. Beats GTUNNEL_LOG_LEVEL
    // and the config file, so a one-off `--log-level debug` answers "why is
    // this tunnel silent" without editing a file and putting it back after.
    #[arg(
        long,
        global = true,
        value_name = "LEVEL",
        help = "How much to log: quiet, info or debug (default: info)"
    )]
    pub log_level: Option<crate::loglevel::LogLevel>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Commands {
    // Set up a gtunnel account from a gcloud configuration
    #[command(about = "Set up a gtunnel account from a gcloud configuration")]
    Init,

    // Add a persistent Access tunnel (IAP TCP forward to localhost)
    //
    // Examples:
    //   gtunnel add db my-vm:5432 -z europe-west1-b --start
    //   gtunnel add pg 10.0.0.5:5432 --dest-group prod --region europe-west1 --network default
    //   gtunnel add k8s 10.0.0.10:443 --via bastion --local-port 8443
    #[command(about = "Add a persistent Access tunnel (IAP TCP forward to localhost)")]
    Add {
        // Tunnel name
        #[arg(help = "Tunnel name")]
        name: String,

        // INSTANCE:PORT, or HOST:PORT when --dest-group or --via is used
        #[arg(help = "INSTANCE:PORT, or HOST:PORT with --dest-group or --via")]
        target: String,

        // An instance that can reach the target, when IAP cannot reach it
        // directly: the forward is made by ssh on that instance
        #[arg(
            long,
            conflicts_with = "dest_group",
            help = "Instance to forward from, for a host only it can reach"
        )]
        via: Option<String>,

        // Compute zone of the instance (overrides the account default)
        #[arg(short, long, help = "Compute zone of the instance")]
        zone: Option<String>,

        // Local port to listen on (allocated automatically when omitted)
        #[arg(
            long,
            help = "Local port to listen on (allocated automatically when omitted)"
        )]
        local_port: Option<u16>,

        // IAP destination group, for targets that are not a compute instance
        #[arg(
            long,
            help = "IAP destination group, for targets that are not a compute instance"
        )]
        dest_group: Option<String>,

        // Region of the destination group
        #[arg(long, help = "Region of the destination group")]
        region: Option<String>,

        // Network of the destination group
        #[arg(long, help = "Network of the destination group")]
        network: Option<String>,

        // Start the tunnel immediately after adding
        #[arg(short, long, help = "Start the tunnel immediately after adding")]
        start: bool,
    },

    // Publish a local service at https://<name>.<domain> through the GCP relay
    #[command(about = "Publish a local service at https://<name>.<domain> via the relay")]
    Expose {
        // Tunnel name (the subdomain part)
        #[arg(help = "Tunnel name (the subdomain part)")]
        name: String,

        // Local service to publish (e.g. 3000, localhost:3000)
        #[arg(help = "Local service to publish (e.g. 3000, localhost:3000)")]
        target: String,

        // Domain to publish under (overrides the account default)
        #[arg(short, long, help = "Domain to publish under")]
        domain: Option<String>,

        // Require Google sign-in via IAP on the load balancer
        #[arg(long, help = "Require Google sign-in via IAP on the load balancer")]
        private: bool,

        // Start the tunnel immediately after adding
        #[arg(short, long, help = "Start the tunnel immediately after adding")]
        start: bool,
    },

    // Run an ephemeral tunnel in the foreground (stops on Ctrl+C)
    //
    // Examples:
    //   gtunnel run my-vm:5432
    //   gtunnel run db my-vm:5432 -z europe-west1-b
    //   gtunnel run --public api localhost:3000
    #[command(about = "Run an ephemeral tunnel in the foreground (stops on Ctrl+C)")]
    Run {
        // Target alone, or name and target
        #[arg(required = true, num_args = 1..=2, help = "Target alone, or name and target")]
        args: Vec<String>,

        // Compute zone of the instance (Access mode)
        #[arg(short, long, help = "Compute zone of the instance (Access mode)")]
        zone: Option<String>,

        // Local port to listen on (Access mode)
        #[arg(long, help = "Local port to listen on (Access mode)")]
        local_port: Option<u16>,

        // Publish a local service publicly instead of forwarding a remote port
        #[arg(
            long,
            help = "Publish a local service publicly instead of forwarding a remote port"
        )]
        public: bool,

        // Domain to publish under (Expose mode)
        #[arg(short, long, help = "Domain to publish under (Expose mode)")]
        domain: Option<String>,
    },

    // Start a stopped tunnel
    #[command(about = "Start a stopped tunnel")]
    Start {
        // Tunnel name
        #[arg(help = "Tunnel name")]
        name: String,
    },

    // Stop a running tunnel
    #[command(about = "Stop a running tunnel")]
    Stop {
        // Tunnel name
        #[arg(help = "Tunnel name")]
        name: String,
    },

    // Start a tunnel at login, or stop doing so
    #[command(about = "Show or set whether a tunnel starts at login")]
    Autostart {
        // Tunnel name
        #[arg(help = "Tunnel name")]
        name: String,
        #[arg(long, help = "Start this tunnel at login", conflicts_with = "off")]
        on: bool,
        #[arg(long, help = "Do not start this tunnel at login")]
        off: bool,
    },

    // Restart a tunnel (stop, rewrite the service, start)
    #[command(about = "Restart a tunnel (stop, rewrite the service, start)")]
    Restart {
        // Tunnel name
        #[arg(help = "Tunnel name")]
        name: String,
    },

    // View logs for a tunnel
    #[command(about = "View logs for a tunnel")]
    Logs {
        // Tunnel name
        #[arg(help = "Tunnel name")]
        name: String,

        // Follow log output (like tail -f)
        #[arg(short, long, help = "Follow log output (like tail -f)")]
        follow: bool,

        // Number of lines to show
        #[arg(short, long, default_value = "50", help = "Number of lines to show")]
        lines: usize,
    },

    // List all tunnels (for scripting)
    #[command(about = "List all tunnels")]
    List,

    // Delete a tunnel
    #[command(about = "Delete a tunnel")]
    Delete {
        // Tunnel name
        #[arg(help = "Tunnel name")]
        name: String,
    },

    // Rename a tunnel
    #[command(about = "Rename a tunnel and move its service and log with it")]
    Rename {
        // Current tunnel name
        #[arg(help = "Current tunnel name")]
        old: String,

        // New tunnel name
        #[arg(help = "New tunnel name")]
        new: String,
    },

    // Move an Access tunnel to another account
    #[command(
        about = "Move an Access tunnel to another account",
        long_about = "Move an Access tunnel to another account. The global --account names the \
source and --to the destination."
    )]
    Move {
        // Tunnel name
        #[arg(help = "Tunnel name")]
        name: String,

        // Destination account
        #[arg(long, help = "Account to move the tunnel to")]
        to: String,

        // Name to land under when the destination holds the current one
        #[arg(long, help = "New name, when the destination already has this one")]
        rename: Option<String>,
    },

    // Manage compute zones
    #[command(about = "Manage compute zones")]
    Zones {
        #[command(subcommand)]
        command: Option<ZonesCommands>,
    },

    // Manage Expose domains
    #[command(about = "Manage Expose domains")]
    Domains {
        #[command(subcommand)]
        command: Option<DomainsCommands>,
    },

    // Manage the shared Expose relay for an account
    #[command(about = "Manage the shared Expose relay for an account")]
    Relay {
        #[command(subcommand)]
        command: RelayCommands,
    },

    // Manage gtunnel accounts
    #[command(about = "Manage gtunnel accounts")]
    Account {
        #[command(subcommand)]
        command: Option<AccountCommands>,
    },

    // Restart tunnels that are running but refusing connections
    #[command(about = "Restart tunnels that are running but refusing connections")]
    Watch {
        // Absent, `watch` is the loop itself; present, it manages the agent
        // that runs the loop
        #[command(subcommand)]
        command: Option<WatchCommands>,

        // One pass over every tunnel, then exit: what the watchdog agent runs
        #[arg(
            long,
            help = "One pass over every tunnel, then exit (what the agent runs)"
        )]
        once: bool,

        // Say what each pass saw, rather than only acting on it
        #[arg(
            short,
            long,
            help = "Say what each pass saw, rather than only acting on it"
        )]
        verbose: bool,
    },

    // Report leftovers and misconfiguration gtunnel can see
    #[command(about = "Report leftovers and misconfiguration gtunnel can see")]
    Doctor {
        // Remove what it finds instead of just reporting it
        #[arg(long, help = "Remove what it finds instead of just reporting it")]
        fix: bool,

        // Skip the confirmation prompt when using --fix
        #[arg(short, long, help = "Skip the confirmation prompt when using --fix")]
        yes: bool,

        // Check every configured account, not just the selected one
        #[arg(
            short,
            long,
            help = "Check every configured account, not just the selected one"
        )]
        all: bool,
    },

    // Remove all local gtunnel configuration
    #[command(about = "Remove all local gtunnel configuration")]
    Reset {
        // Skip confirmation prompt
        #[arg(short = 'y', long, help = "Skip confirmation prompt")]
        yes: bool,
    },

    // Launch the TUI with synthetic data (no config required)
    #[command(about = "Launch the TUI with synthetic data (no config required)")]
    Demo,

    // Check for and install updates
    #[command(about = "Check for and install updates")]
    Update {
        // Only check, don't install
        #[arg(long, help = "Only check, don't install")]
        check: bool,
    },
}

impl Cli {
    // No subcommand is the TUI, which talks to GCP from its first refresh
    pub fn needs_gcp_auth(&self) -> bool {
        self.command.as_ref().is_none_or(Commands::needs_gcp_auth)
    }
}

impl Commands {
    // Whether the command reaches GCP and so wants credentials up front.
    // Exhaustive on purpose: a new variant must be decided here, not default.
    // `doctor` is off the list because it reports missing auth as a finding,
    // and a login prompt would swallow that; `init` has no account yet.
    pub fn needs_gcp_auth(&self) -> bool {
        match self {
            Commands::Init
            | Commands::Account { .. }
            | Commands::Reset { .. }
            | Commands::Demo
            | Commands::Update { .. }
            | Commands::Doctor { .. }
            // Runs from an agent with no terminal: a login prompt here would
            // hang the watchdog forever rather than fail
            | Commands::Watch { .. } => false,
            Commands::Add { .. }
            | Commands::Expose { .. }
            | Commands::Run { .. }
            | Commands::Start { .. }
            | Commands::Stop { .. }
            | Commands::Autostart { .. }
            | Commands::Restart { .. }
            | Commands::Logs { .. }
            | Commands::List
            | Commands::Delete { .. }
            | Commands::Rename { .. }
            | Commands::Move { .. }
            | Commands::Zones { .. }
            | Commands::Domains { .. }
            | Commands::Relay { .. } => true,
        }
    }
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum WatchCommands {
    // Install the watchdog agent so the loop runs without a terminal
    #[command(about = "Install the watchdog agent so the loop runs without a terminal")]
    Install,

    // Remove the watchdog agent
    #[command(about = "Remove the watchdog agent")]
    Uninstall,

    // Say whether the watchdog agent is installed, and where its log is
    #[command(about = "Say whether the watchdog agent is installed, and where its log is")]
    Status,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum ZonesCommands {
    // Set the default compute zone
    #[command(about = "Set the default compute zone")]
    Default {
        // Zone name, e.g. europe-west1-b
        #[arg(help = "Zone name, e.g. europe-west1-b")]
        zone: String,
    },
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum DomainsCommands {
    // Set the default domain for Expose tunnels
    #[command(about = "Set the default domain for Expose tunnels")]
    Default {
        // Domain name, e.g. example.com
        #[arg(help = "Domain name, e.g. example.com")]
        domain: String,
    },

    // Register a Cloud DNS managed zone with the relay
    #[command(about = "Register a Cloud DNS managed zone with the relay")]
    Add {
        // Cloud DNS managed zone name
        #[arg(help = "Cloud DNS managed zone name")]
        managed_zone: String,
    },
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum RelayCommands {
    // Create the relay VM, load balancer, certificate and DNS plumbing
    #[command(about = "Create the relay VM, load balancer, certificate and DNS plumbing")]
    Up {
        // Compute zone for the relay VM (overrides the account default)
        #[arg(short, long, help = "Compute zone for the relay VM")]
        zone: Option<String>,

        // VPC network for the relay VM
        #[arg(long, default_value = "default", help = "VPC network for the relay VM")]
        network: String,

        // Cloud DNS managed zone to serve tunnel hostnames from
        #[arg(long, help = "Cloud DNS managed zone to serve tunnel hostnames from")]
        managed_zone: Option<String>,

        // Skip the confirmation prompt
        #[arg(short, long, help = "Skip the confirmation prompt")]
        yes: bool,
    },

    // Delete the relay and everything that belongs to it
    #[command(about = "Delete the relay and everything that belongs to it")]
    Down {
        // Delete Expose tunnels that still use the relay
        #[arg(long, help = "Delete Expose tunnels that still use the relay")]
        force: bool,

        // Skip the confirmation prompt
        #[arg(short, long, help = "Skip the confirmation prompt")]
        yes: bool,
    },

    // Show relay VM, address and certificate state
    #[command(about = "Show relay VM, address and certificate state")]
    Status,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum AccountCommands {
    // List all configured accounts
    #[command(about = "List all configured accounts")]
    List,

    // Set the default account
    #[command(about = "Set the default account")]
    Select {
        // Account name
        #[arg(help = "Account name")]
        name: String,
    },

    // Alias for select
    #[command(about = "Alias for select")]
    Default {
        // Account name
        name: String,
    },

    // Add an account from an existing gcloud configuration
    #[command(about = "Add an account from an existing gcloud configuration")]
    Add {
        // Account name
        #[arg(help = "Account name")]
        name: String,

        // gcloud configuration to take project and zone from
        #[arg(long, help = "gcloud configuration to take project and zone from")]
        configuration: String,
    },

    // Change an account's project or default zone
    #[command(about = "Change an account's project or default zone")]
    Edit {
        // Account name
        #[arg(help = "Account name")]
        name: String,

        // New project
        #[arg(long, help = "New project")]
        project: Option<String>,

        // New default zone
        #[arg(long, help = "New default zone")]
        zone: Option<String>,
    },

    // Remove an account
    #[command(about = "Remove an account that holds no tunnels")]
    Remove {
        // Account name
        #[arg(help = "Account name")]
        name: String,

        // Skip confirmation prompt
        #[arg(short = 'y', long, help = "Skip confirmation prompt")]
        yes: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("should parse")
    }

    // `watch` is two things under one word: the loop a person runs in a
    // terminal, and the agent that runs the loop for them. The subcommand is
    // optional so the bare word keeps meaning the loop
    #[test]
    fn watch_takes_an_optional_subcommand_and_still_runs_bare() {
        match parse(&["gtunnel", "watch", "install"])
            .command
            .expect("command")
        {
            Commands::Watch { command, .. } => {
                assert_eq!(command, Some(WatchCommands::Install))
            }
            other => panic!("parsed as {other:?}"),
        }
        match parse(&["gtunnel", "watch", "--once"])
            .command
            .expect("command")
        {
            Commands::Watch {
                command,
                once,
                verbose,
            } => {
                assert_eq!(command, None);
                assert!(once);
                assert!(!verbose);
            }
            other => panic!("parsed as {other:?}"),
        }
        for word in ["uninstall", "status"] {
            assert!(
                Cli::try_parse_from(["gtunnel", "watch", word]).is_ok(),
                "watch {word} should parse"
            );
        }
    }

    #[test]
    fn rename_takes_the_old_and_the_new_name_and_an_account() {
        let cli = parse(&["gtunnel", "rename", "db", "pg", "--account", "dev"]);
        assert_eq!(cli.account.as_deref(), Some("dev"));
        match cli.command.expect("command") {
            Commands::Rename { old, new } => assert_eq!((old.as_str(), new.as_str()), ("db", "pg")),
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn move_takes_a_name_a_destination_and_an_optional_rename() {
        let cli = parse(&[
            "gtunnel",
            "move",
            "db",
            "--to",
            "prod",
            "--rename",
            "pg",
            "--account",
            "dev",
        ]);
        assert_eq!(cli.account.as_deref(), Some("dev"));
        match cli.command.expect("command") {
            Commands::Move { name, to, rename } => {
                assert_eq!((name.as_str(), to.as_str()), ("db", "prod"));
                assert_eq!(rename.as_deref(), Some("pg"));
            }
            other => panic!("parsed as {other:?}"),
        }
        assert!(Cli::try_parse_from(["gtunnel", "move", "db"]).is_err());
        let help = Cli::try_parse_from(["gtunnel", "move", "--help"])
            .err()
            .expect("help exits")
            .to_string();
        assert!(help.contains("--account names the source"), "{help}");
    }

    #[test]
    fn add_parses_local_port_and_start() {
        let cli = parse(&[
            "gtunnel",
            "add",
            "db",
            "my-vm:5432",
            "-z",
            "europe-west1-b",
            "--local-port",
            "15432",
            "--start",
        ]);
        match cli.command.expect("command") {
            Commands::Add {
                name,
                target,
                zone,
                local_port,
                start,
                ..
            } => {
                assert_eq!(name, "db");
                assert_eq!(target, "my-vm:5432");
                assert_eq!(zone.as_deref(), Some("europe-west1-b"));
                assert_eq!(local_port, Some(15432));
                assert!(start);
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    // The k9s shape: a private endpoint the bastion can reach, forwarded from
    // the bastion rather than dialled through IAP directly
    #[test]
    fn add_parses_a_jump_host() {
        let cli = parse(&[
            "gtunnel",
            "add",
            "k8s",
            "10.0.0.10:443",
            "--via",
            "bastion",
            "--zone",
            "europe-west1-b",
            "--local-port",
            "8443",
        ]);
        match cli.command.expect("command") {
            Commands::Add {
                target,
                via,
                zone,
                local_port,
                ..
            } => {
                assert_eq!(target, "10.0.0.10:443");
                assert_eq!(via.as_deref(), Some("bastion"));
                assert_eq!(zone.as_deref(), Some("europe-west1-b"));
                assert_eq!(local_port, Some(8443));
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn add_parses_dest_group() {
        let cli = parse(&[
            "gtunnel",
            "add",
            "pg",
            "10.0.0.5:5432",
            "--dest-group",
            "prod",
            "--region",
            "europe-west1",
            "--network",
            "default",
        ]);
        match cli.command.expect("command") {
            Commands::Add {
                dest_group,
                region,
                network,
                start,
                ..
            } => {
                assert_eq!(dest_group.as_deref(), Some("prod"));
                assert_eq!(region.as_deref(), Some("europe-west1"));
                assert_eq!(network.as_deref(), Some("default"));
                assert!(!start);
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn run_public_takes_name_and_target() {
        let cli = parse(&["gtunnel", "run", "--public", "api", "localhost:3000"]);
        match cli.command.expect("command") {
            Commands::Run { args, public, .. } => {
                assert!(public);
                assert_eq!(args, vec!["api".to_string(), "localhost:3000".to_string()]);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_accepts_target_only() {
        let cli = parse(&["gtunnel", "run", "my-vm:5432"]);
        match cli.command.expect("command") {
            Commands::Run { args, public, .. } => {
                assert!(!public);
                assert_eq!(args, vec!["my-vm:5432".to_string()]);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_rejects_three_positionals() {
        assert!(Cli::try_parse_from(["gtunnel", "run", "a", "b", "c"]).is_err());
    }

    #[test]
    fn relay_down_parses_force() {
        let cli = parse(&["gtunnel", "relay", "down", "--force"]);
        match cli.command.expect("command") {
            Commands::Relay { command } => {
                assert_eq!(
                    command,
                    RelayCommands::Down {
                        force: true,
                        yes: false
                    }
                );
            }
            other => panic!("expected Relay, got {other:?}"),
        }
    }

    #[test]
    fn expose_parses_private_and_domain() {
        let cli = parse(&[
            "gtunnel",
            "expose",
            "api",
            "3000",
            "-d",
            "example.com",
            "--private",
            "--start",
        ]);
        match cli.command.expect("command") {
            Commands::Expose {
                name,
                target,
                domain,
                private,
                start,
            } => {
                assert_eq!(name, "api");
                assert_eq!(target, "3000");
                assert_eq!(domain.as_deref(), Some("example.com"));
                assert!(private);
                assert!(start);
            }
            other => panic!("expected Expose, got {other:?}"),
        }
    }

    #[test]
    fn logs_defaults_to_fifty_lines() {
        let cli = parse(&["gtunnel", "logs", "db"]);
        match cli.command.expect("command") {
            Commands::Logs { lines, follow, .. } => {
                assert_eq!(lines, 50);
                assert!(!follow);
            }
            other => panic!("expected Logs, got {other:?}"),
        }
    }

    #[test]
    fn account_flag_is_global() {
        let cli = parse(&["gtunnel", "--account", "prod", "list"]);
        assert_eq!(cli.account.as_deref(), Some("prod"));
        assert_eq!(cli.command, Some(Commands::List));
    }

    #[test]
    fn no_subcommand_is_none() {
        let cli = parse(&["gtunnel"]);
        assert!(cli.command.is_none());
    }

    // Every variant is listed on purpose: a new command must be sorted into
    // one side or the other here, not inherit a default
    #[test]
    fn only_commands_that_touch_gcp_need_a_login() {
        let skip = ["init", "account", "reset", "demo", "update", "doctor"];
        for name in skip {
            let cli = parse(&["gtunnel", name]);
            assert!(
                !cli.command.expect("command").needs_gcp_auth(),
                "{name} must not need a login"
            );
        }
        let check: [&[&str]; 12] = [
            &["gtunnel", "add", "db", "vm:5432"],
            &["gtunnel", "expose", "api", "3000"],
            &["gtunnel", "run", "vm:5432"],
            &["gtunnel", "start", "db"],
            &["gtunnel", "stop", "db"],
            &["gtunnel", "autostart", "db"],
            &["gtunnel", "restart", "db"],
            &["gtunnel", "logs", "db"],
            &["gtunnel", "list"],
            &["gtunnel", "delete", "db"],
            &["gtunnel", "zones"],
            &["gtunnel", "relay", "status"],
        ];
        for argv in check {
            let cli = parse(argv);
            assert!(
                cli.command.expect("command").needs_gcp_auth(),
                "{argv:?} must need a login"
            );
        }
        assert!(parse(&["gtunnel", "domains"])
            .command
            .expect("command")
            .needs_gcp_auth());
    }

    #[test]
    fn the_tui_needs_a_login() {
        assert!(parse(&["gtunnel"]).needs_gcp_auth());
    }
}
