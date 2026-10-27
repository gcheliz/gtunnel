use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::discovery::ExternalTunnel;
use crate::probe::{HealthStatus, MetricsHistory, RestartWatch, TunnelMetrics};
use crate::state::{
    allocate_port, PersistentTunnel, TunnelSpec, TunnelStatus, ACCESS_PORT_BASE, ACCESS_PORT_SPAN,
};

// Longer than any terminal is wide: the pane draws the tail it has room for, so
// a buffer sized to one guess at the width left the rest of the chart blank
pub const SPARKLINE_SAMPLES: usize = 240;
pub const METRICS_EVERY: Duration = Duration::from_secs(5);
// A connection count read once every five seconds lands between two requests
// and reports an idle tunnel under real traffic, so the count is sampled on
// its own clock and the pane reports the peak. Ten samples a render is as fine
// as the reading is worth: one listing covers every tunnel, so the cost is two
// short `lsof` runs a second however many tunnels are open.
pub const SAMPLE_EVERY: Duration = Duration::from_millis(500);
pub const HEALTH_EVERY: Duration = Duration::from_secs(5);
pub const LB_EVERY: Duration = Duration::from_secs(60);
pub const DISCOVERY_EVERY: Duration = Duration::from_secs(60);
// A started service reports its pid before its gcloud child is listening, so
// the refresh a start forces reads a tunnel that is not up yet. One more look
// this long after lets the status settle without waiting out a whole round.
pub const START_FOLLOWUP: Duration = Duration::from_secs(3);

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

/// An op running on one row. The row says so from the frame of the key press,
/// because the op itself returns seconds later and a row that stays as it was
/// reads as a key that did nothing, which is what invites the second press.
#[derive(Debug, Clone, Copy)]
pub struct PendingOp {
    pub verb: &'static str,
    pub since: std::time::Instant,
}

impl PendingOp {
    /// The verb as the row spells it. `stop` doubles its consonant and the
    /// rest only take `ing`, so the one exception is named here and not
    /// guessed by suffix.
    pub fn gerund(&self) -> String {
        match self.verb {
            "stop" => "stopping".to_string(),
            verb => format!("{verb}ing"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TunnelEntry {
    pub tunnel: PersistentTunnel,
    pub status: TunnelStatus,
    pub kind: TunnelKind,
    pub metrics: Option<TunnelMetrics>,
    pub history: MetricsHistory,
    pub health: HealthStatus,
    // Restart counts across metrics rounds, and what they add up to: a loop
    // is growth inside a window, which one round's count cannot say
    pub restart_watch: RestartWatch,
    pub crash_looping: bool,
    pub external: Option<ExternalTunnel>,
    pub pending: Option<PendingOp>,
    // Set when the op has returned: the first entries round begun after that
    // is the one that shows what the op did, and only then does `pending` go
    pub pending_settle: bool,
    /// Unix seconds at which a live `gtunnel run` ends by itself; `None` for a
    /// declared tunnel and for a run started without a TTL.
    pub expires_at: Option<u64>,
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
            restart_watch: RestartWatch::default(),
            crash_looping: false,
            external: None,
            pending: None,
            pending_settle: false,
            expires_at: None,
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
        self.tunnel.display_kind()
    }

    // The suffix that says a row is not an ordinary managed tunnel. A run with
    // a TTL says when it ends, in the words `gtunnel list` uses for it; one
    // without keeps `[ephemeral]`, which also names the public previews that
    // `list` marks `[run]`, so the row does not change what it says by gaining
    // a clock.
    pub fn tag(&self) -> Option<std::borrow::Cow<'static, str>> {
        use std::borrow::Cow;
        match (self.kind, self.expires_at) {
            (TunnelKind::Managed, _) => None,
            (TunnelKind::Ephemeral, Some(at)) => Some(Cow::Owned(
                crate::commands::run_note(Some(at)).trim().to_string(),
            )),
            (TunnelKind::Ephemeral, None) => Some(Cow::Borrowed("[ephemeral]")),
            (TunnelKind::External, _) => Some(Cow::Borrowed(NOT_MANAGED)),
        }
    }
}

// What the UI asks the runtime to do; every one of these touches the system
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Start(String),
    Stop(String),
    // Ends a run, which `stop_tunnel` cannot find: it is not in `tunnels.toml`
    StopRun(String),
    Restart(String),
    Delete(String),
    ToggleAutoStart(String),
    Import(String),
    ClearLog(String),
    Create(NewTunnel),
    Edit(EditTunnel),
    Rename(RenameTunnel),
    Move(MoveTunnel),
    AccountAdd(NewAccount),
    AccountEdit(AccountEdit),
    AccountRemove(String),
    AccountSelect(String),
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

/// What `gtunnel rename` is asked for. The account is named because a rename
/// never leaves it: the name is unique per account, so the pair is what
/// identifies the tunnel being renamed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenameTunnel {
    pub account: String,
    pub name: String,
    pub new_name: String,
}

/// What `gtunnel move` is asked for. `rename_to` is the name the tunnel lands
/// under when the destination account already holds this one: the dialog asks
/// for it rather than giving the move up, because the clash is the only thing
/// wrong with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MoveTunnel {
    pub name: String,
    pub from: String,
    pub to: String,
    pub rename_to: Option<String>,
}

/// What the accounts page sends to `gtunnel account add`: a gcloud
/// configuration and a name. There is no project field, because the project
/// and the zone are read from the configuration, which is the one place that
/// already knows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewAccount {
    pub configuration: String,
    pub name: String,
}

/// An edit of one account. Each field is `None` when it is unchanged, and
/// never `Some("")`: `cmd_account_edit` refuses both an empty value and a call
/// with nothing to change, so the page must not send either.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountEdit {
    pub name: String,
    pub project: Option<String>,
    pub zone: Option<String>,
}

/// What the page's two dialogs have gathered. One struct for both, because
/// the fields they collect do not overlap: add asks for a configuration and a
/// name, edit knows the name already and asks for a project and a zone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountDraft {
    pub configuration: String,
    pub name: String,
    pub project: String,
}

// What an Op produced, handed back to the UI thread
#[derive(Debug, Clone)]
pub struct OpResult {
    pub message: String,
    pub ok: bool,
    // The op changed tunnels.toml or a service, so the list is stale
    pub reload: bool,
    // Ops overlap, so only a create's own result may settle a create's draft
    pub from_create: bool,
    // The op changed config.toml, so the accounts page and the header are
    // stale as well, and the page is where the result has to be read: a
    // refusal goes to a status line the page is covering
    pub from_account: bool,
    // The tunnel the op acted on, for the ops that mark a row pending
    pub name: Option<String>,
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
    // What `e` asks first: the target and ports, a rename, or a move. One
    // chooser rather than three top-level keys, as `a` already is
    ChooseEdit,
    EditTarget,
    RenameTo,
    MoveTo,
    // The name a move lands under, asked only when the destination account
    // already holds the one the tunnel has
    MoveRename,
    Confirm,
    Help,
    Config,
    Accounts,
    AccountConfiguration,
    // The configuration step once the list has arrived, as the project step is
    AccountConfigurationPick,
    AccountName,
    AccountProject,
    // The project step once the list has arrived: the text field stays the
    // state until then, and whenever the list cannot be had
    AccountProjectPick,
    AccountZone,
    // The zone step once the project's zones have arrived
    AccountZonePick,
    // The accounts page's own confirmation. `Confirm` answers for the list,
    // where `y` deletes the selected tunnel, and one mode cannot mean both
    AccountConfirm,
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
                | InputMode::RenameTo
                | InputMode::MoveRename
        ) || self.is_account_text()
    }

    /// The text modes the accounts page opens. They are told apart from the
    /// rest because `Esc` in one of them goes back to the page, not to the
    /// list the page is covering.
    pub fn is_account_text(&self) -> bool {
        matches!(
            self,
            InputMode::AccountConfiguration
                | InputMode::AccountName
                | InputMode::AccountProject
                | InputMode::AccountZone
        )
    }

    pub fn prompt(&self) -> &'static str {
        match self {
            InputMode::AccountConfiguration => "gcloud configuration:",
            InputMode::AccountConfigurationPick => "gcloud configuration (type to filter):",
            InputMode::AccountName => "Account name:",
            InputMode::AccountProject => "Project (Enter keeps it):",
            InputMode::AccountProjectPick => "Project (type to filter):",
            InputMode::AccountZone => "Default zone (Enter keeps it):",
            InputMode::AccountZonePick => "Default zone (type to filter):",
            InputMode::AddName => "Name:",
            InputMode::AddInstance => "Instance (type to filter):",
            InputMode::AddJumpHost => "Host it reaches (Enter for the instance itself):",
            InputMode::AddRemotePort => "Remote port:",
            InputMode::AddLocalPort => "Local port (Enter keeps the suggestion):",
            InputMode::AddLocalTarget => "Local target (e.g. localhost:3000):",
            InputMode::AddDomain => "Domain:",
            InputMode::EditTarget => "Ports (remote:local):",
            InputMode::RenameTo => "New name:",
            InputMode::MoveTo => "Move it to which account:",
            InputMode::MoveRename => "New name on the destination account:",
            _ => "",
        }
    }

    /// One line under the prompt saying what the value is for and what shape
    /// it takes. A form is used rarely, so its reader cannot be expected to
    /// know what a bare label means. `EditTarget` describes an Access tunnel;
    /// the dialog swaps in the Expose wording for an Expose one.
    pub fn describe(&self) -> &'static str {
        match self {
            InputMode::AccountConfiguration => {
                "The gcloud configuration whose login, project and zone this account uses. \
                 List them: `gcloud config configurations list`"
            }
            InputMode::AccountConfigurationPick => {
                "The gcloud configuration whose login, project and zone this account uses"
            }
            InputMode::AccountName => {
                "What you pass to `--account`. Lower case, digits and dashes, e.g. `dev`"
            }
            InputMode::AccountProject | InputMode::AccountProjectPick => {
                "The GCP project every tunnel and API call of this account targets"
            }
            InputMode::AccountZone | InputMode::AccountZonePick => {
                "Used when a tunnel names an instance without a zone"
            }
            InputMode::AddName => {
                "What it connects to, never the account: `postgres`, `k8s`. \
                 Up to 40 of a-z 0-9 -"
            }
            InputMode::AddInstance => {
                "The VM IAP connects to. Its port, or a host behind it, becomes local"
            }
            InputMode::AddJumpHost => {
                "A host the VM can reach, tunnelled through it over ssh. Empty: the VM itself"
            }
            InputMode::AddRemotePort => "The port on that VM or host",
            InputMode::AddLocalPort => {
                "The port on localhost you connect to. The suggestion is derived from the name"
            }
            InputMode::AddDomain => {
                "A domain registered with `gtunnel domains add`; the URL is `<name>.<domain>`"
            }
            InputMode::AddLocalTarget => "The service on this machine to publish",
            InputMode::EditTarget => {
                "The port on the VM or host, then the localhost port, e.g. 5432:15432"
            }
            _ => "",
        }
    }

    /// Where this field sits in its form, for the dialog's title. The add
    /// form's length depends on the mode: an Access tunnel asks five things,
    /// a public URL three.
    pub fn step(&self, mode: TunnelMode) -> Option<(u8, u8)> {
        let access = mode == TunnelMode::Access;
        match self {
            InputMode::AccountConfiguration | InputMode::AccountConfigurationPick => Some((1, 2)),
            InputMode::AccountName => Some((2, 2)),
            InputMode::AccountProject | InputMode::AccountProjectPick => Some((1, 2)),
            InputMode::AccountZone | InputMode::AccountZonePick => Some((2, 2)),
            InputMode::AddName if access => Some((1, 5)),
            InputMode::AddInstance => Some((2, 5)),
            InputMode::AddJumpHost => Some((3, 5)),
            InputMode::AddRemotePort => Some((4, 5)),
            InputMode::AddLocalPort => Some((5, 5)),
            InputMode::AddName => Some((1, 3)),
            InputMode::AddLocalTarget => Some((2, 3)),
            InputMode::AddDomain => Some((3, 3)),
            _ => None,
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
    pub staged: Option<super::config_edit::Staged>,
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

// A project the picker offers, as list_projects reports it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectChoice {
    pub project_id: String,
    pub name: String,
}

// A configuration the new-account picker offers. The project is kept because
// `account add` reads it from the configuration and refuses one without
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationChoice {
    pub name: String,
    pub project: Option<String>,
    pub is_active: bool,
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

const LIST_FAILURE: &str = "Could not list instances: ";
const PROJECT_LIST_FAILURE: &str = "Could not list projects: ";
const CONFIGURATION_LIST_FAILURE: &str = "Could not list gcloud configurations: ";
const ZONE_LIST_FAILURE: &str = "Could not list zones: ";

fn is_list_failure(status: &str) -> bool {
    status.starts_with(LIST_FAILURE)
        || status.starts_with(PROJECT_LIST_FAILURE)
        || status.starts_with(CONFIGURATION_LIST_FAILURE)
        || status.starts_with(ZONE_LIST_FAILURE)
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
    // Zone for an instance the person typed, which the picker never saw and so
    // never learned a zone for; an account whose list cannot be read still needs one
    pub default_zone: String,
    // Every account's zone, so `;` can re-point `default_zone` in the same
    // switch that changes the account and a typed instance never inherits the
    // previous account's zone
    pub zones: HashMap<String, String>,
    // Local ports are a property of the laptop, so this is every account's, not
    // the selected one's; `entries` alone would suggest a port another account holds
    pub used_ports: HashSet<u16>,
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
    // Why the name just submitted was refused. It lives beside `input` and not
    // in `status`, because the dialog covers the status line and the reason
    // has to be read where the text is being corrected.
    pub draft_error: Option<String>,
    pub draft: NewTunnel,
    // The draft a create was submitted with, and the step it had reached.
    // Submitting empties `draft`, so a failed create would otherwise leave the
    // person retyping every prompt. Esc never reads it: that is a discard.
    pub last_draft: Option<(NewTunnel, InputMode)>,
    pub instances: Vec<InstanceChoice>,
    pub instance_selected: usize,
    // The projects the account being edited can see, and the cursor in them
    pub projects_list: Vec<ProjectChoice>,
    pub project_selected: usize,
    // The gcloud configurations the add dialog offers, and the cursor in them
    pub configurations_list: Vec<ConfigurationChoice>,
    pub configuration_selected: usize,
    // The zones of the project the edit dialog chose, and the cursor in them
    pub zones_list: Vec<String>,
    pub zone_selected: usize,
    pub domains: Vec<String>,
    pub domain_selected: usize,
    pub confirm_message: Option<String>,
    // Where the cursor is in the move dialog's list of accounts, which is
    // `accounts` without the one on screen: a tunnel cannot move to where it
    // already is
    pub move_selected: usize,
    // The account the move picked, held while the dialog asks for a name that
    // account does not already hold
    pub move_to: String,
    // Where the cursor is on the accounts page, an index into `accounts`
    pub account_selected: usize,
    // What the add and edit dialogs have collected so far
    pub account_draft: AccountDraft,
    // How the last account operation ended, said on the page. The page covers
    // the status line, and a refusal the user cannot read is a key that did
    // nothing as far as they can tell.
    pub account_notice: Option<Notice>,
    pub has_relay: bool,
    // Set when the instance picker opens, so the loop knows to fetch the list
    pub want_instances: bool,
    // Set when the edit dialog reaches the project, so the loop fetches the list
    pub want_projects: bool,
    // Set when the add dialog opens, so the loop fetches the configurations
    pub want_configurations: bool,
    // Set when the edit dialog reaches the zone, so the loop fetches the zones
    pub want_zones: bool,
    // Which list the open dialog is waiting on, so its field or picker says
    // so: an empty picker read as "no instances" and a plain field gave no
    // sign it was about to become a picker. Cleared when that list answers.
    pub list_loading: Option<&'static str>,
    // Which wording the edit dialog uses: an Expose tunnel edits its local
    // target, an Access one its `remote:local` ports
    pub edit_is_expose: bool,
    // Set by `r`, cleared once the event loop has rescheduled every timer
    pub force_refresh: bool,
    // When the one extra discovery round owed after a reload falls due
    pub follow_up_at: Option<std::time::Instant>,
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
    // The most clients the sampler has seen on each local port since the last
    // metrics round, which is what that round reports and then clears. Keyed
    // by port rather than by tunnel: the sampler reads ports, and a row whose
    // tunnel has just been renamed or replaced is still the same socket
    peaks: HashMap<u16, u64>,
    pub flash: Option<(String, std::time::Instant)>,
    // Read from the update cache, never fetched here: the TUI thread must not
    // wait on the network, and the header rule only shows what is already known
    pub update: Option<crate::update::UpdateNotice>,
}

impl App {
    pub fn new(initial_account: &str) -> Self {
        Self {
            account: initial_account.to_string(),
            accounts: vec![initial_account.to_string()],
            projects: HashMap::new(),
            relays: HashMap::new(),
            default_zone: String::new(),
            zones: HashMap::new(),
            used_ports: HashSet::new(),
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
            draft_error: None,
            draft: NewTunnel::default(),
            last_draft: None,
            instances: Vec::new(),
            instance_selected: 0,
            projects_list: Vec::new(),
            project_selected: 0,
            configurations_list: Vec::new(),
            configuration_selected: 0,
            zones_list: Vec::new(),
            zone_selected: 0,
            domains: Vec::new(),
            domain_selected: 0,
            confirm_message: None,
            move_selected: 0,
            move_to: String::new(),
            account_selected: 0,
            account_draft: AccountDraft::default(),
            account_notice: None,
            has_relay: false,
            want_instances: false,
            want_projects: false,
            want_configurations: false,
            want_zones: false,
            list_loading: None,
            edit_is_expose: false,
            force_refresh: false,
            follow_up_at: None,
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
            peaks: HashMap::new(),
            flash: None,
            update: None,
        }
    }

    // One sample of every port at once. A port missing from a sample has no
    // client at this instant, which says nothing about the window: only a
    // larger reading moves a peak.
    pub fn record_samples(&mut self, samples: HashMap<u16, u64>) {
        for (port, seen) in samples {
            let peak = self.peaks.entry(port).or_default();
            *peak = (*peak).max(seen);
        }
    }

    // The window ends when it is reported, so the next one starts from nothing
    pub fn take_peaks(&mut self) -> HashMap<u16, u64> {
        std::mem::take(&mut self.peaks)
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

    // The picker covers the status line, so while it is open the reason goes
    // inside the dialog; the status line carries it for after the dialog closes.
    // It is an error about the account's list, not about what was typed.
    pub fn instances_arrived(&mut self, instances: Vec<InstanceChoice>) {
        self.instances = instances;
        self.instance_selected = 0;
        if self.list_loading == Some("instances") {
            self.list_loading = None;
        }
    }

    pub fn instances_failed(&mut self, reason: &str) {
        let message = format!("{LIST_FAILURE}{reason}");
        if self.list_loading == Some("instances") {
            self.list_loading = None;
        }
        if self.input_mode == InputMode::AddInstance {
            self.draft_error = Some(format!("{message}. Type the instance name instead"));
        }
        self.set_status(message);
        self.status_error = true;
    }

    // The list is for the account being edited, so a result for any other, or
    // for a dialog since closed, is dropped: it would land in the wrong field
    fn awaiting_projects_for(&self, account: &str) -> bool {
        self.input_mode == InputMode::AccountProject && self.account_draft.name == account
    }

    // A list that arrives while the person is already typing a project is
    // not allowed to take the field from them
    pub fn projects_arrived(&mut self, account: &str, list: Vec<ProjectChoice>) {
        if !self.awaiting_projects_for(account) {
            return;
        }
        self.list_loading = None;
        if list.is_empty() {
            self.projects_failed(account, "no project is visible to this account");
            return;
        }
        let current = self.projects.get(account).cloned().unwrap_or_default();
        if self.input != current {
            return;
        }
        let mut list = list;
        let at = list.iter().position(|p| p.project_id == current);
        self.project_selected = match at {
            Some(i) => i,
            // Enter must keep the project the account has, listed or not
            None if !current.is_empty() => {
                list.insert(
                    0,
                    ProjectChoice {
                        project_id: current,
                        name: String::new(),
                    },
                );
                0
            }
            None => 0,
        };
        self.projects_list = list;
        self.input.clear();
        self.draft_error = None;
        self.input_mode = InputMode::AccountProjectPick;
    }

    // The text field stays, pre-filled, so a denied or unreachable list never
    // blocks an edit. The dialog covers the status line, so the reason is also
    // said inside it.
    pub fn projects_failed(&mut self, account: &str, reason: &str) {
        if !self.awaiting_projects_for(account) {
            return;
        }
        self.list_loading = None;
        let message = format!("{PROJECT_LIST_FAILURE}{reason}");
        self.draft_error = Some(format!("{message}. Type the project instead"));
        self.set_status(message);
        self.status_error = true;
    }

    // Like the project list: a list for a dialog since closed is dropped, and
    // one that arrives after the person started typing leaves the field to them
    pub fn configurations_arrived(&mut self, list: Vec<ConfigurationChoice>) {
        if self.input_mode != InputMode::AccountConfiguration {
            return;
        }
        self.list_loading = None;
        if list.is_empty() {
            self.configurations_failed("no gcloud configuration found. Run `gcloud init`");
            return;
        }
        if !self.input.is_empty() {
            return;
        }
        self.configuration_selected = list.iter().position(|c| c.is_active).unwrap_or(0);
        self.configurations_list = list;
        self.draft_error = None;
        self.input_mode = InputMode::AccountConfigurationPick;
    }

    // The zones are the project's chosen one step back, which need not be the
    // saved one; a list for any other project, account or closed dialog is
    // dropped, and one that arrives mid-typing leaves the field to the person
    fn awaiting_zones_for(&self, account: &str, project: &str) -> bool {
        self.input_mode == InputMode::AccountZone
            && self.account_draft.name == account
            && self.account_draft.project == project
    }

    pub fn zones_arrived(&mut self, account: &str, project: &str, list: Vec<String>) {
        if !self.awaiting_zones_for(account, project) {
            return;
        }
        self.list_loading = None;
        if list.is_empty() {
            self.zones_failed(account, project, "the project has no zone");
            return;
        }
        let current = self.zones.get(account).cloned().unwrap_or_default();
        if self.input != current {
            return;
        }
        let mut list = list;
        let same_project = self.projects.get(account) == Some(&self.account_draft.project);
        self.zone_selected = match list.iter().position(|z| *z == current) {
            Some(i) => i,
            // Enter must keep the zone the account has, listed or not; but a
            // project changed in step 1 may not have it, and the pair would
            // be saved without a check
            None if !current.is_empty() && same_project => {
                list.insert(0, current);
                0
            }
            None => 0,
        };
        self.zones_list = list;
        self.input.clear();
        self.draft_error = None;
        self.input_mode = InputMode::AccountZonePick;
    }

    // The text field stays, pre-filled, as it does for the project list
    pub fn zones_failed(&mut self, account: &str, project: &str, reason: &str) {
        if !self.awaiting_zones_for(account, project) {
            return;
        }
        self.list_loading = None;
        let message = format!("{ZONE_LIST_FAILURE}{reason}");
        self.draft_error = Some(format!("{message}. Type the zone instead"));
        self.set_status(message);
        self.status_error = true;
    }

    // The text field, pre-filled with the saved zone, until the list arrives
    fn open_zone_step(&mut self) {
        self.input = self
            .zones
            .get(&self.account_draft.name)
            .cloned()
            .unwrap_or_default();
        self.want_zones = true;
        self.list_loading = Some("zones");
        self.zones_list.clear();
        self.input_mode = InputMode::AccountZone;
    }

    // The text field stays, so a gcloud that cannot list never blocks an add
    pub fn configurations_failed(&mut self, reason: &str) {
        if self.input_mode != InputMode::AccountConfiguration {
            return;
        }
        self.list_loading = None;
        let message = format!("{CONFIGURATION_LIST_FAILURE}{reason}");
        self.draft_error = Some(format!("{message}. Type the configuration instead"));
        self.set_status(message);
        self.status_error = true;
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

    /// True, with the reason on the status line, when the selected row already
    /// has an op running. A second key on it would start a second op.
    fn refuse_pending(&mut self) -> bool {
        let Some(entry) = self.selected_entry() else {
            return false;
        };
        let Some(pending) = entry.pending else {
            return false;
        };
        let message = format!("{}: {} already in progress", entry.name(), pending.verb);
        self.set_status(message);
        self.status_error = true;
        true
    }

    fn op_for(&mut self, action: &'static str, build: fn(String) -> Op) -> Option<Op> {
        let entry = self.selected_entry()?;
        if entry.kind == TunnelKind::External {
            self.refuse_external(action);
            return None;
        }
        let name = entry.name().to_string();
        if self.refuse_pending() {
            return None;
        }
        if let Some(entry) = self.entries.get_mut(self.selected) {
            entry.pending = Some(PendingOp {
                verb: action,
                since: std::time::Instant::now(),
            });
            entry.pending_settle = false;
        }
        self.spinner.start(&format!("{action} {name}"));
        Some(build(name))
    }

    pub fn next_account(&mut self) {
        if self.accounts.len() < 2 {
            return;
        }
        let current = self.accounts.iter().position(|a| *a == self.account);
        let next = current.map(|i| (i + 1) % self.accounts.len()).unwrap_or(0);
        let name = self.accounts[next].clone();
        self.switch_to_account(&name);
    }

    /// Puts another account on screen. `;` and the accounts page both land
    /// here, so the two cannot drift: everything the old account's rows,
    /// logs and relay said is dropped, because none of it describes this one.
    fn switch_to_account(&mut self, name: &str) {
        self.account = name.to_string();
        self.default_zone = self.zones.get(&self.account).cloned().unwrap_or_default();
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

    // --- the accounts page ---

    /// Opens the page with the cursor on the account the rest of the screen
    /// is showing, which is the row every other key on the page acts on.
    fn open_accounts(&mut self) {
        self.account_selected = self
            .accounts
            .iter()
            .position(|a| *a == self.account)
            .unwrap_or(0);
        self.account_notice = None;
        self.input_mode = InputMode::Accounts;
    }

    pub fn account_at_cursor(&self) -> Option<&String> {
        self.accounts.get(self.account_selected)
    }

    fn accounts_key(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(':') => {
                self.input_mode = InputMode::Normal;
                self.account_notice = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let last = self.accounts.len().saturating_sub(1);
                self.account_selected = (self.account_selected + 1).min(last);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.account_selected = self.account_selected.saturating_sub(1)
            }
            KeyCode::Enter => return self.select_account(),
            KeyCode::Char('a') => self.open_account_add(),
            KeyCode::Char('e') => self.open_account_edit(),
            KeyCode::Char('d') => self.ask_remove_account(),
            _ => {}
        }
        None
    }

    /// The cursor's account takes the screen, and the choice is written to
    /// `config.toml` so the next `gtunnel` command agrees with what is on it.
    fn select_account(&mut self) -> Option<Op> {
        let name = self.account_at_cursor()?.clone();
        if name != self.account {
            self.switch_to_account(&name);
        }
        self.input_mode = InputMode::Normal;
        self.account_notice = None;
        self.spinner.start(&format!("select {name}"));
        Some(Op::AccountSelect(name))
    }

    // The field is typed until the list arrives, and stays typed when it
    // cannot be had: `account add` answers an unknown configuration by name
    fn open_account_add(&mut self) {
        self.account_draft = AccountDraft::default();
        self.input.clear();
        self.draft_error = None;
        self.account_notice = None;
        self.want_configurations = true;
        self.list_loading = Some("configurations");
        self.configurations_list.clear();
        self.input_mode = InputMode::AccountConfiguration;
    }

    // The fields come pre-filled, because an edit of the zone must not make
    // the person retype the project to keep it
    fn open_account_edit(&mut self) {
        let Some(name) = self.account_at_cursor().cloned() else {
            return;
        };
        self.account_draft = AccountDraft {
            name: name.clone(),
            ..AccountDraft::default()
        };
        self.input = self.projects.get(&name).cloned().unwrap_or_default();
        self.draft_error = None;
        self.account_notice = None;
        self.want_projects = true;
        self.list_loading = Some("projects");
        self.projects_list.clear();
        self.input_mode = InputMode::AccountProject;
    }

    fn ask_remove_account(&mut self) {
        let Some(name) = self.account_at_cursor().cloned() else {
            return;
        };
        self.account_draft = AccountDraft {
            name: name.clone(),
            ..AccountDraft::default()
        };
        self.account_notice = None;
        self.confirm_message = Some(format!("Remove account '{name}'? (y/n)"));
        self.input_mode = InputMode::AccountConfirm;
    }

    // Whether the account can go is `gtunnel account remove`'s to answer: it
    // refuses one that still holds tunnels, and the page shows what it said
    fn confirm_account(&mut self, code: KeyCode) -> Option<Op> {
        let name = std::mem::take(&mut self.account_draft.name);
        self.confirm_message = None;
        self.input_mode = InputMode::Accounts;
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') if !name.is_empty() => {
                self.spinner.start(&format!("remove {name}"));
                Some(Op::AccountRemove(name))
            }
            _ => None,
        }
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
            InputMode::Accounts => return self.accounts_key(key.code),
            InputMode::AccountConfirm => return self.confirm_account(key.code),
            InputMode::ChooseMode => return self.choose_mode(key.code),
            InputMode::ChooseEdit => return self.choose_edit(key.code),
            InputMode::Confirm => return self.confirm(key.code),
            InputMode::AddInstance => return self.pick_instance(key.code),
            InputMode::AccountConfigurationPick => return self.pick_configuration(key.code),
            InputMode::AccountProjectPick => return self.pick_project(key.code),
            InputMode::AccountZonePick => return self.pick_zone(key.code),
            InputMode::MoveTo => return self.pick_move_account(key.code),
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
            KeyCode::Char(':') => self.open_accounts(),
            KeyCode::Char('r') => {
                self.force_refresh = true;
                self.set_status("refreshing");
            }
            KeyCode::Char('s') => {
                if self.refuse_pending() {
                    return None;
                }
                if let Some(entry) = self.selected_entry() {
                    if entry.kind == TunnelKind::External {
                        self.refuse_external("start");
                        return None;
                    }
                    if entry.status == TunnelStatus::Running {
                        self.set_status(format!("{} is already running", entry.name()));
                        return None;
                    }
                }
                if let Some(entry) = self.entries.get_mut(self.selected) {
                    entry.health = HealthStatus::Checking;
                }
                return self.op_for("start", Op::Start);
            }
            KeyCode::Char('S') => {
                if self.refuse_pending() {
                    return None;
                }
                if let Some(entry) = self.selected_entry() {
                    if entry.kind == TunnelKind::External {
                        self.refuse_external("stop");
                        return None;
                    }
                    if entry.status != TunnelStatus::Running {
                        self.set_status(format!("{} is not running", entry.name()));
                        return None;
                    }
                }
                if let Some(entry) = self.entries.get_mut(self.selected) {
                    entry.health = HealthStatus::Unknown;
                }
                let is_run = self
                    .selected_entry()
                    .is_some_and(|e| e.kind == TunnelKind::Ephemeral);
                return self.op_for("stop", if is_run { Op::StopRun } else { Op::Stop });
            }
            KeyCode::Char('R') => {
                if self.refuse_pending() || self.refuse_ephemeral("restarting") {
                    return None;
                }
                if let Some(entry) = self.entries.get_mut(self.selected) {
                    entry.health = HealthStatus::Checking;
                }
                return self.op_for("restart", Op::Restart);
            }
            KeyCode::Char('d') => self.ask_delete(),
            KeyCode::Char('A') => {
                return self.op_for("auto-start", Op::ToggleAutoStart);
            }
            KeyCode::Char('m') => return self.import_op(),
            KeyCode::Char('c') => self.copy_url(),
            KeyCode::Char('l') => self.copy_logs(),
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
        // A copy that cannot be removed is no reason to refuse the page
        if let Ok(dir) = page_dir() {
            let _ = super::config_edit::remove_stale_copies(&dir, crate::ephemeral::pid_is_alive);
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

        let edited = self.run_editor(&staged.path);
        // Whatever happened, the editor drew over the screen
        self.clear_screen = true;
        let text = match edited {
            Ok(text) => text,
            Err(e) => {
                // A copy this press made is the edit that was cancelled. One
                // left from a refused edit holds work the user has not given up
                if fresh {
                    let _ = edit::discard(&staged.path);
                } else {
                    self.config_view.staged = Some(staged);
                }
                return self.fail(format!("{e:#}"));
            }
        };

        // Compared with what the copy was taken from, not with the file now: a
        // gtunnel that wrote in between would otherwise make an untouched copy
        // look like an edit, and saving it would overwrite that write
        if text == staged.original_text() {
            let _ = edit::discard(&staged.path);
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
                .and_then(|config| {
                    // Run records that cannot be read count as no runs: the
                    // edit is not blocked over them, and `add` and the start
                    // of the tunnel check the port again
                    let runs = crate::ephemeral::live_run_local_ports().unwrap_or_default();
                    edit::validate_tunnels(&text, &config, &runs)
                })
                .map(Checked::Tunnels),
        };
        let checked = match checked {
            Ok(checked) => checked,
            Err(e) => return self.refuse(staged, text, format!("{e:#}")),
        };

        // The state the edit started from, so the summary counts the edit and
        // not whatever another writer left meanwhile
        let before = toml::from_str::<crate::state::TunnelState>(&staged.original_text())
            .unwrap_or_default();
        if let Err(e) = edit::commit(&staged, &real) {
            if let Some(changed) = e.downcast_ref::<edit::Changed>() {
                let mut staged = staged;
                staged.rebase(changed.now.clone());
                let error = format!("{changed}; saving your edit again replaces that change");
                return self.refuse(staged, text, error);
            }
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

    // The edit stays on the page, with its copy, and says why it was not saved.
    // After a conflict the copy has been re-based on the file as it now is, so
    // a deliberate second save can land; a further write in between is refused
    // again, because the next commit compares with that re-based snapshot.
    fn refuse(&mut self, staged: super::config_edit::Staged, text: String, error: String) {
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
        // A zone edited here would otherwise stay as it was loaded at start,
        // and a typed instance would be looked for in the old one
        self.zones = config
            .accounts
            .iter()
            .map(|a| (a.name.clone(), a.default_zone.clone()))
            .collect();
        // The cursor is an index, so an account removed out from under it
        // would point past the end of a shorter list
        self.account_selected = self
            .account_selected
            .min(self.accounts.len().saturating_sub(1));
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
        self.default_zone = self.zones.get(&self.account).cloned().unwrap_or_default();
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
                    let _ = super::config_edit::discard(&staged.path);
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

    pub fn flash(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), std::time::Instant::now()));
    }

    pub fn flash_message(&self) -> Option<&str> {
        self.flash.as_ref().and_then(|(msg, time)| {
            if time.elapsed() < std::time::Duration::from_secs(3) {
                Some(msg.as_str())
            } else {
                None
            }
        })
    }

    fn copy_url(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let url = entry.tunnel.display_url();
        match (self.clipboard)(&url) {
            Ok(()) => {
                let msg = format!("copied {url}");
                self.flash(&msg);
                self.set_status(msg);
            }
            Err(e) => self.set_status(format!("could not copy: {e}")),
        }
    }

    fn copy_logs(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let content = match entry.tunnel.log_path() {
            Ok(path) if path.exists() => {
                std::fs::read_to_string(&path).unwrap_or_else(|_| self.logs.join("\n"))
            }
            _ => self.logs.join("\n"),
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            self.set_status(format!("log for {} is empty", entry.name()));
            return;
        }
        let lines = trimmed.lines().count();
        let noun = if lines == 1 { "line" } else { "lines" };
        match (self.clipboard)(trimmed) {
            Ok(()) => {
                let msg = format!("copied {} log {} for {}", lines, noun, entry.name());
                self.flash(&msg);
                self.set_status(msg);
            }
            Err(e) => self.set_status(format!("could not copy log: {e}")),
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
        self.draft_error = None;
        self.confirm_message = None;
        self.list_loading = None;
        // The account a move picked belongs to that move: the next one asks
        // again, and a stale answer here would send a tunnel somewhere
        // nobody chose
        self.move_to.clear();
    }

    // `entries` holds the active account only, so a scan is account-scoped.
    // Unmanaged rows count: they are named for their target, and two rows
    // with one name would be indistinguishable in the list.
    fn name_is_taken(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.name() == name)
    }

    // The name step is skipped when a failed create reopens the dialog, and
    // `add` saves the record before it installs the job, so a retry would
    // otherwise reach the daemon only to fail as "already exists".
    fn refuse_taken_name(&mut self) -> bool {
        if !self.name_is_taken(&self.draft.name) {
            return false;
        }
        self.draft_error = Some(format!(
            "'{}' is already used in this account",
            self.draft.name
        ));
        true
    }

    fn open_add(&mut self) {
        self.draft = NewTunnel {
            account: self.account.clone(),
            ..NewTunnel::default()
        };
        self.input.clear();
        self.draft_error = None;
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
        let name = entry.name().to_string();
        if self.refuse_pending() {
            return;
        }
        self.confirm_message = Some(format!("Delete {name}? (y/n)"));
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

    /// `e` asks which edit this is before it asks for anything else.
    ///
    /// The three answers change different things — the ports, the name, the
    /// account — and share one key rather than taking three of the few that
    /// are left, as `a` already shares one between an Access tunnel and a
    /// public URL. An unmanaged row is refused once, here, because none of
    /// the three has a record to write.
    fn open_edit(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.kind == TunnelKind::External {
            self.refuse_external("edit");
            return;
        }
        self.input.clear();
        self.draft_error = None;
        self.input_mode = InputMode::ChooseEdit;
    }

    fn choose_edit(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Char('1') => self.open_edit_target(),
            KeyCode::Char('2') => self.open_rename(),
            KeyCode::Char('3') => self.open_move(),
            KeyCode::Esc => self.cancel_dialog(),
            _ => {}
        }
        None
    }

    fn open_edit_target(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let is_expose = matches!(entry.tunnel.spec, TunnelSpec::Expose { .. });
        self.input = match &entry.tunnel.spec {
            TunnelSpec::Access {
                remote_port,
                local_port,
                ..
            } => format!("{remote_port}:{local_port}"),
            TunnelSpec::Expose { local_target, .. } => local_target.clone(),
        };
        self.edit_is_expose = is_expose;
        self.input_mode = InputMode::EditTarget;
    }

    /// A run is recorded in the ephemeral list and not in `tunnels.toml`, so
    /// a rename has no record to rewrite and a move has none to hand over.
    /// `m` is what gives it one, and the refusal says so.
    fn refuse_ephemeral(&mut self, action: &str) -> bool {
        let Some(entry) = self.selected_entry() else {
            return true;
        };
        if entry.kind != TunnelKind::Ephemeral {
            return false;
        }
        let name = entry.name().to_string();
        self.cancel_dialog();
        self.set_status(format!(
            "{name} is a run: import it with m before {action} it"
        ));
        self.status_error = true;
        true
    }

    // The old name is in the field, because a rename is usually an edit of
    // the name and not a word typed from nothing
    fn open_rename(&mut self) {
        if self.refuse_ephemeral("renaming") {
            return;
        }
        let Some(entry) = self.selected_entry() else {
            return;
        };
        self.input = entry.name().to_string();
        self.draft_error = None;
        self.input_mode = InputMode::RenameTo;
    }

    fn open_move(&mut self) {
        if self.refuse_ephemeral("moving") {
            return;
        }
        if self.other_accounts().is_empty() {
            self.cancel_dialog();
            self.set_status(
                "This is the only account. Add another with `gtunnel account add` to move a tunnel",
            );
            self.status_error = true;
            return;
        }
        self.move_selected = 0;
        self.input.clear();
        self.draft_error = None;
        self.input_mode = InputMode::MoveTo;
    }

    /// Where a move may land: every account but the one on screen, because
    /// `move` refuses a tunnel that is already on the account it is sent to.
    pub fn other_accounts(&self) -> Vec<String> {
        self.accounts
            .iter()
            .filter(|a| **a != self.account)
            .cloned()
            .collect()
    }

    fn pick_move_account(&mut self, code: KeyCode) -> Option<Op> {
        let accounts = self.other_accounts();
        match code {
            KeyCode::Esc => self.cancel_dialog(),
            KeyCode::Down => {
                let last = accounts.len().saturating_sub(1);
                self.move_selected = (self.move_selected + 1).min(last);
            }
            KeyCode::Up => self.move_selected = self.move_selected.saturating_sub(1),
            KeyCode::Enter => {
                let to = accounts.get(self.move_selected)?.clone();
                let name = self.selected_entry()?.name().to_string();
                // Decision 2: the destination holding the name is a question,
                // not a refusal, so the dialog asks it with the reason on
                // screen instead of throwing the move away
                if names_in_account(&to).contains(&name) {
                    self.move_to = to.clone();
                    self.input = name.clone();
                    self.draft_error = Some(format!("'{name}' is already used in {to}"));
                    self.input_mode = InputMode::MoveRename;
                    return None;
                }
                let from = self.account.clone();
                self.cancel_dialog();
                self.spinner.start(&format!("move {name}"));
                return Some(Op::Move(MoveTunnel {
                    name,
                    from,
                    to,
                    rename_to: None,
                }));
            }
            _ => {}
        }
        None
    }

    // The port `gtunnel add` would allocate for this name, so the dialog and the
    // CLI agree. The probe is `|_| true` on purpose: a real one binds a socket per
    // candidate and this runs on the thread that draws. The suggestion is advisory
    // and the CLI re-probes the host when the tunnel is actually added.
    pub fn suggested_local_port(&self, name: &str) -> u16 {
        allocate_port(
            name,
            ACCESS_PORT_BASE,
            ACCESS_PORT_SPAN,
            &self.used_ports,
            |_| true,
        )
        .unwrap_or(ACCESS_PORT_BASE)
    }

    fn type_line(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Esc => {
                // A dialog the page opened goes back to the page: the list
                // underneath is not where this person was
                let from_page = self.input_mode.is_account_text();
                self.cancel_dialog();
                if from_page {
                    self.input_mode = InputMode::Accounts;
                }
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.draft_error = None;
                None
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.draft_error = None;
                None
            }
            KeyCode::Enter => self.advance(),
            _ => None,
        }
    }

    // A dialog the page opened is done with: the page is what it returns to
    fn back_to_page(&mut self) {
        self.input.clear();
        self.draft_error = None;
        self.confirm_message = None;
        self.input_mode = InputMode::Accounts;
    }

    /// A field's new value, or `None` when it is to be left alone. Blank
    /// means unchanged rather than empty, because the field arrives
    /// pre-filled and `cmd_account_edit` refuses an empty value outright.
    fn changed(now: &str, before: Option<&String>) -> Option<String> {
        let now = now.trim();
        (!now.is_empty() && Some(now) != before.map(String::as_str)).then(|| now.to_string())
    }

    fn advance(&mut self) -> Option<Op> {
        let value = std::mem::take(&mut self.input);
        match self.input_mode {
            InputMode::AccountConfiguration => {
                if value.trim().is_empty() {
                    self.draft_error =
                        Some("Name the gcloud configuration this account uses".to_string());
                    return None;
                }
                self.account_draft.configuration = value.trim().to_string();
                self.draft_error = None;
                self.input_mode = InputMode::AccountName;
                None
            }
            InputMode::AccountName => {
                let name = value.trim().to_string();
                if let Err(e) = crate::config::validate_account_name(&name) {
                    self.draft_error = Some(e.to_string());
                    self.input = value;
                    return None;
                }
                if self.accounts.contains(&name) {
                    self.draft_error = Some(format!("'{name}' is already an account"));
                    self.input = value;
                    return None;
                }
                let new = NewAccount {
                    configuration: self.account_draft.configuration.clone(),
                    name: name.clone(),
                };
                self.back_to_page();
                self.spinner.start(&format!("add {name}"));
                Some(Op::AccountAdd(new))
            }
            InputMode::AccountProject => {
                self.account_draft.project = value.trim().to_string();
                self.clear_list_failure();
                self.open_zone_step();
                None
            }
            InputMode::AccountZone => {
                let name = self.account_draft.name.clone();
                let project = Self::changed(&self.account_draft.project, self.projects.get(&name));
                let zone = Self::changed(&value, self.zones.get(&name));
                self.back_to_page();
                if project.is_none() && zone.is_none() {
                    self.account_notice = Some(Notice {
                        text: format!("{name} is unchanged"),
                        ok: true,
                    });
                    return None;
                }
                self.spinner.start(&format!("edit {name}"));
                Some(Op::AccountEdit(AccountEdit {
                    name,
                    project,
                    zone,
                }))
            }
            InputMode::AddName => {
                if value.trim().is_empty() {
                    self.input = value;
                    return None;
                }
                let name = value.trim().to_string();
                if let Err(e) = crate::state::validate_tunnel_name(&name) {
                    self.draft_error = Some(e.to_string());
                    self.input = value;
                    return None;
                }
                if self.name_is_taken(&name) {
                    self.draft_error = Some(format!("'{name}' is already used in this account"));
                    self.input = value;
                    return None;
                }
                self.draft_error = None;
                self.draft.name = name;
                self.input_mode = match self.draft.mode {
                    TunnelMode::Access => {
                        self.want_instances = true;
                        self.list_loading = Some("instances");
                        self.instance_selected = 0;
                        InputMode::AddInstance
                    }
                    TunnelMode::Public => InputMode::AddLocalTarget,
                };
                None
            }
            InputMode::AddJumpHost => {
                let host = value.trim().to_string();
                // Empty means the instance itself is the destination
                if let Err(e) = Some(&host)
                    .filter(|h| !h.is_empty())
                    .map_or(Ok(()), |h| crate::state::validate_host(h))
                {
                    self.draft_error = Some(e.to_string());
                    self.input = value;
                    return None;
                }
                self.draft_error = None;
                self.draft.host = host;
                self.input_mode = InputMode::AddRemotePort;
                None
            }
            InputMode::AddRemotePort => match value.trim().parse::<u16>() {
                Ok(port) => {
                    self.draft.remote_port = port;
                    self.draft.local_port = self.suggested_local_port(&self.draft.name);
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
                if self.refuse_taken_name() {
                    self.input = value;
                    return None;
                }
                let draft = std::mem::take(&mut self.draft);
                self.cancel_dialog();
                self.spinner.start(&format!("create {}", draft.name));
                self.last_draft = Some((draft.clone(), InputMode::AddLocalPort));
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
            InputMode::RenameTo => {
                let account = self.account.clone();
                let taken: Vec<String> =
                    self.entries.iter().map(|e| e.name().to_string()).collect();
                let new_name = self.checked_name(&value, &account, &taken)?;
                let name = self.selected_entry()?.name().to_string();
                self.cancel_dialog();
                self.spinner.start(&format!("rename {name}"));
                Some(Op::Rename(RenameTunnel {
                    account,
                    name,
                    new_name,
                }))
            }
            InputMode::MoveRename => {
                // Cloned and not taken: a refused name reopens the same
                // question, and the destination is half of it
                let to = self.move_to.clone();
                let new_name = self.checked_name(&value, &to, &names_in_account(&to))?;
                let name = self.selected_entry()?.name().to_string();
                let from = self.account.clone();
                self.cancel_dialog();
                self.spinner.start(&format!("move {name}"));
                Some(Op::Move(MoveTunnel {
                    name,
                    from,
                    to,
                    rename_to: Some(new_name),
                }))
            }
            _ => None,
        }
    }

    /// A name the dialog accepts, or `None` with the reason beside the field
    /// and the text still there to be corrected.
    ///
    /// `taken` is what the account the tunnel is landing on already calls its
    /// tunnels, which is the account on screen for a rename and another one
    /// for a move; `account` names it, because "already used" is only an
    /// answer when the reader knows where.
    fn checked_name(&mut self, value: &str, account: &str, taken: &[String]) -> Option<String> {
        let name = value.trim().to_string();
        let refuse = |app: &mut Self, reason: Option<String>| {
            app.draft_error = reason;
            app.input = value.to_string();
            None
        };
        if name.is_empty() {
            return refuse(self, None);
        }
        if let Err(e) = crate::state::validate_tunnel_name(&name) {
            return refuse(self, Some(e.to_string()));
        }
        if taken.iter().any(|t| t == &name) {
            return refuse(self, Some(format!("'{name}' is already used in {account}")));
        }
        self.draft_error = None;
        Some(name)
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
                let (instance, zone) = match chosen {
                    Some(c) => (c.name, c.zone),
                    // A filter that matches nothing is the person naming an
                    // instance the list lacks, or could not fetch: take it
                    None if !self.input.trim().is_empty() => {
                        (self.input.trim().to_string(), self.default_zone.clone())
                    }
                    None => {
                        self.draft_error = Some("Type an instance name".to_string());
                        return None;
                    }
                };
                self.draft.instance = instance;
                self.draft.zone = zone;
                self.draft_error = None;
                self.clear_list_failure();
                self.input.clear();
                self.input_mode = InputMode::AddJumpHost;
            }
            _ => {}
        }
        None
    }

    // Typing a name worked around a failed list, so the list's error no
    // longer describes anything the person must act on. Other errors on the
    // line are not the list's to clear.
    fn clear_list_failure(&mut self) {
        if self.status_error && self.status.as_deref().is_some_and(is_list_failure) {
            self.status = None;
            self.status_error = false;
        }
    }

    // Filters as you type, like the instance picker; the id and the display
    // name both match, since people remember either
    pub fn filtered_projects(&self) -> Vec<&ProjectChoice> {
        let needle = self.input.to_lowercase();
        self.projects_list
            .iter()
            .filter(|p| {
                needle.is_empty()
                    || p.project_id.to_lowercase().contains(&needle)
                    || p.name.to_lowercase().contains(&needle)
            })
            .collect()
    }

    fn pick_project(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Esc => {
                self.cancel_dialog();
                self.input_mode = InputMode::Accounts;
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.project_selected = 0;
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.project_selected = 0;
            }
            KeyCode::Down => {
                let last = self.filtered_projects().len().saturating_sub(1);
                self.project_selected = (self.project_selected + 1).min(last);
            }
            KeyCode::Up => self.project_selected = self.project_selected.saturating_sub(1),
            KeyCode::Enter => {
                let chosen = self
                    .filtered_projects()
                    .get(self.project_selected)
                    .map(|p| p.project_id.clone());
                let project = match chosen {
                    Some(id) => id,
                    // A filter that matches nothing is the person naming a
                    // project the list lacks: take it
                    None if !self.input.trim().is_empty() => self.input.trim().to_string(),
                    None => return None,
                };
                self.account_draft.project = project;
                self.clear_list_failure();
                self.draft_error = None;
                self.open_zone_step();
            }
            _ => {}
        }
        None
    }

    pub fn filtered_zones(&self) -> Vec<&str> {
        let needle = self.input.to_lowercase();
        self.zones_list
            .iter()
            .filter(|z| needle.is_empty() || z.to_lowercase().contains(&needle))
            .map(String::as_str)
            .collect()
    }

    fn pick_zone(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Esc => {
                self.cancel_dialog();
                self.input_mode = InputMode::Accounts;
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.zone_selected = 0;
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.zone_selected = 0;
            }
            KeyCode::Down => {
                let last = self.filtered_zones().len().saturating_sub(1);
                self.zone_selected = (self.zone_selected + 1).min(last);
            }
            KeyCode::Up => self.zone_selected = self.zone_selected.saturating_sub(1),
            KeyCode::Enter => {
                let chosen = self
                    .filtered_zones()
                    .get(self.zone_selected)
                    .map(|z| z.to_string());
                let zone = match chosen {
                    Some(z) => z,
                    // Unlike a project list, the zone list is the project's
                    // whole set, so a zone it lacks is a typo that `account
                    // edit` would save unchecked; `init` refuses it too
                    None if !self.input.trim().is_empty() => {
                        self.draft_error = Some(format!(
                            "'{}' is not a zone of {}",
                            self.input.trim(),
                            self.account_draft.project
                        ));
                        return None;
                    }
                    None => return None,
                };
                self.clear_list_failure();
                // The text step is what submits, so the picker hands it the zone
                self.input = zone;
                self.input_mode = InputMode::AccountZone;
                return self.advance();
            }
            _ => {}
        }
        None
    }

    // Filters as you type, on the configuration name and on its project
    pub fn filtered_configurations(&self) -> Vec<&ConfigurationChoice> {
        let needle = self.input.to_lowercase();
        self.configurations_list
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.name.to_lowercase().contains(&needle)
                    || c.project
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains(&needle))
            })
            .collect()
    }

    fn pick_configuration(&mut self, code: KeyCode) -> Option<Op> {
        match code {
            KeyCode::Esc => {
                self.cancel_dialog();
                self.input_mode = InputMode::Accounts;
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.configuration_selected = 0;
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.configuration_selected = 0;
            }
            KeyCode::Down => {
                let last = self.filtered_configurations().len().saturating_sub(1);
                self.configuration_selected = (self.configuration_selected + 1).min(last);
            }
            KeyCode::Up => {
                self.configuration_selected = self.configuration_selected.saturating_sub(1)
            }
            KeyCode::Enter => {
                let chosen = self
                    .filtered_configurations()
                    .get(self.configuration_selected)
                    .map(|c| (*c).clone());
                let configuration = match chosen {
                    // `account add` would refuse it after a gcloud call
                    Some(c) if c.project.is_none() => {
                        let name = c.name;
                        self.draft_error = Some(format!(
                            "'{name}' has no project. Set one first: \
                             gcloud config set project <id> --configuration={name}"
                        ));
                        return None;
                    }
                    Some(c) => c.name,
                    // A filter that matches nothing is the person naming a
                    // configuration the list lacks: take it
                    None if !self.input.trim().is_empty() => self.input.trim().to_string(),
                    None => return None,
                };
                self.account_draft.configuration = configuration;
                self.clear_list_failure();
                self.input.clear();
                self.draft_error = None;
                self.input_mode = InputMode::AccountName;
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
                if self.refuse_taken_name() {
                    return None;
                }
                self.draft.domain = domain;
                let draft = std::mem::take(&mut self.draft);
                self.cancel_dialog();
                self.spinner.start(&format!("create {}", draft.name));
                self.last_draft = Some((draft.clone(), InputMode::AddDomain));
                return Some(Op::Create(draft));
            }
            _ => {}
        }
        None
    }

    // The port prompt is pre-filled with what was typed, since its field is
    // the only one that holds text; the pickers keep their selection indexes.
    fn reopen(&mut self, draft: NewTunnel, step: InputMode, message: &str) {
        self.input = match step {
            InputMode::AddLocalPort => draft.local_port.to_string(),
            _ => String::new(),
        };
        self.draft = draft;
        self.input_mode = step;
        self.draft_error = Some(message.to_string());
    }

    pub fn apply(&mut self, result: OpResult) {
        self.spinner.stop();
        self.set_status(result.message.clone());
        self.status_error = !result.ok;
        // The page covers the status line, and a refusal there is one nobody
        // reads: it is the page that asked, so the page is told
        if result.from_account {
            self.account_notice = Some(Notice {
                text: result.message.clone(),
                ok: result.ok,
            });
        }
        // Ops run concurrently, so only a create's own result settles its
        // draft. A dialog already open is someone's work in progress and is
        // not overwritten; the failure still reaches the status line.
        if result.from_create
            && let Some((draft, step)) = self.last_draft.take()
            && !result.ok
            && self.input_mode == InputMode::Normal
        {
            self.reopen(draft, step, &result.message);
        }
        if let Some(name) = &result.name
            && let Some(entry) = self.entries.iter_mut().find(|e| e.name() == name)
        {
            entry.pending_settle = true;
        }
        if result.reload {
            self.force_refresh = true;
            self.follow_up_at = Some(std::time::Instant::now() + START_FOLLOWUP);
        }
    }

    /// True once when the look scheduled by the last reload is due.
    pub fn take_follow_up(&mut self) -> bool {
        match self.follow_up_at {
            Some(at) if std::time::Instant::now() >= at => {
                self.follow_up_at = None;
                true
            }
            _ => false,
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
    pub fn replace_entries(&mut self, entries: Vec<TunnelEntry>, used_ports: HashSet<u16>) {
        self.used_ports = used_ports;
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
                        entry.restart_watch = old.restart_watch.clone();
                        entry.crash_looping = old.crash_looping;
                        // A row that is not running has nothing to be healthy
                        // about: carrying the last run's verdict across a stop
                        // made the next start's first poll read as a recovery
                        // or a repeat, by how the previous run happened to end
                        entry.health = if entry.status == TunnelStatus::Running {
                            old.health
                        } else {
                            HealthStatus::Unknown
                        };
                        entry.metrics = old.metrics.clone();
                        // The first round after the op shows what it did, so
                        // it is the one that ends the pending state
                        if !old.pending_settle {
                            entry.pending = old.pending;
                        }
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
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    // Called from the key handler, so a clipboard tool that never exits is a
    // frozen screen; past the bound it counts as a tool that failed
    crate::proc::output_within_blocking_with_input(
        cmd,
        Some(input.as_bytes()),
        crate::proc::LOCAL_TOOL_BOUND,
        program,
    )
    .map(|out| out.status.success())
    .map_err(std::io::Error::other)
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

/// The tunnels on disk, behind the guard the config page reads through.
fn stored_state() -> anyhow::Result<crate::state::TunnelState> {
    // The path is thrown away: calling it is the guard, which under test
    // refuses a suite that never set a directory of its own
    page_dir()?;
    crate::state::TunnelState::load()
}

/// What an account already calls its tunnels.
///
/// Read from `tunnels.toml` rather than from `entries`, which holds the
/// account on screen alone and so knows nothing about the one a move is
/// sending to. `cmd_move` checks this same rule and is the one that refuses;
/// the dialog asks only so that it can offer another name while it is still
/// open, which is why a file it cannot read costs the offer and not the check.
fn names_in_account(account: &str) -> Vec<String> {
    let Ok(state) = stored_state() else {
        return Vec::new();
    };
    state
        .tunnels_for_account(account)
        .iter()
        .map(|t| t.name.clone())
        .collect()
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
    // Not waited on: xdg-open can stay alive as long as the browser it
    // launched, so a bound would turn a working open into an error. The
    // thread only reaps the child so it leaves no zombie.
    let mut child = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
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
mod peak_tests {
    use super::*;

    // A client that opens and closes between two renders is still traffic.
    // The sampler reads far more often than the pane draws, and what the pane
    // reports is the most any of those readings saw, per port
    #[test]
    fn a_peak_keeps_the_most_clients_any_sample_saw() {
        let mut app = App::new("dev");
        app.record_samples(HashMap::from([(8443u16, 1u64), (15432, 4)]));
        app.record_samples(HashMap::from([(8443, 3)]));
        app.record_samples(HashMap::from([(8443, 0), (15432, 1)]));
        assert_eq!(
            app.take_peaks(),
            HashMap::from([(8443u16, 3u64), (15432, 4)]),
            "an idle reading erased the traffic the one before it saw"
        );
    }

    // The window is the gap between two metrics ticks, so taking it ends it:
    // held, the rush of five minutes ago would still read as current
    #[test]
    fn taking_the_peaks_starts_the_next_window_empty() {
        let mut app = App::new("dev");
        app.record_samples(HashMap::from([(8443u16, 5u64)]));
        assert_eq!(app.take_peaks(), HashMap::from([(8443u16, 5u64)]));
        assert!(app.take_peaks().is_empty());
        app.record_samples(HashMap::from([(8443, 1)]));
        assert_eq!(
            app.take_peaks(),
            HashMap::from([(8443u16, 1u64)]),
            "the window kept a count the traffic no longer supports"
        );
    }
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

#[cfg(test)]
mod follow_up_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_reload_op_schedules_one_follow_up_discovery() {
        let mut app = App::new("dev");
        app.apply(OpResult {
            message: "started".to_string(),
            ok: true,
            reload: true,
            from_create: false,
            from_account: false,
            name: None,
        });
        assert!(!app.take_follow_up(), "not before START_FOLLOWUP");
        app.follow_up_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(app.take_follow_up());
        assert!(!app.take_follow_up(), "taken once");
    }

    #[test]
    fn an_op_that_changed_nothing_schedules_no_follow_up() {
        let mut app = App::new("dev");
        app.apply(OpResult {
            message: "copied".to_string(),
            ok: true,
            reload: false,
            from_create: false,
            from_account: false,
            name: None,
        });
        assert!(app.follow_up_at.is_none());
    }
}
