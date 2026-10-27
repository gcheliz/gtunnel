use serde_json::{json, Map, Value};
use std::time::Duration;

use crate::ephemeral::{
    generated_name, list_ephemeral, pid_is_alive, signalable, wait_for_pid_exit, EphemeralRecord,
};
use crate::state::TunnelSpec;

/// An agent opens a tunnel and may never come back to close it, so the
/// lifetime is a required input and has a ceiling, not a default. Eight hours
/// is a working day; anything longer belongs to `gtunnel_add_tunnel`, which the
/// user can see and delete.
pub const MAX_TTL_MINUTES: u64 = 480;

/// How long a launch waits for the run to publish its record before it calls
/// the start failed. gcloud is spawned before the record is written, so this
/// covers a process start, not a login.
const LAUNCH_BUDGET: Duration = Duration::from_secs(5);

/// A validated `gtunnel_run_tunnel` call. Access mode only: publishing a
/// hostname is something a person asks for, never an agent on its own.
#[derive(Debug, PartialEq, Eq)]
pub struct RunRequest {
    pub name: String,
    pub target: String,
    pub zone: Option<String>,
    pub local_port: Option<u16>,
    pub ttl_minutes: u64,
}

impl RunRequest {
    /// Every refusal names the argument, so the model can fix that one input
    /// instead of guessing which of five was wrong.
    pub fn parse(args: &Map<String, Value>) -> Result<Self, String> {
        if args.get("public").is_some_and(|v| !v.is_null()) {
            return Err("'public' is not offered: this tool opens a local tunnel only".into());
        }
        let target = args
            .get("target")
            .and_then(Value::as_str)
            .ok_or("Missing 'target'")?;
        if target.starts_with('-') {
            return Err(format!("Invalid 'target': '{target}' starts with a dash"));
        }
        let (instance, _) = crate::commands::access::parse_access_target(target)
            .map_err(|e| format!("Invalid 'target': {e:#}"))?;
        crate::state::validate_instance_name(&instance)
            .map_err(|e| format!("Invalid 'target': {e:#}"))?;

        let ttl_minutes = args
            .get("ttl_minutes")
            .and_then(Value::as_u64)
            .ok_or("Missing 'ttl_minutes': a whole number of minutes")?;
        if !(1..=MAX_TTL_MINUTES).contains(&ttl_minutes) {
            return Err(format!(
                "'ttl_minutes' must be between 1 and {MAX_TTL_MINUTES}, got {ttl_minutes}"
            ));
        }

        let name = match args.get("name").and_then(Value::as_str) {
            Some(name) => name.to_string(),
            None => generated_name(),
        };
        crate::state::validate_tunnel_name(&name).map_err(|e| format!("Invalid 'name': {e:#}"))?;

        let local_port = match args.get("local_port").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or("'local_port' must be between 1 and 65535")?,
            ),
        };
        let zone = args.get("zone").and_then(Value::as_str).map(str::to_string);
        if let Some(zone) = &zone {
            crate::state::validate_zone(zone).map_err(|e| format!("Invalid 'zone': {e:#}"))?;
        }

        Ok(Self {
            name,
            target: target.to_string(),
            zone,
            local_port,
            ttl_minutes,
        })
    }

    /// The account goes before the subcommand because `--account` is a global
    /// flag of the CLI, and it is always passed so the child never falls back
    /// to whichever account is selected when it starts. The positionals come
    /// last, after `--`, so a value from the model is never parsed as a flag.
    pub fn argv(&self, account: &str) -> Vec<String> {
        let mut argv: Vec<String> = ["--account", account, "run"].map(String::from).into();
        if let Some(zone) = &self.zone {
            argv.extend(["--zone".into(), zone.clone()]);
        }
        if let Some(port) = self.local_port {
            argv.extend(["--local-port".into(), port.to_string()]);
        }
        argv.extend(["--ttl".into(), format!("{}m", self.ttl_minutes)]);
        argv.extend(["--".into(), self.name.clone(), self.target.clone()]);
        argv
    }
}

fn log_tail(path: &std::path::Path) -> String {
    let text = crate::daemon::read_tail(path, 4096);
    let lines: Vec<&str> = text.lines().rev().take(20).collect();
    let tail = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
    if tail.is_empty() {
        "(the log is empty)".to_string()
    } else {
        tail
    }
}

fn find_record(account: &str, name: &str) -> Option<(EphemeralRecord, bool)> {
    list_ephemeral()
        .ok()?
        .into_iter()
        .find(|(r, _)| r.account_name == account && r.name == name)
}

fn terminate(pid: u32) {
    if let Some(pid) = signalable(pid) {
        // SAFETY: kill(2) takes two integers and no pointer. `signalable`
        // rejected 0 and negative values, so this names one process and never
        // a group. A pid that has exited since is a harmless ESRCH.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

/// Start `gtunnel run` as a process that outlives this one, and answer only
/// once it has published its record. Err is the text of a tool error.
pub async fn launch(account: &str, req: &RunRequest) -> Result<Value, String> {
    if let Some((_, true)) = find_record(account, &req.name) {
        return Err(format!(
            "A run named '{}' is already live for account '{account}'",
            req.name
        ));
    }

    let exe = std::env::current_exe().map_err(|e| format!("Cannot find gtunnel: {e}"))?;
    let logs = crate::config::config_dir()
        .map_err(|e| format!("{e:#}"))?
        .join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| format!("Cannot create {}: {e}", logs.display()))?;
    let log_path = logs.join(format!("run-{account}-{}.log", req.name));
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
    };
    let (out, err) = open()
        .and_then(|out| open().map(|err| (out, err)))
        .map_err(|e| format!("Cannot open {}: {e}", log_path.display()))?;

    // Its own process group, so a signal aimed at the MCP server's group (a
    // closed terminal, a client killing its tree) does not take the tunnel
    // with it; the TTL is what ends it. Dropping the handle leaves tokio to
    // reap the child, so it never lingers as a zombie of this server.
    let mut child = tokio::process::Command::new(exe)
        .args(req.argv(account))
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Failed to start gtunnel run: {e}"))?;

    let deadline = std::time::Instant::now() + LAUNCH_BUDGET;
    loop {
        if let Some((record, true)) = find_record(account, &req.name) {
            return Ok(describe(&record));
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "gtunnel run exited with {status} before the tunnel came up. Log {}:\n{}",
                log_path.display(),
                log_tail(&log_path)
            ));
        }
        if std::time::Instant::now() >= deadline {
            // It may still be starting; SIGTERM lets it tear down whatever it made
            if let Some(pid) = child.id() {
                terminate(pid);
            }
            return Err(format!(
                "The tunnel did not come up within {}s. Log {}:\n{}",
                LAUNCH_BUDGET.as_secs(),
                log_path.display(),
                log_tail(&log_path)
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn describe(record: &EphemeralRecord) -> Value {
    let tunnel = record.as_tunnel();
    let local_port = match &record.spec {
        TunnelSpec::Access { local_port, .. } => Some(*local_port),
        TunnelSpec::Expose { .. } => None,
    };
    json!({
        "name": record.name,
        "account": record.account_name,
        "url": tunnel.display_url(),
        "local_port": local_port,
        "target": tunnel.display_target(),
        "ttl_minutes": record.ttl_secs.map(|t| t / 60),
        "expires_at": record.expires_at(),
    })
}

/// Stop a live run by name. `Ok(None)` means there is no such run, so the
/// caller can fall through to its own not-found message.
pub async fn stop_run(account: &str, name: &str) -> Result<Option<()>, String> {
    let Some((record, alive)) = find_record(account, name) else {
        return Ok(None);
    };
    if !alive {
        return Err(format!(
            "Run '{name}' is not running (its record is stale; `gtunnel doctor --fix` clears it)"
        ));
    }
    terminate(record.pid);
    if !wait_for_pid_exit(record.pid, Duration::from_secs(5)).await {
        return Err(format!("Run '{name}' did not exit within 5s of SIGTERM"));
    }
    // gcloud is gone; the run process now removes its record. Give it a moment
    // so the answer matches what `ls` will show.
    for _ in 0..40 {
        if find_record(account, name).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    debug_assert!(!pid_is_alive(record.pid));
    Ok(Some(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        v.as_object().cloned().expect("an object")
    }

    #[test]
    fn a_request_without_a_ttl_is_refused() {
        let err = RunRequest::parse(&args(json!({"target": "my-vm:5432"}))).unwrap_err();
        assert!(err.contains("ttl_minutes"), "{err}");
    }

    #[test]
    fn a_ttl_outside_one_to_four_eighty_minutes_is_refused() {
        for bad in [0, 481, 100_000] {
            let err = RunRequest::parse(&args(json!({"target": "my-vm:5432", "ttl_minutes": bad})))
                .unwrap_err();
            assert!(err.contains("ttl_minutes"), "{bad}: {err}");
        }
        for ok in [1, 480] {
            RunRequest::parse(&args(json!({"target": "my-vm:5432", "ttl_minutes": ok})))
                .expect("in range");
        }
    }

    #[test]
    fn a_target_without_a_port_is_refused() {
        for bad in ["my-vm", "my-vm:0", ":5432", "my-vm:http"] {
            let err =
                RunRequest::parse(&args(json!({"target": bad, "ttl_minutes": 5}))).unwrap_err();
            assert!(err.contains("target"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_request_for_a_public_run_is_refused() {
        let err = RunRequest::parse(&args(
            json!({"target": "my-vm:5432", "ttl_minutes": 5, "public": true}),
        ))
        .unwrap_err();
        assert!(err.contains("public"), "{err}");
    }

    #[test]
    fn a_bad_name_is_refused_and_a_missing_one_is_generated() {
        let err = RunRequest::parse(&args(
            json!({"name": "Bad_Name", "target": "my-vm:5432", "ttl_minutes": 5}),
        ))
        .unwrap_err();
        assert!(err.contains("name"), "{err}");
        let req = RunRequest::parse(&args(json!({"target": "my-vm:5432", "ttl_minutes": 5})))
            .expect("parse");
        assert!(req.name.starts_with("gt-"), "{}", req.name);
    }

    #[test]
    fn the_child_is_told_the_account_the_target_and_the_ttl_in_minutes() {
        let req = RunRequest::parse(&args(json!({
            "name": "db", "target": "my-vm:5432", "zone": "europe-west1-b",
            "local_port": 15432, "ttl_minutes": 30
        })))
        .expect("parse");
        assert_eq!(
            req.argv("demo"),
            [
                "--account",
                "demo",
                "run",
                "--zone",
                "europe-west1-b",
                "--local-port",
                "15432",
                "--ttl",
                "30m",
                "--",
                "db",
                "my-vm:5432"
            ]
        );
    }

    #[test]
    fn a_minimal_request_passes_no_optional_flags() {
        let req = RunRequest::parse(&args(
            json!({"name": "db", "target": "my-vm:5432", "ttl_minutes": 1}),
        ))
        .expect("parse");
        assert_eq!(
            req.argv("demo"),
            [
                "--account",
                "demo",
                "run",
                "--ttl",
                "1m",
                "--",
                "db",
                "my-vm:5432"
            ]
        );
    }

    #[test]
    fn a_target_or_zone_that_reads_as_a_flag_is_refused() {
        for bad in ["--public:22", "-zx:22"] {
            let err =
                RunRequest::parse(&args(json!({"target": bad, "ttl_minutes": 5}))).unwrap_err();
            assert!(err.contains("target"), "{bad}: {err}");
        }
        for bad in ["--public", "-d", "Europe-West1", "a b"] {
            let err = RunRequest::parse(&args(
                json!({"target": "my-vm:5432", "zone": bad, "ttl_minutes": 5}),
            ))
            .unwrap_err();
            assert!(err.contains("zone"), "{bad}: {err}");
        }
    }
}
