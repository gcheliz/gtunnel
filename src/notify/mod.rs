//! Desktop notifications.
//!
//! What a caller builds is a [`Notification`]: how bad the news is, the words
//! and what it is about. How it reaches the desktop is the notifier's business
//! and lives in the files below, so a caller never learns whether this machine
//! speaks D-Bus or runs `osascript`.
//!
//! - `backend`: the programs that can show a notification, and the order this
//!   platform tries them in
//! - `linux_bus`: the freedesktop service on the session bus, spoken directly
//! - `dispatch`: the thread each delivery runs on, and the flush before exit
//! - `check`: what `doctor` asks about this machine

mod backend;
mod check;
mod dispatch;
#[cfg(target_os = "linux")]
mod linux_bus;
mod style;
mod terminal;
#[cfg(test)]
mod tests;

pub use check::check;
pub use dispatch::flush;

/// How much a notification matters, least to most, so the most serious of
/// several can stand for all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Something started
    Info,
    /// Something that was wrong is not
    Recovered,
    /// Something is struggling
    Warning,
    /// Something broke or stopped
    Failure,
}

/// One thing worth telling the person at the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub severity: Severity,
    pub body: String,
    /// What the notification is about, `account` or `account/tunnel`, so a
    /// later one on the same subject can stand in for an earlier one. `None`
    /// for a summary of several.
    pub group: Option<String>,
}

impl Notification {
    pub fn new(severity: Severity, body: impl Into<String>) -> Self {
        Self {
            severity,
            body: body.into(),
            group: None,
        }
    }

    pub fn about(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// Every notification is titled for the program that raised it
    pub fn title(&self) -> &'static str {
        "gtunnel"
    }
}

// Best effort: a desktop without a notifier is not an error. Every caller
// reaches the desktop through here, so this is the one place the guard lives.
// `notifications.enabled` or `GTUNNEL_NOTIFICATIONS` (or the older
// `GTUNNEL_NO_NOTIFY`) is that guard. The `Notifier` trait only protects unit
// tests; an integration test runs the real binary, whose announcements would
// otherwise land on the desktop of whoever ran `cargo test`.
pub fn notify(n: &Notification) {
    let settings = crate::settings::current();
    if !settings.notifications {
        return;
    }
    let how = Options {
        style: settings.notification_style,
        osascript: settings.notification_osascript,
        backend: settings.notification_backend,
    };
    dispatch::PENDING.track(dispatch::spawn_chain(steps_for(n, &how)));
}

/// How a notification is delivered, as the settings resolve it
#[derive(Debug, Clone, Copy)]
struct Options {
    style: crate::config::NotifyStyle,
    osascript: bool,
    backend: crate::config::NotifyBackend,
}

impl Default for Options {
    /// What the settings resolve to when none is set
    fn default() -> Self {
        Self {
            style: crate::config::NotifyStyle::Plain,
            osascript: true,
            backend: crate::config::NotifyBackend::Auto,
        }
    }
}

/// The notifiers to try for `n`, first to last. Plain is what notifications
/// always were; rich reads the style of the notification's severity. The
/// terminal, when asked for, goes ahead of them, and a TUI that is not open or
/// a terminal that cannot show one is a failed step like any other: the chain
/// moves on to the desktop.
fn steps_for(n: &Notification, how: &Options) -> Vec<backend::Step> {
    let (title, steps) = match how.style {
        crate::config::NotifyStyle::Plain => (
            n.title().to_string(),
            backend::chain(n.title(), &n.body, how.osascript),
        ),
        crate::config::NotifyStyle::Rich => {
            let style = style::of(n.severity);
            let title = style::title(&style, n.title());
            let steps = backend::chain_rich(&title, &n.body, &style, how.osascript);
            (title, steps)
        }
    };
    if how.backend == crate::config::NotifyBackend::Terminal {
        let first = backend::Step::Terminal {
            title,
            body: n.body.clone(),
        };
        return std::iter::once(first).chain(steps).collect();
    }
    steps
}

/// The TUI has taken the terminal, and will write what is queued for it
pub fn open_terminal() {
    terminal::open();
}

/// The TUI has given the terminal back
pub fn close_terminal() {
    terminal::close();
}

/// The escape sequences queued for the terminal, for the TUI to write between
/// two frames
pub fn take_terminal_output() -> Vec<Vec<u8>> {
    terminal::drain()
}

/// What lifecycle code talks to, so a test can assert on what would have been
/// said instead of putting it on the desktop of whoever runs `cargo test`.
pub trait Notifier {
    fn send(&self, notification: &Notification);
}

pub struct Desktop;

impl Notifier for Desktop {
    fn send(&self, notification: &Notification) {
        notify(notification);
    }
}
