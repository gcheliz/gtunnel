use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// Newest first, so `[0]` is what an unknown or missing request falls back to.
/// A client newer than this list gets the latest we speak rather than an
/// error: a client that also knows the stateless era then stays on the
/// handshake path instead of guessing at one we do not serve (#149).
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2024-11-05"];

/// Echo the requested revision when we speak it, otherwise offer our latest.
/// The spec lets the server counter-offer, and the client decides whether to
/// continue, so this never fails on a missing or odd `protocolVersion`.
pub fn negotiate_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .copied()
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0])
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

fn default_jsonrpc() -> String {
    "2.0".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(code: i32, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResourcesCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    /// What a client's approval prompt shows instead of the `gtunnel_` id.
    pub title: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    pub annotations: ToolAnnotations,
    /// Only the tools that answer in JSON declare one; a text answer has no
    /// shape to promise. It always admits the error envelope as well, because
    /// the spec holds every structured result to the schema, a failure included.
    #[serde(
        rename = "outputSchema",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_schema: Option<Value>,
}

/// All four hints are always emitted: the spec defaults `destructiveHint` and
/// `openWorldHint` to true, so leaving one out would mislabel a read-only or
/// local-only tool as destructive and open-world to a client that trusts them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolAnnotations {
    #[serde(rename = "readOnlyHint")]
    pub read_only_hint: bool,
    #[serde(rename = "destructiveHint")]
    pub destructive_hint: bool,
    #[serde(rename = "idempotentHint")]
    pub idempotent_hint: bool,
    #[serde(rename = "openWorldHint")]
    pub open_world_hint: bool,
}

impl ToolAnnotations {
    pub const fn new(
        read_only: bool,
        destructive: bool,
        idempotent: bool,
        open_world: bool,
    ) -> Self {
        Self {
            read_only_hint: read_only,
            destructive_hint: destructive,
            idempotent_hint: idempotent,
            open_world_hint: open_world,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContentItem {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallResult {
    pub content: Vec<ContentItem>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// The same `{"error":{"kind","message"}}` object `--json` prints, so an
    /// agent branches on one set of kind strings whichever door it used.
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

impl ToolCallResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: text.into(),
            }],
            is_error: None,
            structured_content: None,
        }
    }

    /// A JSON answer. `text` is what agents already parse out of `content[0]`
    /// and stays byte for byte as it was; `structured` is the same data as the
    /// object the spec requires, which differs only where the text is an array.
    pub fn json(text: &impl Serialize, structured: Value) -> Self {
        Self {
            structured_content: Some(structured),
            ..Self::text(serde_json::to_string_pretty(text).unwrap_or_default())
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ContentItem {
                content_type: "text".to_string(),
                text: message.into(),
            }],
            is_error: Some(true),
            structured_content: None,
        }
    }

    /// A tool error that keeps its kind. The text stays the full chain.
    pub fn failure(err: &anyhow::Error) -> Self {
        Self {
            structured_content: Some(crate::error::to_json(err)),
            ..Self::error(crate::error::message_of(err))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceDefinition {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceReadResult {
    pub contents: Vec<ResourceContent>,
}

#[cfg(test)]
mod negotiation_tests {
    use super::*;

    #[test]
    fn a_supported_version_is_echoed() {
        for v in ["2025-11-25", "2025-06-18", "2024-11-05"] {
            assert_eq!(negotiate_version(Some(v)), v);
        }
    }

    #[test]
    fn anything_else_gets_the_latest() {
        for v in [None, Some(""), Some("1900-01-01"), Some("2026-07-28")] {
            assert_eq!(negotiate_version(v), "2025-11-25");
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;

    #[test]
    fn a_failed_tool_carries_the_kind_beside_the_text() {
        let err: anyhow::Error =
            crate::error::GtunnelError::not_found("Tunnel 'x' not found").into();
        let result = ToolCallResult::failure(&err.context("stop"));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content[0].text, "stop: Tunnel 'x' not found");
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["structuredContent"]["error"]["kind"], "not_found");
        assert_eq!(
            json["structuredContent"]["error"]["message"],
            "stop: Tunnel 'x' not found"
        );
    }
}
