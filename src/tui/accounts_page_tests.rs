use super::app::*;
use super::*;
use crate::config::testing::TempConfigDir;
use crate::config::{save_config, Account, Config};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn account(name: &str, project: &str, zone: &str) -> Account {
    Account {
        name: name.to_string(),
        gcloud_configuration: "default".to_string(),
        project: project.to_string(),
        default_zone: zone.to_string(),
        impersonate_service_account: None,
        credential_file: None,
        expose: None,
    }
}

fn two_accounts() -> Config {
    let mut config = Config::default();
    config
        .add_account(account("dev", "example-dev-1a2b", "europe-west1-b"))
        .expect("fixture adds");
    config
        .add_account(account("prod", "example-prod-3c4d", "europe-west4-a"))
        .expect("fixture adds");
    config
}

fn tunnel(name: &str, local_port: u16) -> PersistentTunnel {
    PersistentTunnel {
        name: name.to_string(),
        account_name: "dev".to_string(),
        auto_start: false,
        spec: TunnelSpec::Access {
            target: AccessTarget::Instance {
                instance: "bastion".to_string(),
                zone: "europe-west1-b".to_string(),
            },
            remote_port: 5432,
            local_port,
        },
    }
}

fn page_app() -> App {
    let mut app = App::new("dev");
    app.reload_config(&two_accounts());
    app.handle_key(key(KeyCode::Char(':')));
    app
}

fn rows(app: &App, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal.draw(|f| ui::render(f, app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

// Clears whatever the field was pre-filled with, then types
fn retype(app: &mut App, text: &str) {
    for _ in 0..app.input.chars().count() {
        app.handle_key(key(KeyCode::Backspace));
    }
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

// The header names one account; this page is where the others are, so each
// row has to carry what tells them apart - the project and the zone - and
// the one the rest of the screen is showing has to be findable at a glance.
#[test]
fn the_accounts_page_lists_every_account_and_marks_the_selected_one() {
    let app = page_app();
    assert_eq!(app.input_mode, InputMode::Accounts);
    let rows = rows(&app, 80, 24);
    // Both values, because the header already carries the project of the
    // account on screen and a row found by that alone could be the header
    let row_of = |project: &str, zone: &str| {
        rows.iter()
            .find(|r| r.contains(project) && r.contains(zone))
            .unwrap_or_else(|| panic!("no row for {project} in {zone}:\n{}", rows.join("\n")))
            .clone()
    };
    let dev = row_of("example-dev-1a2b", "europe-west1-b");
    let prod = row_of("example-prod-3c4d", "europe-west4-a");
    assert!(dev.contains(" dev "), "{dev:?}");
    assert!(prod.contains(" prod "), "{prod:?}");
    assert!(
        dev.contains('*'),
        "the account on screen is marked: {dev:?}"
    );
    assert!(!prod.contains('*'), "only that one is marked: {prod:?}");
    let page = rows.join("\n");
    for hint in ["a add", "e edit", "d remove", "Esc back"] {
        assert!(page.contains(hint), "{hint} is missing:\n{page}");
    }
}

// The dialog asks for a gcloud configuration and not a project, because
// `account add` reads the project and the zone from the configuration.
// The name is checked here as well as there: a duplicate that reached the
// command would come back as a failure after a gcloud call.
#[test]
fn the_add_dialog_asks_for_a_configuration_and_a_name_it_can_use() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('a')));
    assert_eq!(app.input_mode, InputMode::AccountConfiguration);
    retype(&mut app, "work");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(app.input_mode, InputMode::AccountName);

    retype(&mut app, "dev");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None, "dev is taken");
    assert_eq!(app.input_mode, InputMode::AccountName, "the dialog stays");
    let said = app.draft_error.clone().expect("a reason beside the field");
    assert!(said.contains("already an account"), "{said}");

    retype(&mut app, "Sandbox");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        None,
        "not a valid name"
    );
    let said = app.draft_error.clone().expect("a reason beside the field");
    assert!(said.contains("lowercase"), "{said}");

    retype(&mut app, "sandbox");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Op::AccountAdd(NewAccount {
            configuration: "work".to_string(),
            name: "sandbox".to_string(),
        }))
    );
    assert_eq!(app.input_mode, InputMode::Accounts, "back on the page");
}

// `cmd_account_edit` refuses a call with nothing to change and refuses an
// empty value, so the dialog pre-fills both fields and sends only what
// the person actually altered.
#[test]
fn the_edit_dialog_sends_only_the_field_that_changed() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('e')));
    assert_eq!(app.input_mode, InputMode::AccountProject);
    assert_eq!(app.input, "example-dev-1a2b", "the project is pre-filled");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(app.input, "europe-west1-b", "the zone is pre-filled");

    retype(&mut app, "europe-west4-a");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Op::AccountEdit(AccountEdit {
            name: "dev".to_string(),
            project: None,
            zone: Some("europe-west4-a".to_string()),
        }))
    );
    assert_eq!(app.input_mode, InputMode::Accounts);

    // Enter through both fields alters nothing, and a command that would
    // be refused for having nothing to change is not sent
    app.handle_key(key(KeyCode::Char('e')));
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(app.input_mode, InputMode::Accounts);
    let notice = app.account_notice.clone().expect("the page says so");
    assert_eq!(notice.text, "dev is unchanged");
    assert!(notice.ok);
}

// Selecting is two things at once: the screen moves to that account, and
// the choice is written down, so the next `gtunnel` command agrees with
// what the person is looking at.
#[test]
fn enter_puts_an_account_on_screen_and_writes_the_choice_down() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Down));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Op::AccountSelect("prod".to_string()))
    );
    assert_eq!(app.account, "prod", "the screen moved");
    assert_eq!(app.default_zone, "europe-west4-a", "and so did the zone");
    assert!(app.entries.is_empty(), "dev's rows are not prod's");
    assert_eq!(app.input_mode, InputMode::Normal, "the page is done");
}

// The two cells between pairs are what make the row read as keys rather
// than as a sentence. Word-wrapping collapsed them to one, which turned
// the hint into "Enter select a add e edit".
#[test]
fn the_page_keys_keep_two_cells_between_each_pair() {
    let app = page_app();
    let wide = rows(&app, 80, 24);
    let hint = wide
        .iter()
        .find(|r| r.contains("Esc back"))
        .unwrap_or_else(|| panic!("no hint row:\n{}", wide.join("\n")));
    assert!(hint.contains("j/k move  Enter select  a add"), "{hint:?}");

    // Too narrow for one row: the pairs take two rows whole, rather than
    // one row with the end of it cut off
    let narrow = rows(&app, 50, 24);
    let hinted: Vec<&String> = narrow
        .iter()
        .filter(|r| r.contains("j/k move") || r.contains("Esc back"))
        .collect();
    assert_eq!(hinted.len(), 2, "{}", narrow.join("\n"));
    for pair in ["j/k move", "Enter select", "a add", "e edit", "d remove"] {
        assert!(
            narrow.iter().any(|r| r.contains(pair)),
            "{pair} was cut:\n{}",
            narrow.join("\n")
        );
    }
}

// `gtunnel account remove` refuses an account that still holds tunnels,
// before the confirmation and whatever `--yes` says, because no flag
// answers what becomes of them. The page asks the same command and so
// meets the same refusal: its job is to show it, not to pre-empt it.
#[tokio::test]
async fn deleting_an_account_with_tunnels_says_why_it_cannot() {
    let _dir = TempConfigDir::new();
    let config = two_accounts();
    save_config(&config).expect("fixture saves");
    let mut state = TunnelState::default();
    state.add(tunnel("db", 15432)).expect("fixture adds");
    state.add(tunnel("cache", 16379)).expect("fixture adds");
    state.save().expect("fixture saves");

    let mut app = App::new("dev");
    app.reload_config(&config);
    app.handle_key(key(KeyCode::Char(':')));
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.input_mode, InputMode::AccountConfirm);
    let op = app.handle_key(key(KeyCode::Char('y'))).expect("y removes");
    assert_eq!(op, Op::AccountRemove("dev".to_string()));
    assert_eq!(app.input_mode, InputMode::Accounts, "back on the page");

    let result = ops::execute("dev", op).await;
    assert!(!result.ok, "{}", result.message);
    assert!(result.from_account, "the page is what has to hear of it");
    for part in ["2 tunnel(s)", "gtunnel delete", "gtunnel move"] {
        assert!(result.message.contains(part), "{}", result.message);
    }
    let refusal = result.message.clone();
    app.apply(result);
    assert_eq!(
        app.account_notice.as_ref().map(|n| (n.text.clone(), n.ok)),
        Some((refusal, false)),
        "the page carries the refusal whole"
    );
    let page = rows(&app, 80, 24).join("\n");
    assert!(page.contains("tunnel(s)"), "the page says why:\n{page}");
    assert!(page.contains("gtunnel"), "and what to do about it:\n{page}");
    let config = crate::config::load_config().expect("loads");
    assert!(config.get_account(Some("dev")).is_ok(), "the account stays");
}

#[test]
fn the_account_form_title_counts_its_steps_and_says_what_a_configuration_is() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('a')));
    let page = rows(&app, 100, 30).join("\n");
    assert!(page.contains("New account 1/2"), "{page}");
    assert!(page.contains("gcloud config configurations list"), "{page}");
}

fn configuration(name: &str, project: Option<&str>, is_active: bool) -> ConfigurationChoice {
    ConfigurationChoice {
        name: name.to_string(),
        project: project.map(str::to_string),
        is_active,
    }
}

fn add_dialog_with(list: Vec<ConfigurationChoice>) -> App {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('a')));
    assert!(
        app.want_configurations,
        "opening the dialog asks for the list"
    );
    app.configurations_arrived(list);
    app
}

// The active configuration is the one `gcloud` itself would use, so it is
// the likeliest answer and Enter alone takes it
#[test]
fn the_configuration_picker_preselects_the_active_one() {
    let app = add_dialog_with(vec![
        configuration("ci", Some("example-dev-1a2b"), false),
        configuration("work", Some("example-prod-3c4d"), true),
    ]);
    assert_eq!(app.input_mode, InputMode::AccountConfigurationPick);
    let chosen = &app.filtered_configurations()[app.configuration_selected];
    assert_eq!(chosen.name, "work");
    let page = rows(&app, 100, 30).join("\n");
    assert!(page.contains("New account 1/2"), "{page}");
    assert!(page.contains("(active)"), "{page}");
}

// `account add` reads the project from the configuration, so one without a
// project would fail after a gcloud call; the dialog says the fix instead
#[test]
fn a_configuration_without_a_project_is_refused_with_the_fix() {
    let mut app = add_dialog_with(vec![configuration("bare", None, true)]);
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(app.input_mode, InputMode::AccountConfigurationPick);
    let said = app.draft_error.clone().expect("a reason beside the list");
    assert!(
        said.contains("gcloud config set project <id> --configuration=bare"),
        "{said}"
    );
}

#[test]
fn a_typed_configuration_the_list_lacks_is_taken() {
    let mut app = add_dialog_with(vec![configuration("work", Some("example-dev-1a2b"), true)]);
    retype(&mut app, "ci");
    assert!(app.filtered_configurations().is_empty());
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(app.account_draft.configuration, "ci");
    assert_eq!(app.input_mode, InputMode::AccountName);
}

#[test]
fn a_late_list_does_not_take_a_field_being_typed() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('a')));
    retype(&mut app, "de");
    app.configurations_arrived(vec![configuration("work", Some("example-dev-1a2b"), true)]);
    assert_eq!(app.input_mode, InputMode::AccountConfiguration);
    assert_eq!(app.input, "de");
}

#[test]
fn a_list_for_a_closed_dialog_is_dropped() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('a')));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.input_mode, InputMode::Accounts);
    app.configurations_arrived(vec![configuration("work", Some("example-dev-1a2b"), true)]);
    assert_eq!(app.input_mode, InputMode::Accounts);
}

#[test]
fn no_configurations_leaves_the_text_field_with_the_reason() {
    let app = add_dialog_with(Vec::new());
    assert_eq!(app.input_mode, InputMode::AccountConfiguration);
    let said = app.draft_error.clone().expect("a reason beside the field");
    assert!(said.contains("gcloud init"), "{said}");
}

fn zones() -> Vec<String> {
    ["europe-west1-c", "europe-west4-a", "us-central1-a"]
        .map(str::to_string)
        .to_vec()
}

// Opens the edit dialog on dev and keeps its project, which is where the
// zone step asks for the project's zones
fn zone_step() -> App {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('e')));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.input_mode, InputMode::AccountZone);
    assert!(app.want_zones, "reaching the zone asks for the list");
    app
}

// dev's zone is europe-west1-b, which the list lacks: Enter must still keep it
#[test]
fn the_zone_picker_preselects_the_current_zone_even_when_unlisted() {
    let mut app = zone_step();
    app.zones_arrived("dev", "example-dev-1a2b", zones());
    assert_eq!(app.input_mode, InputMode::AccountZonePick);
    assert_eq!(app.filtered_zones()[app.zone_selected], "europe-west1-b");
    let page = rows(&app, 100, 30).join("\n");
    assert!(page.contains("Edit account 2/2"), "{page}");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None, "nothing changed");
    assert_eq!(app.input_mode, InputMode::Accounts);
}

#[test]
fn typing_narrows_the_zones_and_enter_sends_the_pick() {
    let mut app = zone_step();
    app.zones_arrived("dev", "example-dev-1a2b", zones());
    retype(&mut app, "europe-w");
    assert_eq!(
        app.filtered_zones(),
        vec!["europe-west1-b", "europe-west1-c", "europe-west4-a"]
    );
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Op::AccountEdit(AccountEdit {
            name: "dev".to_string(),
            project: None,
            zone: Some("europe-west4-a".to_string()),
        }))
    );
}

#[test]
// Unlike a project list, which permissions or the 5,000 cap can cut short, a
// zone list is the project's whole set: a zone it lacks is a typo, and
// `account edit` would save it unchecked. `init` refuses the same input.
fn a_typed_zone_the_list_lacks_is_refused() {
    let mut app = zone_step();
    app.zones_arrived("dev", "example-dev-1a2b", zones());
    retype(&mut app, "eurpoe-west1-b");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(
        app.input_mode,
        InputMode::AccountZonePick,
        "the dialog stays"
    );
    let said = app.draft_error.clone().expect("a reason beside the list");
    assert!(
        said.contains("'eurpoe-west1-b' is not a zone of example-dev-1a2b"),
        "{said}"
    );
}

// The saved zone is offered first only while the project is the saved one;
// a project chosen in step 1 may not have it, and Enter would save that pair
#[test]
fn a_new_project_does_not_inherit_a_zone_it_lacks() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('e')));
    retype(&mut app, "example-prod-3c4d");
    app.handle_key(key(KeyCode::Enter));
    app.zones_arrived("dev", "example-prod-3c4d", zones());
    assert_eq!(app.input_mode, InputMode::AccountZonePick);
    assert!(
        !app.zones_list.iter().any(|z| z == "europe-west1-b"),
        "{:?}",
        app.zones_list
    );
    assert_eq!(app.filtered_zones()[app.zone_selected], "europe-west1-c");
}

// The zones belong to the project chosen one step back, so a list fetched
// for any other project would offer zones the account cannot use
#[test]
fn a_zone_list_for_another_project_is_dropped() {
    let mut app = zone_step();
    app.zones_arrived("dev", "example-prod-3c4d", zones());
    assert_eq!(app.input_mode, InputMode::AccountZone);
    app.zones_failed("dev", "example-prod-3c4d", "denied");
    assert_eq!(app.draft_error, None);
}

#[test]
fn a_failed_zone_list_keeps_the_text_field() {
    let mut app = zone_step();
    app.zones_failed("dev", "example-dev-1a2b", "permission denied");
    assert_eq!(app.input_mode, InputMode::AccountZone);
    assert_eq!(app.input, "europe-west1-b", "still pre-filled");
    let said = app.draft_error.clone().expect("a reason beside the field");
    assert!(said.contains("Type the zone instead"), "{said}");
}

// A field whose list is on its way says so, so the person neither waits on a
// blank field nor is surprised when it turns into a picker under the cursor
#[test]
fn a_field_says_its_list_is_loading_until_the_list_answers() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('a')));
    let page = rows(&app, 100, 30).join("\n");
    assert!(page.contains("loading configurations"), "{page}");
    app.configurations_failed("gcloud is not installed");
    let page = rows(&app, 100, 30).join("\n");
    assert!(!page.contains("loading configurations"), "{page}");

    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('e')));
    let page = rows(&app, 100, 30).join("\n");
    assert!(page.contains("loading projects"), "{page}");
    app.handle_key(key(KeyCode::Enter));
    let page = rows(&app, 100, 30).join("\n");
    assert!(page.contains("loading zones"), "{page}");
    app.zones_arrived("dev", "example-dev-1a2b", zones());
    let page = rows(&app, 100, 30).join("\n");
    assert!(!page.contains("loading zones"), "{page}");
}

// Esc ends the wait with the dialog, so the next dialog starts with no
// leftover loading line
#[test]
fn closing_a_dialog_ends_its_loading_line() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('a')));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.list_loading, None);
}

// The spinner runs in the logs pane title, behind the page; the page itself
// says what is running until the notice replaces it
#[test]
fn the_accounts_page_shows_the_operation_it_is_running() {
    let mut app = page_app();
    app.handle_key(key(KeyCode::Char('a')));
    retype(&mut app, "work");
    app.handle_key(key(KeyCode::Enter));
    retype(&mut app, "sandbox");
    assert!(app.handle_key(key(KeyCode::Enter)).is_some());
    assert_eq!(app.input_mode, InputMode::Accounts);
    let page = rows(&app, 100, 30).join("\n");
    assert!(page.contains("add sandbox"), "{page}");
}
