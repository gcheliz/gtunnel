use std::process::Command;

// AppleScript string literals escape backslashes and quotes
#[cfg(target_os = "macos")]
fn applescript_quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
pub(super) fn osascript_body(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_quote(body),
        applescript_quote(title)
    )
}

/// One notifier to try.
#[derive(Debug, PartialEq)]
pub(super) enum Step {
    /// A program and its arguments
    Program { program: String, args: Vec<String> },
    /// The freedesktop notification service on the session bus, which every
    /// Linux desktop runs, so nothing has to be installed for it
    #[cfg(target_os = "linux")]
    Bus {
        title: String,
        body: String,
        hints: super::linux_bus::Hints,
    },
}

impl Step {
    pub(super) fn new(program: &str, args: &[&str]) -> Self {
        Step::Program {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    /// What a failure is reported as
    pub(super) fn name(&self) -> &str {
        match self {
            Step::Program { program, .. } => program,
            #[cfg(target_os = "linux")]
            Step::Bus { .. } => "session bus",
        }
    }

    /// Delivered, or why not, in words short enough to share one log line.
    pub(super) fn run(&self) -> Result<(), String> {
        match self {
            Step::Program { program, args } => {
                let mut cmd = Command::new(program);
                cmd.args(args);
                run_outcome(crate::proc::output_within_blocking(
                    cmd,
                    crate::proc::NOTIFY_BOUND,
                    program,
                ))
            }
            #[cfg(target_os = "linux")]
            Step::Bus { title, body, hints } => {
                super::linux_bus::send(title, body, hints, crate::proc::NOTIFY_BOUND)
                    .map_err(|e| e.to_string())
            }
        }
    }
}

#[cfg(unix)]
fn killed_by(status: &std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

#[cfg(not(unix))]
fn killed_by(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

pub(super) fn run_outcome(out: anyhow::Result<std::process::Output>) -> Result<(), String> {
    match out {
        Ok(out) if out.status.success() => Ok(()),
        // A program ended by a signal it never raised itself was ended by
        // someone: this is how an endpoint security product blocks one
        Ok(out) if killed_by(&out.status).is_some() => Err(format!(
            "killed by signal {}, which is how endpoint security software ends a program it blocks",
            killed_by(&out.status).unwrap_or_default()
        )),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            Err(format!("{}: {}", out.status, first.trim())
                .trim_end_matches([':', ' '])
                .to_string())
        }
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Err("not installed".to_string())
        }
        Err(e) => Err(format!("{e:#}")),
    }
}

/// The notifiers this platform has, best first.
///
/// macOS ships `osascript`; `terminal-notifier` goes first when installed
/// because its notifications are attributed to it rather than to Script
/// Editor. On Linux the bus is spoken directly: `notify-send` comes from a
/// package minimal installs leave out, so it is only the fallback.
pub(super) fn chain(title: &str, body: &str, osascript: bool) -> Vec<Step> {
    // Only macOS has an interpreter to leave out
    #[cfg(not(target_os = "macos"))]
    let _ = osascript;
    #[cfg(target_os = "macos")]
    return with_osascript(
        vec![Step::new(
            "terminal-notifier",
            &["-title", title, "-message", body],
        )],
        Step::new("osascript", &["-e", &osascript_body(title, body)]),
        osascript,
    );
    #[cfg(not(target_os = "macos"))]
    return vec![
        #[cfg(target_os = "linux")]
        Step::Bus {
            title: title.to_string(),
            body: body.to_string(),
            hints: super::linux_bus::Hints::default(),
        },
        Step::new("notify-send", &[title, body]),
    ];
}

/// The interpreter goes last and only when allowed: a security product that
/// watches for `osascript` run by an unsigned program has no quarrel with
/// `terminal-notifier`, and a person can choose which of the two they accept.
#[cfg(target_os = "macos")]
fn with_osascript(mut steps: Vec<Step>, osascript: Step, allowed: bool) -> Vec<Step> {
    if allowed {
        steps.push(osascript);
    }
    steps
}

/// `chain` for `notifications.style = "rich"`: the same notifiers in the same
/// order, each asked for the columns of the style it can show.
pub(super) fn chain_rich(
    title: &str,
    body: &str,
    style: &super::style::Style,
    osascript: bool,
) -> Vec<Step> {
    #[cfg(not(target_os = "macos"))]
    let _ = osascript;
    #[cfg(target_os = "macos")]
    return with_osascript(
        vec![Step::new(
            "terminal-notifier",
            &[
                "-title",
                title,
                "-subtitle",
                style.label,
                "-message",
                body,
                "-sound",
                style.sound,
            ],
        )],
        Step::new(
            "osascript",
            &["-e", &osascript_body_rich(title, body, style)],
        ),
        osascript,
    );
    #[cfg(not(target_os = "macos"))]
    return vec![
        #[cfg(target_os = "linux")]
        Step::Bus {
            title: title.to_string(),
            body: body.to_string(),
            hints: super::linux_bus::Hints {
                urgency: Some(style.urgency),
                category: Some(style.category),
            },
        },
        Step::new("notify-send", &[title, body]),
    ];
}

#[cfg(target_os = "macos")]
pub(super) fn osascript_body_rich(title: &str, body: &str, style: &super::style::Style) -> String {
    format!(
        "display notification \"{}\" with title \"{}\" subtitle \"{}\" sound name \"{}\"",
        applescript_quote(body),
        applescript_quote(title),
        applescript_quote(style.label),
        applescript_quote(style.sound),
    )
}
