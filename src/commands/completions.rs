//! Shell completion: the registration script a user installs, and the
//! tunnel names it asks for when the cursor is on a name.
//!
//! The scripts come from clap_complete's dynamic engine, not from a static
//! dump of the command tree. A static script knows the subcommands but has no
//! way to say that `start` takes a tunnel and `add` takes a name that does not
//! exist yet; the engine asks the binary on every TAB, so each argument says
//! for itself what completes. The cost is that the engine is behind
//! `unstable-dynamic`, so the version is pinned in `Cargo.toml` and the script
//! should be regenerated on upgrade, as `docs/usage.md` says.

use std::ffi::OsStr;
use std::io::Write;

use anyhow::{anyhow, Result};
use clap::ValueEnum;
use clap_complete::engine::CompletionCandidate;
use clap_complete::env::Shells;
use clap_complete::CompleteEnv;

/// Only the shells the docs install lines cover: offering one that was never
/// tried would be a promise nobody checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }
}

/// Answers a completion request and exits, when the shell asked for one.
/// Must run before anything writes to stdout, because that is the reply.
pub fn answer_if_asked() {
    CompleteEnv::with_factory(<crate::cli::Cli as clap::CommandFactory>::command).complete();
}

pub fn cmd_completions(shell: Shell) -> Result<()> {
    let shells = Shells::builtins();
    let completer = shells
        .completer(shell.name())
        .ok_or_else(|| anyhow!("no completer for {}", shell.name()))?;
    let mut buf = Vec::new();
    // The script calls back into `gtunnel` by name, so it follows whichever
    // copy is first on PATH instead of freezing the path it was printed from
    completer.write_registration("COMPLETE", "gtunnel", "gtunnel", "gtunnel", &mut buf)?;
    match std::io::stdout().write_all(&buf) {
        // The reader stopped early, as `head` does: see
        // `dispatch::exit_quietly_on_broken_pipe`
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

/// Tunnel names of the selected account that start with what is typed.
///
/// Every failure is an empty list: a completion that prints an error lands in
/// the middle of the user's command line. `--account` is not honoured, since
/// the engine hands a completer only the word under the cursor.
pub fn tunnel_names(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    names_for_selected_account()
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.starts_with(prefix.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

fn names_for_selected_account() -> Result<Vec<String>> {
    let config = crate::config::load_config()?;
    let state = crate::state::TunnelState::load()?;
    Ok(state
        .tunnels_for_account(&config.selected_account)
        .into_iter()
        .map(|t| t.name.clone())
        .collect())
}
