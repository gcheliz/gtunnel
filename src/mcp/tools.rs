use serde_json::{json, Value};

use super::protocol::{
    JsonRpcError, ResourceContent, ResourceDefinition, ResourceReadResult, ToolAnnotations,
    ToolCallResult, ToolDefinition, INTERNAL_ERROR, INVALID_PARAMS,
};
use crate::commands::access::{
    add_tunnel, declared_health, delete_tunnel, health_word, list_rows_json, probe_local,
    restart_tunnel, start_tunnel, stop_by_name, stopped_for_credentials, AddRequest, Stopped,
};
use crate::commands::expose::{expose_tunnel, ExposeRequest, Exposed};
use crate::commands::relocate::{move_tunnel, rename_tunnel};
use crate::commands::{find_tunnel, resolve};
use crate::config::load_config;
use crate::daemon;
use crate::doctor::run_doctor;
use crate::state::{TunnelSpec, TunnelState, TunnelStatus};

/// The `{"error":{"kind","message"}}` envelope `ToolCallResult::failure`
/// attaches. It sits in every schema because a failed call must conform too.
fn error_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {"type": "string"},
            "message": {"type": "string"}
        },
        "required": ["kind", "message"]
    })
}

/// A result is either the success shape or the error envelope. `anyOf` over
/// `required` keeps both sets of properties declared in one object schema, which
/// is what the spec asks of `outputSchema`. No `additionalProperties`: a field
/// added later must not turn a validating client's check into a failure.
fn output_schema(success_properties: Value, success_required: &[&str]) -> Value {
    let mut properties = success_properties;
    properties["error"] = error_schema();
    json!({
        "type": "object",
        "properties": properties,
        "anyOf": [
            required_branch(success_required),
            required_branch(&["error"])
        ]
    })
}

/// An `anyOf` branch that names its required keys in its own `properties` too:
/// Ajv's `strictRequired` refuses a `required` key the same subschema does not
/// declare, and a client that opts into strict mode then fails `tools/list`.
fn required_branch(keys: &[&str]) -> Value {
    let stubs: serde_json::Map<String, Value> =
        keys.iter().map(|k| (k.to_string(), json!({}))).collect();
    json!({"required": keys, "properties": stubs})
}

fn nullable(ty: &str) -> Value {
    json!({"type": [ty, "null"]})
}

fn list_tunnels_schema() -> Value {
    output_schema(
        json!({
            "tunnels": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "status": {"type": "string"},
                        "health": {"type": "string"},
                        "mode": {"type": "string"},
                        "url": {"type": "string"},
                        "target": {"type": "string"},
                        "account": {"type": "string"},
                        "auto_start": {"type": "boolean"},
                        "unmanaged": {"type": "boolean"},
                        "run": {"type": "boolean"},
                        "expires_at": {"type": "integer"},
                        "local_port": nullable("integer")
                    },
                    "required": ["name", "status", "health"]
                }
            }
        }),
        &["tunnels"],
    )
}

fn get_status_schema() -> Value {
    output_schema(
        json!({
            "name": {"type": "string"},
            "account": {"type": "string"},
            "run": {"type": "boolean"},
            "status": {"type": "string"},
            "health": {"type": "string"},
            "pid": nullable("integer"),
            "restarts": {"type": "integer"},
            "local_port": nullable("integer"),
            "expires_at": nullable("integer"),
            "probe_latency_ms": nullable("number"),
            "url": nullable("string"),
            "target": {"type": "string"}
        }),
        &["name", "account", "run", "status", "health"],
    )
}

fn run_tunnel_schema() -> Value {
    output_schema(
        json!({
            "name": {"type": "string"},
            "account": {"type": "string"},
            "url": nullable("string"),
            "local_port": nullable("integer"),
            "target": {"type": "string"},
            "ttl_minutes": nullable("integer"),
            "expires_at": nullable("integer")
        }),
        &["name", "account", "target"],
    )
}

fn expose_tunnel_schema() -> Value {
    output_schema(
        json!({
            "name": {"type": "string"},
            "account": {"type": "string"},
            "url": {"type": "string"},
            "target": {"type": "string"},
            "domain": {"type": "string"},
            "public": {"type": "boolean"},
            "certificate_state": {"type": "string"},
            "started": {"type": "boolean"}
        }),
        &["name", "account", "url", "public", "started"],
    )
}

fn list_accounts_schema() -> Value {
    output_schema(
        json!({
            "selected_account": {"type": "string"},
            "accounts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "project": {"type": "string"},
                        "default_zone": {"type": "string"},
                        "gcloud_configuration": {"type": "string"},
                        "selected": {"type": "boolean"}
                    },
                    "required": ["name", "project", "selected"]
                }
            }
        }),
        &["selected_account", "accounts"],
    )
}

fn doctor_schema() -> Value {
    output_schema(
        json!({
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "account": {"type": "string"},
                        "description": {"type": "string"},
                        "fixable": {"type": "boolean"}
                    },
                    "required": ["account", "description", "fixable"]
                }
            }
        }),
        &["findings"],
    )
}

pub fn all_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "gtunnel_list_tunnels".to_string(),
            title: "List tunnels".to_string(),
            annotations: ToolAnnotations::new(true,false,true,false),
            output_schema: Some(list_tunnels_schema()),
            description: "List all configured tunnels and live short-lived runs (run: true, with expires_at when they have a TTL) for the given or default account, each with its status, local_port (null on a public tunnel) and probed health (healthy, unhealthy, auth_expired or unknown)".to_string(),
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
            title: "Start tunnel".to_string(),
            annotations: ToolAnnotations::new(false,false,true,true),
            output_schema: None,
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
            title: "Stop tunnel".to_string(),
            annotations: ToolAnnotations::new(false,true,true,false),
            output_schema: None,
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
            title: "Restart tunnel".to_string(),
            annotations: ToolAnnotations::new(false,true,true,true),
            output_schema: None,
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
            title: "Get tunnel status".to_string(),
            annotations: ToolAnnotations::new(true,false,true,false),
            output_schema: Some(get_status_schema()),
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
            title: "Read tunnel logs".to_string(),
            annotations: ToolAnnotations::new(true,false,true,false),
            output_schema: None,
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
            title: "Add tunnel".to_string(),
            annotations: ToolAnnotations::new(false,false,false,true),
            output_schema: None,
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
            title: "Run short-lived tunnel".to_string(),
            annotations: ToolAnnotations::new(false,false,false,true),
            output_schema: Some(run_tunnel_schema()),
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
            title: "Delete tunnel".to_string(),
            annotations: ToolAnnotations::new(false,true,false,false),
            output_schema: None,
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
            name: "gtunnel_rename_tunnel".to_string(),
            title: "Rename tunnel".to_string(),
            annotations: ToolAnnotations::new(false,true,false,true),
            output_schema: None,
            description: "Rename a tunnel on its account, moving its service and log with it; a running tunnel is restarted under the new name".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Current tunnel name"
                    },
                    "new_name": {
                        "type": "string",
                        "description": "New tunnel name"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional GCP account name the tunnel is on"
                    }
                },
                "required": ["name", "new_name"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_move_tunnel".to_string(),
            title: "Move tunnel to another account".to_string(),
            annotations: ToolAnnotations::new(false,true,false,true),
            output_schema: None,
            description: "Move an Access tunnel to another configured account, checking the target exists there first; `rename` gives it a new name when the destination already uses this one".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name to move"
                    },
                    "to": {
                        "type": "string",
                        "description": "Destination account name"
                    },
                    "rename": {
                        "type": "string",
                        "description": "Name to give the tunnel on the destination"
                    },
                    "account": {
                        "type": "string",
                        "description": "Optional source account name"
                    }
                },
                "required": ["name", "to"]
            }),
        },
        ToolDefinition {
            name: "gtunnel_expose_tunnel".to_string(),
            title: "Expose a local service".to_string(),
            annotations: ToolAnnotations::new(false,true,false,true),
            output_schema: Some(expose_tunnel_schema()),
            description: "Publish a local service at https://<name>.<domain> through the account's relay. Private by default: visitors must sign in with Google (IAP). public: true makes it reachable by anyone on the internet. Publishes only a service on this machine (a port, localhost or a loopback address); other hosts need the CLI. Needs a relay (gtunnel relay up, CLI only)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tunnel name, also the hostname's first label"
                    },
                    "target": {
                        "type": "string",
                        "description": "Local service to publish: a port (3000) or localhost:PORT; other hosts are refused"
                    },
                    "domain": {
                        "type": "string",
                        "description": "Registered relay domain (defaults to the account's default domain)"
                    },
                    "public": {
                        "type": "boolean",
                        "description": "true makes the service reachable by anyone; false or omitted keeps it behind Google sign-in. This is the reverse of the CLI, which is public unless --private"
                    },
                    "start": {
                        "type": "boolean",
                        "description": "Start the tunnel after creating it"
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
            name: "gtunnel_list_accounts".to_string(),
            title: "List accounts".to_string(),
            annotations: ToolAnnotations::new(true,false,true,false),
            output_schema: Some(list_accounts_schema()),
            description: "List all configured accounts and show the currently selected active account".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "gtunnel_doctor".to_string(),
            title: "Run diagnostics".to_string(),
            annotations: ToolAnnotations::new(true,false,true,true),
            output_schema: Some(doctor_schema()),
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
                "The selected account and every configured account, as gtunnel_list_accounts returns them"
                    .to_string(),
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

/// The MCP default is the safe one: `public` is read as a strict boolean and a
/// missing value means private, the reverse of the CLI's public-unless-`--private`.
/// A string `"true"` is refused rather than coerced, because a wrong guess here
/// publishes a service to the internet.
fn expose_request(
    args: &serde_json::Map<String, Value>,
) -> Result<ExposeRequest<'_>, JsonRpcError> {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    let public = match args.get("public") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(JsonRpcError::new(
                INVALID_PARAMS,
                "Parameter 'public' must be of type boolean",
            ))
        }
    };
    let target =
        text("target").ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'target'"))?;
    require_local_target(target)?;
    Ok(ExposeRequest {
        name: text("name").ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?,
        target,
        domain: text("domain"),
        private: !public,
    })
}

/// An agent that can publish any host:port can publish a LAN machine or a
/// cloud metadata endpoint to the internet, so over MCP only this machine is
/// allowed; the CLI, where a person types the target, keeps any host. The host
/// is parsed as an address, not prefix-matched, so `127.0.0.1.evil.com` is a
/// hostname and fails. IPv6 needs brackets, since `::1:3000` has no single reading.
fn require_local_target(target: &str) -> Result<(), JsonRpcError> {
    const ONLY_LOCAL: &str = "gtunnel_expose_tunnel publishes only a service on this machine: use a port (3000) or localhost:PORT; other hosts need the CLI";
    let normalized = crate::tunnel::normalize_local_target(target)
        .map_err(|e| JsonRpcError::new(INVALID_PARAMS, format!("{e:#}")))?;
    let host = normalized
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or_default();
    let local = if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        inner
            .parse::<std::net::Ipv6Addr>()
            .is_ok_and(|a| a.is_loopback())
    } else if host.contains(':') {
        false
    } else {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|a| a.is_loopback())
    };
    if local {
        Ok(())
    } else {
        Err(JsonRpcError::new(INVALID_PARAMS, ONLY_LOCAL))
    }
}

/// The route and record exist by the time a start is tried, so a failed start
/// must not read as a failed expose: the text says what was created and which
/// tool finishes the job. `start` is `None` when none was asked for.
fn expose_result(exposed: &Exposed, start: Option<anyhow::Result<()>>) -> ToolCallResult {
    if let Some(Err(err)) = start {
        let err = err.context(format!(
            "Tunnel '{}' was created at {} but not started; start it with gtunnel_start_tunnel",
            exposed.name, exposed.url
        ));
        return ToolCallResult::failure(&err);
    }
    let mut payload = serde_json::to_value(exposed).unwrap_or(Value::Null);
    payload["started"] = json!(matches!(start, Some(Ok(()))));
    ToolCallResult::json(&payload, payload.clone())
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
        TunnelSpec::Access { local_port, .. } => {
            (Some(*local_port), probe_local(*local_port, &log).await)
        }
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
    ToolCallResult::json(&payload, payload.clone())
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
            // The rows `list --json` prints, less the unmanaged ones: an agent
            // cannot start, stop or read the log of a process gtunnel does
            // not own, so naming it would offer an action that fails
            let rows = list_rows_json(&acct, false)
                .await
                .map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            // The text stays the bare array agents parse today; the spec
            // wants an object in `structuredContent`, so that one is wrapped
            Ok(ToolCallResult::json(&rows, json!({ "tunnels": rows })))
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
                    probe_local(*local_port, &t.log_path().unwrap_or_default()).await
                } else {
                    None
                }
            } else {
                None
            };

            let health = declared_health(
                status == TunnelStatus::Running,
                status != TunnelStatus::Running && stopped_for_credentials(&t),
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
            Ok(ToolCallResult::json(&payload, payload.clone()))
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
                Ok(started) => ToolCallResult::json(&started, started.clone()),
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
        "gtunnel_rename_tunnel" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let new_name = get_str("new_name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'new_name'"))?;
            Ok(
                match rename_tunnel(get_str("account"), tunnel_name, new_name).await {
                    Ok(done) => ToolCallResult::text(done.message()),
                    Err(e) => ToolCallResult::failure(&e),
                },
            )
        }
        "gtunnel_move_tunnel" => {
            let tunnel_name = get_str("name")
                .ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'name'"))?;
            let to =
                get_str("to").ok_or_else(|| JsonRpcError::new(INVALID_PARAMS, "Missing 'to'"))?;
            Ok(
                match move_tunnel(tunnel_name, get_str("account"), to, get_str("rename")).await {
                    Ok(done) => ToolCallResult::text(done.message()),
                    Err(e) => ToolCallResult::failure(&e),
                },
            )
        }
        "gtunnel_expose_tunnel" => {
            let req = expose_request(map)?;
            let account = get_str("account");
            // `start_tunnel`, not `cmd_start`: the core and the start both stay
            // off stdout, which belongs to the protocol frames
            Ok(match expose_tunnel(account, &req).await {
                Ok(exposed) => {
                    let started = if get_bool("start") {
                        Some(start_tunnel(account, &exposed.name).await.map(|_| ()))
                    } else {
                        None
                    };
                    expose_result(&exposed, started)
                }
                Err(e) => ToolCallResult::failure(&e),
            })
        }
        "gtunnel_list_accounts" => {
            let config =
                load_config().map_err(|e| JsonRpcError::new(INTERNAL_ERROR, e.to_string()))?;
            let payload = accounts_payload(&config);
            Ok(ToolCallResult::json(&payload, payload.clone()))
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
            Ok(ToolCallResult::json(
                &json_findings,
                json!({ "findings": json_findings }),
            ))
        }
        _ => Err(JsonRpcError::new(
            super::protocol::METHOD_NOT_FOUND,
            format!("Unknown tool '{name}'"),
        )),
    }
}

/// One function for the tool and the resource, so the two cannot drift apart
/// again: the resource once served a bare array with no selection while the
/// tool served the selection, and the docs promised the second for both.
/// `impersonate_service_account` and `expose` ride along only when set, so a
/// client that read them from the resource keeps them after the shapes merged.
fn accounts_payload(config: &crate::config::Config) -> Value {
    json!({
        "selected_account": config.selected_account,
        "accounts": config.accounts.iter().map(|a| {
            let mut entry = json!({
                "name": a.name,
                "project": a.project,
                "default_zone": a.default_zone,
                "gcloud_configuration": a.gcloud_configuration,
                "selected": a.name == config.selected_account,
            });
            if let Some(sa) = &a.impersonate_service_account {
                entry["impersonate_service_account"] = json!(sa);
            }
            if let Some(expose) = &a.expose {
                entry["expose"] = serde_json::to_value(expose).unwrap_or(Value::Null);
            }
            entry
        }).collect::<Vec<Value>>(),
    })
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
            let text = serde_json::to_string_pretty(&accounts_payload(&config)).unwrap_or_default();
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

#[cfg(test)]
mod schema_tests {
    use super::*;
    use crate::ephemeral::EphemeralRecord;
    use crate::state::AccessTarget;

    fn schema_of(tool: &str) -> Value {
        all_tools()
            .into_iter()
            .find(|t| t.name == tool)
            .and_then(|t| t.output_schema)
            .expect("the tool declares an outputSchema")
    }

    fn record(ttl_secs: Option<u64>) -> EphemeralRecord {
        EphemeralRecord {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            pid: 4242,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "my-vm".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
            started_at: 1_700_000_000,
            ttl_secs,
            pid_start: None,
        }
    }

    #[test]
    fn a_launched_run_matches_the_run_tunnel_schema() {
        let validator = jsonschema::validator_for(&schema_of("gtunnel_run_tunnel")).unwrap();
        for ttl in [Some(30 * 60), None] {
            let payload = super::super::run::describe(&record(ttl));
            assert!(validator.is_valid(&payload), "{payload}");
        }
    }

    #[test]
    fn the_error_envelope_matches_every_output_schema() {
        let err: anyhow::Error = crate::error::GtunnelError::not_found("no such tunnel").into();
        let failure = serde_json::to_value(ToolCallResult::failure(&err)).unwrap();
        for tool in all_tools() {
            let Some(schema) = tool.output_schema else {
                continue;
            };
            let validator = jsonschema::validator_for(&schema).unwrap();
            assert!(
                validator.is_valid(&failure["structuredContent"]),
                "{} rejects its own failure",
                tool.name
            );
        }
    }

    #[test]
    fn a_payload_with_neither_shape_is_rejected() {
        let validator = jsonschema::validator_for(&schema_of("gtunnel_list_accounts")).unwrap();
        assert!(!validator.is_valid(&json!({"unrelated": true})));
    }

    // Ajv `strictRequired`: a client in strict mode fails `tools/list` on a
    // branch that requires a key its own `properties` does not name
    #[test]
    fn every_any_of_branch_declares_the_keys_it_requires() {
        for tool in all_tools() {
            let Some(schema) = tool.output_schema else {
                continue;
            };
            for branch in schema["anyOf"].as_array().unwrap() {
                for key in branch["required"].as_array().unwrap() {
                    let key = key.as_str().unwrap();
                    assert!(
                        branch["properties"].get(key).is_some(),
                        "{} requires {key} without declaring it",
                        tool.name
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod expose_tests {
    use super::*;
    use crate::commands::expose::Exposed;

    fn args(value: Value) -> serde_json::Map<String, Value> {
        let mut map = value.as_object().cloned().unwrap();
        map.entry("name").or_insert(json!("web"));
        map.entry("target").or_insert(json!("3000"));
        map
    }

    fn exposed() -> Exposed {
        Exposed {
            name: "web".to_string(),
            account: "dev".to_string(),
            url: "https://web.example.com".to_string(),
            target: "localhost:3000".to_string(),
            domain: "example.com".to_string(),
            public: false,
            certificate_state: "ACTIVE".to_string(),
        }
    }

    #[test]
    fn expose_is_private_unless_public_is_true() {
        for (given, private) in [
            (json!({}), true),
            (json!({"public": false}), true),
            (json!({"public": true}), false),
        ] {
            let map = args(given.clone());
            let req = expose_request(&map).unwrap();
            assert_eq!(req.private, private, "{given}");
        }
    }

    #[test]
    fn expose_accepts_only_a_service_on_this_machine() {
        for target in [
            "3000",
            "localhost:3000",
            "127.0.0.1:3000",
            "127.9.9.9:3000",
            "[::1]:3000",
            "http://localhost:3000/app",
        ] {
            let map = args(json!({"target": target}));
            assert!(expose_request(&map).is_ok(), "{target} should be accepted");
        }
        for target in [
            "10.0.0.10:80",
            "169.254.169.254:80",
            "127.0.0.1.evil.com:80",
            "db.internal:5432",
            "::1:3000",
            "user@127.0.0.1:80",
            "http://10.0.0.10:80/x",
        ] {
            let map = args(json!({"target": target}));
            let err = expose_request(&map)
                .err()
                .unwrap_or_else(|| panic!("{target} should be rejected"));
            assert_eq!(err.code, INVALID_PARAMS, "{target}");
        }
        let map = args(json!({"target": "10.0.0.10:80"}));
        let err = expose_request(&map).err().unwrap();
        assert!(
            err.message
                .contains("publishes only a service on this machine"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_public_that_is_not_a_boolean_is_never_coerced() {
        let map = args(json!({"public": "true"}));
        let err = expose_request(&map).err().expect("rejected");
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[test]
    fn a_failed_start_after_expose_is_a_failure_naming_the_tunnel() {
        let result = expose_result(&exposed(), Some(Err(anyhow::anyhow!("login expired"))));
        assert_eq!(result.is_error, Some(true));
        let text = &result.content[0].text;
        assert!(text.contains("web"), "{text}");
        assert!(text.contains("gtunnel_start_tunnel"), "{text}");
        assert!(text.contains("login expired"), "{text}");
    }

    #[test]
    fn a_started_expose_reports_started_and_the_exposed_fields() {
        let result = expose_result(&exposed(), Some(Ok(())));
        assert_eq!(result.is_error, None);
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["started"], json!(true));
        assert_eq!(structured["url"], json!("https://web.example.com"));
        assert_eq!(structured["public"], json!(false));

        let skipped = expose_result(&exposed(), None);
        assert_eq!(skipped.structured_content.unwrap()["started"], json!(false));
    }

    #[test]
    fn an_expose_result_matches_the_expose_schema() {
        let schema = all_tools()
            .into_iter()
            .find(|t| t.name == "gtunnel_expose_tunnel")
            .and_then(|t| t.output_schema)
            .expect("the tool declares an outputSchema");
        let required = schema["anyOf"][0]["required"].as_array().unwrap();
        assert!(required.contains(&json!("started")), "{required:?}");
        let validator = jsonschema::validator_for(&schema).unwrap();
        let ok = expose_result(&exposed(), None).structured_content.unwrap();
        assert!(validator.is_valid(&ok), "{ok}");
        let started = expose_result(&exposed(), Some(Ok(())))
            .structured_content
            .unwrap();
        assert!(validator.is_valid(&started), "{started}");
        assert_eq!(started["started"], true);
    }
}
