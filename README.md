# mcpconfig

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
# Linux: command = "/root/hands/target/release/hands"

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

With hands binary at `C:\github\hands\target\release\hands.exe`:

```bash
./target/release/mcpconfig list-tools hands
# Output: Server info, 118 tools listed
```

## What remains for the droplet

- Update `config/models.toml` server paths to Linux (`/root/hands/target/release/hands`)
- Start vLLM with `--enable-auto-tool-choice --tool-call-parser harmony`
- Run: `mcpconfig run tasks/example_smoke.json`
- Full end-to-end test: LLM calls tools, gets results, produces final answer
- Streaming, retries, breadcrumb integration (v2)

## License

MIT or Apache-2.0 (TBD).
