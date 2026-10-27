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

const TWO_ACCOUNTS: &str = r#"selected_account = "demo"

[[accounts]]
name = "demo"
gcloud_configuration = "work"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"

[[accounts]]
name = "other"
gcloud_configuration = "other"
project = "example-prod-3c4d"
default_zone = "europe-west1-b"
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

    #[cfg(target_os = "macos")]
    fn failing_gcloud() -> Self {
        Self::with_stub(FAILING_STUB)
    }

    // Two accounts, neither logged in: a move has a destination to be refused by
    fn logged_out() -> Self {
        Self::build(LOGGED_OUT_STUB, TWO_ACCOUNTS)
    }

    fn with_stub(stub: &str) -> Self {
        Self::build(stub, CONFIG)
    }

    fn build(stub: &str, config: &str) -> Self {
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

        fs::write(root.join("config/config.toml"), config).unwrap();
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
            .env("GTUNNEL_NO_NOTIFY", "1")
            .env("GTUNNEL_NO_DISCOVERY", "1")
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
            .env("GTUNNEL_NO_NOTIFY", "1")
            .env("GTUNNEL_NO_DISCOVERY", "1")
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
    assert!(stdout.contains("Validated 14 tools and 2 resources successfully."));
}

#[test]
fn mcp_stdio_initialize_handshake() {
    let h = Harness::new();
    for version in ["2024-11-05", "2025-06-18", "2025-11-25"] {
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{version}"}}}}"#
        );
        let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
        assert!(success, "stderr: {stderr}");

        let line = stdout.lines().next().expect("response line");
        let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], version);
        assert_eq!(resp["result"]["serverInfo"]["name"], "gtunnel");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert!(resp["result"]["capabilities"]["resources"].is_object());
    }
}

#[test]
fn initialize_without_params_answers_the_latest() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    assert_eq!(resp["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn initialize_from_a_newer_client_answers_the_latest() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28"}}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    assert_eq!(resp["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn mcp_stdio_tools_list_returns_fourteen_tools() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 14);

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
    assert!(tool_names.contains(&"gtunnel_run_tunnel"));
    assert!(tool_names.contains(&"gtunnel_rename_tunnel"));
    assert!(tool_names.contains(&"gtunnel_move_tunnel"));
    assert!(tool_names.contains(&"gtunnel_expose_tunnel"));
}

fn listed_tools() -> Vec<serde_json::Value> {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");
    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    resp["result"]["tools"].as_array().expect("tools").clone()
}

#[test]
fn every_tool_carries_a_title_and_all_four_hints() {
    for tool in listed_tools() {
        let name = tool["name"].as_str().unwrap();
        let title = tool["title"].as_str().unwrap_or("");
        assert!(!title.is_empty(), "{name} has no title");
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(
                tool["annotations"][hint].is_boolean(),
                "{name} lacks boolean {hint}"
            );
        }
    }
}

#[test]
fn read_only_and_destructive_tools_say_so() {
    let read_only = [
        "gtunnel_list_tunnels",
        "gtunnel_get_status",
        "gtunnel_get_logs",
        "gtunnel_list_accounts",
        "gtunnel_doctor",
    ];
    let destructive = [
        "gtunnel_stop_tunnel",
        "gtunnel_restart_tunnel",
        "gtunnel_delete_tunnel",
        "gtunnel_rename_tunnel",
        "gtunnel_move_tunnel",
        "gtunnel_expose_tunnel",
    ];
    for tool in listed_tools() {
        let name = tool["name"].as_str().unwrap();
        let a = &tool["annotations"];
        assert_eq!(
            a["readOnlyHint"].as_bool().unwrap(),
            read_only.contains(&name),
            "readOnlyHint of {name}"
        );
        assert_eq!(
            a["destructiveHint"].as_bool().unwrap(),
            destructive.contains(&name),
            "destructiveHint of {name}"
        );
    }
}

// The hints are four positional bools, so a swap of the last two compiles and
// passes every other test; this pins each tool to its row
#[test]
fn every_tool_carries_its_idempotent_and_open_world_hints() {
    let expected = [
        ("gtunnel_list_tunnels", true, false),
        ("gtunnel_get_status", true, false),
        ("gtunnel_get_logs", true, false),
        ("gtunnel_list_accounts", true, false),
        ("gtunnel_doctor", true, true),
        ("gtunnel_start_tunnel", true, true),
        ("gtunnel_stop_tunnel", true, false),
        ("gtunnel_restart_tunnel", true, true),
        ("gtunnel_add_tunnel", false, true),
        ("gtunnel_run_tunnel", false, true),
        ("gtunnel_delete_tunnel", false, false),
        ("gtunnel_rename_tunnel", false, true),
        ("gtunnel_move_tunnel", false, true),
        ("gtunnel_expose_tunnel", false, true),
    ];
    let tools = listed_tools();
    for (name, idempotent, open_world) in expected {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        let a = &tool["annotations"];
        assert_eq!(a["idempotentHint"], idempotent, "idempotentHint of {name}");
        assert_eq!(a["openWorldHint"], open_world, "openWorldHint of {name}");
    }
}

/// `tools/list` and one `tools/call` in a single process, so the schema the
/// server advertised is the one its own answer is held to.
fn call_with_schema(
    h: &Harness,
    tool: &str,
    args: serde_json::Value,
) -> (Option<serde_json::Value>, serde_json::Value) {
    let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": tool, "arguments": args},
    });
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{list}\n{call}\n"));
    assert!(success, "stderr: {stderr}");
    let mut lines = stdout.lines();
    let listed: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let called: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let schema = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == tool)
        .and_then(|t| t.get("outputSchema").cloned());
    (schema, called["result"].clone())
}

fn assert_conforms(schema: &Option<serde_json::Value>, result: &serde_json::Value) {
    let schema = schema.as_ref().expect("the tool declares an outputSchema");
    let validator = jsonschema::validator_for(schema).expect("a valid schema");
    let structured = &result["structuredContent"];
    assert!(
        validator.is_valid(structured),
        "structuredContent does not match its outputSchema: {structured}"
    );
}

#[test]
fn list_accounts_structured_output_matches_its_schema() {
    let h = Harness::new();
    let (schema, result) = call_with_schema(&h, "gtunnel_list_accounts", serde_json::json!({}));
    assert_conforms(&schema, &result);
    assert_eq!(result["structuredContent"]["selected_account"], "demo");
}

#[test]
fn list_tunnels_structured_output_matches_its_schema() {
    let h = Harness::new();
    let (schema, result) = call_with_schema(&h, "gtunnel_list_tunnels", serde_json::json!({}));
    assert_conforms(&schema, &result);
    assert_eq!(result["structuredContent"]["tunnels"][0]["name"], "db");
}

#[test]
fn list_tunnels_with_no_tunnels_is_an_empty_array() {
    let h = Harness::new();
    fs::write(h.dir.path().join("config/tunnels.toml"), "").unwrap();
    let (schema, result) = call_with_schema(&h, "gtunnel_list_tunnels", serde_json::json!({}));
    assert_conforms(&schema, &result);
    assert_eq!(
        result["structuredContent"],
        serde_json::json!({"tunnels": []})
    );
}

#[test]
fn get_status_of_a_stopped_tunnel_matches_its_schema() {
    let h = Harness::new();
    let (schema, result) =
        call_with_schema(&h, "gtunnel_get_status", serde_json::json!({"name": "db"}));
    assert_conforms(&schema, &result);
    assert!(result["structuredContent"]["probe_latency_ms"].is_null());
}

#[test]
fn doctor_structured_output_matches_its_schema() {
    let h = Harness::new();
    let (schema, result) = call_with_schema(&h, "gtunnel_doctor", serde_json::json!({}));
    assert_conforms(&schema, &result);
    assert!(result["structuredContent"]["findings"].is_array());
}

#[test]
fn a_failed_json_tool_still_matches_its_schema() {
    let h = Harness::new();
    let (schema, result) = call_with_schema(
        &h,
        "gtunnel_list_tunnels",
        serde_json::json!({"account": "nope"}),
    );
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"]["kind"], "not_found");
    assert_conforms(&schema, &result);
}

#[test]
fn json_tool_text_is_unchanged() {
    let h = Harness::new();
    let (_, result) = call_with_schema(&h, "gtunnel_list_tunnels", serde_json::json!({}));
    let text = result["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(parsed.is_array(), "the text stays the bare array: {text}");
    // Byte for byte what the text was before structured output: the row's own
    // field order, which a re-serialised `Value` would sort away
    let expected = r#"[
  {
    "status": "stopped",
    "name": "db",
    "mode": "access",
    "url": "localhost:15432",
    "target": "postgres-1:5432 (europe-west1-b)",
    "account": "demo",
    "auto_start": false,
    "unmanaged": false,
    "run": false,
    "local_port": 15432,
    "health": "unknown"
  }
]"#;
    assert_eq!(text, expected);
}

#[test]
fn text_tools_declare_no_output_schema() {
    for tool in listed_tools() {
        let name = tool["name"].as_str().unwrap();
        let text_only = [
            "gtunnel_start_tunnel",
            "gtunnel_stop_tunnel",
            "gtunnel_restart_tunnel",
            "gtunnel_get_logs",
            "gtunnel_add_tunnel",
            "gtunnel_delete_tunnel",
            "gtunnel_rename_tunnel",
            "gtunnel_move_tunnel",
        ];
        assert_eq!(
            tool.get("outputSchema").is_some(),
            !text_only.contains(&name),
            "outputSchema presence of {name}"
        );
    }
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
fn the_accounts_resource_is_the_list_accounts_payload() {
    let h = Harness::new();
    // An impersonated account with a relay: the two fields the resource served
    // before the two paths shared one payload must not be lost on the way.
    fs::write(
        h.dir.path().join("config/config.toml"),
        format!(
            "{CONFIG}impersonate_service_account = \"sa@example-dev-1a2b.iam.gserviceaccount.com\"\n\n\
             [accounts.expose]\n\
             relay_instance = \"relay\"\n\
             relay_zone = \"europe-west1-b\"\n\
             network = \"default\"\n\
             default_domain = \"example.internal\"\n"
        ),
    )
    .unwrap();
    let tool = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_list_accounts","arguments":{}}}"#;
    let read = r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"gtunnel://accounts"}}"#;
    let list = r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#;
    let (stdout, stderr, ok) = h.run_mcp_pipe(&format!("{tool}\n{read}\n{list}\n"));
    assert!(ok, "stderr: {stderr}");
    let f = frames(&stdout);
    let from_tool: serde_json::Value =
        serde_json::from_str(f[0]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let from_resource: serde_json::Value =
        serde_json::from_str(f[1]["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(from_resource, from_tool);
    assert_eq!(from_resource["selected_account"], "demo");
    assert_eq!(from_resource["accounts"][0]["selected"], true);
    assert_eq!(
        from_resource["accounts"][0]["impersonate_service_account"],
        "sa@example-dev-1a2b.iam.gserviceaccount.com"
    );
    assert_eq!(
        from_resource["accounts"][0]["expose"]["relay_instance"],
        "relay"
    );
    let desc = f[2]["result"]["resources"][1]["description"]
        .as_str()
        .unwrap();
    assert!(desc.contains("gtunnel_list_accounts"), "{desc}");
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

#[test]
fn add_with_an_instance_that_reads_as_a_flag_is_invalid_params() {
    let h = Harness::new();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_add_tunnel","arguments":{"name":"bad","target":"10.0.0.10:22","via":"--command=id"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"gtunnel_add_tunnel","arguments":{"name":"bad","target":"-oProxyCommand=id:22","via":"bastion"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"gtunnel_run_tunnel","arguments":{"target":"--command=id:22","ttl_minutes":5}}}"#,
        "\n",
    );
    let (stdout, _, _) = h.run_mcp_pipe(input);
    let frames = frames(&stdout);
    assert_eq!(frames.len(), 3, "{stdout}");
    for f in &frames {
        assert_eq!(f["error"]["code"], -32602, "{f}");
    }
    let msg = frames[0]["error"]["message"].as_str().unwrap();
    assert!(msg.contains("--command=id"), "{msg}");
    let tunnels = fs::read_to_string(h.dir.path().join("config/tunnels.toml")).unwrap();
    assert!(!tunnels.contains("bad"), "{tunnels}");
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

#[test]
fn mcp_list_tunnels_includes_a_live_run_and_marks_it() {
    let h = Harness::new();
    let mut sleeper = Command::new("/bin/sleep").arg("30").spawn().expect("sleep");
    write_run_record(&h, "demo", "scratch", sleeper.id(), ACCESS_SPEC);
    let req = r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"gtunnel_list_tunnels","arguments":{}}}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert!(success, "stderr: {stderr}");
    let resp: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("line")).expect("json");
    let text = resp["result"]["content"][0]["text"].as_str().expect("text");
    let rows: serde_json::Value = serde_json::from_str(text).expect("rows");
    let run = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "scratch")
        .expect("the run is listed");
    assert_eq!(run["run"], true);
    assert_eq!(run["mode"], "access");
    assert_eq!(run["status"], "running");
    assert_eq!(run["expires_at"], 1_700_000_300u64);
    let declared = rows.as_array().unwrap().iter().find(|r| r["name"] == "db");
    assert_eq!(declared.map(|r| r["run"].clone()), Some(false.into()));
}

/// The keys of each row, by name, from a JSON array of list rows.
fn keys_by_name(rows: &serde_json::Value) -> std::collections::BTreeMap<String, Vec<String>> {
    rows.as_array()
        .expect("an array of rows")
        .iter()
        .map(|r| {
            let obj = r.as_object().expect("a row object");
            let mut keys: Vec<String> = obj.keys().cloned().collect();
            keys.sort();
            (r["name"].as_str().unwrap().to_string(), keys)
        })
        .collect()
}

#[test]
fn list_json_and_mcp_list_tunnels_have_the_same_keys_for_a_tunnel_and_a_run() {
    let h = Harness::new();
    let mut sleeper = Command::new("/bin/sleep").arg("30").spawn().expect("sleep");
    write_run_record(&h, "demo", "scratch", sleeper.id(), ACCESS_SPEC);
    let mcp = json_text(&call(&h, "gtunnel_list_tunnels", "{}"));
    let (cli, stderr, success) = h.run(&["list", "--json"]);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert!(success, "stderr: {stderr}");
    let cli: serde_json::Value = serde_json::from_str(&cli).expect("list --json");

    let (cli, mcp) = (keys_by_name(&cli), keys_by_name(&mcp));
    assert_eq!(cli.len(), 2, "{cli:?}");
    assert_eq!(cli, mcp);
    for keys in cli.values() {
        assert!(keys.contains(&"health".to_string()), "{keys:?}");
        assert!(keys.contains(&"local_port".to_string()), "{keys:?}");
        assert!(keys.contains(&"unmanaged".to_string()), "{keys:?}");
    }
}

#[test]
fn mcp_list_tunnels_gives_a_public_run_no_local_port() {
    let h = Harness::new();
    let mut sleeper = Command::new("/bin/sleep").arg("30").spawn().expect("sleep");
    write_run_record(&h, "demo", "public", sleeper.id(), EXPOSE_SPEC);
    let rows = json_text(&call(&h, "gtunnel_list_tunnels", "{}"));
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let run = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "public")
        .expect("the public run");
    assert_eq!(run["mode"], "public", "{run}");
    assert!(run["local_port"].is_null(), "{run}");
    assert_eq!(run["health"], "unknown", "{run}");
}

const ACCESS_SPEC: &str = r#"{"mode":"access","target":{"kind":"instance","instance":"my-vm","zone":"europe-west1-b"},"remote_port":5432,"local_port":15999}"#;

const EXPOSE_SPEC: &str = r#"{"mode":"expose","local_target":"localhost:8080","domain":"example.com","hostname":"app.example.com","relay_port":9000,"private":false}"#;

/// A run record as `gtunnel run` writes it, for a pid the test owns. No start
/// token, so liveness is judged by the pid alone, as for a record written
/// before the token existed.
fn write_run_record(h: &Harness, account: &str, name: &str, pid: u32, spec_json: &str) {
    let dir = h.dir.path().join("config/run");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{account}-{name}.json")),
        format!(
            r#"{{"name":"{name}","account_name":"{account}","pid":{pid},"started_at":1700000000,"ttl_secs":300,"spec":{spec_json}}}"#
        ),
    )
    .unwrap();
}

/// Send one `tools/call` and return its `result`.
fn call(h: &Harness, tool: &str, args: &str) -> serde_json::Value {
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{tool}","arguments":{args}}}}}"#
    );
    let (stdout, stderr, _) = h.run_mcp_pipe(&format!("{req}\n"));
    let f = frames(&stdout)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no reply; stderr: {stderr}"));
    f["result"].clone()
}

/// The text of a tool result, parsed as JSON.
fn json_text(result: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(result["content"][0]["text"].as_str().expect("text"))
        .unwrap_or_else(|e| panic!("not JSON ({e}): {result}"))
}

fn kind(result: &serde_json::Value) -> &serde_json::Value {
    &result["structuredContent"]["error"]["kind"]
}

// Login passes; any other call prints one line and stays up like a tunnel
const RUN_STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token") echo stub-token; exit 0 ;;
esac
echo "Testing if tunnel connection works."
exec /bin/sleep 60
"#;

/// Launch `name` with gtunnel_run_tunnel and return its record as JSON. The
/// one-minute TTL is the backstop if a test dies before it stops the run.
fn launch_run(h: &Harness, name: &str) -> serde_json::Value {
    let result = call(
        h,
        "gtunnel_run_tunnel",
        &format!(r#"{{"name":"{name}","target":"my-vm:5432","ttl_minutes":1}}"#),
    );
    assert_ne!(result["isError"], true, "{result}");
    let mut record = json_text(&result);
    // The tool answers with the run's description; the pid is in the record
    let file = h.dir.path().join(format!("config/run/demo-{name}.json"));
    let on_disk: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(file).expect("the record")).expect("json");
    record["pid"] = on_disk["pid"].clone();
    record
}

/// Poll the run's record until it is gone; false after 5s.
fn record_gone(h: &Harness, name: &str) -> bool {
    let path = h.dir.path().join(format!("config/run/demo-{name}.json"));
    for _ in 0..100 {
        if !path.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

fn pid_alive(pid: u64) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn sleeper() -> std::process::Child {
    Command::new("/bin/sleep").arg("30").spawn().expect("sleep")
}

fn reap(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn the_cli_stop_finds_a_live_run_and_its_record_goes() {
    let h = Harness::with_stub(RUN_STUB);
    let record = launch_run(&h, "clistop");
    let pid = record["pid"].as_u64().expect("a pid");
    let (stdout, stderr, ok) = h.run(&["stop", "clistop"]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("clistop stopped"), "{stdout}");
    assert!(record_gone(&h, "clistop"));
    assert!(!pid_alive(pid), "the run's process {pid} is still alive");
}

#[test]
fn a_persistent_tunnel_wins_over_a_run_of_the_same_name() {
    let h = Harness::new();
    let run = sleeper();
    write_run_record(&h, "demo", "db", run.id(), ACCESS_SPEC);
    let result = call(&h, "gtunnel_stop_tunnel", r#"{"name":"db"}"#);
    let still_alive = pid_alive(u64::from(run.id()));
    reap(run);
    assert_eq!(
        result["content"][0]["text"], "Stopped tunnel 'db'",
        "{result}"
    );
    assert!(still_alive, "the run was stopped in the tunnel's place");
}

#[test]
fn the_skill_flow_for_a_run_is_run_status_logs_stop() {
    let h = Harness::with_stub(RUN_STUB);
    let record = launch_run(&h, "flow");
    let port = record["local_port"].as_u64().expect("a port");

    // The stub's line reaches the run log a moment after the record
    let ask = r#"{"name":"flow","lines":10}"#;
    let mut logs = call(&h, "gtunnel_get_logs", ask);
    for _ in 0..100 {
        if logs["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Testing if tunnel connection works.")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        logs = call(&h, "gtunnel_get_logs", ask);
    }
    assert_ne!(logs["isError"], true, "{logs}");
    assert!(
        logs["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Testing if tunnel connection works."),
        "{logs}"
    );

    let s = json_text(&call(&h, "gtunnel_get_status", r#"{"name":"flow"}"#));
    assert_eq!(s["run"], true, "{s}");
    assert_eq!(s["status"], "running", "{s}");
    assert_eq!(s["local_port"], port, "{s}");
    assert_eq!(s["pid"], record["pid"], "{s}");
    assert!(s["expires_at"].as_u64().is_some(), "{s}");
    assert!(s.get("health").is_some(), "{s}");

    let stop = call(&h, "gtunnel_stop_tunnel", r#"{"name":"flow"}"#);
    assert_eq!(stop["content"][0]["text"], "Stopped run 'flow'", "{stop}");
    assert!(record_gone(&h, "flow"));
}

#[test]
fn status_and_logs_of_a_run_without_a_log_or_in_another_account() {
    let h = Harness::new();
    let (a, b, c) = (sleeper(), sleeper(), sleeper());
    write_run_record(&h, "demo", "term", a.id(), ACCESS_SPEC);
    write_run_record(&h, "other", "elsewhere", b.id(), ACCESS_SPEC);
    write_run_record(&h, "demo", "public", c.id(), EXPOSE_SPEC);

    let logs = call(&h, "gtunnel_get_logs", r#"{"name":"term"}"#);
    assert_eq!(logs["isError"], true, "{logs}");
    assert_eq!(kind(&logs), "not_found", "{logs}");
    assert!(
        logs["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("started from a terminal"),
        "{logs}"
    );

    let s = json_text(&call(&h, "gtunnel_get_status", r#"{"name":"term"}"#));
    assert_eq!(s["run"], true, "{s}");
    assert_eq!(s["local_port"], 15999, "{s}");
    assert_eq!(s["expires_at"], 1_700_000_300u64, "{s}");
    assert!(s["probe_latency_ms"].is_null(), "{s}");
    assert_eq!(s["health"], "unhealthy", "{s}");
    assert!(s.get("restarts").is_none(), "{s}");

    let other = call(&h, "gtunnel_get_status", r#"{"name":"elsewhere"}"#);
    assert_eq!(kind(&other), "not_found", "{other}");

    let p = json_text(&call(&h, "gtunnel_get_status", r#"{"name":"public"}"#));
    assert!(p["local_port"].is_null(), "{p}");
    assert_eq!(p["health"], "unknown", "{p}");

    for child in [a, b, c] {
        reap(child);
    }
}

#[test]
fn a_stale_run_record_is_not_found_by_status_or_logs() {
    let h = Harness::new();
    let mut dead = sleeper();
    let pid = dead.id();
    dead.kill().unwrap();
    dead.wait().unwrap();
    write_run_record(&h, "demo", "ghost", pid, ACCESS_SPEC);
    for tool in ["gtunnel_get_status", "gtunnel_get_logs"] {
        let r = call(&h, tool, r#"{"name":"ghost"}"#);
        assert_eq!(r["isError"], true, "{tool}: {r}");
        assert_eq!(kind(&r), "not_found", "{tool}: {r}");
    }
}

#[test]
fn a_persistent_tunnel_wins_for_status_and_logs() {
    let h = Harness::new();
    let run = sleeper();
    write_run_record(&h, "demo", "db", run.id(), ACCESS_SPEC);
    let s = json_text(&call(&h, "gtunnel_get_status", r#"{"name":"db"}"#));
    reap(run);
    assert_eq!(s["run"], false, "{s}");
}

const LOGGED_OUT_STUB: &str = r#"#!/bin/sh
echo "ERROR: (gcloud.auth.print-access-token) No credential found" >&2
exit 1
"#;

// The run child is gtunnel itself: it fails at the login gate with exit 4,
// before any record, so the kind is deterministic
#[test]
fn a_failed_run_carries_the_childs_kind() {
    let h = Harness::with_stub(LOGGED_OUT_STUB);
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_run_tunnel","arguments":{"name":"r1","target":"my-vm:5432","ttl_minutes":1}}}"#;
    let (stdout, _, _) = h.run_mcp_pipe(&format!("{req}\n"));
    let f = &frames(&stdout)[0];
    assert_eq!(f["result"]["isError"], true, "{f}");
    assert_eq!(
        f["result"]["structuredContent"]["error"]["kind"], "auth",
        "{f}"
    );
}

#[test]
fn a_run_name_that_is_already_live_is_a_usage_error() {
    let h = Harness::new();
    let mut sleeper = Command::new("/bin/sleep").arg("30").spawn().expect("sleep");
    write_run_record(&h, "demo", "scratch", sleeper.id(), ACCESS_SPEC);
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_run_tunnel","arguments":{"name":"scratch","target":"my-vm:5432","ttl_minutes":1}}}"#;
    let (stdout, _, _) = h.run_mcp_pipe(&format!("{req}\n"));
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let f = &frames(&stdout)[0];
    assert_eq!(f["result"]["isError"], true, "{f}");
    assert_eq!(
        f["result"]["structuredContent"]["error"]["kind"], "usage",
        "{f}"
    );
}

#[test]
fn an_unknown_account_or_tunnel_is_a_tool_error_with_not_found() {
    let h = Harness::new();
    let calls = [
        r#"{"name":"gtunnel_list_tunnels","arguments":{"account":"nope"}}"#,
        r#"{"name":"gtunnel_stop_tunnel","arguments":{"name":"db","account":"nope"}}"#,
        r#"{"name":"gtunnel_get_status","arguments":{"name":"db","account":"nope"}}"#,
        r#"{"name":"gtunnel_get_logs","arguments":{"name":"db","account":"nope"}}"#,
        r#"{"name":"gtunnel_run_tunnel","arguments":{"target":"my-vm:5432","ttl_minutes":1,"account":"nope"}}"#,
        r#"{"name":"gtunnel_doctor","arguments":{"account":"nope"}}"#,
        r#"{"name":"gtunnel_get_status","arguments":{"name":"nope"}}"#,
        r#"{"name":"gtunnel_get_logs","arguments":{"name":"nope"}}"#,
    ];
    let input: String = calls
        .iter()
        .enumerate()
        .map(|(i, p)| {
            format!(r#"{{"jsonrpc":"2.0","id":{i},"method":"tools/call","params":{p}}}"#) + "\n"
        })
        .collect();
    let (stdout, _, _) = h.run_mcp_pipe(&input);
    let frames = frames(&stdout);
    assert_eq!(frames.len(), calls.len(), "{stdout}");
    for f in &frames {
        assert!(f.get("error").is_none(), "{f}");
        assert_eq!(f["result"]["isError"], true, "{f}");
        assert_eq!(
            f["result"]["structuredContent"]["error"]["kind"], "not_found",
            "{f}"
        );
    }
}

#[test]
fn add_with_a_port_out_of_range_is_invalid_params_and_writes_nothing() {
    let h = Harness::new();
    let before = fs::read_to_string(h.dir.path().join("config/tunnels.toml")).unwrap();
    let mut input = String::new();
    for (id, port) in [(1, "70000"), (2, "0"), (3, "-1")] {
        input.push_str(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"gtunnel_add_tunnel","arguments":{{"name":"k9","target":"10.0.0.10:443","via":"bastion","local_port":{port}}}}}}}"#
        ));
        input.push('\n');
    }
    let (stdout, _, _) = h.run_mcp_pipe(&input);
    let frames = frames(&stdout);
    assert_eq!(frames.len(), 3, "{stdout}");
    for f in &frames {
        assert_eq!(f["error"]["code"], -32602, "{f}");
        assert!(
            f["error"]["message"]
                .as_str()
                .unwrap()
                .contains("between 1 and 65535"),
            "{f}"
        );
    }
    let after = fs::read_to_string(h.dir.path().join("config/tunnels.toml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn a_notification_gets_no_reply_and_the_session_goes_on() {
    let h = Harness::new();
    let input = concat!(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"reason":"x"}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":1,"progress":1}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        "\n",
    );
    let (stdout, stderr, ok) = h.run_mcp_pipe(input);
    assert!(ok, "stderr: {stderr}");
    let frames = frames(&stdout);
    assert_eq!(frames.len(), 1, "{stdout}");
    assert_eq!(frames[0]["id"], 2);
    assert!(frames[0].get("error").is_none(), "{stdout}");
}

#[test]
fn the_cli_logs_of_a_run_print_its_log_and_a_terminal_run_has_none() {
    let h = Harness::with_stub(RUN_STUB);
    launch_run(&h, "clilogs");
    let mut out = String::new();
    for _ in 0..100 {
        out = h.run(&["logs", "clilogs"]).0;
        if out.contains("Testing if tunnel connection works.") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let (_, stop_err, stopped) = h.run(&["stop", "clilogs"]);
    assert!(
        out.contains("Testing if tunnel connection works."),
        "{out}\n{stop_err}"
    );
    assert!(stopped, "{stop_err}");

    // A run started from a terminal prints there and leaves no file
    let term = sleeper();
    write_run_record(&h, "demo", "term", term.id(), ACCESS_SPEC);
    let (stdout, stderr, ok) = h.run(&["logs", "term"]);
    reap(term);
    assert!(!ok, "{stdout}");
    assert!(stderr.contains("started from a terminal"), "{stderr}");
}

// Rename and move share one core with the CLI; these pin the MCP door of it

fn tunnels_toml(h: &Harness) -> String {
    fs::read_to_string(h.dir.path().join("config/tunnels.toml")).unwrap()
}

fn result_text(result: &serde_json::Value) -> &str {
    result["content"][0]["text"].as_str().expect("text")
}

#[test]
fn rename_over_mcp_renames_the_record() {
    let h = Harness::new();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_rename_tunnel","arguments":{"name":"db","new_name":"database"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"gtunnel_list_tunnels","arguments":{}}}"#,
        "\n",
    );
    let (stdout, stderr, ok) = h.run_mcp_pipe(input);
    assert!(ok, "stderr: {stderr}");
    let frames = frames(&stdout);
    assert_eq!(frames.len(), 2, "{stdout}");
    let renamed = &frames[0]["result"];
    assert_ne!(renamed["isError"], serde_json::json!(true), "{renamed}");
    assert_eq!(result_text(renamed), "Renamed db to database");
    let list = result_text(&frames[1]["result"]);
    assert!(list.contains("\"database\""), "{list}");
    assert!(!list.contains("\"db\""), "{list}");
}

#[test]
fn rename_to_an_existing_name_is_refused_and_changes_nothing() {
    let h = Harness::new();
    let both = format!(
        "{}\n{}",
        TUNNELS,
        TUNNELS
            .replace("name = \"db\"", "name = \"cache\"")
            .replace("15432", "15433")
    );
    fs::write(h.dir.path().join("config/tunnels.toml"), &both).unwrap();
    let before = tunnels_toml(&h);
    let result = call(
        &h,
        "gtunnel_rename_tunnel",
        r#"{"name":"db","new_name":"cache"}"#,
    );
    assert_eq!(result["isError"], serde_json::json!(true), "{result}");
    assert_eq!(tunnels_toml(&h), before);
}

#[test]
fn rename_of_an_unknown_tunnel_is_not_found() {
    let h = Harness::new();
    let result = call(
        &h,
        "gtunnel_rename_tunnel",
        r#"{"name":"nope","new_name":"other"}"#,
    );
    assert_eq!(result["isError"], serde_json::json!(true), "{result}");
    assert_eq!(kind(&result), "not_found", "{result}");
}

#[test]
fn move_to_an_unknown_account_is_not_found_before_anything_stops() {
    let h = Harness::new();
    let before = tunnels_toml(&h);
    let result = call(&h, "gtunnel_move_tunnel", r#"{"name":"db","to":"nowhere"}"#);
    assert_eq!(result["isError"], serde_json::json!(true), "{result}");
    assert_eq!(kind(&result), "not_found", "{result}");
    assert_eq!(tunnels_toml(&h), before);
}

#[test]
fn move_to_the_same_account_points_at_rename() {
    let h = Harness::new();
    let result = call(&h, "gtunnel_move_tunnel", r#"{"name":"db","to":"demo"}"#);
    assert_eq!(result["isError"], serde_json::json!(true), "{result}");
    assert!(
        result_text(&result).contains("use `gtunnel rename`"),
        "{result}"
    );
}

#[test]
fn move_to_a_logged_out_account_fails_with_auth_and_no_prompt() {
    let h = Harness::logged_out();
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_move_tunnel","arguments":{"name":"db","to":"other"}}}"#;
    let (stdout, stderr, _) = h.run_mcp_pipe(&format!("{req}\n"));
    let frames = frames(&stdout);
    assert_eq!(frames.len(), 1, "stdout: {stdout}; stderr: {stderr}");
    let result = &frames[0]["result"];
    assert_eq!(result["isError"], serde_json::json!(true), "{result}");
    assert_eq!(kind(result), "auth", "{result}");
}

#[test]
fn rename_with_a_non_string_new_name_is_invalid_params() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_rename_tunnel","arguments":{"name":"db","new_name":5}}}"#;
    let (stdout, _, _) = h.run_mcp_pipe(&format!("{req}\n"));
    let frames = frames(&stdout);
    assert_eq!(frames[0]["error"]["code"], -32602, "{stdout}");
}

// The expose tool is private unless told otherwise; these pin its door

// A relay block with placeholder values: enough for the name check, which runs
// before any GCP call, to be reached
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

#[test]
fn expose_without_a_relay_is_refused_and_changes_nothing() {
    let h = Harness::new();
    let before = tunnels_toml(&h);
    let result = call(
        &h,
        "gtunnel_expose_tunnel",
        r#"{"name":"web","target":"3000"}"#,
    );
    assert_eq!(result["isError"], serde_json::json!(true), "{result}");
    assert!(
        result_text(&result).contains("gtunnel relay up"),
        "{result}"
    );
    assert_eq!(tunnels_toml(&h), before);
}

#[test]
fn expose_with_a_string_public_is_invalid_params() {
    let h = Harness::build(STUB, RELAY_CONFIG);
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_expose_tunnel","arguments":{"name":"web","target":"3000","public":"true"}}}"#;
    let (stdout, _, _) = h.run_mcp_pipe(&format!("{req}\n"));
    let frames = frames(&stdout);
    assert_eq!(frames[0]["error"]["code"], -32602, "{stdout}");
}

#[test]
fn expose_of_an_existing_name_is_refused() {
    let h = Harness::build(STUB, RELAY_CONFIG);
    let before = tunnels_toml(&h);
    let result = call(
        &h,
        "gtunnel_expose_tunnel",
        r#"{"name":"db","target":"3000"}"#,
    );
    assert_eq!(result["isError"], serde_json::json!(true), "{result}");
    assert!(result_text(&result).contains("already exists"), "{result}");
    assert_eq!(tunnels_toml(&h), before);
}

#[test]
fn expose_of_a_remote_target_is_refused_and_changes_nothing() {
    let h = Harness::build(STUB, RELAY_CONFIG);
    let before = tunnels_toml(&h);
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gtunnel_expose_tunnel","arguments":{"name":"web","target":"169.254.169.254:80"}}}"#;
    let (stdout, _, _) = h.run_mcp_pipe(&format!("{req}\n"));
    let frames = frames(&stdout);
    assert_eq!(frames[0]["error"]["code"], -32602, "{stdout}");
    assert!(
        frames[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("only a service on this machine"),
        "{stdout}"
    );
    assert_eq!(tunnels_toml(&h), before);
}

#[test]
fn expose_tool_says_public_in_its_description() {
    let tool = listed_tools()
        .into_iter()
        .find(|t| t["name"] == "gtunnel_expose_tunnel")
        .expect("the expose tool is listed");
    let description = tool["description"].as_str().unwrap();
    assert!(description.contains("public: true"), "{description}");
    assert!(description.contains("anyone"), "{description}");
    assert!(description.contains("Private by default"), "{description}");
}
