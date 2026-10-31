use std::process::Command;

// AppleScript string literals escape backslashes and quotes
#[cfg(target_os = "macos")]
fn applescript_quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
pub fn osascript_body(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_quote(body),
        applescript_quote(title)
    )
}

/// One notifier to try.
#[derive(Debug, PartialEq)]
enum Step {
    /// A program and its arguments
    Program { program: String, args: Vec<String> },
    /// The freedesktop notification service on the session bus, which every
    /// Linux desktop runs, so nothing has to be installed for it
    #[cfg(target_os = "linux")]
    Bus { title: String, body: String },
}

impl Step {
    fn new(program: &str, args: &[&str]) -> Self {
        Step::Program {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    /// What a failure is reported as
    fn name(&self) -> &str {
        match self {
            Step::Program { program, .. } => program,
            #[cfg(target_os = "linux")]
            Step::Bus { .. } => "session bus",
        }
    }

    /// Delivered, or why not, in words short enough to share one log line.
    fn run(&self) -> Result<(), String> {
        match self {
            Step::Program { program, args } => {
                let mut cmd = Command::new(program);
                cmd.args(args);
                run_outcome(crate::proc::output_within_blocking(
                    cmd,
                    crate::proc::NOTIFY_BOUND,
                    program,
                ))
            }
            #[cfg(target_os = "linux")]
            Step::Bus { title, body } => {
                bus::send(title, body, crate::proc::NOTIFY_BOUND).map_err(|e| e.to_string())
            }
        }
    }
}

fn run_outcome(out: anyhow::Result<std::process::Output>) -> Result<(), String> {
    match out {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            Err(format!("{}: {}", out.status, first.trim())
                .trim_end_matches([':', ' '])
                .to_string())
        }
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Err("not installed".to_string())
        }
        Err(e) => Err(format!("{e:#}")),
    }
}

/// The line a chain that delivered nothing leaves behind, naming each step
/// and why it failed.
fn failure_line(failures: &[(String, String)]) -> String {
    let each: Vec<String> = failures
        .iter()
        .map(|(step, why)| format!("{step}: {why}"))
        .collect();
    format!("notification not delivered: {}", each.join("; "))
}

/// The notifiers this platform has, best first.
///
/// macOS ships `osascript`; `terminal-notifier` goes first when installed
/// because its notifications are attributed to it rather than to Script
/// Editor. On Linux the bus is spoken directly: `notify-send` comes from a
/// package minimal installs leave out, so it is only the fallback.
fn chain(title: &str, body: &str) -> Vec<Step> {
    #[cfg(target_os = "macos")]
    return vec![
        Step::new("terminal-notifier", &["-title", title, "-message", body]),
        Step::new("osascript", &["-e", &osascript_body(title, body)]),
    ];
    #[cfg(not(target_os = "macos"))]
    return vec![
        #[cfg(target_os = "linux")]
        Step::Bus {
            title: title.to_string(),
            body: body.to_string(),
        },
        Step::new("notify-send", &[title, body]),
    ];
}

/// `org.freedesktop.Notifications.Notify` on the session bus.
#[cfg(target_os = "linux")]
mod bus {
    use std::collections::HashMap;
    use std::time::Duration;

    use anyhow::{anyhow, Context, Result};
    use zbus::Variant;

    pub const SERVICE: &str = "org.freedesktop.Notifications";
    pub const PATH: &str = "/org/freedesktop/Notifications";

    /// app_name, replaces_id, app_icon, summary, body, actions, hints,
    /// expire_timeout: the `susssasa{sv}i` the specification names.
    pub type NotifyArgs<'a> = (
        &'a str,
        u32,
        &'a str,
        &'a str,
        &'a str,
        Vec<&'a str>,
        HashMap<&'a str, Variant<'a>>,
        i32,
    );

    /// A new notification (`replaces_id` 0) that expires when the server
    /// decides (-1).
    pub fn args<'a>(title: &'a str, body: &'a str) -> NotifyArgs<'a> {
        (
            "gtunnel",
            0,
            "",
            title,
            body,
            Vec::new(),
            HashMap::new(),
            -1,
        )
    }

    pub fn send(title: &str, body: &str, bound: Duration) -> Result<()> {
        send_on(zbus::connection::Builder::session(), title, body, bound)
    }

    /// Whether something on the bus owns the notification service
    pub fn has_service(bus: zbus::connection::Builder<'_>, bound: Duration) -> Result<bool> {
        within(bound, async {
            let bus = bus.build().await?;
            let reply = bus
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "NameHasOwner",
                    &(SERVICE,),
                )
                .await?;
            reply.body().deserialize::<bool>()
        })
    }

    /// Runs `call` to completion within `bound`. The caller is outside any
    /// runtime (the notifier thread, or `doctor` through `spawn_blocking`), so
    /// it brings a current-thread one of its own for zbus's tokio backend.
    fn within<T>(
        bound: Duration,
        call: impl std::future::Future<Output = zbus::Result<T>>,
    ) -> Result<T> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("notification runtime")?;
        runtime.block_on(async {
            let answer = tokio::time::timeout(bound, call)
                .await
                .map_err(|_| anyhow!("no answer within {bound:?}"))?;
            Ok(answer?)
        })
    }

    pub fn send_on(
        bus: zbus::connection::Builder<'_>,
        title: &str,
        body: &str,
        bound: Duration,
    ) -> Result<()> {
        within(bound, async {
            let bus = bus.build().await?;
            bus.call_method(
                Some(SERVICE),
                PATH,
                Some(SERVICE),
                "Notify",
                &args(title, body),
            )
            .await?;
            Ok(())
        })
    }
}

/// A chain's thread, which ends with the failure line when nothing delivered.
type Chain = std::thread::JoinHandle<Option<String>>;

/// The chain runs on a thread of its own: it is called from the TUI event
/// loop, and three notifiers each allowed their bound would otherwise freeze
/// the screen for fifteen seconds. The first to succeed ends the chain.
fn spawn_chain(chain: Vec<Step>) -> Chain {
    std::thread::spawn(move || {
        let mut failures = Vec::new();
        for step in chain {
            match step.run() {
                Ok(()) => return None,
                Err(why) => failures.push((step.name().to_string(), why)),
            }
        }
        Some(failure_line(&failures))
    })
}

/// The chains still delivering, so the process can wait for them before it
/// exits, and the last failure among those that ended.
///
/// The watchdog is a oneshot job: its pass ends milliseconds after it
/// announces, and systemd (and launchd) kill whatever the job left running,
/// a `notify-send` already spawned included. The change is in the ledger as
/// announced by then, so a dropped chain is an alert lost for good.
struct Pending(std::sync::Mutex<Chains>);

struct Chains {
    running: Vec<Chain>,
    failure: Option<String>,
}

impl Chains {
    /// Keeps a finished chain's failure, then lets the chain go
    fn reap(&mut self, chain: Chain) {
        if let Ok(Some(line)) = chain.join() {
            self.failure = Some(line);
        }
    }
}

impl Pending {
    const fn new() -> Self {
        Pending(std::sync::Mutex::new(Chains {
            running: Vec::new(),
            failure: None,
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Chains> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn track(&self, chain: Chain) {
        let mut chains = self.lock();
        // The TUI runs for hours; a finished chain is not kept for its exit
        let (done, running): (Vec<_>, Vec<_>) = std::mem::take(&mut chains.running)
            .into_iter()
            .partition(|c| c.is_finished());
        for chain in done {
            chains.reap(chain);
        }
        chains.running = running;
        chains.running.push(chain);
    }

    /// Waits for every chain, all of them within `bound` together: a notifier
    /// that hangs must not hold the exit, so one still running is abandoned.
    /// Returns the last failure, once: a pass that failed to notify five times
    /// says so in one line.
    fn flush_within(&self, bound: std::time::Duration) -> Option<String> {
        let running = std::mem::take(&mut self.lock().running);
        let deadline = std::time::Instant::now() + bound;
        let mut chains = self.lock();
        for chain in running {
            while !chain.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if chain.is_finished() {
                chains.reap(chain);
            }
        }
        chains.failure.take()
    }
}

static PENDING: Pending = Pending::new();

/// Lets the notifications already sent finish before the process exits, and
/// says on stderr when one could not be delivered.
///
/// Called where the process ends, never from the TUI loop, which must not
/// wait on a notifier. Printed here and never from the chain's thread: by
/// now the TUI has given the terminal back, and the watchdog's stderr is its
/// log.
#[allow(clippy::print_stderr)]
pub fn flush() {
    if let Some(line) = PENDING.flush_within(crate::proc::NOTIFY_BOUND) {
        eprintln!("{line}");
    }
}

/// Whether this machine can show a notification, or what it is missing, for
/// `doctor`.
pub fn check() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("osascript");
        cmd.args(["-e", "return"]);
        run_outcome(crate::proc::output_within_blocking(
            cmd,
            crate::proc::NOTIFY_BOUND,
            "osascript",
        ))
        .map_err(|why| format!("osascript did not run ({why})"))
    }
    #[cfg(target_os = "linux")]
    {
        let bound = crate::proc::NOTIFY_BOUND;
        match bus::has_service(zbus::connection::Builder::session(), bound) {
            Err(e) => Err(format!(
                "there is no session D-Bus to deliver to ({e}); a desktop session provides one"
            )),
            Ok(false) => Err(format!(
                "nothing on the session D-Bus owns {}; every desktop environment runs a \
                 notification service, and a bare window manager needs one such as dunst or mako",
                bus::SERVICE
            )),
            Ok(true) => watchdog_reaches_bus(),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Ok(())
}

/// The watchdog runs under `systemd --user`, whose environment is not this
/// shell's. zbus finds the bus in `DBUS_SESSION_BUS_ADDRESS`, else at
/// `$XDG_RUNTIME_DIR/bus`, which the user manager always sets.
#[cfg(target_os = "linux")]
fn watchdog_reaches_bus() -> Result<(), String> {
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "show-environment"]);
    let Ok(out) = crate::proc::output_within_blocking(cmd, crate::proc::NOTIFY_BOUND, "systemctl")
    else {
        // No user manager: no watchdog either, and doctor says that already
        return Ok(());
    };
    let manager = String::from_utf8_lossy(&out.stdout);
    let shell = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
    match bus_unseen_by_manager(shell.as_deref(), &manager, |p| p.exists()) {
        None => Ok(()),
        Some(addr) => Err(format!(
            "this shell's session bus ({addr}) is not in the systemd user environment, so the \
             watchdog cannot notify; run `systemctl --user import-environment \
             DBUS_SESSION_BUS_ADDRESS`"
        )),
    }
}

/// The shell's bus address when the user manager would connect elsewhere, or
/// nowhere.
#[cfg(target_os = "linux")]
fn bus_unseen_by_manager<'a>(
    shell: Option<&'a str>,
    manager_env: &str,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<&'a str> {
    let var = |name: &str| {
        manager_env
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
    };
    let manager = var("DBUS_SESSION_BUS_ADDRESS")
        .map(str::to_string)
        .or_else(|| {
            var("XDG_RUNTIME_DIR")
                .map(|dir| std::path::Path::new(dir).join("bus"))
                .filter(|p| exists(p))
                .map(|p| format!("unix:path={}", p.display()))
        });
    match (shell, manager) {
        (Some(shell), Some(manager)) if shell != manager => Some(shell),
        (Some(shell), None) => Some(shell),
        _ => None,
    }
}

// Best effort: a desktop without a notifier is not an error. Every caller
// reaches the desktop through here, so this is the one place the guard lives.
// `notifications.enabled` or `GTUNNEL_NOTIFICATIONS` (or the older
// `GTUNNEL_NO_NOTIFY`) is that guard. The `Notifier` trait only protects unit
// tests; an integration test runs the real binary, whose announcements would
// otherwise land on the desktop of whoever ran `cargo test`.
pub fn notify(title: &str, body: &str) {
    if !crate::settings::current().notifications {
        return;
    }
    PENDING.track(spawn_chain(chain(title, body)));
}

/// What lifecycle code talks to, so a test can assert on what would have been
/// said instead of putting it on the desktop of whoever runs `cargo test`.
pub trait Notifier {
    fn send(&self, title: &str, body: &str);
}

pub struct Desktop;

impl Notifier for Desktop {
    fn send(&self, title: &str, body: &str) {
        notify(title, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_applescript_payload_escapes_quotes_and_backslashes() {
        let script = osascript_body("he said \"hi\"", "path C:\\tmp");
        assert!(script.contains("\\\"hi\\\""), "{script}");
        assert!(script.contains("C:\\\\tmp"), "{script}");
        assert!(script.starts_with("display notification"), "{script}");
    }

    #[test]
    fn notify_returns_before_the_notifier_finishes() {
        let chain = vec![Step::new("sleep", &["1"])];
        let started = std::time::Instant::now();
        let handle = spawn_chain(chain);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "the caller waited {:?} for the notifier",
            started.elapsed()
        );
        handle.join().unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_tries_terminal_notifier_then_osascript() {
        let steps = chain("t", "b");
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert_eq!(
            steps[0],
            Step::new("terminal-notifier", &["-title", "t", "-message", "b"])
        );
        assert_eq!(
            steps[1],
            Step::new("osascript", &["-e", &osascript_body("t", "b")])
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_speaks_the_bus_first_and_never_tries_the_macos_programs() {
        let steps = chain("t", "b");
        assert_eq!(
            steps,
            vec![
                Step::Bus {
                    title: "t".into(),
                    body: "b".into()
                },
                Step::new("notify-send", &["t", "b"]),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_bus_call_has_the_signature_the_specification_names() {
        use zbus::Type;
        assert_eq!(
            <bus::NotifyArgs as Type>::SIGNATURE.to_string(),
            "(susssasa{sv}i)"
        );
        let (app, replaces, _, summary, body, _, _, timeout) = bus::args("t", "b");
        assert_eq!(
            (app, replaces, summary, body, timeout),
            ("gtunnel", 0, "t", "b", -1)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_bus_step_fails_fast_when_there_is_no_bus() {
        let started = std::time::Instant::now();
        let missing = zbus::connection::Builder::address("unix:path=/nonexistent/gtunnel-bus");
        let err = bus::send_on(missing, "t", "b", std::time::Duration::from_secs(2)).unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{err:#}"
        );
    }

    #[test]
    fn flush_waits_for_a_chain_that_is_still_delivering() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("delivered");
        let script = format!("sleep 0.3; touch '{}'", marker.display());
        let pending = Pending::new();
        pending.track(spawn_chain(vec![Step::new("sh", &["-c", &script])]));
        assert!(!marker.exists(), "the chain had not run yet");
        pending.flush_within(std::time::Duration::from_secs(5));
        assert!(marker.exists(), "flush returned before the chain finished");
    }

    #[test]
    fn flush_abandons_a_chain_that_outlives_the_bound() {
        let pending = Pending::new();
        pending.track(spawn_chain(vec![Step::new("sleep", &["3"])]));
        let started = std::time::Instant::now();
        pending.flush_within(std::time::Duration::from_millis(200));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "flush waited {:?} past its bound",
            started.elapsed()
        );
    }

    #[test]
    fn flush_with_nothing_pending_returns_at_once() {
        let started = std::time::Instant::now();
        Pending::new().flush_within(std::time::Duration::from_secs(5));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn a_finished_chain_is_not_kept() {
        let pending = Pending::new();
        let done = spawn_chain(vec![Step::new("true", &[])]);
        while !done.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        pending.track(done);
        pending.track(spawn_chain(vec![Step::new("true", &[])]));
        assert_eq!(pending.lock().running.len(), 1);
    }

    fn finished(chain: Chain) -> Option<String> {
        chain.join().unwrap()
    }

    #[test]
    fn a_chain_that_delivers_reports_nothing() {
        let steps = vec![Step::new("false", &[]), Step::new("true", &[])];
        assert_eq!(finished(spawn_chain(steps)), None);
    }

    #[test]
    fn a_chain_that_delivers_nothing_names_every_step_and_why() {
        let steps = vec![
            Step::new("gtunnel-no-such-notifier", &[]),
            Step::new("sh", &["-c", "echo 'no display' >&2; exit 3"]),
        ];
        let line = finished(spawn_chain(steps)).expect("a failure line");
        assert_eq!(
            line,
            "notification not delivered: gtunnel-no-such-notifier: not installed; \
             sh: exit status: 3: no display"
        );
    }

    #[test]
    fn a_failing_program_with_nothing_on_stderr_says_only_its_status() {
        let line = finished(spawn_chain(vec![Step::new("false", &[])])).unwrap();
        assert_eq!(line, "notification not delivered: false: exit status: 1");
    }

    #[test]
    fn flush_returns_the_failure_once_and_a_finished_chain_keeps_it() {
        let pending = Pending::new();
        let failed = spawn_chain(vec![Step::new("false", &[])]);
        while !failed.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        pending.track(failed);
        // Reaps the finished one: its failure must outlive it
        pending.track(spawn_chain(vec![Step::new("true", &[])]));
        let bound = std::time::Duration::from_secs(5);
        assert_eq!(
            pending.flush_within(bound).as_deref(),
            Some("notification not delivered: false: exit status: 1")
        );
        assert_eq!(pending.flush_within(bound), None, "said once");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_watchdog_sees_the_bus_the_shell_sees() {
        let at_runtime_dir = |p: &std::path::Path| p == std::path::Path::new("/run/user/1000/bus");
        let manager = "HOME=/home/you\nXDG_RUNTIME_DIR=/run/user/1000\n";
        // The standard socket, reached through the runtime directory
        assert_eq!(
            bus_unseen_by_manager(
                Some("unix:path=/run/user/1000/bus"),
                manager,
                at_runtime_dir
            ),
            None
        );
        // No address in the shell: zbus uses the runtime directory there too
        assert_eq!(bus_unseen_by_manager(None, manager, at_runtime_dir), None);
        // A bus launched by the X session, which systemd never heard of
        let launched = "unix:abstract=/tmp/dbus-XYZ,guid=1";
        assert_eq!(
            bus_unseen_by_manager(Some(launched), manager, at_runtime_dir),
            Some(launched)
        );
        // Imported into the manager: seen
        let imported = format!("{manager}DBUS_SESSION_BUS_ADDRESS={launched}\n");
        assert_eq!(
            bus_unseen_by_manager(Some(launched), &imported, at_runtime_dir),
            None
        );
        // No socket in the runtime directory and nothing imported
        assert_eq!(
            bus_unseen_by_manager(Some("unix:path=/run/user/1000/bus"), manager, |_| false),
            Some("unix:path=/run/user/1000/bus")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_service_check_fails_fast_when_there_is_no_bus() {
        let missing = zbus::connection::Builder::address("unix:path=/nonexistent/gtunnel-bus");
        assert!(bus::has_service(missing, std::time::Duration::from_secs(2)).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_can_notify() {
        assert_eq!(check(), Ok(()));
    }
}
