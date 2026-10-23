// The startup credential check, driven through the real binary with a stub
// `gcloud` on PATH. The harness has no terminal (stdin is null, stdout a
// pipe), so these cover the paths that must hold without one: a logged-in
// account goes through, a logged-out one fails without ever starting a
// login, and commands that do not touch GCP never probe. The paths where
// the login runs need a TTY and are covered by the unit tests on `next_step`
// and `login_argv` plus a live run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

struct Harness {
    dir: TempDir,
}

const STUB: &str = r#"#!/bin/sh
echo "$@" >> "$STUB_DIR/calls"
case "$1 $2" in
  "auth print-access-token")
    if [ -f "$STUB_DIR/logged-in" ]; then echo stub-token; exit 0; fi
    echo "ERROR: you do not currently have an active account selected" >&2
    exit 1
    ;;
  "auth login")
    touch "$STUB_DIR/login-ran"
    touch "$STUB_DIR/logged-in"
    exit 0
    ;;
esac
exit 0
"#;

const CONFIG: &str = r#"selected_account = "demo"

[[accounts]]
name = "demo"
gcloud_configuration = "work"
project = "demo-project"
default_zone = "europe-west1-b"
"#;

impl Harness {
    fn new(logged_in: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("stub")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();

        let gcloud = root.join("bin/gcloud");
        fs::write(&gcloud, STUB).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&gcloud, fs::Permissions::from_mode(0o755)).unwrap();
        }

        fs::write(root.join("config/config.toml"), CONFIG).unwrap();
        // A fresh cache keeps the post-command update hint from spawning a
        // network check out of the test
        fs::write(
            root.join("config/update-check.json"),
            r#"{"latest_version":"0.0.1","checked_at":99999999999}"#,
        )
        .unwrap();
        if logged_in {
            fs::write(root.join("stub/logged-in"), "").unwrap();
        }
        Self { dir }
    }

    fn stub(&self) -> PathBuf {
        self.dir.path().join("stub")
    }

    fn run(&self, args: &[&str]) -> Output {
        let root = self.dir.path();
        let path = format!(
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            root.join("bin").display()
        );
        Command::new(env!("CARGO_BIN_EXE_gtunnel"))
            .args(args)
            .env("PATH", path)
            .env("HOME", root)
            .env("GTUNNEL_CONFIG_DIR", root.join("config"))
            .env("STUB_DIR", self.stub())
            .stdin(Stdio::null())
            .output()
            .expect("run gtunnel")
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.stub().join("calls")).unwrap_or_default()
    }

    fn exists(&self, marker: &str) -> bool {
        Path::new(&self.stub().join(marker)).exists()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_logged_in_account_goes_through_without_a_login() {
    let h = Harness::new(true);
    let out = h.run(&["list"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(h.calls().contains("auth print-access-token"));
    assert!(!h.calls().contains("auth login"));
    assert!(!h.exists("login-ran"));
}

#[test]
fn the_probe_carries_the_accounts_configuration_and_project() {
    let h = Harness::new(true);
    h.run(&["list"]);
    let calls = h.calls();
    assert!(calls.contains("--configuration=work"), "calls: {calls}");
    assert!(calls.contains("--project=demo-project"), "calls: {calls}");
}

#[test]
fn a_logged_out_account_without_a_terminal_fails_and_never_starts_a_login() {
    let h = Harness::new(false);
    let out = h.run(&["list"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("gtunnel needs a logged-in Google account"),
        "stderr: {err}"
    );
    assert!(err.contains("gcloud auth login"), "stderr: {err}");
    assert!(!h.calls().contains("auth login"));
    assert!(!h.exists("login-ran"));
}

#[test]
fn the_failure_names_the_account_once() {
    let h = Harness::new(false);
    let err = stderr(&h.run(&["list"]));
    assert_eq!(err.matches("needs a logged-in").count(), 1, "stderr: {err}");
    assert!(err.contains("'demo'"), "stderr: {err}");
}

#[test]
fn account_list_never_probes() {
    let h = Harness::new(false);
    let out = h.run(&["account", "list"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(h.calls(), "");
}

#[test]
fn doctor_is_not_stopped_by_the_startup_check() {
    // `doctor` must be free to say auth is broken, so it never gets a
    // login prompt or the startup failure in front of it
    let h = Harness::new(false);
    let out = h.run(&["doctor"]);
    assert!(
        !stderr(&out).contains("needs a logged-in Google account"),
        "stderr: {}",
        stderr(&out)
    );
    assert!(!h.exists("login-ran"));
}
