# mcpconfig

> Archived May 2026 hackathon artifact. This repository is not maintained and its example
> endpoints and paths are illustrative only.

Universal MCP-aware agent driver. Bridges any OpenAI-compatible LLM endpoint to any MCP server.

## What this does

A single Rust binary that:
1. Spawns one or more MCP servers as subprocesses, speaks MCP over stdio
2. Aggregates their tool definitions into a unified registry
3. Registers that registry with any OpenAI-compatible LLM endpoint (vLLM, llama.cpp, Ollama, etc.)
4. Runs an agent loop: send chat -> receive tool_calls -> dispatch to correct MCP server -> feed results back -> repeat
5. Logs every event to JSONL and produces a human-readable Markdown report

**Model-agnostic** (any OpenAI-compatible endpoint) and **server-agnostic** (any MCP server speaking stdio). Adding a new model or server is a config edit, not a code change.

## Build

```bash
cargo build --release
```

Requires Rust 1.70+. Builds on Windows (primary) and Linux.

## Usage

### Run a task (full agent loop)

```bash
# Requires a running OpenAI-compatible LLM endpoint (e.g. vLLM)
./target/release/mcpconfig run tasks/example_smoke.json
```

Outputs land in `runs/<timestamp>_<task>/`:
- `run.jsonl` -- append-only event stream (machine-readable)
- `shared_state.md` -- composed report (human-readable)

### List tools from an MCP server (integration test)

```bash
./target/release/mcpconfig list-tools hands
```

This spawns the MCP server, completes the initialize handshake, calls `tools/list`, prints the tool count and names, then shuts down. No LLM needed.

### Custom config path

```bash
./target/release/mcpconfig -c path/to/models.toml run tasks/my_task.json
```

## Configuration

### `config/models.toml`

Defines MCP servers and model endpoints:

```toml
[[mcp_servers]]
name = "hands"
command = "C:\\github\\hands\\target\\release\\hands.exe"
# Linux: command = "/path/to/hands"

[[models]]
name = "gpt-oss-20b"
base_url = "http://localhost:8000/v1"
model_id = "openai/gpt-oss-20b"
api_key_env = "VLLM_API_KEY"
mcp_servers = ["hands"]
tool_filter = ["browser_*"]
```

### Task files (`tasks/*.json`)

```json
{
  "name": "example_smoke",
  "model": "gpt-oss-20b",
  "mcp_servers": ["hands"],
  "user_prompt": "Navigate to https://example.com and tell me the exact text of the main heading.",
  "max_iterations": 6,
  "tool_filter": ["browser_navigate", "browser_get_text", "browser_extract_content"]
}
```

## Architecture

```
src/
  main.rs      -- CLI (clap), subcommands: run, list-tools
  config.rs    -- TOML + JSON config loading
  mcp.rs       -- MCP stdio client (JSON-RPC, auto-detect framing)
  openai.rs    -- OpenAI chat completions HTTP client
  tools.rs     -- MCP tool -> OpenAI tool conversion (verbatim schema pass-through)
  registry.rs  -- ToolRegistry: multi-server multiplexer with namespace collision handling
  agent.rs     -- Agent loop: chat -> tool_calls -> dispatch -> repeat
  events.rs    -- JSONL event writer (flush per line)
  report.rs    -- Markdown report composer from JSONL stream
```

Key design decisions:
- **Hand-rolled MCP client** over `rmcp` crate -- the protocol surface needed (initialize, tools/list, tools/call) is small, and hands uses bare JSON-line framing (not LSP Content-Length), so a simple line-based reader with auto-detection was faster to get working
- **clap derive** for CLI
- **anyhow** for error handling throughout (v1 simplicity)
- **ToolRegistry** handles namespace collisions by prefixing with `server__tool` when two servers expose the same tool name

## Tests

```bash
cargo test
```

7 unit tests covering:
- Tool conversion: MCP -> OpenAI schema pass-through (3 tests)
- ToolRegistry: namespace collision handling, no-collision case (2 tests)
- Glob filter matching (1 test)
- JSONL event serialization and readback (1 test)

## Integration test (hands.exe)

With a local hands binary at `<repo>\target\release\hands.exe`:

```bash
./target/release/mcpconfig list-tools hands
# Output: Server info, 118 tools listed
```

## What remains for the droplet

- Update `config/models.toml` to the actual server paths on the target machine.
- Start vLLM with `--enable-auto-tool-choice --tool-call-parser harmony`
- Run: `mcpconfig run tasks/example_smoke.json`
- Full end-to-end test: LLM calls tools, gets results, produces final answer
- Streaming, retries, breadcrumb integration (v2)

## vLLM serving constraints

mcpconfig does **not** manage vLLM lifecycle — you start vLLM yourself with the correct parser flags. The `ModelConfig` fields `tool_call_parser`, `reasoning_parser`, and `auto_tool_choice` are **declarative**: they document which flags vLLM must have been started with for that model entry to work correctly.

### Example vLLM startup (gpt-oss-20b)

```bash
vllm serve openai/gpt-oss-20b \
  --enable-auto-tool-choice \
  --tool-call-parser openai \
  --reasoning-parser openai_gptoss \
  --port 8000
```

### Common parser names by model family

| Model family | `tool_call_parser` | `reasoning_parser` |
|---|---|---|
| gpt-oss (20b, 120b) | `openai` | `openai_gptoss` |
| Qwen3 / Qwen3-Coder | `qwen3_coder` | `qwen3` |
| Ministral | `mistral` | `mistral` |
| Llama 3 | `llama3_json` | *(none)* |
| DeepSeek-R1 / V3 | `deepseek_v3` | `deepseek_r1` |

### Failure mode if mismatched

If vLLM is started with parser X but the model expects parser Y:

- **Wrong `tool_call_parser`**: The model's tool-call tokens are not recognized. You get empty `tool_calls: []` in the response even though the model intended to call a tool. The raw text may contain unparsed JSON tool calls in the `content` field.
- **Wrong `reasoning_parser`**: Reasoning/thinking tokens are not extracted. They end up trapped inside `reasoning_content` in an opaque format (e.g. harmony-encoded), or leak into `content` with raw `<|channel|>` tags.

Always verify parser alignment before running bakeoffs. The `run_start` JSONL event now logs the configured parsers for post-hoc debugging.

## Serve mode (HTTP + SSE)

Start an HTTP server so a remote UI (e.g. Gradio on HuggingFace Spaces) can drive agent runs and stream events in real time:

```bash
./target/release/mcpconfig serve --port 8003 --bind 0.0.0.0
```

### Endpoints

**GET /health**
```bash
curl http://localhost:8003/health
# {"status":"ok","models_configured":3,"mcp_servers":[{"name":"hands","command":"...","alive_check":"not_implemented_yet"}]}
```

**GET /models**
```bash
curl http://localhost:8003/models
# [{"name":"gpt-oss-120b","model_id":"openai/gpt-oss-120b","base_url":"http://...","mcp_servers":["hands"],...}]
```

**POST /run** (SSE stream)
```bash
curl -N -X POST http://localhost:8003/run \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-oss-120b",
    "task": {
      "name": "demo",
      "model": "gpt-oss-120b",
      "user_prompt": "Navigate to https://example.com and tell me the main heading.",
      "max_iterations": 6,
      "mcp_servers": ["hands"],
      "tool_filter": ["browser_navigate", "browser_get_text", "browser_extract_content"]
    }
  }'
```

Response is `text/event-stream` (SSE). Each event:
```
event: run_start
data: {"ts":"...","kind":"run_start","task":"demo","model":"gpt-oss-120b",...}

event: llm_response
data: {"ts":"...","kind":"llm_response","iteration":1,...}

event: tool_call
data: {"ts":"...","kind":"tool_call","name":"browser_navigate",...}

event: final_answer
data: {"ts":"...","kind":"final_answer","content":"The heading is..."}

event: run_end
data: {"ts":"...","kind":"run_end","ok":true,"duration_ms":4200,...}
```

Errors during the run appear as `event: error` on the stream (not HTTP 500s). CORS is configured for the HuggingFace Space origins.

## License

No license grant is made for this archived historical artifact. Its dependencies retain
their own licenses.
