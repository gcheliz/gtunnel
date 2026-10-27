use std::fs;
use std::process::{Command, Output, Stdio};

// A config with two tunnels on the selected account and one on another, so a
// completion that ignored the account would show up as a third name
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

[[tunnels]]
name = "cache"
account_name = "demo"
auto_start = false

[tunnels.spec]
mode = "access"
remote_port = 6379
local_port = 16379

[tunnels.spec.target]
kind = "instance"
instance = "redis-1"
zone = "europe-west1-b"

[[tunnels]]
name = "elsewhere"
account_name = "other"
auto_start = false

[tunnels.spec]
mode = "access"
remote_port = 22
local_port = 10022

[tunnels.spec.target]
kind = "instance"
instance = "bastion"
zone = "europe-west1-b"
"#;

// `config` is None for a machine on which `gtunnel init` has never run
fn run(config: Option<(&str, &str)>, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("config");
    fs::create_dir_all(&root).unwrap();
    if let Some((cfg, tunnels)) = config {
        fs::write(root.join("config.toml"), cfg).unwrap();
        fs::write(root.join("tunnels.toml"), tunnels).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gtunnel"));
    cmd.args(args)
        .env("HOME", dir.path())
        .env("GTUNNEL_CONFIG_DIR", &root)
        .env("GTUNNEL_NO_NOTIFY", "1")
        // A gcloud lookup would fail here: completion must not reach for one
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("run gtunnel")
}

// The engine lists the global flags after the values on an empty word, so a
// test of names looks past those
fn names_in(stdout: &[u8]) -> Vec<String> {
    text(stdout)
        .lines()
        .map(|l| l.split('\t').next().unwrap_or("").to_string())
        .filter(|n| !n.starts_with('-'))
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_completion_script_is_printed_for_each_shell_with_no_config() {
    for shell in ["bash", "zsh", "fish"] {
        let out = run(None, &[], &["completions", shell]);
        assert_eq!(out.status.code(), Some(0), "{shell}: {}", text(&out.stderr));
        let script = text(&out.stdout);
        assert!(script.contains("gtunnel"), "{shell}: {script}");
        assert!(out.stderr.is_empty(), "{shell}: {}", text(&out.stderr));
    }
}

#[test]
fn an_unknown_shell_is_refused() {
    let out = run(None, &[], &["completions", "tcsh"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn a_tunnel_name_position_completes_to_the_names_of_the_selected_account() {
    for sub in [
        "start",
        "stop",
        "restart",
        "delete",
        "logs",
        "rename",
        "autostart",
    ] {
        let out = run(
            Some((CONFIG, TUNNELS)),
            &[("COMPLETE", "fish")],
            &["--", "gtunnel", sub, ""],
        );
        assert_eq!(out.status.code(), Some(0), "{sub}: {}", text(&out.stderr));
        assert_eq!(names_in(&out.stdout), ["cache", "db"], "{sub}");
    }
}

#[test]
fn a_name_that_is_being_typed_narrows_the_candidates() {
    let out = run(
        Some((CONFIG, TUNNELS)),
        &[("COMPLETE", "fish")],
        &["--", "gtunnel", "start", "d"],
    );
    assert_eq!(text(&out.stdout).trim(), "db");
}

#[test]
fn name_completion_with_no_config_offers_no_names_and_succeeds() {
    let out = run(
        None,
        &[("COMPLETE", "fish")],
        &["--", "gtunnel", "start", ""],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(names_in(&out.stdout).is_empty(), "{}", text(&out.stdout));
}
