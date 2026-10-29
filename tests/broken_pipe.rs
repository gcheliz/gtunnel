use std::fs;
use std::process::{Command, Output, Stdio};

// An account and no tunnels, so `list` reaches its one line of output without
// gcloud, launchd or a network
const CONFIG: &str = r#"selected_account = "demo"

[[accounts]]
name = "demo"
gcloud_configuration = "work"
project = "demo-project"
default_zone = "europe-west1-b"
"#;

// Stdout is a pipe whose reader closed before the process started, so the
// first write gets EPIPE every time. A reader that stops early (`head -1`, a
// pager quit with `q`) is the same pipe, only with a race over who goes first.
fn run_into_closed_pipe(config: Option<&str>, args: &[&str]) -> Output {
    run_into_closed_pipe_with(config, args, None)
}

fn run_into_closed_pipe_with(config: Option<&str>, args: &[&str], input: Option<&str>) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("gtunnel");
    fs::create_dir_all(&root).unwrap();
    if let Some(config) = config {
        fs::write(root.join("config.toml"), config).unwrap();
    }
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = Command::new(env!("CARGO_BIN_EXE_gtunnel"))
        .args(args)
        .env("HOME", dir.path())
        .env("GTUNNEL_CONFIG_DIR", &root)
        .env("GTUNNEL_NO_NOTIFY", "1")
        .env("PATH", "/usr/bin:/bin")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        use std::io::Write;
        // Dropped at the end of the statement, so the server sees end of input
        // if it ever gets past the write that fails
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

fn assert_quiet(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(out.status.success(), "{:?}: {stderr}", out.status);
}

#[test]
fn a_completion_script_into_a_closed_pipe_ends_quietly() {
    assert_quiet(&run_into_closed_pipe(None, &["completions", "zsh"]));
}

#[test]
fn a_list_into_a_closed_pipe_ends_quietly() {
    assert_quiet(&run_into_closed_pipe(Some(CONFIG), &["list"]));
}

// An MCP client that closed its end has left, which ends the session
#[test]
fn an_mcp_reply_into_a_closed_pipe_ends_quietly() {
    let ping = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
    assert_quiet(&run_into_closed_pipe_with(None, &["mcp"], Some(ping)));
}
