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
            .env("GTUNNEL_NO_NOTIFY", "1")
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
    assert!(stdout.contains("Validated 11 tools and 2 resources successfully."));
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
fn mcp_stdio_tools_list_returns_eleven_tools() {
    let h = Harness::new();
    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let (stdout, stderr, success) = h.run_mcp_pipe(&format!("{req}\n"));
    assert!(success, "stderr: {stderr}");

    let line = stdout.lines().next().expect("response line");
    let resp: serde_json::Value = serde_json::from_str(line).expect("valid json response");
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 11);

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
