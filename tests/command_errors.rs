use std::fs;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

struct Harness {
    dir: TempDir,
    // Set only by a test of the timeout path: a short bound on every real
    // launchctl would make the other tests flaky
    service_bound_ms: Option<u64>,
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
#[cfg(target_os = "macos")]
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
    #[cfg(target_os = "macos")]
    fn failing_gcloud() -> Self {
        Self::with_stub(FAILING_STUB)
    }

    fn with_stub(stub: &str) -> Self {
        Self::with_stubs(&[("gcloud", stub)])
    }

    // Each stub is an executable of that name in the first directory on PATH,
    // so it stands in for the real program in the run
    fn with_stubs(stubs: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();

        for (name, body) in stubs {
            let path = root.join("bin").join(name);
            fs::write(&path, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        fs::write(root.join("config/config.toml"), CONFIG).unwrap();
        fs::write(root.join("config/tunnels.toml"), TUNNELS).unwrap();
        fs::write(
            root.join("config/update-check.json"),
            r#"{"latest_version":"0.0.1","checked_at":99999999999}"#,
        )
        .unwrap();
        Self {
            dir,
            service_bound_ms: None,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let root = self.dir.path();
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gtunnel"));
        if let Some(ms) = self.service_bound_ms {
            cmd.env("GTUNNEL_SERVICE_BOUND_MS", ms.to_string());
        }
        cmd.args(args)
            .env("PATH", path)
            .env("HOME", root)
            .env("GTUNNEL_CONFIG_DIR", root.join("config"))
            .env("GTUNNEL_NO_NOTIFY", "1")
            .stdin(Stdio::null())
            .output()
            .expect("run gtunnel")
    }
}

#[cfg(target_os = "macos")]
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
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("Error: Tunnel 'nope' not found for account 'demo'"));
}

#[test]
fn move_same_account_fails_with_rename_suggestion() {
    let h = Harness::new();
    let out = h.run(&["move", "db", "--to", "demo"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains(
        "Error: Tunnel 'db' is already on account 'demo'; use `gtunnel rename` to change its name"
    ));
}

#[test]
fn add_of_an_existing_name_exits_2() {
    let h = Harness::new();
    let out = h.run(&["add", "db", "10.0.0.10:5432", "--via", "bastion"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("Error: Tunnel 'db' already exists for account 'demo'"));
}

// A gcloud that lists one configuration, so `account add` reaches the
// duplicate check instead of stopping at an unknown configuration
const CONFIGURATIONS_STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "config configurations")
    echo '[{"name":"work","is_active":true,"properties":{"core":{"project":"example-dev-1a2b"},"compute":{"zone":"europe-west1-b"}}}]'
    exit 0
    ;;
esac
exit 0
"#;

#[test]
fn account_add_of_an_existing_account_exits_2() {
    let h = Harness::with_stub(CONFIGURATIONS_STUB);
    let out = h.run(&["account", "add", "demo", "--configuration", "work"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("Error: Account 'demo' already exists"));
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
    assert_eq!(out.status.code(), Some(2));
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

// argv never touches a shell, but gcloud reads any element that starts with a
// dash as one of its own options, so these never get as far as a record
#[test]
fn add_refuses_an_instance_that_gcloud_would_read_as_a_flag() {
    let h = Harness::new();
    let out = h.run(&["add", "bad", "--via=--command=id", "10.0.0.10:22"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "stderr: {err}");
    assert!(err.contains("Invalid instance '--command=id'"), "{err}");
    let out = h.run(&["add", "--", "bad", "--command=id:22"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "stderr: {err}");
    assert!(err.contains("Invalid instance '--command=id'"), "{err}");
}

#[test]
fn add_refuses_a_host_or_zone_that_gcloud_would_read_as_a_flag() {
    let h = Harness::new();
    let out = h.run(&[
        "add",
        "--via",
        "bastion",
        "--",
        "bad",
        "-oProxyCommand=id:22",
    ]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "stderr: {err}");
    assert!(err.contains("Invalid host '-oProxyCommand=id'"), "{err}");
    let out = h.run(&[
        "add",
        "bad",
        "10.0.0.10:22",
        "--via",
        "bastion",
        "--zone=-x",
    ]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "stderr: {err}");
    assert!(err.contains("Invalid zone '-x'"), "{err}");
    let tunnels = fs::read_to_string(h.dir.path().join("config/tunnels.toml")).unwrap();
    assert!(!tunnels.contains("bad"), "{tunnels}");
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

// The job is a launchd one, so the test needs a macOS user session. The
// launchd domain is shared by every test in this binary, and the others run
// `delete db` and `rename db` in parallel, so this one starts a tunnel no
// other test names
#[cfg(target_os = "macos")]
#[test]
fn start_exits_non_zero_when_the_tunnel_failed_to_start() {
    let h = Harness::failing_gcloud();
    let add = h.run(&["add", "failstart", "10.0.0.10:5432", "--via", "bastion"]);
    assert!(add.status.success(), "stderr: {}", stderr(&add));
    let out = h.run(&["start", "failstart"]);
    // The job stays loaded in the user's launchd domain after the test
    h.run(&["stop", "failstart"]);
    let (so, se) = (stdout(&out), stderr(&out));
    let both = format!("stdout: {so}\nstderr: {se}");
    assert_eq!(out.status.code(), Some(1), "{both}");
    // The lines a failed start always printed, where scripts already read them
    assert!(so.contains("✗ failstart failed to start"), "{both}");
    assert!(so.contains("Logs: gtunnel logs failstart"), "{both}");
    assert!(!se.contains("failed to start"), "printed twice: {both}");
}

// `launchctl` is a stub that outlives the bound, so no real job is touched.
// The name is used by no other test: the label is still in the real launchd
// domain's namespace.
#[cfg(target_os = "macos")]
#[test]
fn delete_keeps_the_record_when_stop_times_out() {
    let mut h = Harness::with_stubs(&[
        ("gcloud", STUB),
        ("launchctl", "#!/bin/sh\nexec sleep 20\n"),
    ]);
    h.service_bound_ms = Some(300);
    let add = h.run(&["add", "slowdelete", "10.0.0.10:5432", "--via", "bastion"]);
    assert!(add.status.success(), "stderr: {}", stderr(&add));

    let out = h.run(&["delete", "slowdelete"]);
    let se = stderr(&out);
    assert!(!out.status.success(), "stderr: {se}");
    assert!(se.contains("timed out after"), "{se}");

    let tunnels = fs::read_to_string(h.dir.path().join("config/tunnels.toml")).unwrap();
    assert!(tunnels.contains("name = \"slowdelete\""), "{tunnels}");
}

#[test]
fn stop_of_a_name_that_is_neither_tunnel_nor_run_is_not_found() {
    let h = Harness::new();
    let out = h.run(&["stop", "nope"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("Tunnel 'nope' not found for account 'demo'"),
        "{}",
        stderr(&out)
    );
}

// A relay block with placeholder values: enough to get past the no-relay
// refusal, so the checks after it are the ones under test
const RELAY_CONFIG: &str = r#"selected_account = "demo"

[[accounts]]
name = "demo"
gcloud_configuration = "work"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"

[accounts.expose]
relay_instance = "gtunnel-relay"
relay_zone = "europe-west1-b"
network = "default"
default_domain = "example.com"

[[accounts.expose.domains]]
domain = "example.com"
managed_zone = "example-zone"
"#;

// Every gcloud call is appended to a log beside the stub, so a test can say
// that none was made
const RECORDING_STUB: &str = r#"#!/bin/sh
echo "$@" >> "$(dirname "$0")/calls.log"
case "$1 $2" in
  "auth print-access-token")
    echo stub-token
    exit 0
    ;;
esac
exit 0
"#;

impl Harness {
    fn with_relay(&self) {
        fs::write(self.dir.path().join("config/config.toml"), RELAY_CONFIG).unwrap();
    }

    fn tunnels_toml(&self) -> String {
        fs::read_to_string(self.dir.path().join("config/tunnels.toml")).unwrap()
    }

    fn gcloud_calls(&self) -> String {
        fs::read_to_string(self.dir.path().join("bin/calls.log")).unwrap_or_default()
    }
}

// The login check asks gcloud for a token before any command body runs; what
// the refusal must precede is everything that touches the project
fn assert_only_login_calls(h: &Harness) {
    for call in h.gcloud_calls().lines() {
        assert!(
            call.starts_with("auth print-access-token"),
            "a GCP call before the refusal: {call}"
        );
    }
}

#[test]
fn run_public_with_no_relay_exits_2_with_the_same_message() {
    let h = Harness::new();
    let out = h.run(&["run", "web", "3000", "--public"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("Error: No relay for account 'demo'. Run `gtunnel relay up`"));
}

#[test]
fn expose_of_an_invalid_local_target_exits_2_before_any_gcp_call() {
    let h = Harness::with_stub(RECORDING_STUB);
    h.with_relay();
    let before = h.tunnels_toml();
    let out = h.run(&["expose", "web", "localhost:99999"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("Invalid local target 'localhost:99999'"));
    assert_only_login_calls(&h);
    assert_eq!(h.tunnels_toml(), before);
}

#[test]
fn run_public_of_an_invalid_local_target_exits_2_before_any_gcp_call() {
    let h = Harness::with_stub(RECORDING_STUB);
    h.with_relay();
    let before = h.tunnels_toml();
    let out = h.run(&["run", "web", "localhost:99999", "--public"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("Invalid local target 'localhost:99999'"));
    assert_only_login_calls(&h);
    assert_eq!(h.tunnels_toml(), before);
}
