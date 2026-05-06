use anyhow::{bail, Result};
use globset::GlobBuilder;
use std::collections::HashMap;
use tracing::info;

use crate::mcp::{McpClient, McpTool, ToolCallResult};
use crate::openai::OpenAITool;
use crate::tools::mcp_tool_to_openai;
use serde_json::Value;

/// Multi-server tool registry. Routes tool calls to the correct MCP server.
pub struct ToolRegistry {
    /// server_name -> McpClient
    clients: HashMap<String, McpClient>,
    /// exposed_tool_name -> server_name
    tool_owners: HashMap<String, String>,
    /// exposed_tool_name -> original MCP tool definition
    raw_tools: HashMap<String, McpTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry {
            clients: HashMap::new(),
            tool_owners: HashMap::new(),
            raw_tools: HashMap::new(),
        }
    }

    /// Add an MCP server's tools to the registry, applying glob-pattern filters.
    /// Handles namespace collisions by prefixing with server name.
    pub async fn add_server(
        &mut self,
        name: String,
        client: McpClient,
        all_tools: Vec<McpTool>,
        filter: &[String],
    ) -> Result<()> {
        let tools = if filter.is_empty() {
            all_tools
        } else {
            all_tools
                .into_iter()
                .filter(|t| {
                    filter.iter().any(|pattern| {
                        GlobBuilder::new(pattern)
                            .literal_separator(false)
                            .build()
                            .map(|g| g.compile_matcher().is_match(&t.name))
                            .unwrap_or(false)
                    })
                })
                .collect()
        };

        for tool in tools {
            let bare_name = tool.name.clone();

            if let Some(existing_server) = self.tool_owners.get(&bare_name).cloned() {
                // Collision: prefix both the existing tool and this new one
                let old_prefixed = format!("{}__{}", existing_server, bare_name);
                let new_prefixed = format!("{}__{}", name, bare_name);

                // Move existing tool to prefixed name
                if let Some(old_tool) = self.raw_tools.remove(&bare_name) {
                    self.tool_owners.remove(&bare_name);
                    self.raw_tools.insert(old_prefixed.clone(), old_tool);
                    self.tool_owners
                        .insert(old_prefixed.clone(), existing_server.clone());
                    info!(
                        "namespace collision on '{}': renamed existing to '{}'",
                        bare_name, old_prefixed
                    );
                }

                // Insert new tool with prefix
                self.raw_tools.insert(new_prefixed.clone(), tool);
                self.tool_owners.insert(new_prefixed.clone(), name.clone());
                info!(
                    "namespace collision on '{}': new tool registered as '{}'",
                    bare_name, new_prefixed
                );
            } else {
                self.raw_tools.insert(bare_name.clone(), tool);
                self.tool_owners.insert(bare_name, name.clone());
            }
        }

        self.clients.insert(name, client);
        Ok(())
    }

    /// Convert all registered tools to OpenAI format.
    pub fn to_openai_tools(&self) -> Vec<OpenAITool> {
        self.raw_tools
            .iter()
            .map(|(exposed_name, mcp_tool)| mcp_tool_to_openai(exposed_name, mcp_tool))
            .collect()
    }

    /// Dispatch a tool call to the correct MCP server.
    /// Strips any server__ prefix before calling the server.
    pub async fn dispatch(&mut self, tool_name: &str, arguments: Value) -> Result<ToolCallResult> {
        let server_name = self
            .tool_owners
            .get(tool_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", tool_name))?;

        // Determine the original MCP tool name (strip prefix if present)
        let mcp_name = if tool_name.contains("__") {
            let prefix = format!("{}__{}", server_name, "");
            tool_name.strip_prefix(&prefix).unwrap_or(tool_name)
        } else {
            tool_name
        };

        let client = self
            .clients
            .get_mut(&server_name)
            .ok_or_else(|| anyhow::anyhow!("no client for server: {}", server_name))?;

        client.call_tool(mcp_name, arguments).await
    }

    /// Number of registered tools.
    pub fn tool_count(&self) -> usize {
        self.raw_tools.len()
    }

    /// List all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.raw_tools.keys().cloned().collect();
        names.sort();
        names
    }

    /// Shutdown all MCP server clients.
    pub async fn shutdown_all(self) -> Result<()> {
        for (name, client) in self.clients {
            if let Err(e) = client.shutdown().await {
                tracing::warn!("error shutting down {}: {}", name, e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool(name: &str) -> McpTool {
        McpTool {
            name: name.to_string(),
            description: Some(format!("Tool: {}", name)),
            input_schema: json!({"type": "object"}),
        }
    }

    #[tokio::test]
    async fn test_no_collision() {
        let mut registry = ToolRegistry::new();

        // We can't easily create a real McpClient without a process,
        // but we can test the tool registration logic by inspecting internal state.
        // Using add_server with a dummy — we'll bypass the client for this test.

        let tools_a = vec![make_tool("browser_navigate"), make_tool("browser_click")];
        let tools_b = vec![make_tool("read_file"), make_tool("write_file")];

        // Directly insert tools to test registration logic
        for tool in tools_a {
            let name = tool.name.clone();
            registry.raw_tools.insert(name.clone(), tool);
            registry.tool_owners.insert(name, "hands".to_string());
        }
        for tool in tools_b {
            let name = tool.name.clone();
            registry.raw_tools.insert(name.clone(), tool);
            registry.tool_owners.insert(name, "local".to_string());
        }

        assert_eq!(registry.tool_count(), 4);
        let openai_tools = registry.to_openai_tools();
        assert_eq!(openai_tools.len(), 4);
    }

    #[tokio::test]
    async fn test_namespace_collision() {
        let mut registry = ToolRegistry::new();

        // Simulate adding server "hands" with a read_file tool
        let hands_tools = vec![make_tool("read_file"), make_tool("browser_navigate")];
        for tool in hands_tools {
            let name = tool.name.clone();
            registry.raw_tools.insert(name.clone(), tool);
            registry.tool_owners.insert(name, "hands".to_string());
        }

        // Now simulate adding server "local" with a colliding read_file tool
        let local_tools = vec![make_tool("read_file"), make_tool("run_command")];

        // Process collision for read_file
        for tool in local_tools {
            let bare_name = tool.name.clone();
            if let Some(existing_server) = registry.tool_owners.get(&bare_name).cloned() {
                let old_prefixed = format!("{}__{}", existing_server, bare_name);
                let new_prefixed = format!("local__{}", bare_name);

                if let Some(old_tool) = registry.raw_tools.remove(&bare_name) {
                    registry.tool_owners.remove(&bare_name);
                    registry.raw_tools.insert(old_prefixed.clone(), old_tool);
                    registry
                        .tool_owners
                        .insert(old_prefixed, existing_server);
                }
                registry.raw_tools.insert(new_prefixed.clone(), tool);
                registry
                    .tool_owners
                    .insert(new_prefixed, "local".to_string());
            } else {
                let name = tool.name.clone();
                registry.raw_tools.insert(name.clone(), tool);
                registry.tool_owners.insert(name, "local".to_string());
            }
        }

        // Should have: hands__read_file, local__read_file, browser_navigate, run_command
        assert_eq!(registry.tool_count(), 4);

        let names = registry.tool_names();
        assert!(names.contains(&"hands__read_file".to_string()));
        assert!(names.contains(&"local__read_file".to_string()));
        assert!(names.contains(&"browser_navigate".to_string()));
        assert!(names.contains(&"run_command".to_string()));
        // bare read_file should not exist
        assert!(!names.contains(&"read_file".to_string()));
    }

    #[test]
    fn test_glob_filter() {
        let tools = vec![
            make_tool("browser_navigate"),
            make_tool("browser_click"),
            make_tool("read_file"),
            make_tool("vision_analyze"),
        ];

        let filter = vec!["browser_*".to_string()];
        let filtered: Vec<McpTool> = tools
            .into_iter()
            .filter(|t| {
                filter.iter().any(|pattern| {
                    GlobBuilder::new(pattern)
                        .literal_separator(false)
                        .build()
                        .map(|g| g.compile_matcher().is_match(&t.name))
                        .unwrap_or(false)
                })
            })
            .collect();

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "browser_navigate");
        assert_eq!(filtered[1].name, "browser_click");
    }
}
