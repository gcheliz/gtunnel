use std::process::Command;

#[cfg(target_os = "macos")]
use super::backend::run_outcome;

/// Whether this machine can show a notification, or what it is missing, for
/// `doctor`.
pub fn check() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("osascript");
        cmd.args(["-e", "return"]);
        run_outcome(crate::proc::output_within_blocking(
            cmd,
            crate::proc::NOTIFY_BOUND,
            "osascript",
        ))
        .map_err(|why| format!("osascript did not run ({why})"))
    }
    #[cfg(target_os = "linux")]
    {
        let bound = crate::proc::NOTIFY_BOUND;
        match super::linux_bus::has_service(zbus::connection::Builder::session(), bound) {
            Err(e) => Err(format!(
                "there is no session D-Bus to deliver to ({e}); a desktop session provides one"
            )),
            Ok(false) => Err(format!(
                "nothing on the session D-Bus owns {}; every desktop environment runs a \
                 notification service, and a bare window manager needs one such as dunst or mako",
                super::linux_bus::SERVICE
            )),
            Ok(true) => watchdog_reaches_bus(),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Ok(())
}

/// The watchdog runs under `systemd --user`, whose environment is not this
/// shell's. zbus finds the bus in `DBUS_SESSION_BUS_ADDRESS`, else at
/// `$XDG_RUNTIME_DIR/bus`, which the user manager always sets.
#[cfg(target_os = "linux")]
fn watchdog_reaches_bus() -> Result<(), String> {
    let mut cmd = Command::new("systemctl");
    cmd.args(["--user", "show-environment"]);
    let Ok(out) = crate::proc::output_within_blocking(cmd, crate::proc::NOTIFY_BOUND, "systemctl")
    else {
        // No user manager: no watchdog either, and doctor says that already
        return Ok(());
    };
    let manager = String::from_utf8_lossy(&out.stdout);
    let shell = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
    match bus_unseen_by_manager(shell.as_deref(), &manager, |p| p.exists()) {
        None => Ok(()),
        Some(addr) => Err(format!(
            "this shell's session bus ({addr}) is not in the systemd user environment, so the \
             watchdog cannot notify; run `systemctl --user import-environment \
             DBUS_SESSION_BUS_ADDRESS`"
        )),
    }
}

/// The shell's bus address when the user manager would connect elsewhere, or
/// nowhere.
#[cfg(target_os = "linux")]
pub(super) fn bus_unseen_by_manager<'a>(
    shell: Option<&'a str>,
    manager_env: &str,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<&'a str> {
    let var = |name: &str| {
        manager_env
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
    };
    let manager = var("DBUS_SESSION_BUS_ADDRESS")
        .map(str::to_string)
        .or_else(|| {
            var("XDG_RUNTIME_DIR")
                .map(|dir| std::path::Path::new(dir).join("bus"))
                .filter(|p| exists(p))
                .map(|p| format!("unix:path={}", p.display()))
        });
    match (shell, manager) {
        (Some(shell), Some(manager)) if shell != manager => Some(shell),
        (Some(shell), None) => Some(shell),
        _ => None,
    }
}
