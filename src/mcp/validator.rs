use anyhow::{bail, Result};
use serde_json::Value;

use super::protocol::{JsonRpcError, ToolDefinition, INVALID_PARAMS};
use super::tools::{all_resources, all_tools};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationSummary {
    pub tools_count: usize,
    pub resources_count: usize,
}

pub struct ToolValidator;

impl ToolValidator {
    /// Validates that a tool definition structurally complies with MCP and JSON Schema.
    pub fn validate_definition(tool: &ToolDefinition) -> Result<()> {
        if tool.name.is_empty() {
            bail!("Tool name cannot be empty");
        }
        if tool.description.is_empty() {
            bail!("Tool '{}' must have a non-empty description", tool.name);
        }
        let schema = &tool.input_schema;
        if !schema.is_object() {
            bail!("Tool '{}' inputSchema must be a JSON object", tool.name);
        }
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            bail!(
                "Tool '{}' inputSchema must have 'type': 'object'",
                tool.name
            );
        }
        if let Some(props) = schema.get("properties")
            && !props.is_object()
        {
            bail!(
                "Tool '{}' inputSchema.properties must be an object",
                tool.name
            );
        }
        if let Some(req) = schema.get("required") {
            let Some(req_arr) = req.as_array() else {
                bail!("Tool '{}' inputSchema.required must be an array", tool.name);
            };
            for r in req_arr {
                if !r.is_string() {
                    bail!(
                        "Tool '{}' inputSchema.required entries must be strings",
                        tool.name
                    );
                }
            }
        }
        Ok(())
    }

    /// Validates incoming tool call arguments against the tool's inputSchema.
    pub fn validate_call_params(
        tool: &ToolDefinition,
        args: Option<&Value>,
    ) -> Result<(), JsonRpcError> {
        let schema = &tool.input_schema;
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        let properties = schema.get("properties").and_then(Value::as_object);

        match args {
            None | Some(Value::Null) => {
                if let Some(first_req) = required.first() {
                    return Err(JsonRpcError::new(
                        INVALID_PARAMS,
                        format!("Missing required parameter '{first_req}'"),
                    ));
                }
                Ok(())
            }
            Some(Value::Object(map)) => {
                // Check for missing required properties
                for req in &required {
                    match map.get(*req) {
                        None | Some(Value::Null) => {
                            return Err(JsonRpcError::new(
                                INVALID_PARAMS,
                                format!("Missing required parameter '{req}'"),
                            ));
                        }
                        _ => {}
                    }
                }

                // Check types for supplied properties
                if let Some(props) = properties {
                    for (key, val) in map {
                        if val.is_null() {
                            continue;
                        }
                        if let Some(prop_def) = props.get(key)
                            && let Some(expected_type) =
                                prop_def.get("type").and_then(Value::as_str)
                        {
                            let matches = match expected_type {
                                "string" => val.is_string(),
                                "integer" => val.is_i64() || val.is_u64(),
                                "boolean" => val.is_boolean(),
                                "array" => val.is_array(),
                                "object" => val.is_object(),
                                _ => true,
                            };
                            if !matches {
                                return Err(JsonRpcError::new(
                                    INVALID_PARAMS,
                                    format!("Parameter '{key}' must be of type {expected_type}"),
                                ));
                            }
                        }
                    }
                }
                Ok(())
            }
            Some(_) => Err(JsonRpcError::new(
                INVALID_PARAMS,
                "Tool arguments must be a JSON object",
            )),
        }
    }
}

/// Runs self-validation checks across all registered tools and resources.
pub fn validate_registered_tools() -> Result<ValidationSummary, Vec<String>> {
    let tools = all_tools();
    let resources = all_resources();
    let mut errors = Vec::new();

    for tool in &tools {
        if let Err(e) = ToolValidator::validate_definition(tool) {
            errors.push(format!("Tool '{}' validation error: {e}", tool.name));
        }
    }

    for resource in &resources {
        if resource.uri.is_empty() {
            errors.push("Resource URI cannot be empty".to_string());
        }
        if resource.name.is_empty() {
            errors.push(format!(
                "Resource '{}' must have a non-empty name",
                resource.uri
            ));
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(ValidationSummary {
        tools_count: tools.len(),
        resources_count: resources.len(),
    })
}

/// CLI command runner for `gtunnel mcp --validate`.
pub fn run_validate() -> Result<()> {
    match validate_registered_tools() {
        Ok(summary) => {
            println!(
                "Validated {} tools and {} resources successfully.",
                summary.tools_count, summary.resources_count
            );
            Ok(())
        }
        Err(errors) => {
            eprintln!("Tool validation failed with {} error(s):", errors.len());
            for err in errors {
                eprintln!("  - {err}");
            }
            bail!("Validation failed");
        }
    }
}
