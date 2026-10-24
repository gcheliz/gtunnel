//! The pure half of editing the configuration from the TUI: which editor to
//! run, whether what came back is fit to replace the real file, and what the
//! edit changed. Nothing here touches the terminal, so each rule can be proven
//! without one; the page and the terminal handoff live in `ui.rs` and `mod.rs`.

use anyhow::{anyhow, bail, Context, Result};

use crate::config::Config;
use crate::state::{validate_tunnel_name, TunnelState};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What `$VISUAL` and `$EDITOR` resolve to, as a program and its arguments.
///
/// `VISUAL` wins because it names the full-screen editor and `EDITOR` the line
/// one; an empty value counts as unset, since `EDITOR=` is how a shell clears
/// it. The value is split here and not handed to a shell so `code -w` works and
/// nothing in an environment variable is ever interpreted as a command line.
/// Quotes group a path with spaces and nothing else: there is no escaping,
/// because a variable that needs it should be a wrapper script.
pub fn resolve_editor(visual: Option<&str>, editor: Option<&str>) -> (String, Vec<String>) {
    for candidate in [visual, editor].into_iter().flatten() {
        let mut words = split_command(candidate);
        if !words.is_empty() {
            let program = words.remove(0);
            return (program, words);
        }
    }
    ("vi".to_string(), Vec::new())
}

fn split_command(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // A quoted run can be empty, and `''` is still a word the user typed
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Parses `text`, and reports a failure as one line: `line 3: invalid string`.
///
/// `toml`'s own Display prints the offending source and a caret under it,
/// which is five rows on a page that has four to spare; the line number and
/// the reason are what the reader needs, and the file is on screen already.
fn parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    toml::from_str(text).map_err(|e| match e.span() {
        Some(span) => {
            let upto = span.start.min(text.len());
            let line = text.as_bytes()[..upto]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
                + 1;
            anyhow!("line {line}: {}", e.message())
        }
        None => anyhow!("{}", e.message()),
    })
}

/// `config.toml` text, parsed, and checked to name an account it contains.
///
/// A selected account that is not in the list would not fail at the next load;
/// it would fail at the first command that resolves the account, far from the
/// edit that caused it. A parse error names its line.
pub fn validate_config(text: &str) -> Result<Config> {
    let config: Config = parse(text)?;
    if config.accounts.is_empty() {
        bail!("config.toml has no accounts: gtunnel needs at least one");
    }
    if !config
        .accounts
        .iter()
        .any(|a| a.name == config.selected_account)
    {
        bail!(
            "selected_account '{}' is not one of the accounts in config.toml",
            config.selected_account
        );
    }
    Ok(config)
}

/// `tunnels.toml` text, parsed and held to the rules `gtunnel add` enforces.
///
/// Names are validated on `add` and not on load, so a hand edit is the one way
/// a name that cannot be a DNS label or a service label reaches the file. A
/// tunnel for an account the config does not have is refused for the same
/// reason: nothing could start it. A semantic error names the line of the
/// tunnel's `[[tunnels]]` header when the file can be matched back to it.
pub fn validate_tunnels(text: &str, config: &Config) -> Result<TunnelState> {
    let state: TunnelState = parse(text)?;
    // The i-th table is the i-th header in the usual layout; a file written as
    // inline tables has none, and then the error simply carries no line
    let headers: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.trim() == "[[tunnels]]")
        .map(|(i, _)| i + 1)
        .collect();
    let at = |i: usize| match (headers.len() == state.tunnels.len(), headers.get(i)) {
        (true, Some(line)) => format!("line {line}: "),
        _ => String::new(),
    };
    let mut seen = HashSet::new();
    for (i, t) in state.tunnels.iter().enumerate() {
        if let Err(e) = validate_tunnel_name(&t.name) {
            bail!("{}{e}", at(i));
        }
        if !config.accounts.iter().any(|a| a.name == t.account_name) {
            bail!(
                "{}tunnel '{}' belongs to account '{}', which config.toml does not have",
                at(i),
                t.name,
                t.account_name
            );
        }
        if !seen.insert((t.name.as_str(), t.account_name.as_str())) {
            bail!(
                "{}tunnel '{}' is listed more than once for account '{}'",
                at(i),
                t.name,
                t.account_name
            );
        }
    }
    Ok(state)
}

/// What an edit did to the tunnel list, by name and account.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Summary {
    pub changed: usize,
    pub added: usize,
    pub removed: usize,
    /// A removed tunnel and an added one with the same account and settings:
    /// almost certainly one tunnel under a new name. Also counted in `added`
    /// and `removed`, because that is what the file did.
    pub renamed: Vec<(String, String)>,
}

impl Summary {
    /// The status line. Two facts surprise people after a hand edit: a running
    /// tunnel does not pick the change up, and a launchd job is named for the
    /// tunnel, so a tunnel that left the file leaves its job behind. The line
    /// carries the first always and the second when it applies.
    pub fn line(&self) -> String {
        let mut line = format!(
            "reloaded: {} changed, {} added, {} removed",
            self.changed, self.added, self.removed
        );
        if let Some((old, new)) = self.renamed.first() {
            line.push_str(&format!(
                "; renamed {old} to {new}: launchd keeps the job under the old name until it is unloaded"
            ));
        } else if self.removed > 0 {
            line.push_str("; a removed tunnel keeps its launchd job until it is unloaded");
        }
        line.push_str("; running tunnels keep their old settings until R");
        line
    }
}

/// Counts the tunnels that changed, appeared and disappeared between two states.
pub fn summarise(old: &TunnelState, new: &TunnelState) -> Summary {
    let mut summary = Summary::default();
    let mut removed = Vec::new();
    for o in &old.tunnels {
        match new.find_for_account(&o.name, &o.account_name) {
            Some(n) if n != o => summary.changed += 1,
            Some(_) => {}
            None => removed.push(o),
        }
    }
    let mut added: Vec<_> = new
        .tunnels
        .iter()
        .filter(|n| old.find_for_account(&n.name, &n.account_name).is_none())
        .collect();
    summary.added = added.len();
    summary.removed = removed.len();
    for r in removed {
        let twin = added
            .iter()
            .position(|a| a.account_name == r.account_name && a.spec == r.spec);
        if let Some(i) = twin {
            let a = added.remove(i);
            summary.renamed.push((r.name.clone(), a.name.clone()));
        }
    }
    summary
}

/// A private copy of `path` for the editor to work on, in the same directory.
///
/// The editor never sees the real file. A half-valid `tunnels.toml` breaks the
/// CLI and the daemon the moment it is saved, so the real file is replaced only
/// by `commit`, after the caller has validated what came back; `visudo` and
/// `crontab -e` work the same way. The copy lives in the config directory and
/// not in `/tmp` so the final rename never crosses a filesystem, which is what
/// makes it atomic, and it keeps the `.toml` ending so the editor highlights
/// it. It is owner-only because `config.toml` is. A missing original stages as
/// an empty file, since a first `tunnels.toml` is a legitimate thing to write.
pub fn stage(path: &Path) -> Result<PathBuf> {
    let dir = path.parent().context("the file has no directory")?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("the file has no name")?;
    // The process id keeps two gtunnels from editing one copy
    let staged = dir.join(format!("{stem}.edit-{}.toml", std::process::id()));
    let text = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
    };
    std::fs::write(&staged, text)
        .with_context(|| format!("Failed to write {}", staged.display()))?;
    // `write` only applies a mode when it creates the file, so a leftover copy
    // from a crash would keep whatever mode it had
    set_mode(&staged, 0o600)?;
    Ok(staged)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("Failed to set the mode of {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn mode_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn mode_of(_path: &Path) -> Option<u32> {
    None
}

/// Replaces `path` with the staged copy in one rename.
///
/// It does not validate: the caller decides whether the text is fit, because
/// which rules apply depends on which file it is. `config.toml` ends at 0600
/// whatever mode the editor left, since editors that write by rename reset it;
/// any other file keeps the mode it had.
pub fn commit(staged: &Path, path: &Path) -> Result<()> {
    let wanted = if path.file_name().is_some_and(|n| n == "config.toml") {
        Some(0o600)
    } else {
        mode_of(path)
    };
    if let Some(mode) = wanted {
        set_mode(staged, mode)?;
    }
    // The bytes are on disk before the name points at them, or a crash between
    // the two would leave the real file empty
    std::fs::File::open(staged)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("Failed to flush {}", staged.display()))?;
    std::fs::rename(staged, path).with_context(|| format!("Failed to replace {}", path.display()))
}

/// Removes the staged copy. A copy that is already gone is not an error: the
/// aim is that none is left.
pub fn discard(staged: &Path) -> Result<()> {
    match std::fs::remove_file(staged) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("Failed to remove {}", staged.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Account;
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec};

    fn account(name: &str) -> Account {
        Account {
            name: name.to_string(),
            gcloud_configuration: "default".to_string(),
            project: "example-dev-1a2b".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: None,
            expose: None,
        }
    }

    fn config(selected: &str, accounts: &[&str]) -> Config {
        Config {
            selected_account: selected.to_string(),
            accounts: accounts.iter().map(|a| account(a)).collect(),
            ..Default::default()
        }
    }

    fn tunnel(name: &str, account: &str, local_port: u16) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: account.to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "my-vm".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port,
            },
        }
    }

    fn state(tunnels: Vec<PersistentTunnel>) -> TunnelState {
        TunnelState { tunnels }
    }

    fn text_of(tunnels: Vec<PersistentTunnel>) -> String {
        toml::to_string_pretty(&state(tunnels)).unwrap()
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn visual_wins_over_editor() {
        assert_eq!(
            resolve_editor(Some("nvim"), Some("nano")),
            ("nvim".to_string(), vec![])
        );
    }

    #[test]
    fn editor_is_used_when_visual_is_unset_or_empty() {
        assert_eq!(
            resolve_editor(None, Some("nano")),
            ("nano".to_string(), vec![])
        );
        assert_eq!(
            resolve_editor(Some(""), Some("nano")),
            ("nano".to_string(), vec![])
        );
        assert_eq!(
            resolve_editor(Some("   "), Some("nano")),
            ("nano".to_string(), vec![])
        );
    }

    #[test]
    fn with_neither_set_the_editor_is_vi() {
        assert_eq!(resolve_editor(None, None), ("vi".to_string(), vec![]));
        assert_eq!(
            resolve_editor(Some(""), Some("")),
            ("vi".to_string(), vec![])
        );
    }

    #[test]
    fn an_editor_with_a_flag_splits_into_program_and_argument() {
        assert_eq!(
            resolve_editor(Some("code -w"), None),
            ("code".to_string(), args(&["-w"]))
        );
        assert_eq!(
            resolve_editor(None, Some("  emacs   -nw  ")),
            ("emacs".to_string(), args(&["-nw"]))
        );
    }

    #[test]
    fn a_quoted_path_with_a_space_stays_one_program() {
        assert_eq!(
            resolve_editor(Some("'/opt/my editor/ed' -w"), None),
            ("/opt/my editor/ed".to_string(), args(&["-w"]))
        );
        assert_eq!(
            resolve_editor(Some("\"/opt/my editor/ed\""), None),
            ("/opt/my editor/ed".to_string(), vec![])
        );
        // An unbalanced quote keeps the rest as one word and never panics
        assert_eq!(
            resolve_editor(Some("'/opt/my editor"), None),
            ("/opt/my editor".to_string(), vec![])
        );
    }

    #[test]
    fn a_config_must_parse_and_the_error_names_the_line() {
        let err = validate_config("selected_account = \"dev\"\n[[accounts]\nname = 1\n")
            .expect_err("invalid toml");
        let msg = err.to_string();
        assert!(msg.contains("line 2"), "{msg}");
    }

    #[test]
    fn a_config_must_name_an_account_it_contains() {
        let good = toml::to_string_pretty(&config("dev", &["dev", "prod"])).unwrap();
        assert_eq!(validate_config(&good).unwrap().selected_account, "dev");

        let bad = toml::to_string_pretty(&config("staging", &["dev", "prod"])).unwrap();
        let msg = validate_config(&bad)
            .expect_err("unknown account")
            .to_string();
        assert!(msg.contains("staging"), "{msg}");
        assert!(msg.contains("selected_account"), "{msg}");

        let none = toml::to_string_pretty(&config("", &[])).unwrap();
        assert!(validate_config(&none).is_err(), "no account at all");
    }

    #[test]
    fn tunnels_must_parse_and_the_error_names_the_line() {
        let cfg = config("dev", &["dev"]);
        let msg = validate_tunnels("[[tunnels]\nname = 1\n", &cfg)
            .expect_err("invalid toml")
            .to_string();
        assert!(msg.contains("line 1"), "{msg}");
    }

    #[test]
    fn a_valid_tunnels_file_round_trips() {
        let cfg = config("dev", &["dev", "prod"]);
        let text = text_of(vec![
            tunnel("db", "dev", 15432),
            tunnel("db", "prod", 15433),
        ]);
        let parsed = validate_tunnels(&text, &cfg).expect("valid");
        assert_eq!(parsed.tunnels.len(), 2);
        assert!(validate_tunnels("", &cfg).unwrap().tunnels.is_empty());
    }

    #[test]
    fn a_name_that_cannot_be_a_label_is_refused_with_its_line() {
        let cfg = config("dev", &["dev"]);
        let text = text_of(vec![
            tunnel("db", "dev", 15432),
            tunnel("Bad_Name", "dev", 15433),
        ]);
        let second_header = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.trim() == "[[tunnels]]")
            .nth(1)
            .map(|(i, _)| i + 1)
            .unwrap();
        let msg = validate_tunnels(&text, &cfg)
            .expect_err("bad name")
            .to_string();
        assert!(msg.contains("Bad_Name"), "{msg}");
        assert!(msg.contains(&format!("line {second_header}")), "{msg}");
    }

    #[test]
    fn a_duplicate_name_within_one_account_is_refused_but_not_across_accounts() {
        let cfg = config("dev", &["dev", "prod"]);
        let dup = text_of(vec![tunnel("db", "dev", 15432), tunnel("db", "dev", 15433)]);
        let msg = validate_tunnels(&dup, &cfg)
            .expect_err("duplicate")
            .to_string();
        assert!(msg.contains("db") && msg.contains("dev"), "{msg}");
        assert!(msg.contains("more than once"), "{msg}");

        let across = text_of(vec![
            tunnel("db", "dev", 15432),
            tunnel("db", "prod", 15433),
        ]);
        assert!(validate_tunnels(&across, &cfg).is_ok());
    }

    #[test]
    fn a_tunnel_for_an_account_the_config_lacks_is_refused() {
        let cfg = config("dev", &["dev"]);
        let text = text_of(vec![tunnel("db", "staging", 15432)]);
        let msg = validate_tunnels(&text, &cfg)
            .expect_err("unknown account")
            .to_string();
        assert!(msg.contains("staging"), "{msg}");
        assert!(msg.contains("config.toml"), "{msg}");
    }

    #[test]
    fn the_summary_counts_changed_added_and_removed_by_name_and_account() {
        let old = state(vec![
            tunnel("db", "dev", 15432),
            tunnel("cache", "dev", 16379),
            tunnel("db", "prod", 15433),
        ]);
        let new = state(vec![
            // same name, different port: changed
            tunnel("db", "dev", 15999),
            // untouched
            tunnel("cache", "dev", 16379),
            // `db` on prod is gone; `queue` is new
            tunnel("queue", "dev", 15672),
        ]);
        let summary = summarise(&old, &new);
        assert_eq!(
            (summary.changed, summary.added, summary.removed),
            (1, 1, 1),
            "{summary:?}"
        );
        assert!(summary.renamed.is_empty());
        assert!(summary
            .line()
            .starts_with("reloaded: 1 changed, 1 added, 1 removed"));
    }

    #[test]
    fn the_same_name_on_another_account_is_not_the_same_tunnel() {
        let old = state(vec![tunnel("db", "dev", 15432)]);
        let new = state(vec![tunnel("db", "prod", 15432)]);
        let summary = summarise(&old, &new);
        assert_eq!((summary.changed, summary.added, summary.removed), (0, 1, 1));
    }

    #[test]
    fn a_rename_is_named_and_warns_about_the_launchd_job() {
        let old = state(vec![tunnel("db", "dev", 15432)]);
        let new = state(vec![tunnel("postgres", "dev", 15432)]);
        let summary = summarise(&old, &new);
        assert_eq!(
            summary.renamed,
            vec![("db".to_string(), "postgres".to_string())]
        );
        assert_eq!((summary.added, summary.removed), (1, 1));
        let line = summary.line();
        assert!(line.contains("renamed db to postgres"), "{line}");
        assert!(line.contains("launchd"), "{line}");
    }

    #[test]
    fn a_plain_removal_also_warns_and_an_untouched_list_does_not() {
        let old = state(vec![
            tunnel("db", "dev", 15432),
            tunnel("cache", "dev", 16379),
        ]);
        let gone = summarise(&old, &state(vec![tunnel("cache", "dev", 16379)]));
        assert!(gone.line().contains("launchd"), "{}", gone.line());

        let same = summarise(&old, &old);
        assert_eq!(same.line(), "reloaded: 0 changed, 0 added, 0 removed; running tunnels keep their old settings until R");
    }

    fn mode(path: &Path) -> u32 {
        mode_of(path).expect("a mode")
    }

    fn chmod(path: &Path, mode: u32) {
        set_mode(path, mode).unwrap();
    }

    #[test]
    fn a_staged_copy_is_private_next_to_the_original_and_leaves_it_alone() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, "original\n").unwrap();

        let staged = stage(&real).expect("stage");
        assert_ne!(staged, real);
        assert_eq!(staged.parent(), real.parent(), "same directory");
        assert_eq!(staged.extension().unwrap(), "toml", "{staged:?}");
        assert_eq!(std::fs::read_to_string(&staged).unwrap(), "original\n");
        assert_eq!(mode(&staged), 0o600);
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "original\n");
    }

    #[test]
    fn a_missing_file_stages_as_an_empty_one() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        let staged = stage(&real).expect("stage");
        assert_eq!(std::fs::read_to_string(&staged).unwrap(), "");
        assert!(!real.exists(), "staging must not create the real file");
    }

    #[test]
    fn commit_replaces_the_original_and_consumes_the_copy() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, "before\n").unwrap();
        let staged = stage(&real).unwrap();
        std::fs::write(&staged, "after\n").unwrap();

        commit(&staged, &real).expect("commit");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "after\n");
        assert!(!staged.exists(), "a rename leaves no copy behind");
    }

    #[test]
    fn config_toml_ends_at_0600_even_when_the_editor_widened_it() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("config.toml");
        std::fs::write(&real, "before\n").unwrap();
        chmod(&real, 0o644);
        let staged = stage(&real).unwrap();
        std::fs::write(&staged, "after\n").unwrap();
        chmod(&staged, 0o644);

        commit(&staged, &real).unwrap();
        assert_eq!(mode(&real), 0o600);
    }

    #[test]
    fn another_file_keeps_the_mode_it_had() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, "before\n").unwrap();
        chmod(&real, 0o644);
        let staged = stage(&real).unwrap();
        std::fs::write(&staged, "after\n").unwrap();

        commit(&staged, &real).unwrap();
        assert_eq!(mode(&real), 0o644);
    }

    #[test]
    fn discard_removes_the_copy_and_tolerates_one_already_gone() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, "keep\n").unwrap();
        let staged = stage(&real).unwrap();

        discard(&staged).expect("discard");
        assert!(!staged.exists());
        discard(&staged).expect("a second discard is not an error");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "keep\n");
    }
}
