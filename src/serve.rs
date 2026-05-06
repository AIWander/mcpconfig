use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tracing::info;

use crate::config::{DriverConfig, Task};
use crate::events::{ChannelSink, EventSink};
use crate::run;

/// Shared state across all requests.
pub struct AppState {
    pub config: DriverConfig,
}

/// POST /run request body.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub task: Task,
    pub model: String,
}

/// Build the axum Router.
pub fn build_router(config: DriverConfig) -> Router {
    let state = Arc::new(AppState { config });

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            "https://aiwanderai-amd-gradio-workshop-demo.hf.space"
                .parse()
                .unwrap(),
            "https://aiwanderai-amd-gradio-workshop-demo-judge-ui.hf.space"
                .parse()
                .unwrap(),
        ]))
        .allow_methods(AllowMethods::list([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ]))
        .allow_headers(AllowHeaders::list([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ]));

    Router::new()
        .route("/health", get(health))
        .route("/models", get(models))
        .route("/run", post(run_sse))
        .layer(cors)
        .with_state(state)
}

// ── GET /health ──

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let servers: Vec<serde_json::Value> = state
        .config
        .mcp_servers
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "command": s.command,
                "alive_check": "not_implemented_yet"
            })
        })
        .collect();

    Json(json!({
        "status": "ok",
        "models_configured": state.config.models.len(),
        "mcp_servers": servers
    }))
}

// ── GET /models ──

async fn models(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let list: Vec<serde_json::Value> = state
        .config
        .models
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "model_id": m.model_id,
                "base_url": m.base_url,
                "mcp_servers": m.mcp_servers,
                "tool_call_parser": m.tool_call_parser,
                "reasoning_parser": m.reasoning_parser,
            })
        })
        .collect();

    Json(json!(list))
}

// ── POST /run → SSE stream ──

async fn run_sse(
    State(state): State<Arc<AppState>>,
    Json(mut req): Json<RunRequest>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, (StatusCode, Json<serde_json::Value>)>
{
    // Override the task's model field with the top-level model from the request
    req.task.model = req.model.clone();

    // Validate model exists
    if state.config.find_model(&req.task.model).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("model '{}' not found in config", req.task.model)})),
        ));
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    info!("POST /run: run_id={} model={}", run_id, req.task.model);

    let (tx, rx) = mpsc::channel(256);
    let config = state.config.clone();
    let task = req.task;

    // Spawn the agent loop in a background task
    tokio::spawn(async move {
        let mut sink = ChannelSink::new(tx);
        let result = run::run_task(&config, &task, &mut sink).await;
        if let Err(e) = result {
            // Send error as final event (best-effort)
            let _ = sink.log("error", json!({"error": e.to_string()}));
        }
        // tx is dropped here, closing the channel → stream ends
    });

    // Convert receiver into an SSE stream
    let stream = ReceiverStream::new(rx);
    let sse_stream = futures_util::stream::StreamExt::map(stream, |event| {
        let data = serde_json::to_string(&event).unwrap_or_default();
        Ok(SseEvent::default().event(event.kind).data(data))
    });

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}

/// Start the HTTP server.
pub async fn start(config: DriverConfig, bind: &str, port: u16) -> anyhow::Result<()> {
    let router = build_router(config);
    let addr = format!("{}:{}", bind, port);
    info!("serve mode listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}
