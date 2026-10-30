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

use anyhow::Result;

use crate::state::{PersistentTunnel, TunnelState, TunnelStatus};

use super::ServiceSpec;

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
}

/// What one pass did: the labels brought up to date, and the ones that could
/// not be, each with the reason
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub reinstalled: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Rewrites each installed definition that differs from the current render.
/// A running tunnel is restarted so the new argv is read; any other state is
/// left as it was, because a tunnel the user stopped is stopped on purpose.
pub async fn refresh(host: &impl ServiceHost, tunnels: &[PersistentTunnel]) -> Outcome {
    let mut out = Outcome::default();
    for t in tunnels {
        let label = host.label(t);
        // Read before the spec is built: building one looks for gcloud, and
        // most tunnels on most runs are not behind
        let Some(installed) = host.installed_text(&label) else {
            continue;
        };
        // A tunnel whose spec cannot be built (no gcloud, a missing
        // credential file) could not be started either, and that failure has
        // its own message at the command that needs it. Reporting it here
        // would print on every command until it was fixed.
        let Ok(mut spec) = host.spec_for(t) else {
            continue;
        };
        // The shell that happens to run this is not the user's word on the
        // proxy: only add, start and restart take its values
        super::keep_installed_forwarded(&mut spec, &installed);
        if !is_stale(&installed, &host.render(&spec), |p| host.program_exists(p)) {
            continue;
        }
        // Read before the write: a running job is the only one to reload, and
        // the answer must not depend on a file the write is about to change
        let running = host.status(&label).await == TunnelStatus::Running;
        let result = async {
            host.write(&spec).await?;
            if running {
                host.restart(&label).await?;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        match result {
            Ok(()) => out.reinstalled.push(label),
            Err(e) => out.failed.push((label, format!("{e:#}"))),
        }
    }
    out
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
/// masked too, because a moved binary is judged separately. The forwarded
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
        let tok = if prev == "--log-level" {
            "*".to_string()
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
        format!("{} {}", spec.label, spec.args.join(" "))
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
                env: self.env.clone(),
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
        fn program_exists(&self, _: &str) -> bool {
            true
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
            Ok(())
        }
    }

    const LABEL: &str = "com.gtunnel.dev.k8s";
    const OLD: &str = "com.gtunnel.dev.k8s __supervise";

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
        };
        let lines = report(&out);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("2 services"));
        assert!(lines[0].contains("com.gtunnel.dev.a") && lines[0].contains("com.gtunnel.dev.b"));
    }
}
