use serde_json::{json, Value};
use std::time::Duration;

use super::protocol::{
    JsonRpcError, ResourceContent, ResourceDefinition, ResourceReadResult, ToolCallResult,
    ToolDefinition, INTERNAL_ERROR, INVALID_PARAMS,
};
use crate::commands::access::{
    add_tunnel, delete_tunnel, restart_tunnel, start_tunnel, stop_by_name, AddRequest, Stopped,
};
use crate::commands::{find_tunnel, resolve};
use crate::config::load_config;
use crate::daemon;
use crate::doctor::run_doctor;
use crate::probe;
use crate::state::{TunnelSpec, TunnelState, TunnelStatus};

pub fn all_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "gtunnel_list_tunnels".to_string(),
            description: "List all configured tunnels and live short-lived runs (run: true, with expires_at when they have a TTL) and their statuses for the given or default account".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name (defaults to active account)"
                    }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "gtunnel_start_tunnel".to_string(),
            description: "Start a persistent tunnel daemon".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name to start"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_stop_tunnel".to_string(),
            description: "Stop a running tunnel daemon, or a live run started by gtunnel_run_tunnel".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name to stop"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_restart_tunnel".to_string(),
            description: "Restart a tunnel daemon (reinstalls service spec and restarts)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name to restart"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_get_status".to_string(),
            description: "Get detailed runtime status, PID, restarts, health (healthy, unhealthy, auth_expired, unknown) and active TCP probe latency for a tunnel or a live run (a run reports pid, local port and expires_at)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_get_logs".to_string(),
            description: "Read the most recent log lines for a tunnel daemon or a live run started from MCP".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name"
                    },
                    "lines": {
                        "type": "integer",
                        "description": "Number of lines to read from log tail (default: 50)"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_add_tunnel".to_string(),
            description: "Add a persistent Access tunnel (IAP TCP forward to localhost)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name (lowercase letters, digits, and dashes; no env suffixes)"
                    },
                    "target": {
                        "type": "string",
                        "description": "Remote target: INSTANCE:PORT, or HOST:PORT with --via or --dest-group"
                    },
                    "via": {
                        "type": "string",
                        "description": "Instance to forward from via ssh (bastion jump)"
                    },
                    "zone": {
                        "type": "string",
                        "description": "Compute zone of the instance"
                    },
                    "local_port": {
                        "type": "integer",
                        "description": "Local port to listen on (automatically allocated if omitted)"
                    },
                    "dest_group": {
                        "type": "string",
                        "description": "IAP destination group"
                    },
                    "region": {
                        "type": "string",
                        "description": "Region of the destination group"
                    },
                    "network": {
                        "type": "string",
                        "description": "Network of the destination group"
                    },
                    "start": {
                        "type": "boolean",
                        "description": "Start the tunnel immediately after adding"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    }
                },
                "required": ["name", "target"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_run_tunnel".to_string(),
            description: "Open a short-lived Access tunnel that closes by itself when ttl_minutes elapses. Prefer this over gtunnel_add_tunnel for temporary access; it is never public and leaves nothing to clean up. Stop it early with gtunnel_stop_tunnel".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Remote target as INSTANCE:PORT, e.g. my-vm:5432"
                    },
                    "ttl_minutes": {
                        "type": "integer",
                        "description": "Minutes until the tunnel closes by itself (1 to 480)"
                    },
                    "name": {
                        "type": "string",
                        "description": "Tunnel name (generated if omitted)"
                    },
                    "zone": {
                        "type": "string",
                        "description": "Compute zone of the instance"
                    },
                    "local_port": {
                        "type": "integer",
                        "description": "Local port to listen on (automatically allocated if omitted)"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    }
                },
                "required": ["target", "ttl_minutes"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_delete_tunnel".to_string(),
            description: "Delete a persistent tunnel, stopping its daemon and removing its service and log".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name to delete"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_list_accounts".to_string(),
            description: "List all configured accounts and show the currently selected active account".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "gtunnel_doctor".to_string(),
            description: "Run diagnostic checks on local configuration and GCP resources".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name"
                    },
                    "all": {
                        "type": "boolean",
                        "description": "Check every configured account"
                    }
                },
                "required": []
            }),
        },
    ]
}

pub fn all_resources() -> Vec<ResourceDefinition> {
    vec![
        ResourceDefinition {
            uri: "gtunnel://tunnels".to_string(),
            name: "All Configured Tunnels".to_string(),
            description: Some("Complete JSON snapshot of all persistent tunnels".to_string()),
            mime_type: Some("application/json".to_string()),
        },
        ResourceDefinition {
            uri: "gtunnel://accounts".to_string(),
            name: "All Configured Accounts".to_string(),
            description: Some(
                "Complete JSON snapshot of all configured accounts and selection".to_string(),
            ),
            mime_type: Some("application/json".to_string()),
        },
    ]
}

/// An unknown account is the caller's mistake, not a fault in the protocol, so
/// it comes back as a tool result carrying its kind (`not_found`). Only a
/// tool result has a `structuredContent` to put the kind in; a JSON-RPC error
/// frame has none.
fn resolve_for_tool(
    account: Option<&str>,
) -> Result<(crate::config::Config, crate::config::Account), ToolCallResult> {
    resolve(account).map_err(|e| ToolCallResult::failure(&e))
}

/// The not-found a name earns once neither a tunnel nor a live run has it.
/// The text comes from `find_tunnel`, so every command words it alike.
fn not_found(state: &TunnelState, name: &str, account: &str) -> ToolCallResult {
    match find_tunnel(state, name, account) {
        Err(e) => ToolCallResult::failure(&e),
        Ok(_) => ToolCallResult::failure(
            &crate::error::GtunnelError::new(
                crate::error::ErrorKind::Internal,
                format!("'{name}' was found by one lookup and missed by another"),
            )
            .into(),
        ),
    }
}

/// A probe that got no answer is named by the log: expired credentials are
/// `auth_expired`, not just `unhealthy`. A crash loop needs restarts over
/// time, which one call cannot see.
fn health_word(
    running: bool,
    probe_latency_ms: Option<u128>,
    access: bool,
    log: &std::path::Path,
) -> &'static str {
    match (running, probe_latency_ms) {
        (true, Some(_)) => "healthy",
        (true, None) if access => match probe::failed_health(log) {
            probe::HealthStatus::AuthExpired => "auth_expired",
            _ => "unhealthy",
        },
        _ => "unknown",
    }
}

/// The last `n` lines of `text`, oldest first.
fn last_lines(text: &str, n: usize) -> String {
    let mut tail: Vec<&str> = text.lines().rev().take(n).collect();
    tail.reverse();
    tail.join("\n")
}

/// Status of a live run. It has no daemon, so pid and expiry come from its
/// record, and a run that exists is by definition running. A public run has
/// no local port to probe, so its health stays unknown rather than guessed.
async fn run_status(run: &crate::ephemeral::EphemeralRecord) -> ToolCallResult {
    let tunnel = run.as_tunnel();
    let log = super::run::run_log_path(&run.account_name, &run.name).unwrap_or_default();
    let (local_port, probe_latency_ms) = match &run.spec {
        TunnelSpec::Access { local_port, .. } => (
            Some(*local_port),
            probe::probe_tunnel(
                &format!("127.0.0.1:{local_port}"),
                Duration::from_millis(500),
                &log,
            )
            .await
            .map(|d| d.as_millis()),
        ),
        TunnelSpec::Expose { .. } => (None, None),
    };
    let health = health_word(
        true,
        probe_latency_ms,
        matches!(run.spec, TunnelSpec::Access { .. }),
        &log,
    );
    let payload = json!({
        "name": run.name,
        "account": run.account_name,
        "run": true,
        "status": "running",
        "health": health,
        "pid": run.pid,
        "local_port": local_port,
        "expires_at": run.expires_at(),
        "probe_latency_ms": probe_latency_ms,
        "url": tunnel.display_url(),
        "target": tunnel.display_target(),
    });
    ToolCallResult::text(serde_json::to_string_pretty(&payload).unwrap_or_default())
}

pub async fn execute_tool(
    name: &str,
    params: Option<&Value>,
) -> Result<ToolCallResult, JsonRpcError> {
    let empty_map = serde_json::Map::new();
    let map = params.and_then(Value::as_object).unwrap_or(&empty_map);

    let get_str = |key: &str| map.get(key).and_then(Value::as_str);
    let get_usize = |key: &str| map.get(key).and_then(Value::as_u64).map(|n| n as usize);
    let get_bool = |key: &str| map.get(key).and_then(Value::as_bool).unwrap_or(false);

    match name {
        "gtunnel_list_tunnels" => {
            let account = get_str("account");
            let (_, acct) = match resolve_for_tool(account) {
                Ok(r) => r,
                Err(fail) => return Ok(fail),
            };
            let state = TunnelState::load()
                .map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            let tunnels = state.tunnels_for_account(&acct.name);

            let mut results = Vec::new();
            for t in tunnels {
                let label = daemon::platform_label(&t.account_name, &t.name);
                let status = daemon::status(&label).await;
                results.push(json!({
                    "name": t.name,
                    "account": t.account_name,
                    "status": match status {
                        TunnelStatus::Running => "running",
                        TunnelStatus::Stopped => "stopped",
                        TunnelStatus::Error => "error",
                    },
                    "mode": t.display_kind(),
                    "url": t.display_url(),
                    "target": t.display_target(),
                    "auto_start": t.auto_start,
                    "run": false,
                }));
            }
            for run in crate::ephemeral::live_runs(&acct.name).unwrap_or_default() {
                let t = run.as_tunnel();
                let mut row = json!({
                    "name": t.name,
                    "account": t.account_name,
                    "status": "running",
                    "mode": t.display_kind(),
                    "url": t.display_url(),
                    "target": t.display_target(),
                    "auto_start": false,
                    "run": true,
                });
                if let Some(expires_at) = run.expires_at() {
                    row["expires_at"] = json!(expires_at);
                }
                results.push(row);
            }
            Ok(ToolCallResult::text(
                serde_json::to_string_pretty(&results).unwrap_or_default(),
            ))
        }
        "gtunnel_start_tunnel" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let account = get_str("account");
            // A failed tool is a result the model can read and act on, not a
            // protocol fault: `{e:#}` keeps the whole cause chain
            Ok(match start_tunnel(account, tunnel_name).await {
                Ok(_) => ToolCallResult::text(format!("Started tunnel '{tunnel_name}'")),
                Err(e) => ToolCallResult::failure(&e),
            })
        }
        "gtunnel_stop_tunnel" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let account = get_str("account");
            Ok(match stop_by_name(account, tunnel_name).await {
                Ok(Stopped::Run) => ToolCallResult::text(format!("Stopped run '{tunnel_name}'")),
                Ok(Stopped::Tunnel) => {
                    ToolCallResult::text(format!("Stopped tunnel '{tunnel_name}'"))
                }
                Err(e) => ToolCallResult::failure(&e),
            })
        }
        "gtunnel_restart_tunnel" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let account = get_str("account");
            Ok(match restart_tunnel(account, tunnel_name).await {
                Ok(_) => ToolCallResult::text(format!("Restarted tunnel '{tunnel_name}'")),
                Err(e) => ToolCallResult::failure(&e),
            })
        }
        "gtunnel_get_status" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let account = get_str("account");
            let (_, acct) = match resolve_for_tool(account) {
                Ok(r) => r,
                Err(fail) => return Ok(fail),
            };
            let state = TunnelState::load()
                .map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            // A persistent tunnel of that name wins; without one, a live run
            // of it, and only then the not-found
            let t = match state.find_for_account(tunnel_name, &acct.name) {
                Some(t) => t.clone(),
                None => {
                    if let Some(run) = super::run::find_live_run(&acct.name, tunnel_name) {
                        return Ok(run_status(&run).await);
                    }
                    return Ok(not_found(&state, tunnel_name, &acct.name));
                }
            };

            let label = daemon::platform_label(&t.account_name, &t.name);
            let status = daemon::status(&label).await;
            let runtime = daemon::runtime_info(&label).await;

            let probe_latency_ms = if status == TunnelStatus::Running {
                if let TunnelSpec::Access { local_port, .. } = &t.spec {
                    // No log to read means a silent close is taken as an answer
                    let log = t.log_path().unwrap_or_default();
                    probe::probe_tunnel(
                        &format!("127.0.0.1:{local_port}"),
                        Duration::from_millis(500),
                        &log,
                    )
                    .await
                    .map(|d| d.as_millis())
                } else {
                    None
                }
            } else {
                None
            };

            let health = health_word(
                status == TunnelStatus::Running,
                probe_latency_ms,
                matches!(t.spec, TunnelSpec::Access { .. }),
                &t.log_path().unwrap_or_default(),
            );
            let payload = json!({
                "name": t.name,
                "account": t.account_name,
                "run": false,
                "health": health,
                "status": match status {
                    TunnelStatus::Running => "running",
                    TunnelStatus::Stopped => "stopped",
                    TunnelStatus::Error => "error",
                },
                "pid": runtime.pid,
                "restarts": runtime.restarts,
                "probe_latency_ms": probe_latency_ms,
                "url": t.display_url(),
                "target": t.display_target(),
            });
            Ok(ToolCallResult::text(
                serde_json::to_string_pretty(&payload).unwrap_or_default(),
            ))
        }
        "gtunnel_get_logs" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let lines = get_usize("lines").unwrap_or(50);
            let account = get_str("account");
            let (_, acct) = match resolve_for_tool(account) {
                Ok(r) => r,
                Err(fail) => return Ok(fail),
            };
            let state = TunnelState::load()
                .map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            // The same order as stop and status: a persistent tunnel, then a
            // live run, then not-found
            let log_path = match state.find_for_account(tunnel_name, &acct.name) {
                Some(t) => t
                    .log_path()
                    .map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?,
                None => match super::run::live_run_log(&acct.name, tunnel_name) {
                    Some(Ok(path)) => path,
                    Some(Err(e)) => return Ok(ToolCallResult::failure(&e)),
                    None => return Ok(not_found(&state, tunnel_name, &acct.name)),
                },
            };
            let text = daemon::read_tail(&log_path, daemon::TAIL_WINDOW);
            Ok(ToolCallResult::text(last_lines(&text, lines)))
        }
        "gtunnel_add_tunnel" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let target = get_str("target")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'target'"))?;
            let via = get_str("via");
            let zone = get_str("zone");
            let local_port = super::run::parse_port(map, "local_port")
                .map_err(|e| JsonRpcError::new(INVALID_PARAMS, e))?;
            let dest_group = get_str("dest_group");
            let region = get_str("region");
            let network = get_str("network");
            let start = get_bool("start");
            let account = get_str("account");

            let req = AddRequest {
                name: tunnel_name,
                target,
                via,
                zone,
                local_port,
                dest_group,
                region,
                network,
                start,
            };
            req.validate()
                .map_err(|e| JsonRpcError::new(INVALID_PARAMS, format!("{e:#}")))?;
            // The record is saved before a start is tried, so an error here
            // may describe a tunnel that exists: the text says what failed
            Ok(match add_tunnel(account, req).await {
                Ok(_) => ToolCallResult::text(format!(
                    "Added tunnel '{tunnel_name}' for target '{target}'"
                )),
                Err(e) => ToolCallResult::failure(&e),
            })
        }
        "gtunnel_run_tunnel" => {
            let req = match super::run::RunRequest::parse(map) {
                Ok(req) => req,
                Err(e) => return Err(JsonRpcError::new(INVALID_PARAMS, e)),
            };
            let (_, acct) = match resolve_for_tool(get_str("account")) {
                Ok(r) => r,
                Err(fail) => return Ok(fail),
            };
            Ok(match super::run::launch(&acct.name, &req).await {
                Ok(started) => {
                    ToolCallResult::text(serde_json::to_string_pretty(&started).unwrap_or_default())
                }
                Err(e) => ToolCallResult::failure(&e),
            })
        }
        "gtunnel_delete_tunnel" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let account = get_str("account");
            Ok(match delete_tunnel(account, tunnel_name).await {
                Ok(()) => ToolCallResult::text(format!("Deleted tunnel '{tunnel_name}'")),
                Err(e) => ToolCallResult::failure(&e),
            })
        }
        "gtunnel_list_accounts" => {
            let config =
                load_config().map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            let payload = json!({
                "selected_account": config.selected_account,
                "accounts": config.accounts.iter().map(|a| json!({
                    "name": a.name,
                    "project": a.project,
                    "default_zone": a.default_zone,
                    "gcloud_configuration": a.gcloud_configuration,
                    "selected": a.name == config.selected_account,
                })).collect::<Vec<Value>>(),
            });
            Ok(ToolCallResult::text(
                serde_json::to_string_pretty(&payload).unwrap_or_default(),
            ))
        }
        "gtunnel_doctor" => {
            let account = get_str("account");
            let all = get_bool("all");
            let (config, acct) = match resolve_for_tool(account) {
                Ok(r) => r,
                Err(fail) => return Ok(fail),
            };
            let state = TunnelState::load()
                .map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            let accounts: Vec<&crate::config::Account> = if all {
                config.accounts.iter().collect()
            } else {
                vec![config
                    .accounts
                    .iter()
                    .find(|a| a.name == acct.name)
                    .unwrap_or(&acct)]
            };

            let findings = run_doctor(&config, &state, &accounts).await;
            let json_findings: Vec<Value> = findings
                .iter()
                .map(|(acct_name, finding)| {
                    json!({
                        "account": acct_name,
                        "description": finding.describe(),
                        "fixable": finding.fixable(),
                    })
                })
                .collect();
            Ok(ToolCallResult::text(
                serde_json::to_string_pretty(&json_findings).unwrap_or_default(),
            ))
        }
        _ => Err(JsonRpcError::new(
            super::protocol::METHOD_NOT_FOUND,
            format!("Unknown tool '{name}'"),
        )),
    }
}

pub fn read_resource(uri: &str) -> Result<ResourceReadResult, JsonRpcError> {
    match uri {
        "gtunnel://tunnels" => {
            let state = TunnelState::load()
                .map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            let text = serde_json::to_string_pretty(&state.tunnels).unwrap_or_default();
            Ok(ResourceReadResult {
                contents: vec![ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text,
                }],
            })
        }
        "gtunnel://accounts" => {
            let config =
                load_config().map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            let text = serde_json::to_string_pretty(&config.accounts).unwrap_or_default();
            Ok(ResourceReadResult {
                contents: vec![ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text,
                }],
            })
        }
        _ => Err(JsonRpcError::new(
            INVALID_PARAMS,
            format!("Resource '{uri}' not found"),
        )),
    }
}
