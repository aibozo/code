use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::agent_tool::create_cancel_agent_tool;
use crate::agent_tool::create_check_agent_status_tool;
use crate::agent_tool::create_get_agent_result_tool;
use crate::agent_tool::create_list_agents_tool;
use crate::agent_tool::create_run_agent_tool;
use crate::agent_tool::create_wait_for_agent_tool;
use crate::model_family::ModelFamily;
use crate::plan_tool::PLAN_TOOL;
use crate::protocol::AskForApproval;
use crate::protocol::SandboxPolicy;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResponsesApiTool {
    pub(crate) name: String,
    pub(crate) description: String,
    /// TODO: Validation. When strict is set to true, the JSON schema,
    /// `required` and `additional_properties` must be present. All fields in
    /// `properties` must be present in `required`.
    pub(crate) strict: bool,
    pub(crate) parameters: JsonSchema,
}

/// When serialized as JSON, this produces a valid "Tool" in the OpenAI
/// Responses API.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
pub(crate) enum OpenAiTool {
    #[serde(rename = "function")]
    Function(ResponsesApiTool),
    #[serde(rename = "local_shell")]
    LocalShell {},
}

#[derive(Debug, Clone)]
pub enum ConfigShellToolType {
    DefaultShell,
    ShellWithRequest { sandbox_policy: SandboxPolicy },
    LocalShell,
}

#[derive(Debug, Clone)]
pub struct ToolsConfig {
    pub shell_type: ConfigShellToolType,
    pub plan_tool: bool,
}

impl ToolsConfig {
    pub fn new(
        model_family: &ModelFamily,
        approval_policy: AskForApproval,
        sandbox_policy: SandboxPolicy,
        include_plan_tool: bool,
    ) -> Self {
        let mut shell_type = if model_family.uses_local_shell_tool {
            ConfigShellToolType::LocalShell
        } else {
            ConfigShellToolType::DefaultShell
        };
        if matches!(approval_policy, AskForApproval::OnRequest) {
            shell_type = ConfigShellToolType::ShellWithRequest {
                sandbox_policy: sandbox_policy.clone(),
            }
        }

        Self {
            shell_type,
            plan_tool: include_plan_tool,
        }
    }
}

/// Generic JSON‑Schema subset needed for our tool definitions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum JsonSchema {
    Boolean {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    String {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// MCP schema allows "number" | "integer" for Number
    #[serde(alias = "integer")]
    Number {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Array {
        items: Box<JsonSchema>,

        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Object {
        properties: BTreeMap<String, JsonSchema>,
        #[serde(skip_serializing_if = "Option::is_none")]
        required: Option<Vec<String>>,
        #[serde(
            rename = "additionalProperties",
            skip_serializing_if = "Option::is_none"
        )]
        additional_properties: Option<bool>,
    },
}

fn create_shell_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "command".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: None,
        },
    );
    properties.insert(
        "workdir".to_string(),
        JsonSchema::String { description: None },
    );
    properties.insert(
        "timeout".to_string(),
        JsonSchema::Number {
            description: Some("The timeout for the command in milliseconds (default: 120000 ms = 120s)".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "shell".to_string(),
        description: "Runs a shell command and returns its output. Default timeout: 120000 ms (120s). Override via the `timeout` parameter.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false),
        },
    })
}

fn create_shell_tool_for_sandbox(sandbox_policy: &SandboxPolicy) -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "command".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some("The command to execute".to_string()),
        },
    );
    properties.insert(
        "workdir".to_string(),
        JsonSchema::String {
            description: Some("The working directory to execute the command in".to_string()),
        },
    );
    properties.insert(
        "timeout".to_string(),
        JsonSchema::Number {
            description: Some("The timeout for the command in milliseconds (default: 120000 ms = 120s)".to_string()),
        },
    );

    if matches!(sandbox_policy, SandboxPolicy::WorkspaceWrite { .. }) {
        properties.insert(
        "with_escalated_permissions".to_string(),
        JsonSchema::Boolean {
            description: Some("Whether to request escalated permissions. Set to true if command needs to be run without sandbox restrictions".to_string()),
        },
    );
        properties.insert(
        "justification".to_string(),
        JsonSchema::String {
            description: Some("Only set if ask_for_escalated_permissions is true. 1-sentence explanation of why we want to run this command.".to_string()),
        },
    );
    }

    let description = match sandbox_policy {
        SandboxPolicy::WorkspaceWrite {
            network_access,
            ..
        } => {
            format!(
                r#"
The shell tool is used to execute shell commands.
- When invoking the shell tool, your call will be running in a landlock sandbox, and some shell commands will require escalated privileges:
  - Types of actions that require escalated privileges:
    - Reading files outside the current directory
    - Writing files outside the current directory, and protected folders like .git or .env{}
  - Examples of commands that require escalated privileges:
    - git commit
    - npm install or pnpm install
    - cargo build
    - cargo test
- When invoking a command that will require escalated privileges:
  - Provide the with_escalated_permissions parameter with the boolean value true
  - Include a short, 1 sentence explanation for why we need to run with_escalated_permissions in the justification parameter.

Default timeout: 120000 ms (120s). Override via the `timeout` parameter."#,
                if !network_access {
                    "\n  - Commands that require network access\n"
                } else {
                    ""
                }
            )
        }
        SandboxPolicy::DangerFullAccess => {
            "Runs a shell command and returns its output. Default timeout: 120000 ms (120s). Override via the `timeout` parameter.".to_string()
        }
        SandboxPolicy::ReadOnly => {
            r#"
The shell tool is used to execute shell commands.
- When invoking the shell tool, your call will be running in a landlock sandbox, and some shell commands (including apply_patch) will require escalated permissions:
  - Types of actions that require escalated privileges:
    - Reading files outside the current directory
    - Writing files
    - Applying patches
  - Examples of commands that require escalated privileges:
    - apply_patch
    - git commit
    - npm install or pnpm install
    - cargo build
    - cargo test
- When invoking a command that will require escalated privileges:
  - Provide the with_escalated_permissions parameter with the boolean value true
  - Include a short, 1 sentence explanation for why we need to run with_escalated_permissions in the justification parameter

Default timeout: 120000 ms (120s). Override via the `timeout` parameter."#.to_string()
        }
    };

    OpenAiTool::Function(ResponsesApiTool {
        name: "shell".to_string(),
        description,
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false),
        },
    })
}

/// Returns JSON values that are compatible with Function Calling in the
/// Responses API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=responses
pub(crate) fn create_tools_json_for_responses_api(
    tools: &Vec<OpenAiTool>,
) -> crate::error::Result<Vec<serde_json::Value>> {
    let mut tools_json = Vec::new();

    for tool in tools {
        tools_json.push(serde_json::to_value(tool)?);
    }

    Ok(tools_json)
}

/// Returns JSON values that are compatible with Function Calling in the
/// Chat Completions API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=chat
pub(crate) fn create_tools_json_for_chat_completions_api(
    tools: &Vec<OpenAiTool>,
) -> crate::error::Result<Vec<serde_json::Value>> {
    // We start with the JSON for the Responses API and than rewrite it to match
    // the chat completions tool call format.
    let responses_api_tools_json = create_tools_json_for_responses_api(tools)?;
    let tools_json = responses_api_tools_json
        .into_iter()
        .filter_map(|mut tool| {
            if tool.get("type") != Some(&serde_json::Value::String("function".to_string())) {
                return None;
            }

            if let Some(map) = tool.as_object_mut() {
                // Remove "type" field as it is not needed in chat completions.
                map.remove("type");
                Some(json!({
                    "type": "function",
                    "function": map,
                }))
            } else {
                None
            }
        })
        .collect::<Vec<serde_json::Value>>();
    Ok(tools_json)
}

pub(crate) fn mcp_tool_to_openai_tool(
    fully_qualified_name: String,
    tool: mcp_types::Tool,
) -> Result<ResponsesApiTool, serde_json::Error> {
    let mcp_types::Tool {
        description,
        mut input_schema,
        ..
    } = tool;

    // OpenAI models mandate the "properties" field in the schema. The Agents
    // SDK fixed this by inserting an empty object for "properties" if it is not
    // already present https://github.com/openai/openai-agents-python/issues/449
    // so here we do the same.
    if input_schema.properties.is_none() {
        input_schema.properties = Some(serde_json::Value::Object(serde_json::Map::new()));
    }

    // Serialize to a raw JSON value so we can sanitize schemas coming from MCP
    // servers. Some servers omit the top-level or nested `type` in JSON
    // Schemas (e.g. using enum/anyOf), or use unsupported variants like
    // `integer`. Our internal JsonSchema is a small subset and requires
    // `type`, so we coerce/sanitize here for compatibility.
    let mut serialized_input_schema = serde_json::to_value(input_schema)?;
    sanitize_json_schema(&mut serialized_input_schema);
    let input_schema = serde_json::from_value::<JsonSchema>(serialized_input_schema)?;

    Ok(ResponsesApiTool {
        name: fully_qualified_name,
        description: description.unwrap_or_default(),
        strict: false,
        parameters: input_schema,
    })
}

/// Sanitize a JSON Schema (as serde_json::Value) so it can fit our limited
/// JsonSchema enum. This function:
/// - Ensures every schema object has a "type". If missing, infers it from
///   common keywords (properties => object, items => array, enum/const/format => string)
///   and otherwise defaults to "string".
/// - Fills required child fields (e.g. array items, object properties) with
///   permissive defaults when absent.
fn sanitize_json_schema(value: &mut JsonValue) {
    match value {
        JsonValue::Bool(_) => {
            // JSON Schema boolean form: true/false. Coerce to an accept-all string.
            *value = json!({ "type": "string" });
        }
        JsonValue::Array(arr) => {
            for v in arr.iter_mut() {
                sanitize_json_schema(v);
            }
        }
        JsonValue::Object(map) => {
            // First, recursively sanitize known nested schema holders
            if let Some(props) = map.get_mut("properties") {
                if let Some(props_map) = props.as_object_mut() {
                    for (_k, v) in props_map.iter_mut() {
                        sanitize_json_schema(v);
                    }
                }
            }
            if let Some(items) = map.get_mut("items") {
                sanitize_json_schema(items);
            }
            // Some schemas use oneOf/anyOf/allOf - sanitize their entries
            for combiner in ["oneOf", "anyOf", "allOf", "prefixItems"] {
                if let Some(v) = map.get_mut(combiner) {
                    sanitize_json_schema(v);
                }
            }

            // Normalize/ensure type
            let mut ty = map
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // If type is an array (union), pick first supported; else leave to inference
            if ty.is_none() {
                if let Some(JsonValue::Array(types)) = map.get("type") {
                    for t in types {
                        if let Some(tt) = t.as_str() {
                            if matches!(
                                tt,
                                "object" | "array" | "string" | "number" | "integer" | "boolean"
                            ) {
                                ty = Some(tt.to_string());
                                break;
                            }
                        }
                    }
                }
            }

            // Infer type if still missing
            if ty.is_none() {
                if map.contains_key("properties")
                    || map.contains_key("required")
                    || map.contains_key("additionalProperties")
                {
                    ty = Some("object".to_string());
                } else if map.contains_key("items") || map.contains_key("prefixItems") {
                    ty = Some("array".to_string());
                } else if map.contains_key("enum")
                    || map.contains_key("const")
                    || map.contains_key("format")
                {
                    ty = Some("string".to_string());
                } else if map.contains_key("minimum")
                    || map.contains_key("maximum")
                    || map.contains_key("exclusiveMinimum")
                    || map.contains_key("exclusiveMaximum")
                    || map.contains_key("multipleOf")
                {
                    ty = Some("number".to_string());
                }
            }
            // If we still couldn't infer, default to string
            let ty = ty.unwrap_or_else(|| "string".to_string());
            map.insert("type".to_string(), JsonValue::String(ty.to_string()));

            // Ensure object schemas have properties map
            if ty == "object" {
                if !map.contains_key("properties") {
                    map.insert(
                        "properties".to_string(),
                        JsonValue::Object(serde_json::Map::new()),
                    );
                }
                // If additionalProperties is an object schema, sanitize it too.
                // Leave booleans as-is, since JSON Schema allows boolean here.
                if let Some(ap) = map.get_mut("additionalProperties") {
                    let is_bool = matches!(ap, JsonValue::Bool(_));
                    if !is_bool {
                        sanitize_json_schema(ap);
                    }
                }
            }

            // Ensure array schemas have items
            if ty == "array" && !map.contains_key("items") {
                map.insert("items".to_string(), json!({ "type": "string" }));
            }
        }
        _ => {}
    }
}

/// Returns a list of OpenAiTools based on the provided config and MCP tools.
/// Note that the keys of mcp_tools should be fully qualified names. See
/// [`McpConnectionManager`] for more details.
pub(crate) fn get_openai_tools(
    config: &ToolsConfig,
    mcp_tools: Option<HashMap<String, mcp_types::Tool>>,
    browser_enabled: bool,
) -> Vec<OpenAiTool> {
    let mut tools: Vec<OpenAiTool> = Vec::new();

    match &config.shell_type {
        ConfigShellToolType::DefaultShell => {
            tools.push(create_shell_tool());
        }
        ConfigShellToolType::ShellWithRequest { sandbox_policy } => {
            tools.push(create_shell_tool_for_sandbox(sandbox_policy));
        }
        ConfigShellToolType::LocalShell => {
            tools.push(OpenAiTool::LocalShell {});
        }
    }

    if config.plan_tool {
        tools.push(PLAN_TOOL.clone());
    }

    // Add browser tools only when browser is enabled
    if browser_enabled {
        tools.push(create_browser_open_tool());
        tools.push(create_browser_close_tool());
        tools.push(create_browser_status_tool());
        tools.push(create_browser_click_tool());
        tools.push(create_browser_move_tool());
        tools.push(create_browser_type_tool());
        tools.push(create_browser_key_tool());
        tools.push(create_browser_javascript_tool());
        tools.push(create_browser_scroll_tool());
        tools.push(create_browser_history_tool());
        tools.push(create_browser_inspect_tool());
        tools.push(create_browser_console_tool());
        tools.push(create_browser_cleanup_tool());
        tools.push(create_browser_cdp_tool());
    } else {
        // Only include browser_open and browser_status when browser is disabled
        tools.push(create_browser_open_tool());
        tools.push(create_browser_status_tool());
    }

    // Add agent management tools for calling external LLMs asynchronously
    tools.push(create_run_agent_tool());
    tools.push(create_check_agent_status_tool());
    tools.push(create_get_agent_result_tool());
    tools.push(create_cancel_agent_tool());
    tools.push(create_wait_for_agent_tool());
    tools.push(create_list_agents_tool());

    if let Some(mcp_tools) = mcp_tools {
        for (name, tool) in mcp_tools {
            match mcp_tool_to_openai_tool(name.clone(), tool.clone()) {
                Ok(converted_tool) => tools.push(OpenAiTool::Function(converted_tool)),
                Err(e) => {
                    tracing::error!("Failed to convert {name:?} MCP tool to OpenAI tool: {e:?}");
                }
            }
        }
    }

    tools
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::model_family::find_family_for_model;
    use mcp_types::ToolInputSchema;
    use pretty_assertions::assert_eq;

    use super::*;

    fn assert_eq_tool_names(tools: &[OpenAiTool], expected_names: &[&str]) {
        let tool_names = tools
            .iter()
            .map(|tool| match tool {
                OpenAiTool::Function(ResponsesApiTool { name, .. }) => name,
                OpenAiTool::LocalShell {} => "local_shell",
            })
            .collect::<Vec<_>>();

        assert_eq!(
            tool_names.len(),
            expected_names.len(),
            "tool_name mismatch, {tool_names:?}, {expected_names:?}",
        );
        for (name, expected_name) in tool_names.iter().zip(expected_names.iter()) {
            assert_eq!(
                name, expected_name,
                "tool_name mismatch, {name:?}, {expected_name:?}"
            );
        }
    }

    #[test]
    fn test_get_openai_tools() {
        let model_family = find_family_for_model("codex-mini-latest")
            .expect("codex-mini-latest should be a valid model family");
        let config = ToolsConfig::new(
            &model_family,
            AskForApproval::Never,
            SandboxPolicy::ReadOnly,
            true,
        );
        let tools = get_openai_tools(&config, Some(HashMap::new()), false);

        assert_eq_tool_names(&tools, &["local_shell", "update_plan", "browser_open", "browser_status", "agent_run", "agent_check", "agent_result", "agent_cancel", "agent_wait", "agent_list"]);
    }

    #[test]
    fn test_get_openai_tools_default_shell() {
        let model_family = find_family_for_model("o3").expect("o3 should be a valid model family");
        let config = ToolsConfig::new(
            &model_family,
            AskForApproval::Never,
            SandboxPolicy::ReadOnly,
            true,
        );
        let tools = get_openai_tools(&config, Some(HashMap::new()), false);

        assert_eq_tool_names(&tools, &["shell", "update_plan", "browser_open", "browser_status", "agent_run", "agent_check", "agent_result", "agent_cancel", "agent_wait", "agent_list"]);
    }

    #[test]
    fn test_get_openai_tools_mcp_tools() {
        let model_family = find_family_for_model("o3").expect("o3 should be a valid model family");
        let config = ToolsConfig::new(
            &model_family,
            AskForApproval::Never,
            SandboxPolicy::ReadOnly,
            false,
        );
        let tools = get_openai_tools(
            &config,
            Some(HashMap::from([(
                "test_server/do_something_cool".to_string(),
                mcp_types::Tool {
                    name: "do_something_cool".to_string(),
                    input_schema: ToolInputSchema {
                        properties: Some(serde_json::json!({
                            "string_argument": {
                                "type": "string",
                            },
                            "number_argument": {
                                "type": "number",
                            },
                            "object_argument": {
                                "type": "object",
                                "properties": {
                                    "string_property": { "type": "string" },
                                    "number_property": { "type": "number" },
                                },
                                "required": [
                                    "string_property",
                                    "number_property"
                                ],
                                "additionalProperties": Some(false),
                            },
                        })),
                        required: None,
                        r#type: "object".to_string(),
                    },
                    output_schema: None,
                    title: None,
                    annotations: None,
                    description: Some("Do something cool".to_string()),
                },
            )])),
            false,
        );

        assert_eq_tool_names(&tools, &["shell", "browser_open", "browser_status", "agent_run", "agent_check", "agent_result", "agent_cancel", "agent_wait", "agent_list", "test_server/do_something_cool"]);

        let last = tools.last().expect("should include mcp tool");
        assert_eq!(
            *last,
            OpenAiTool::Function(ResponsesApiTool {
                name: "test_server/do_something_cool".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([
                        (
                            "string_argument".to_string(),
                            JsonSchema::String { description: None }
                        ),
                        (
                            "number_argument".to_string(),
                            JsonSchema::Number { description: None }
                        ),
                        (
                            "object_argument".to_string(),
                            JsonSchema::Object {
                                properties: BTreeMap::from([
                                    (
                                        "string_property".to_string(),
                                        JsonSchema::String { description: None }
                                    ),
                                    (
                                        "number_property".to_string(),
                                        JsonSchema::Number { description: None }
                                    ),
                                ]),
                                required: Some(vec![
                                    "string_property".to_string(),
                                    "number_property".to_string(),
                                ]),
                                additional_properties: Some(false),
                            },
                        ),
                    ]),
                    required: None,
                    additional_properties: None,
                },
                description: "Do something cool".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_mcp_tool_property_missing_type_defaults_to_string() {
        let model_family = find_family_for_model("o3").expect("o3 should be a valid model family");
        let config = ToolsConfig::new(
            &model_family,
            AskForApproval::Never,
            SandboxPolicy::ReadOnly,
            false,
        );

        let tools = get_openai_tools(
            &config,
            Some(HashMap::from([(
                "dash/search".to_string(),
                mcp_types::Tool {
                    name: "search".to_string(),
                    input_schema: ToolInputSchema {
                        properties: Some(serde_json::json!({
                            "query": {
                                "description": "search query"
                            }
                        })),
                        required: None,
                        r#type: "object".to_string(),
                    },
                    output_schema: None,
                    title: None,
                    annotations: None,
                    description: Some("Search docs".to_string()),
                },
            )])),
            false,
        );

        assert_eq_tool_names(&tools, &["shell", "browser_open", "browser_status", "agent_run", "agent_check", "agent_result", "agent_cancel", "agent_wait", "agent_list", "dash/search"]);

        let last = tools.last().expect("should include mcp tool");
        assert_eq!(
            *last,
            OpenAiTool::Function(ResponsesApiTool {
                name: "dash/search".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "query".to_string(),
                        JsonSchema::String {
                            description: Some("search query".to_string())
                        }
                    )]),
                    required: None,
                    additional_properties: None,
                },
                description: "Search docs".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_mcp_tool_integer_normalized_to_number() {
        let model_family = find_family_for_model("o3").expect("o3 should be a valid model family");
        let config = ToolsConfig::new(
            &model_family,
            AskForApproval::Never,
            SandboxPolicy::ReadOnly,
            false,
        );

        let tools = get_openai_tools(
            &config,
            Some(HashMap::from([(
                "dash/paginate".to_string(),
                mcp_types::Tool {
                    name: "paginate".to_string(),
                    input_schema: ToolInputSchema {
                        properties: Some(serde_json::json!({
                            "page": { "type": "integer" }
                        })),
                        required: None,
                        r#type: "object".to_string(),
                    },
                    output_schema: None,
                    title: None,
                    annotations: None,
                    description: Some("Pagination".to_string()),
                },
            )])),
            false,
        );

        assert_eq_tool_names(&tools, &["shell", "browser_open", "browser_status", "agent_run", "agent_check", "agent_result", "agent_cancel", "agent_wait", "agent_list", "dash/paginate"]);
        let last = tools.last().expect("should include mcp tool");
        assert_eq!(
            *last,
            OpenAiTool::Function(ResponsesApiTool {
                name: "dash/paginate".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "page".to_string(),
                        JsonSchema::Number { description: None }
                    )]),
                    required: None,
                    additional_properties: None,
                },
                description: "Pagination".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_mcp_tool_array_without_items_gets_default_string_items() {
        let model_family = find_family_for_model("o3").expect("o3 should be a valid model family");
        let config = ToolsConfig::new(
            &model_family,
            AskForApproval::Never,
            SandboxPolicy::ReadOnly,
            false,
        );

        let tools = get_openai_tools(
            &config,
            Some(HashMap::from([(
                "dash/tags".to_string(),
                mcp_types::Tool {
                    name: "tags".to_string(),
                    input_schema: ToolInputSchema {
                        properties: Some(serde_json::json!({
                            "tags": { "type": "array" }
                        })),
                        required: None,
                        r#type: "object".to_string(),
                    },
                    output_schema: None,
                    title: None,
                    annotations: None,
                    description: Some("Tags".to_string()),
                },
            )])),
            false,
        );

        assert_eq_tool_names(&tools, &["shell", "browser_open", "browser_status", "agent_run", "agent_check", "agent_result", "agent_cancel", "agent_wait", "agent_list", "dash/tags"]);
        let last = tools.last().expect("should include mcp tool");
        assert_eq!(
            *last,
            OpenAiTool::Function(ResponsesApiTool {
                name: "dash/tags".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "tags".to_string(),
                        JsonSchema::Array {
                            items: Box::new(JsonSchema::String { description: None }),
                            description: None
                        }
                    )]),
                    required: None,
                    additional_properties: None,
                },
                description: "Tags".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_mcp_tool_anyof_defaults_to_string() {
        let model_family = find_family_for_model("o3").expect("o3 should be a valid model family");
        let config = ToolsConfig::new(
            &model_family,
            AskForApproval::Never,
            SandboxPolicy::ReadOnly,
            false,
        );

        let tools = get_openai_tools(
            &config,
            Some(HashMap::from([(
                "dash/value".to_string(),
                mcp_types::Tool {
                    name: "value".to_string(),
                    input_schema: ToolInputSchema {
                        properties: Some(serde_json::json!({
                            "value": { "anyOf": [ { "type": "string" }, { "type": "number" } ] }
                        })),
                        required: None,
                        r#type: "object".to_string(),
                    },
                    output_schema: None,
                    title: None,
                    annotations: None,
                    description: Some("AnyOf Value".to_string()),
                },
            )])),
            false,
        );

        assert_eq_tool_names(&tools, &["shell", "browser_open", "browser_status", "agent_run", "agent_check", "agent_result", "agent_cancel", "agent_wait", "agent_list", "dash/value"]);
        let last = tools.last().expect("should include mcp tool");
        assert_eq!(
            *last,
            OpenAiTool::Function(ResponsesApiTool {
                name: "dash/value".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "value".to_string(),
                        JsonSchema::String { description: None }
                    )]),
                    required: None,
                    additional_properties: None,
                },
                description: "AnyOf Value".to_string(),
                strict: false,
            })
        );
    }
}

fn create_browser_open_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "url".to_string(),
        JsonSchema::String {
            description: Some("The URL to navigate to (e.g., https://example.com)".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_open".to_string(),
        description: "Opens a browser window and navigates to the specified URL. Screenshots will be automatically attached to subsequent messages.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["url".to_string()]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_close_tool() -> OpenAiTool {
    let properties = BTreeMap::new();

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_close".to_string(),
        description: "Closes the browser window and disables screenshot capture.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_status_tool() -> OpenAiTool {
    let properties = BTreeMap::new();

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_status".to_string(),
        description: "Gets the current browser status including whether it's enabled, current URL, and viewport settings.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_click_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "type".to_string(),
        JsonSchema::String {
            description: Some("Optional type of mouse event: 'click' (default), 'mousedown', or 'mouseup'. Use mousedown, browser_move, mouseup sequence to drag.".to_string()),
        },
    );
    properties.insert(
        "x".to_string(),
        JsonSchema::Number {
            description: Some("Optional absolute X coordinate to click. If provided (with y), the cursor will first move to (x,y).".to_string()),
        },
    );
    properties.insert(
        "y".to_string(),
        JsonSchema::Number {
            description: Some("Optional absolute Y coordinate to click. If provided (with x), the cursor will first move to (x,y).".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_click".to_string(),
        description: "Performs a mouse action. By default acts at the current cursor; if x,y are provided, moves there (briefly waits for animation) then clicks.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_move_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "x".to_string(),
        JsonSchema::Number {
            description: Some(
                "The absolute X coordinate to move the mouse to (use with y)".to_string(),
            ),
        },
    );
    properties.insert(
        "y".to_string(),
        JsonSchema::Number {
            description: Some(
                "The absolute Y coordinate to move the mouse to (use with x)".to_string(),
            ),
        },
    );
    properties.insert(
        "dx".to_string(),
        JsonSchema::Number {
            description: Some(
                "Relative (+/-) X movement in CSS pixels from current mouse position (use with dy)"
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "dy".to_string(),
        JsonSchema::Number {
            description: Some(
                "Relative (+/-) Y movement in CSS pixels from current mouse position (use with dx)"
                    .to_string(),
            ),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_move".to_string(),
        description: "Move your mouse [as shown as a blue cursor in your screenshot] to new coordinates in the browser window (x,y - top left origin) or by relative offset to your current mouse position (dx,dy). If the mouse is close to where it should be then dx,dy may be easier to judge. Always confirm your mouse is where you expected it to be in the next screenshot after a move, otherwise try again.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_type_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "text".to_string(),
        JsonSchema::String {
            description: Some("The text to type into the currently focused element".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_type".to_string(),
        description: "Types text into the currently focused element in the browser.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["text".to_string()]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_key_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "key".to_string(),
        JsonSchema::String {
            description: Some("The key to press (e.g., 'Enter', 'Tab', 'Escape', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Backspace', 'Delete')".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_key".to_string(),
        description:
            "Presses a keyboard key in the browser (e.g., Enter, Tab, Escape, arrow keys)."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["key".to_string()]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_javascript_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "code".to_string(),
        JsonSchema::String {
            description: Some("The JavaScript code to execute in the browser context".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_javascript".to_string(),
        description: "Executes JavaScript code in the browser and returns the result. The code is wrapped to automatically capture return values and console.log output.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["code".to_string()]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_scroll_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "dx".to_string(),
        JsonSchema::Number {
            description: Some("Horizontal scroll delta in pixels (positive = right)".to_string()),
        },
    );
    properties.insert(
        "dy".to_string(),
        JsonSchema::Number {
            description: Some("Vertical scroll delta in pixels (positive = down)".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_scroll".to_string(),
        description: "Scrolls the page by the specified CSS pixel deltas.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_history_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "direction".to_string(),
        JsonSchema::String {
            description: Some("History direction: 'back' or 'forward'".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_history".to_string(),
        description: "Navigates browser history backward or forward.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["direction".to_string()]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_inspect_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "x".to_string(),
        JsonSchema::Number {
            description: Some("Optional absolute X coordinate to inspect.".to_string()),
        },
    );
    properties.insert(
        "y".to_string(),
        JsonSchema::Number {
            description: Some("Optional absolute Y coordinate to inspect.".to_string()),
        },
    );
    properties.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Optional element id attribute value. If provided, looks up '#id' and inspects that element.".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_inspect".to_string(),
        description: "Inspects a DOM element by coordinates or id, returns attributes, outerHTML, box model, and matched styles.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_console_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "lines".to_string(),
        JsonSchema::Number {
            description: Some("Optional: Number of recent console lines to return (default: all available)".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_console".to_string(),
        description: "Captures and returns the console output from the browser, including logs, warnings, and errors.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_cleanup_tool() -> OpenAiTool {
    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_cleanup".to_string(),
        description: "Cleans up injected artifacts (cursor overlays, highlights) and resets viewport metrics without closing the browser.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties: BTreeMap::new(),
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

fn create_browser_cdp_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "method".to_string(),
        JsonSchema::String {
            description: Some("CDP method name, e.g. 'Page.navigate' or 'Input.dispatchKeyEvent'".to_string()),
        },
    );
    properties.insert(
        "params".to_string(),
        JsonSchema::Object {
            properties: BTreeMap::new(),
            required: None,
            additional_properties: Some(true),
        },
    );
    properties.insert(
        "target".to_string(),
        JsonSchema::String {
            description: Some("Target for the command: 'page' (default) or 'browser'".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "browser_cdp".to_string(),
        description: "Executes an arbitrary Chrome DevTools Protocol command with a JSON payload against the active page session.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["method".to_string()]),
            additional_properties: Some(false),
        },
    })
}
