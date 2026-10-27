use std::process::Command;

// AppleScript string literals escape backslashes and quotes
fn applescript_quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn osascript_body(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_quote(body),
        applescript_quote(title)
    )
}

/// One notifier to try: a program and its arguments.
struct Step {
    program: String,
    args: Vec<String>,
}

impl Step {
    fn new(program: &str, args: &[&str]) -> Self {
        Step {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn run(&self) -> bool {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        crate::proc::output_within_blocking(cmd, crate::proc::NOTIFY_BOUND, &self.program)
            .map(|out| out.status.success())
            .unwrap_or(false)
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

/// `GTUNNEL_NO_NOTIFY` set to anything but empty or `0` silences the desktop.
/// The `Notifier` trait only protects unit tests; an integration test runs the
/// real binary, whose announcements would otherwise land on the desktop of
/// whoever ran `cargo test`. Taking the value as an argument keeps the rule
/// testable without mutating the process environment.
fn suppressed(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

// Best effort: a desktop without a notifier is not an error. Every caller
// reaches the desktop through here, so this is the one place the guard lives.
pub fn notify(title: &str, body: &str) {
    if suppressed(std::env::var("GTUNNEL_NO_NOTIFY").ok().as_deref()) {
        return;
    }
    let _ = spawn_chain(vec![
        Step::new("terminal-notifier", &["-title", title, "-message", body]),
        Step::new("osascript", &["-e", &osascript_body(title, body)]),
        Step::new("notify-send", &[title, body]),
    ]);
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

    #[test]
    fn the_applescript_payload_escapes_quotes_and_backslashes() {
        let script = osascript_body("he said \"hi\"", "path C:\\tmp");
        assert!(script.contains("\\\"hi\\\""), "{script}");
        assert!(script.contains("C:\\\\tmp"), "{script}");
        assert!(script.starts_with("display notification"), "{script}");
    }

    #[test]
    fn the_guard_reads_set_and_non_empty_as_silence() {
        assert!(suppressed(Some("1")));
        assert!(suppressed(Some("yes")));
        assert!(!suppressed(None), "unset: notifications stay on");
        assert!(!suppressed(Some("")), "an empty value counts as unset");
        assert!(!suppressed(Some("0")), "0 is how a shell says off");
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
}
