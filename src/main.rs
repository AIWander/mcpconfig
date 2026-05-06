mod agent;
mod config;
mod events;
mod mcp;
mod openai;
mod registry;
mod report;
mod tools;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "bakeoff-driver", about = "Universal MCP-aware agent driver")]
struct Cli {
    /// Path to models.toml config
    #[arg(short, long, default_value = "config/models.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a task file against a model+MCP configuration
    Run {
        /// Path to task JSON file
        task: PathBuf,
    },
    /// Spawn an MCP server, list its tools, and exit (integration test)
    ListTools {
        /// MCP server name from config
        server: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let driver_config = config::load_config(&cli.config)?;

    match cli.command {
        Commands::Run { task: task_path } => cmd_run(&driver_config, &task_path).await,
        Commands::ListTools { server } => cmd_list_tools(&driver_config, &server).await,
    }
}

async fn cmd_run(driver_config: &config::DriverConfig, task_path: &PathBuf) -> Result<()> {
    let task = config::load_task(task_path)?;
    let model = driver_config.find_model(&task.model)?;

    info!("task: {} | model: {}", task.name, model.name);

    // Determine which MCP servers to spawn
    let server_names = task.mcp_servers.as_ref().unwrap_or(&model.mcp_servers);

    // Determine tool filter
    let tool_filter = task.tool_filter.as_ref().unwrap_or(&model.tool_filter);

    // Load system prompt
    let system_prompt = task
        .system_prompt
        .clone()
        .unwrap_or_else(load_default_system_prompt);

    // Create event writer
    let runs_dir = PathBuf::from("runs");
    let mut events = events::EventWriter::new(&runs_dir, &task.name)?;

    events.log(
        "run_start",
        json!({
            "task": task.name,
            "model": model.name,
            "base_url": model.base_url,
            "user_prompt": task.user_prompt,
            "mcp_servers": server_names,
        }),
    )?;

    // Build tool registry — spawn each MCP server
    let mut reg = registry::ToolRegistry::new();
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
        agent::run_agent_loop(&task, model, &openai_client, &mut reg, &mut events, &system_prompt)
            .await;

    match &result {
        Ok(r) => {
            events.log(
                "run_end",
                json!({
                    "ok": true,
                    "duration_ms": r.duration_ms,
                    "total_tokens": r.total_usage.total_tokens,
                    "iterations": r.iterations,
                }),
            )?;
            info!(
                "run complete: {} iterations, {}ms, {} tokens",
                r.iterations, r.duration_ms, r.total_usage.total_tokens
            );
            println!("\n=== Final Answer ===\n{}\n", r.final_answer);
        }
        Err(e) => {
            events.log("run_end", json!({"ok": false, "error": e.to_string()}))?;
            eprintln!("run failed: {}", e);
        }
    }

    // Compose report
    report::write_report(&events.run_dir)?;
    info!(
        "report written to {}",
        events.run_dir.join("shared_state.md").display()
    );

    // Shutdown MCP servers
    reg.shutdown_all().await?;

    result.map(|_| ())
}

async fn cmd_list_tools(driver_config: &config::DriverConfig, server_name: &str) -> Result<()> {
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

    let init_result = client.initialize().await?;
    let server_info = init_result.get("serverInfo");
    println!("Server: {:?}", server_info);

    let tools = client.list_tools().await?;
    println!("\n{} tools:", tools.len());
    for tool in &tools {
        let desc = tool.description.as_deref().unwrap_or("(no description)");
        let desc_short = if desc.len() > 80 {
            format!("{}...", &desc[..80])
        } else {
            desc.to_string()
        };
        println!("  {} - {}", tool.name, desc_short);
    }

    client.shutdown().await?;
    Ok(())
}

fn load_default_system_prompt() -> String {
    let path = PathBuf::from("config/prompts/system.txt");
    std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "You are an agent with access to browser automation tools. When asked to find information online, use the tools to navigate and extract content. Provide a clear final answer once you have what you need.".to_string()
    })
}
