mod common;

use std::fs;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

struct Harness {
    dir: TempDir,
}

const STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token")
    echo stub-token
    exit 0
    ;;
esac
exit 0
"#;

const CONFIG: &str = r#"selected_account = "demo"

[[accounts]]
name = "demo"
gcloud_configuration = "work"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"

[[accounts]]
name = "prod"
gcloud_configuration = "prod-work"
project = "example-prod-3c4d"
default_zone = "europe-west1-c"
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
    fn new() -> Self {
        Self::with_stub(STUB)
    }

    fn with_stub(stub: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();

        let gcloud = root.join("bin/gcloud");
        fs::write(&gcloud, stub).unwrap();
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
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
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
            .env("GTUNNEL_NO_NOTIFY", "1")
            .env("GTUNNEL_NO_DISCOVERY", "1")
            .envs(env.iter().copied())
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
fn list_json_outputs_valid_array_of_tunnels() {
    let h = Harness::new();
    let out = h.run(&["list", "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let val: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout should be valid json");
    assert!(val.is_array(), "expected json array: {val:?}");
    let arr = val.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "db");
    assert_eq!(arr[0]["account"], "demo");
    assert_eq!(arr[0]["mode"], "access");
    assert_eq!(arr[0]["target"], "postgres-1:5432 (europe-west1-b)");
    assert_eq!(arr[0]["auto_start"], false);
    assert_eq!(arr[0]["unmanaged"], false);
}

#[test]
fn account_list_json_outputs_valid_array_of_accounts() {
    let h = Harness::new();
    let out = h.run(&["account", "list", "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let val: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout should be valid json");
    assert!(val.is_array(), "expected json array: {val:?}");
    let arr = val.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "demo");
    assert_eq!(arr[0]["project"], "example-dev-1a2b");
    assert_eq!(arr[0]["selected"], true);
    assert_eq!(arr[1]["name"], "prod");
    assert_eq!(arr[1]["project"], "example-prod-3c4d");
    assert_eq!(arr[1]["selected"], false);
}

#[test]
fn doctor_json_outputs_valid_array_of_findings() {
    let h = Harness::new();
    let out = h.run(&["doctor", "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let val: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout should be valid json");
    assert!(val.is_array(), "expected json array: {val:?}");
}

// Started now, and the start time is returned, for the assertions on expiry
fn write_run_record(h: &Harness, name: &str, pid: u32, ttl_secs: Option<u64>) -> u64 {
    write_run_record_on(h, name, pid, ttl_secs, 15999)
}

fn write_run_record_on(h: &Harness, name: &str, pid: u32, ttl_secs: Option<u64>, port: u16) -> u64 {
    let started_at = common::unix_now();
    let spec = format!(
        r#"{{"mode":"access","target":{{"kind":"instance","instance":"my-vm","zone":"europe-west1-b"}},"remote_port":5432,"local_port":{port}}}"#
    );
    common::write_run_record(h.dir.path(), "demo", name, pid, started_at, ttl_secs, &spec);
    started_at
}

#[test]
fn a_live_run_is_listed_in_json_with_its_expiry() {
    let h = Harness::new();
    let mut sleeper = common::sleeper();
    let pid = sleeper.id();
    let started_at = write_run_record(&h, "scratch", pid, Some(120));
    let out = h.run(&["list", "--json"]);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let val: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    let arr = val.as_array().unwrap();
    assert_eq!(arr.len(), 2, "{val:?}");
    let run = arr
        .iter()
        .find(|r| r["name"] == "scratch")
        .expect("the run");
    assert_eq!(run["run"], true);
    assert_eq!(run["mode"], "access", "a run keeps the mode it really is");
    assert_eq!(run["status"], "running");
    assert_eq!(run["url"], "localhost:15999");
    assert_eq!(run["target"], "my-vm:5432 (europe-west1-b)");
    assert_eq!(run["expires_at"], started_at + 120);
    let declared = arr.iter().find(|r| r["name"] == "db").expect("declared");
    assert_eq!(declared["run"], false, "the schema stays uniform");
    assert!(declared.get("expires_at").is_none());
}

#[test]
fn a_run_without_a_ttl_has_no_expires_at() {
    let h = Harness::new();
    let mut sleeper = common::sleeper();
    let pid = sleeper.id();
    write_run_record(&h, "scratch", pid, None);
    let out = h.run(&["list", "--json"]);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let val: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    let run = val
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "scratch")
        .expect("the run");
    assert_eq!(run["run"], true);
    assert!(run.get("expires_at").is_none(), "{run:?}");
}

#[test]
fn a_dead_run_record_is_not_listed() {
    let h = Harness::new();
    write_run_record(&h, "scratch", 999_999, Some(120));
    let out = h.run(&["list", "--json"]);
    let val: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    assert_eq!(val.as_array().unwrap().len(), 1, "{val:?}");
    let text = stdout(&h.run(&["list"]));
    assert!(!text.contains("scratch"), "{text}");
}

#[test]
fn a_live_run_is_listed_in_the_text_table() {
    let h = Harness::new();
    let mut sleeper = common::sleeper();
    let pid = sleeper.id();
    write_run_record(&h, "scratch", pid, None);
    let out = h.run(&["list"]);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let text = stdout(&out);
    let row = text.lines().find(|l| l.contains("scratch")).expect(&text);
    assert!(row.contains("running"), "{row}");
    assert!(row.contains(" access "), "{row}");
    assert!(row.trim_end().ends_with("[run]"), "no TTL, no time: {row}");
    assert!(row.contains("localhost:15999"), "{row}");
    assert!(row.contains("my-vm:5432"), "{row}");
}

#[test]
fn a_live_run_with_a_ttl_says_when_it_stops() {
    let h = Harness::new();
    let mut sleeper = common::sleeper();
    let pid = sleeper.id();
    write_run_record(&h, "scratch", pid, Some(120));
    let out = h.run(&["list"]);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let text = stdout(&out);
    let row = text.lines().find(|l| l.contains("scratch")).expect(&text);
    let tail = row
        .rsplit_once("[run, stops at ")
        .map(|(_, t)| t)
        .expect(row);
    let bytes = tail.as_bytes();
    assert_eq!(tail.len(), 6, "HH:MM and the bracket: {tail:?}");
    assert!(bytes[0].is_ascii_digit() && bytes[1].is_ascii_digit() && bytes[2] == b':');
    assert!(bytes[3].is_ascii_digit() && bytes[4].is_ascii_digit() && bytes[5] == b']');
}

// A logged-out gcloud, and one whose every other call fails
const STUB_LOGGED_OUT: &str = r#"#!/bin/sh
echo "ERROR: you do not currently have an active account selected" >&2
exit 1
"#;

const STUB_BROKEN: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token") echo stub-token; exit 0 ;;
esac
echo "ERROR: quota exceeded" >&2
exit 1
"#;

// The JSON error object of a failed command, with its exit code
fn json_error(out: &Output) -> (String, String, i32) {
    let val: serde_json::Value = serde_json::from_str(&stdout(out))
        .unwrap_or_else(|e| panic!("stdout should be json ({e}): {}", stdout(out)));
    let err = val.get("error").expect("an error object");
    assert_eq!(
        err.as_object().unwrap().len(),
        2,
        "only kind and message: {err}"
    );
    (
        err["kind"].as_str().expect("kind").to_string(),
        err["message"].as_str().expect("message").to_string(),
        out.status.code().expect("an exit code"),
    )
}

#[test]
fn an_unknown_account_under_json_is_not_found_with_exit_3() {
    let h = Harness::new();
    let out = h.run(&["--account", "nope", "list", "--json"]);
    let (kind, message, code) = json_error(&out);
    assert_eq!(kind, "not_found");
    assert_eq!(message, "Account 'nope' not found");
    assert_eq!(code, 3);
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
}

#[test]
fn account_list_and_doctor_json_report_an_unknown_account_the_same_way() {
    let h = Harness::new();
    let out = h.run(&["--account", "nope", "doctor", "--json"]);
    let (kind, _, code) = json_error(&out);
    assert_eq!((kind.as_str(), code), ("not_found", 3));
}

#[test]
fn an_unknown_tunnel_without_json_is_prose_on_stderr_with_exit_3() {
    let h = Harness::new();
    for verb in ["start", "stop"] {
        let out = h.run(&[verb, "ghost"]);
        assert_eq!(out.status.code(), Some(3), "{verb}: {}", stderr(&out));
        assert!(stdout(&out).is_empty(), "{verb}: {}", stdout(&out));
        assert!(
            stderr(&out).contains("Tunnel 'ghost' not found for account 'demo'"),
            "{verb}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn a_bad_log_level_under_json_is_a_usage_error_with_exit_2() {
    let h = Harness::new();
    let out = h.run_env(&["list", "--json"], &[("GTUNNEL_LOG_LEVEL", "loud")]);
    let (kind, message, code) = json_error(&out);
    assert_eq!(kind, "usage");
    assert!(message.starts_with("GTUNNEL_LOG_LEVEL: "), "{message}");
    assert_eq!(code, 2);
}

#[test]
fn a_bad_target_is_a_usage_error_with_exit_2() {
    let h = Harness::new();
    let out = h.run(&["add", "x", "no-port"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("expected INSTANCE:PORT"));
}

#[test]
fn list_under_json_works_with_a_logged_out_gcloud() {
    let h = Harness::with_stub(STUB_LOGGED_OUT);
    let out = h.run(&["list", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let val: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout should be json ({e}): {}", stdout(&out)));
    assert!(val.is_array(), "{val}");
}

#[test]
fn a_failing_gcloud_call_is_a_gcloud_error_with_exit_5() {
    let h = Harness::with_stub(STUB_BROKEN);
    let out = h.run(&["init"]);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(stderr(&out).contains("quota exceeded"), "{}", stderr(&out));
}

#[test]
fn an_unreadable_config_under_json_is_internal_with_exit_1() {
    let h = Harness::new();
    fs::write(h.dir.path().join("config/config.toml"), "not = [valid").unwrap();
    let out = h.run(&["list", "--json"]);
    let (kind, _, code) = json_error(&out);
    assert_eq!((kind.as_str(), code), ("internal", 1));
}

#[test]
fn a_successful_json_command_still_exits_0_with_no_error_object() {
    let h = Harness::new();
    let out = h.run(&["list", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!stdout(&out).contains("\"error\""));
}

/// A local port that answers every connection with a byte, the way a tunnel
/// with a live service behind it does, so a probe of it succeeds without a
/// real `start-iap-tunnel` on the machine.
fn answering_port() -> u16 {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.write_all(b"x");
        }
    });
    port
}

#[test]
fn a_running_run_carries_its_local_port_and_the_probes_health() {
    let h = Harness::new();
    let port = answering_port();
    let mut sleeper = common::sleeper();
    let pid = sleeper.id();
    write_run_record_on(&h, "scratch", pid, None, port);
    let out = h.run(&["list", "--json"]);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let val: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    let run = val
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "scratch")
        .expect("the run");
    assert_eq!(run["local_port"], port, "{run}");
    assert_eq!(run["health"], "healthy", "{run}");
}

#[test]
fn a_stopped_tunnel_carries_its_local_port_and_stopped_health() {
    let h = Harness::new();
    let out = h.run(&["list", "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let val: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("json");
    let db = &val.as_array().unwrap()[0];
    assert_eq!(db["status"], "stopped", "{db}");
    assert_eq!(db["local_port"], 15432, "{db}");
    assert_eq!(db["health"], "stopped", "{db}");
}
