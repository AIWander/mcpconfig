use anyhow::{Context, Result};
use serde_json::json;
use std::path::PathBuf;
use tracing::info;

use crate::agent::{self, AgentResult};
use crate::config::{DriverConfig, Task};
use crate::events::EventSink;
use crate::mcp;
use crate::openai;
use crate::registry::ToolRegistry;

/// Run a task against a model+MCP configuration, emitting events to the provided sink.
/// This is the shared orchestration used by both CLI `run` and HTTP `serve`.
pub async fn run_task(
    driver_config: &DriverConfig,
    task: &Task,
    sink: &mut dyn EventSink,
) -> Result<AgentResult> {
    let model = driver_config.find_model(&task.model)?;

    info!("task: {} | model: {}", task.name, model.name);

    let server_names = task.mcp_servers.as_ref().unwrap_or(&model.mcp_servers);
    let tool_filter = task.tool_filter.as_ref().unwrap_or(&model.tool_filter);

    let system_prompt = task
        .system_prompt
        .clone()
        .unwrap_or_else(load_default_system_prompt);

    sink.log(
        "run_start",
        json!({
            "task": task.name,
            "model": model.name,
            "base_url": model.base_url,
            "user_prompt": task.user_prompt,
            "mcp_servers": server_names,
            "tool_call_parser": model.tool_call_parser,
            "reasoning_parser": model.reasoning_parser,
            "auto_tool_choice": model.auto_tool_choice,
        }),
    )?;

    // Build tool registry — spawn each MCP server
    let mut reg = ToolRegistry::new();
    for server_name in server_names {
        let server_config = driver_config.find_server(server_name)?;
        info!(
            "spawning MCP server: {} ({})",
            server_name, server_config.command
        );

        let mut client = mcp::McpClient::spawn(
            &server_config.command,
            &server_config.args,
            &server_config.env,
        )?;

        let init_result = client
            .initialize()
            .await
            .with_context(|| format!("MCP initialize handshake with '{}'", server_name))?;
        info!(
            "initialized {}: {:?}",
            server_name,
            init_result.get("serverInfo")
        );

        let all_tools = client
            .list_tools()
            .await
            .with_context(|| format!("tools/list from '{}'", server_name))?;
        info!("{} exposes {} tools", server_name, all_tools.len());

        reg.add_server(server_name.clone(), client, all_tools, tool_filter)
            .await?;
    }

    info!("{} tools registered after filtering", reg.tool_count());

    // Build OpenAI client
    let api_key = model
        .api_key_env
        .as_ref()
        .and_then(|env_name| std::env::var(env_name).ok());
    let openai_client = openai::OpenAIClient::new(&model.base_url, api_key);

    // Run agent loop
    let result =
        agent::run_agent_loop(task, model, &openai_client, &mut reg, sink, &system_prompt).await;

    match &result {
        Ok(r) => {
            sink.log(
                "run_end",
                json!({
                    "ok": true,
                    "duration_ms": r.duration_ms,
                    "total_tokens": r.total_usage.total_tokens,
                    "iterations": r.iterations,
                }),
            )?;
        }
        Err(e) => {
            sink.log("run_end", json!({"ok": false, "error": e.to_string()}))?;
        }
    }

    // Shutdown MCP servers
    reg.shutdown_all().await?;

    result
}

fn load_default_system_prompt() -> String {
    let path = PathBuf::from("config/prompts/system.txt");
    std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "You are an agent with access to browser automation tools. When asked to find information online, use the tools to navigate and extract content. Provide a clear final answer once you have what you need.".to_string()
    })
}
