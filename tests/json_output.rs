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
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();

        let gcloud = root.join("bin/gcloud");
        fs::write(&gcloud, STUB).unwrap();
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
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            root.join("bin").display()
        );
        Command::new(env!("CARGO_BIN_EXE_gtunnel"))
            .args(args)
            .env("PATH", path)
            .env("HOME", root)
            .env("GTUNNEL_CONFIG_DIR", root.join("config"))
            .env("GTUNNEL_NO_NOTIFY", "1")
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

// A run record as `gtunnel run` writes it. The pid decides whether the run
// counts as live, so the caller picks one that is or is not.
fn write_run_record(h: &Harness, name: &str, pid: u32, ttl_secs: Option<u64>) {
    let dir = h.dir.path().join("config/run");
    fs::create_dir_all(&dir).unwrap();
    let ttl = ttl_secs
        .map(|t| format!(r#","ttl_secs":{t}"#))
        .unwrap_or_default();
    fs::write(
        dir.join(format!("demo-{name}.json")),
        format!(
            r#"{{"name":"{name}","account_name":"demo","pid":{pid},"started_at":1700000000,
"spec":{{"mode":"access","target":{{"kind":"instance","instance":"my-vm","zone":"europe-west1-b"}},"remote_port":5432,"local_port":15999}}{ttl}}}"#
        ),
    )
    .unwrap();
}

// Orphaned so it is a live pid that is not this test process
fn live_pid() -> (std::process::Child, u32) {
    let child = Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    (child, pid)
}

#[test]
fn a_live_run_is_listed_in_json_with_its_expiry() {
    let h = Harness::new();
    let (mut sleeper, pid) = live_pid();
    write_run_record(&h, "scratch", pid, Some(120));
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
    assert_eq!(run["expires_at"], 1_700_000_120u64);
    let declared = arr.iter().find(|r| r["name"] == "db").expect("declared");
    assert_eq!(declared["run"], false, "the schema stays uniform");
    assert!(declared.get("expires_at").is_none());
}

#[test]
fn a_run_without_a_ttl_has_no_expires_at() {
    let h = Harness::new();
    let (mut sleeper, pid) = live_pid();
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
    let (mut sleeper, pid) = live_pid();
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
    let (mut sleeper, pid) = live_pid();
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
