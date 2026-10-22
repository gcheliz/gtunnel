use std::process::{Command, Stdio};

// AppleScript string literals escape backslashes and quotes
fn applescript_quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn osascript_body(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_quote(body),
        applescript_quote(title)
    )
}

fn run(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// Best effort: a desktop without a notifier is not an error
pub fn notify(title: &str, body: &str) {
    if run("terminal-notifier", &["-title", title, "-message", body]) {
        return;
    }
    if run("osascript", &["-e", &osascript_body(title, body)]) {
        return;
    }
    run("notify-send", &[title, body]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_applescript_payload_escapes_quotes_and_backslashes() {
        let script = osascript_body("he said \"hi\"", "path C:\\tmp");
        assert!(script.contains("\\\"hi\\\""), "{script}");
        assert!(script.contains("C:\\\\tmp"), "{script}");
        assert!(script.starts_with("display notification"), "{script}");
    }
}
