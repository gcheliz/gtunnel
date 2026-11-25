#[cfg(target_os = "macos")]
use super::backend::osascript_body;
use super::backend::{chain, Step};
#[cfg(target_os = "linux")]
use super::check::bus_unseen_by_manager;
#[cfg(target_os = "macos")]
use super::check::check;
use super::dispatch::{spawn_chain, Chain, Pending};
#[cfg(target_os = "linux")]
use super::linux_bus;

#[cfg(target_os = "macos")]
#[test]
fn the_applescript_payload_escapes_quotes_and_backslashes() {
    let script = osascript_body("he said \"hi\"", "path C:\\tmp");
    assert!(script.contains("\\\"hi\\\""), "{script}");
    assert!(script.contains("C:\\\\tmp"), "{script}");
    assert!(script.starts_with("display notification"), "{script}");
}

#[test]
fn notify_returns_before_the_notifier_finishes() {
    let chain = vec![Step::new("sleep", &["1"])];
    let started = std::time::Instant::now();
    let handle = spawn_chain(chain);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "the caller waited {:?} for the notifier",
        started.elapsed()
    );
    handle.join().unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn macos_tries_terminal_notifier_then_osascript() {
    let steps = chain("t", "b", true);
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert_eq!(
        steps[0],
        Step::new("terminal-notifier", &["-title", "t", "-message", "b"])
    );
    assert_eq!(
        steps[1],
        Step::new("osascript", &["-e", &osascript_body("t", "b")])
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_speaks_the_bus_first_and_never_tries_the_macos_programs() {
    let steps = chain("t", "b", true);
    assert_eq!(
        steps,
        vec![
            Step::Bus {
                title: "t".into(),
                body: "b".into(),
                hints: linux_bus::Hints::default(),
            },
            Step::new("notify-send", &["t", "b"]),
        ]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_bus_call_has_the_signature_the_specification_names() {
    use zbus::Type;
    assert_eq!(
        <linux_bus::NotifyArgs as Type>::SIGNATURE.to_string(),
        "(susssasa{sv}i)"
    );
    let (app, replaces, _, summary, body, _, _, timeout) =
        linux_bus::args("t", "b", &linux_bus::Hints::default());
    assert_eq!(
        (app, replaces, summary, body, timeout),
        ("gtunnel", 0, "t", "b", -1)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_bus_step_fails_fast_when_there_is_no_bus() {
    let started = std::time::Instant::now();
    let missing = zbus::connection::Builder::address("unix:path=/nonexistent/gtunnel-bus");
    let err = linux_bus::send_on(
        missing,
        "t",
        "b",
        &linux_bus::Hints::default(),
        std::time::Duration::from_secs(2),
    )
    .unwrap_err();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "{err:#}"
    );
}

#[test]
fn flush_waits_for_a_chain_that_is_still_delivering() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("delivered");
    let script = format!("sleep 0.3; touch '{}'", marker.display());
    let pending = Pending::new();
    pending.track(spawn_chain(vec![Step::new("sh", &["-c", &script])]));
    assert!(!marker.exists(), "the chain had not run yet");
    pending.flush_within(std::time::Duration::from_secs(5));
    assert!(marker.exists(), "flush returned before the chain finished");
}

#[test]
fn flush_abandons_a_chain_that_outlives_the_bound() {
    let pending = Pending::new();
    pending.track(spawn_chain(vec![Step::new("sleep", &["3"])]));
    let started = std::time::Instant::now();
    pending.flush_within(std::time::Duration::from_millis(200));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "flush waited {:?} past its bound",
        started.elapsed()
    );
}

#[test]
fn flush_with_nothing_pending_returns_at_once() {
    let started = std::time::Instant::now();
    Pending::new().flush_within(std::time::Duration::from_secs(5));
    assert!(started.elapsed() < std::time::Duration::from_millis(100));
}

#[test]
fn a_finished_chain_is_not_kept() {
    let pending = Pending::new();
    let done = spawn_chain(vec![Step::new("true", &[])]);
    while !done.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    pending.track(done);
    pending.track(spawn_chain(vec![Step::new("true", &[])]));
    assert_eq!(pending.lock().running.len(), 1);
}

fn finished(chain: Chain) -> Option<String> {
    chain.join().unwrap()
}

#[test]
fn a_chain_that_delivers_reports_nothing() {
    let steps = vec![Step::new("false", &[]), Step::new("true", &[])];
    assert_eq!(finished(spawn_chain(steps)), None);
}

#[test]
fn a_chain_that_delivers_nothing_names_every_step_and_why() {
    let steps = vec![
        Step::new("gtunnel-no-such-notifier", &[]),
        Step::new("sh", &["-c", "echo 'no display' >&2; exit 3"]),
    ];
    let line = finished(spawn_chain(steps)).expect("a failure line");
    assert_eq!(
        line,
        "notification not delivered: gtunnel-no-such-notifier: not installed; \
         sh: exit status: 3: no display"
    );
}

#[test]
fn a_failing_program_with_nothing_on_stderr_says_only_its_status() {
    let line = finished(spawn_chain(vec![Step::new("false", &[])])).unwrap();
    assert_eq!(line, "notification not delivered: false: exit status: 1");
}

#[test]
fn flush_returns_the_failure_once_and_a_finished_chain_keeps_it() {
    let pending = Pending::new();
    let failed = spawn_chain(vec![Step::new("false", &[])]);
    while !failed.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    pending.track(failed);
    // Reaps the finished one: its failure must outlive it
    pending.track(spawn_chain(vec![Step::new("true", &[])]));
    let bound = std::time::Duration::from_secs(5);
    assert_eq!(
        pending.flush_within(bound).as_deref(),
        Some("notification not delivered: false: exit status: 1")
    );
    assert_eq!(pending.flush_within(bound), None, "said once");
}

#[cfg(target_os = "linux")]
#[test]
fn the_watchdog_sees_the_bus_the_shell_sees() {
    let at_runtime_dir = |p: &std::path::Path| p == std::path::Path::new("/run/user/1000/bus");
    let manager = "HOME=/home/you\nXDG_RUNTIME_DIR=/run/user/1000\n";
    // The standard socket, reached through the runtime directory
    assert_eq!(
        bus_unseen_by_manager(
            Some("unix:path=/run/user/1000/bus"),
            manager,
            at_runtime_dir
        ),
        None
    );
    // No address in the shell: zbus uses the runtime directory there too
    assert_eq!(bus_unseen_by_manager(None, manager, at_runtime_dir), None);
    // A bus launched by the X session, which systemd never heard of
    let launched = "unix:abstract=/tmp/dbus-XYZ,guid=1";
    assert_eq!(
        bus_unseen_by_manager(Some(launched), manager, at_runtime_dir),
        Some(launched)
    );
    // Imported into the manager: seen
    let imported = format!("{manager}DBUS_SESSION_BUS_ADDRESS={launched}\n");
    assert_eq!(
        bus_unseen_by_manager(Some(launched), &imported, at_runtime_dir),
        None
    );
    // No socket in the runtime directory and nothing imported
    assert_eq!(
        bus_unseen_by_manager(Some("unix:path=/run/user/1000/bus"), manager, |_| false),
        Some("unix:path=/run/user/1000/bus")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_service_check_fails_fast_when_there_is_no_bus() {
    let missing = zbus::connection::Builder::address("unix:path=/nonexistent/gtunnel-bus");
    assert!(linux_bus::has_service(missing, std::time::Duration::from_secs(2)).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_can_notify() {
    assert_eq!(check(), Ok(()));
}

#[test]
fn severity_runs_from_least_to_most_serious() {
    use super::Severity::*;
    assert!(Info < Recovered && Recovered < Warning && Warning < Failure);
    assert_eq!([Recovered, Failure, Info].into_iter().max(), Some(Failure));
}

#[test]
fn a_notification_is_titled_for_the_program_and_grouped_only_when_asked() {
    use super::{Notification, Severity};
    let plain = Notification::new(Severity::Warning, "db is unhealthy");
    assert_eq!(plain.title(), "gtunnel");
    assert_eq!(plain.group, None);
    let about = plain.about("dev/db");
    assert_eq!(about.group.as_deref(), Some("dev/db"));
    assert_eq!(about.body, "db is unhealthy");
}

#[test]
fn every_severity_has_a_style_that_a_server_will_accept() {
    use super::{style, Severity::*};
    for severity in [Info, Recovered, Warning, Failure] {
        let s = style::of(severity);
        assert!(s.urgency <= 2, "{severity:?}: urgency is 0, 1 or 2");
        assert!(s.category.starts_with("network."), "{severity:?}");
        assert!(!s.glyph.is_empty() && !s.label.is_empty() && !s.sound.is_empty());
    }
    // Only a failure is critical, so only a failure outlasts its banner
    assert_eq!(style::of(Failure).urgency, 2);
    assert!([Info, Recovered, Warning]
        .into_iter()
        .all(|s| style::of(s).urgency < 2));
}

#[test]
fn the_title_leads_with_the_glyph_of_the_severity() {
    use super::{style, Severity};
    let s = style::of(Severity::Failure);
    assert_eq!(style::title(&s, "gtunnel"), format!("{} gtunnel", s.glyph));
}

#[test]
fn plain_is_the_chain_notifications_always_had() {
    use super::{steps_for, Notification, Severity};
    for severity in [Severity::Info, Severity::Warning, Severity::Failure] {
        let n = Notification::new(severity, "db is unhealthy");
        assert_eq!(
            steps_for(&n, crate::config::NotifyStyle::Plain, true),
            chain("gtunnel", "db is unhealthy", true),
            "{severity:?}"
        );
    }
}

#[test]
fn rich_asks_the_same_notifiers_in_the_same_order_but_not_the_same_words() {
    use super::{steps_for, Notification, Severity};
    let n = Notification::new(Severity::Failure, "db failed");
    let plain = steps_for(&n, crate::config::NotifyStyle::Plain, true);
    let rich = steps_for(&n, crate::config::NotifyStyle::Rich, true);
    assert_eq!(plain.len(), rich.len());
    let names = |steps: &[Step]| {
        steps
            .iter()
            .map(|s| s.name().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&plain), names(&rich));
    assert_ne!(plain, rich);
}

#[cfg(target_os = "macos")]
#[test]
fn a_rich_macos_notification_carries_a_subtitle_and_a_sound() {
    use super::backend::osascript_body_rich;
    use super::{style, Severity};
    let s = style::of(Severity::Warning);
    let script = osascript_body_rich("t", "he said \"hi\"", &s);
    assert!(script.starts_with("display notification"), "{script}");
    assert!(script.contains("\\\"hi\\\""), "{script}");
    assert!(
        script.contains(&format!("subtitle \"{}\"", s.label)),
        "{script}"
    );
    assert!(
        script.contains(&format!("sound name \"{}\"", s.sound)),
        "{script}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_rich_linux_notification_sends_urgency_and_category_as_hints() {
    let hints = linux_bus::Hints {
        urgency: Some(2),
        category: Some("network.error"),
    };
    let (_, _, _, _, _, _, sent, _) = linux_bus::args("t", "b", &hints);
    let mut keys: Vec<_> = sent.keys().copied().collect();
    keys.sort_unstable();
    assert_eq!(keys, ["category", "urgency"]);
    let (_, _, _, _, _, _, none, _) = linux_bus::args("t", "b", &linux_bus::Hints::default());
    assert!(none.is_empty(), "plain sends no hints, as before");
}

#[cfg(target_os = "macos")]
#[test]
fn osascript_is_the_last_notifier_and_only_when_allowed() {
    let with = chain("t", "b", true);
    let without = chain("t", "b", false);
    let names = |steps: &[Step]| {
        steps
            .iter()
            .map(|s| s.name().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&with), ["terminal-notifier", "osascript"]);
    assert_eq!(names(&without), ["terminal-notifier"]);
    // Rich takes the same decision
    let rich = super::style::of(super::Severity::Failure);
    let rich_without = super::backend::chain_rich("t", "b", &rich, false);
    assert_eq!(names(&rich_without), ["terminal-notifier"]);
}

#[cfg(target_os = "linux")]
#[test]
fn the_osascript_switch_changes_nothing_on_linux() {
    assert_eq!(chain("t", "b", true), chain("t", "b", false));
}

#[cfg(unix)]
#[test]
fn a_notifier_ended_by_a_signal_is_reported_as_probably_blocked() {
    let step = Step::new("sh", &["-c", "kill -KILL $$"]);
    let why = step.run().unwrap_err();
    assert!(why.starts_with("killed by signal 9"), "{why}");
    assert!(why.contains("endpoint security"), "{why}");
    // An ordinary non-zero exit is still just its status
    let plain = Step::new("sh", &["-c", "exit 3"]).run().unwrap_err();
    assert_eq!(plain, "exit status: 3");
}

#[cfg(target_os = "macos")]
#[test]
fn on_path_finds_an_executable_file_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join("terminal-notifier");
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    let path = std::env::join_paths([dir.path()]).unwrap();
    // Present but not executable is not installed
    assert!(!super::check::on_path("terminal-notifier", Some(&path)));
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(super::check::on_path("terminal-notifier", Some(&path)));
    assert!(!super::check::on_path("other", Some(&path)));
    assert!(!super::check::on_path("terminal-notifier", None));
    // A directory of that name is not a program
    std::fs::remove_file(&tool).unwrap();
    std::fs::create_dir(&tool).unwrap();
    assert!(!super::check::on_path("terminal-notifier", Some(&path)));
}
