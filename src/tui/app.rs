use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
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

pub struct App {
    pub account: String,
    pub accounts: Vec<String>,
    // Keyed by account name so `;` swaps the subtitle in the same draw as the
    // name, with no second piece of state to fall out of step
    pub projects: HashMap<String, String>,
    pub entries: Vec<TunnelEntry>,
    pub selected: usize,
    pub logs: Vec<String>,
    pub status: Option<String>,
    // The last operation failed, so the status line says so in red
    pub status_error: bool,
    pub input_mode: InputMode,
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
    // The demo carries its own data and has no runtime to reload it from
    pub demo: bool,
}

impl App {
    pub fn new(initial_account: &str) -> Self {
        Self {
            account: initial_account.to_string(),
            accounts: vec![initial_account.to_string()],
            projects: HashMap::new(),
            entries: Vec::new(),
            selected: 0,
            logs: Vec::new(),
            status: None,
            status_error: false,
            input_mode: InputMode::Normal,
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
            demo: false,
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

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(1),
            _ => (self.selected + 1).min(last),
        };
        // The logs panel follows the selection, so what is on screen is stale
        self.logs.clear();
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
        self.logs.clear();
        // The demo has no runtime behind it, so a refresh would leave the
        // screen empty: it swaps in the next account's synthetic data instead
        if self.demo {
            self.load_demo_account();
        } else {
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
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    self.input_mode = InputMode::Normal;
                }
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
            _ => {}
        }
        None
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
        match copy_to_clipboard(&url) {
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
        match open_url(&url) {
            Ok(()) => self.set_status(format!("opened {url}")),
            Err(e) => self.set_status(format!("could not open {url}: {e}")),
        }
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

    // Keeps the selection on the same tunnel across a reload
    pub fn replace_entries(&mut self, entries: Vec<TunnelEntry>) {
        let selected_name = self.selected_entry().map(|e| e.name().to_string());
        let carried = std::mem::take(&mut self.entries);
        self.entries = entries
            .into_iter()
            .map(|mut entry| {
                if let Some(old) = carried.iter().find(|o| o.name() == entry.name()) {
                    entry.history = old.history.clone();
                    entry.health = old.health;
                    entry.metrics = old.metrics.clone();
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
