//! Bringing installed service definitions up to what this binary would write.
//!
//! A definition is a file written once, so an upgrade changes the binary and
//! leaves every installed job on the arguments it was installed with. The
//! restart budget made that visible: a job installed before it has no
//! `--crash-marker`, so it loops until someone starts it again. Comparing the
//! installed text with the rendered text is the test for "behind", which
//! makes the common case, nothing differs, a file read per tunnel and no
//! process. The comparison is of structure, not of every byte: see
//! [`is_stale`].

use anyhow::{Context, Result};

use crate::state::{PersistentTunnel, TunnelState, TunnelStatus};

use super::ServiceSpec;
use crate::version::{parse_version, Version};

/// Everything the refresh does to the outside world, so the order of those
/// effects is asserted without a service manager. The live implementation is
/// the only one that may reach `launchctl` or `systemctl`.
pub trait ServiceHost {
    fn label(&self, tunnel: &PersistentTunnel) -> String;
    /// The installed definition's text, or `None` when the tunnel was never
    /// installed: a tunnel without a definition is not behind, it is absent
    fn installed_text(&self, label: &str) -> Option<String>;
    fn spec_for(&self, tunnel: &PersistentTunnel) -> Result<ServiceSpec>;
    fn render(&self, spec: &ServiceSpec) -> String;
    /// Whether the binary an installed definition names is still there
    fn program_exists(&self, path: &str) -> bool;
    /// Writes the definition only: no log reset, no start
    async fn write(&self, spec: &ServiceSpec) -> Result<()>;
    async fn status(&self, label: &str) -> TunnelStatus;
    async fn restart(&self, label: &str) -> Result<()>;
    /// Whether an earlier pass wrote this label's definition and never saw the
    /// restart through
    fn restart_pending(&self, label: &str) -> bool;
    fn set_restart_pending(&self, label: &str) -> Result<()>;
    fn clear_restart_pending(&self, label: &str);
    /// The watchdog's installed text, or `None` when it was never installed
    fn watchdog_installed_text(&self) -> Option<String>;
    /// What `watch install` would write now, or `None` when this binary cannot
    /// say which program it is
    fn watchdog_render(&self) -> Option<String>;
    /// Rewrites the watchdog's definition and reloads it
    async fn watchdog_install(&self) -> Result<()>;
    /// The version this binary stamps into what it writes
    fn version(&self) -> &str;
    /// Where the pass takes its lock, or `None` for a host with no state
    /// shared with another process
    fn lock_path(&self) -> Option<std::path::PathBuf>;
    /// Established connections on a local port
    fn connections(&self, port: u16) -> u64;
    /// Records the newest version found stamped on a definition this binary
    /// had to leave alone, or that none was found (`None`). Returns whether
    /// that is news: the notice is printed once per version, not per command.
    fn note_newer(&self, seen: Option<&str>) -> bool;
}

/// What one pass did: the labels brought up to date, and the ones that could
/// not be, each with the reason
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub reinstalled: Vec<String>,
    pub failed: Vec<(String, String)>,
    /// Definitions written whose job did not restart: the label, the tunnel
    /// name the retry command takes, and the reason
    pub restart_failed: Vec<(String, String, String)>,
    /// Rewritten but left running on the old definition because a client was
    /// connected: the tunnel name and how many connections
    pub deferred: Vec<(String, u64)>,
    /// The newest version found stamped on a definition this binary left
    /// alone, and this binary's own, when that is news
    pub newer: Option<(String, String)>,
}

/// Rewrites each installed definition that is behind what this binary would
/// write, and finishes the restarts that an earlier pass began and did not
/// end.
///
/// A running tunnel is restarted so the new argv is read, unless a client is
/// connected: that definition is still written, so the next command has
/// nothing to do, and the tunnel is named so the person can restart it when it
/// is idle. A tunnel the user stopped stays stopped.
///
/// The pass skips itself when another command holds `refresh.lock`. Two
/// commands that start together (a prompt hook and a manual one) would
/// otherwise both see the old definition and both restart every tunnel.
pub async fn refresh(host: &impl ServiceHost, tunnels: &[PersistentTunnel]) -> Outcome {
    let mut out = Outcome::default();
    // Held to the end of the pass, and never waited for: the holder is doing
    // this very work, so a command that cannot take it has nothing to add
    let _lock = match host.lock_path() {
        Some(path) => match crate::fsutil::lock_exclusive(&path, std::time::Duration::ZERO) {
            Ok(lock) => Some(lock),
            Err(_) => return out,
        },
        None => None,
    };
    let ours = parse_version(host.version());
    let mut newest: Option<((u64, u64, u64), String)> = None;
    for t in tunnels {
        let label = host.label(t);
        // Read before the spec is built: building one looks for gcloud, and
        // most tunnels on most runs are not behind
        let Some(installed) = host.installed_text(&label) else {
            // A marker left by a tunnel that was deleted must not restart the
            // next one that takes the name
            host.clear_restart_pending(&label);
            continue;
        };
        let pending = host.restart_pending(&label);
        // A tunnel whose spec cannot be built (no gcloud, a missing
        // credential file) could not be started either, and that failure has
        // its own message at the command that needs it. Reporting it here
        // would print on every command until it was fixed.
        let verdict = host.spec_for(t).ok().map(|mut spec| {
            // The shell that happens to run this is not the user's word on the
            // proxy: only add, start and restart take its values
            super::keep_installed_forwarded(&mut spec, &installed);
            let behind = is_stale(&installed, &host.render(&spec), |p| host.program_exists(p));
            (spec, judge(&installed, behind, ours))
        });
        let rewrite = match verdict {
            Some((spec, Verdict::Behind)) => Some((spec, true)),
            Some((spec, Verdict::Unstamped)) => Some((spec, false)),
            Some((_, Verdict::Newer(version))) => {
                remember_newest(&mut newest, version);
                None
            }
            _ => None,
        };
        if rewrite.is_none() && !pending {
            continue;
        }
        // "The file differs" is the only record of work to do, and a write
        // that succeeded erases it. So the restart is recorded first, and
        // cleared only once the job has started again: a later pass retries a
        // pending label even though its file is current.
        let mut restart = pending;
        let mut deferred = None;
        if let Some((_, true)) = &rewrite {
            // Read before the write: the answer must not depend on a file the
            // write is about to change. A job in error is reloaded too: launchd
            // keeps the definition it loaded however often the file changes
            let status = host.status(&label).await;
            let live = matches!(status, TunnelStatus::Running | TunnelStatus::Error);
            // An errored job serves nobody, and a pending restart has already
            // cost the connections; only a running job has clients to spare
            let clients = match (status, t.spec.local_port()) {
                (TunnelStatus::Running, Some(port)) if !pending => host.connections(port),
                _ => 0,
            };
            if live && clients > 0 {
                deferred = Some(clients);
            } else {
                restart |= live;
            }
        }
        if let Some((spec, _)) = &rewrite {
            let written = async {
                if restart {
                    host.set_restart_pending(&label)?;
                }
                host.write(spec).await
            }
            .await;
            if let Err(e) = written {
                // Nothing changed, so there is nothing to finish later
                if !pending {
                    host.clear_restart_pending(&label);
                }
                out.failed.push((label, format!("{e:#}")));
                continue;
            }
        }
        if restart {
            if let Err(e) = host.restart(&label).await {
                out.restart_failed
                    .push((label, t.name.clone(), format!("{e:#}")));
                continue;
            }
            host.clear_restart_pending(&label);
        }
        if let Some(clients) = deferred {
            out.deferred.push((t.name.clone(), clients));
        }
        // Stamping a definition that was already right changes nothing the
        // user could act on, so it is not worth a line
        if matches!(rewrite, Some((_, false))) && !pending {
            continue;
        }
        out.reinstalled.push(label);
    }
    refresh_watchdog(host, ours, &mut newest, &mut out).await;
    out.newer = match newest {
        Some((_, version)) => host
            .note_newer(Some(&version))
            .then(|| (version, host.version().to_string())),
        None => {
            host.note_newer(None);
            None
        }
    };
    out
}

/// What the stamp says about an installed definition, given whether its shape
/// is behind. The stamp only decides which way a difference points: a
/// definition that is current is left alone however old its stamp, or a bump
/// of the version would write every file for nothing.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Current,
    /// Behind, and not written by a newer binary: rewrite it
    Behind,
    /// The right shape and no stamp: write the stamp, restart nothing
    Unstamped,
    /// Behind, but written by a newer binary: another install on `PATH`, or a
    /// downgrade. Rewriting would make that one rewrite it back
    Newer(String),
}

fn judge(installed: &str, behind: bool, ours: Option<Version>) -> Verdict {
    let text = super::forwarded_in(installed, super::DEFINITION_VERSION_VAR);
    let stamp = text.as_deref().and_then(parse_version);
    match (behind, stamp, text) {
        (true, Some(s), Some(text)) if ours.is_some_and(|o| s > o) => Verdict::Newer(text),
        (true, _, _) => Verdict::Behind,
        (false, None, _) => Verdict::Unstamped,
        (false, Some(_), _) => Verdict::Current,
    }
}

fn remember_newest(newest: &mut Option<(Version, String)>, version: String) {
    if let Some(parsed) = parse_version(&version)
        && newest.as_ref().is_none_or(|(n, _)| parsed > *n)
    {
        *newest = Some((parsed, version));
    }
}

/// The watchdog is one job for the machine and has its own definition, so it
/// is judged like a tunnel's but is never restarted: installing it is the
/// reload. Not installed means not the user's choice to have one yet.
async fn refresh_watchdog(
    host: &impl ServiceHost,
    ours: Option<Version>,
    newest: &mut Option<(Version, String)>,
    out: &mut Outcome,
) {
    let Some(installed) = host.watchdog_installed_text() else {
        return;
    };
    let Some(rendered) = host.watchdog_render() else {
        return;
    };
    let behind = is_stale(&installed, &rendered, |p| host.program_exists(p));
    let quiet = match judge(&installed, behind, ours) {
        Verdict::Current => return,
        Verdict::Newer(version) => {
            remember_newest(newest, version);
            return;
        }
        Verdict::Behind => false,
        Verdict::Unstamped => true,
    };
    let label = super::watchdog_label().to_string();
    match host.watchdog_install().await {
        Ok(()) if quiet => {}
        Ok(()) => out.reinstalled.push(label),
        Err(e) => out.failed.push((label, format!("{e:#}"))),
    }
}

/// The tokens of a definition that an upgrade can change, with what the
/// invoking shell can change left out.
///
/// A definition is rendered from the shell that installs it: `GTUNNEL_LOG_LEVEL`
/// sets `--log-level` and the verbosity words inside the gcloud argv, and
/// `GTUNNEL_RESTART_*` become environment entries. If those counted, one
/// command run as `GTUNNEL_LOG_LEVEL=debug gtunnel list` would rewrite and
/// restart every running tunnel, and the next plain command would do it back.
/// So their values are masked, the `GTUNNEL_*` entries are dropped (their
/// absence is as legitimate as their presence), and the program path is
/// masked too, because a moved binary is judged separately. The watchdog's
/// interval is masked for the same reason as the level: it is `watch.interval`
/// of the installing shell. The forwarded
/// gcloud, proxy and CA variables are dropped the same way: the shell that
/// runs a command is not the one that set the tunnel up, so a different proxy
/// there must not rewrite and restart every running tunnel. A new value is
/// taken by `add`, `start` or `restart`, and `doctor` reports the gap.
fn shape(text: &str) -> Vec<String> {
    let text = without_forwarded(text);
    let mut out = Vec::new();
    let mut prev = String::new();
    let mut skip_value = false;
    let mut after_array = false;
    for raw in text.split_whitespace() {
        // A plist wraps each value in an element; the bare text is what counts
        let t = raw
            .strip_prefix("<string>")
            .and_then(|r| r.strip_suffix("</string>"))
            .unwrap_or(raw);
        if skip_value {
            skip_value = false;
            continue;
        }
        if t.starts_with("<key>GTUNNEL_") {
            skip_value = true;
            continue;
        }
        if t.starts_with("Environment=GTUNNEL_") {
            continue;
        }
        let tok = if prev == "--log-level" || prev == "<key>StartInterval</key>" {
            "*".to_string()
        } else if t.starts_with("OnBootSec=") || t.starts_with("OnUnitActiveSec=") {
            // The watchdog's interval is `watch.interval`, read from the
            // shell that installs it
            format!("{}*", &t[..t.find('=').unwrap_or(0) + 1])
        } else if t.starts_with("--verbosity=") {
            "--verbosity=*".to_string()
        } else if let Some(i) = t.find("LogLevel=") {
            format!("{}*", &t[..i + "LogLevel=".len()])
        } else if after_array || t.starts_with("ExecStart=") {
            // The program is the first element of the argument array, and the
            // text right after `ExecStart=` in a unit
            match t.strip_prefix("ExecStart=") {
                Some(_) => "ExecStart=*".to_string(),
                None => "*".to_string(),
            }
        } else {
            t.to_string()
        };
        after_array = t == "<array>" && prev == "<key>ProgramArguments</key>";
        prev = t.to_string();
        out.push(tok);
    }
    out
}

/// The text with every forwarded variable's entry removed, key and value.
/// Their values differ per shell, so a difference must not count (see
/// [`shape`]), and a value may hold a space (a gcloud home under `Application
/// Support`), which the whitespace split would turn into stray tokens, so
/// this works a line at a time first.
fn without_forwarded(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        let plist_key = t
            .strip_prefix("<key>")
            .and_then(|r| r.strip_suffix("</key>"));
        if plist_key.is_some_and(|k| super::FORWARDED_ENV_VARS.contains(&k)) {
            lines.next();
            continue;
        }
        let unit = t
            .strip_prefix("Environment=")
            .map(|r| r.trim_start_matches('"'));
        if unit.is_some_and(|r| {
            super::FORWARDED_ENV_VARS
                .iter()
                .any(|n| r.strip_prefix(n).is_some_and(|x| x.starts_with('=')))
        }) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The binary an installed definition runs, as written
fn program_of(text: &str) -> Option<String> {
    let mut it = text.split_whitespace();
    while let Some(t) = it.next() {
        if t == "<key>ProgramArguments</key>" {
            it.next()?;
            let arg = it.next()?;
            return arg
                .strip_prefix("<string>")?
                .strip_suffix("</string>")
                .map(str::to_string);
        }
        if let Some(rest) = t.strip_prefix("ExecStart=") {
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}

/// Whether `installed` is behind what this binary would write.
///
/// Stale means the argument list differs in shape (a flag added by an
/// upgrade, such as `--crash-marker`, or one removed), or the binary the
/// definition names no longer exists. Values that come from the invoking shell
/// do not count; see [`shape`]. Pure, so a test can hold it to both file
/// formats without a service manager.
pub fn is_stale(installed: &str, rendered: &str, program_exists: impl Fn(&str) -> bool) -> bool {
    if program_of(installed).is_some_and(|p| !program_exists(&p)) {
        return true;
    }
    shape(installed) != shape(rendered)
}

/// The stderr lines for an outcome: one for everything reinstalled, then one
/// per failure. Empty when nothing happened, so a quiet run stays quiet.
pub fn report(outcome: &Outcome) -> Vec<String> {
    let mut lines = Vec::new();
    match outcome.reinstalled.as_slice() {
        [] => {}
        [one] => lines.push(format!("gtunnel: reinstalled {one} to match this version")),
        many => lines.push(format!(
            "gtunnel: reinstalled {} services to match this version: {}",
            many.len(),
            many.join(", ")
        )),
    }
    for (label, reason) in &outcome.failed {
        lines.push(format!("gtunnel: could not reinstall {label}: {reason}"));
    }
    // The definition is current, so the next pass finishes this by itself; the
    // command is for the person who would rather not wait for a next command
    for (label, name, reason) in &outcome.restart_failed {
        lines.push(format!(
            "gtunnel: reinstalled {label} but could not restart it: {reason}; \
             `gtunnel restart {name}` retries it"
        ));
    }
    for (name, clients) in &outcome.deferred {
        lines.push(format!(
            "gtunnel: {name} was rewritten but not restarted, it has {clients} open connection{}; \
             run `gtunnel restart {name}` when it is idle",
            if *clients == 1 { "" } else { "s" }
        ));
    }
    if let Some((written_by, this)) = &outcome.newer {
        lines.push(format!(
            "gtunnel: services were written by gtunnel {written_by}; this is {this}"
        ));
    }
    lines
}

/// A throwaway `GTUNNEL_CONFIG_DIR` moves the files but not the service
/// label, which stays `com.gtunnel.<account>.<name>`: refreshing from one
/// would write into the scratch directory and then restart the live job that
/// shares the label. The variable is the only signal there is, so any value
/// counts, the way `config_dir` reads it.
pub fn is_throwaway(config_dir_var: Option<&std::ffi::OsStr>) -> bool {
    config_dir_var.is_some()
}

struct Live {
    config: crate::config::Config,
}

impl ServiceHost for Live {
    fn label(&self, t: &PersistentTunnel) -> String {
        super::platform_label(&t.account_name, &t.name)
    }
    fn installed_text(&self, label: &str) -> Option<String> {
        super::installed_service_text(label)
    }
    fn spec_for(&self, t: &PersistentTunnel) -> Result<ServiceSpec> {
        super::service_spec(&self.config, t)
    }
    fn render(&self, spec: &ServiceSpec) -> String {
        #[cfg(target_os = "macos")]
        return super::launchd::render_plist(spec);
        #[cfg(not(target_os = "macos"))]
        return super::systemd::render_unit(spec);
    }
    fn program_exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }
    async fn write(&self, spec: &ServiceSpec) -> Result<()> {
        super::write_definition(spec).await
    }
    async fn status(&self, label: &str) -> TunnelStatus {
        super::status(label).await
    }
    async fn restart(&self, label: &str) -> Result<()> {
        super::restart(label).await
    }
    fn restart_pending(&self, label: &str) -> bool {
        self.pending_path(label).is_some_and(|p| p.exists())
    }
    fn set_restart_pending(&self, label: &str) -> Result<()> {
        let path = self
            .pending_path(label)
            .context("Could not find the run directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
        crate::fsutil::write_atomic(&path, "")
    }
    fn clear_restart_pending(&self, label: &str) {
        if let Some(path) = self.pending_path(label) {
            let _ = std::fs::remove_file(path);
        }
    }
    fn watchdog_installed_text(&self) -> Option<String> {
        super::installed_watchdog_text()
    }
    fn watchdog_render(&self) -> Option<String> {
        // The binary that runs the watchdog is this one, as `watch install`
        // writes it
        let program = crate::update::exe_path().ok()?;
        super::render_watchdog(&program).ok()
    }
    async fn watchdog_install(&self) -> Result<()> {
        super::install_watchdog(&crate::update::exe_path()?).await
    }
    fn version(&self) -> &str {
        super::DEFINITION_VERSION
    }
    fn lock_path(&self) -> Option<std::path::PathBuf> {
        Some(crate::config::config_dir().ok()?.join("refresh.lock"))
    }
    fn connections(&self, port: u16) -> u64 {
        crate::probe::active_connections(port)
    }
    fn note_newer(&self, seen: Option<&str>) -> bool {
        let Ok(dir) = crate::config::config_dir() else {
            return false;
        };
        let path = dir.join("run").join("definitions-newer");
        let Some(seen) = seen else {
            // Nothing newer any more: forget, so a later downgrade is news again
            let _ = std::fs::remove_file(&path);
            return false;
        };
        if std::fs::read_to_string(&path).is_ok_and(|text| text.trim() == seen) {
            return false;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = crate::fsutil::write_atomic(&path, seen);
        true
    }
}

impl Live {
    fn pending_path(&self, label: &str) -> Option<std::path::PathBuf> {
        let dir = crate::config::config_dir().ok()?;
        Some(super::restart_pending_in(&dir, label))
    }
}

/// The whole pass against any host, writing its lines to `err`. The seam the
/// live entry point goes through, so a test can drive the wiring (the
/// throwaway guard, the report) with a fake and no service manager.
pub async fn refresh_and_report(
    host: &impl ServiceHost,
    tunnels: &[PersistentTunnel],
    throwaway: bool,
    err: &mut impl std::io::Write,
) {
    if throwaway {
        return;
    }
    for line in report(&refresh(host, tunnels).await) {
        let _ = writeln!(err, "{line}");
    }
}

/// Entry point for a command: loads what it needs and does nothing it cannot
/// do safely. Never an error and never an exit status, because a stale
/// definition is not a reason for `gtunnel list` to fail.
pub async fn refresh_installed() {
    let throwaway = is_throwaway(std::env::var_os("GTUNNEL_CONFIG_DIR").as_deref());
    if throwaway {
        return;
    }
    let (Ok(config), Ok(state)) = (crate::config::load_config(), TunnelState::load()) else {
        return;
    };
    refresh_and_report(
        &Live { config },
        &state.tunnels,
        throwaway,
        &mut std::io::stderr(),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AccessTarget, TunnelSpec};
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn tunnel(name: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            account_name: "dev".to_string(),
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 22,
                local_port: 2222,
            },
            auto_start: false,
        }
    }

    /// A service manager made of a map: files by label, a status by label,
    /// and a record of every effect in the order it happened
    #[derive(Default)]
    struct Fake {
        files: RefCell<HashMap<String, String>>,
        status: HashMap<String, TunnelStatus>,
        fail_write: Vec<String>,
        // Set to render real unit text, carrying the level this shell has
        real_render: Option<String>,
        env: Vec<(String, String)>,
        // Labels whose next `restart` fails, once each
        fail_restart_once: RefCell<Vec<String>>,
        // The version this fake binary is; empty reads as `VERSION`
        version: String,
        lock_path: Option<std::path::PathBuf>,
        // Established connections by local port
        busy: HashMap<u16, u64>,
        // What the last "newer" notice named
        told: RefCell<Option<String>>,
        pending: RefCell<std::collections::HashSet<String>>,
        // The watchdog as installed, and as this binary would write it
        watchdog: RefCell<Option<String>>,
        watchdog_now: String,
        // Program paths that no longer exist
        gone: Vec<String>,
        written: RefCell<Vec<ServiceSpec>>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn with(label: &str, text: &str, status: TunnelStatus) -> Self {
            let mut f = Fake::default();
            f.files
                .borrow_mut()
                .insert(label.to_string(), text.to_string());
            f.status.insert(label.to_string(), status);
            f
        }
    }

    // The text this binary writes for a tunnel
    fn current(spec: &ServiceSpec) -> String {
        let stamp = spec
            .env
            .iter()
            .filter(|(k, _)| k == crate::daemon::DEFINITION_VERSION_VAR)
            .map(|(k, v)| format!("\nEnvironment={k}={v}"))
            .collect::<String>();
        format!("{} {}{stamp}", spec.label, spec.args.join(" "))
    }

    // What an older binary wrote: the same arguments, and no stamp
    fn unstamped(spec: &ServiceSpec) -> String {
        current(spec)
            .lines()
            .filter(|l| !l.starts_with("Environment="))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // The default fake binary's version, and ones around it
    const VERSION: &str = "1.2.3";
    const OLDER: &str = "1.2.0";
    const NEWER: &str = "1.3.0";

    fn stamped(version: &str) -> String {
        format!(
            "\nEnvironment={}={version}",
            crate::daemon::DEFINITION_VERSION_VAR
        )
    }

    impl ServiceHost for Fake {
        fn label(&self, t: &PersistentTunnel) -> String {
            format!("com.gtunnel.{}.{}", t.account_name, t.name)
        }
        fn installed_text(&self, label: &str) -> Option<String> {
            self.files.borrow().get(label).cloned()
        }
        fn spec_for(&self, t: &PersistentTunnel) -> Result<ServiceSpec> {
            Ok(ServiceSpec {
                label: format!("com.gtunnel.{}.{}", t.account_name, t.name),
                program: "gtunnel".into(),
                args: {
                    let mut a = vec!["__supervise".to_string()];
                    if let Some(level) = &self.real_render {
                        a.extend(["--log-level".to_string(), level.clone()]);
                    }
                    a.extend(["--crash-marker".to_string(), "m".to_string()]);
                    a
                },
                log_path: "log".into(),
                env: self
                    .env
                    .iter()
                    .cloned()
                    .chain([(
                        crate::daemon::DEFINITION_VERSION_VAR.to_string(),
                        self.version().to_string(),
                    )])
                    .collect(),
                auto_start: false,
                description: String::new(),
            })
        }
        fn render(&self, spec: &ServiceSpec) -> String {
            match &self.real_render {
                Some(_) => super::super::systemd::render_unit(spec),
                None => current(spec),
            }
        }
        fn program_exists(&self, path: &str) -> bool {
            !self.gone.iter().any(|g| g == path)
        }
        async fn write(&self, spec: &ServiceSpec) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("write {}", spec.label));
            self.written.borrow_mut().push(spec.clone());
            if self.fail_write.contains(&spec.label) {
                anyhow::bail!("disk full");
            }
            self.files
                .borrow_mut()
                .insert(spec.label.clone(), current(spec));
            Ok(())
        }
        async fn status(&self, label: &str) -> TunnelStatus {
            self.calls.borrow_mut().push(format!("status {label}"));
            self.status
                .get(label)
                .copied()
                .unwrap_or(TunnelStatus::Stopped)
        }
        async fn restart(&self, label: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("restart {label}"));
            let mut failing = self.fail_restart_once.borrow_mut();
            if let Some(i) = failing.iter().position(|l| l == label) {
                failing.remove(i);
                anyhow::bail!("launchctl bootstrap failed: Input/output error");
            }
            Ok(())
        }
        fn version(&self) -> &str {
            if self.version.is_empty() {
                VERSION
            } else {
                &self.version
            }
        }
        fn lock_path(&self) -> Option<std::path::PathBuf> {
            self.lock_path.clone()
        }
        fn connections(&self, port: u16) -> u64 {
            self.calls.borrow_mut().push(format!("connections {port}"));
            self.busy.get(&port).copied().unwrap_or(0)
        }
        fn note_newer(&self, seen: Option<&str>) -> bool {
            let mut told = self.told.borrow_mut();
            let news = seen.is_some() && told.as_deref() != seen;
            *told = seen.map(str::to_string);
            news
        }
        fn restart_pending(&self, label: &str) -> bool {
            self.pending.borrow().contains(label)
        }
        fn set_restart_pending(&self, label: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("pending {label}"));
            self.pending.borrow_mut().insert(label.to_string());
            Ok(())
        }
        fn clear_restart_pending(&self, label: &str) {
            self.pending.borrow_mut().remove(label);
        }
        fn watchdog_installed_text(&self) -> Option<String> {
            self.watchdog.borrow().clone()
        }
        fn watchdog_render(&self) -> Option<String> {
            Some(self.watchdog_now.clone()).filter(|t| !t.is_empty())
        }
        async fn watchdog_install(&self) -> Result<()> {
            self.calls.borrow_mut().push("watchdog install".to_string());
            *self.watchdog.borrow_mut() = Some(self.watchdog_now.clone());
            Ok(())
        }
    }

    const LABEL: &str = "com.gtunnel.dev.k8s";
    const OLD: &str = "com.gtunnel.dev.k8s __supervise";

    // The watchdog's text as an old binary wrote it and as this one does: a
    // flag the old one lacked, and a binary that is a unit's `ExecStart=`
    const WATCHDOG_OLD: &str = "ExecStart=/opt/gtunnel watch\n";
    const WATCHDOG_NOW: &str =
        "ExecStart=/opt/gtunnel watch --once\nEnvironment=GTUNNEL_DEFINITION_VERSION=1.2.3\n";

    fn with_watchdog(installed: &str) -> Fake {
        Fake {
            watchdog: RefCell::new(Some(installed.to_string())),
            watchdog_now: WATCHDOG_NOW.to_string(),
            ..Fake::default()
        }
    }

    #[tokio::test]
    async fn a_watchdog_definition_with_an_old_shape_is_rewritten_and_reloaded() {
        let host = with_watchdog(WATCHDOG_OLD);
        let out = refresh(&host, &[]).await;
        assert_eq!(out.reinstalled, vec![crate::daemon::watchdog_label()]);
        assert_eq!(*host.calls.borrow(), vec!["watchdog install".to_string()]);
        assert_eq!(
            host.watchdog_installed_text().as_deref(),
            Some(WATCHDOG_NOW)
        );
        assert!(report(&out)[0].contains(crate::daemon::watchdog_label()));
    }

    #[tokio::test]
    async fn a_watchdog_whose_program_is_missing_is_rewritten() {
        let mut host = with_watchdog("ExecStart=/gone/gtunnel watch --once\n");
        host.gone.push("/gone/gtunnel".to_string());
        let out = refresh(&host, &[]).await;
        assert_eq!(out.reinstalled.len(), 1, "{out:?}");
        assert_eq!(*host.calls.borrow(), vec!["watchdog install".to_string()]);
    }

    #[test]
    fn a_watchdog_interval_from_another_shell_is_not_a_difference() {
        let program = std::path::Path::new("/opt/gtunnel");
        let log = std::path::Path::new("/tmp/watchdog.log");
        let (service, timer) = super::super::systemd::render_watchdog_units(program, log, 30);
        let (service_b, timer_b) = super::super::systemd::render_watchdog_units(program, log, 90);
        assert_ne!(timer, timer_b);
        assert!(!is_stale(
            &format!("{service}\n{timer}"),
            &format!("{service_b}\n{timer_b}"),
            |_| true
        ));
        let plist = super::super::launchd::render_watchdog_plist(program, log, 30);
        let plist_b = super::super::launchd::render_watchdog_plist(program, log, 90);
        assert_ne!(plist, plist_b);
        assert!(!is_stale(&plist, &plist_b, |_| true));
        // but a flag does count, and so does a missing binary
        let old = plist.replace("<string>--once</string>", "");
        assert!(is_stale(&old, &plist, |_| true));
        assert!(is_stale(&plist, &plist, |_| false));
    }

    #[tokio::test]
    async fn a_current_watchdog_and_one_never_installed_are_left_alone() {
        let current = with_watchdog(WATCHDOG_NOW);
        assert_eq!(refresh(&current, &[]).await, Outcome::default());
        let absent = Fake {
            watchdog_now: WATCHDOG_NOW.to_string(),
            ..Fake::default()
        };
        assert_eq!(refresh(&absent, &[]).await, Outcome::default());
        assert!(current.calls.borrow().is_empty() && absent.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn a_restart_that_fails_leaves_a_marker_and_the_next_pass_finishes_it() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        host.fail_restart_once.borrow_mut().push(LABEL.to_string());
        let first = refresh(&host, &[tunnel("k8s")]).await;
        // The file is current and the job is not: the marker is the record
        assert!(first.reinstalled.is_empty(), "{first:?}");
        assert_eq!(first.restart_failed.len(), 1, "{first:?}");
        assert!(host.restart_pending(LABEL));
        let calls = host.calls.borrow().clone();
        let marked = calls
            .iter()
            .position(|c| c.starts_with("pending "))
            .unwrap();
        let wrote = calls.iter().position(|c| c.starts_with("write ")).unwrap();
        assert!(
            marked < wrote,
            "the marker is recorded before the write: {calls:?}"
        );
        host.calls.borrow_mut().clear();

        let second = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(second.reinstalled, vec![LABEL.to_string()], "{second:?}");
        assert!(!host.restart_pending(LABEL));
        let calls = host.calls.borrow().clone();
        assert_eq!(calls, vec![format!("restart {LABEL}")], "no second write");
    }

    #[tokio::test]
    async fn a_failed_restart_is_reported_with_the_command_that_retries_it() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        host.fail_restart_once.borrow_mut().push(LABEL.to_string());
        let lines = report(&refresh(&host, &[tunnel("k8s")]).await);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains(LABEL), "{lines:?}");
        assert!(lines[0].contains("gtunnel restart k8s"), "{lines:?}");
        assert!(lines[0].contains("Input/output error"), "{lines:?}");
    }

    #[tokio::test]
    async fn a_failed_write_leaves_no_marker_behind() {
        let mut host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        host.fail_write.push(LABEL.to_string());
        refresh(&host, &[tunnel("k8s")]).await;
        assert!(!host.restart_pending(LABEL));
    }

    #[tokio::test]
    async fn a_second_pass_with_nothing_pending_calls_no_service_manager() {
        let mut host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        host.watchdog = RefCell::new(Some(WATCHDOG_OLD.to_string()));
        host.watchdog_now = WATCHDOG_NOW.to_string();
        let first = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(first.reinstalled.len(), 2, "tunnel and watchdog: {first:?}");
        assert!(!host.restart_pending(LABEL));
        host.calls.borrow_mut().clear();
        let again = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(again, Outcome::default());
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls);
    }

    fn current_shape() -> String {
        unstamped(&Fake::default().spec_for(&tunnel("k8s")).unwrap())
    }

    #[tokio::test]
    async fn a_definition_stamped_newer_than_the_binary_is_not_rewritten() {
        let behind = format!("{OLD}{}", stamped(NEWER));
        let host = Fake::with(LABEL, &behind, TunnelStatus::Running);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(
            out.newer.as_ref().map(|n| n.0.as_str()),
            Some(NEWER),
            "{out:?}"
        );
        assert!(
            out.reinstalled.is_empty() && out.failed.is_empty(),
            "{out:?}"
        );
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls);
        assert_eq!(host.installed_text(LABEL).as_deref(), Some(behind.as_str()));
        let lines = report(&out);
        assert_eq!(
            lines,
            vec![format!(
                "gtunnel: services were written by gtunnel {NEWER}; this is {VERSION}"
            )]
        );
        // Said once: the next command is quiet while the situation is the same
        let again = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(again, Outcome::default());
        assert!(report(&again).is_empty());
    }

    #[tokio::test]
    async fn a_newer_stamp_on_a_definition_that_already_matches_says_nothing() {
        let text = format!("{}{}", current_shape(), stamped(NEWER));
        let host = Fake::with(LABEL, &text, TunnelStatus::Running);
        assert_eq!(refresh(&host, &[tunnel("k8s")]).await, Outcome::default());
        assert!(host.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn an_unstamped_definition_is_rewritten_and_stamped_without_a_restart() {
        // The right shape, written before stamps existed: nothing to reload
        let host = Fake::with(LABEL, &current_shape(), TunnelStatus::Running);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(*host.calls.borrow(), vec![format!("write {LABEL}")]);
        let text = host.installed_text(LABEL).unwrap();
        assert!(text.contains(&stamped(VERSION)), "{text}");
        // Not worth a line: nothing the user could act on changed
        assert!(report(&out).is_empty(), "{out:?}");
        host.calls.borrow_mut().clear();
        assert_eq!(refresh(&host, &[tunnel("k8s")]).await, Outcome::default());
        assert!(host.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn an_older_stamp_on_the_right_shape_is_left_alone() {
        // Rewriting on every version bump would restart nothing but write
        // every file; the stamp only has to be there, not current
        let text = format!("{}{}", current_shape(), stamped(OLDER));
        let host = Fake::with(LABEL, &text, TunnelStatus::Running);
        assert_eq!(refresh(&host, &[tunnel("k8s")]).await, Outcome::default());
        assert!(host.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn a_definition_stamped_older_with_a_different_shape_is_rewritten_and_restarted() {
        let text = format!("{OLD}{}", stamped(OLDER));
        let host = Fake::with(LABEL, &text, TunnelStatus::Running);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out.reinstalled, vec![LABEL.to_string()], "{out:?}");
        assert!(host
            .installed_text(LABEL)
            .unwrap()
            .contains(&stamped(VERSION)));
        assert!(host.calls.borrow().contains(&format!("restart {LABEL}")));
    }

    #[tokio::test]
    async fn a_pass_that_cannot_take_the_lock_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("refresh.lock");
        let mut host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        host.lock_path = Some(lock.clone());
        let held = crate::fsutil::lock_exclusive(&lock, std::time::Duration::ZERO).unwrap();
        // The other command is mid-pass: this one skips, with no wait
        let started = std::time::Instant::now();
        assert_eq!(refresh(&host, &[tunnel("k8s")]).await, Outcome::default());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls);
        drop(held);
        // and once it is done, the next one does the work, once
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out.reinstalled.len(), 1, "{out:?}");
        let restarts = host
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("restart "))
            .count();
        assert_eq!(restarts, 1);
    }

    #[tokio::test]
    async fn an_errored_job_with_a_stale_definition_is_reloaded() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Error);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out.reinstalled, vec![LABEL.to_string()], "{out:?}");
        let calls = host.calls.borrow().clone();
        let write = calls.iter().position(|c| c.starts_with("write ")).unwrap();
        let restart = calls
            .iter()
            .position(|c| c.starts_with("restart "))
            .expect("an errored job keeps its loaded definition until it is reloaded");
        assert!(write < restart, "{calls:?}");
        // An errored job has no client to drop: connections are not asked
        assert!(
            calls.iter().all(|c| !c.starts_with("connections ")),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn a_busy_tunnel_is_rewritten_but_not_restarted_and_is_named() {
        let mut host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        host.busy.insert(2222, 3);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out.deferred, vec![("k8s".to_string(), 3)], "{out:?}");
        let calls = host.calls.borrow().clone();
        assert!(calls.contains(&format!("write {LABEL}")), "{calls:?}");
        assert!(
            calls.iter().all(|c| !c.starts_with("restart ")),
            "{calls:?}"
        );
        assert!(
            !host.restart_pending(LABEL),
            "a deferral is not a failed restart"
        );
        let lines = report(&out);
        let said = lines.join("\n");
        assert!(
            said.contains("k8s") && said.contains("3 open connection"),
            "{said}"
        );
        assert!(said.contains("gtunnel restart k8s"), "{said}");
        // The file is current now, so the next command has nothing to say
        host.calls.borrow_mut().clear();
        assert_eq!(refresh(&host, &[tunnel("k8s")]).await, Outcome::default());
        assert!(host.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn an_idle_tunnel_is_restarted_and_its_port_was_asked_about() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert!(out.deferred.is_empty(), "{out:?}");
        let calls = host.calls.borrow().clone();
        assert!(calls.contains(&"connections 2222".to_string()), "{calls:?}");
        assert!(calls.contains(&format!("restart {LABEL}")), "{calls:?}");
    }

    #[tokio::test]
    async fn a_pending_restart_goes_ahead_even_when_a_client_is_connected() {
        // The job was already taken down by the failed attempt, so there is
        // no connection worth sparing
        let mut host = Fake::with(LABEL, &current_shape(), TunnelStatus::Running);
        host.busy.insert(2222, 3);
        host.pending.borrow_mut().insert(LABEL.to_string());
        refresh(&host, &[tunnel("k8s")]).await;
        assert!(host.calls.borrow().contains(&format!("restart {LABEL}")));
        assert!(!host.restart_pending(LABEL));
    }

    #[tokio::test]
    async fn a_watchdog_stamped_newer_is_left_alone_and_an_unstamped_one_is_stamped() {
        let newer = format!("{WATCHDOG_OLD}{}", stamped(NEWER));
        let host = with_watchdog(&newer);
        let out = refresh(&host, &[]).await;
        assert_eq!(
            out.newer.as_ref().map(|n| n.0.as_str()),
            Some(NEWER),
            "{out:?}"
        );
        assert!(host.calls.borrow().is_empty());

        // The right shape, no stamp: rewritten, quietly
        let unstamped_now =
            WATCHDOG_NOW.replace("Environment=GTUNNEL_DEFINITION_VERSION=1.2.3\n", "");
        let host = with_watchdog(&unstamped_now);
        let out = refresh(&host, &[]).await;
        assert!(report(&out).is_empty(), "{out:?}");
        assert_eq!(*host.calls.borrow(), vec!["watchdog install".to_string()]);
    }

    #[test]
    fn a_version_compares_by_its_numbers_and_an_unreadable_one_is_unstamped() {
        assert!(parse_version("0.2.67") < parse_version("0.2.100"));
        assert!(parse_version("1.0.0") > parse_version("0.99.99"));
        assert_eq!(parse_version("1.2.3-rc.1"), parse_version("1.2.3"));
        assert_eq!(parse_version("1.2.3+build"), parse_version("1.2.3"));
        assert_eq!(parse_version("main"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn every_definition_this_binary_renders_carries_the_stamp() {
        use crate::daemon::{forwarded_in, DEFINITION_VERSION, DEFINITION_VERSION_VAR};
        let program = std::path::Path::new("/opt/gtunnel");
        let log = std::path::Path::new("/tmp/watchdog.log");
        let plist = super::super::launchd::render_watchdog_plist(program, log, 30);
        let (service, _) = super::super::systemd::render_watchdog_units(program, log, 30);
        for text in [&plist, &service] {
            assert_eq!(
                forwarded_in(text, DEFINITION_VERSION_VAR).as_deref(),
                Some(DEFINITION_VERSION),
                "{text}"
            );
        }
        // and a tunnel's, through the same reader the refresh uses
        let mut spec = Fake::default().spec_for(&tunnel("k8s")).unwrap();
        spec.env = vec![(
            DEFINITION_VERSION_VAR.to_string(),
            DEFINITION_VERSION.to_string(),
        )];
        for text in [
            super::super::launchd::render_plist(&spec),
            super::super::systemd::render_unit(&spec),
        ] {
            assert_eq!(
                forwarded_in(&text, DEFINITION_VERSION_VAR).as_deref(),
                Some(DEFINITION_VERSION)
            );
        }
        // A version bump alone never makes a definition stale
        let bumped = plist.replace(DEFINITION_VERSION, "999.0.0");
        assert!(!is_stale(&bumped, &plist, |_| true));
    }

    #[tokio::test]
    async fn a_marker_for_a_tunnel_that_is_not_installed_is_dropped() {
        let host = Fake::default();
        host.pending.borrow_mut().insert(LABEL.to_string());
        refresh(&host, &[tunnel("k8s")]).await;
        assert!(!host.restart_pending(LABEL));
        assert!(host.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn a_definition_that_differs_is_rewritten_and_reported_once() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Stopped);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out.reinstalled, vec![LABEL.to_string()]);
        assert!(out.failed.is_empty());
        assert!(host
            .installed_text(LABEL)
            .unwrap()
            .contains("--crash-marker"));
        let lines = report(&out);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains(LABEL));
        assert!(lines[0].starts_with("gtunnel: "));
    }

    #[tokio::test]
    async fn a_definition_that_already_matches_is_not_touched() {
        let spec = Fake::default().spec_for(&tunnel("k8s")).unwrap();
        let host = Fake::with(LABEL, &current(&spec), TunnelStatus::Running);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out, Outcome::default());
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls);
        assert!(report(&out).is_empty());
    }

    #[tokio::test]
    async fn a_tunnel_that_was_never_installed_is_left_alone() {
        let host = Fake::default();
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out, Outcome::default());
        assert!(host.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn a_running_tunnel_is_restarted_after_its_definition_is_written() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        refresh(&host, &[tunnel("k8s")]).await;
        let calls = host.calls.borrow().clone();
        let write = calls.iter().position(|c| c.starts_with("write ")).unwrap();
        let restart = calls
            .iter()
            .position(|c| c.starts_with("restart "))
            .expect("a running tunnel must be reloaded");
        assert!(write < restart, "{calls:?}");
    }

    #[tokio::test]
    async fn a_stopped_tunnel_stays_stopped() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Stopped);
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out.reinstalled.len(), 1);
        let calls = host.calls.borrow().clone();
        assert!(
            calls.iter().all(|c| !c.starts_with("restart ")),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_reinstall_names_the_label_and_the_reason() {
        let mut host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        host.fail_write.push(LABEL.to_string());
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert!(out.reinstalled.is_empty());
        assert_eq!(out.failed.len(), 1);
        let lines = report(&out);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(LABEL) && lines[0].contains("disk full"));
        // A tunnel whose definition could not be written is not restarted
        let calls = host.calls.borrow().clone();
        assert!(
            calls.iter().all(|c| !c.starts_with("restart ")),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn one_failure_does_not_stop_the_others() {
        let mut host = Fake::with(LABEL, OLD, TunnelStatus::Stopped);
        host.files.borrow_mut().insert(
            "com.gtunnel.dev.db".to_string(),
            "com.gtunnel.dev.db old".to_string(),
        );
        host.fail_write.push(LABEL.to_string());
        let out = refresh(&host, &[tunnel("k8s"), tunnel("db")]).await;
        assert_eq!(out.reinstalled, vec!["com.gtunnel.dev.db".to_string()]);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(report(&out).len(), 2);
    }

    #[tokio::test]
    async fn a_second_pass_changes_nothing() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Running);
        let first = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(first.reinstalled.len(), 1);
        host.calls.borrow_mut().clear();
        let second = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(second, Outcome::default());
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls);
    }

    // The seam is a fake; this ties it to the real renderer, so "differs"
    // means what it does on a laptop: a plist from before the budget is not
    // the plist written now
    #[cfg(target_os = "macos")]
    #[test]
    fn a_plist_without_the_crash_marker_differs_from_the_current_render() {
        let mut spec = Fake::default().spec_for(&tunnel("k8s")).unwrap();
        spec.args = vec![
            "__supervise".to_string(),
            "--log-level".to_string(),
            "info".to_string(),
            "--".to_string(),
        ];
        let before = super::super::launchd::render_plist(&spec);
        spec.args.splice(
            3..3,
            ["--crash-marker".to_string(), "/run/k8s.crash".to_string()],
        );
        let after = super::super::launchd::render_plist(&spec);
        assert_ne!(before, after);
        assert!(!before.contains("--crash-marker") && after.contains("--crash-marker"));
    }

    fn unit(args: &[&str], env: &[(&str, &str)]) -> String {
        let mut spec = Fake::default().spec_for(&tunnel("k8s")).unwrap();
        spec.args = args.iter().map(|a| a.to_string()).collect();
        spec.env = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        super::super::systemd::render_unit(&spec)
    }

    const BASE: [&str; 6] = [
        "__supervise",
        "--log-level",
        "info",
        "--crash-marker",
        "/run/k8s.crash",
        "--",
    ];

    #[test]
    fn a_definition_differing_only_in_level_and_restart_values_is_not_stale() {
        let installed = unit(
            &BASE,
            &[("PATH", "/bin"), ("GTUNNEL_RESTART_MAX_RESTARTS", "5")],
        );
        let mut debug = BASE;
        debug[2] = "debug";
        let shell = unit(
            &debug,
            &[
                ("PATH", "/bin"),
                ("GTUNNEL_RESTART_MAX_RESTARTS", "9"),
                ("GTUNNEL_RESTART_WINDOW", "10m"),
            ],
        );
        assert!(!is_stale(&installed, &shell, |_| true));
        // and with no restart variable at all in the invoking shell
        assert!(!is_stale(
            &installed,
            &unit(&BASE, &[("PATH", "/bin")]),
            |_| true
        ));
    }

    #[test]
    fn the_verbosity_words_inside_the_gcloud_argv_follow_the_level() {
        let a = unit(&["--verbosity=info", "-o", "LogLevel=ERROR"], &[]);
        let b = unit(&["--verbosity=debug", "-o", "LogLevel=DEBUG1"], &[]);
        assert!(!is_stale(&a, &b, |_| true));
    }

    #[test]
    fn a_value_change_alone_does_not_make_a_definition_stale() {
        let installed = unit(&BASE, &[("CLOUDSDK_CONFIG", "/home/you/gcloud a")]);
        let moved = unit(&BASE, &[("CLOUDSDK_CONFIG", "/home/you/gcloud b")]);
        let added = unit(
            &BASE,
            &[
                ("CLOUDSDK_CONFIG", "/home/you/gcloud a"),
                ("HTTPS_PROXY", "http://proxy.example:3128/?a=1&b=2%40x"),
            ],
        );
        let none = unit(&BASE, &[]);
        // A shell with another proxy, or none, would otherwise rewrite and
        // restart every running tunnel, and the next shell would undo it
        for shell in [&moved, &added, &none] {
            assert!(!is_stale(&installed, shell, |_| true));
            assert!(!is_stale(shell, &installed, |_| true));
        }
        // but a flag next to them still counts
        let behind = unit(&["__supervise", "--"], &[("CLOUDSDK_CONFIG", "/x y")]);
        assert!(is_stale(&behind, &installed, |_| true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_same_holds_for_a_plist() {
        let render = |value: &str| {
            let mut spec = Fake::default().spec_for(&tunnel("k8s")).unwrap();
            spec.env = vec![("CLOUDSDK_CONFIG".to_string(), value.to_string())];
            super::super::launchd::render_plist(&spec)
        };
        assert!(!is_stale(&render("/a b"), &render("/c d"), |_| true));
    }

    #[tokio::test]
    async fn a_refresh_rewrite_keeps_the_forwarded_values_already_in_the_definition() {
        let mut installed = Fake::default().spec_for(&tunnel("k8s")).unwrap();
        installed.args = vec!["__supervise".to_string(), "--".to_string()];
        installed.env = vec![
            (
                "CLOUDSDK_CONFIG".to_string(),
                "/home/you/gcloud a".to_string(),
            ),
            ("PATH".to_string(), "/bin".to_string()),
        ];
        let text = super::super::systemd::render_unit(&installed);

        let mut host = Fake::with(LABEL, &text, TunnelStatus::Stopped);
        host.real_render = Some("info".to_string());
        // The refreshing shell has another gcloud home, a proxy, and a blank
        host.env = vec![
            ("CLOUDSDK_CONFIG".to_string(), "/home/you/other".to_string()),
            (
                "HTTPS_PROXY".to_string(),
                "http://proxy.example:3128".to_string(),
            ),
            ("PATH".to_string(), "/bin".to_string()),
        ];
        let out = refresh(&host, &[tunnel("k8s")]).await;
        assert_eq!(out.reinstalled.len(), 1, "{out:?}");

        let written = host.written.borrow();
        let env = &written[0].env;
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("CLOUDSDK_CONFIG"),
            Some("/home/you/gcloud a"),
            "{env:?}"
        );
        assert_eq!(get("HTTPS_PROXY"), None, "{env:?}");
        assert_eq!(get("PATH"), Some("/bin"));
    }

    #[test]
    fn a_definition_lacking_the_crash_marker_is_stale() {
        let old = unit(&["__supervise", "--log-level", "info", "--"], &[]);
        assert!(is_stale(&old, &unit(&BASE, &[]), |_| true));
    }

    #[test]
    fn a_binary_that_no_longer_exists_makes_a_definition_stale() {
        let text = unit(&BASE, &[]);
        assert!(!is_stale(&text, &text, |_| true));
        assert!(is_stale(&text, &text, |_| false));
        // a moved binary that still exists is the user's business
        let moved = text.replace("ExecStart=gtunnel", "ExecStart=/opt/gtunnel");
        assert_ne!(moved, text);
        assert!(!is_stale(&moved, &text, |_| true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_same_rule_holds_for_a_plist() {
        let render = |level: &str, env: Vec<(String, String)>, marker: bool| {
            let mut spec = Fake::default().spec_for(&tunnel("k8s")).unwrap();
            spec.args = vec!["__supervise".into(), "--log-level".into(), level.into()];
            if marker {
                spec.args.extend(["--crash-marker".into(), "m".into()]);
            }
            spec.env = env;
            super::super::launchd::render_plist(&spec)
        };
        let env = |v: &str| vec![("GTUNNEL_RESTART_WINDOW".to_string(), v.to_string())];
        let installed = render("info", env("5m"), true);
        assert!(!is_stale(
            &installed,
            &render("debug", env("9m"), true),
            |_| true
        ));
        assert!(!is_stale(&installed, &render("info", vec![], true), |_| {
            true
        }));
        assert!(is_stale(&render("info", vec![], false), &installed, |_| {
            true
        }));
    }

    #[tokio::test]
    async fn a_different_level_in_the_invoking_shell_restarts_nothing() {
        // Installed under `info`; this shell says debug and sets a budget
        let shell = Fake {
            real_render: Some("debug".to_string()),
            env: vec![("GTUNNEL_RESTART_WINDOW".to_string(), "10m".to_string())],
            ..Fake::default()
        };
        let installed = Fake {
            real_render: Some("info".to_string()),
            ..Fake::default()
        };
        let text = installed.render(&installed.spec_for(&tunnel("k8s")).unwrap());
        shell.files.borrow_mut().insert(LABEL.to_string(), text);
        let mut shell = shell;
        shell
            .status
            .insert(LABEL.to_string(), TunnelStatus::Running);
        let out = refresh(&shell, &[tunnel("k8s")]).await;
        assert_eq!(out, Outcome::default());
        assert!(shell.calls.borrow().is_empty(), "{:?}", shell.calls);
    }

    #[tokio::test]
    async fn the_live_entry_point_reports_through_the_host_and_skips_a_throwaway() {
        let host = Fake::with(LABEL, OLD, TunnelStatus::Stopped);
        let mut quiet = Vec::new();
        refresh_and_report(&host, &[tunnel("k8s")], true, &mut quiet).await;
        assert!(quiet.is_empty() && host.calls.borrow().is_empty());
        let mut err = Vec::new();
        refresh_and_report(&host, &[tunnel("k8s")], false, &mut err).await;
        let text = String::from_utf8(err).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains(LABEL));
        // the second command after the upgrade prints nothing
        let mut again = Vec::new();
        refresh_and_report(&host, &[tunnel("k8s")], false, &mut again).await;
        assert!(again.is_empty());
    }

    #[test]
    fn a_throwaway_config_directory_is_never_refreshed() {
        assert!(is_throwaway(Some(std::ffi::OsStr::new("/tmp/cfgdemo"))));
        assert!(!is_throwaway(None));
    }

    #[test]
    fn several_reinstalls_share_one_line() {
        let out = Outcome {
            reinstalled: vec!["com.gtunnel.dev.a".into(), "com.gtunnel.dev.b".into()],
            failed: vec![],
            ..Outcome::default()
        };
        let lines = report(&out);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("2 services"));
        assert!(lines[0].contains("com.gtunnel.dev.a") && lines[0].contains("com.gtunnel.dev.b"));
    }
}
