mod agent;
mod config;
mod events;
mod mcp;
mod openai;
mod registry;
mod report;
mod run;
mod serve;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "mcpconfig", about = "Universal MCP-aware agent driver")]
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
    /// Start HTTP server for live UI (SSE streaming)
    Serve {
        /// Port to listen on
        #[arg(long, default_value_t = 8003)]
        port: u16,
        /// Address to bind to
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
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
        Commands::Serve { port, bind } => serve::start(driver_config, &bind, port).await,
        Commands::ListTools { server } => cmd_list_tools(&driver_config, &server).await,
    }
}

async fn cmd_run(driver_config: &config::DriverConfig, task_path: &PathBuf) -> Result<()> {
    let task = config::load_task(task_path)?;

    // Create file-based event writer (CLI mode writes run.jsonl + report)
    let runs_dir = PathBuf::from("runs");
    let mut events = events::EventWriter::new(&runs_dir, &task.name)?;

    let result = run::run_task(driver_config, &task, &mut events).await;

    match &result {
        Ok(r) => {
            info!(
                "run complete: {} iterations, {}ms, {} tokens",
                r.iterations, r.duration_ms, r.total_usage.total_tokens
            );
            println!("\n=== Final Answer ===\n{}\n", r.final_answer);
        }
        Err(e) => {
            eprintln!("run failed: {}", e);
        }
    }

    // Compose report
    report::write_report(&events.run_dir)?;
    info!(
        "report written to {}",
        events.run_dir.join("shared_state.md").display()
    );

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
