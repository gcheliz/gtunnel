//! The pure half of editing the configuration from the TUI: which editor to
//! run, whether what came back is fit to replace the real file, and what the
//! edit changed. Nothing here touches the terminal, so each rule can be proven
//! without one; the page and the terminal handoff live in `ui.rs` and `mod.rs`.

use anyhow::{anyhow, bail, Context, Result};

use crate::config::Config;
use crate::state::{label_of, validate_target, validate_tunnel_name, TunnelSpec, TunnelState};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// What `$VISUAL` and `$EDITOR` resolve to, as a program and its arguments.
///
/// `VISUAL` wins because it names the full-screen editor and `EDITOR` the line
/// one; an empty value counts as unset, since `EDITOR=` is how a shell clears
/// it. The value is split here and not handed to a shell so `code -w` works and
/// nothing in an environment variable is ever interpreted as a command line.
/// Quotes group a path with spaces and nothing else: there is no escaping,
/// because a variable that needs it should be a wrapper script.
#[cfg(test)]
pub fn resolve_editor(visual: Option<&str>, editor: Option<&str>) -> (String, Vec<String>) {
    resolve_editor_from(&[visual, editor])
}

/// The first usable candidate, in the order given, or `vi`.
///
/// The caller orders them `GTUNNEL_EDITOR`, `VISUAL`, `EDITOR`, then the
/// config file's `editor`: every variable outranks the file, and the file
/// ranks above nothing but the platform default, so a person who set `$EDITOR`
/// in their profile is not overruled by a key they wrote once.
pub fn resolve_editor_from(candidates: &[Option<&str>]) -> (String, Vec<String>) {
    for candidate in candidates.iter().copied().flatten() {
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
    refuse_unknown_keys::<Config>(text)?;
    Ok(config)
}

/// Refuses a key the structs would drop in silence.
///
/// Last in each validator so a parse error or a missing account is still the
/// first thing said: those block the file outright, this one only tells the
/// editor that part of what was typed will not take effect. Loading stays
/// lenient, so a file with a stray key that is already on disk keeps working;
/// it is the save of a new one that is refused. Names one key and counts
/// the rest, because one line is all the page has to spare.
fn refuse_unknown_keys<T: serde::de::DeserializeOwned + serde::Serialize>(
    text: &str,
) -> Result<()> {
    refuse_keys(crate::config::unknown_keys::<T>(text)?)
}

fn refuse_keys(keys: Vec<String>) -> Result<()> {
    match keys.as_slice() {
        [] => Ok(()),
        [only] => bail!("unknown key {only}"),
        [first, rest @ ..] => bail!("unknown key {first} and {} more", rest.len()),
    }
}

/// `tunnels.toml` text, parsed and held to the rules `gtunnel add` enforces.
///
/// Names are validated on `add` and not on load, so a hand edit is the one way
/// a name that cannot be a DNS label or a service label reaches the file. A
/// tunnel for an account the config does not have is refused for the same
/// reason: nothing could start it. A semantic error names the line of the
/// tunnel's `[[tunnels]]` header when the file can be matched back to it.
///
/// Two tunnels may not share a local port, in any account: the listener binds
/// on this machine whatever the account, so the second one would crash-loop
/// the moment it started. An Expose tunnel has no local listener and is
/// skipped, as `TunnelState::used_local_ports` skips it.
///
/// `runs` is the live `gtunnel run` ports with the run's name, which this
/// function cannot read itself: it stays a function of its arguments. `add`
/// refuses those ports, so a hand edit must too, or the save succeeds and the
/// tunnel fails at its next start.
pub fn validate_tunnels(
    text: &str,
    config: &Config,
    runs: &HashMap<u16, String>,
) -> Result<TunnelState> {
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
    let mut labels: HashMap<String, (&str, &str)> = HashMap::new();
    let mut ports: HashMap<u16, (&str, &str)> = HashMap::new();
    for (i, t) in state.tunnels.iter().enumerate() {
        if let Err(e) = validate_tunnel_name(&t.name) {
            bail!("{}{e}", at(i));
        }
        if let TunnelSpec::Access { target, .. } = &t.spec
            && let Err(e) = validate_target(target)
        {
            bail!("{}tunnel '{}': {e}", at(i), t.name);
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
        let label = label_of(&t.account_name, &t.name);
        if let Some((first, first_account)) =
            labels.insert(label.clone(), (t.name.as_str(), t.account_name.as_str()))
        {
            bail!(
                "{}tunnels '{}' ({}) and '{}' ({}) would share the file name '{}'",
                at(i),
                first,
                first_account,
                t.name,
                t.account_name,
                label
            );
        }
        if let TunnelSpec::Access { local_port, .. } = &t.spec
            && let Some((first, first_account)) =
                ports.insert(*local_port, (t.name.as_str(), t.account_name.as_str()))
        {
            bail!(
                "{}tunnels '{}' ({}) and '{}' ({}) both use local port {}",
                at(i),
                first,
                first_account,
                t.name,
                t.account_name,
                local_port
            );
        }
        if let TunnelSpec::Access { local_port, .. } = &t.spec
            && let Some(run) = runs.get(local_port)
        {
            bail!(
                "{}Local port {local_port} is already used by the run '{run}'",
                at(i)
            );
        }
    }
    refuse_keys(crate::state::unknown_tunnel_keys(text)?)?;
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

/// A private copy of `path` for the editor to work on, and the bytes it was
/// copied from.
///
/// The editor never sees the real file. A half-valid `tunnels.toml` breaks the
/// CLI and the daemon the moment it is saved, so the real file is replaced only
/// by `commit`, after the caller has validated what came back; `visudo` and
/// `crontab -e` work the same way. The copy lives in the config directory and
/// not in `/tmp` so the final rename never crosses a filesystem, which is what
/// makes it atomic, and it keeps the `.toml` ending so the editor highlights
/// it. It is owner-only because `config.toml` is. A missing original stages as
/// an empty file, since a first `tunnels.toml` is a legitimate thing to write.
///
/// The snapshot is what turns the save into a compare-and-swap: without it the
/// only thing `commit` could do is win against whoever wrote in between. It is
/// the raw bytes and not a hash, since the files are a few KB and bytes cannot
/// collide.
pub fn stage(path: &Path) -> Result<Staged> {
    let dir = path.parent().context("the file has no directory")?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("the file has no name")?;
    // The process id keeps two gtunnels from editing one copy
    let copy = dir.join(format!("{stem}.edit-{}.toml", std::process::id()));
    let original = read_if_present(path)?;
    std::fs::write(&copy, original.as_deref().unwrap_or_default())
        .with_context(|| format!("Failed to write {}", copy.display()))?;
    // `write` only applies a mode when it creates the file, so a leftover copy
    // from a crash would keep whatever mode it had
    set_mode(&copy, 0o600)?;
    Ok(Staged {
        path: copy,
        original,
    })
}

/// `None` is a file that does not exist, which is not the same snapshot as an
/// empty one: a `tunnels.toml` created meanwhile must refuse the save.
fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

/// An edit copy and the file it was taken from, as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub path: PathBuf,
    original: Option<Vec<u8>>,
}

impl Staged {
    /// What the editor was handed: lossy UTF-8, empty when the file was absent.
    pub fn original_text(&self) -> String {
        String::from_utf8_lossy(self.original.as_deref().unwrap_or_default()).into_owned()
    }

    /// Takes `now` as the file the edit is based on. After a refusal the user
    /// has seen what changed and may choose to replace it; a write after this
    /// point is still caught, because the next `commit` compares with `now`.
    pub fn rebase(&mut self, now: Option<Vec<u8>>) {
        self.original = now;
    }
}

/// The file on disk is not the one the edit started from.
#[derive(Debug)]
pub struct Changed {
    pub file: String,
    /// The file as `commit` found it, for the caller to re-base on
    pub now: Option<Vec<u8>>,
}

impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} changed since you opened it", self.file)
    }
}

impl std::error::Error for Changed {}

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

/// Replaces `path` with the staged copy in one rename, if `path` is still what
/// the copy was taken from.
///
/// Every other writer of `tunnels.toml` holds `tunnels.lock` from its read to
/// its write (`TunnelState::update`), and of `config.toml` `config.lock`
/// (`update_config`), so the editor's rename must too, and the
/// snapshot is what makes the save refuse instead of silently winning: a file
/// that differs from it, or that appeared since, is `Changed` and nothing is
/// renamed. The lock is held only from the re-read to the rename, never while
/// the editor runs.
///
/// It does not validate: the caller decides whether the text is fit, because
/// which rules apply depends on which file it is. `config.toml` ends at 0600
/// whatever mode the editor left, since editors that write by rename reset it;
/// any other file keeps the mode it had.
pub fn commit(staged: &Staged, path: &Path) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let _lock = match name {
        "tunnels.toml" => Some(crate::state::lock_tunnels()?),
        "config.toml" => Some(crate::config::lock_config()?),
        _ => None,
    };
    let now = read_if_present(path)?;
    if now != staged.original {
        return Err(Changed {
            file: name.to_string(),
            now,
        }
        .into());
    }
    let wanted = if name == "config.toml" {
        Some(0o600)
    } else {
        mode_of(path)
    };
    if let Some(mode) = wanted {
        set_mode(&staged.path, mode)?;
    }
    // The bytes are on disk before the name points at them, or a crash between
    // the two would leave the real file empty
    std::fs::File::open(&staged.path)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("Failed to flush {}", staged.path.display()))?;
    std::fs::rename(&staged.path, path)
        .with_context(|| format!("Failed to replace {}", path.display()))
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

/// Removes the edit copies of gtunnels that are no longer running, and returns
/// what it removed.
///
/// A crash or a closed terminal skips `discard`, and the process id in the
/// name is what tells an abandoned copy from another TUI's work in progress:
/// a live pid's copy is kept, and so is this process's own, which the page may
/// be holding after a refused edit. A recycled pid only delays the removal.
/// Only `{config,tunnels}.edit-<digits>.toml` matches, so nothing a user
/// parked in the directory is ever taken for a copy.
pub fn remove_stale_copies(dir: &Path, alive: impl Fn(u32) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = copy_pid(&name.to_string_lossy()) else {
            continue;
        };
        if pid == std::process::id() || alive(pid) {
            continue;
        }
        let path = entry.path();
        if std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

fn copy_pid(name: &str) -> Option<u32> {
    let digits = name
        .strip_prefix("config.edit-")
        .or_else(|| name.strip_prefix("tunnels.edit-"))?
        .strip_suffix(".toml")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
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
            credential_file: None,
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
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port,
            },
        }
    }

    fn state(tunnels: Vec<PersistentTunnel>) -> TunnelState {
        TunnelState {
            tunnels,
            ..Default::default()
        }
    }

    fn text_of(tunnels: Vec<PersistentTunnel>) -> String {
        toml::to_string_pretty(&state(tunnels)).unwrap()
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_config_editor_ranks_below_visual_and_editor_and_above_vi() {
        let chain =
            |gtunnel, visual, editor, file| resolve_editor_from(&[gtunnel, visual, editor, file]).0;
        assert_eq!(chain(Some("g"), Some("v"), Some("e"), Some("f")), "g");
        assert_eq!(chain(None, Some("v"), Some("e"), Some("f")), "v");
        assert_eq!(chain(None, None, Some("e"), Some("f")), "e");
        assert_eq!(chain(None, Some(""), None, Some("f")), "f");
        assert_eq!(chain(None, None, None, None), "vi");
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
        let msg = validate_tunnels("[[tunnels]\nname = 1\n", &cfg, &HashMap::new())
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
        let parsed = validate_tunnels(&text, &cfg, &HashMap::new()).expect("valid");
        assert_eq!(parsed.tunnels.len(), 2);
        assert!(validate_tunnels("", &cfg, &HashMap::new())
            .unwrap()
            .tunnels
            .is_empty());
    }

    #[test]
    fn two_accounts_holding_one_joined_name_are_refused_with_its_line() {
        let cfg = config("prod", &["prod", "prod-eu"]);
        let text = text_of(vec![
            tunnel("api", "prod-eu", 15432),
            tunnel("eu-api", "prod", 15433),
        ]);
        let second_header = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.trim() == "[[tunnels]]")
            .nth(1)
            .map(|(i, _)| i + 1)
            .unwrap();
        let msg = validate_tunnels(&text, &cfg, &HashMap::new())
            .expect_err("clash")
            .to_string();
        assert!(msg.contains("'prod-eu-api'"), "{msg}");
        assert!(msg.contains(&format!("line {second_header}")), "{msg}");
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
        let msg = validate_tunnels(&text, &cfg, &HashMap::new())
            .expect_err("bad name")
            .to_string();
        assert!(msg.contains("Bad_Name"), "{msg}");
        assert!(msg.contains(&format!("line {second_header}")), "{msg}");
    }

    #[test]
    fn an_instance_that_gcloud_would_read_as_a_flag_is_refused_with_its_line() {
        let cfg = config("dev", &["dev"]);
        let mut bad = tunnel("db", "dev", 15432);
        bad.spec = TunnelSpec::Access {
            target: AccessTarget::Instance {
                instance: "--command=id".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 22,
            local_port: 15432,
        };
        let msg = validate_tunnels(&text_of(vec![bad]), &cfg, &HashMap::new())
            .expect_err("a flag for an instance")
            .to_string();
        assert!(msg.contains("--command=id"), "{msg}");
        assert!(msg.contains("line 1"), "{msg}");
    }

    #[test]
    fn a_duplicate_name_within_one_account_is_refused_but_not_across_accounts() {
        let cfg = config("dev", &["dev", "prod"]);
        let dup = text_of(vec![tunnel("db", "dev", 15432), tunnel("db", "dev", 15433)]);
        let msg = validate_tunnels(&dup, &cfg, &HashMap::new())
            .expect_err("duplicate")
            .to_string();
        assert!(msg.contains("db") && msg.contains("dev"), "{msg}");
        assert!(msg.contains("more than once"), "{msg}");

        let across = text_of(vec![
            tunnel("db", "dev", 15432),
            tunnel("db", "prod", 15433),
        ]);
        assert!(validate_tunnels(&across, &cfg, &HashMap::new()).is_ok());
    }

    #[test]
    fn two_tunnels_on_one_local_port_are_refused_naming_both() {
        let cfg = config("dev", &["dev"]);
        let text = text_of(vec![
            tunnel("db", "dev", 15432),
            tunnel("cache", "dev", 15432),
        ]);
        let second_header = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.trim() == "[[tunnels]]")
            .nth(1)
            .map(|(i, _)| i + 1)
            .unwrap();
        let msg = validate_tunnels(&text, &cfg, &HashMap::new())
            .expect_err("one port")
            .to_string();
        assert_eq!(
            msg,
            format!(
                "line {second_header}: tunnels 'db' (dev) and 'cache' (dev) both use local port 15432"
            )
        );
    }

    #[test]
    fn one_port_across_accounts_is_still_one_port() {
        let cfg = config("dev", &["dev", "prod"]);
        let text = text_of(vec![
            tunnel("db", "dev", 15432),
            tunnel("db", "prod", 15432),
        ]);
        let msg = validate_tunnels(&text, &cfg, &HashMap::new())
            .expect_err("machine-wide")
            .to_string();
        assert!(msg.contains("'db' (dev) and 'db' (prod)"), "{msg}");
    }

    #[test]
    fn an_expose_tunnel_never_collides_on_local_port() {
        // An Expose spec has no IAP listener: `used_local_ports` skips it, so must this rule
        let cfg = config("dev", &["dev"]);
        let mut public = tunnel("site", "dev", 0);
        public.spec = TunnelSpec::Expose {
            local_target: "localhost:15432".into(),
            domain: "example.com".into(),
            hostname: "site.example.com".into(),
            relay_port: 30001,
            private: false,
        };
        let text = text_of(vec![tunnel("db", "dev", 15432), public]);
        assert!(validate_tunnels(&text, &cfg, &HashMap::new()).is_ok());
    }

    #[test]
    fn a_tunnel_on_a_port_a_live_run_holds_is_refused_naming_the_run() {
        let cfg = config("dev", &["dev"]);
        let runs = HashMap::from([(15432, "scratch".to_string())]);
        let text = text_of(vec![tunnel("db", "dev", 15432)]);
        let msg = validate_tunnels(&text, &cfg, &runs)
            .expect_err("a run holds the port")
            .to_string();
        assert_eq!(
            msg,
            "line 1: Local port 15432 is already used by the run 'scratch'"
        );
        let elsewhere = text_of(vec![tunnel("db", "dev", 15433)]);
        assert!(validate_tunnels(&elsewhere, &cfg, &runs).is_ok());
    }

    #[test]
    fn an_expose_tunnel_is_not_held_to_a_run_port() {
        let cfg = config("dev", &["dev"]);
        let runs = HashMap::from([(0, "scratch".to_string())]);
        let mut public = tunnel("site", "dev", 0);
        public.spec = TunnelSpec::Expose {
            local_target: "localhost:15432".into(),
            domain: "example.com".into(),
            hostname: "site.example.com".into(),
            relay_port: 30001,
            private: false,
        };
        assert!(validate_tunnels(&text_of(vec![public]), &cfg, &runs).is_ok());
    }

    #[test]
    fn a_tunnel_for_an_account_the_config_lacks_is_refused() {
        let cfg = config("dev", &["dev"]);
        let text = text_of(vec![tunnel("db", "staging", 15432)]);
        let msg = validate_tunnels(&text, &cfg, &HashMap::new())
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
        assert_ne!(staged.path, real);
        assert_eq!(staged.path.parent(), real.parent(), "same directory");
        assert_eq!(
            staged.path.extension().unwrap(),
            "toml",
            "{:?}",
            staged.path
        );
        assert_eq!(std::fs::read_to_string(&staged.path).unwrap(), "original\n");
        assert_eq!(mode(&staged.path), 0o600);
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "original\n");
    }

    #[test]
    fn a_missing_file_stages_as_an_empty_one() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        let staged = stage(&real).expect("stage");
        assert_eq!(std::fs::read_to_string(&staged.path).unwrap(), "");
        assert!(!real.exists(), "staging must not create the real file");
    }

    #[test]
    fn commit_replaces_the_original_and_consumes_the_copy() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, "before\n").unwrap();
        let staged = stage(&real).unwrap();
        std::fs::write(&staged.path, "after\n").unwrap();

        commit(&staged, &real).expect("commit");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "after\n");
        assert!(!staged.path.exists(), "a rename leaves no copy behind");
    }

    #[test]
    fn config_toml_ends_at_0600_even_when_the_editor_widened_it() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("config.toml");
        std::fs::write(&real, "before\n").unwrap();
        chmod(&real, 0o644);
        let staged = stage(&real).unwrap();
        std::fs::write(&staged.path, "after\n").unwrap();
        chmod(&staged.path, 0o644);

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
        std::fs::write(&staged.path, "after\n").unwrap();

        commit(&staged, &real).unwrap();
        assert_eq!(mode(&real), 0o644);
    }

    #[test]
    fn discard_removes_the_copy_and_tolerates_one_already_gone() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, "keep\n").unwrap();
        let staged = stage(&real).unwrap();

        discard(&staged.path).expect("discard");
        assert!(!staged.path.exists());
        discard(&staged.path).expect("a second discard is not an error");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "keep\n");
    }

    #[test]
    fn commit_refuses_when_the_file_changed_after_staging_and_both_versions_survive() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, text_of(vec![tunnel("db", "dev", 15432)])).unwrap();
        let staged = stage(&real).unwrap();
        TunnelState::update(|s| s.add(tunnel("cache", "dev", 16379))).unwrap();
        std::fs::write(&staged.path, text_of(vec![tunnel("db", "dev", 15999)])).unwrap();

        let err = commit(&staged, &real).expect_err("a concurrent write");
        let changed = err.downcast_ref::<Changed>().expect("typed");
        assert_eq!(err.to_string(), "tunnels.toml changed since you opened it");
        let on_disk = std::fs::read_to_string(&real).unwrap();
        assert!(
            on_disk.contains("cache") && on_disk.contains("15432"),
            "{on_disk}"
        );
        assert_eq!(changed.now.as_deref(), Some(on_disk.as_bytes()));
        assert!(
            std::fs::read_to_string(&staged.path)
                .unwrap()
                .contains("15999"),
            "the edit is kept"
        );
    }

    #[test]
    fn commit_refuses_when_a_missing_file_appeared_after_staging() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        let staged = stage(&real).unwrap();
        TunnelState::update(|s| s.add(tunnel("cache", "dev", 16379))).unwrap();
        std::fs::write(&staged.path, "# my edit\n").unwrap();

        let err = commit(&staged, &real).expect_err("the file appeared");
        assert!(err.downcast_ref::<Changed>().is_some(), "{err:#}");
        assert!(std::fs::read_to_string(&real).unwrap().contains("cache"));
    }

    #[test]
    fn commit_waits_for_the_tunnels_lock() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, "before\n").unwrap();
        let staged = stage(&real).unwrap();
        std::fs::write(&staged.path, "after\n").unwrap();

        let lock = crate::state::lock_tunnels().unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(lock);
        });
        let started = std::time::Instant::now();
        commit(&staged, &real).expect("commit once the lock is free");
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(150),
            "{:?}",
            started.elapsed()
        );
        holder.join().unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "after\n");
    }

    #[test]
    fn the_snapshot_is_what_was_staged_and_rebase_replaces_it() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("tunnels.toml");
        std::fs::write(&real, "a\n").unwrap();
        let mut staged = stage(&real).unwrap();
        assert_eq!(staged.original_text(), "a\n");
        staged.rebase(Some(b"b\n".to_vec()));
        assert_eq!(staged.original_text(), "b\n");

        std::fs::remove_file(&real).unwrap();
        assert_eq!(stage(&real).unwrap().original_text(), "");
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), "x\n").unwrap();
    }

    #[test]
    fn a_copy_left_by_a_dead_gtunnel_is_removed() {
        let dir = crate::config::testing::TempConfigDir::new();
        touch(dir.path(), "tunnels.edit-41.toml");
        touch(dir.path(), "config.edit-42.toml");
        touch(dir.path(), "tunnels.toml");

        let mut removed = remove_stale_copies(dir.path(), |_| false);
        removed.sort();
        assert_eq!(
            removed,
            [
                dir.path().join("config.edit-42.toml"),
                dir.path().join("tunnels.edit-41.toml")
            ]
        );
        assert!(!dir.path().join("tunnels.edit-41.toml").exists());
        assert!(!dir.path().join("config.edit-42.toml").exists());
        assert!(dir.path().join("tunnels.toml").exists());
    }

    #[test]
    fn a_copy_of_a_live_gtunnel_and_our_own_are_kept() {
        let dir = crate::config::testing::TempConfigDir::new();
        let own = format!("tunnels.edit-{}.toml", std::process::id());
        touch(dir.path(), "tunnels.edit-41.toml");
        touch(dir.path(), &own);

        // Our own copy is dead to `alive` and still kept; 41 is kept because it is alive
        assert!(remove_stale_copies(dir.path(), |p| p == 41).is_empty());
        assert!(dir.path().join("tunnels.edit-41.toml").exists());
        assert!(dir.path().join(own).exists());
    }

    #[test]
    fn files_that_only_look_like_copies_are_left_alone() {
        let dir = crate::config::testing::TempConfigDir::new();
        let names = [
            "tunnels.edit-abc.toml",
            "notes.edit-41.toml",
            "tunnels.edit-41.toml.bak",
            "tunnels.41.new",
            "tunnels.edit-.toml",
        ];
        for name in names {
            touch(dir.path(), name);
        }
        assert!(remove_stale_copies(dir.path(), |_| false).is_empty());
        for name in names {
            assert!(dir.path().join(name).exists(), "{name}");
        }
    }

    #[test]
    fn commit_refuses_a_config_that_changed_after_staging() {
        let dir = crate::config::testing::TempConfigDir::new();
        let mut config = config("dev", &["dev", "prod"]);
        crate::config::save_config(&config).unwrap();
        let real = dir.path().join("config.toml");
        let staged = stage(&real).unwrap();
        crate::config::update_config(|c| c.select_account("prod")).unwrap();
        config.selected_account = "dev".to_string();
        std::fs::write(&staged.path, toml::to_string_pretty(&config).unwrap()).unwrap();

        let err = commit(&staged, &real).expect_err("account select wrote meanwhile");
        assert!(err.downcast_ref::<Changed>().is_some(), "{err:#}");
        assert_eq!(err.to_string(), "config.toml changed since you opened it");
        let on_disk = std::fs::read_to_string(&real).unwrap();
        assert!(on_disk.contains("selected_account = \"prod\""), "{on_disk}");
        assert_eq!(mode(&real), 0o600);
    }

    #[test]
    fn commit_waits_for_the_config_lock() {
        let dir = crate::config::testing::TempConfigDir::new();
        let real = dir.path().join("config.toml");
        std::fs::write(&real, "before\n").unwrap();
        let staged = stage(&real).unwrap();
        std::fs::write(&staged.path, "after\n").unwrap();

        let lock = crate::config::lock_config().unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(lock);
        });
        let started = std::time::Instant::now();
        commit(&staged, &real).expect("commit once the lock is free");
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(150),
            "{:?}",
            started.elapsed()
        );
        holder.join().unwrap();
    }
}
