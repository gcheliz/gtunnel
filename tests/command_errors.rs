use std::fs;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

struct Harness {
    dir: TempDir,
}

const LOGGED_OUT_STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token")
    echo "ERROR: (gcloud.auth.print-access-token) No credential found" >&2
    exit 1
    ;;
esac
exit 0
"#;

const STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token")
    echo stub-token
    exit 0
    ;;
esac
exit 0
"#;

// Login must still pass, or `start` stops at the login check before it ever
// starts a job; only the tunnel itself fails
const FAILING_STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token")
    echo stub-token
    exit 0
    ;;
esac
echo "ERROR: boom" >&2
exit 1
"#;

const CONFIG: &str = r#"selected_account = "demo"

[[accounts]]
name = "demo"
gcloud_configuration = "work"
project = "demo-project"
default_zone = "europe-west1-b"
"#;

const TUNNELS: &str = r#"[[tunnels]]
name = "db"
account_name = "demo"
auto_start = false

[tunnels.spec]
mode = "access"
remote_port = 5432
local_port = 15432

[tunnels.spec.target]
kind = "instance"
instance = "postgres-1"
zone = "europe-west1-b"
"#;

impl Harness {
    fn logged_out() -> Self {
        Self::with_stub(LOGGED_OUT_STUB)
    }

    fn new() -> Self {
        Self::with_stub(STUB)
    }

    // A gcloud that dies at once, so a started tunnel's job exits inside the
    // settle window and its status reads as an error
    fn failing_gcloud() -> Self {
        Self::with_stub(FAILING_STUB)
    }

    fn with_stub(stub: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();

        let gcloud = root.join("bin/gcloud");
        fs::write(&gcloud, stub).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&gcloud, fs::Permissions::from_mode(0o755)).unwrap();
        }

        fs::write(root.join("config/config.toml"), CONFIG).unwrap();
        fs::write(root.join("config/tunnels.toml"), TUNNELS).unwrap();
        fs::write(
            root.join("config/update-check.json"),
            r#"{"latest_version":"0.0.1","checked_at":99999999999}"#,
        )
        .unwrap();
        Self { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        let root = self.dir.path();
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_gtunnel"))
            .args(args)
            .env("PATH", path)
            .env("HOME", root)
            .env("GTUNNEL_CONFIG_DIR", root.join("config"))
            .stdin(Stdio::null())
            .output()
            .expect("run gtunnel")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn logs_nonexistent_tunnel_fails_with_actionable_error() {
    let h = Harness::new();
    let out = h.run(&["logs", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Error: Tunnel 'nope' not found for account 'demo'"));
}

#[test]
fn move_same_account_fails_with_rename_suggestion() {
    let h = Harness::new();
    let out = h.run(&["move", "db", "--to", "demo"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains(
        "Error: Tunnel 'db' is already on account 'demo'; use `gtunnel rename` to change its name"
    ));
}

#[test]
fn account_remove_with_tunnels_fails_with_guidance() {
    let h = Harness::new();
    let out = h.run(&["account", "remove", "demo"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Error: Account 'demo' still has 1 tunnel(s). Delete them with `gtunnel delete`, or move them to another account with `gtunnel move`, then remove the account"));
}

#[test]
fn account_edit_no_args_fails_with_hint() {
    let h = Harness::new();
    let out = h.run(&["account", "edit", "demo"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Error: Nothing to change: pass --project, --zone, or both"));
}

#[test]
fn add_without_start_does_not_require_gcp_login() {
    let h = Harness::logged_out();
    let out = h.run(&["add", "new-db", "10.0.0.10:5432", "--via", "bastion"]);
    let err = stderr(&out);
    assert!(out.status.success(), "stderr: {err}");
    assert!(
        !err.contains("needs a logged-in Google account"),
        "stderr: {err}"
    );
}

#[test]
fn add_with_start_requires_gcp_login() {
    let h = Harness::logged_out();
    let out = h.run(&["add", "new-db", "10.0.0.10:5432", "--via", "bastion", "-s"]);
    let err = stderr(&out);
    assert!(!out.status.success(), "stderr: {err}");
    assert!(
        err.contains("needs a logged-in Google account"),
        "stderr: {err}"
    );
}

#[test]
fn rename_does_not_require_gcp_login() {
    let h = Harness::logged_out();
    let out = h.run(&["rename", "db", "database"]);
    let err = stderr(&out);
    assert!(out.status.success(), "stderr: {err}");
    assert!(
        !err.contains("needs a logged-in Google account"),
        "stderr: {err}"
    );
}

#[test]
fn autostart_does_not_require_gcp_login() {
    let h = Harness::logged_out();
    let out = h.run(&["autostart", "db"]);
    let err = stderr(&out);
    assert!(out.status.success(), "stderr: {err}");
    assert!(
        !err.contains("needs a logged-in Google account"),
        "stderr: {err}"
    );
}

#[test]
fn delete_does_not_require_gcp_login() {
    let h = Harness::logged_out();
    let out = h.run(&["delete", "db"]);
    let err = stderr(&out);
    assert!(out.status.success(), "stderr: {err}");
    assert!(
        !err.contains("needs a logged-in Google account"),
        "stderr: {err}"
    );
}

// The job is a launchd one, so the test needs a macOS user session
#[cfg(target_os = "macos")]
#[test]
fn start_exits_non_zero_when_the_tunnel_failed_to_start() {
    let h = Harness::failing_gcloud();
    let out = h.run(&["start", "db"]);
    // The job stays loaded in the user's launchd domain after the test
    h.run(&["stop", "db"]);
    let (so, se) = (stdout(&out), stderr(&out));
    assert_eq!(out.status.code(), Some(1), "stdout: {so}\nstderr: {se}");
    // The lines a failed start always printed, where scripts already read them
    assert!(so.contains("✗ db failed to start"), "stdout: {so}");
    assert!(so.contains("Logs: gtunnel logs db"), "stdout: {so}");
    assert!(!se.contains("failed to start"), "printed twice: {se}");
}
