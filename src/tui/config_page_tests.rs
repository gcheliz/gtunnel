use super::app::*;
use super::*;
use crate::config::testing::TempConfigDir;
use crate::config::{save_config, Account, Config};
use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(key(code));
}

fn account(name: &str) -> Account {
    Account {
        name: name.to_string(),
        gcloud_configuration: "default".to_string(),
        project: format!("example-{name}-1a2b"),
        default_zone: "europe-west1-b".to_string(),
        impersonate_service_account: None,
        credential_file: None,
        expose: None,
    }
}

fn tunnel(name: &str, port: u16) -> PersistentTunnel {
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
            local_port: port,
        },
    }
}

// A config directory with a two-account config and `n` tunnels on `dev`
pub(super) fn seeded(n: usize) -> TempConfigDir {
    let dir = TempConfigDir::new();
    let mut config = Config::default();
    config.add_account(account("dev")).unwrap();
    config.add_account(account("prod")).unwrap();
    save_config(&config).unwrap();
    let tunnels = (0..n)
        .map(|i| tunnel(&format!("svc-{i:02}"), 15000 + i as u16))
        .collect();
    TunnelState {
        tunnels,
        ..Default::default()
    }
    .save()
    .unwrap();
    dir
}

fn rows(app: &App, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

fn page_app() -> App {
    let mut app = App::new("dev");
    app.accounts = vec!["dev".to_string(), "prod".to_string()];
    press(&mut app, KeyCode::Char('C'));
    app
}

#[test]
fn c_opens_a_page_with_the_path_the_text_the_count_and_the_restart_note() {
    let dir = seeded(1);
    let app = page_app();
    assert_eq!(app.input_mode, InputMode::Config);
    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    let screen = rows(&app, 120, 64).join("\n");
    let path = dir.path().join("config.toml");
    assert!(screen.contains(&path.display().to_string()), "{screen}");
    assert!(screen.contains("selected_account"), "{screen}");
    assert!(screen.contains("example-prod-1a2b"), "{screen}");
    assert!(
        screen.contains(&format!("{} lines", on_disk.lines().count())),
        "{screen}"
    );
    assert!(
        screen.contains("Running tunnels keep their old settings until R"),
        "{screen}"
    );
    assert!(screen.contains("Esc"), "{screen}");
}

#[test]
fn tab_switches_between_the_two_files() {
    let dir = seeded(2);
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.config_view.file, ConfigFile::Tunnels);
    let screen = rows(&app, 120, 40).join("\n");
    assert!(
        screen.contains(&dir.path().join("tunnels.toml").display().to_string()),
        "{screen}"
    );
    assert!(screen.contains("svc-01"), "{screen}");
    assert!(!screen.contains("selected_account"), "{screen}");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.config_view.file, ConfigFile::Config);
    assert!(rows(&app, 120, 40).join("\n").contains("selected_account"));
}

#[test]
fn a_tunnels_file_that_does_not_exist_yet_says_so() {
    let dir = seeded(0);
    std::fs::remove_file(dir.path().join("tunnels.toml")).ok();
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    let screen = rows(&app, 120, 40).join("\n");
    assert!(screen.contains("no tunnels file yet"), "{screen}");
}

// The counter `3-14 of 31 lines` and what is on screen
fn counter(rows: &[String]) -> Option<(usize, usize, usize)> {
    let row = rows.iter().find(|r| r.contains(" lines"))?;
    let words: Vec<&str> = row.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "of")?;
    let (first, last) = words[at - 1].split_once('-')?;
    Some((
        first.parse().ok()?,
        last.parse().ok()?,
        words[at + 1].parse().ok()?,
    ))
}

// The page is drawn before every key, as the loop does: the scroll keys
// clamp to what the last draw measured
fn after(app: &mut App, keys: &[KeyCode], w: u16, h: u16) -> Vec<String> {
    for code in keys {
        rows(app, w, h);
        press(app, *code);
    }
    rows(app, w, h)
}

#[test]
fn the_page_scrolls_with_the_help_pages_keys() {
    let _dir = seeded(30);
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    let (first, last, total) = counter(&after(&mut app, &[], 80, 24)).expect("a cut page counts");
    assert_eq!(first, 1);
    assert!(last < total, "{first}-{last} of {total}");

    let (first, _, _) = counter(&after(&mut app, &[KeyCode::Char('j')], 80, 24)).unwrap();
    assert_eq!(first, 2);
    let (first, _, _) = counter(&after(&mut app, &[KeyCode::Char('k')], 80, 24)).unwrap();
    assert_eq!(first, 1);

    let end = after(&mut app, &[KeyCode::Char('G')], 80, 24);
    let (_, last, total) = counter(&end).unwrap();
    assert_eq!(last, total, "G lands on the last line");
    assert!(end.join("\n").contains("svc-29"));
    let (first, _, _) = counter(&after(&mut app, &[KeyCode::Char('g')], 80, 24)).unwrap();
    assert_eq!(first, 1);

    let (first, last, _) = counter(&after(&mut app, &[KeyCode::PageDown], 80, 24)).unwrap();
    assert!(first > 1 && last > first, "{first}-{last}");
    let (first, _, _) = counter(&after(&mut app, &[KeyCode::PageUp], 80, 24)).unwrap();
    assert_eq!(first, 1);
}

#[test]
fn the_way_out_stays_on_screen_at_every_size() {
    let _dir = seeded(30);
    for (w, h) in [(120, 40), (80, 24), (40, 12), (20, 8)] {
        let mut app = page_app();
        press(&mut app, KeyCode::Tab);
        let screen = after(&mut app, &[], w, h);
        assert_eq!(screen.len(), h as usize);
        assert!(
            screen.iter().any(|r| r.contains("Esc")),
            "{w}x{h}: no Esc\n{}",
            screen.join("\n")
        );
    }
}

#[test]
fn no_page_row_is_wider_than_the_frame_or_taller_than_the_terminal() {
    let _dir = seeded(30);
    for (w, h) in [
        (120u16, 40u16),
        (80, 24),
        (40, 12),
        (20, 8),
        (10, 4),
        (1, 1),
    ] {
        let mut app = page_app();
        press(&mut app, KeyCode::Tab);
        // Drawing must not panic at any size
        rows(&app, w, h);
        let inner = ui::help_inner_width(w);
        let room = (h as usize).saturating_sub(4);
        let page = ui::config_page(inner, room, &app.config_view);
        assert!(
            page.lines.len() <= room.max(1),
            "{w}x{h}: {} rows",
            page.lines.len()
        );
        for line in &page.lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.chars().count() <= inner.max(1),
                "{w}x{h}: {text:?} is wider than {inner}"
            );
        }
    }
}

#[test]
fn the_title_is_cut_from_the_front_so_the_file_name_survives() {
    let _dir = seeded(1);
    let app = page_app();
    let screen = rows(&app, 30, 14).join("\n");
    assert!(screen.contains("config.toml"), "{screen}");
}

#[test]
fn closing_the_page_resets_it_and_leaves_the_list_alone() {
    let _dir = seeded(30);
    for close in [KeyCode::Esc, KeyCode::Char('q')] {
        let mut app = page_app();
        press(&mut app, KeyCode::Tab);
        after(&mut app, &[KeyCode::Char('G')], 80, 24);
        assert!(app.config_view.scroll > 0);
        press(&mut app, close);
        assert_eq!(app.input_mode, InputMode::Normal, "{close:?}");
        assert!(!app.should_quit, "q closes the page, it does not quit");
        assert_eq!(app.config_view.scroll, 0);
        // Reopening starts at the config file again
        press(&mut app, KeyCode::Char('C'));
        assert_eq!(app.config_view.file, ConfigFile::Config);
    }
}

#[test]
fn the_demo_has_no_config_files_to_show() {
    let mut app = App::new_demo();
    // The demo greets with a status of its own, which would satisfy a
    // check for the word "demo" before the key was ever pressed
    app.status = None;
    press(&mut app, KeyCode::Char('C'));
    assert_eq!(app.input_mode, InputMode::Normal);
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("no config files"), "{status}");
}

// A test that forgot its temporary directory would read the maintainer's
// real configuration; the page refuses before it can
#[test]
fn under_test_the_page_refuses_the_real_config_directory() {
    let _guard = TempConfigDir::new();
    // SAFETY: the guard above holds the lock every config-dir test takes
    unsafe { std::env::remove_var("GTUNNEL_CONFIG_DIR") };
    let mut app = App::new("dev");
    press(&mut app, KeyCode::Char('C'));
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.status_error);
}

// --- editing: stage, edit, validate, commit, reload ---

use std::path::Path;

fn append(path: &Path, extra: &str) -> anyhow::Result<()> {
    let mut text = std::fs::read_to_string(path)?;
    text.push_str(extra);
    std::fs::write(path, text)?;
    Ok(())
}

const DEV_AND_PROD: &str = "selected_account = \"dev\"\n\
    [[accounts]]\nname = \"dev\"\ngcloud_configuration = \"default\"\nproject = \"example-dev-1a2b\"\ndefault_zone = \"europe-west1-b\"\n\
    [[accounts]]\nname = \"prod\"\ngcloud_configuration = \"default\"\nproject = \"example-prod-3c4d\"\ndefault_zone = \"europe-west1-b\"\n";

fn adds_staging(path: &Path) -> anyhow::Result<()> {
    append(
        path,
        "\n[[accounts]]\nname = \"staging\"\ngcloud_configuration = \"default\"\nproject = \"example-staging-5e6f\"\ndefault_zone = \"europe-west1-b\"\n",
    )
}

fn drops_dev(path: &Path) -> anyhow::Result<()> {
    std::fs::write(
        path,
        "selected_account = \"prod\"\n[[accounts]]\nname = \"prod\"\ngcloud_configuration = \"default\"\nproject = \"example-prod-3c4d\"\ndefault_zone = \"europe-west1-b\"\n",
    )?;
    Ok(())
}

fn breaks_config(path: &Path) -> anyhow::Result<()> {
    std::fs::write(path, "selected_account = \"dev\"\nbroken = = 1\n")?;
    Ok(())
}

fn selects_a_ghost(path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    std::fs::write(
        path,
        text.replace("selected_account = \"dev\"", "selected_account = \"ghost\""),
    )?;
    Ok(())
}

fn repairs_if_broken(path: &Path) -> anyhow::Result<()> {
    if std::fs::read_to_string(path)?.contains("broken") {
        std::fs::write(path, DEV_AND_PROD)?;
        return Ok(());
    }
    anyhow::bail!("the staged copy did not hold the earlier edit")
}

fn leaves_alone(_: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn editor_fails(_: &Path) -> anyhow::Result<()> {
    anyhow::bail!("vi exited with exit status: 1")
}

fn adds_a_tunnel(path: &Path) -> anyhow::Result<()> {
    let mut text = std::fs::read_to_string(path)?;
    let extra = toml::to_string_pretty(&TunnelState {
        tunnels: vec![tunnel("queue", 16000)],
        ..Default::default()
    })?;
    text.push('\n');
    text.push_str(&extra);
    std::fs::write(path, text)?;
    Ok(())
}

fn renames_a_tunnel(path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    std::fs::write(path, text.replace("svc-00", "postgres"))?;
    Ok(())
}

fn names_a_tunnel_badly(path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    std::fs::write(path, text.replace("svc-01", "Bad_Name"))?;
    Ok(())
}

fn leftovers(dir: &TempConfigDir) -> Vec<String> {
    std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".edit-"))
        .collect()
}

fn edit(app: &mut App, editor: fn(&Path) -> anyhow::Result<()>) {
    app.editor = editor;
    press(app, KeyCode::Char('e'));
}

#[test]
fn a_valid_config_edit_replaces_the_file_and_reloads_without_moving_the_account() {
    let dir = seeded(1);
    let mut app = page_app();
    edit(&mut app, adds_staging);

    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(on_disk.contains("staging"), "{on_disk}");
    assert_eq!(app.accounts, ["dev", "prod", "staging"]);
    assert_eq!(app.account, "dev", "the selected account survives");
    assert_eq!(app.project(), "example-dev-1a2b");
    assert_eq!(
        app.projects.get("staging").map(String::as_str),
        Some("example-staging-5e6f")
    );
    let status = app.status.clone().unwrap_or_default();
    assert!(status.starts_with("reloaded"), "{status}");
    assert!(!app.status_error, "{status}");
    assert!(app.force_refresh, "the list is asked for again");
    assert!(app.clear_screen, "the editor drew over the screen");
    assert_eq!(app.config_view.error, None);
    // A finished edit is not drawn by the log title, which says nothing
    // once an operation is done, and the page covers it anyway: so the
    // page itself has to say what happened
    let screen = rows(&app, 120, 40).join("\n");
    assert!(screen.contains("reloaded: config.toml"), "{screen}");
    assert!(
        app.config_view.text.as_deref().unwrap().contains("staging"),
        "the page shows the file as it is now"
    );
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.path().join("config.toml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_valid_tunnels_edit_replaces_the_file_and_counts_what_changed() {
    let dir = seeded(2);
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, adds_a_tunnel);

    let on_disk = std::fs::read_to_string(dir.path().join("tunnels.toml")).unwrap();
    assert!(on_disk.contains("queue"), "{on_disk}");
    let status = app.status.clone().unwrap_or_default();
    assert!(
        status.starts_with("reloaded: 0 changed, 1 added, 0 removed"),
        "{status}"
    );
    assert!(
        status.contains("running tunnels keep their old settings until R"),
        "{status}"
    );
    assert!(app.force_refresh);
    assert!(leftovers(&dir).is_empty());
    let screen = rows(&app, 120, 40).join("\n");
    assert!(
        screen.contains("reloaded: 0 changed, 1 added, 0 removed"),
        "{screen}"
    );
}

#[test]
fn an_edit_that_changes_nothing_is_not_a_reload() {
    let dir = seeded(1);
    let mut app = page_app();
    edit(&mut app, leaves_alone);
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("no changes"), "{status}");
    assert!(rows(&app, 120, 40)
        .join("\n")
        .contains("no changes to config.toml"));
    assert!(!app.force_refresh, "nothing was reloaded");
    assert!(leftovers(&dir).is_empty());
    assert!(app.clear_screen, "the editor still drew over the screen");
}

#[test]
fn an_invalid_edit_never_reaches_the_file_and_the_page_says_why() {
    let dir = seeded(1);
    let real = dir.path().join("config.toml");
    let before = std::fs::read(&real).unwrap();
    let mut app = page_app();
    edit(&mut app, breaks_config);

    assert_eq!(std::fs::read(&real).unwrap(), before, "byte for byte");
    assert_eq!(app.input_mode, InputMode::Config, "the page stays open");
    let error = app.config_view.error.clone().expect("an error");
    assert!(error.contains("line 2"), "{error}");
    assert!(app.status_error);
    let staged = app.config_view.staged.clone().expect("the copy is kept");
    assert!(staged.path.exists());
    let screen = rows(&app, 120, 40).join("\n");
    assert!(screen.contains("not saved: line 2"), "{screen}");
    assert!(
        screen.contains("broken = = 1"),
        "the page shows the edit: {screen}"
    );
    assert!(screen.contains("edit again"), "{screen}");
    assert!(screen.contains("discard"), "{screen}");
}

#[test]
fn e_after_a_refused_edit_reopens_the_same_copy() {
    let dir = seeded(1);
    let mut app = page_app();
    edit(&mut app, breaks_config);
    assert!(app.config_view.error.is_some());
    // The second editor refuses to run unless it finds the first one's text
    edit(&mut app, repairs_if_broken);

    assert_eq!(app.config_view.error, None, "{:?}", app.status);
    assert!(app.status.clone().unwrap().starts_with("reloaded"));
    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(on_disk.contains("example-prod-3c4d"), "{on_disk}");
    assert!(leftovers(&dir).is_empty());
    assert_eq!(app.config_view.staged, None);
}

#[test]
fn esc_after_a_refused_edit_discards_the_copy_and_keeps_the_file() {
    let dir = seeded(1);
    let real = dir.path().join("config.toml");
    let before = std::fs::read(&real).unwrap();
    let mut app = page_app();
    edit(&mut app, breaks_config);
    assert_eq!(leftovers(&dir).len(), 1);

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
    assert_eq!(std::fs::read(&real).unwrap(), before);
    assert!(app.status.clone().unwrap().contains("discarded"));
}

#[test]
fn tab_is_inert_while_a_refused_edit_waits() {
    let _dir = seeded(1);
    let mut app = page_app();
    edit(&mut app, breaks_config);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.config_view.file, ConfigFile::Config);
    assert!(app.config_view.error.is_some(), "the edit is not lost");
}

#[test]
fn a_selected_account_that_does_not_exist_is_refused() {
    let dir = seeded(1);
    let before = std::fs::read(dir.path().join("config.toml")).unwrap();
    let mut app = page_app();
    edit(&mut app, selects_a_ghost);
    let error = app.config_view.error.clone().expect("an error");
    assert!(error.contains("ghost"), "{error}");
    assert_eq!(
        std::fs::read(dir.path().join("config.toml")).unwrap(),
        before
    );
}

// `log_level` typed below the last table lands inside it, where serde drops it
fn misplaces_log_level(path: &Path) -> anyhow::Result<()> {
    std::fs::write(
        path,
        "selected_account = \"dev\"\n\
         [[accounts]]\nname = \"dev\"\ngcloud_configuration = \"default\"\nproject = \"example-dev-1a2b\"\ndefault_zone = \"europe-west1-b\"\n\
         [accounts.expose]\nrelay_instance = \"relay\"\nrelay_zone = \"europe-west1-b\"\nnetwork = \"default\"\ndefault_domain = \"example.com\"\n\
         [[accounts.expose.domains]]\ndomain = \"example.com\"\nmanaged_zone = \"example\"\nlog_level = \"debug\"\n",
    )?;
    Ok(())
}

// Two stray keys in one tunnel's spec, to see the first named and the rest
// counted; the real keys stay, so only the strays are wrong
fn adds_two_tunnel_keys(path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    std::fs::write(
        path,
        text.replacen(
            "remote_port = 5432",
            "remote_port = 5432\nremote_prot = 5432\nlocal_prot = 5432",
            1,
        ),
    )?;
    Ok(())
}

#[test]
fn config_with_an_unknown_key_is_refused() {
    let dir = seeded(1);
    let before = std::fs::read(dir.path().join("config.toml")).unwrap();
    let mut app = page_app();
    edit(&mut app, misplaces_log_level);
    let error = app.config_view.error.clone().expect("an error");
    assert_eq!(error, "unknown key accounts[0].expose.domains[0].log_level");
    assert!(app.status_error);
    assert_eq!(
        std::fs::read(dir.path().join("config.toml")).unwrap(),
        before,
        "byte for byte"
    );
    let screen = rows(&app, 120, 40).join("\n");
    assert!(
        screen.contains("not saved: unknown key accounts[0]"),
        "{screen}"
    );
}

#[test]
fn tunnels_with_an_unknown_key_is_refused() {
    let dir = seeded(1);
    let before = std::fs::read(dir.path().join("tunnels.toml")).unwrap();
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, adds_two_tunnel_keys);
    let error = app.config_view.error.clone().expect("an error");
    assert_eq!(error, "unknown key tunnels[0].spec.local_prot and 1 more");
    assert_eq!(
        std::fs::read(dir.path().join("tunnels.toml")).unwrap(),
        before
    );
}

#[test]
fn an_unknown_key_keeps_the_copy() {
    let dir = seeded(1);
    let mut app = page_app();
    edit(&mut app, misplaces_log_level);
    assert!(app.config_view.error.is_some());
    assert_eq!(leftovers(&dir).len(), 1, "the refused copy stays");
    let staged = app.config_view.staged.clone().expect("the copy is kept");
    assert!(staged.path.exists());

    // The second editor refuses to run unless it finds the first one's text
    fn repairs_if_misplaced(path: &Path) -> anyhow::Result<()> {
        if std::fs::read_to_string(path)?.contains("log_level") {
            std::fs::write(path, DEV_AND_PROD)?;
            return Ok(());
        }
        anyhow::bail!("the staged copy did not hold the earlier edit")
    }
    edit(&mut app, repairs_if_misplaced);
    assert_eq!(app.config_view.error, None, "{:?}", app.status);
    assert!(app.status.clone().unwrap().starts_with("reloaded"));
    assert!(leftovers(&dir).is_empty());
}

#[test]
fn a_valid_file_still_saves() {
    let dir = seeded(1);
    let mut app = page_app();
    edit(&mut app, adds_staging);
    assert_eq!(app.config_view.error, None, "{:?}", app.status);
    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(on_disk.contains("staging"), "{on_disk}");
}

#[test]
fn a_tunnel_name_that_cannot_be_a_label_is_refused_with_its_line() {
    let dir = seeded(3);
    let before = std::fs::read(dir.path().join("tunnels.toml")).unwrap();
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, names_a_tunnel_badly);
    let error = app.config_view.error.clone().expect("an error");
    assert!(
        error.contains("Bad_Name") && error.contains("line "),
        "{error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("tunnels.toml")).unwrap(),
        before
    );
}

#[test]
fn a_tunnel_moved_onto_a_live_runs_port_is_refused_and_the_file_is_unchanged() {
    let dir = seeded(2);
    crate::ephemeral::EphemeralRecord {
        name: "scratch".to_string(),
        account_name: "prod".to_string(),
        pid: std::process::id(),
        spec: tunnel("scratch", 17777).spec,
        started_at: 1_700_000_000,
        ttl_secs: None,
        pid_start: None,
        owner_pid: None,
        owner_start: None,
    }
    .save()
    .unwrap();
    let before = std::fs::read(dir.path().join("tunnels.toml")).unwrap();
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, moves_a_tunnel_to_the_runs_port);
    assert_eq!(
        app.config_view.error.as_deref(),
        // Line 1 is the schema_version marker, so the first header is line 2
        Some("line 2: Local port 17777 is already used by the run 'scratch'")
    );
    assert_eq!(
        std::fs::read(dir.path().join("tunnels.toml")).unwrap(),
        before
    );
}

fn moves_a_tunnel_to_the_runs_port(path: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    std::fs::write(path, text.replacen("15000", "17777", 1))?;
    Ok(())
}

#[test]
fn removing_the_selected_account_moves_to_the_first_remaining() {
    let _dir = seeded(1);
    let mut app = page_app();
    app.entries = vec![TunnelEntry::new(
        tunnel("svc-00", 15000),
        crate::state::TunnelStatus::Running,
        TunnelKind::Managed,
    )];
    app.loaded = true;
    edit(&mut app, drops_dev);

    assert_eq!(app.accounts, ["prod"]);
    assert_eq!(app.account, "prod");
    assert!(app.entries.is_empty(), "dev's rows do not stay under prod");
    assert!(!app.loaded, "prod's list has not answered yet");
    assert!(app.force_refresh);
}

#[test]
fn a_rename_names_the_launchd_job_it_leaves_behind() {
    let _dir = seeded(1);
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, renames_a_tunnel);
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("renamed svc-00 to postgres"), "{status}");
    assert!(status.contains("launchd"), "{status}");
}

#[test]
fn an_editor_that_fails_leaves_everything_as_it_was() {
    let dir = seeded(1);
    let real = dir.path().join("config.toml");
    let before = std::fs::read(&real).unwrap();
    let mut app = page_app();
    edit(&mut app, editor_fails);

    assert_eq!(std::fs::read(&real).unwrap(), before);
    assert!(
        leftovers(&dir).is_empty(),
        "a cancelled edit leaves no copy"
    );
    assert!(app.status_error);
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("exit status: 1"), "{status}");
    assert_eq!(app.config_view.error, None);
    let screen = rows(&app, 120, 40).join("\n");
    assert!(screen.contains("exit status: 1"), "{screen}");
    assert!(app.clear_screen);
    assert!(!app.input_gate.is_paused());
    assert_eq!(app.input_mode, InputMode::Config);
}

#[test]
fn the_default_editor_under_test_refuses_and_leaves_no_copy() {
    let dir = seeded(1);
    let mut app = page_app();
    press(&mut app, KeyCode::Char('e'));
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("off under test"), "{status}");
    assert!(leftovers(&dir).is_empty());
}

// Red text straight under the last line of the file reads as part of the
// file, and so does the grey note: a blank row says where the file ends
#[test]
fn a_blank_row_keeps_the_file_apart_from_what_is_said_about_it() {
    let _dir = seeded(1);
    let mut app = page_app();
    edit(&mut app, breaks_config);
    let screen = rows(&app, 120, 40);
    let last = screen
        .iter()
        .position(|r| r.contains("broken = = 1"))
        .unwrap();
    let inside = |row: &str| {
        row.split('\u{2502}')
            .nth(2)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    assert_eq!(inside(&screen[last + 1]), "", "{}", screen.join("\n"));
    assert!(screen[last + 2].contains("not saved: line 2"));

    let mut app = page_app();
    let screen = rows(&app, 120, 64);
    let last = screen
        .iter()
        .position(|r| r.contains("default_zone"))
        .unwrap();
    let last = screen
        .iter()
        .rposition(|r| r.contains("default_zone"))
        .unwrap()
        .max(last);
    assert_eq!(inside(&screen[last + 1]), "", "{}", screen.join("\n"));
    assert!(screen[last + 2].contains("A saved edit reloads"));
    // And a short page gives the row up before it gives up the file
    press(&mut app, KeyCode::Tab);
    let _ = rows(&app, 40, 12);
}

// The editor and a second gtunnel at once: the copy is edited, and the CLI adds
// a tunnel to the real file before the editor returns
fn adds_a_tunnel_while_the_cli_adds_another(path: &Path) -> anyhow::Result<()> {
    adds_a_tunnel(path)?;
    only_the_cli_writes(path)
}

fn only_the_cli_writes(_: &Path) -> anyhow::Result<()> {
    TunnelState::update(|s| s.add(tunnel("cache", 16001)))
}

fn only_the_cli_writes_a_third(_: &Path) -> anyhow::Result<()> {
    TunnelState::update(|s| s.add(tunnel("bus", 16002)))
}

#[test]
fn a_save_after_another_gtunnel_wrote_the_file_is_refused_and_keeps_both() {
    let dir = seeded(2);
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, adds_a_tunnel_while_the_cli_adds_another);

    let on_disk = std::fs::read_to_string(dir.path().join("tunnels.toml")).unwrap();
    assert!(
        on_disk.contains("cache") && !on_disk.contains("queue"),
        "{on_disk}"
    );
    let error = app.config_view.error.clone().expect("refused");
    assert!(
        error.starts_with("tunnels.toml changed since you opened it"),
        "{error}"
    );
    assert!(app
        .status
        .clone()
        .unwrap()
        .starts_with("not saved: tunnels.toml changed since you opened it"));
    assert!(app.status_error);
    let staged = app.config_view.staged.clone().expect("the edit is kept");
    assert!(std::fs::read_to_string(&staged.path)
        .unwrap()
        .contains("queue"));
    assert!(
        app.config_view.text.as_deref().unwrap().contains("queue"),
        "the page shows the edit"
    );
    let screen = rows(&app, 120, 40).join("\n");
    assert!(screen.contains("changed since you opened it"), "{screen}");
    assert!(
        screen.contains("edit again") && screen.contains("discard"),
        "{screen}"
    );
}

#[test]
fn a_second_save_after_a_refusal_replaces_the_change_it_was_warned_about() {
    let dir = seeded(2);
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, adds_a_tunnel_while_the_cli_adds_another);
    assert!(app.config_view.error.is_some());

    edit(&mut app, leaves_alone);
    assert_eq!(app.config_view.error, None, "{:?}", app.status);
    assert!(
        app.status.clone().unwrap().starts_with("reloaded"),
        "{:?}",
        app.status
    );
    let on_disk = std::fs::read_to_string(dir.path().join("tunnels.toml")).unwrap();
    assert!(
        on_disk.contains("queue") && !on_disk.contains("cache"),
        "{on_disk}"
    );
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

#[test]
fn a_retry_is_refused_again_if_the_file_changed_again() {
    let dir = seeded(2);
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, adds_a_tunnel_while_the_cli_adds_another);
    edit(&mut app, only_the_cli_writes_a_third);

    let error = app.config_view.error.clone().expect("refused again");
    assert!(
        error.starts_with("tunnels.toml changed since you opened it"),
        "{error}"
    );
    let on_disk = std::fs::read_to_string(dir.path().join("tunnels.toml")).unwrap();
    assert!(
        on_disk.contains("cache") && on_disk.contains("bus"),
        "{on_disk}"
    );
}

#[test]
fn an_unchanged_save_after_another_gtunnel_wrote_the_file_is_not_a_save() {
    let dir = seeded(1);
    let mut app = page_app();
    press(&mut app, KeyCode::Tab);
    edit(&mut app, only_the_cli_writes);
    assert!(app
        .status
        .clone()
        .unwrap()
        .contains("no changes to tunnels.toml"));
    assert!(std::fs::read_to_string(dir.path().join("tunnels.toml"))
        .unwrap()
        .contains("cache"));
    assert!(leftovers(&dir).is_empty());
}

#[test]
fn opening_the_page_removes_a_dead_gtunnels_copy() {
    // A pid above i32::MAX is never signalable, so it is dead on every machine
    let dir = seeded(1);
    std::fs::write(dir.path().join("tunnels.edit-4294967295.toml"), "stale\n").unwrap();
    assert_eq!(leftovers(&dir).len(), 1);

    let _app = page_app();
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

#[test]
fn a_config_save_after_account_select_is_refused() {
    fn adds_staging_while_prod_is_selected(path: &Path) -> anyhow::Result<()> {
        adds_staging(path)?;
        crate::config::update_config(|c| c.select_account("prod"))
    }
    let dir = seeded(1);
    let mut app = page_app();
    edit(&mut app, adds_staging_while_prod_is_selected);

    let error = app.config_view.error.clone().expect("refused");
    assert!(
        error.starts_with("config.toml changed since you opened it"),
        "{error}"
    );
    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(on_disk.contains("selected_account = \"prod\""), "{on_disk}");
    assert!(!on_disk.contains("staging"), "{on_disk}");
}
