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

    fn run(&self) -> bool {
        match self {
            Step::Program { program, args } => {
                let mut cmd = Command::new(program);
                cmd.args(args);
                crate::proc::output_within_blocking(cmd, crate::proc::NOTIFY_BOUND, program)
                    .map(|out| out.status.success())
                    .unwrap_or(false)
            }
            #[cfg(target_os = "linux")]
            Step::Bus { title, body } => bus::send(title, body, crate::proc::NOTIFY_BOUND).is_ok(),
        }
    }
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

    /// Runs on the notifier thread, which is outside any runtime, so it brings
    /// a current-thread one of its own for zbus's tokio backend.
    pub fn send_on(
        bus: zbus::connection::Builder<'_>,
        title: &str,
        body: &str,
        bound: Duration,
    ) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("notification runtime")?;
        runtime.block_on(async {
            let call = async {
                let bus = bus.build().await?;
                bus.call_method(
                    Some(SERVICE),
                    PATH,
                    Some(SERVICE),
                    "Notify",
                    &args(title, body),
                )
                .await?;
                Ok::<_, zbus::Error>(())
            };
            tokio::time::timeout(bound, call)
                .await
                .map_err(|_| anyhow!("no answer within {bound:?}"))?
                .context("session bus")
        })
    }
}

/// The chain runs on a thread of its own: it is called from the TUI event
/// loop, and three notifiers each allowed their bound would otherwise freeze
/// the screen for fifteen seconds. The first to succeed ends the chain.
fn spawn_chain(chain: Vec<Step>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for step in chain {
            if step.run() {
                return;
            }
        }
    })
}

/// The chains still delivering, so the process can wait for them before it
/// exits.
///
/// The watchdog is a oneshot job: its pass ends milliseconds after it
/// announces, and systemd (and launchd) kill whatever the job left running,
/// a `notify-send` already spawned included. The change is in the ledger as
/// announced by then, so a dropped chain is an alert lost for good.
struct Pending(std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>);

impl Pending {
    const fn new() -> Self {
        Pending(std::sync::Mutex::new(Vec::new()))
    }

    fn track(&self, handle: std::thread::JoinHandle<()>) {
        let mut pending = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // The TUI runs for hours; a finished chain is not kept for its exit
        pending.retain(|h| !h.is_finished());
        pending.push(handle);
    }

    /// Waits for every chain, all of them within `bound` together: a notifier
    /// that hangs must not hold the exit, so one still running is abandoned.
    fn flush_within(&self, bound: std::time::Duration) {
        let pending = std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()));
        let deadline = std::time::Instant::now() + bound;
        for handle in pending {
            while !handle.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

static PENDING: Pending = Pending::new();

/// Lets the notifications already sent finish before the process exits.
///
/// Called where the process ends, never from the TUI loop, which must not
/// wait on a notifier.
pub fn flush() {
    PENDING.flush_within(crate::proc::NOTIFY_BOUND);
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
        assert_eq!(pending.0.lock().unwrap().len(), 1);
    }
}
