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
