//! The TUI driven the way a person drives it: the real `gtunnel demo` binary
//! in a pseudo-terminal, keys typed, the screen read back.
//!
//! The render tests in `src/tui/ui/tests.rs` draw `App` onto a `TestBackend`,
//! so they cannot see a key that never reaches the app, a panic on resize, or
//! a terminal left on the alternate screen after `q`. These tests exist for
//! exactly those, and for nothing about layout.
#![cfg(unix)]

use portable_pty::{native_pty_system, CommandBuilder, ExitStatus, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Generous on purpose: a bound is only reached when something is wrong, so a
/// slow CI runner costs nothing and a tight one makes the suite flaky.
const BOUND: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);

/// `Index(236)`, the selected row's background in `src/tui/theme.rs`. The only
/// thing on screen that marks the cursor row, so it is read from the cells.
const SELECTED_BG: u8 = 236;

/// A lock that survives another thread's panic. One failed assertion must show
/// as itself, not as a `PoisonError` in the reader thread that hides the screen.
fn relock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn nz(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n).expect("a screen has at least one row and column")
}

struct Screen {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Every byte the child wrote, so the terminal-restore sequences can be
    /// checked even after the parser has acted on them.
    raw: Arc<Mutex<Vec<u8>>>,
    _home: tempfile::TempDir,
}

impl Screen {
    fn demo() -> Screen {
        let home = tempfile::tempdir().expect("temp dir");
        // `demo` needs no gcloud; an empty bin directory means a stray call
        // fails loudly instead of reaching a real one.
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin).expect("bin dir");

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open a pty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_gtunnel"));
        cmd.arg("demo");
        cmd.env_clear();
        cmd.env("HOME", home.path());
        cmd.env("GTUNNEL_CONFIG_DIR", home.path());
        cmd.env("GTUNNEL_NO_NOTIFY", "1");
        cmd.env("PATH", &bin);
        cmd.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cmd).expect("spawn gtunnel demo");
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(nz(30), nz(100), 0)));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        {
            let (parser, raw) = (parser.clone(), raw.clone());
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                // Ends on EOF or on the EIO a closed pty gives on Linux
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    relock(&raw).extend_from_slice(&buf[..n]);
                    relock(&parser).process(&buf[..n]);
                }
            });
        }
        let writer = pair.master.take_writer().expect("pty writer");
        Screen {
            master: pair.master,
            writer,
            child,
            parser,
            raw,
            _home: home,
        }
    }

    fn press(&mut self, keys: &str) {
        self.writer.write_all(keys.as_bytes()).expect("write keys");
        self.writer.flush().expect("flush keys");
    }

    fn contents(&self) -> String {
        relock(&self.parser).screen().contents()
    }

    /// Polls the parsed screen until `text` is on it. A bounded poll, never a
    /// sleep standing in for synchronisation.
    fn wait_for(&self, text: &str) {
        self.wait_until(&format!("the text {text:?}"), |s| {
            s.contents().contains(text)
        });
    }

    fn wait_gone(&self, text: &str) {
        self.wait_until(&format!("{text:?} to leave the screen"), |s| {
            !s.contents().contains(text)
        });
    }

    fn wait_until(&self, what: &str, ok: impl Fn(&vt100::Screen) -> bool) {
        let start = Instant::now();
        loop {
            if ok(relock(&self.parser).screen()) {
                return;
            }
            assert!(
                start.elapsed() < BOUND,
                "waited {BOUND:?} for {what}; the screen was:\n{}",
                self.contents()
            );
            thread::sleep(POLL);
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        // The parser first: the app redraws the moment the pty changes size, and
        // bytes laid out for the new size must not be parsed at the old one
        relock(&self.parser)
            .screen_mut()
            .set_size(nz(rows), nz(cols));
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize the pty");
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("poll the child") {
                return status;
            }
            assert!(
                start.elapsed() < BOUND,
                "gtunnel demo still running {BOUND:?} after the key; the screen was:\n{}",
                self.contents()
            );
            thread::sleep(POLL);
        }
    }

    fn raw_contains(&self, seq: &[u8]) -> bool {
        relock(&self.raw).windows(seq.len()).any(|w| w == seq)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        // A failed assertion must not leave a TUI running behind the test
        let _ = self.child.kill();
    }
}

/// A bounded poll on the raw stream: the reader thread may still be draining
/// when the child has already exited.
fn wait_raw(screen: &Screen, seq: &[u8], what: &str) {
    let start = Instant::now();
    while !screen.raw_contains(seq) {
        assert!(
            start.elapsed() < BOUND,
            "waited {BOUND:?} for {what} ({:?}) in the output",
            String::from_utf8_lossy(seq)
        );
        thread::sleep(POLL);
    }
}

/// The name on the row that carries the selection background, which is the
/// only thing on screen that marks the cursor row. Takes the parsed screen so a
/// `wait_until` predicate can call it while the parser is locked, so it never
/// panics: nothing may assert under that lock. The third
/// word is the name: the first two are the frame's border and the status dot.
fn selected_name(screen: &vt100::Screen) -> String {
    let rows: Vec<u16> = (0..screen.size().0.get())
        .filter(|&y| {
            screen
                .cell(y, 2)
                .is_some_and(|c| c.bgcolor() == vt100::Color::Idx(SELECTED_BG))
        })
        .collect();
    rows.first()
        .and_then(|&y| screen.rows(0, screen.size().1.get()).nth(y as usize))
        .and_then(|row| row.split_whitespace().nth(2).map(str::to_string))
        .unwrap_or_default()
}

#[test]
fn the_list_draws_with_the_demo_tunnels() {
    let s = Screen::demo();
    s.wait_for("TUNNELS (7)");
    for name in ["db", "redis", "pg-hosts"] {
        s.wait_for(name);
    }
    // A frame can arrive in more than one read, so the highlight is waited for
    s.wait_until("db to be the selected row", |screen| {
        selected_name(screen) == "db"
    });
}

#[test]
fn j_and_k_move_the_selection_between_rows() {
    let mut s = Screen::demo();
    s.wait_for("TUNNELS (7)");
    s.press("j");
    s.wait_until("redis to be the selected row", |screen| {
        selected_name(screen) == "redis"
    });
    s.press("j");
    s.wait_until("pg-hosts to be the selected row", |screen| {
        selected_name(screen) == "pg-hosts"
    });
    s.press("k");
    s.wait_until("redis to be the selected row again", |screen| {
        selected_name(screen) == "redis"
    });
}

#[test]
fn the_keymap_opens_on_question_mark_and_closes_on_escape() {
    let mut s = Screen::demo();
    s.wait_for("TUNNELS (7)");
    s.press("?");
    s.wait_for("Keys - gtunnel");
    s.press("\x1b");
    s.wait_gone("Keys - gtunnel");
    s.wait_for("TUNNELS (7)");
}

#[test]
fn the_add_chooser_opens_on_a_and_escape_cancels_it() {
    let mut s = Screen::demo();
    s.wait_for("TUNNELS (7)");
    s.press("a");
    s.wait_for("New tunnel");
    s.press("\x1b");
    s.wait_gone("New tunnel");
    s.wait_for("TUNNELS (7)");
}

#[test]
fn a_resize_redraws_and_the_app_keeps_answering() {
    let mut s = Screen::demo();
    s.wait_for("TUNNELS (7)");
    s.resize(40, 120);
    // The list's frame spans the whole new width only once it has redrawn
    s.wait_until("the list frame to span 120 columns", |screen| {
        screen
            .rows(0, 120)
            .any(|r| r.starts_with("\u{256d}") && r.chars().count() == 120)
    });
    s.resize(20, 70);
    s.wait_until("a redraw at 70 columns", |screen| {
        screen
            .rows(0, 120)
            .any(|r| r.starts_with("\u{256d}") && r.chars().count() == 70)
    });
    assert!(
        s.child.try_wait().expect("poll").is_none(),
        "the app died on resize"
    );
    s.press("?");
    s.wait_for("Keys - gtunnel");
}

#[test]
fn q_exits_zero_and_restores_the_terminal() {
    let mut s = Screen::demo();
    s.wait_for("TUNNELS (7)");
    // ratatui writes the hide-cursor sequence after the frame, so the frame can
    // be on screen a read before the cursor is hidden
    s.wait_until("the alternate screen with the cursor hidden", |screen| {
        screen.alternate_screen() && screen.hide_cursor()
    });
    s.press("q");
    let status = s.wait_exit();
    assert!(status.success(), "exit status {status:?}");
    wait_raw(&s, b"\x1b[?1049l", "leave-alternate-screen");
    wait_raw(&s, b"\x1b[?25h", "show-cursor");
    // The reader appends to the raw bytes before it parses them, so the parser
    // is waited on too, never read once
    s.wait_until("the main screen with the cursor shown", |screen| {
        !screen.alternate_screen() && !screen.hide_cursor()
    });
}
