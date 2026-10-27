//! Tests for `ui.rs`. A child module of `ui`, so a private item stays
//! reachable without widening its visibility; the app builders they share
//! with the event-loop tests live in `tui/tests.rs`.

use crate::state::{PersistentTunnel, TunnelSpec, TunnelStatus};
use crate::tui::app::*;
use crate::tui::tests::{
    a_stopped_app, access, app_with_two, at_the_name_prompt, copy_ok, expose, failed, key,
    keymap_fixture, keys, open_ok, screen, submitted_access_draft, with_a_second_account,
};
use crate::tui::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// A refusal that never reaches the screen is a key that does nothing and
// says nothing. Only a status marked as an error survives to be drawn: the
// rest are cleared results, which the pane title deliberately drops the
// moment the operation behind them has finished.
#[test]
fn refusing_an_unmanaged_action_puts_the_reason_on_screen() {
    let mut app = app_with_two();
    app.entries[0].kind = TunnelKind::External;
    app.handle_key(key(KeyCode::Char('m')));
    assert!(app.status_error, "a refusal has to survive to be drawn");

    let buffer = screen(&app, 120, 30);
    let text: String = (0..buffer.area.height)
        .map(|y| row_chars(&buffer, y).into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("not managed by gtunnel"), "{text}");
}

// Every action in the running set refuses on someone else's process, so a
// footer built from the status alone offered five keys that all did
// nothing. The unmanaged set is empty now; `a add` is in the program
// group, which every row's kind shares.
#[test]
fn an_unmanaged_row_offers_no_action_that_would_refuse() {
    let (ctx, _) = ui::footer_groups(Some((TunnelStatus::Running, TunnelKind::External)), true);
    let keys: Vec<&str> = ctx.iter().map(|a| a.key).collect();
    assert!(keys.is_empty(), "{keys:?}");
}

// The pane read `started by gtunnel (auto-start off)` over a process
// gtunnel did not start and cannot stop: the restart count behind that
// sentence is zero because nothing is counting, not because nothing
// restarted.
#[test]
fn details_do_not_credit_gtunnel_with_someone_elses_process() {
    let mut app = app_with_two();
    app.entries[0].kind = TunnelKind::External;
    let buffer = screen(&app, 120, 30);
    let text: String = (0..buffer.area.height)
        .map(|y| row_chars(&buffer, y).into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("started by"),
        "the row is still there: {text}"
    );
    assert!(!text.contains("started by gtunnel"), "{text}");
}

// A hop that answers in under a millisecond is a measurement, not a zero:
// whole milliseconds reported every local tunnel as 0 ms, which left the
// chart with nothing to draw and the peak reading 0
#[test]
fn a_sub_millisecond_round_trip_is_not_reported_as_zero() {
    assert_eq!(ui::format_latency(Duration::from_micros(400)), "0.4 ms");
    assert_eq!(ui::format_latency(Duration::from_micros(1500)), "1.5 ms");
    assert_eq!(ui::format_latency(Duration::from_millis(340)), "340 ms");
}

// A gcloud error is a paragraph, not a line: clipped at the pane's width it
// loses the half that names the cause, so the pane wraps and the count of
// rows a wrapped line takes is what scrolls it to the newest one
#[test]
fn a_log_line_too_wide_for_the_pane_counts_as_the_rows_it_wraps_to() {
    use ratatui::text::Line;
    let lines = vec![Line::raw("0123456789"), Line::raw(""), Line::raw("abc")];
    assert_eq!(ui::wrapped_rows(&lines, 4), 3 + 1 + 1);
    // A pane with no width to speak of must not divide by zero
    assert_eq!(ui::wrapped_rows(&lines, 0), 3);
}

// "Running" alone cannot answer the question the user actually asks, which is
// whether anything started this tunnel without being told to
#[test]
fn details_distinguish_a_launchd_revival_from_a_start() {
    // RuntimeInfo already reports runs minus the one gtunnel asked for
    assert_eq!(ui::started_by(0, false), "gtunnel (auto-start off)");
    assert_eq!(ui::started_by(0, true), "gtunnel (auto-start on)");
    // Any run past the first is the service manager reviving a crash
    let revived = ui::started_by(2, false);
    assert!(revived.contains("after 2 restarts"), "{revived}");
    assert!(revived.contains("auto-start off"), "{revived}");
}

// A jump tunnel runs `ssh -N`, which prints nothing while it works, so an
// empty pane is the healthy case and must not read as a broken one
#[test]
fn an_empty_log_says_why_it_is_empty() {
    use crate::state::{AccessTarget, TunnelSpec, TunnelStatus};
    let jump = TunnelSpec::Access {
        target: AccessTarget::Jump {
            instance: "bastion".to_string(),
            zone: "europe-west1-b".to_string(),
            host: "10.0.0.10".to_string(),
        },
        remote_port: 443,
        local_port: 8443,
    };
    assert!(ui::empty_log_note(&jump, TunnelStatus::Running).contains("silent"));
    // Review Focus 5: a failed tunnel is not a quiet healthy one
    assert!(!ui::empty_log_note(&jump, TunnelStatus::Error).contains("silent"));
}

// The field and the selected list row are two roles. If they shared a
// shade the header would read as a second selection, and the field must
// stay an indexed colour so the terminal's palette still shapes it.
#[test]
fn the_field_background_is_its_own_indexed_colour_and_not_the_selection() {
    use ratatui::style::Color;
    assert!(matches!(theme::FIELD_BG, Color::Indexed(_)));
    assert_ne!(theme::FIELD_BG, theme::SELECTED_BG);
    assert_ne!(theme::FIELD_BG, Color::Reset);
}

// The brand hues are the one exception to the named-ANSI contract. The
// account field's half of that contract is asserted where the field is
// introduced, in the header tests.
#[test]
fn the_logo_is_the_only_truecolor_in_the_theme() {
    use ratatui::style::Color;
    assert_eq!(theme::LOGO_BLUE, Color::Rgb(0x42, 0x85, 0xF4));
    assert_eq!(theme::LOGO_RED, Color::Rgb(0xEA, 0x43, 0x35));
    assert_eq!(theme::LOGO_YELLOW, Color::Rgb(0xFB, 0xBC, 0x04));
    assert_eq!(theme::LOGO_GREEN, Color::Rgb(0x34, 0xA8, 0x53));
    assert_eq!(theme::LOGO_LINE, Color::Rgb(0x4D, 0xD0, 0xE1));
}

// The mark is fixed artwork, not a parameter. It has been redrawn larger
// three times and come out wrong three times, for a reason that does not
// go away: braille draws separated dots, so scaling the raster scales the
// gaps with it and the cloud reads as a dot matrix, while the word under
// it cannot scale at all. This test pins the exact cells so the next
// attempt fails here instead of on screen.
#[test]
fn the_mark_is_the_one_drawn_for_the_design_and_never_another() {
    assert_eq!(ui::MARK_ROWS, 1, "the mark is one row and stays one row");
    assert_eq!(ui::MARK_WIDTH, 11);
    let drawn: Vec<u32> = ui::mark_spans(true, 0)
        .iter()
        .flat_map(|s| s.content.chars())
        .map(|c| c as u32)
        .collect();
    assert_eq!(
        drawn,
        vec![
            0x2824, 0x2824, 0x28a4, 0x28f6, 0x28fe, 0x28ff, 0x28f7, 0x28f6, 0x2864, 0x2824, 0x2824,
        ],
        "the mark must be the cloud from the design, cell for cell"
    );
    assert_eq!(ui::NAME_ROW, "  gtunnel  ");
}

/// A braille glyph cannot be enlarged, so the README carries a vector of
/// the same dots rather than a second drawing. Two drawings drift the
/// moment nothing compares them, which is how the repository came to hold
/// a cloud the terminal never draws.
///
/// One dot is one circle on the same lattice, separated as braille
/// separates its dots. The gaps are the drawing: they are what keeps the
/// full-width row reading as a line running behind the cloud rather than
/// as its base. Squares as wide as the pitch close those gaps, and the
/// mark then reads as one bottom-heavy blob with the line swallowed.
/// The lattice is read off the file here, so this pins the shape and the
/// colours without pinning the pitch, the radius or the margins.
#[test]
fn the_readme_banner_is_the_mark_dot_for_dot() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let svg = std::fs::read_to_string(root.join("docs/logo.svg")).unwrap();

    // Only the dots are circles; the ground is the file's one <rect>.
    let mut dots: Vec<(f64, f64, String)> = Vec::new();
    for tag in svg.split("<circle").skip(1) {
        let head = tag.split('>').next().expect("an unclosed <circle");
        let attr = |name: &str| -> String {
            let key = format!(" {name}=\"");
            let at = head
                .find(&key)
                .unwrap_or_else(|| panic!("a dot <circle> with no {name}"));
            head[at + key.len()..]
                .split('"')
                .next()
                .unwrap()
                .to_string()
        };
        dots.push((
            attr("cx").parse().expect("cx is a number"),
            attr("cy").parse().expect("cy is a number"),
            attr("fill").to_ascii_uppercase(),
        ));
    }

    let axis = |vals: Vec<f64>| -> Vec<f64> {
        let mut v = vals;
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        v
    };
    let xs = axis(dots.iter().map(|c| c.0).collect());
    let ys = axis(dots.iter().map(|c| c.1).collect());
    assert_eq!(
        xs.len(),
        2 * ui::MARK_WIDTH as usize,
        "a braille cell is two dots wide, so the grid is 2 x MARK_WIDTH across"
    );
    assert_eq!(ys.len(), 4, "a braille cell is four dots tall");

    let index = |v: f64, axis: &[f64]| -> u16 {
        axis.iter().position(|a| (a - v).abs() < 0.5).unwrap() as u16
    };
    let mut drawn: Vec<(u16, u16, String)> = dots
        .iter()
        .map(|(x, y, fill)| (index(*x, &xs), index(*y, &ys), fill.clone()))
        .collect();
    drawn.sort();

    let hex = |c: ratatui::style::Color| match c {
        ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
        other => panic!("the mark's bands must be exact hues, found {other:?}"),
    };
    let mut want: Vec<(u16, u16, String)> = ui::mark_dots()
        .into_iter()
        .map(|(x, y, colour)| (x, y, hex(colour)))
        .collect();
    want.sort();

    assert_eq!(
        drawn, want,
        "docs/logo.svg is no longer the mark the TUI draws"
    );
}

/// The design canvas drew a different fallback. The code's is the one a
/// terminal without braille coverage actually shows, so it is the one the
/// canvas and the docs follow, not the other way round.
///
/// The upper-half block is barred by name: the shape has no cell whose
/// lower rows are empty, so any `▀` in here is a hole punched under the
/// cloud. The fallback carried one for exactly that long.
#[test]
fn the_fallback_is_the_one_the_code_draws_not_the_canvas() {
    let drawn: String = ui::mark_spans(false, 0)
        .iter()
        .flat_map(|s| s.content.chars())
        .collect();
    assert_eq!(
        drawn,
        "\u{2500}\u{2500}\u{2584}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2584}\u{2500}\u{2500}",
        "the fallback is the code's, not the canvas's"
    );
    assert_eq!(
        drawn.chars().count(),
        ui::MARK_WIDTH as usize,
        "the fallback keeps the lockup's geometry"
    );
    assert!(
        !drawn.contains('\u{2580}'),
        "an upper-half block opens a hole under the cloud"
    );
}

// The lockup only centres if the mark and the name are the same number of
// cells. A terminal has no half cells, so this is an equality, not a target.
#[test]
fn the_lockup_rows_are_the_same_width() {
    for row in 0..ui::MARK_ROWS {
        let mark: String = ui::mark_spans(true, row)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(mark.chars().count(), ui::MARK_WIDTH as usize, "row {row}");
    }
    assert_eq!(ui::NAME_ROW.chars().count(), ui::MARK_WIDTH as usize);
    let fallback: String = ui::mark_spans(false, 0)
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(
        fallback.chars().count(),
        ui::MARK_WIDTH as usize,
        "the fallback must not change the lockup width"
    );
}

fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

// Activity used to sit on a row of its own above the footer, where it
// crowded the keymap and said nothing about which tunnel it concerned. It
// belongs on the log it describes.
#[test]
fn activity_rides_on_the_log_pane_title() {
    assert_eq!(ui::logs_title(Some("k8s-dev"), None), "Logs: k8s-dev");
    assert_eq!(
        ui::logs_title(Some("k8s-dev"), Some("\u{280b} starting")),
        "Logs: k8s-dev \u{2014} \u{280b} starting"
    );
    // with nothing selected there is still a pane, and still activity
    assert_eq!(ui::logs_title(None, None), "Logs");
    assert_eq!(
        ui::logs_title(None, Some("importing")),
        "Logs \u{2014} importing"
    );
}

// `App::status` is set when an operation finishes and never cleared, so a
// title that showed it kept saying "start k8s-dev" long after k8s-dev had
// started. A title is standing chrome, not a log line.
#[test]
fn the_log_title_stops_saying_starting_once_it_has_started() {
    // while the operation is in flight
    assert_eq!(
        ui::activity_text(Some("\u{280b} start k8s-dev"), None, false).as_deref(),
        Some("\u{280b} start k8s-dev")
    );
    // it finished: the title going back to naming the pane is the signal
    assert_eq!(
        ui::activity_text(None, Some("started k8s-dev"), false),
        None
    );
    // a failure stays, because an error that clears itself is one nobody saw
    assert_eq!(
        ui::activity_text(None, Some("could not start k8s-dev"), true).as_deref(),
        Some("could not start k8s-dev")
    );
    // and the spinner wins over a stale error while the next op runs
    assert_eq!(
        ui::activity_text(Some("\u{280b} stop k8s-dev"), Some("it failed"), true).as_deref(),
        Some("\u{280b} stop k8s-dev")
    );
}

// The account is the one piece of state that changes what every other key
// acts on, so it is pinned to the corner where nothing else can push it.
#[test]
fn the_account_sits_flush_right_and_never_touches_the_logo() {
    let lines = ui::header_lines(120, 24, "dev", "sample-dev-1");
    assert_eq!(lines.len() as u16, ui::HEADER_HEIGHT_FULL);
    // The field is two rows tall and shares the lockup's two rows, so row
    // 0 is air: no box character, nothing at all.
    let top = line_text(&lines[0]);
    assert!(top.trim().is_empty(), "{top:?}");
    // The two caption lines are as tall as the lockup: the platform on the
    // mark's row, the subtitle on the name's row, both in the same column.
    let platform = line_text(&lines[1]);
    let subtitle = line_text(&lines[2]);
    let col = |t: &str, needle: &str| t.find(needle).map(|i| t[..i].chars().count());
    assert!(
        col(&platform, "Google Cloud Platform").is_some(),
        "{platform:?}"
    );
    assert_eq!(
        col(&platform, "Google Cloud Platform"),
        col(&subtitle, "IAP tunnel manager"),
        "{platform:?} / {subtitle:?}"
    );
    // the rule follows the name row directly
    assert!(line_text(&lines[3]).starts_with(" \u{2500}"));
    let row0 = line_text(&lines[1]);
    // One cell of margin at each end, the same the footer and the panes
    // have, so the field stops a cell short of the edge rather than on it.
    // The field is as wide as its longer line plus a cell each side.
    assert!(
        row0.ends_with(&format!(" dev{}", " ".repeat(11))),
        "{row0:?}"
    );
    assert!(line_text(&lines[2]).ends_with(" sample-dev-1  "));
    assert!(row0.starts_with(' '), "{row0:?}");
    assert_eq!(row0.chars().count(), 120);
    // The captions are separated from the mark by a divider, not by air
    assert!(row0.contains("\u{2502}  Google Cloud Platform"), "{row0:?}");
    // and that divider runs the lockup's whole height: the mark's lower
    // row and the name both carry it, so it does not stop halfway.
    for row in [1 + ui::MARK_ROWS - 1, 1 + ui::MARK_ROWS] {
        let text = line_text(&lines[row]);
        // Cells, not bytes: a braille cell is three bytes, so `find`
        // would put the divider past the mark on the row that has one.
        let divider = text
            .chars()
            .position(|c| c == '\u{2502}')
            .expect("the divider runs every row");
        assert!(
            divider < ui::MARK_WIDTH as usize + 6,
            "the divider belongs to the lockup, not the account box: {text:?}"
        );
    }
    // the name sits under the mark, letterspaced to the mark's own width
    assert!(line_text(&lines[1 + ui::MARK_ROWS]).contains(ui::NAME_ROW));
    // the rule closes the header off, inset to the same margin
    let rule = line_text(&lines[ui::HEADER_HEIGHT_FULL as usize - 1]);
    assert_eq!(rule.chars().count(), 120);
    assert_eq!(rule.matches('\u{2500}').count(), 118);
}

// The field's two rows are the lockup's two rows. If they drift apart by
// one the name sits level with the rule and the project id with the air.
#[test]
fn the_name_and_the_project_share_a_column_on_the_lockups_rows() {
    for w in [120u16, 100, 80, 60] {
        let lines = ui::header_lines(w, 24, "dev", "sample-dev-1");
        let col = |row: usize, needle: &str| {
            let t = line_text(&lines[row]);
            t.find(needle).map(|i| t[..i].chars().count())
        };
        let name = col(1, "dev").unwrap_or_else(|| panic!("name on row 1 at {w}"));
        // The mark's braille cells never contain letters, so the first
        // "dev" on the row is the account.
        assert_eq!(Some(name), col(2, "sample-dev-1"), "{w}");
        assert_eq!(line_text(&lines[1]).chars().count(), w as usize);
        assert_eq!(line_text(&lines[2]).chars().count(), w as usize);
        assert!(line_text(&lines[3]).starts_with(" \u{2500}"));
    }
}

// No border, no label: a box character on any row would mean the field has
// grown past two rows again.
#[test]
fn the_field_has_no_border_and_no_label() {
    let lines = ui::header_lines(120, 24, "dev", "sample-dev-1");
    let all: String = lines.iter().take(3).map(line_text).collect();
    for banned in ['\u{256d}', '\u{256e}', '\u{2570}', '\u{256f}'] {
        assert!(!all.contains(banned), "{all:?}");
    }
    assert!(!all.contains("ACCOUNT"), "{all:?}");
}

// The fill is what makes the two rows a field, and it must come from the
// theme rather than an exact hue.
#[test]
fn the_field_is_filled_with_a_theme_colour_and_the_name_is_bold() {
    let lines = ui::header_lines(120, 24, "dev", "sample-dev-1");
    let name = lines[1]
        .spans
        .iter()
        .find(|s| s.content.contains("dev"))
        .expect("name span");
    assert_eq!(name.style.bg, Some(theme::FIELD_BG));
    assert!(name
        .style
        .add_modifier
        .contains(ratatui::style::Modifier::BOLD));
    let project = lines[2]
        .spans
        .iter()
        .find(|s| s.content.contains("sample-dev-1"))
        .expect("project span");
    assert_eq!(project.style.bg, Some(theme::FIELD_BG));
    assert_eq!(project.style.fg, Some(theme::FIELD_SUBTITLE));
}

// Review Focus 2: cut, never wrapped, never into the lockup
#[test]
fn a_long_project_id_is_cut_with_an_ellipsis_and_the_row_fits() {
    let project = "sample-platform-production-emea-0123456789";
    assert_eq!(project.chars().count(), 42);
    for w in [120u16, 80, 60, 55, 48] {
        let lines = ui::header_lines(w, 24, "dev", project);
        for line in &lines {
            assert!(line_text(line).chars().count() <= w as usize, "{w}");
        }
        let row = line_text(&lines[2]);
        // the field may take everything but the mark and the margins: 42
        // cells need a 56-column terminal
        if w >= 56 {
            assert!(row.contains(project), "{w}: wide enough to show it whole");
        } else {
            assert!(row.contains('\u{2026}'), "{w}: {row:?}");
            // the mark's row still starts with the whole mark
            let mark: String = ui::mark_spans(true, 0)
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert!(line_text(&lines[1]).contains(&mark), "{w}");
        }
        assert!(line_text(&lines[1]).contains("dev"), "{w}");
    }
}

// The name is cut last: at the narrowest width that still draws the
// lockup, the project is dropped before a letter of the name is, and it
// is never left as a stub shorter than the floor.
#[test]
fn the_project_goes_before_the_account_name_does() {
    let project = "sample-dev-1";
    // 1 + 11 + 1 + (len("dev") + 2) + 1 = 19: room for the name only
    let lines = ui::header_lines(19, 24, "dev", project);
    assert_eq!(lines.len() as u16, ui::HEADER_HEIGHT_FULL);
    assert!(line_text(&lines[1]).contains("dev"));
    assert!(!line_text(&lines[2]).contains("sam"), "{:?}", lines[2]);
    // one cell wider and the id returns at the four-cell floor
    let lines = ui::header_lines(20, 24, "dev", project);
    assert!(
        line_text(&lines[2]).contains(" sam\u{2026} "),
        "{:?}",
        lines[2]
    );
    // one cell fewer than the name needs and the single-line fallback
    // takes over
    assert_eq!(ui::header_lines(18, 24, "dev", project).len(), 1);
}

// An empty project is a field one row tall, not a field with a blank line
#[test]
fn an_account_with_no_project_shows_the_name_alone() {
    let lines = ui::header_lines(120, 24, "dev", "");
    assert_eq!(lines.len() as u16, ui::HEADER_HEIGHT_FULL);
    assert!(line_text(&lines[1]).trim_end().ends_with("dev"));
    assert!(
        lines[2]
            .spans
            .iter()
            .all(|s| s.style.bg != Some(theme::FIELD_BG)),
        "row 2 must carry no fill: {:?}",
        lines[2]
    );
}

// Review Focus 3: an account name long enough to push the field off the edge
#[test]
fn a_long_account_name_does_not_overflow_the_row() {
    let lines = ui::header_lines(
        60,
        24,
        "platform-production-emea",
        "sample-production-emea-1",
    );
    assert!(lines.iter().all(|l| line_text(l).chars().count() <= 60));
}

// Review Focus 1: narrower than the lockup plus the field, the header sheds
// a row rather than wrapping the mark mid-glyph
#[test]
fn a_terminal_too_narrow_for_the_lockup_gets_one_line() {
    let lines = ui::header_lines(18, 24, "dev", "sample-dev-1");
    assert_eq!(lines.len(), 1);
    assert_eq!(ui::header_height(18, 24, "dev", "sample-dev-1"), 1);
    assert!(line_text(&lines[0]).chars().count() <= 18);
}

fn sample_notice() -> crate::update::UpdateNotice {
    crate::update::UpdateNotice {
        latest: "9.9.9".to_string(),
        command: "gtunnel update",
    }
}

// The rule already spans the screen, so the notice costs no row: it is the
// header's own last line with words in it, still exactly as wide.
#[test]
fn the_header_rule_carries_an_available_update() {
    let mut app = app_with_two();
    app.update = Some(sample_notice());
    let buffer = screen(&app, 120, 30);
    let rule = row_chars(&buffer, (ui::HEADER_HEIGHT_FULL - 1) as usize as u16);
    let text: String = rule.iter().collect();
    assert!(
        text.contains("\u{2191} v9.9.9 \u{b7} gtunnel update"),
        "{text:?}"
    );
    assert_eq!(rule.len(), 120);
    let at = find(&rule, "\u{2191}").expect("the arrow");
    assert_eq!(buffer[(at as u16, 3)].fg, theme::WARN);
    let dot = find(&rule, "\u{b7}").expect("the separator");
    assert_eq!(buffer[(dot as u16, 3)].fg, theme::MUTED);
    let command = find(&rule, "gtunnel update").expect("the command");
    assert_eq!(buffer[(command as u16, 3)].fg, theme::LABEL);
    // Never both rules: the footer keeps its plain dashes
    assert_eq!(render_to_string(&app, 120, 30).matches("v9.9.9").count(), 1);
}

// A fragment of a command is worse than none, so a step is dropped whole
// and only while at least RULE_LEAD dashes stay before it.
#[test]
fn the_notice_sheds_whole_steps_as_the_width_drops() {
    let notice = sample_notice();
    let full = "\u{2191} v9.9.9 \u{b7} gtunnel update";
    let short = "\u{2191} v9.9.9";
    assert_eq!(full.chars().count(), 25);
    for w in 2..=140usize {
        let line = ui::rule_line_with(w, Some(&notice));
        let text = line_text(&line);
        assert_eq!(text.chars().count(), w, "width {w}: {text:?}");
        let shown = if text.contains(full) {
            full
        } else if text.contains(short) {
            short
        } else {
            assert!(
                !text.contains('\u{2191}') && !text.contains("gtunnel") && !text.contains('v'),
                "width {w} shows a fragment: {text:?}"
            );
            ""
        };
        // margin, dashes, space, notice, space, two dashes, margin
        let lead = if shown.is_empty() {
            w
        } else {
            w - shown.chars().count() - 6
        };
        assert!(shown.is_empty() || lead >= ui::RULE_LEAD, "width {w}");
        let wanted = if w >= full.chars().count() + 6 + ui::RULE_LEAD {
            full
        } else if w >= short.chars().count() + 6 + ui::RULE_LEAD {
            short
        } else {
            ""
        };
        assert_eq!(shown, wanted, "width {w}: {text:?}");
    }
}

#[test]
fn no_notice_leaves_the_rule_as_it_was() {
    for w in 0..=140usize {
        assert_eq!(
            line_text(&ui::rule_line_with(w, None)),
            line_text(&ui::rule_line(w)),
            "width {w}"
        );
    }
}

// Under 16 rows there is no header, and the footer rule is the only rule
// left that can carry it.
#[test]
fn a_hidden_header_moves_the_notice_to_the_footer_rule() {
    let mut app = app_with_two();
    app.update = Some(sample_notice());
    let buffer = screen(&app, 100, 12);
    let holders: Vec<u16> = (0..12)
        .filter(|y| {
            row_chars(&buffer, *y)
                .iter()
                .collect::<String>()
                .contains("\u{2191} v9.9.9 \u{b7} gtunnel update")
        })
        .collect();
    // The rule sits directly above the one-row footer
    assert_eq!(holders, vec![12 - 2]);
}

// The design draws the same eleven cells on a 1200-pixel board as on a
// narrow one. A mark that scales with the terminal breaks the lockup: the
// cloud grows and the word under it cannot, so the two stop being one mark.
#[test]
fn the_lockup_is_one_size_at_every_width() {
    for (w, h) in [(80, 24), (100, 30), (160, 40), (200, 60)] {
        let lines = ui::header_lines(w, h, "dev", "sample-dev-1");
        assert_eq!(
            lines.len() as u16,
            ui::HEADER_HEIGHT_FULL,
            "{w}x{h} must draw the mark, the name and the rule"
        );
        assert_eq!(
            ui::header_height(w, h, "dev", "sample-dev-1"),
            ui::HEADER_HEIGHT_FULL
        );
        for row in 0..ui::MARK_ROWS {
            let drawn: String = line_text(&lines[1 + row])
                .chars()
                .skip(1)
                .take(ui::MARK_WIDTH as usize)
                .collect();
            let expected: String = ui::mark_spans(true, row)
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(drawn, expected, "mark row {row} differs at {w}x{h}");
        }
        assert!(line_text(&lines[1 + ui::MARK_ROWS]).contains(ui::NAME_ROW));
        for line in &lines {
            assert!(line_text(line).chars().count() <= w as usize);
        }
    }
}

// Only the mark names exact hues. Everything that answers a question about
// what gtunnel found stays on the terminal's own palette, so it is legible
// against whatever background the user chose.
#[test]
fn only_the_logo_names_an_exact_hue() {
    let judgements = [
        ("ACCENT", theme::ACCENT),
        ("MUTED", theme::MUTED),
        ("LABEL", theme::LABEL),
        ("OK", theme::OK),
        ("WARN", theme::WARN),
        ("BAD", theme::BAD),
        ("FLAG", theme::FLAG),
    ];
    for (name, colour) in judgements {
        assert!(
            !matches!(colour, ratatui::style::Color::Rgb(..)),
            "{name} names an exact hue; only the LOGO_* constants may"
        );
    }
    // And the mark's own colours are the documented exception, so the
    // test fails if someone quietly demotes them to ANSI.
    for (name, colour) in [
        ("LOGO_LINE", theme::LOGO_LINE),
        ("LOGO_BLUE", theme::LOGO_BLUE),
        ("LOGO_RED", theme::LOGO_RED),
        ("LOGO_YELLOW", theme::LOGO_YELLOW),
        ("LOGO_GREEN", theme::LOGO_GREEN),
    ] {
        assert!(
            matches!(colour, ratatui::style::Color::Rgb(..)),
            "{name} is part of the mark and must name its hue"
        );
    }
}

// Case is load-bearing in three pairs, and the old bar printed both halves
// of each. Showing only what the selection can do dissolves the trap
// instead of annotating it.
#[test]
fn the_footer_offers_only_what_the_selection_can_do() {
    let (ctx, _) = ui::footer_groups(Some((TunnelStatus::Running, TunnelKind::Managed)), true);
    let keys: Vec<_> = ctx.iter().map(|a| a.key).collect();
    assert_eq!(keys, ["S", "R", "c", "o", "L"]);
    assert!(!keys.contains(&"s"), "a running tunnel cannot be started");

    let (ctx, _) = ui::footer_groups(Some((TunnelStatus::Stopped, TunnelKind::Managed)), true);
    let keys: Vec<_> = ctx.iter().map(|a| a.key).collect();
    assert_eq!(keys, ["s", "e", "d", "A"]);
    assert!(!keys.contains(&"S"), "s and S are never on screen together");
    // a/A is the one pair that can share the screen, in two groups.
    let text = line_text(&ui::footer_line(
        120,
        Some((TunnelStatus::Stopped, TunnelKind::Managed)),
        true,
    ));
    assert!(
        text.contains("A auto") && text.contains("a add"),
        "{text:?}"
    );
}

// Review Focus 4: no tunnels at all, so there is no selection to derive a
// contextual group from. The footer still has to offer a way forward.
#[test]
fn an_empty_account_offers_a_way_to_fill_it() {
    let (ctx, _) = ui::footer_groups(None, false);
    assert_eq!(ctx.iter().map(|a| a.key).collect::<Vec<_>>(), ["m"]);
}

// `a` sat in the two sets that least needed it: an empty account and a
// stranger's row. On every managed row the key that adds a tunnel was not
// advertised at all, so the footer taught it only where nothing else was
// on offer. It belongs to the program, which is on screen whatever is
// selected.
#[test]
fn adding_a_tunnel_is_on_the_footer_in_every_state() {
    let states = [
        (Some((TunnelStatus::Running, TunnelKind::Managed)), true),
        (Some((TunnelStatus::Stopped, TunnelKind::Managed)), true),
        (Some((TunnelStatus::Error, TunnelKind::Managed)), true),
        (Some((TunnelStatus::Running, TunnelKind::External)), true),
        (Some((TunnelStatus::Stopped, TunnelKind::External)), true),
        (None, true),
        (None, false),
    ];
    for (selected, any) in states {
        let (contextual, program) = ui::footer_groups(selected, any);
        let listed = contextual.iter().chain(program).filter(|a| a.key == "a");
        assert_eq!(listed.count(), 1, "{selected:?} any={any}");
        let text = line_text(&ui::footer_line(120, selected, any));
        assert!(text.contains("a add"), "{selected:?} any={any}: {text:?}");
    }
}

// Review Focus 2: a width landing exactly on a shedding boundary, where an
// action would be drawn half-way
#[test]
fn the_footer_sheds_whole_actions_and_never_half_of_one() {
    for width in 20u16..=140 {
        let text = line_text(&ui::footer_line(
            width,
            Some((TunnelStatus::Running, TunnelKind::Managed)),
            true,
        ));
        assert!(
            text.chars().count() <= width as usize,
            "overflows at {width}"
        );
        for action in ["stop", "restart", "clear log", "account", "keys", "quit"] {
            let drawn = text.matches(&format!(" {action}")).count();
            assert!(drawn <= 1, "{action} drawn twice at {width}");
        }
        // every label that appears, appears whole
        for action in [
            "stop",
            "restart",
            "copy",
            "open",
            "clear log",
            "account",
            "keys",
            "quit",
        ] {
            let prefix = &action[..action.len() - 1];
            if text.contains(prefix) {
                assert!(
                    text.contains(action),
                    "{action} was cut mid-word at {width}: {text:?}"
                );
            }
        }
    }
}

// A failed tunnel has one obvious next move. Giving it the only WARN in the
// footer makes it the loudest thing there without moving or reordering it.
#[test]
fn a_failed_tunnel_footer_leads_with_restart_in_the_warning_colour() {
    use ratatui::style::Modifier;
    let line = ui::footer_line(120, Some((TunnelStatus::Error, TunnelKind::Managed)), true);
    let at = line
        .spans
        .iter()
        .position(|s| s.content.as_ref() == "R")
        .expect("a failed footer offers R");
    let key = &line.spans[at];
    assert_eq!(key.style.fg, Some(theme::WARN));
    assert!(key.style.add_modifier.contains(Modifier::BOLD));
    // key, one space, then the label
    let label = &line.spans[at + 2];
    assert_eq!(label.content.as_ref(), "restart");
    assert_eq!(label.style.fg, Some(ratatui::style::Color::White));

    let warn_spans = line
        .spans
        .iter()
        .filter(|s| s.style.fg == Some(theme::WARN))
        .count();
    assert_eq!(warn_spans, 1, "only the lead action is loud");

    let running = ui::footer_line(
        120,
        Some((TunnelStatus::Running, TunnelKind::Managed)),
        true,
    );
    assert!(
        running
            .spans
            .iter()
            .all(|s| s.style.fg != Some(theme::WARN)),
        "a running tunnel has nothing to lead with"
    );
}

// At 48 columns the footer holds `a add` beside restart and not yet
// `e edit`, so this marks restart leading a footer with more than restart.
#[test]
fn the_failed_footer_keeps_restart_first_at_forty_eight_columns() {
    let text = line_text(&ui::footer_line(
        48,
        Some((TunnelStatus::Error, TunnelKind::Managed)),
        true,
    ));
    assert!(text.trim_start().starts_with("R restart"), "{text:?}");
}

// `a add` is not about the selected row, and `R restart` is the one action
// a failed row exists for, so the narrow footer gives up the former first.
#[test]
fn a_narrow_failed_footer_sheds_add_before_restart() {
    let text = line_text(&ui::footer_line(
        40,
        Some((TunnelStatus::Error, TunnelKind::Managed)),
        true,
    ));
    assert!(text.trim_start().starts_with("R restart"), "{text:?}");
    assert!(!text.contains("a add"), "{text:?}");
}

// Between restart alone and restart with edit, `a add` is what the
// footer spends the next cells on: it is true of every row, `e edit` of
// this one.
#[test]
fn a_failed_footer_at_forty_six_columns_has_room_for_restart_and_add_but_not_edit() {
    let text = line_text(&ui::footer_line(
        46,
        Some((TunnelStatus::Error, TunnelKind::Managed)),
        true,
    ));
    assert!(text.contains("R restart"), "{text:?}");
    assert!(text.contains("a add"), "{text:?}");
    assert!(!text.contains("e edit"), "{text:?}");
}

// A blank pane under a header reads as a render that failed. The footer
// already names `a` and `m`, so the row only has to say the list is empty.
#[test]
fn an_empty_account_says_so_under_the_list_header() {
    let mut app = App::new("dev");
    app.replace_entries(Vec::new(), HashSet::new());
    let buffer = screen(&app, 120, 30);
    let rows: Vec<String> = (0..30)
        .map(|y| row_chars(&buffer, y).iter().collect())
        .collect();
    let title = rows
        .iter()
        .position(|r| r.contains("TUNNELS (0)"))
        .expect("the pane title counts zero tunnels");
    let header = title + 1;
    assert!(rows[header].contains("NAME"), "{:?}", rows[header]);
    let empty = &rows[header + 1];
    assert!(empty.contains("no tunnels in this account"), "{empty:?}");
    let x = find(&row_chars(&buffer, (header + 1) as u16), "no tunnels").unwrap();
    assert_eq!(buffer[(x as u16, (header + 1) as u16)].fg, theme::MUTED);
}

fn list_body_row(app: &App, width: u16, height: u16) -> String {
    let rows = text_of(&screen(app, width, height));
    let title = rows
        .iter()
        .position(|r| r.contains("TUNNELS ("))
        .expect("the pane title");
    rows[title + 2].clone()
}

// An empty list before the first answer is not an empty account: claiming
// so is false for as long as the load takes, and for good if it failed
#[test]
fn a_list_that_has_not_loaded_says_loading_not_empty() {
    let app = App::new("dev");
    let row = list_body_row(&app, 120, 12);
    assert!(row.contains("loading"), "{row:?}");
    assert!(!row.contains("no tunnels"), "{row:?}");
    let buffer = screen(&app, 120, 12);
    let y = text_of(&buffer)
        .iter()
        .position(|r| r.contains("loading"))
        .unwrap() as u16;
    let x = find(&row_chars(&buffer, y), "loading").unwrap() as u16;
    assert_eq!(buffer[(x, y)].fg, theme::MUTED);
}

#[test]
fn the_first_answer_turns_loading_into_the_empty_row() {
    let mut app = App::new("dev");
    app.replace_entries(Vec::new(), HashSet::new());
    assert!(app.loaded);
    assert!(list_body_row(&app, 120, 12).contains("no tunnels in this account"));
}

#[test]
fn cycling_accounts_goes_back_to_loading_until_the_new_list_arrives() {
    let mut app = keymap_fixture();
    app.loaded = true;
    app.handle_key(key(KeyCode::Char(';')));
    assert!(!app.loaded);
    assert!(list_body_row(&app, 120, 12).contains("loading"));
}

// `loading` promises an answer. After an error none is coming until the
// timer asks again, and a broken tunnels.toml read as a load in progress
// for as long as it stayed broken.
#[test]
fn a_first_load_that_failed_says_so_not_loading() {
    let mut app = App::new("dev");
    app.load_failed("tunnels.toml is not valid");
    let row = list_body_row(&app, 120, 12);
    assert!(row.contains("could not load tunnels"), "{row:?}");
    assert!(!row.contains("loading"), "{row:?}");
    let buffer = screen(&app, 120, 12);
    let y = text_of(&buffer)
        .iter()
        .position(|r| r.contains("could not load tunnels"))
        .unwrap() as u16;
    let x = find(&row_chars(&buffer, y), "could not load tunnels").unwrap() as u16;
    assert_eq!(buffer[(x, y)].fg, theme::MUTED);

    app.replace_entries(Vec::new(), HashSet::new());
    let row = list_body_row(&app, 120, 12);
    assert!(row.contains("no tunnels in this account"), "{row:?}");
    assert!(!row.contains("could not load"), "{row:?}");
}

#[test]
fn a_failure_on_one_account_does_not_follow_the_next() {
    let mut app = keymap_fixture();
    app.entries.clear();
    app.load_failed("tunnels.toml is not valid");
    app.handle_key(key(KeyCode::Char(';')));
    assert!(list_body_row(&app, 120, 12).contains("loading"));
}

// The footer and the help page are two views of one table. A footer action
// with no row in it, or a different word for the same key, is the drift the
// table exists to prevent.
#[test]
fn every_footer_action_is_in_the_keymap_under_the_same_label() {
    let states = [
        (Some((TunnelStatus::Running, TunnelKind::Managed)), true),
        (Some((TunnelStatus::Stopped, TunnelKind::Managed)), true),
        (Some((TunnelStatus::Error, TunnelKind::Managed)), true),
        (Some((TunnelStatus::Running, TunnelKind::External)), true),
        (None, false),
    ];
    for (selected, any) in states {
        let (contextual, program) = ui::footer_groups(selected, any);
        for action in contextual.iter().chain(program) {
            let doc = ui::KEYMAP
                .iter()
                .find(|d| d.key == action.key)
                .unwrap_or_else(|| panic!("{} is on the footer but not in KEYMAP", action.key));
            assert_eq!(doc.footer, Some(action.label), "key {}", action.key);
        }
    }
}

// What a key does, as far as the app can say without a terminal: any of
// these moving is an effect. `Esc` and `Ctrl+Z` are not Normal-mode keys and
// get their own checks below.
fn footprint(app: &App, op: &Option<Op>) -> String {
    format!(
        "{:?}",
        (
            app.selected,
            app.input_mode,
            app.should_quit,
            app.suspend,
            &app.status,
            app.force_refresh,
            &app.account,
            app.entries.get(1).map(|e| e.health),
            app.spinner.is_running(),
            op.is_some(),
        )
    )
}

// A help page that lists a key nothing handles is worse than none. `c`, `l` and
// `o` act outside the process, so they run against stand-ins for the
// clipboard and the browser and are checked by what the status line says.
#[test]
fn every_key_the_help_page_lists_is_handled() {
    const OUTSIDE_THE_PROCESS: [&str; 3] = ["c", "o", "l"];
    const HELP_ONLY: [&str; 2] = ["PgUp/Dn", "g/G"];
    for doc in ui::KEYMAP {
        if doc.key == "Esc" {
            let mut app = keymap_fixture();
            app.input_mode = InputMode::Help;
            app.handle_key(key(KeyCode::Esc));
            assert_eq!(app.input_mode, InputMode::Normal, "Esc closes help");
            continue;
        }
        if doc.key == "Ctrl+C" {
            let mut app = keymap_fixture();
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
            assert!(app.should_quit, "Ctrl+C quits");
            continue;
        }
        // The config page's keys act on the page, which reads files: each
        // runs against a temporary directory so none can touch the real one
        if doc.key == "C" {
            let _dir = config_page_tests::seeded(1);
            let mut app = keymap_fixture();
            app.handle_key(key(KeyCode::Char('C')));
            assert_eq!(app.input_mode, InputMode::Config, "C opens the page");
            continue;
        }
        if doc.key == "Tab" {
            let _dir = config_page_tests::seeded(1);
            let mut app = keymap_fixture();
            app.handle_key(key(KeyCode::Char('C')));
            app.handle_key(key(KeyCode::Tab));
            assert_eq!(app.config_view.file, ConfigFile::Tunnels, "Tab switches");
            continue;
        }
        if doc.key == "e" && doc.group == ui::Group::Program {
            let _dir = config_page_tests::seeded(1);
            let mut app = keymap_fixture();
            app.handle_key(key(KeyCode::Char('C')));
            let before = footprint(&app, &None);
            let op = app.handle_key(key(KeyCode::Char('e')));
            assert_ne!(
                before,
                footprint(&app, &op),
                "e on the page changes nothing"
            );
            continue;
        }
        if doc.key == "Ctrl+Z" {
            let mut app = keymap_fixture();
            app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
            assert!(app.suspend, "Ctrl+Z suspends");
            continue;
        }
        // Bound on the help page only: pressed in the list they do nothing,
        // so each is checked where it acts, by the position it moves
        if HELP_ONLY.contains(&doc.key) {
            let (down, up) = if doc.key == "g/G" {
                (KeyCode::Char('G'), KeyCode::Char('g'))
            } else {
                (KeyCode::PageDown, KeyCode::PageUp)
            };
            let mut app = keymap_fixture();
            app.input_mode = InputMode::Help;
            screen(&app, 80, 24);
            app.handle_key(key(down));
            assert!(
                app.help_scroll > 0,
                "{} ({down:?}) scrolls nothing",
                doc.key
            );
            screen(&app, 80, 24);
            app.handle_key(key(up));
            assert!(
                app.help_scroll < app.help_extent.get().max_scroll,
                "{} ({up:?}) scrolls nothing",
                doc.key
            );
            continue;
        }
        if OUTSIDE_THE_PROCESS.contains(&doc.key) {
            let mut app = keymap_fixture();
            app.clipboard = copy_ok;
            app.opener = open_ok;
            app.logs = vec!["line 1".to_string()];
            let op = app.handle_key(key(KeyCode::Char(doc.key.chars().next().unwrap())));
            assert!(op.is_none(), "{} starts no operation", doc.key);
            let status = app.status.clone().unwrap_or_default();
            let wanted = if doc.key == "c" {
                "copied localhost:16379"
            } else if doc.key == "l" {
                "copied 1 log line for cache"
            } else {
                "opened http://localhost:16379"
            };
            assert_eq!(status, wanted, "{} reports what it did", doc.key);
            assert!(!app.status_error, "{} succeeded", doc.key);
            continue;
        }
        let codes: Vec<KeyCode> = doc
            .key
            .split('/')
            .map(|part| match part {
                "up" => KeyCode::Up,
                "down" => KeyCode::Down,
                one => KeyCode::Char(one.chars().next().unwrap()),
            })
            .collect();
        for code in codes {
            let mut app = keymap_fixture();
            let before = footprint(&app, &None);
            let op = app.handle_key(key(code));
            assert_ne!(
                before,
                footprint(&app, &op),
                "{} ({code:?}) is listed but changes nothing",
                doc.key
            );
        }
    }
}

#[test]
fn running_tunnel_bullet_color_is_orange_until_health_is_confirmed() {
    assert_eq!(
        ui::status_style(TunnelStatus::Running, HealthStatus::Checking).fg,
        Some(theme::WARN)
    );
    assert_eq!(
        ui::status_style(TunnelStatus::Running, HealthStatus::Unknown).fg,
        Some(theme::WARN)
    );
    assert_eq!(
        ui::status_style(TunnelStatus::Running, HealthStatus::Healthy).fg,
        Some(theme::OK)
    );
    assert_eq!(
        ui::status_style(TunnelStatus::Running, HealthStatus::Unhealthy).fg,
        Some(theme::BAD)
    );
    assert_eq!(
        ui::status_style(TunnelStatus::Stopped, HealthStatus::Unknown).fg,
        Some(theme::MUTED)
    );
}

fn help_app() -> App {
    let mut app = app_with_two();
    app.input_mode = InputMode::Help;
    app
}

#[test]
fn the_footer_never_shows_more_than_the_board_says_it_does() {
    assert_eq!(ui::footer_most(), 10);
}

// The page is reached with `:`, the shift of the `;` that cycles accounts,
// so it pairs with none of the three the case note warns about. Its footer
// word is not "accounts": beside `; account` the two would differ by one
// letter, which is the trap the contextual footer exists to avoid.
#[test]
fn the_accounts_page_is_in_the_keymap_and_on_the_footer() {
    let doc = ui::KEYMAP
        .iter()
        .find(|d| d.key == ":")
        .expect(": is not in KEYMAP");
    assert_eq!(doc.group, ui::Group::Program);
    assert_eq!(doc.footer, Some("manage"));
    let text = line_text(&ui::footer_line(
        120,
        Some((TunnelStatus::Running, TunnelKind::Managed)),
        true,
    ));
    assert!(text.contains(": manage"), "{text:?}");
    let rows = text_of(&screen(&help_app(), 120, 40));
    let row = format!("{:<8}{}", doc.key, doc.label);
    assert!(
        rows.iter().any(|r| r.contains(&row)),
        "{row:?} is not on the page:\n{}",
        rows.join("\n")
    );
}

#[test]
fn a_wide_terminal_lays_the_help_page_out_in_two_columns() {
    let rows = text_of(&screen(&help_app(), 120, 40));
    let both = rows
        .iter()
        .find(|r| r.contains("THIS TUNNEL'S URL AND LOG"))
        .expect("the url and log group has a heading");
    assert!(
        both.contains("TUNNEL "),
        "the first group shares its row: {both:?}"
    );
    for heading in [
        "TUNNEL ",
        "THE LIST",
        "THIS TUNNEL'S URL AND LOG",
        "THE PROGRAM",
    ] {
        assert!(rows.iter().any(|r| r.contains(heading)), "{heading}");
    }
    let all = rows.join("\n");
    for doc in ui::KEYMAP {
        assert!(
            all.contains(doc.label),
            "{} is missing: {}",
            doc.key,
            doc.label
        );
    }
    assert!(all.contains("Case is load-bearing"), "the case note");
    assert!(all.contains("Metrics every 5s"), "the cadence lines");
    // Literal on purpose: re-deriving the figure from the table would pass
    // for any table, and this one has to change when a key is added
    let count_row = rows
        .iter()
        .find(|r| r.contains("32 keys bound - the footer shows at most 10"))
        .expect("the count line");
    // In this page `q` closes the page, it does not quit the program
    assert!(count_row.contains("Esc back  q back"), "{count_row:?}");
    assert!(!count_row.contains("q quit"), "{count_row:?}");
}

#[test]
fn a_narrow_terminal_lays_the_help_page_out_in_one_column() {
    let buffer = screen(&help_app(), 60, 24);
    let rows = text_of(&buffer);
    let at = |needle: &str| {
        rows.iter()
            .enumerate()
            .find_map(|(y, r)| r.find(needle).map(|x| (y, r[..x].chars().count())))
    };
    let (tunnel_y, tunnel_x) = at("TUNNEL ").expect("first heading");
    let (list_y, list_x) = at("THE LIST").expect("second heading");
    assert_eq!(tunnel_x, list_x, "one column: headings share a left edge");
    assert!(list_y > tunnel_y);
    assert!(
        at("THIS TUNNEL'S URL AND LOG").is_none_or(|(y, _)| y > list_y),
        "the url group is below, not beside"
    );
    assert!(
        rows.iter().any(|r| r.contains("Esc back")),
        "Esc survives the truncation:\n{}",
        rows.join("\n")
    );
}

// Nothing on the main screen says which gtunnel is running, and a bug
// report that names no version is a bug report about a guess. The help
// page is where someone already goes to ask what the program does.
#[test]
fn the_help_frame_names_the_running_version_when_the_border_can_hold_it() {
    let title = ui::help_modal_title(ui::help_modal_width(120));
    assert!(title.contains("Keys"), "{title:?} dropped the subject");
    assert!(
        title.contains(env!("CARGO_PKG_VERSION")),
        "{title:?} does not name the running version"
    );
}

// A clipped title is worse than no version: ratatui truncates it silently,
// so the frame would read ` Keys - gtunn`.
#[test]
fn a_narrow_help_frame_drops_the_version_rather_than_clipping_it() {
    for screen_width in 1u16..=200 {
        let modal = ui::help_modal_width(screen_width);
        let title = ui::help_modal_title(modal);
        let room = modal.saturating_sub(2) as usize;
        assert!(
            title.chars().count() <= room || room == 0,
            "{screen_width}: title {title:?} does not fit {room} cells"
        );
    }
}

#[test]
fn the_help_page_never_panics_and_keeps_every_row_in_the_terminal() {
    for (width, height) in [(1, 1), (10, 4), (20, 8), (40, 12), (60, 24), (120, 40)] {
        screen(&help_app(), width, height);
    }
    // Where the labels fit, no laid-out row is wider than the frame's inside
    for (width, height) in [(60, 24), (80, 24), (120, 40)] {
        let inner = ui::help_inner_width(width);
        for line in ui::help_page(inner, height as usize - 4, 0).lines {
            let text = line_text(&line);
            assert!(
                text.chars().count() <= inner,
                "{width}x{height}: {text:?} is wider than {inner}"
            );
        }
    }
}

// The threshold is where a right-hand row used to be clipped: a column has to
// hold the longest key row, and the gap has to survive between the columns.
#[test]
fn no_help_row_is_wider_than_its_column_at_the_threshold_or_one_above() {
    for inner in [
        ui::HELP_TWO_COLUMNS - 1,
        ui::HELP_TWO_COLUMNS,
        ui::HELP_TWO_COLUMNS + 1,
    ] {
        let rows: Vec<Vec<char>> = ui::help_page(inner, 40, 0)
            .lines
            .iter()
            .map(|l| line_text(l).chars().collect())
            .collect();
        let column = (inner - 4) / 2;
        for row in &rows {
            let text: String = row.iter().collect();
            assert!(row.len() <= inner, "inner {inner}: {text:?} overflows");
            // The cadence lines and the footer span the page, not a column
            let spans_page = ["Metrics every", "A tunnel going"]
                .iter()
                .any(|lead| text.starts_with(lead))
                || text.contains("keys bound");
            if inner >= ui::HELP_TWO_COLUMNS && row.len() > column && !spans_page {
                assert!(
                    row[column..(column + 4).min(row.len())]
                        .iter()
                        .all(|c| *c == ' '),
                    "inner {inner}: the left column runs into the gap: {text:?}"
                );
            }
        }
    }
    // And on a real screen: 98 and 99 cells are one column, 100 and 101 two
    for width in [98u16, 99, 100, 101] {
        let inner = ui::help_inner_width(width);
        for line in ui::help_page(inner, 40, 0).lines {
            assert!(line_text(&line).chars().count() <= inner, "{width}");
        }
    }
}

// Keys the page shows, found by their row (key cell then label) so a label
// that happens to occur elsewhere cannot count
fn keys_shown(rows: &[String]) -> usize {
    ui::KEYMAP
        .iter()
        .filter(|d| {
            let row = format!("{:<8}{}", d.key, d.label);
            rows.iter().any(|r| r.contains(&row))
        })
        .map(|d| d.key.split('/').count())
        .sum()
}

// The page is drawn before every key, as the loop does: the terminal's
// height is only known to a draw, and the scroll keys clamp to what it found.
fn help_after(keys: &[KeyCode], width: u16, height: u16) -> (App, Vec<String>) {
    let mut app = help_app();
    for code in keys {
        screen(&app, width, height);
        app.handle_key(key(*code));
    }
    let rows = text_of(&screen(&app, width, height));
    (app, rows)
}

// What the page says about its position: the `3-14 of 31 rows` row
fn counter(rows: &[String]) -> Option<(usize, usize, usize)> {
    let row = rows.iter().find(|r| r.contains(" rows"))?;
    let words: Vec<&str> = row.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "of")?;
    let (first, last) = words[at - 1].split_once('-')?;
    Some((
        first.parse().ok()?,
        last.parse().ok()?,
        words[at + 1].parse().ok()?,
    ))
}

// A page cut to the terminal used to say `N more keys` and stop there: the
// rest was not reachable at any size under 38 rows. A cut page says where the
// reader is in it instead, and the keys below move it.
#[test]
fn a_page_that_is_cut_says_where_the_reader_is_in_it() {
    let (_, rows) = help_after(&[], 80, 24);
    let (first, last, total) =
        counter(&rows).unwrap_or_else(|| panic!("no position counter:\n{}", rows.join("\n")));
    assert_eq!(first, 1);
    assert!(last < total, "{first}-{last} of {total}");
    assert!(
        !rows.iter().any(|r| r.contains("more keys")),
        "the old cut notice is gone:\n{}",
        rows.join("\n")
    );
    assert!(rows.iter().any(|r| r.contains("Esc back")));

    let (_, rows) = help_after(&[KeyCode::Char('j')], 80, 24);
    assert_eq!(counter(&rows), Some((2, last + 1, total)));
    let x = {
        let buffer = screen(&help_after(&[], 80, 24).0, 80, 24);
        let y = text_of(&buffer)
            .iter()
            .position(|r| r.contains(" rows"))
            .unwrap();
        buffer[(
            find(&row_chars(&buffer, y as u16), "of").unwrap() as u16,
            y as u16,
        )]
            .fg
    };
    assert_eq!(x, theme::MUTED);
}

// The case a reader at 80x24 hit: the group and the note that explain the
// collision were the rows the cut always took
#[test]
fn the_end_of_the_page_at_80_by_24_shows_the_program_group_and_the_case_note() {
    let (app, rows) = help_after(&[KeyCode::Char('G')], 80, 24);
    let all = rows.join("\n");
    assert!(all.contains("THE PROGRAM"), "{all}");
    assert!(all.contains("Ctrl+C"), "{all}");
    assert!(all.contains("Case is load-bearing"), "{all}");
    // The note wraps, so its last words are checked, not a phrase across rows
    assert!(all.contains("has to be stated."), "{all}");
    let (_, last, total) = counter(&rows).expect("a counter");
    assert_eq!(last, total, "G lands on the last row");
    assert!(app.help_scroll > 0);
    // g is the way back
    let (app, rows) = help_after(&[KeyCode::Char('G'), KeyCode::Char('g')], 80, 24);
    assert_eq!(app.help_scroll, 0);
    assert!(rows.iter().any(|r| r.contains("TUNNEL ")));
}

#[test]
fn every_key_is_reachable_on_the_help_page_at_80_by_24() {
    let mut app = help_app();
    let mut seen = String::new();
    for _ in 0..40 {
        let rows = text_of(&screen(&app, 80, 24));
        seen.push_str(&rows.join("\n"));
        app.handle_key(key(KeyCode::PageDown));
    }
    for doc in ui::KEYMAP {
        let row = format!("{:<8}{}", doc.key, doc.label);
        assert!(seen.contains(&row), "{} never reaches the screen", doc.key);
    }
}

// The keys go no further than the last row: past it each press would be a
// dead one that `k` then has to undo before the page moves back
#[test]
fn the_scroll_position_never_exceeds_the_page() {
    let mut app = help_app();
    for _ in 0..200 {
        screen(&app, 80, 24);
        app.handle_key(key(KeyCode::Char('j')));
    }
    let end = app.help_scroll;
    assert_eq!(end, app.help_extent.get().max_scroll);
    assert!(end > 0);
    for code in [KeyCode::PageDown, KeyCode::Down, KeyCode::Char('G')] {
        screen(&app, 80, 24);
        app.handle_key(key(code));
        assert_eq!(app.help_scroll, end, "{code:?}");
    }
    screen(&app, 80, 24);
    app.handle_key(key(KeyCode::Char('k')));
    assert_eq!(
        app.help_scroll,
        end - 1,
        "one k moves, it does not pay a debt"
    );
    // And the top is a floor
    for code in [KeyCode::Char('g'), KeyCode::Up, KeyCode::PageUp] {
        screen(&app, 80, 24);
        app.handle_key(key(code));
    }
    assert_eq!(app.help_scroll, 0);
}

#[test]
fn page_keys_move_by_a_screen_and_arrows_by_a_row() {
    let (app, _) = help_after(&[KeyCode::Down], 80, 24);
    assert_eq!(app.help_scroll, 1);
    let (app, _) = help_after(&[KeyCode::PageDown], 80, 24);
    let view = app.help_extent.get().view;
    assert!(view > 1);
    assert_eq!(app.help_scroll, view - 1, "a page keeps one row of overlap");
    let (app, _) = help_after(&[KeyCode::PageDown, KeyCode::PageUp], 80, 24);
    assert_eq!(app.help_scroll, 0);
}

#[test]
fn closing_the_help_page_resets_where_it_was_scrolled_to() {
    for close in [KeyCode::Esc, KeyCode::Char('?'), KeyCode::Char('q')] {
        let (mut app, _) = help_after(&[KeyCode::Char('G')], 80, 24);
        assert!(app.help_scroll > 0);
        app.handle_key(key(close));
        assert_eq!(app.input_mode, InputMode::Normal, "{close:?}");
        assert_eq!(app.help_scroll, 0, "{close:?}");
        assert!(!app.should_quit, "q closes the page, it does not quit");
        app.handle_key(key(KeyCode::Char('?')));
        let rows = text_of(&screen(&app, 80, 24));
        assert_eq!(counter(&rows).map(|c| c.0), Some(1), "{close:?}");
    }
}

// After a resize the position is clamped by the draw, so a terminal that
// grows shows the whole page and not a window past its end
#[test]
fn a_terminal_that_grows_shows_the_whole_page_again() {
    let (app, _) = help_after(&[KeyCode::Char('G')], 80, 24);
    let rows = text_of(&screen(&app, 80, 60));
    assert!(
        rows.iter().any(|r| r.contains("TUNNEL ")),
        "{}",
        rows.join("\n")
    );
    assert!(counter(&rows).is_none(), "{}", rows.join("\n"));
    assert_eq!(
        keys_shown(&rows),
        ui::KEYMAP
            .iter()
            .map(|d| d.key.split('/').count())
            .sum::<usize>()
    );
}

// Two columns are one page: scrolling a row moves both, so a row never pairs
// the left column's key with a right column that did not move
#[test]
fn two_columns_scroll_together() {
    let (_, rows) = help_after(&[], 100, 16);
    assert!(
        rows.iter()
            .any(|r| r.contains("TUNNEL ") && r.contains("THIS TUNNEL'S URL AND LOG")),
        "{}",
        rows.join("\n")
    );
    let (_, rows) = help_after(&[KeyCode::Char('j')], 100, 16);
    assert!(
        rows.iter()
            .any(|r| r.contains("start the selected tunnel") && r.contains("copy the tunnel URL")),
        "{}",
        rows.join("\n")
    );
    assert!(
        !rows
            .iter()
            .any(|r| r.contains("TUNNEL ") && r.contains("THIS TUNNEL'S")),
        "the headings scrolled off in both columns"
    );
}

// The page documents itself, and the count line counts the new rows
#[test]
fn the_keymap_documents_the_scroll_keys_in_the_program_group() {
    for key in ["PgUp/Dn", "g/G"] {
        let doc = ui::KEYMAP
            .iter()
            .find(|d| d.key == key)
            .unwrap_or_else(|| panic!("{key} is not in KEYMAP"));
        assert_eq!(doc.group, ui::Group::Program, "{key}");
    }
    let rows = text_of(&screen(&help_app(), 120, 40));
    assert!(
        rows.iter().any(|r| r.contains("32 keys bound")),
        "{}",
        rows.join("\n")
    );
}

#[test]
fn a_page_that_fits_says_nothing_about_cuts() {
    for (w, h) in [(120, 40), (80, 60)] {
        let rows = text_of(&screen(&help_app(), w, h));
        assert!(counter(&rows).is_none(), "{w}x{h}:\n{}", rows.join("\n"));
        assert_eq!(keys_shown(&rows), 32, "{w}x{h}");
    }
}

// The program group sheds last and `q quit` sheds never: a user who cannot
// read the footer must still be able to leave.
#[test]
fn the_way_out_survives_the_narrowest_terminal() {
    let text = line_text(&ui::footer_line(
        24,
        Some((TunnelStatus::Running, TunnelKind::Managed)),
        true,
    ));
    assert!(text.contains("keys") && text.contains("quit"), "{text:?}");
}

// A lockup that wraps is worse than no lockup: the mark breaks mid-glyph
// and the name lands inside it. The compact form is the escape hatch, and
// it still has to carry the account.
#[test]
fn the_header_steps_down_before_it_wraps() {
    let wide = ui::header_lines(120, 24, "dev", "sample-dev-1");
    assert_eq!(wide.len() as u16, ui::HEADER_HEIGHT_FULL);
    for line in &wide {
        assert!(
            line_text(line).chars().count() <= 120,
            "{}",
            line_text(line)
        );
    }
    let narrow = ui::header_lines(18, 24, "dev", "sample-dev-1");
    assert_eq!(narrow.len() as u16, ui::HEADER_HEIGHT_COMPACT);
    assert!(line_text(&narrow[0]).chars().count() <= 18);
    assert!(line_text(&narrow[0]).contains("dev"), "{:?}", narrow[0]);
}

// Review Focus: the header must never be the reason the list has no rows
#[test]
fn a_short_terminal_keeps_a_visible_tunnel_row() {
    let app = App::new_demo();
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).expect("terminal");
    terminal.draw(|f| ui::render(f, &app)).expect("draw");
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(text.contains("localhost:15432"), "{text}");
}

// A sparkline draws one cell per sample, so 60 samples left two fifths of a
// wide pane empty and the chart read as a chart that had stopped
#[test]
fn the_chart_uses_every_column_it_is_given() {
    let samples: Vec<u64> = (0..240).collect();
    let shown = ui::sparkline_slice(&samples, 100);
    assert_eq!(shown.len(), 100);
    assert_eq!(
        shown.last(),
        samples.last(),
        "the newest sample must be drawn"
    );
    // Fewer samples than columns is the first minute, not an error
    assert_eq!(ui::sparkline_slice(&samples[..10], 100).len(), 10);
    assert!(ui::sparkline_slice(&samples, 0).is_empty());
}

// A sparkline with no scale cannot be read: 60 bars of "some latency" say
// nothing without the number the tallest one stands for
#[test]
fn the_sparkline_is_labelled_with_its_peak() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = app_with_two();
    for sample in [12_000u64, 31_000, 18_000, 44_000, 22_000] {
        app.entries[0].history.record(sample);
    }
    let mut terminal = Terminal::new(TestBackend::new(120, 38)).unwrap();
    terminal.draw(|f| ui::render(f, &app)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(text.contains("peak 44 ms"), "{text}");
}

#[test]
fn the_frame_shows_the_list_and_both_kinds_of_url() {
    let app = app_with_two();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &app)).unwrap();

    let rendered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    for wanted in ["TUNNELS (2)", "localhost:15432", "https://api.example.com"] {
        assert!(rendered.contains(wanted), "missing {wanted:?}");
    }
}

fn render_to_string(app: &App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn row_chars(buffer: &ratatui::buffer::Buffer, y: u16) -> Vec<char> {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
        .collect()
}

fn find(row: &[char], needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    row.windows(needle.len()).position(|w| w == needle)
}

// The header row, then one row per entry, in screen order
fn list_rows(buffer: &ratatui::buffer::Buffer, entries: usize) -> (u16, Vec<Vec<char>>) {
    let title = (0..buffer.area.height)
        .find(|y| find(&row_chars(buffer, *y), "TUNNELS (").is_some())
        .expect("the list pane has a title");
    let rows = (1..=entries as u16 + 1)
        .map(|d| row_chars(buffer, title + d))
        .collect();
    (title + 1, rows)
}

fn columns_app() -> App {
    let mut app = App::new("dev");
    let mut with_flags = TunnelEntry::new(
        access("mongo-activitylog-dev", 15001),
        TunnelStatus::Running,
        TunnelKind::Managed,
    );
    with_flags.tunnel.auto_start = true;
    let mut unhealthy = TunnelEntry::new(
        expose("k8s-prod", "k8s.example.com"),
        TunnelStatus::Running,
        TunnelKind::Managed,
    );
    unhealthy.health = crate::probe::HealthStatus::Unhealthy;
    let plain = TunnelEntry::new(
        access("redis-dev", 16379),
        TunnelStatus::Stopped,
        TunnelKind::Managed,
    );
    let mut unmanaged = TunnelEntry::new(
        access("pid-24083", 0),
        TunnelStatus::Running,
        TunnelKind::External,
    );
    unmanaged.tunnel.account_name = String::new();
    app.entries = vec![with_flags, unhealthy, plain, unmanaged];
    app
}

#[test]
fn every_row_of_the_list_starts_its_columns_where_the_header_does() {
    let app = columns_app();
    let buffer = screen(&app, 160, 30);
    let (header_y, rows) = list_rows(&buffer, app.entries.len());
    let header = &rows[0];
    let name_x = find(header, "NAME").expect("NAME header");
    let mode_x = find(header, "MODE").expect("MODE header");
    let url_x = find(header, "URL").expect("URL header");

    let cells = |row: &[char], x: usize, n: usize| -> String { row[x..x + n].iter().collect() };
    let expected_modes = ["access", "public", "access", "access"];
    for (i, mode) in expected_modes.iter().enumerate() {
        let row = &rows[i + 1];
        assert_eq!(
            &cells(row, mode_x, 6),
            mode,
            "row {i}: {}",
            row.iter().collect::<String>()
        );
        assert_ne!(row[name_x], ' ', "row {i} has no name under NAME");
        assert_ne!(row[url_x], ' ', "row {i} has no url under URL");
    }
    // the longest name is shown whole, without the suffix the header carries
    let shown: String = rows[1].iter().collect();
    assert!(shown.contains("mongo-activitylog "), "{shown}");
    assert!(!shown.contains("-dev"), "{shown}");
    assert!(rows[2].iter().collect::<String>().contains("k8s-prod"));
    // the header is air around a muted label, not part of the entries
    assert_eq!(buffer[(name_x as u16, header_y)].fg, theme::MUTED);
}

#[test]
fn the_flags_sit_in_their_own_column_whatever_the_row_carries() {
    let mut app = columns_app();
    // an unmanaged row says so in the url column, so it carries nothing in
    // the flags column or after it
    let buffer = screen(&app, 160, 30);
    let (_, rows) = list_rows(&buffer, app.entries.len());
    let auto_x = find(&rows[1], "\u{27f3}").expect("auto-start flag");
    assert_eq!(
        find(&rows[2], "\u{26a0}"),
        Some(auto_x + 1),
        "the warning has the cell after the auto-start one"
    );
    assert_eq!(find(&rows[3], "\u{27f3}"), None);
    assert_eq!(find(&rows[3], "\u{26a0}"), None);
    assert_eq!(
        find(&rows[4], "[not managed"),
        find(&rows[0], "URL"),
        "the note takes the url column"
    );

    // both flags at once fill both cells and push nothing
    app.entries[0].health = crate::probe::HealthStatus::Unhealthy;
    let buffer = screen(&app, 160, 30);
    let (_, rows) = list_rows(&buffer, app.entries.len());
    assert_eq!(find(&rows[1], "\u{27f3}"), Some(auto_x));
    assert_eq!(find(&rows[1], "\u{26a0}"), Some(auto_x + 1));
}

// The header occupies a screen row, and `selected` indexes entries
#[test]
fn the_header_row_does_not_shift_the_selection() {
    use ratatui::style::Color;
    let mut app = columns_app();
    app.selected = 2;
    let buffer = screen(&app, 160, 30);
    let (header_y, rows) = list_rows(&buffer, app.entries.len());
    let name_x = find(&rows[0], "NAME").unwrap() as u16;
    let highlighted: Vec<usize> = (0..=app.entries.len())
        .filter(|i| buffer[(name_x, header_y + *i as u16)].bg == theme::SELECTED_BG)
        .collect();
    assert_eq!(
        highlighted,
        vec![3],
        "entry 2 is screen row 3 under the header"
    );
    assert_ne!(theme::SELECTED_BG, Color::Reset);
}

// The url gives way first: a name is the only thing that says which row is
// which, and a row that wrapped would be two rows
#[test]
fn a_narrow_pane_shortens_the_url_before_the_name_and_never_wraps() {
    let app = columns_app();
    let wide = screen(&app, 160, 30);
    // 50 columns: the stacked list has 46 cells inside its frame, which is
    // less than the longest name and url together
    let narrow = screen(&app, 50, 30);
    let (_, wide_rows) = list_rows(&wide, app.entries.len());
    let (_, narrow_rows) = list_rows(&narrow, app.entries.len());
    for (i, row) in narrow_rows.iter().enumerate().skip(1) {
        let text: String = row.iter().collect();
        let name_x = find(row, "\u{25cf} ").or(find(row, "\u{25cb} ")).unwrap();
        assert!(!text.is_empty(), "row {i}");
        assert_eq!(
            name_x,
            find(&wide_rows[i], "\u{25cf} ")
                .or(find(&wide_rows[i], "\u{25cb} "))
                .unwrap()
        );
    }
    let first: String = narrow_rows[1].iter().collect();
    assert!(first.contains("mongo-activitylog "), "{first}");
    let second: String = narrow_rows[2].iter().collect();
    assert!(second.contains("k8s-prod"), "{second}");
    assert!(
        !second.contains("https://k8s.example.com"),
        "the url must shrink at this width: {second}"
    );
    assert!(second.contains('\u{2026}'), "a cut url says so: {second}");
    // one screen row per entry: the row below the last entry is not a list row
    let below: String = row_chars(&narrow, list_rows(&narrow, 4).0 + 5)
        .iter()
        .collect();
    assert!(!below.contains("pid-24083"), "{below}");
}

// The list cuts a name, so the pane that names the selection is where the
// whole one has to be readable
#[test]
fn details_open_with_the_full_name_of_a_cut_tunnel() {
    let app = columns_app();
    let buffer = screen(&app, 160, 30);
    let title = (0..buffer.area.height)
        .find(|y| find(&row_chars(&buffer, *y), "DETAILS").is_some())
        .expect("the details pane has a title");
    let first: String = row_chars(&buffer, title + 1).iter().collect();
    assert!(
        first.contains("name") && first.contains("mongo-activitylog-dev"),
        "{first}"
    );
}

// The header names the project once for the whole list, where it is a
// property of the account; Details answers it for the tunnel in front of
// you, which is the pane open while a gcloud command of your own is typed
#[test]
fn details_name_the_project_behind_the_account() {
    let mut app = columns_app();
    app.projects
        .insert("dev".to_string(), "example-dev-1a2b".to_string());
    let buffer = screen(&app, 160, 30);
    let title = (0..buffer.area.height)
        .find(|y| find(&row_chars(&buffer, *y), "DETAILS").is_some())
        .expect("the details pane has a title");
    let pane: String = (title + 1..buffer.area.height)
        .flat_map(|y| row_chars(&buffer, y))
        .collect();
    assert!(
        pane.contains("project") && pane.contains("example-dev-1a2b"),
        "{pane}"
    );
}

// An account whose config names no project prints no row for it, the same
// silence the header keeps rather than a label over a blank
#[test]
fn details_drop_the_project_row_when_the_config_has_none() {
    let app = columns_app();
    let buffer = screen(&app, 160, 30);
    let text: String = (0..buffer.area.height)
        .flat_map(|y| row_chars(&buffer, y))
        .collect();
    assert!(!text.contains("project"), "{text}");
}

// The name row must not be the reason `started by` is off the pane, for a
// public tunnel, which has the most rows
#[test]
fn the_name_row_does_not_push_started_by_out_of_the_details_pane() {
    let mut app = columns_app();
    app.selected = 1;
    let buffer = screen(&app, 160, 40);
    let text: String = (0..buffer.area.height)
        .flat_map(|y| row_chars(&buffer, y))
        .collect();
    assert!(text.contains("started by"), "{text}");
    assert!(text.contains("relay port"), "{text}");
}

// An unmanaged row has no account to match; a long unsuffixed name shares
// the column like any other and is cut only when the pane is too narrow
#[test]
fn an_unmanaged_row_shares_the_name_column() {
    let mut app = columns_app();
    app.entries[3].tunnel.name = "pid-2408312345678".to_string();
    let buffer = screen(&app, 160, 30);
    let (_, rows) = list_rows(&buffer, app.entries.len());
    let text: String = rows[4].iter().collect();
    assert!(text.contains("pid-2408312345678"), "{text}");
    let mode_x = find(&rows[0], "MODE").unwrap();
    assert_eq!(
        rows[4][mode_x..mode_x + 6].iter().collect::<String>(),
        "access"
    );
}

// Three access shapes reach three different kinds of thing, and an expose
// behind IAP is not reachable by the public at all. One word each, because
// the column is what a reader checks before trusting a URL
#[test]
fn the_mode_column_names_the_kind_of_tunnel_not_just_the_direction() {
    let mut app = columns_app();
    app.entries[0].tunnel.spec = TunnelSpec::Access {
        target: crate::state::AccessTarget::Jump {
            instance: "bastion".to_string(),
            zone: "europe-west1-b".to_string(),
            host: "10.0.0.10".to_string(),
        },
        remote_port: 443,
        local_port: 8443,
    };
    app.entries[2].tunnel.spec = TunnelSpec::Access {
        target: crate::state::AccessTarget::DestGroup {
            host: "10.0.0.5".to_string(),
            region: "europe-west1".to_string(),
            group: "prod".to_string(),
            network: "default".to_string(),
        },
        remote_port: 5432,
        local_port: 15002,
    };
    if let TunnelSpec::Expose { private, .. } = &mut app.entries[1].tunnel.spec {
        *private = true;
    }

    let buffer = screen(&app, 160, 30);
    let (_, rows) = list_rows(&buffer, app.entries.len());
    let mode_x = find(&rows[0], "MODE").unwrap();
    let cell = |row: usize| -> String {
        rows[row][mode_x..mode_x + 6]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_string()
    };
    assert_eq!(cell(1), "jump");
    assert_eq!(cell(2), "gated");
    assert_eq!(cell(3), "group");
    assert_eq!(cell(4), "access");
}

// The note that a process is not gtunnel's belongs where the URL would be:
// a process gtunnel does not manage has no URL of its own to show, and the
// note used to trail the row where a narrow pane clipped it mid-word
#[test]
fn an_unmanaged_row_says_so_in_the_url_column() {
    let mut app = columns_app();
    app.entries[3].kind = TunnelKind::External;
    let buffer = screen(&app, 160, 30);
    let (_, rows) = list_rows(&buffer, app.entries.len());
    let url_x = find(&rows[0], "URL").unwrap();
    let text: String = rows[4].iter().collect();
    assert!(
        rows[4][url_x..]
            .iter()
            .collect::<String>()
            .starts_with("[not managed by gtunnel]"),
        "{text}"
    );
    assert_eq!(text.matches("not managed by gtunnel").count(), 1, "{text}");
}

// The column fitter is arithmetic, so it is tested as arithmetic: sizes in,
// cells out, no terminal
#[test]
fn a_wide_pane_gives_the_longest_name_and_url_in_full() {
    let cols = ui::fit_columns(&[17, 8], &[23, 15], 120);
    assert_eq!(cols, ui::Columns { name: 17, url: 23 });
}

#[test]
fn a_pane_too_narrow_for_both_shrinks_the_name_first() {
    // 60 cells less 13 of lead, mode, air and flags leaves 47 to share
    let cols = ui::fit_columns(&[30], &[40], 60);
    // the name keeps all 30 and the url takes the 17 left, above its minimum
    assert_eq!(cols, ui::Columns { name: 30, url: 17 });
    // a longer name than the pane can hold is what makes it shrink
    let squeezed = ui::fit_columns(&[40], &[40], 60);
    assert_eq!(squeezed, ui::Columns { name: 35, url: 12 });
    // the url keeps its twelve cells while the name still has cells to lose
    let tight = ui::fit_columns(&[30], &[40], 13 + 8 + 12);
    assert_eq!(tight, ui::Columns { name: 8, url: 12 });
    // two cells above the floor, the name takes them and the url holds
    let loose = ui::fit_columns(&[30], &[40], 13 + 10 + 12);
    assert_eq!(loose, ui::Columns { name: 10, url: 12 });
}

#[test]
fn below_the_name_floor_and_url_minimum_the_url_gives_way() {
    let cols = ui::fit_columns(&[30], &[40], 13 + 8 + 5);
    assert_eq!(cols, ui::Columns { name: 8, url: 5 });
    let none = ui::fit_columns(&[30], &[40], 5);
    assert_eq!(none.url, 0);
}

#[test]
fn a_name_shorter_than_the_floor_is_not_padded_to_it() {
    let cols = ui::fit_columns(&[5], &[40], 13 + 20);
    assert_eq!(cols.name, 5);
}

// Entries `svc-00-...` to `svc-NN-...`, long enough that a narrow pane has to
// decide what to cut, and in a count no pane of ordinary height can hold
fn many_entries(count: usize) -> App {
    let mut app = App::new("dev");
    app.entries = (0..count)
        .map(|i| {
            TunnelEntry::new(
                access(&format!("svc-{i:02}-replica-reader-dev"), 15000 + i as u16),
                TunnelStatus::Running,
                TunnelKind::Managed,
            )
        })
        .collect();
    app
}

fn text_of(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| row_chars(buffer, y).iter().collect())
        .collect()
}

// A stateless List draws from its first row, so a selection past the pane's
// height moved on a list nobody could see
#[test]
fn the_selected_row_is_on_screen_when_the_list_is_longer_than_the_pane() {
    // only the list's own columns: Details prints the selected name as well
    let list_rows = |app: &App| -> Vec<String> {
        text_of(&screen(app, 130, 10))
            .iter()
            .map(|r| r.chars().take(54).collect())
            .collect()
    };
    let mut app = many_entries(20);
    app.selected = 19;
    let rows = list_rows(&app);
    assert!(rows.iter().any(|r| r.contains("svc-19-")), "{rows:#?}");
    assert!(!rows.iter().any(|r| r.contains("svc-00-")), "{rows:#?}");

    // and it scrolls back when the selection does
    app.selected = 0;
    let rows = list_rows(&app);
    assert!(rows.iter().any(|r| r.contains("svc-00-")), "{rows:#?}");
    assert!(!rows.iter().any(|r| r.contains("svc-19-")), "{rows:#?}");
}

// The names the list pane shows, top to bottom. Only its own columns are
// read: Details prints the selected name as well.
fn visible_list_names(app: &App, width: u16, height: u16) -> Vec<String> {
    let list_width = (width as usize * 46 / 100).max(1);
    text_of(&screen(app, width, height))
        .iter()
        .filter_map(|r| {
            let cells: String = r.chars().take(list_width).collect();
            let at = cells.find("svc-")?;
            Some(cells[at..].chars().take(6).collect())
        })
        .collect()
}

// Rebuilding the offset from the selection every draw pinned the selected
// row to the bottom edge whenever it was above it: moving up one row moved
// the whole list under the cursor instead of the cursor over the list.
#[test]
fn moving_the_selection_inside_the_window_does_not_scroll_the_list() {
    let mut app = many_entries(20);
    app.selected = 10;
    let first = visible_list_names(&app, 130, 14);
    assert_eq!(first.len(), 8, "the fixture is an 8-row pane: {first:?}");
    assert_eq!(
        first.last().map(String::as_str),
        Some("svc-10"),
        "{first:?}"
    );

    app.selected = 9;
    let second = visible_list_names(&app, 130, 14);
    assert_eq!(second, first, "the window moved for a selection inside it");

    // and it still follows the selection past either edge
    app.selected = 2;
    let up = visible_list_names(&app, 130, 14);
    assert_eq!(up.first().map(String::as_str), Some("svc-02"), "{up:?}");
}

// A window left scrolled by a short terminal must not leave blank rows
// under the last entry once the terminal grows
#[test]
fn a_taller_terminal_pulls_the_window_back_to_fill_the_pane() {
    let mut app = many_entries(20);
    app.selected = 19;
    let short = visible_list_names(&app, 130, 14);
    assert_eq!(
        short.last().map(String::as_str),
        Some("svc-19"),
        "{short:?}"
    );
    let tall = visible_list_names(&app, 130, 24);
    assert!(tall.len() > short.len(), "{tall:?}");
    assert_eq!(tall.last().map(String::as_str), Some("svc-19"), "{tall:?}");
}

#[test]
fn the_selected_row_keeps_its_highlight_after_the_list_scrolls() {
    let mut app = many_entries(20);
    app.selected = 12;
    let buffer = screen(&app, 120, 10);
    // the list is the left 46 percent; Details also prints the stored name
    let y = (0..buffer.area.height)
        .find(|y| find(&row_chars(&buffer, *y)[..54], "svc-12-").is_some())
        .expect("selected row on screen");
    let x = find(&row_chars(&buffer, y), "svc-12-").unwrap() as u16;
    assert_eq!(buffer[(x, y)].bg, theme::SELECTED_BG);
}

// Side by side the list got 32 cells at 80 columns, which cost it the url;
// stacked it gets the width of the terminal
#[test]
fn below_121_columns_the_list_stacks_above_the_other_panes() {
    let app = columns_app();
    let buffer = screen(&app, 80, 30);
    let rows = text_of(&buffer);
    let list_y = rows.iter().position(|r| r.contains("TUNNELS (")).unwrap();
    let details_y = rows.iter().position(|r| r.contains("DETAILS")).unwrap();
    assert!(details_y > list_y, "details must sit below the list");
    // the list's frame spans the terminal, and no second pane shares its row
    assert_eq!(buffer[(0, list_y as u16 - 1)].symbol(), "\u{256d}");
    assert_eq!(buffer[(79, list_y as u16 - 1)].symbol(), "\u{256e}");
    let (_, list) = list_rows(&buffer, app.entries.len());
    let second: String = list[2].iter().collect();
    assert!(second.contains("https://k8s.example.com"), "{second}");
    assert!(!second.contains('\u{2026}'), "{second}");
}

// Measured at 24 rows: side by side, the list's 46 percent was 46 cells at
// 100 columns and 55 at 120. This carries the two urls and the unmanaged
// note the stacked layout has to show whole.
fn wide_url_app() -> App {
    let mut app = App::new("dev");
    app.entries = vec![
        TunnelEntry::new(
            expose("api", "api.demo.example.com"),
            TunnelStatus::Running,
            TunnelKind::Managed,
        ),
        TunnelEntry::new(
            expose("callback", "checkout-callback.sandbox.internal.ex.com"),
            TunnelStatus::Running,
            TunnelKind::Managed,
        ),
        TunnelEntry::new(
            access("pid-24083", 0),
            TunnelStatus::Running,
            TunnelKind::External,
        ),
    ];
    app.entries[2].tunnel.account_name = String::new();
    app
}

#[test]
fn from_100_to_120_columns_the_list_shows_every_url_uncut() {
    let app = wide_url_app();
    assert_eq!(
        "https://api.demo.example.com".chars().count(),
        28,
        "the short url is 28 cells"
    );
    assert_eq!(
        "https://checkout-callback.sandbox.internal.ex.com"
            .chars()
            .count(),
        49,
        "the long url is 49 cells"
    );
    for width in [100, 110, 120] {
        let buffer = screen(&app, width, 24);
        let rows = text_of(&buffer);
        let list_y = rows.iter().position(|r| r.contains("TUNNELS (")).unwrap();
        let details_y = rows.iter().position(|r| r.contains("DETAILS")).unwrap();
        assert!(details_y > list_y, "{width}: details sits below the list");
        let text = rows.join("\n");
        for wanted in [
            "https://api.demo.example.com",
            "https://checkout-callback.sandbox.internal.ex.com",
            "[not managed by gtunnel]",
        ] {
            assert!(text.contains(wanted), "{width}: missing {wanted:?}\n{text}");
        }
        assert!(
            !rows[list_y..list_y + 6].join("").contains('\u{2026}'),
            "{width}: the list carries an ellipsis\n{text}"
        );
    }
}

#[test]
fn from_121_columns_the_panes_are_side_by_side() {
    let app = wide_url_app();
    let buffer = screen(&app, 121, 24);
    let rows = text_of(&buffer);
    let list_y = rows.iter().position(|r| r.contains("TUNNELS (")).unwrap();
    let details_y = rows.iter().position(|r| r.contains("DETAILS")).unwrap();
    assert_eq!(details_y, list_y, "the panes share a row");
    // the first frame on the top edge closes at the 46 percent split, the
    // same split as at 120 columns, not at the terminal's edge
    let top = row_chars(&buffer, list_y as u16 - 1);
    let right_edge = top.iter().position(|c| *c == '\u{256e}').unwrap();
    assert!((53..=56).contains(&right_edge), "{right_edge}");
    assert_eq!(top[right_edge + 1], '\u{256d}', "a second pane follows");
}

#[test]
fn at_100x24_the_stacked_list_keeps_rows_and_the_panes_stay_usable() {
    let mut app = with_logs(wide_url_app(), 40);
    app.selected = 1;
    let rows = text_of(&screen(&app, 100, 24));
    let text = rows.join("\n");
    let list_y = rows.iter().position(|r| r.contains("TUNNELS (")).unwrap();
    let details_y = rows.iter().position(|r| r.contains("DETAILS")).unwrap();
    assert!(
        details_y - list_y >= 5,
        "the list shows its header and two rows"
    );
    for wanted in ["DETAILS", "LOGS: ", "the newest log line"] {
        assert!(text.contains(wanted), "missing {wanted:?}\n{text}");
    }
}

#[test]
fn the_selection_survives_the_change_of_layout_at_121_columns() {
    let mut app = many_entries(20);
    app.selected = 17;
    // The highlight is the proof it is the list's row, not Details printing
    // the same name
    for width in [99, 120, 121, 120, 60] {
        let buffer = screen(&app, width, 30);
        let highlighted = (0..buffer.area.height).any(|y| {
            let row: String = row_chars(&buffer, y).iter().collect();
            row.contains("svc-17-") && buffer[(2, y)].bg == theme::SELECTED_BG
        });
        assert!(highlighted, "width {width}: {:#?}", text_of(&buffer));
    }
}

// The height budget is arithmetic too: what each pane wants in, what each
// gets out, and the order they give way in
#[test]
fn with_height_to_spare_every_pane_gets_what_it_wants() {
    let (details, logs, metrics) = ui::right_heights(60, 8, 7);
    assert_eq!((details, metrics), (8, 7));
    assert_eq!(logs, 60 - 8 - 7, "logs takes what the others leave");
}

// 17 is the right column at 100x24, where Logs used to vanish
#[test]
fn at_seventeen_rows_logs_keeps_its_minimum_and_metrics_shrinks() {
    let (details, logs, metrics) = ui::right_heights(17, 8, 7);
    assert_eq!((details, logs, metrics), (8, 4, 5));
    let (details, logs, metrics) = ui::right_heights(17, 10, 9);
    assert_eq!(details, 10, "details is whole while logs is at its minimum");
    assert!(logs >= 4, "{logs}");
    assert_eq!(details + logs + metrics, 17);
}

#[test]
fn metrics_hides_before_logs_goes_under_its_minimum() {
    // 8 + 4 leaves 3, too few for a pane that shows a figure
    let (details, logs, metrics) = ui::right_heights(15, 8, 7);
    assert_eq!((details, logs, metrics), (8, 7, 0));
    let (details, logs, metrics) = ui::right_heights(14, 10, 9);
    assert_eq!((details, logs, metrics), (10, 4, 0));
}

#[test]
fn details_is_cut_only_when_logs_has_nothing_left_to_give() {
    let (details, logs, metrics) = ui::right_heights(11, 10, 9);
    assert_eq!((details, logs, metrics), (7, 4, 0));
    // a column shorter than the minimum goes to logs whole
    assert_eq!(ui::right_heights(3, 10, 9), (0, 3, 0));
}

#[test]
fn the_budget_always_adds_up_to_the_column() {
    for avail in 0..80 {
        for (d, m) in [(0, 0), (5, 7), (8, 7), (10, 9), (14, 10)] {
            let (details, logs, metrics) = ui::right_heights(avail, d, m);
            assert_eq!(details + logs + metrics, avail, "{avail} {d} {m}");
            assert!(details <= d && metrics <= m);
        }
    }
}

fn row_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[test]
fn a_detail_that_fits_is_one_row() {
    let rows = ui::wrap_detail("url", "localhost:15432", 40);
    assert_eq!(rows.len(), 1);
    assert_eq!(row_text(&rows[0]), format!("{:<11}localhost:15432", "url"));
}

#[test]
fn a_long_detail_wraps_under_the_value_column() {
    let value = "launchd, after 2 restarts (auto-start off)";
    let rows = ui::wrap_detail("started by", value, 30);
    assert!(rows.len() >= 2, "{rows:?}");
    for (i, row) in rows.iter().enumerate() {
        let text = row_text(row);
        assert!(text.chars().count() <= 30, "{text:?}");
        if i > 0 {
            assert_eq!(&text[..11], " ".repeat(11), "{text:?} overwrote the label");
        }
    }
    // nothing is lost, and words are not split while they fit
    let rejoined: Vec<String> = rows.iter().map(|r| row_text(r)[11..].to_string()).collect();
    assert_eq!(rejoined.join(" "), value);
}

#[test]
fn a_word_longer_than_the_value_column_breaks_at_the_column() {
    let url = "https://a-very-long-hostname.example.com";
    let rows = ui::wrap_detail("url", url, 26);
    let value: String = rows.iter().map(|r| row_text(r)[11..].to_string()).collect();
    assert_eq!(value.replace(' ', ""), url);
    assert!(rows.iter().all(|r| row_text(r).chars().count() <= 26));
}

#[test]
fn a_pane_too_narrow_for_a_value_gets_one_cut_row() {
    let rows = ui::wrap_detail("url", "localhost:15432", 14);
    assert_eq!(rows.len(), 1);
    assert!(row_text(&rows[0]).chars().count() <= 14);
}

fn with_logs(mut app: App, lines: usize) -> App {
    app.logs = (1..=lines).map(|i| format!("log line {i}")).collect();
    app.logs.push("the newest log line".to_string());
    app
}

// Details and Metrics took fixed heights first, so Logs, the pane that says
// why a tunnel is failing, was what a 24-row terminal could not show
#[test]
fn at_100x24_the_log_title_and_newest_line_are_on_screen_for_both_kinds() {
    for selected in [0, 1] {
        let mut app = with_logs(app_with_two(), 40);
        app.selected = selected;
        let text = text_of(&screen(&app, 100, 24)).join("\n");
        assert!(text.contains("LOGS: "), "selected {selected}\n{text}");
        assert!(
            text.contains("the newest log line"),
            "selected {selected}\n{text}"
        );
    }
}

#[test]
fn at_60_columns_no_details_or_metrics_row_is_cut_mid_word() {
    let mut app = app_with_two();
    app.selected = 1;
    app.entries[1].tunnel.name = "payments-callback-sandbox-dev".to_string();
    if let TunnelSpec::Expose { hostname, .. } = &mut app.entries[1].tunnel.spec {
        *hostname = "payments-callback.sandbox.internal".to_string();
    }
    app.entries[1].metrics = Some(crate::probe::TunnelMetrics {
        latency: Some(std::time::Duration::from_millis(123)),
        active_connections: 12,
        uptime: Some(std::time::Duration::from_secs(90_000)),
        restarts: 2,
        ..Default::default()
    });
    app.entries[1].history.record(123_000);
    // 44 is where even the url and the four figures stop fitting a row
    for width in [60, 44] {
        let text = text_of(&screen(&app, width, 60)).join("\n");
        // the whole stored name and url are readable, wrapped
        for wanted in [
            "payments-callback-sandbox-dev",
            "after 2 restarts",
            "peak",
            "uptime",
        ] {
            assert!(text.contains(wanted), "{width}: {wanted}\n{text}");
        }
        let flat = text.replace(['\u{2502}', '\n'], "").replace(' ', "");
        assert!(
            flat.contains("https://payments-callback.sandbox.internal"),
            "{width}\n{text}"
        );
    }
}

// Dividing characters by width assumes words can be split anywhere. The
// widget moves a word that does not fit to the next row, so a line of
// six-letter words in a ten-cell pane takes a row per word, not 27 / 10
#[test]
fn a_line_of_short_words_counts_the_rows_the_widget_wraps_it_to() {
    use ratatui::text::Line;
    let line = Line::raw("aaaaaa bbbbbb cccccc dddddd");
    assert_eq!(ui::wrapped_rows(&[line], 10), 4);
    // a word longer than the pane is broken, as the widget breaks it
    assert_eq!(ui::wrapped_rows(&[Line::raw("abcdefghijklmno")], 10), 2);
    // leading air, as the hint rows carry, is kept and counts
    assert_eq!(ui::wrapped_rows(&[Line::raw("  aaaaaa bbbbbb")], 10), 2);
}

#[test]
fn the_newest_log_line_is_on_screen_when_the_lines_above_it_are_short_words() {
    let mut app = App::new_demo();
    // twenty-one cells a word: two never share a row of a 40-cell pane, so
    // each line takes ten rows where characters over width says six
    let long_words = ["abcdefghijklmnopqrstu"; 10].join(" ");
    app.logs = (0..4).map(|_| long_words.clone()).collect();
    app.logs.push("zzzzzz yyyyyy xxxxxx wwwwww".to_string());
    for (width, height) in [(100, 30), (60, 40), (44, 60)] {
        let text = text_of(&screen(&app, width, height)).join("\n");
        assert!(text.contains("wwwwww"), "{width}x{height}\n{text}");
    }
}

// The count is only worth having if it is the widget's own, so this renders
// the same lines through the real `Paragraph` and counts the rows it drew
#[test]
fn the_wrapped_row_count_agrees_with_the_widget_it_scrolls() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::text::Line;
    use ratatui::widgets::{Paragraph, Widget, Wrap};
    let texts = [
        "alpha bravo delta gamma sigma omega kappa theta",
        "abcdefghijklmnopqrstu abcdefghijklmnopqrstu abcdefghijklmnopqrstu",
        "  hint: a short hint that wraps",
        "a b c d e f g h i j k l m n o p q r s t",
        "https://payments-callback.sandbox.internal/path/to/a/resource?x=1",
        "one two  three   four",
        "ab    cd efgh",
        "  https://payments-callback.sandbox.internal/path",
        "abc  defg hi   jklmnopq",
        "x",
    ];
    for text in texts {
        for width in [3u16, 5, 8, 10, 13, 20, 33, 40, 80] {
            let line = Line::raw(text);
            let area = Rect::new(0, 0, width, 60);
            let mut buffer = Buffer::empty(area);
            Paragraph::new(vec![line.clone()])
                .wrap(Wrap { trim: false })
                .render(area, &mut buffer);
            let drawn = (0..60)
                .rev()
                .find(|y| (0..width).any(|x| buffer[(x, *y)].symbol() != " "))
                .map_or(1, |y| y + 1);
            assert_eq!(
                ui::wrapped_rows(&[line], width),
                drawn,
                "{text:?} at {width}"
            );
        }
    }
}

// One pass over the sizes a terminal really takes, and the two it should
// never be reduced to a panic at. A buffer is exactly as wide as the
// terminal, so "no row wider" holds by construction there; what it can show
// is a draw that panics on a size, or a selection the list lost.
#[test]
fn every_size_draws_without_panic_and_keeps_the_selection_in_view() {
    let sizes = [
        (1, 1),
        (10, 4),
        (20, 8),
        (40, 12),
        (60, 20),
        (80, 24),
        (100, 24),
        (120, 40),
        (200, 50),
    ];
    for (width, height) in sizes {
        for selected in [0, 13, 19] {
            let mut app = with_logs(many_entries(20), 30);
            app.selected = selected;
            app.entries[selected].tunnel.name = format!("svc-{selected:02}-replica-reader-dev");
            let buffer = screen(&app, width, height);
            for y in 0..height {
                assert_eq!(row_chars(&buffer, y).len(), width as usize);
            }
            if height >= 12 {
                let on_screen = (0..height).any(|y| {
                    let row: String = row_chars(&buffer, y).iter().collect();
                    row.contains(&format!("svc-{selected:02}-"))
                        && buffer[(2, y)].bg == theme::SELECTED_BG
                });
                assert!(
                    on_screen,
                    "{width}x{height} selected {selected}\n{:#?}",
                    text_of(&buffer)
                );
            }
        }
    }
}

// Public tunnels carry the most Details rows and an extra Metrics pane
#[test]
fn a_public_tunnel_draws_at_every_size_too() {
    for (width, height) in [(1, 1), (10, 4), (20, 8), (44, 16), (100, 24), (200, 50)] {
        let mut app = with_logs(app_with_two(), 30);
        app.selected = 1;
        let _ = screen(&app, width, height);
    }
}

fn app_with_many_instances(selected: usize) -> App {
    let mut app = app_with_two();
    app.instances = (0..36)
        .map(|i| InstanceChoice {
            name: format!("instance-{i:02}"),
            zone: "europe-west1-b".to_string(),
        })
        .collect();
    app.input_mode = InputMode::AddInstance;
    app.instance_selected = selected;
    app
}

#[test]
fn the_instance_picker_scrolls_to_keep_the_selection_visible() {
    let rendered = render_to_string(&app_with_many_instances(30), 120, 40);
    assert!(
        rendered.contains("instance-30"),
        "the selected instance is off screen"
    );
    assert!(rendered.contains("31/36"), "no position counter");
}

#[test]
fn the_instance_picker_starts_at_the_top() {
    let rendered = render_to_string(&app_with_many_instances(0), 120, 40);
    assert!(
        rendered.contains("instance-00"),
        "the list is scrolled past"
    );
    assert!(
        !rendered.contains("instance-35"),
        "the last row cannot fit with the first"
    );
    assert!(rendered.contains("1/36"), "no position counter");
}

#[test]
fn the_instance_picker_draws_the_error_set_for_it() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "cache\n");
    app.instances_failed("gcloud exited 1");
    let buffer = screen(&app, 120, 40);
    let text: String = (0..buffer.area.height)
        .map(|y| row_chars(&buffer, y).into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    // The dialog wraps the line, so only a fragment that fits one row is stable
    assert!(text.contains("Could not list instances"), "{text}");
}

#[test]
fn the_refusal_is_on_screen_at_80_by_24() {
    let mut app = at_the_name_prompt();
    keys(&mut app, "db\n");
    let rendered = render_to_string(&app, 80, 24);
    assert!(
        rendered.contains("already used in this account"),
        "the reason is not on screen"
    );
}

#[test]
fn the_frame_after_s_shows_the_row_starting() {
    let mut app = a_stopped_app();
    let op = app.handle_key(key(KeyCode::Char('s')));
    assert_eq!(op, Some(Op::Start("db".to_string())));
    let text = render_to_string(&app, 120, 30);
    assert!(text.contains("starting"), "{text}");
}

#[test]
fn a_create_that_succeeded_leaves_no_dialog_behind() {
    let mut app = submitted_access_draft();
    app.apply(OpResult {
        message: "cache created".to_string(),
        ok: true,
        reload: true,
        from_create: true,
        from_account: false,
        name: None,
    });
    assert_eq!(app.input_mode, InputMode::Normal);
    app.apply(failed("some later failure"));
    assert_eq!(app.input_mode, InputMode::Normal);
}

#[test]
fn escape_on_a_reopened_dialog_discards_the_draft_for_good() {
    let mut app = submitted_access_draft();
    app.apply(failed("instance not found"));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.draft_error.is_none());
    app.apply(failed("again"));
    assert_eq!(app.input_mode, InputMode::Normal);
}

fn submitted_public_draft() -> App {
    let mut app = app_with_two();
    app.has_relay = true;
    app.domains = vec!["svc.example.test".to_string()];
    keys(&mut app, "a2site\nlocalhost:3000\n\n");
    assert_eq!(app.input_mode, InputMode::Normal);
    app
}

#[test]
fn a_public_create_that_failed_reopens_the_domain_picker_with_the_reason() {
    let mut app = submitted_public_draft();
    app.apply(failed("relay unreachable"));
    assert_eq!(app.input_mode, InputMode::AddDomain);
    assert_eq!(app.draft.name, "site");
    assert_eq!(app.draft.local_target, "localhost:3000");
    assert_eq!(app.input, "");
    let rendered = render_to_string(&app, 80, 24);
    assert!(rendered.contains("relay unreachable"), "{rendered}");
}

// The chart's samples and the latency reading are one measurement in one
// unit. The demo seeded milliseconds into a history the live path fills in
// microseconds, so a 21 ms peak was labelled `0.0 ms` under a chart of bars
// Adding a tunnel to a private endpoint has to be possible from the TUI,
// not only from the CLI: the dialog asks what the chosen instance reaches,
// and an empty answer means the instance itself
#[test]
fn the_add_dialog_asks_what_the_instance_reaches() {
    let mut app = App::new_demo();
    app.draft.instance = "bastion".to_string();
    app.draft.zone = "europe-west1-b".to_string();
    app.input_mode = InputMode::AddJumpHost;
    app.input = "10.0.0.10".to_string();
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(app.draft.host, "10.0.0.10");
    assert_eq!(app.input_mode, InputMode::AddRemotePort);
}

fn tunnels_on_disk(names: &[(&str, &str)]) -> crate::config::testing::TempConfigDir {
    let dir = crate::config::testing::TempConfigDir::new();
    let tunnels = names
        .iter()
        .enumerate()
        .map(|(i, (name, account))| PersistentTunnel {
            account_name: account.to_string(),
            ..access(name, 15500 + i as u16)
        })
        .collect();
    crate::state::TunnelState { tunnels }.save().expect("saved");
    dir
}

// `e` used to open the target prompt and nothing else, so renaming a
// tunnel or moving it between accounts was a CLI-only act. The three are
// one chooser rather than three top-level keys, as `a` already is.
#[test]
fn e_offers_target_rename_and_move() {
    let mut app = with_a_second_account();
    keys(&mut app, "e");
    assert_eq!(app.input_mode, InputMode::ChooseEdit);
    let text = dialog_text(&screen(&app, 80, 24));
    for phrase in ["Target and ports", "Rename", "Move"] {
        assert!(text.contains(phrase), "{phrase:?} is not offered: {text}");
    }

    keys(&mut app, "1");
    assert_eq!(app.input_mode, InputMode::EditTarget);
    assert_eq!(app.input, "5432:15432", "the ports are pre-filled");

    let mut app = with_a_second_account();
    keys(&mut app, "e2");
    assert_eq!(app.input_mode, InputMode::RenameTo);
    assert_eq!(app.input, "db", "the name is pre-filled to be edited");

    let mut app = with_a_second_account();
    keys(&mut app, "e3");
    assert_eq!(app.input_mode, InputMode::MoveTo);
}

// Decision 2: the destination already holding the name is a question, not
// a refusal, so the dialog asks it rather than throwing the move away
#[test]
fn moving_onto_a_taken_name_offers_a_rename_rather_than_cancelling() {
    let _dir = tunnels_on_disk(&[("db", "dev"), ("db", "prod")]);
    let mut app = with_a_second_account();
    keys(&mut app, "e3");
    assert_eq!(app.input_mode, InputMode::MoveTo);
    assert_eq!(keys(&mut app, "\n"), None, "prod already holds a db");
    assert_eq!(app.input_mode, InputMode::MoveRename, "the move goes on");
    let said = app.draft_error.clone().expect("the reason is on screen");
    assert!(said.contains("prod"), "{said}");
    let text = dialog_text(&screen(&app, 80, 24));
    assert!(text.contains("prod"), "{text}");

    for _ in 0..app.input.chars().count() {
        app.handle_key(key(KeyCode::Backspace));
    }
    assert_eq!(
        keys(&mut app, "postgres\n"),
        Some(Op::Move(MoveTunnel {
            name: "db".to_string(),
            from: "dev".to_string(),
            to: "prod".to_string(),
            rename_to: Some("postgres".to_string()),
        }))
    );
    assert_eq!(app.input_mode, InputMode::Normal);
}

// Every dialog and picker with the longest text it carries, so a size that
// clips one shows. Each phrase is what a reader needs to see whole.
fn dialog_cases() -> Vec<(&'static str, App, Vec<&'static str>)> {
    let base = || {
        let mut app = app_with_two();
        app.instances = (0..12)
            .map(|i| InstanceChoice {
                name: format!("sample-instance-{i:02}"),
                zone: "europe-west1-b".to_string(),
            })
            .collect();
        app.domains = (0..6).map(|i| format!("svc{i}.example.test")).collect();
        app
    };
    let prompt = |mode: InputMode, input: &str, words: Vec<&'static str>| {
        let mut app = base();
        app.input_mode = mode;
        app.input = input.to_string();
        (app, words)
    };
    let mut cases = Vec::new();
    let mut add = |name, (app, words): (App, Vec<&'static str>)| cases.push((name, app, words));

    let mut app = base();
    app.input_mode = InputMode::ChooseMode;
    add(
        "choose",
        (
            app,
            vec![
                "Access - forward a remote port to localhost over IAP",
                "Public - publish a local service at an HTTPS URL",
                "Esc to cancel",
            ],
        ),
    );
    let hint = "Enter to continue, Esc to cancel";
    add(
        "name",
        prompt(
            InputMode::AddName,
            "sample-name",
            vec!["Name:", "sample-name_", hint],
        ),
    );
    add(
        "host",
        prompt(
            InputMode::AddJumpHost,
            "10.0.0.10",
            vec![
                "Host it reaches (Enter for the instance itself):",
                "10.0.0.10_",
                hint,
            ],
        ),
    );
    add(
        "local port",
        prompt(
            InputMode::AddLocalPort,
            "15000",
            vec!["Local port (Enter keeps the suggestion):", "15000_", hint],
        ),
    );
    add(
        "target",
        prompt(
            InputMode::AddLocalTarget,
            "localhost:3000",
            vec![
                "Local target (e.g. localhost:3000):",
                "localhost:3000_",
                hint,
            ],
        ),
    );
    add(
        "edit",
        prompt(
            InputMode::EditTarget,
            "10.0.0.10:443",
            vec!["New value:", "10.0.0.10:443_", hint],
        ),
    );
    let mut app = base();
    app.input_mode = InputMode::ChooseEdit;
    add(
        "edit choice",
        (
            app,
            vec![
                "Target and ports - what it forwards, and where to",
                "Rename - the same account, another name",
                "Move - hand it to another account",
                "Esc to cancel",
            ],
        ),
    );
    add(
        "rename",
        prompt(
            InputMode::RenameTo,
            "postgres",
            vec!["New name:", "postgres_", hint],
        ),
    );
    let mut app = base();
    app.accounts = vec!["dev".to_string(), "prod".to_string()];
    app.input_mode = InputMode::MoveTo;
    add(
        "move account",
        (app, vec!["Move it to which account:", "prod"]),
    );
    add(
        "move rename",
        prompt(
            InputMode::MoveRename,
            "postgres",
            vec!["New name on the destination account:", "postgres_", hint],
        ),
    );
    let mut app = base();
    app.input_mode = InputMode::Confirm;
    app.confirm_message = Some("Delete sample-platform-redis-session? (y/n)".to_string());
    add(
        "confirm",
        (app, vec!["Delete sample-platform-redis-session? (y/n)"]),
    );
    let mut app = base();
    app.input_mode = InputMode::AddInstance;
    app.instance_selected = 5;
    add(
        "instances",
        (
            app,
            vec!["Instance (type to filter):", "6/12", "sample-instance-05"],
        ),
    );
    let mut app = base();
    app.input_mode = InputMode::AddDomain;
    app.domain_selected = 3;
    add(
        "domains",
        (app, vec!["Domain:", "4/6", "svc3.example.test"]),
    );
    cases
}

// Where the dialog's frame is on the screen, found by its corners: the panes
// behind it use rounded ones. None when any corner is missing, which is how a
// dialog clipped by the terminal's edge shows.
fn dialog_box(buffer: &ratatui::buffer::Buffer) -> Option<(u16, u16, u16, u16)> {
    let at = |x: u16, y: u16| buffer[(x, y)].symbol().to_string();
    let (w, h) = (buffer.area.width, buffer.area.height);
    let (x0, y0) = (0..h).find_map(|y| (0..w).find(|x| at(*x, y) == "\u{250c}").map(|x| (x, y)))?;
    let x1 = (x0 + 1..w).find(|x| at(*x, y0) == "\u{2510}")?;
    let y1 = (y0 + 1..h).find(|y| at(x0, *y) == "\u{2514}")?;
    (at(x1, y1) == "\u{2518}").then_some((x0, y0, x1, y1))
}

// What the dialog says, rows joined and runs of blanks collapsed, so a phrase
// that wraps still reads as the phrase
fn dialog_text(buffer: &ratatui::buffer::Buffer) -> String {
    let (x0, y0, x1, y1) = dialog_box(buffer).expect("a dialog on screen");
    let rows: Vec<String> = (y0 + 1..y1)
        .map(|y| {
            (x0 + 1..x1)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect();
    rows.join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn dialog_case(name: &str) -> (App, Vec<&'static str>) {
    let (_, app, words) = dialog_cases()
        .into_iter()
        .find(|(n, _, _)| *n == name)
        .unwrap_or_else(|| panic!("no dialog case {name}"));
    (app, words)
}

// The cancel hint was the row the dialog's height cut: a quarter of 24 rows
// is six, a frame and four rows of text, for a dialog that needs five. The
// one line that says how to get out of a prompt was off screen at 80x24.
#[test]
fn a_prompt_shows_how_to_cancel_at_the_sizes_a_terminal_starts_at() {
    for (name, width, height) in [("host", 80, 24), ("name", 60, 24), ("edit", 80, 24)] {
        let (app, _) = dialog_case(name);
        let text = dialog_text(&screen(&app, width, height));
        assert!(
            text.contains("Esc to cancel"),
            "{name} {width}x{height}: {text}"
        );
    }
}

// `Host it reaches (Enter for the instance itself):` is 48 cells and the
// dialog was 60 percent of the width, so the end of the question, the part
// that says what Enter does, was cut at every width under 83.
#[test]
fn a_long_prompt_wraps_inside_the_dialog_instead_of_being_clipped() {
    for (width, height) in [(40, 12), (48, 20), (60, 24)] {
        let (app, _) = dialog_case("host");
        let text = dialog_text(&screen(&app, width, height));
        assert!(
            text.contains("Host it reaches (Enter for the instance itself):"),
            "{width}x{height}: {text}"
        );
    }
}

// The cursor sits at the end of what was typed; a value longer than the
// dialog used to push it out of sight and leave the head on screen
#[test]
fn a_long_value_keeps_the_end_of_it_and_the_cursor_on_screen() {
    let (mut app, _) = dialog_case("name");
    app.input = format!("{}-tail", "x".repeat(60));
    for (width, height) in [(40, 12), (60, 24), (120, 40)] {
        let text = dialog_text(&screen(&app, width, height));
        assert!(text.contains("-tail_"), "{width}x{height}: {text}");
    }
}

// Two options of fifty cells each, in a dialog that was 60 percent wide
#[test]
fn the_mode_dialog_wraps_its_options_and_keeps_its_cancel_row() {
    for (width, height) in [(40, 12), (48, 20), (60, 24)] {
        let (app, words) = dialog_case("choose");
        let text = dialog_text(&screen(&app, width, height));
        for phrase in words {
            assert!(
                text.contains(phrase),
                "{width}x{height}: {phrase:?} in {text}"
            );
        }
    }
}

// Half of 48 columns is 24, and the question is 43 cells with its answer keys
#[test]
fn the_delete_question_is_asked_whole_at_the_widths_it_was_clipped() {
    for (width, height) in [(40, 12), (48, 20)] {
        let (app, words) = dialog_case("confirm");
        let text = dialog_text(&screen(&app, width, height));
        assert!(text.contains(words[0]), "{width}x{height}: {text}");
    }
}

// The prompt line carries the position counter, and a prompt wider than the
// dialog pushed the counter off the end; at 20x8 the header took the whole
// frame and not one row of the list was left, so the selection was invisible
#[test]
fn a_picker_keeps_its_counter_and_always_shows_the_selected_row() {
    let (app, words) = dialog_case("instances");
    let text = dialog_text(&screen(&app, 40, 12));
    for phrase in &words {
        assert!(text.contains(phrase), "40x12: {phrase:?} in {text}");
    }
    for name in ["instances", "domains"] {
        let (app, _) = dialog_case(name);
        for (width, height) in [(20, 8), (10, 4)] {
            let buffer = screen(&app, width, height);
            let (x0, y0, x1, y1) =
                dialog_box(&buffer).unwrap_or_else(|| panic!("{name} {width}x{height}: no frame"));
            let selected = (y0..=y1).any(|y| {
                (x0..=x1).any(|x| {
                    buffer[(x, y)].fg == theme::ACCENT
                        && buffer[(x, y)]
                            .modifier
                            .contains(ratatui::style::Modifier::BOLD)
                        && y > y0
                        && y < y1
                        && x > x0
                        && x < x1
                })
            });
            assert!(
                selected,
                "{name} {width}x{height}: no selected row in the frame"
            );
        }
    }
}

// Whatever the terminal, a dialog is a box inside it with a row to draw in;
// `TestBackend` clips what overflows, so this reads the rectangle itself
#[test]
fn a_dialog_is_never_laid_out_outside_the_terminal() {
    for width in 1..=70u16 {
        for height in 1..=40u16 {
            let screen = ratatui::layout::Rect::new(0, 0, width, height);
            for (want_w, want_h) in [(0, 0), (30, 5), (200, 100), (u16::MAX, u16::MAX)] {
                let r = ui::dialog_rect(screen, want_w, want_h);
                assert!(
                    r.x + r.width <= width && r.y + r.height <= height,
                    "{width}x{height} want {want_w}x{want_h}: {r:?}"
                );
                assert!(
                    r.width <= want_w.max(width.min(6)),
                    "{width}x{height}: {r:?}"
                );
                assert!(r.height >= height.min(3), "{width}x{height}: {r:?}");
            }
        }
    }
}

// The matrix: every dialog and picker at every size draws without panicking,
// keeps its whole frame on the screen from 10x4 up, and from 40x12 up says
// everything it has to say
#[test]
fn every_dialog_and_picker_fits_every_terminal_size() {
    let sizes = [
        (1, 1),
        (10, 4),
        (20, 8),
        (40, 12),
        (48, 20),
        (60, 24),
        (80, 24),
        (120, 40),
    ];
    for (name, app, words) in dialog_cases() {
        for (width, height) in sizes {
            let buffer = screen(&app, width, height);
            if width < 10 || height < 4 {
                continue;
            }
            let (x0, y0, x1, y1) = dialog_box(&buffer)
                .unwrap_or_else(|| panic!("{name} {width}x{height}: the frame is clipped"));
            assert!(x1 < width && y1 < height && x0 <= x1 && y0 <= y1);
            if width >= 40 && height >= 12 {
                let text = dialog_text(&buffer);
                for phrase in &words {
                    assert!(
                        text.contains(phrase),
                        "{name} {width}x{height}: {phrase:?} in {text}"
                    );
                }
            }
        }
    }
}

/// Every colour the theme exports has a documented job. A colour the doc
/// does not name is a colour with no stated meaning, which is how a palette
/// turns into decoration.
#[test]
fn the_design_doc_names_every_colour_the_theme_exports() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = std::fs::read_to_string(root.join("docs/design.md")).unwrap();
    let theme = std::fs::read_to_string(root.join("src/tui/theme.rs")).unwrap();
    let mut named = 0;
    for line in theme.lines() {
        let Some(rest) = line.strip_prefix("pub const ") else {
            continue;
        };
        let name = rest.split(':').next().unwrap().trim();
        assert!(
            doc.contains(name),
            "docs/design.md never mentions theme::{name}"
        );
        named += 1;
    }
    assert!(named >= 13, "theme.rs exports its colours, found {named}");
}

// The row used to say "unhealthy" for a log that said "gcloud auth login"
// and for a service that restarts every few seconds. Both now say so, in
// Details, with the one thing to do about it.
#[test]
fn details_name_expired_credentials_and_the_command_that_fixes_them() {
    let mut app = app_with_two();
    app.entries[0].metrics = Some(crate::probe::TunnelMetrics::default());
    app.entries[0].health = HealthStatus::AuthExpired;
    let text = text_of(&screen(&app, 160, 40)).join("\n");
    assert!(text.contains("auth expired"), "{text}");
    assert!(text.contains("gcloud auth login"), "{text}");
    assert!(text.contains(HealthStatus::AuthExpired.symbol()), "{text}");
}

// A run's stop time, in its tag and in the details pane
mod run_stop_time {
    use super::super::*;
    use crate::state::{AccessTarget, PersistentTunnel};

    fn run_entry(expires_at: Option<u64>) -> TunnelEntry {
        let mut e = TunnelEntry::new(
            PersistentTunnel {
                name: "gt-3b2c10".to_string(),
                account_name: "demo".to_string(),
                auto_start: false,
                spec: TunnelSpec::Access {
                    target: AccessTarget::Instance {
                        instance: "my-vm".to_string(),
                        zone: "europe-west1-b".to_string(),
                    },
                    remote_port: 5432,
                    local_port: 15900,
                },
            },
            TunnelStatus::Running,
            TunnelKind::Ephemeral,
        );
        e.expires_at = expires_at;
        e
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn a_run_with_a_ttl_says_when_it_stops_in_its_tag() {
        let tag = run_entry(Some(1_700_000_000)).tag().unwrap();
        let clock = crate::commands::local_hhmm(1_700_000_000).unwrap();
        assert_eq!(tag, format!("[run, stops at {clock}]"));
        assert_eq!(tag.chars().count(), "[run, stops at 00:00]".chars().count());
    }

    #[test]
    fn a_run_without_a_ttl_keeps_the_ephemeral_tag() {
        assert_eq!(run_entry(None).tag().unwrap(), "[ephemeral]");
    }

    #[test]
    fn the_detail_pane_counts_down_to_a_runs_stop_time() {
        let now = 1_700_000_000;
        let lines = text(&detail_lines_at(
            &run_entry(Some(now + 23 * 60)),
            "",
            60,
            now,
        ));
        let clock = crate::commands::local_hhmm(now + 23 * 60).unwrap();
        let row = lines.iter().find(|l| l.starts_with("stops at")).unwrap();
        assert_eq!(row, &format!("{:<11}{clock} (in 23m)", "stops at"));
    }

    #[test]
    fn the_countdown_rounds_up_and_never_goes_negative() {
        let now = 1_700_000_000;
        assert!(stops_at(now + 61, now).ends_with("(in 2m)"));
        assert!(stops_at(now + 5, now).ends_with("(in 1m)"));
        assert!(stops_at(now + 90 * 60, now).ends_with("(in 1h 30m)"));
        assert!(stops_at(now, now).ends_with("(ending)"));
        assert!(stops_at(now - 30, now).ends_with("(ending)"));
    }

    #[test]
    fn a_row_without_a_ttl_has_no_stops_at_line() {
        let lines = text(&detail_lines_at(&run_entry(None), "", 60, 1_700_000_000));
        assert!(
            !lines.iter().any(|l| l.starts_with("stops at")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_runs_stop_time_survives_a_wide_list_and_a_narrow_one_does_not_panic() {
        let mut app = App::new_demo();
        let tag = app.entries[3].tag().unwrap().to_string();
        assert!(super::render_to_string(&app, 200, 40).contains(&tag));
        app.selected = 3;
        for w in [60u16, 80, 100, 120] {
            let _ = super::render_to_string(&app, w, 30);
        }
    }
}
