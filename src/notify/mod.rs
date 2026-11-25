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
    if !crate::settings::current().notifications {
        return;
    }
    let settings = crate::settings::current();
    let steps = steps_for(
        n,
        settings.notification_style,
        settings.notification_osascript,
    );
    dispatch::PENDING.track(dispatch::spawn_chain(steps));
}

/// The notifiers to try for `n`. Plain is what notifications always were; rich
/// reads the style of the notification's severity.
fn steps_for(
    n: &Notification,
    how: crate::config::NotifyStyle,
    osascript: bool,
) -> Vec<backend::Step> {
    match how {
        crate::config::NotifyStyle::Plain => backend::chain(n.title(), &n.body, osascript),
        crate::config::NotifyStyle::Rich => {
            let style = style::of(n.severity);
            backend::chain_rich(&style::title(&style, n.title()), &n.body, &style, osascript)
        }
    }
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
