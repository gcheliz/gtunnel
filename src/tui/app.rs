use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::discovery::ExternalTunnel;
use crate::probe::{HealthStatus, MetricsHistory, TunnelMetrics};
use crate::state::{PersistentTunnel, TunnelSpec, TunnelStatus};

// Longer than any terminal is wide: the pane draws the tail it has room for, so
// a buffer sized to one guess at the width left the rest of the chart blank
pub const SPARKLINE_SAMPLES: usize = 240;
pub const METRICS_EVERY: Duration = Duration::from_secs(5);
pub const HEALTH_EVERY: Duration = Duration::from_secs(30);
pub const LB_EVERY: Duration = Duration::from_secs(60);
pub const DISCOVERY_EVERY: Duration = Duration::from_secs(60);

/// What the list shows for a tunnel name. The account is already in the header
/// and in the details pane, so a trailing `-<account>` says it a third time on
/// every row; it is dropped only when it is this entry's own account, so a
/// `k8s-prod` listed under `dev` still tells the truth. The stored name, and
/// every command that takes one, keeps the suffix.
///
/// The name is not cut here. How wide the column is depends on the pane, which
/// only the list knows, so the cut lives with the column fitter; a cap in this
/// function would hide the end of every long name however wide the terminal is.
///
/// Managed names are ASCII (`validate_tunnel_name`), so a character is a cell.
pub fn display_name(name: &str, account: &str) -> String {
    let stripped = if account.is_empty() {
        name
    } else {
        name.strip_suffix(account)
            .and_then(|rest| rest.strip_suffix('-'))
            // Nothing left means the whole name was the suffix, and an empty
            // cell cannot be told from the row beneath it
            .filter(|rest| !rest.is_empty())
            .unwrap_or(name)
    };
    stripped.to_string()
}

// Where a row came from, which decides what may be done to it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelKind {
    Managed,
    Ephemeral,
    External,
}

#[derive(Debug, Clone)]
pub struct TunnelEntry {
    pub tunnel: PersistentTunnel,
    pub status: TunnelStatus,
    pub kind: TunnelKind,
    pub metrics: Option<TunnelMetrics>,
    pub history: MetricsHistory,
    pub health: HealthStatus,
    pub external: Option<ExternalTunnel>,
}

const NOT_MANAGED: &str = "[not managed by gtunnel]";

impl TunnelEntry {
    pub fn new(tunnel: PersistentTunnel, status: TunnelStatus, kind: TunnelKind) -> Self {
        Self {
            tunnel,
            status,
            kind,
            metrics: None,
            history: MetricsHistory::new(SPARKLINE_SAMPLES),
            health: HealthStatus::Unknown,
            external: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.tunnel.name
    }

    // What the list's URL column shows. A process gtunnel does not manage has
    // no URL of its own here, so the column says whose it is instead; Details
    // still carries the real url
    pub fn url_cell(&self) -> String {
        match self.kind {
            TunnelKind::External => NOT_MANAGED.to_string(),
            _ => self.url(),
        }
    }

    pub fn display_name(&self) -> String {
        display_name(&self.tunnel.name, &self.tunnel.account_name)
    }

    // An unmanaged process does not always reveal its local port, and
    // "localhost:0" reads as a port rather than as "unknown"
    pub fn url(&self) -> String {
        match (self.kind, &self.tunnel.spec) {
            (TunnelKind::External, TunnelSpec::Access { local_port: 0, .. }) => "-".to_string(),
            _ => self.tunnel.display_url(),
        }
    }

    pub fn mode(&self) -> &'static str {
        if self.tunnel.is_expose() {
            "public"
        } else {
            "access"
        }
    }

    // The suffix that says a row is not an ordinary managed tunnel
    pub fn tag(&self) -> Option<&'static str> {
        match self.kind {
            TunnelKind::Managed => None,
            TunnelKind::Ephemeral => Some("[ephemeral]"),
            TunnelKind::External => Some(NOT_MANAGED),
        }
    }
}

// What the UI asks the runtime to do; every one of these touches the system
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Start(String),
    Stop(String),
    Restart(String),
    Delete(String),
    ToggleAutoStart(String),
    Import(String),
    ClearLog(String),
    Create(NewTunnel),
    Edit(EditTunnel),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TunnelMode {
    #[default]
    Access,
    Public,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTunnel {
    pub account: String,
    pub name: String,
    pub mode: TunnelMode,
    pub instance: String,
    pub zone: String,
    // What the instance reaches, when the target is not a port of the instance
    // itself: empty means the instance
    pub host: String,
    pub remote_port: u16,
    pub local_port: u16,
    pub local_target: String,
    pub domain: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditTunnel {
    pub account: String,
    pub name: String,
    // "<remote>:<local>" for Access, a local target or "private"/"public" for Expose
    pub value: String,
}

// What an Op produced, handed back to the UI thread
#[derive(Debug, Clone)]
pub struct OpResult {
    pub message: String,
    pub ok: bool,
    // The op changed tunnels.toml or a service, so the list is stale
    pub reload: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    ChooseMode,
    AddName,
    AddInstance,
    AddJumpHost,
    AddRemotePort,
    AddLocalPort,
    AddLocalTarget,
    AddDomain,
    EditTarget,
    Confirm,
    Help,
    Config,
}

impl InputMode {
    // The modes that read a line of text rather than a single key
    pub fn is_text(&self) -> bool {
        matches!(
            self,
            InputMode::AddName
                | InputMode::AddJumpHost
                | InputMode::AddRemotePort
                | InputMode::AddLocalPort
                | InputMode::AddLocalTarget
                | InputMode::EditTarget
        )
    }

    pub fn prompt(&self) -> &'static str {
        match self {
            InputMode::AddName => "Name:",
            InputMode::AddInstance => "Instance (type to filter):",
            InputMode::AddJumpHost => "Host it reaches (Enter for the instance itself):",
            InputMode::AddRemotePort => "Remote port:",
            InputMode::AddLocalPort => "Local port (Enter keeps the suggestion):",
            InputMode::AddLocalTarget => "Local target (e.g. localhost:3000):",
            InputMode::AddDomain => "Domain:",
            InputMode::EditTarget => "New value:",
            _ => "",
        }
    }
}

/// The two files the config page can show, in the order `Tab` cycles them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFile {
    Config,
    Tunnels,
}

impl ConfigFile {
    pub fn file_name(self) -> &'static str {
        match self {
            ConfigFile::Config => "config.toml",
            ConfigFile::Tunnels => "tunnels.toml",
        }
    }

    pub fn other(self) -> Self {
        match self {
            ConfigFile::Config => ConfigFile::Tunnels,
            ConfigFile::Tunnels => ConfigFile::Config,
        }
    }
}

/// How the last edit ended, said on the page. The log title is where a
/// failed operation shows, and says nothing of a finished one, and the page
/// covers it in any case, so a page that changes the world has to report on
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub ok: bool,
}

/// What the config page is showing. `text` is `None` when the file does not
/// exist, which the page says in words: an empty file and an absent one are
/// different facts, and `tunnels.toml` is absent until the first `add`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigView {
    pub file: ConfigFile,
    pub scroll: usize,
    pub text: Option<String>,
    // Why the last edit was not saved, with its line when the file gave one
    pub error: Option<String>,
    // The absolute path the title shows
    pub path: String,
    // The private copy the editor works on, kept after a refused edit so `e`
    // reopens the user's work and `Esc` is what throws it away
    pub staged: Option<std::path::PathBuf>,
    pub notice: Option<Notice>,
}

impl Default for ConfigView {
    fn default() -> Self {
        Self {
            file: ConfigFile::Config,
            scroll: 0,
            text: None,
            error: None,
            path: String::new(),
            staged: None,
            notice: None,
        }
    }
}

/// How far the keymap page scrolls on the terminal it was last drawn on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HelpExtent {
    pub max_scroll: usize,
    pub view: usize,
}

// An instance the picker offers, as list_instances reports it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceChoice {
    pub name: String,
    pub zone: String,
}

// Braille dots, shown while an Op is in flight
#[derive(Debug, Clone)]
pub struct Spinner {
    frames: Vec<char>,
    frame: usize,
    pub message: Option<String>,
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Spinner {
    pub fn new() -> Self {
        Self {
            frames: vec!['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'],
            frame: 0,
            message: None,
        }
    }

    pub fn start(&mut self, message: &str) {
        self.message = Some(message.to_string());
    }

    pub fn stop(&mut self) {
        self.message = None;
    }

    pub fn tick(&mut self) {
        self.frame = (self.frame + 1) % self.frames.len();
    }

    pub fn frame(&self) -> char {
        self.frames[self.frame]
    }

    pub fn is_running(&self) -> bool {
        self.message.is_some()
    }
}

/// Lets the editor own the keyboard.
///
/// The input thread polls stdin on its own, so while an editor runs it would
/// read keystrokes meant for the editor and send them to a list that is not on
/// screen. The thread checks this gate before every poll; `pause` raises it and
/// then waits for the thread to say it is parked, because a poll already in
/// flight would otherwise still swallow the first key the user types. The wait
/// is bounded, so a thread that has died cannot hang the editor.
#[derive(Default)]
pub struct InputGate {
    paused: AtomicBool,
    parked: AtomicBool,
    // No thread, no one to wait for: the demo and every test run without one
    live: AtomicBool,
}

/// How long `pause` waits for the input thread to park: a poll lasts 200ms.
const PARK_WAIT: Duration = Duration::from_millis(1000);

impl InputGate {
    pub fn mark_live(&self) {
        self.live.store(true, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// The input thread's side: asked at the top of every turn, true means
    /// skip the poll. Records that the thread is parked, or that it is not.
    pub fn hold(&self) -> bool {
        let paused = self.is_paused();
        self.parked.store(paused, Ordering::SeqCst);
        paused
    }

    /// Raises the gate until the returned guard is dropped, so every exit path
    /// from the editor, including a failed spawn and a panic, resumes input.
    pub fn pause(self: &Arc<Self>) -> Pause {
        self.paused.store(true, Ordering::SeqCst);
        if self.live.load(Ordering::SeqCst) {
            let deadline = std::time::Instant::now() + PARK_WAIT;
            while !self.parked.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Pause(self.clone())
    }
}

/// Resumes input when dropped.
pub struct Pause(Arc<InputGate>);

impl Drop for Pause {
    fn drop(&mut self) {
        // Parked first: a pause that follows at once must not read this
        // one's flag as the thread having stopped
        self.0.parked.store(false, Ordering::SeqCst);
        self.0.paused.store(false, Ordering::SeqCst);
    }
}

pub struct App {
    pub account: String,
    pub accounts: Vec<String>,
    // Keyed by account name so `;` swaps the subtitle in the same draw as the
    // name, with no second piece of state to fall out of step
    pub projects: HashMap<String, String>,
    // Account to its relay domains; an account with no entry has no relay.
    // Keyed like `projects` for the same reason, so `has_relay` and `domains`
    // are derived from the account on screen and never outlive a switch
    pub relays: HashMap<String, Vec<String>>,
    pub entries: Vec<TunnelEntry>,
    // False until a load has answered for this account. An empty list means
    // nothing before that, so the pane must not claim the account is empty
    pub loaded: bool,
    // The last load answered with an error and nothing has answered since. Only
    // read while `loaded` is false: once a list exists the status line carries
    // the error and the rows are the answer.
    pub load_failed: bool,
    pub selected: usize,
    // The first entry the list pane drew. A draw reads it and writes it back, so
    // it lives in a `Cell` for the reason `help_extent` does. Held rather than
    // derived from the selection, or the selected row sits on the bottom edge
    // after every move up and the list slides under the cursor.
    pub list_offset: Cell<usize>,
    pub logs: Vec<String>,
    pub status: Option<String>,
    // The last operation failed, so the status line says so in red
    pub status_error: bool,
    pub input_mode: InputMode,
    // The first row of the keymap page on screen. Zero whenever the page is
    // closed, so it always opens at the top.
    pub help_scroll: usize,
    // What the last draw of the page measured: the furthest it can scroll and
    // how many rows it shows. Only a draw knows the terminal's height, and
    // `draw` takes `&App`, so the measure comes back through a `Cell` rather
    // than a second pass that would have to guess the geometry.
    pub help_extent: Cell<HelpExtent>,
    // The config page scrolls on the same measure as the keymap page: only one
    // of them is ever open, so one `Cell` is enough and neither can read the
    // other's stale figure
    pub config_view: ConfigView,
    pub spinner: Spinner,
    pub should_quit: bool,
    pub suspend: bool,
    pub input: String,
    pub draft: NewTunnel,
    pub instances: Vec<InstanceChoice>,
    pub instance_selected: usize,
    pub domains: Vec<String>,
    pub domain_selected: usize,
    pub confirm_message: Option<String>,
    pub has_relay: bool,
    // Set when the instance picker opens, so the loop knows to fetch the list
    pub want_instances: bool,
    // Set by `r`, cleared once the event loop has rescheduled every timer
    pub force_refresh: bool,
    // An entry arrived with no health to carry, so the next round is owed one
    // whatever the clock says. Startup and `;` both land here: the list is
    // replaced wholesale and nothing in it has been probed.
    pub health_stale: bool,
    // The demo carries its own data and has no runtime to reload it from
    pub demo: bool,
    // What `c` and `o` reach outside the process, as plain function pointers
    // so a test can swap them without a trait object or a generic on `App`.
    // The defaults are the real clipboard and browser; under test they refuse,
    // so a key press can never copy to or open anything on the machine
    // running the suite.
    pub clipboard: fn(&str) -> anyhow::Result<()>,
    pub opener: fn(&str) -> anyhow::Result<()>,
    // What `e` on the config page runs on a file. The real one hands the
    // terminal to the user's editor; under test it refuses, for the reason the
    // two above do: a key press must never start a real editor.
    pub editor: fn(&Path) -> anyhow::Result<()>,
    pub input_gate: Arc<InputGate>,
    // An editor drew over the alternate screen, so the terminal's idea of what
    // is on it is wrong: the loop clears it and redraws in full, then lowers this
    pub clear_screen: bool,
}

impl App {
    pub fn new(initial_account: &str) -> Self {
        Self {
            account: initial_account.to_string(),
            accounts: vec![initial_account.to_string()],
            projects: HashMap::new(),
            relays: HashMap::new(),
            entries: Vec::new(),
            loaded: false,
            load_failed: false,
            selected: 0,
            list_offset: Cell::new(0),
            logs: Vec::new(),
            status: None,
            status_error: false,
            input_mode: InputMode::Normal,
            help_scroll: 0,
            help_extent: Cell::new(HelpExtent::default()),
            config_view: ConfigView::default(),
            spinner: Spinner::new(),
            should_quit: false,
            suspend: false,
            input: String::new(),
            draft: NewTunnel::default(),
            instances: Vec::new(),
            instance_selected: 0,
            domains: Vec::new(),
            domain_selected: 0,
            confirm_message: None,
            has_relay: false,
            want_instances: false,
            force_refresh: false,
            health_stale: false,
            demo: false,
            #[cfg(not(test))]
            clipboard: copy_to_clipboard,
            #[cfg(test)]
            clipboard: refuse_clipboard,
            #[cfg(not(test))]
            opener: open_url,
            #[cfg(test)]
            opener: refuse_opener,
            #[cfg(not(test))]
            editor: edit_in_terminal,
            #[cfg(test)]
            editor: refuse_editor,
            input_gate: Arc::new(InputGate::default()),
            clear_screen: false,
        }
    }

    /// The GCP project behind the current account, empty when the config does
    /// not say, so the header can drop the subtitle rather than print a blank.
    pub fn project(&self) -> &str {
        self.projects
            .get(&self.account)
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn selected_entry(&self) -> Option<&TunnelEntry> {
        self.entries.get(self.selected)
    }

    pub fn selected_sparkline(&self) -> Vec<u64> {
        self.selected_entry()
            .map(|e| e.history.samples())
            .unwrap_or_default()
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
        self.status_error = false;
    }

    /// A load that errored leaves `loaded` as it was: the list is still not an
    /// answer, and the timer asks again. The error goes to the status line,
    /// where the next successful operation replaces it, because dropping it left
    /// a broken `tunnels.toml` looking like an empty account. The pane says so
    /// too: `loading` is a claim that an answer is coming, and a broken file
    /// does not send one.
    pub fn load_failed(&mut self, error: &str) {
        self.load_failed = true;
        self.set_status(format!("could not load tunnels: {error}"));
        self.status_error = true;
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        let before = self.selected;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(1),
            _ => (self.selected + 1).min(last),
        };
        // The logs panel follows the selection, so what is on screen is stale —
        // but only once the selection has actually moved. Both ends of the list
        // clamp, and clearing there emptied the pane of a tunnel that stayed
        // selected: the loop refetches on a change of name, so nothing refilled
        // it and the tunnel read as silent until the log timer came round.
        if self.selected != before {
            self.logs.clear();
        }
    }

    // An External tunnel is someone else's process; gtunnel reports it and
    // refuses to touch it
    fn refuse_external(&mut self, action: &str) {
        let name = self
            .selected_entry()
            .map(|e| e.name().to_string())
            .unwrap_or_default();
        self.set_status(format!(
            "{name} is not managed by gtunnel: {action} is unavailable"
        ));
        // A cleared status is dropped the moment it is drawn, because a
        // finished operation should not leave the pane title claiming it is
        // still under way. A refusal has nothing behind it to finish, so
        // leaving it cleared meant the key did nothing and said nothing.
        self.status_error = true;
    }

    fn op_for(&mut self, action: &str, build: fn(String) -> Op) -> Option<Op> {
        let entry = self.selected_entry()?;
        if entry.kind == TunnelKind::External {
            self.refuse_external(action);
            return None;
        }
        let name = entry.name().to_string();
        self.spinner.start(&format!("{action} {name}"));
        Some(build(name))
    }

    pub fn next_account(&mut self) {
        if self.accounts.len() < 2 {
            return;
        }
        let current = self.accounts.iter().position(|a| *a == self.account);
        let next = current.map(|i| (i + 1) % self.accounts.len()).unwrap_or(0);
        self.account = self.accounts[next].clone();
        self.selected = 0;
        self.entries.clear();
        // The old account's answer says nothing about the new one's
        self.loaded = false;
        self.load_failed = false;
        self.list_offset.set(0);
        self.logs.clear();
        // The demo has no runtime behind it, so a refresh would leave the
        // screen empty: it swaps in the next account's synthetic data instead
        if self.demo {
            self.load_demo_account();
        } else {
            self.show_relay_for_account();
            self.force_refresh = true;
        }
        self.set_status(format!("account {}", self.account));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Op> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    self.should_quit = true;
                    return None;
                }
                KeyCode::Char('z') => {
                    self.suspend = true;
                    return None;
                }
                _ => return None,
            }
        }

        match self.input_mode {
            InputMode::Normal => {}
            InputMode::Help => {
                self.help_key(key.code);
                return None;
            }
            InputMode::Config => {
                self.config_key(key.code);
                return None;
            }
            InputMode::ChooseMode => return self.choose_mode(key.code),
            InputMode::Confirm => return self.confirm(key.code),
            InputMode::AddInstance => return self.pick_instance(key.code),
            InputMode::AddDomain => return self.pick_domain(key.code),
            mode if mode.is_text() => return self.type_line(key.code),
            _ => return None,
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.input_mode = InputMode::Help,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char(';') => self.next_account(),
            KeyCode::Char('r') => {
                self.force_refresh = true;
                self.set_status("refreshing");
            }
            KeyCode::Char('s') => return self.op_for("start", Op::Start),
            KeyCode::Char('S') => return self.op_for("stop", Op::Stop),
            KeyCode::Char('R') => return self.op_for("restart", Op::Restart),
            KeyCode::Char('d') => self.ask_delete(),
            KeyCode::Char('A') => return self.op_for("auto-start", Op::ToggleAutoStart),
            KeyCode::Char('m') => return self.import_op(),
            KeyCode::Char('c') => self.copy_url(),
            KeyCode::Char('o') => self.open_url(),
            KeyCode::Char('h') => self.force_health(),
            KeyCode::Char('a') => self.open_add(),
            KeyCode::Char('e') => self.open_edit(),
            KeyCode::Char('L') => return self.clear_log_op(),
            KeyCode::Char('C') => self.open_config(),
            _ => {}
        }
        None
    }

    // The page scrolls with the list's own keys and with the paging keys, and
    // only here: `PgUp`, `g` and `G` are not bound anywhere else. The position
    // is clamped to the last draw's measure before the move and after it, so a
    // press at either end is a no-op and the first press back always moves.
    fn scrolled(&self, from: usize, code: KeyCode) -> usize {
        let HelpExtent { max_scroll, view } = self.help_extent.get();
        let at = from.min(max_scroll);
        // A page keeps one row of the last screen, so the eye has a place to land
        let page = view.saturating_sub(1).max(1);
        match code {
            KeyCode::Down | KeyCode::Char('j') => at + 1,
            KeyCode::Up | KeyCode::Char('k') => at.saturating_sub(1),
            KeyCode::PageDown => at + page,
            KeyCode::PageUp => at.saturating_sub(page),
            KeyCode::Char('g') => 0,
            KeyCode::Char('G') => max_scroll,
            _ => at,
        }
        .min(max_scroll)
    }

    fn help_key(&mut self, code: KeyCode) {
        self.help_scroll = match code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.input_mode = InputMode::Normal;
                0
            }
            _ => self.scrolled(self.help_scroll, code),
        };
    }

    // The files are read when the page opens and when `Tab` moves to the other
    // one, never on a timer: an edit made elsewhere while the page is open is
    // not something this page promises to notice
    fn open_config(&mut self) {
        if self.demo {
            self.set_status("the demo has no config files");
            return;
        }
        match self.load_config_file(ConfigFile::Config) {
            Ok(()) => {
                self.help_extent.set(HelpExtent::default());
                self.input_mode = InputMode::Config;
            }
            Err(e) => {
                self.set_status(format!("could not open the config: {e}"));
                self.status_error = true;
            }
        }
    }

    fn load_config_file(&mut self, file: ConfigFile) -> anyhow::Result<()> {
        let path = page_dir()?.join(file.file_name());
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => anyhow::bail!("could not read {}: {e}", path.display()),
        };
        self.config_view = ConfigView {
            file,
            scroll: 0,
            text,
            error: None,
            path: std::path::absolute(&path)
                .unwrap_or(path)
                .display()
                .to_string(),
            staged: None,
            notice: None,
        };
        Ok(())
    }

    /// stage, edit, validate, commit, reload, in that order and no other.
    ///
    /// The real file changes in exactly one place, `commit`, and only after
    /// the text has passed the rules for that file. Anything earlier leaves it
    /// as it was, so an editor that crashes, a mistake in the TOML and a
    /// closed terminal are all the same harmless thing. A refused edit keeps
    /// its copy, and `e` reopens it: the user's work is never thrown away
    /// except by `Esc`, which says so.
    fn edit_config(&mut self) {
        use super::config_edit as edit;

        let file = self.config_view.file;
        self.config_view.notice = None;
        let real = match page_dir() {
            Ok(dir) => dir.join(file.file_name()),
            Err(e) => return self.fail(format!("could not edit {}: {e:#}", file.file_name())),
        };
        let fresh = self.config_view.staged.is_none();
        let staged = match self.config_view.staged.clone() {
            Some(staged) => staged,
            None => match edit::stage(&real) {
                Ok(staged) => staged,
                Err(e) => return self.fail(format!("could not prepare the edit: {e:#}")),
            },
        };

        let edited = self.run_editor(&staged);
        // Whatever happened, the editor drew over the screen
        self.clear_screen = true;
        let text = match edited {
            Ok(text) => text,
            Err(e) => {
                // A copy this press made is the edit that was cancelled. One
                // left from a refused edit holds work the user has not given up
                if fresh {
                    let _ = edit::discard(&staged);
                } else {
                    self.config_view.staged = Some(staged);
                }
                return self.fail(format!("{e:#}"));
            }
        };

        let current = std::fs::read_to_string(&real).unwrap_or_default();
        if text == current {
            let _ = edit::discard(&staged);
            self.config_view.staged = None;
            self.config_view.error = None;
            let _ = self.load_config_file(file);
            self.finish(format!("no changes to {}", file.file_name()));
            return;
        }

        enum Checked {
            Config(Box<crate::config::Config>),
            Tunnels(crate::state::TunnelState),
        }
        let checked = match file {
            ConfigFile::Config => {
                edit::validate_config(&text).map(|c| Checked::Config(Box::new(c)))
            }
            // Judged against the config on disk, which is the one the daemon
            // and the CLI will read, not whatever this process loaded at start
            ConfigFile::Tunnels => crate::config::load_config()
                .and_then(|config| edit::validate_tunnels(&text, &config))
                .map(Checked::Tunnels),
        };
        let checked = match checked {
            Ok(checked) => checked,
            Err(e) => return self.refuse(staged, text, format!("{e:#}")),
        };

        let before = crate::state::TunnelState::load().unwrap_or_default();
        if let Err(e) = edit::commit(&staged, &real) {
            return self.refuse(staged, text, format!("{e:#}"));
        }
        self.config_view.staged = None;
        // The page shows the file as it is now, not the copy it was edited in
        let _ = self.load_config_file(file);
        let message = match checked {
            Checked::Config(config) => {
                self.reload_config(&config);
                format!(
                    "reloaded: {}; running tunnels keep their old settings until R",
                    file.file_name()
                )
            }
            Checked::Tunnels(after) => {
                self.force_refresh = true;
                edit::summarise(&before, &after).line()
            }
        };
        self.finish(message);
    }

    // The edit ended without a refusal: say so on the page and on the status
    fn finish(&mut self, message: String) {
        self.set_status(message.clone());
        self.config_view.notice = Some(Notice {
            text: message,
            ok: true,
        });
    }

    // The edit stays on the page, with its copy, and says why it was not saved
    fn refuse(&mut self, staged: std::path::PathBuf, text: String, error: String) {
        self.config_view.staged = Some(staged);
        self.config_view.text = Some(text);
        self.config_view.error = Some(error.clone());
        // The page carries the error itself, with its keys
        self.set_status(format!("not saved: {error}"));
        self.status_error = true;
    }

    fn fail(&mut self, message: String) {
        self.set_status(message.clone());
        self.status_error = true;
        self.config_view.notice = Some(Notice {
            text: message,
            ok: false,
        });
    }

    /// Takes in a config that changed under a running TUI: accounts, projects,
    /// the relay for the account on screen, and a fresh request for the list.
    ///
    /// The account on screen survives when the new config still has it. When
    /// it does not, its rows must not linger under another account's name, so
    /// the list is emptied and the first remaining account takes over, as
    /// `Config::remove_account` does for the stored selection.
    pub fn reload_config(&mut self, config: &crate::config::Config) {
        self.accounts = config.accounts.iter().map(|a| a.name.clone()).collect();
        self.projects = config
            .accounts
            .iter()
            .map(|a| (a.name.clone(), a.project.clone()))
            .collect();
        if !self.accounts.contains(&self.account)
            && let Some(first) = self.accounts.first()
        {
            self.account = first.clone();
            self.selected = 0;
            self.entries.clear();
            self.loaded = false;
            self.load_failed = false;
            self.list_offset.set(0);
            self.logs.clear();
        }
        self.relays = config
            .accounts
            .iter()
            .filter_map(|a| {
                let expose = a.expose.as_ref()?;
                let domains = expose.domains.iter().map(|d| d.domain.clone());
                Some((a.name.clone(), domains.collect()))
            })
            .collect();
        self.show_relay_for_account();
        self.force_refresh = true;
    }

    /// The Public flow and the domain picker only make sense with a relay, and
    /// whether there is one belongs to the account on screen.
    fn show_relay_for_account(&mut self) {
        self.has_relay = self.relays.contains_key(&self.account);
        self.domains = self.relays.get(&self.account).cloned().unwrap_or_default();
        self.domain_selected = 0;
    }

    fn config_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if let Some(staged) = self.config_view.staged.take() {
                    let _ = super::config_edit::discard(&staged);
                    self.set_status(format!(
                        "discarded the edit to {}",
                        self.config_view.file.file_name()
                    ));
                }
                self.input_mode = InputMode::Normal;
                self.config_view = ConfigView::default();
            }
            // A refused edit is waiting here for `e` or `Esc`; moving to the
            // other file would strand its copy
            KeyCode::Tab if self.config_view.staged.is_some() => {}
            KeyCode::Tab => {
                let other = self.config_view.file.other();
                if let Err(e) = self.load_config_file(other) {
                    self.set_status(format!("{e}"));
                    self.status_error = true;
                }
                self.help_extent.set(HelpExtent::default());
            }
            KeyCode::Char('e') => self.edit_config(),
            _ => self.config_view.scroll = self.scrolled(self.config_view.scroll, code),
        }
    }

    // Only an ephemeral run has a name and a spec already; an external tunnel
    // would need one invented, which is a dialog
    fn import_op(&mut self) -> Option<Op> {
        let entry = self.selected_entry()?;
        match entry.kind {
            TunnelKind::External => {
                self.refuse_external("import");
                None
            }
            TunnelKind::Managed => {
                self.set_status(format!("{} is already managed", entry.name()));
                None
            }
            TunnelKind::Ephemeral => {
                let name = entry.name().to_string();
                self.spinner.start(&format!("import {name}"));
                Some(Op::Import(name))
            }
        }
    }

    // Only a managed tunnel keeps a log file: a run writes to the terminal that
    // started it, and an unmanaged process writes wherever its owner sent it
    fn clear_log_op(&mut self) -> Option<Op> {
        let entry = self.selected_entry()?;
        match entry.kind {
            TunnelKind::External => {
                self.refuse_external("clear the log of");
                None
            }
            TunnelKind::Ephemeral => {
                self.set_status(format!(
                    "{} is a run: its log goes to the terminal that started it",
                    entry.name()
                ));
                None
            }
            TunnelKind::Managed => {
                let name = entry.name().to_string();
                // The panel follows the file, so it empties with it instead of
                // showing lines until the next reload
                self.logs.clear();
                self.spinner.start(&format!("clear log {name}"));
                Some(Op::ClearLog(name))
            }
        }
    }

    fn force_health(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.selected) {
            entry.health = HealthStatus::Checking;
        }
        self.force_refresh = true;
    }

    fn copy_url(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let url = entry.tunnel.display_url();
        match (self.clipboard)(&url) {
            Ok(()) => self.set_status(format!("copied {url}")),
            Err(e) => self.set_status(format!("could not copy: {e}")),
        }
    }

    fn open_url(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let url = match &entry.tunnel.spec {
            TunnelSpec::Access { local_port, .. } => format!("http://localhost:{local_port}"),
            TunnelSpec::Expose { hostname, .. } => format!("https://{hostname}"),
        };
        match (self.opener)(&url) {
            Ok(()) => self.set_status(format!("opened {url}")),
            Err(e) => self.set_status(format!("could not open {url}: {e}")),
        }
    }

    /// Runs the editor on `path` with input paused, and returns what the file
    /// holds afterwards. The pause is a guard, so it lifts on an editor that
    /// failed as surely as on one that did not.
    pub fn run_editor(&mut self, path: &Path) -> anyhow::Result<String> {
        let _paused = self.input_gate.pause();
        (self.editor)(path)?;
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("could not read {} back: {e}", path.display()))
    }

    // --- dialogs ---

    fn cancel_dialog(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input.clear();
        self.confirm_message = None;
    }

    fn open_add(&mut self) {
        self.draft = NewTunnel {
            account: self.account.clone(),
            ..NewTunnel::default()
        };
        self.input.clear();
        self.input_mode = InputMode::ChooseMode;
    }

    fn choose_mode(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Char('1') => {
                self.draft.mode = TunnelMode::Access;
                self.input_mode = InputMode::AddName;
            }
            KeyCode::Char('2') => {
                if !self.has_relay {
                    self.cancel_dialog();
                    self.set_status(
                        "This account has no relay. Run `gtunnel relay up` before publishing a URL",
                    );
                    return None;
                }
                self.draft.mode = TunnelMode::Public;
                self.input_mode = InputMode::AddName;
            }
            KeyCode::Esc => self.cancel_dialog(),
            _ => {}
        }
        None
    }

    fn ask_delete(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.kind == TunnelKind::External {
            self.refuse_external("delete");
            return;
        }
        self.confirm_message = Some(format!("Delete {}? (y/n)", entry.name()));
        self.input_mode = InputMode::Confirm;
    }

    fn confirm(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.cancel_dialog();
                self.op_for("delete", Op::Delete)
            }
            _ => {
                self.cancel_dialog();
                None
            }
        }
    }

    fn open_edit(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.kind == TunnelKind::External {
            self.refuse_external("edit");
            return;
        }
        self.input = match &entry.tunnel.spec {
            TunnelSpec::Access {
                remote_port,
                local_port,
                ..
            } => format!("{remote_port}:{local_port}"),
            TunnelSpec::Expose { local_target, .. } => local_target.clone(),
        };
        self.input_mode = InputMode::EditTarget;
    }

    // The next free port in gtunnel's own range, so the suggestion never
    // collides with a tunnel already on screen
    pub fn suggested_local_port(&self) -> u16 {
        let taken: std::collections::HashSet<u16> = self
            .entries
            .iter()
            .filter_map(|e| match e.tunnel.spec {
                TunnelSpec::Access { local_port, .. } => Some(local_port),
                _ => None,
            })
            .collect();
        (15_000..16_000)
            .find(|port| !taken.contains(port))
            .unwrap_or(15_000)
    }

    fn type_line(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Esc => {
                self.cancel_dialog();
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                None
            }
            KeyCode::Enter => self.advance(),
            _ => None,
        }
    }

    fn advance(&mut self) -> Option<Op> {
        let value = std::mem::take(&mut self.input);
        match self.input_mode {
            InputMode::AddName => {
                if value.trim().is_empty() {
                    self.input = value;
                    return None;
                }
                self.draft.name = value.trim().to_string();
                self.input_mode = match self.draft.mode {
                    TunnelMode::Access => {
                        self.want_instances = true;
                        self.instance_selected = 0;
                        InputMode::AddInstance
                    }
                    TunnelMode::Public => InputMode::AddLocalTarget,
                };
                None
            }
            InputMode::AddJumpHost => {
                self.draft.host = value.trim().to_string();
                self.input_mode = InputMode::AddRemotePort;
                None
            }
            InputMode::AddRemotePort => match value.trim().parse::<u16>() {
                Ok(port) => {
                    self.draft.remote_port = port;
                    self.draft.local_port = self.suggested_local_port();
                    self.input_mode = InputMode::AddLocalPort;
                    None
                }
                Err(_) => {
                    self.set_status(format!("{value:?} is not a port"));
                    None
                }
            },
            InputMode::AddLocalPort => {
                if !value.trim().is_empty() {
                    match value.trim().parse::<u16>() {
                        Ok(port) => self.draft.local_port = port,
                        Err(_) => {
                            self.set_status(format!("{value:?} is not a port"));
                            return None;
                        }
                    }
                }
                let draft = std::mem::take(&mut self.draft);
                self.cancel_dialog();
                self.spinner.start(&format!("create {}", draft.name));
                Some(Op::Create(draft))
            }
            InputMode::AddLocalTarget => {
                if value.trim().is_empty() {
                    return None;
                }
                self.draft.local_target = value.trim().to_string();
                self.domain_selected = 0;
                self.input_mode = InputMode::AddDomain;
                None
            }
            InputMode::EditTarget => {
                let name = self.selected_entry()?.name().to_string();
                let edit = EditTunnel {
                    account: self.account.clone(),
                    name: name.clone(),
                    value: value.trim().to_string(),
                };
                self.cancel_dialog();
                self.spinner.start(&format!("edit {name}"));
                Some(Op::Edit(edit))
            }
            _ => None,
        }
    }

    // The picker filters as you type, so a long instance list stays usable
    pub fn filtered_instances(&self) -> Vec<&InstanceChoice> {
        let needle = self.input.to_lowercase();
        self.instances
            .iter()
            .filter(|i| needle.is_empty() || i.name.to_lowercase().contains(&needle))
            .collect()
    }

    fn pick_instance(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Esc => self.cancel_dialog(),
            KeyCode::Backspace => {
                self.input.pop();
                self.instance_selected = 0;
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.instance_selected = 0;
            }
            KeyCode::Down => {
                let last = self.filtered_instances().len().saturating_sub(1);
                self.instance_selected = (self.instance_selected + 1).min(last);
            }
            KeyCode::Up => self.instance_selected = self.instance_selected.saturating_sub(1),
            KeyCode::Enter => {
                let chosen = self
                    .filtered_instances()
                    .get(self.instance_selected)
                    .map(|i| (*i).clone());
                let Some(chosen) = chosen else {
                    self.set_status("No instance matches that filter");
                    return None;
                };
                self.draft.instance = chosen.name;
                self.draft.zone = chosen.zone;
                self.input.clear();
                self.input_mode = InputMode::AddJumpHost;
            }
            _ => {}
        }
        None
    }

    fn pick_domain(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Esc => self.cancel_dialog(),
            KeyCode::Down => {
                let last = self.domains.len().saturating_sub(1);
                self.domain_selected = (self.domain_selected + 1).min(last);
            }
            KeyCode::Up => self.domain_selected = self.domain_selected.saturating_sub(1),
            KeyCode::Enter => {
                let Some(domain) = self.domains.get(self.domain_selected).cloned() else {
                    self.set_status("This account has no domain. Run `gtunnel domains add`");
                    return None;
                };
                self.draft.domain = domain;
                let draft = std::mem::take(&mut self.draft);
                self.cancel_dialog();
                self.spinner.start(&format!("create {}", draft.name));
                return Some(Op::Create(draft));
            }
            _ => {}
        }
        None
    }

    pub fn apply(&mut self, result: OpResult) {
        self.spinner.stop();
        self.set_status(result.message);
        self.status_error = !result.ok;
        if result.reload {
            self.force_refresh = true;
        }
    }

    /// Drop one tunnel's last measurement, so the next one is a first sight.
    /// Used after an op that emptied the tunnel's log: that count belongs to
    /// the run whose lines are gone, and comparing against it writes a drop
    /// nobody on this run caused.
    pub fn forget_metrics(&mut self, name: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name() == name) {
            entry.metrics = None;
        }
    }

    // Keeps the selection on the same tunnel across a reload
    pub fn replace_entries(&mut self, entries: Vec<TunnelEntry>) {
        self.loaded = true;
        self.load_failed = false;
        let selected_name = self.selected_entry().map(|e| e.name().to_string());
        let carried = std::mem::take(&mut self.entries);
        self.entries = entries
            .into_iter()
            .map(|mut entry| {
                match carried.iter().find(|o| o.name() == entry.name()) {
                    Some(old) => {
                        entry.history = old.history.clone();
                        entry.health = old.health;
                        entry.metrics = old.metrics.clone();
                    }
                    // Nothing to carry: this row has never been probed, and
                    // waiting out the period would leave it reading `?`
                    None => self.health_stale = true,
                }
                entry
            })
            .collect();
        self.selected = selected_name
            .and_then(|name| self.entries.iter().position(|e| e.name() == name))
            .unwrap_or(0);
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }
}

// The three below are what `App::new` wires in outside tests; a test build
// replaces them with refusals, so they go unused there by design.
#[cfg_attr(test, allow(dead_code))]
fn run_piped(program: &str, args: &[&str], input: &str) -> std::io::Result<bool> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(input.as_bytes())?;
    }
    Ok(child.wait()?.success())
}

#[cfg(test)]
fn refuse_clipboard(_: &str) -> anyhow::Result<()> {
    anyhow::bail!("the clipboard is off under test")
}

/// The directory the config page reads. Under test it demands the override
/// that `TempConfigDir` sets, so a test that forgot its directory fails here
/// instead of reading the real configuration of whoever runs the suite.
fn page_dir() -> anyhow::Result<std::path::PathBuf> {
    #[cfg(test)]
    if std::env::var_os("GTUNNEL_CONFIG_DIR").is_none() {
        anyhow::bail!("the real config directory is off under test");
    }
    crate::config::config_dir()
}

#[cfg(test)]
fn refuse_editor(_: &Path) -> anyhow::Result<()> {
    anyhow::bail!("the editor is off under test")
}

#[cfg(test)]
fn refuse_opener(_: &str) -> anyhow::Result<()> {
    anyhow::bail!("the browser is off under test")
}

#[cfg_attr(test, allow(dead_code))]
pub fn copy_to_clipboard(text: &str) -> anyhow::Result<()> {
    let candidates: [(&str, &[&str]); 3] = [
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
    ];
    for (program, args) in candidates {
        if let Ok(true) = run_piped(program, args, text) {
            return Ok(());
        }
    }
    anyhow::bail!("no pbcopy, wl-copy or xclip on PATH")
}

#[cfg_attr(test, allow(dead_code))]
pub fn open_url(url: &str) -> anyhow::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

/// Hands the terminal to the user's editor and takes it back.
///
/// The screen is left before the editor starts and entered again by a guard, so
/// a failed spawn, a non-zero exit and a panic all land back in the TUI instead
/// of leaving the shell in raw mode. The event loop clears the screen
/// afterwards: the editor drew over the alternate screen, and the diff the
/// terminal draws from would otherwise paint only what it thinks changed.
#[cfg_attr(test, allow(dead_code))]
pub fn edit_in_terminal(path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;

    struct Handoff;
    impl Handoff {
        fn leave() -> Self {
            ratatui::restore();
            Handoff
        }
    }
    impl Drop for Handoff {
        fn drop(&mut self) {
            use crossterm::{execute, terminal};
            let _ = terminal::enable_raw_mode();
            let _ = execute!(std::io::stdout(), terminal::EnterAlternateScreen);
        }
    }

    let (program, args) = super::config_edit::resolve_editor(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
    );
    let _handoff = Handoff::leave();
    let status = std::process::Command::new(&program)
        .args(&args)
        .arg(path)
        .status()
        .with_context(|| format!("could not run {program}"))?;
    if !status.success() {
        anyhow::bail!("{program} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod display_name_tests {
    use super::*;

    #[test]
    fn a_suffix_equal_to_the_account_is_not_shown() {
        assert_eq!(display_name("k8s-dev", "dev"), "k8s");
        assert_eq!(display_name("redis-dev", "dev"), "redis");
    }

    // The suffix says which account a tunnel is on only when it is that account's
    #[test]
    fn a_suffix_that_names_another_account_stays() {
        assert_eq!(display_name("k8s-prod", "dev"), "k8s-prod");
    }

    // An empty cell is a row nobody can tell apart from the next
    #[test]
    fn a_name_that_would_vanish_is_shown_whole() {
        assert_eq!(display_name("dev", "dev"), "dev");
        assert_eq!(display_name("-dev", "dev"), "-dev");
        assert_eq!(display_name("anything", ""), "anything");
    }

    // The list sizes its name column to the longest name, so the name reaches it
    // whole; the cut, when the pane is too narrow, is the column fitter's
    #[test]
    fn a_long_name_is_not_cut_by_display_name() {
        let long = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(display_name(long, "dev"), long);
    }

    #[test]
    fn a_long_name_keeps_all_of_itself_after_its_suffix_is_dropped() {
        assert_eq!(
            display_name("mongo-activitylog-prod", "prod"),
            "mongo-activitylog"
        );
        assert_eq!(
            display_name("mongo-production-prod", "prod"),
            "mongo-production"
        );
    }

    // Two tunnels of one account must never read as the same row; this guards
    // the shape of that, and the real config is checked in the evidence run.
    #[test]
    fn cut_names_of_one_account_stay_distinct() {
        let names = [
            "k8s-prod",
            "redis-prod",
            "mongo-activitylog-prod",
            "mongo-production-prod",
            "mongo-payments-prod",
            "mongo-billing-prod",
            "postgres-primary-prod",
            "postgres-replica-prod",
        ];
        let shown: std::collections::HashSet<String> =
            names.iter().map(|n| display_name(n, "prod")).collect();
        assert_eq!(shown.len(), names.len(), "{shown:?}");
    }

    #[test]
    fn an_entry_uses_its_own_account_for_the_suffix() {
        use crate::state::AccessTarget;
        let tunnel = PersistentTunnel {
            name: "k8s-dev".to_string(),
            account_name: "dev".to_string(),
            auto_start: false,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "vm".to_string(),
                    zone: "z".to_string(),
                },
                remote_port: 1,
                local_port: 2,
            },
        };
        let entry = TunnelEntry::new(tunnel, TunnelStatus::Stopped, TunnelKind::Managed);
        assert_eq!(entry.display_name(), "k8s");
        assert_eq!(entry.name(), "k8s-dev");
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;
    use std::cell::RefCell;

    fn writes_known_text(path: &Path) -> anyhow::Result<()> {
        std::fs::write(path, "edited = true\n")?;
        Ok(())
    }

    fn fails(_: &Path) -> anyhow::Result<()> {
        anyhow::bail!("editor exited with status: 1")
    }

    thread_local! {
        // What the stand-in saw of the gate while it ran: a function pointer
        // carries no state, and a test owns its thread
        static GATE: RefCell<Option<Arc<InputGate>>> = const { RefCell::new(None) };
        static SAW_PAUSED: RefCell<Option<bool>> = const { RefCell::new(None) };
    }

    fn looks_at_the_gate(_: &Path) -> anyhow::Result<()> {
        GATE.with(|g| {
            let paused = g.borrow().as_ref().map(|g| g.is_paused());
            SAW_PAUSED.with(|s| *s.borrow_mut() = paused);
        });
        Ok(())
    }

    fn scratch() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tunnels.toml");
        std::fs::write(&file, "before = 1\n").unwrap();
        (dir, file)
    }

    #[test]
    fn run_editor_returns_what_the_editor_wrote_and_resumes_input() {
        let (_dir, file) = scratch();
        let mut app = App::new("dev");
        app.editor = writes_known_text;
        let text = app.run_editor(&file).expect("editor ran");
        assert_eq!(text, "edited = true\n");
        assert!(!app.input_gate.is_paused(), "input must resume");
    }

    #[test]
    fn input_is_paused_for_the_whole_editor_run() {
        let (_dir, file) = scratch();
        let mut app = App::new("dev");
        app.editor = looks_at_the_gate;
        GATE.with(|g| *g.borrow_mut() = Some(app.input_gate.clone()));
        app.run_editor(&file).unwrap();
        assert_eq!(SAW_PAUSED.with(|s| *s.borrow()), Some(true));
        assert!(!app.input_gate.is_paused());
    }

    #[test]
    fn a_failing_editor_still_resumes_input_and_returns_its_error() {
        let (_dir, file) = scratch();
        let mut app = App::new("dev");
        app.editor = fails;
        let err = app.run_editor(&file).expect_err("the editor failed");
        assert!(err.to_string().contains("status: 1"), "{err}");
        assert!(!app.input_gate.is_paused(), "input must resume on failure");
    }

    #[test]
    fn under_test_the_default_editor_refuses() {
        let (_dir, file) = scratch();
        let mut app = App::new("dev");
        let err = app.run_editor(&file).expect_err("refused");
        assert!(err.to_string().contains("off under test"), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "before = 1\n");
        assert!(!app.input_gate.is_paused());
    }

    // The thread this gate talks to checks `hold` before every poll. The
    // guard must not return before that thread has parked, or a poll in
    // flight would take the editor's first keystroke.
    #[test]
    fn pausing_waits_for_a_live_input_thread_to_park() {
        let gate = Arc::new(InputGate::default());
        gate.mark_live();
        let worker = {
            let gate = gate.clone();
            std::thread::spawn(move || {
                // A poll in flight: it notices the gate only after a while
                std::thread::sleep(Duration::from_millis(80));
                while !gate.hold() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                // Parked: stays so until released
                while gate.hold() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        let started = std::time::Instant::now();
        let guard = gate.pause();
        assert!(gate.is_paused());
        assert!(
            gate.parked.load(Ordering::SeqCst),
            "pause returned before the thread parked"
        );
        assert!(started.elapsed() >= Duration::from_millis(60));
        drop(guard);
        assert!(!gate.is_paused());
        worker.join().unwrap();
    }

    #[test]
    fn pausing_does_not_wait_when_no_thread_is_listening() {
        let gate = Arc::new(InputGate::default());
        let started = std::time::Instant::now();
        let _guard = gate.pause();
        assert!(started.elapsed() < Duration::from_millis(200));
    }
}
