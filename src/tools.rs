use crate::mcp::McpTool;
use crate::openai::{OpenAIFunction, OpenAITool};

/// Convert an MCP tool definition to an OpenAI function tool.
/// The inputSchema is passed through verbatim — it's already JSON Schema.
pub fn mcp_tool_to_openai(name: &str, tool: &McpTool) -> OpenAITool {
    OpenAITool {
        tool_type: "function".to_string(),
        function: OpenAIFunction {
            name: name.to_string(),
            description: tool.description.clone().unwrap_or_default(),
            parameters: tool.input_schema.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_mcp_to_openai_conversion() {
        let mcp_tool = McpTool {
            name: "browser_navigate".to_string(),
            description: Some("Navigate the browser to a URL".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to navigate to" },
                    "wait_until": { "type": "string", "enum": ["load", "domcontentloaded", "networkidle"] }
                },
                "required": ["url"]
            }),
        };

        let openai_tool = mcp_tool_to_openai("browser_navigate", &mcp_tool);

        assert_eq!(openai_tool.tool_type, "function");
        assert_eq!(openai_tool.function.name, "browser_navigate");
        assert_eq!(
            openai_tool.function.description,
            "Navigate the browser to a URL"
        );
        // Parameters should be identical to inputSchema — verbatim pass-through
        assert_eq!(openai_tool.function.parameters, mcp_tool.input_schema);
    }

    #[test]
    fn test_mcp_to_openai_missing_description() {
        let mcp_tool = McpTool {
            name: "some_tool".to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
        };

        let openai_tool = mcp_tool_to_openai("some_tool", &mcp_tool);
        assert_eq!(openai_tool.function.description, "");
    }

    #[test]
    fn test_namespaced_conversion() {
        let mcp_tool = McpTool {
            name: "read_file".to_string(),
            description: Some("Read a file".to_string()),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        };

        // When registry prefixes for collision, the exposed name differs from original
        let openai_tool = mcp_tool_to_openai("hands__read_file", &mcp_tool);
        assert_eq!(openai_tool.function.name, "hands__read_file");
        assert_eq!(openai_tool.function.description, "Read a file");
    }
}
