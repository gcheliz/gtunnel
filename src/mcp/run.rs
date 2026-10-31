use anyhow::anyhow;
use serde_json::{json, Map, Value};
use std::time::Duration;

use crate::ephemeral::{generated_name, list_ephemeral, signalable, EphemeralRecord};
use crate::error::{ErrorKind, GtunnelError};

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

        let local_port = parse_port(args, "local_port")?;
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

/// One range check for every tool that takes a port: a cast with `as u16`
/// turned 65536 into 0 and 70000 into 4464, and wrote that to disk. Absent or
/// null means "pick one for me", so it is the only way to get an allocation.
pub(crate) fn parse_port(args: &Map<String, Value>, key: &str) -> Result<Option<u16>, String> {
    match args.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be between 1 and 65535")),
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

pub(crate) fn find_record(account: &str, name: &str) -> Option<(EphemeralRecord, bool)> {
    list_ephemeral()
        .ok()?
        .into_iter()
        .find(|(r, _)| r.account_name == account && r.name == name)
}

/// The live run of that name, or none. A record whose process is gone is not
/// a run: status and logs must never describe a dead one as running.
pub(crate) fn find_live_run(account: &str, name: &str) -> Option<EphemeralRecord> {
    match find_record(account, name) {
        Some((record, true)) => Some(record),
        _ => None,
    }
}

/// Where a launched run's output goes. One function for the writer in
/// `launch` and the readers (the logs tool and `gtunnel logs`), so they cannot
/// disagree about the file.
pub(crate) fn run_log_path(account: &str, name: &str) -> anyhow::Result<std::path::PathBuf> {
    Ok(crate::config::config_dir()?
        .join("logs")
        .join(format!("run-{}.log", crate::state::label_of(account, name))))
}

/// The log of a live run, for the two readers. `None` means no live run of
/// that name, so the caller can fall through to its own not-found. Only a
/// launch from MCP redirects output to a file; a run in a terminal prints
/// there, and an empty success would read as "no output yet".
pub(crate) fn live_run_log(
    account: &str,
    name: &str,
) -> Option<anyhow::Result<std::path::PathBuf>> {
    find_live_run(account, name)?;
    Some(run_log_path(account, name).and_then(|path| {
        if path.exists() {
            Ok(path)
        } else {
            Err(GtunnelError::not_found(format!(
                "Run '{name}' has no log: it was started from a terminal, which has its output"
            ))
            .into())
        }
    }))
}

/// The stdout and stderr handles of a launch, on a log that holds nothing of
/// an earlier one. A name is reused (the TTL ends a run and the agent launches
/// it again), and a log that kept appending would hand the next reader the
/// last run's errors as if they were this one's. Truncated once, here, and not
/// through either handle: two descriptors each keep their own offset, so
/// truncating on both opens would let the second overwrite the first's output.
/// The file is left in place when the run ends, so `gtunnel logs` still reads
/// it until the name is launched again.
fn fresh_run_log(path: &std::path::Path) -> anyhow::Result<(std::fs::File, std::fs::File)> {
    crate::daemon::reset_log(path)?;
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    };
    Ok((open()?, open()?))
}

/// Start `gtunnel run` as a process that outlives this one, and answer only
/// once it has published its record. The error carries its kind, so a failed
/// launch reaches the caller through `ToolCallResult::failure` like every
/// other tool error.
pub async fn launch(account: &str, req: &RunRequest) -> anyhow::Result<Value> {
    // The child `gtunnel run` checks again under the lock; this early check
    // only returns the refusal without waiting out the launch budget
    let state = crate::state::TunnelState::load()?;
    crate::ephemeral::check_run_name(&state, account, &req.name)?;

    let exe = crate::update::exe_path()?;
    let log_path = run_log_path(account, &req.name)?;
    if let Some(logs) = log_path.parent() {
        std::fs::create_dir_all(logs)
            .map_err(|e| anyhow!("Cannot create {}: {e}", logs.display()))?;
    }
    let (out, err) =
        fresh_run_log(&log_path).map_err(|e| anyhow!("Cannot open {}: {e}", log_path.display()))?;

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
        .map_err(|e| anyhow!("Failed to start gtunnel run: {e}"))?;

    let deadline = std::time::Instant::now() + LAUNCH_BUDGET;
    loop {
        if let Some((record, true)) = find_record(account, &req.name) {
            return Ok(describe(&record));
        }
        if let Ok(Some(status)) = child.try_wait() {
            // The child is gtunnel, so its exit code already says what kind
            // of failure it was
            return Err(GtunnelError::new(
                ErrorKind::from_exit_code(status.code()),
                format!(
                    "gtunnel run exited with {status} before the tunnel came up. Log {}:\n{}",
                    log_path.display(),
                    log_tail(&log_path)
                ),
            )
            .into());
        }
        if std::time::Instant::now() >= deadline {
            // It may still be starting; SIGTERM lets it tear down whatever it made
            if let Some(pid) = child.id().and_then(signalable) {
                // SAFETY: kill(2) takes two integers and no pointer. This is
                // the `gtunnel run` this function just spawned and still
                // holds, not a record, so the pid cannot be a recycled one;
                // `signalable` rejected 0 and negative values.
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            return Err(GtunnelError::new(
                ErrorKind::Internal,
                format!(
                    "The tunnel did not come up within {}s. Log {}:\n{}",
                    LAUNCH_BUDGET.as_secs(),
                    log_path.display(),
                    log_tail(&log_path)
                ),
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(super) fn describe(record: &EphemeralRecord) -> Value {
    let tunnel = record.as_tunnel();
    let local_port = record.spec.local_port();
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
pub async fn stop_run(account: &str, name: &str) -> anyhow::Result<Option<()>> {
    let Some((record, alive)) = find_record(account, name) else {
        return Ok(None);
    };
    if !alive {
        return Err(GtunnelError::not_found(format!(
            "Run '{name}' is not running (its record is stale; `gtunnel doctor --fix` clears it)"
        ))
        .into());
    }
    record.terminate();
    if !record.wait_for_exit(Duration::from_secs(5)).await {
        return Err(GtunnelError::new(
            ErrorKind::Internal,
            format!("Run '{name}' did not exit within 5s of SIGTERM"),
        )
        .into());
    }
    // gcloud is gone; the run process now sweeps its group, which can take the
    // whole grace, and then removes its record. Give it that long so the
    // answer matches what `ls` will show.
    let deadline = std::time::Instant::now() + crate::proc::STOP_GRACE + Duration::from_secs(1);
    while find_record(account, name).is_some() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
    fn launching_a_run_name_again_leaves_only_the_second_launchs_output() {
        use std::io::Write;
        let _dir = crate::config::testing::TempConfigDir::new();
        let path = run_log_path("dev", "db").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let (mut out, _err) = fresh_run_log(&path).unwrap();
        writeln!(out, "first launch").unwrap();
        drop(out);

        let (mut out, mut err) = fresh_run_log(&path).unwrap();
        writeln!(out, "second stdout").unwrap();
        writeln!(err, "second stderr").unwrap();

        let log = std::fs::read_to_string(&path).unwrap();
        assert_eq!(log, "second stdout\nsecond stderr\n");
    }

    #[test]
    fn a_runs_log_is_still_there_once_its_handles_are_closed() {
        use std::io::Write;
        let _dir = crate::config::testing::TempConfigDir::new();
        let path = run_log_path("dev", "db").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let (mut out, err) = fresh_run_log(&path).unwrap();
        writeln!(out, "gtunnel: ttl expired").unwrap();
        drop((out, err));

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "gtunnel: ttl expired\n"
        );
    }

    #[test]
    fn a_port_outside_one_to_65535_is_refused_and_an_absent_one_is_none() {
        for bad in [
            json!(0),
            json!(65536),
            json!(70000),
            json!(-1),
            json!(1.5),
            json!("8080"),
        ] {
            let err = parse_port(&args(json!({"local_port": bad})), "local_port").unwrap_err();
            assert_eq!(err, "'local_port' must be between 1 and 65535", "{bad}");
        }
        assert_eq!(
            parse_port(&args(json!({"local_port": 1})), "local_port"),
            Ok(Some(1))
        );
        assert_eq!(
            parse_port(&args(json!({"local_port": 65535})), "local_port"),
            Ok(Some(65535))
        );
        assert_eq!(
            parse_port(&args(json!({"local_port": null})), "local_port"),
            Ok(None)
        );
        assert_eq!(parse_port(&args(json!({})), "local_port"), Ok(None));
    }

    #[test]
    fn a_request_without_a_ttl_is_refused() {
        let err = RunRequest::parse(&args(json!({"target": "bastion:5432"}))).unwrap_err();
        assert!(err.contains("ttl_minutes"), "{err}");
    }

    #[test]
    fn a_ttl_outside_one_to_four_eighty_minutes_is_refused() {
        for bad in [0, 481, 100_000] {
            let err =
                RunRequest::parse(&args(json!({"target": "bastion:5432", "ttl_minutes": bad})))
                    .unwrap_err();
            assert!(err.contains("ttl_minutes"), "{bad}: {err}");
        }
        for ok in [1, 480] {
            RunRequest::parse(&args(json!({"target": "bastion:5432", "ttl_minutes": ok})))
                .expect("in range");
        }
    }

    #[test]
    fn a_target_without_a_port_is_refused() {
        for bad in ["bastion", "bastion:0", ":5432", "bastion:http"] {
            let err =
                RunRequest::parse(&args(json!({"target": bad, "ttl_minutes": 5}))).unwrap_err();
            assert!(err.contains("target"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_request_for_a_public_run_is_refused() {
        let err = RunRequest::parse(&args(
            json!({"target": "bastion:5432", "ttl_minutes": 5, "public": true}),
        ))
        .unwrap_err();
        assert!(err.contains("public"), "{err}");
    }

    #[test]
    fn a_bad_name_is_refused_and_a_missing_one_is_generated() {
        let err = RunRequest::parse(&args(
            json!({"name": "Bad_Name", "target": "bastion:5432", "ttl_minutes": 5}),
        ))
        .unwrap_err();
        assert!(err.contains("name"), "{err}");
        let req = RunRequest::parse(&args(json!({"target": "bastion:5432", "ttl_minutes": 5})))
            .expect("parse");
        assert!(req.name.starts_with("gt-"), "{}", req.name);
    }

    #[test]
    fn the_child_is_told_the_account_the_target_and_the_ttl_in_minutes() {
        let req = RunRequest::parse(&args(json!({
            "name": "db", "target": "bastion:5432", "zone": "europe-west1-b",
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
                "bastion:5432"
            ]
        );
    }

    #[test]
    fn a_minimal_request_passes_no_optional_flags() {
        let req = RunRequest::parse(&args(
            json!({"name": "db", "target": "bastion:5432", "ttl_minutes": 1}),
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
                "bastion:5432"
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
                json!({"target": "bastion:5432", "zone": bad, "ttl_minutes": 5}),
            ))
            .unwrap_err();
            assert!(err.contains("zone"), "{bad}: {err}");
        }
    }
}
