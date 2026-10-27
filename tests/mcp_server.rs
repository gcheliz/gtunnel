use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

struct Harness {
    dir: TempDir,
}

const STUB: &str = r#"#!/bin/sh
exit 0
"#;

// Login passes, so a start gets as far as the job; only the tunnel itself fails
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
project = "example-dev-1a2b"
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
    fn new() -> Self {
        Self::with_stub(STUB)
    }

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

    fn run_mcp_pipe(&self, input: &str) -> (String, String, bool) {
        let root = self.dir.path();
        let path = format!(
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            root.join("bin").display()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_gtunnel"))
            .args(["mcp"])
            .env("PATH", path)
            .env("HOME", root)
            .env("GTUNNEL_CONFIG_DIR", root.join("config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gtunnel mcp");

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes()).unwrap();
            stdin.flush().unwrap();
        }

        let out = child.wait_with_output().expect("wait on mcp");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    }

    fn run(&self, args: &[&str]) -> (String, String, bool) {
        let root = self.dir.path();
        let path = format!(
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            root.join("bin").display()
        );
        let out = Command::new(env!("CARGO_BIN_EXE_gtunnel"))
            .args(args)
            .env("PATH", path)
            .env("HOME", root)
            .env("GTUNNEL_CONFIG_DIR", root.join("config"))
            .stdin(Stdio::null())
            .output()
            .expect("run gtunnel");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    }
}

#[test]
fn mcp_validate_flag_exits_successfully() {
    let h = Harness::new();
    let (stdout, stderr, success) = h.run(&["mcp", "--validate"]);
    assert!(success, "stderr: {stderr}");
    assert!(stdout.contains("Validated 10 tools and 2 resources successfully."));
}

#[test]
fn mcp_stdio_initialize_handshake() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(resp["result"]["serverInfo"]["name"], "gtunnel");
    assert!(resp["result"]["capabilities"]["tools"].is_object());
    assert!(resp["result"]["capabilities"]["resources"].is_object());
}

#[test]
fn mcp_stdio_tools_list_returns_ten_tools() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 10);

    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"gtunnel_list_tunnels"));
    assert!(tool_names.contains(&"gtunnel_start_tunnel"));
    assert!(tool_names.contains(&"gtunnel_stop_tunnel"));
    assert!(tool_names.contains(&"gtunnel_restart_tunnel"));
    assert!(tool_names.contains(&"gtunnel_get_status"));
    assert!(tool_names.contains(&"gtunnel_get_logs"));
    assert!(tool_names.contains(&"gtunnel_add_tunnel"));
    assert!(tool_names.contains(&"gtunnel_delete_tunnel"));
    assert!(tool_names.contains(&"gtunnel_list_accounts"));
    assert!(tool_names.contains(&"gtunnel_doctor"));
}

#[test]
fn mcp_stdio_tool_call_missing_required_param_returns_invalid_params() {
    let h = Harness::new();
    // gtunnel_start_tunnel requires "name"
    let req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"gtunnel_start_tunnel","arguments":{}}}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    assert_eq!(resp["id"], 3);
    assert!(resp["error"].is_object());
    assert_eq!(resp["error"]["code"], -32602); // INVALID_PARAMS
    assert!(resp["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Missing required parameter 'name'"));
}

#[test]
fn mcp_stdio_tool_call_invalid_param_type_returns_invalid_params() {
    let h = Harness::new();
    // name must be a string, pass integer instead
    let req = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"gtunnel_start_tunnel","arguments":{"name":12345}}}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    assert_eq!(resp["id"], 4);
    assert_eq!(resp["error"]["code"], -32602); // INVALID_PARAMS
    assert!(resp["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Parameter 'name' must be of type string"));
}

#[test]
fn mcp_stdio_tool_call_list_accounts_succeeds() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"gtunnel_list_accounts","arguments":{}}}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    assert_eq!(resp["id"], 5);
    assert!(resp["result"]["content"].is_array());
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["selected_account"], "demo");
    assert_eq!(parsed["accounts"][0]["name"], "demo");
}

#[test]
fn mcp_stdio_resources_list_and_read() {
    let h = Harness::new();
    let req_list = r#"{"jsonrpc":"2.0","id":6,"method":"resources/list"}"#;
    let req_read = r#"{"jsonrpc":"2.0","id":7,"method":"resources/read","params":{"uri":"gtunnel://accounts"}}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req_list}\n{req_read}\n"));
    assert!(success, "stderr: {stderr}");

    let mut lines = stdout.lines();
    let list_line = lines.next().expect("list line");
    let resp_list: serde_json::Value = serde_json::from_str(list_line).unwrap();
    let resources = resp_list["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 2);

    let read_line = lines.next().expect("read line");
    let resp_read: serde_json::Value = serde_json::from_str(read_line).unwrap();
    assert_eq!(resp_read["id"], 7);
    assert_eq!(
        resp_read["result"]["contents"][0]["uri"],
        "gtunnel://accounts"
    );
}

#[test]
fn mcp_stdio_unknown_method_returns_method_not_found() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":8,"method":"unsupported/method"}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    assert_eq!(resp["id"], 8);
    assert_eq!(resp["error"]["code"], -32601); // METHOD_NOT_FOUND
}

#[test]
fn mcp_stdio_parse_error_returns_parse_error() {
    let h = Harness::new();
    let malformed = "not valid json at all";
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{malformed}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    assert_eq!(resp["error"]["code"], -32700); // PARSE_ERROR
}

// Every line of stdout is a frame: a stray `println!` from a command core would
// break the client's parser, so one that is not a JSON-RPC 2.0 object fails here
fn frames(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"));
            assert_eq!(v["jsonrpc"], "2.0", "not a JSON-RPC 2.0 object: {line}");
            v
        })
        .collect()
}

#[test]
fn mutating_tools_write_only_json_rpc_to_stdout() {
    let h = Harness::new();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"gtunnel_add_tunnel","arguments":{"name":"k8s","target":"10.0.0.10:443","via":"bastion","start":false}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"gtunnel_stop_tunnel","arguments":{"name":"db"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"gtunnel_delete_tunnel","arguments":{"name":"k8s"}}}"#,
        "\n",
    );
    let (stdout, stderr, ok) = h.run_mcp_pipe(input);
    assert!(ok, "stderr: {stderr}");
    let frames = frames(&stdout);
    assert_eq!(frames.len(), 4, "{stdout}");
    for f in &frames[1..] {
        assert!(f.get("result").is_some(), "{f}");
        assert_ne!(f["result"]["isError"], serde_json::json!(true), "{f}");
    }
}

#[test]
fn a_tool_failure_carries_is_error_and_the_cause_chain() {
    let h = Harness::new();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_delete_tunnel","arguments":{"name":"nope"}}}"#,
        "\n",
    );
    let (stdout, _, _) = h.run_mcp_pipe(input);
    let frames = frames(&stdout);
    assert_eq!(
        frames[0]["result"]["isError"],
        serde_json::json!(true),
        "{stdout}"
    );
    let text = frames[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("nope"), "{text}");
}

// The job is a launchd one, so the test needs a macOS user session. Its tunnel
// is not `k8s`: tests run in parallel and a label is shared by name, so the
// other tests' delete of `k8s` would unload this job mid-start
#[cfg(target_os = "macos")]
#[test]
fn add_with_failed_start_keeps_the_record_and_reports_the_failure() {
    let h = Harness::failing_gcloud();
    let add = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_add_tunnel","arguments":{"name":"redis","target":"10.0.0.10:443","via":"bastion","start":true}}}"#,
        "\n",
    );
    let (stdout, _, _) = h.run_mcp_pipe(add);
    let tunnels = fs::read_to_string(h.dir.path().join("config/tunnels.toml")).unwrap();
    // The failed start leaves a loaded job in the user's launchd domain
    let del = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_delete_tunnel","arguments":{"name":"redis"}}}"#,
        "\n",
    );
    h.run_mcp_pipe(del);

    let frames = frames(&stdout);
    assert_eq!(
        frames[0]["result"]["isError"],
        serde_json::json!(true),
        "{stdout}"
    );
    let text = frames[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("redis failed to start"), "{text}");
    assert!(tunnels.contains("name = \"redis\""), "{tunnels}");
}
