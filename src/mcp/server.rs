use anyhow::Result;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::protocol::{
    InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse, ResourcesCapability,
    ServerCapabilities, ServerInfo, ToolsCapability, INVALID_PARAMS, METHOD_NOT_FOUND, PARSE_ERROR,
};
use super::tools::{all_resources, all_tools, execute_tool, read_resource};
use super::validator::ToolValidator;

/// Handles a single incoming JSON-RPC 2.0 message line.
/// Returns Some(response) if a response is expected, or None for notifications.
pub async fn handle_message(line: &str) -> Option<JsonRpcResponse> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
        Ok(r) => r,
        Err(e) => {
            return Some(JsonRpcResponse::error(
                None,
                JsonRpcError::new(PARSE_ERROR, format!("Parse error: {e}")),
            ));
        }
    };

    let id = req.id.clone();

    match req.method.as_str() {
        "initialize" => {
            let requested = req
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(serde_json::Value::as_str);
            let result = InitializeResult {
                protocol_version: super::protocol::negotiate_version(requested).to_string(),
                capabilities: ServerCapabilities {
                    tools: Some(ToolsCapability {
                        list_changed: Some(false),
                    }),
                    resources: Some(ResourcesCapability {
                        subscribe: Some(false),
                        list_changed: Some(false),
                    }),
                },
                server_info: ServerInfo {
                    name: "gtunnel".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
            };
            Some(JsonRpcResponse::success(id, json!(result)))
        }
        // JSON-RPC forbids a reply to a notification, and clients send
        // cancelled and progress ones the server has no use for
        m if m.starts_with("notifications/") => None,
        "ping" => Some(JsonRpcResponse::success(id, json!({}))),
        "tools/list" => {
            let tools = all_tools();
            Some(JsonRpcResponse::success(id, json!({ "tools": tools })))
        }
        "tools/call" => {
            let params = match req.params.as_ref() {
                Some(p) if p.is_object() => p,
                _ => {
                    return Some(JsonRpcResponse::error(
                        id,
                        JsonRpcError::new(INVALID_PARAMS, "Missing or invalid 'params' object"),
                    ));
                }
            };

            let tool_name = match params.get("name").and_then(serde_json::Value::as_str) {
                Some(n) => n,
                None => {
                    return Some(JsonRpcResponse::error(
                        id,
                        JsonRpcError::new(INVALID_PARAMS, "Missing 'name' in tool call parameters"),
                    ));
                }
            };

            let tools = all_tools();
            let tool_def = match tools.iter().find(|t| t.name == tool_name) {
                Some(t) => t,
                None => {
                    return Some(JsonRpcResponse::error(
                        id,
                        JsonRpcError::new(
                            METHOD_NOT_FOUND,
                            format!("Tool '{tool_name}' not found"),
                        ),
                    ));
                }
            };

            let arguments = params.get("arguments");
            if let Err(val_err) = ToolValidator::validate_call_params(tool_def, arguments) {
                return Some(JsonRpcResponse::error(id, val_err));
            }

            match execute_tool(tool_name, arguments).await {
                Ok(res) => Some(JsonRpcResponse::success(id, json!(res))),
                Err(err) => Some(JsonRpcResponse::error(id, err)),
            }
        }
        "resources/list" => {
            let resources = all_resources();
            Some(JsonRpcResponse::success(
                id,
                json!({ "resources": resources }),
            ))
        }
        "resources/read" => {
            let params = match req.params.as_ref() {
                Some(p) if p.is_object() => p,
                _ => {
                    return Some(JsonRpcResponse::error(
                        id,
                        JsonRpcError::new(INVALID_PARAMS, "Missing or invalid 'params' object"),
                    ));
                }
            };

            let uri = match params.get("uri").and_then(serde_json::Value::as_str) {
                Some(u) => u,
                None => {
                    return Some(JsonRpcResponse::error(
                        id,
                        JsonRpcError::new(
                            INVALID_PARAMS,
                            "Missing 'uri' in resource read parameters",
                        ),
                    ));
                }
            };

            match read_resource(uri) {
                Ok(res) => Some(JsonRpcResponse::success(id, json!(res))),
                Err(err) => Some(JsonRpcResponse::error(id, err)),
            }
        }
        other => Some(JsonRpcResponse::error(
            id,
            JsonRpcError::new(METHOD_NOT_FOUND, format!("Method '{other}' not found")),
        )),
    }
}

/// Runs the Model Context Protocol stdio server loop.
/// Reads from stdin line-by-line and writes JSON-RPC responses to stdout.
pub async fn run() -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();

    while let Ok(Some(line)) = reader.next_line().await {
        if let Some(resp) = handle_message(&line).await {
            let serialized = serde_json::to_string(&resp)?;
            stdout.write_all(serialized.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}
