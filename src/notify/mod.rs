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
mod spool;
mod style;
mod terminal;
#[cfg(test)]
mod tests;

pub use check::check;

/// How much a notification matters, least to most, so the most serious of
/// several can stand for all of them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
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
    // A process with no terminal hands the notification to a TUI that is open
    // and beating, which shows it by escape sequence; this process is the TUI
    // when the terminal is its own, and then the terminal step is what runs
    if how.backend == crate::config::NotifyBackend::Terminal
        && !terminal::is_open()
        && let Some(dir) = state_dir()
        && spool::tui_fresh(&dir, now_secs())
        && spool::write(&dir, n, now_secs()).is_ok()
    {
        return;
    }
    dispatch::PENDING.track(dispatch::spawn_chain(steps_for(n, &how)));
}

fn state_dir() -> Option<std::path::PathBuf> {
    crate::config::config_dir().ok()
}

fn now_secs() -> u64 {
    crate::lifecycle::now_secs()
}

/// Lets the notifications already sent finish before the process exits, and
/// says on stderr when one could not be delivered. Ahead of that, whatever a
/// TUI was handed and never took is delivered the ordinary way: the TUI went
/// away first, and an alert must not wait for the next one to be sent.
pub fn flush() {
    deliver_stale_spool();
    dispatch::flush();
}

fn deliver_stale_spool() {
    let settings = crate::settings::current();
    if settings.notification_backend != crate::config::NotifyBackend::Terminal
        || !settings.notifications
    {
        return;
    }
    let Some(dir) = state_dir() else {
        return;
    };
    let how = Options {
        style: settings.notification_style,
        osascript: settings.notification_osascript,
        // The terminal is the TUI's, and it did not take these
        backend: crate::config::NotifyBackend::Auto,
    };
    for entry in spool::take_stale(&dir, now_secs()) {
        let n = entry.into_notification();
        dispatch::PENDING.track(dispatch::spawn_chain(steps_for(&n, &how)));
    }
}

/// Whether this TUI is to take notifications from the watchdog: the person
/// asked for the terminal, notifications are on, and this terminal can show
/// one. Read once when the TUI starts, so a setting changed meanwhile applies
/// to the next one, and a TUI that is not asked pays nothing per tick.
pub fn terminal_handoff_wanted() -> bool {
    let settings = crate::settings::current();
    settings.notifications
        && settings.notification_backend == crate::config::NotifyBackend::Terminal
        && terminal::detect(&|name| std::env::var(name).ok()).is_some()
}

/// What such a TUI does on each tick: says it is here, and shows what a
/// process without a terminal left for it.
pub fn terminal_tick() {
    let Some(dir) = state_dir() else {
        return;
    };
    let _ = spool::beat(&dir, now_secs());
    let entries = spool::take_all(&dir);
    if entries.is_empty() {
        return;
    }
    let settings = crate::settings::current();
    for entry in entries {
        let n = entry.into_notification();
        let (title, body) = shown(&n, settings.notification_style);
        // A failed step here is a terminal that stopped being one; the entry
        // is already taken, so it is the desktop's now
        if terminal::deliver(&title, &body).is_err() {
            let how = Options {
                style: settings.notification_style,
                osascript: settings.notification_osascript,
                backend: crate::config::NotifyBackend::Auto,
            };
            dispatch::PENDING.track(dispatch::spawn_chain(steps_for(&n, &how)));
        }
    }
}

/// The title and the words a notification is shown with
fn shown(n: &Notification, how: crate::config::NotifyStyle) -> (String, String) {
    match how {
        crate::config::NotifyStyle::Plain => (n.title().to_string(), n.body.clone()),
        crate::config::NotifyStyle::Rich => (
            style::title(&style::of(n.severity), n.title()),
            n.body.clone(),
        ),
    }
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
