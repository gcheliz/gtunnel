//! A notification the terminal shows, by escape sequence.
//!
//! Several terminals turn an operating system command into a desktop
//! notification: nothing is spawned, nothing is installed, and no program an
//! endpoint security product watches for ever runs. It only works while the
//! TUI has a terminal open and only in a terminal that understands one of the
//! sequences, so it is a first choice that falls through to the desktop chain,
//! never a replacement for it.
//!
//! The TUI owns the screen, so a sequence is not written from the thread that
//! raised the notification: it is queued here and the TUI writes it between
//! two frames, where it cannot land inside a half-written one.

use std::sync::Mutex;

/// The sequence a terminal turns into a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `OSC 9 ; text`: iTerm2, Ghostty. One line, no separate title.
    Osc9,
    /// `OSC 777 ; notify ; title ; body`: WezTerm, foot
    Osc777,
    /// kitty's `OSC 99`, which carries the title and the body as two chunks
    Osc99,
}

/// The protocol this terminal understands, from the variables it sets, or
/// `None` for one that is not known to show notifications (Terminal.app, a
/// plain `xterm`, a Linux console). `env` is a parameter so a test does not
/// touch the process environment, which `cargo test` shares between threads.
pub fn detect(env: &dyn Fn(&str) -> Option<String>) -> Option<Protocol> {
    let set = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    let program = env("TERM_PROGRAM").unwrap_or_default();
    let term = env("TERM").unwrap_or_default();
    if set("KITTY_WINDOW_ID") || term == "xterm-kitty" {
        Some(Protocol::Osc99)
    } else if program == "iTerm.app"
        || set("ITERM_SESSION_ID")
        || program == "ghostty"
        || set("GHOSTTY_RESOURCES_DIR")
        || term == "xterm-ghostty"
    {
        // iTerm2 and Ghostty both take the one-line form
        Some(Protocol::Osc9)
    } else if program == "WezTerm" || set("WEZTERM_EXECUTABLE") || term.starts_with("foot") {
        Some(Protocol::Osc777)
    } else {
        None
    }
}

/// Text that cannot end or reshape the sequence it is put in: a control
/// character would close it early and a `;` would start another field.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            ';' => ',',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

/// The bytes to write to the terminal. Inside tmux the sequence is wrapped so
/// tmux passes it on, which it only does with `allow-passthrough on`.
pub fn encode(protocol: Protocol, title: &str, body: &str, in_tmux: bool) -> Vec<u8> {
    let (title, body) = (clean(title), clean(body));
    let raw = match protocol {
        Protocol::Osc9 => format!("\x1b]9;{title}: {body}\x07"),
        Protocol::Osc777 => format!("\x1b]777;notify;{title};{body}\x07"),
        // Two chunks that share an id: the title, then the body, which is the last
        Protocol::Osc99 => format!(
            "\x1b]99;i=gtunnel:d=0:p=title;{title}\x1b\\\x1b]99;i=gtunnel:d=1:p=body;{body}\x1b\\"
        ),
    };
    if in_tmux {
        // Every escape inside the passthrough is doubled
        format!("\x1bPtmux;{}\x1b\\", raw.replace('\x1b', "\x1b\x1b")).into_bytes()
    } else {
        raw.into_bytes()
    }
}

/// What the TUI has yet to write. `None` is no TUI: nothing can be shown by
/// the terminal, and a delivery says so, so the chain moves on.
static QUEUE: Mutex<Option<Vec<Vec<u8>>>> = Mutex::new(None);

fn queue() -> std::sync::MutexGuard<'static, Option<Vec<Vec<u8>>>> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The TUI has taken the terminal and will write what is queued
pub fn open() {
    *queue() = Some(Vec::new());
}

/// Whether this process is the one with a terminal open
pub fn is_open() -> bool {
    queue().is_some()
}

/// The TUI has given the terminal back
pub fn close() {
    *queue() = None;
}

/// What is queued, for the TUI to write between two frames
pub fn drain() -> Vec<Vec<u8>> {
    queue().as_mut().map(std::mem::take).unwrap_or_default()
}

fn from_process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Queues the notification for the TUI, or says why the terminal cannot show it.
pub fn deliver(title: &str, body: &str) -> Result<(), String> {
    deliver_with(title, body, &from_process_env)
}

fn deliver_with(
    title: &str,
    body: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let mut slot = queue();
    let Some(pending) = slot.as_mut() else {
        return Err("the TUI is not open".to_string());
    };
    let Some(protocol) = detect(env) else {
        return Err("this terminal is not known to show notifications".to_string());
    };
    let in_tmux = env("TMUX").is_some_and(|v| !v.is_empty());
    pending.push(encode(protocol, title, body, in_tmux));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn a_terminal_is_known_by_the_variables_it_sets() {
        let seen = |pairs: &[(&str, &str)]| detect(&env_of(pairs));
        assert_eq!(seen(&[("KITTY_WINDOW_ID", "3")]), Some(Protocol::Osc99));
        assert_eq!(seen(&[("TERM", "xterm-kitty")]), Some(Protocol::Osc99));
        assert_eq!(seen(&[("TERM_PROGRAM", "iTerm.app")]), Some(Protocol::Osc9));
        assert_eq!(seen(&[("TERM_PROGRAM", "ghostty")]), Some(Protocol::Osc9));
        assert_eq!(seen(&[("TERM", "xterm-ghostty")]), Some(Protocol::Osc9));
        assert_eq!(seen(&[("TERM_PROGRAM", "WezTerm")]), Some(Protocol::Osc777));
        assert_eq!(seen(&[("TERM", "foot-extra")]), Some(Protocol::Osc777));
    }

    #[test]
    fn a_terminal_that_is_not_known_to_notify_is_not_guessed_at() {
        let seen = |pairs: &[(&str, &str)]| detect(&env_of(pairs));
        assert_eq!(seen(&[]), None);
        assert_eq!(seen(&[("TERM_PROGRAM", "Apple_Terminal")]), None);
        assert_eq!(seen(&[("TERM", "xterm-256color")]), None);
        assert_eq!(seen(&[("KITTY_WINDOW_ID", "")]), None, "set but empty");
    }

    #[test]
    fn each_protocol_is_the_bytes_its_terminal_documents() {
        assert_eq!(
            encode(Protocol::Osc9, "gtunnel", "db failed", false),
            b"\x1b]9;gtunnel: db failed\x07"
        );
        assert_eq!(
            encode(Protocol::Osc777, "gtunnel", "db failed", false),
            b"\x1b]777;notify;gtunnel;db failed\x07"
        );
        assert_eq!(
            encode(Protocol::Osc99, "gtunnel", "db failed", false),
            b"\x1b]99;i=gtunnel:d=0:p=title;gtunnel\x1b\\\x1b]99;i=gtunnel:d=1:p=body;db failed\x1b\\"
        );
    }

    #[test]
    fn text_cannot_end_or_split_the_sequence_it_is_in() {
        let bytes = encode(Protocol::Osc777, "t", "a;b\x07\x1b]0;evil\x07", false);
        let text = String::from_utf8(bytes).unwrap();
        // Only the terminator the encoder wrote is a control character
        assert_eq!(text.matches('\x07').count(), 1, "{text:?}");
        assert_eq!(text.matches('\x1b').count(), 1, "{text:?}");
        // Exactly the three separators of the format
        assert_eq!(text.matches(';').count(), 3, "{text:?}");
    }

    #[test]
    fn inside_tmux_the_sequence_is_wrapped_and_its_escapes_doubled() {
        let bytes = encode(Protocol::Osc9, "t", "b", true);
        assert_eq!(bytes, b"\x1bPtmux;\x1b\x1b]9;t: b\x07\x1b\\");
    }

    #[test]
    fn delivery_queues_for_the_tui_and_says_why_when_it_cannot() {
        // One test, because the queue is the process's: parallel tests would share it
        close();
        let kitty = env_of(&[("KITTY_WINDOW_ID", "1")]);
        assert_eq!(
            deliver_with("t", "b", &kitty),
            Err("the TUI is not open".to_string())
        );
        open();
        let plain = env_of(&[("TERM", "xterm-256color")]);
        assert_eq!(
            deliver_with("t", "b", &plain),
            Err("this terminal is not known to show notifications".to_string())
        );
        assert!(drain().is_empty(), "a refusal queues nothing");
        assert_eq!(deliver_with("t", "b", &kitty), Ok(()));
        let queued = drain();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].starts_with(b"\x1b]99;"));
        assert!(drain().is_empty(), "drained once");
        let in_tmux = env_of(&[
            ("TERM_PROGRAM", "iTerm.app"),
            ("TMUX", "/tmp/tmux-1/default,1,0"),
        ]);
        assert_eq!(deliver_with("t", "b", &in_tmux), Ok(()));
        assert!(drain()[0].starts_with(b"\x1bPtmux;"));
        close();
        assert!(drain().is_empty());
    }
}
