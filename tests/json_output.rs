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
