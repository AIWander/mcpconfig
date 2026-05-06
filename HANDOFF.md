# HANDOFF — Universal MCP-Aware Agent Driver

**Status:** Spec, ready to delegate
**Target:** Build a Rust binary that bridges any OpenAI-compatible LLM endpoint to any MCP server, with gpt-oss-20B (vLLM) + hands (MCP) as the initial integration target.
**Estimated build time:** 30–45 min in a Claude Code session
**Delegate to:** `manager:session_start` with Claude Code, `effort=high`, `working_dir=<repo path>`

---

## 1. Goal

Build `bakeoff-driver` — a single Rust binary that:

1. Spawns **one or more** MCP servers as subprocesses, speaks MCP protocol over stdio to each
2. Discovers each server's tools via `tools/list` and aggregates them into a single tool registry
3. **Registers the combined tool list with any OpenAI-compatible LLM endpoint**
4. Runs an agent loop: send chat → receive `tool_calls` → route each call to the correct MCP server → feed results back → repeat
5. Logs every event to JSONL and produces a human-readable Markdown report

The binary must be **model-agnostic AND server-agnostic** — adding a new model is a config edit, adding a new MCP server is a config edit. Neither requires code changes.

Initial test target: **one MCP server (hands)** + **one LLM (gpt-oss-20B via vLLM)**. The same code must work unchanged against:
- Other models: gpt-oss-120B, Ministral 14B, Qwen3.6-35B-A3B, anything vLLM serves with auto-tool-choice
- Other MCP servers: workflow, autonomous, ops/local, any future MCP server speaking standard stdio MCP
- Combinations: e.g. hands + workflow tools both available to one model in one run

## 2. Success criteria for v1

- [ ] Binary spawns hands MCP server, completes initialization handshake
- [ ] Tool list from hands is correctly converted to OpenAI tool schema and registered with vLLM endpoint
- [ ] Test prompt ("navigate to example.com and tell me the heading text") produces a multi-turn loop where the LLM calls `browser_navigate` and `browser_get_text`/`browser_extract_content`, and returns a final answer containing "Example Domain"
- [ ] Run produces both `runs/<timestamp>/run.jsonl` (structured) and `runs/<timestamp>/shared_state.md` (human-readable)
- [ ] Same binary run against a second model endpoint (config-only change) produces a comparable run

**Out of scope for v1:** streaming token output, retry/recovery on tool errors, breadcrumb integration, multi-model parallel runs. v2 concerns.

---

## 3. Architecture

Three layers, kept independent.

### 3.1 Model registry and MCP server registry (config, not code)

`config/models.toml` — driver reads at startup. Schema:

```toml
# MCP servers the driver can spawn. Reference these by name in models or tasks.
[[mcp_servers]]
name = "hands"
command = "/home/joe/hands/target/release/hands"
args = []
env = { "RUST_LOG" = "info" }

[[mcp_servers]]
name = "workflow"
command = "/home/joe/workflow/target/release/workflow"
args = []

# Models the driver can route to. Each lists which MCP servers to spawn for runs against it.
[[models]]
name = "gpt-oss-20b"
base_url = "http://localhost:8000/v1"
model_id = "openai/gpt-oss-20b"        # what to put in the chat request's `model` field
api_key_env = "VLLM_API_KEY"           # env var name; empty/missing = no auth
strip_thinking_tags = true             # filter <|channel|>analysis blocks from content
system_prompt_strategy = "system_role" # or "first_user_turn"
max_tokens = 4096
temperature = 0.7
mcp_servers = ["hands"]                # default servers; tasks can override
tool_filter = ["browser_*"]            # glob patterns; only matching tools registered

[[models]]
name = "gpt-oss-120b"
base_url = "http://localhost:8000/v1"
model_id = "openai/gpt-oss-120b"
api_key_env = "VLLM_API_KEY"
strip_thinking_tags = true
mcp_servers = ["hands"]
# ... per-model overrides
```

Driver MUST NOT special-case any model name or server name in code. Behavior changes happen via config flags only.

### 3.2 OpenAI-compatible HTTP client

One module, ~80 lines. Uses `reqwest` (async, with `json` feature) + `serde` for types.

Public surface:
```rust
pub struct OpenAIClient { /* base_url, api_key, http client */ }

pub async fn chat_completion(
    &self,
    request: ChatCompletionRequest,
) -> Result<ChatCompletionResponse>;
```

Request type carries: `model`, `messages`, `tools` (Option), `tool_choice`, `max_tokens`, `temperature`, `stream: false` (for v1).

Response type has: `id`, `choices[0].message.content`, `choices[0].message.tool_calls`, `choices[0].message.reasoning_content` (Option), `usage` (token counts).

Knows nothing about MCP. Knows nothing about specific models. Just speaks OpenAI Chat Completions over HTTP.

### 3.3 MCP stdio client

Spawns child process. Reads/writes JSON-RPC 2.0 messages framed by Content-Length headers (LSP-style framing — this is the MCP wire format).

Public surface:
```rust
pub struct McpClient { /* child process, stdin, stdout reader, request_id counter */ }

pub async fn initialize(&mut self) -> Result<InitializeResult>;
pub async fn list_tools(&mut self) -> Result<Vec<Tool>>;
pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<ToolCallResult>;
pub async fn shutdown(&mut self) -> Result<()>;
```

Use the `rmcp` crate if it works cleanly; otherwise hand-roll it (the protocol is small — initialize handshake, tools/list, tools/call, shutdown).

### 3.4 Tool registry (the multi-server abstraction)

`ToolRegistry` is the layer that holds N MCP clients and routes between them. This is the abstraction that keeps the driver server-agnostic.

Public surface:
```rust
pub struct ToolRegistry {
    clients: HashMap<String, McpClient>,           // server_name → client
    tool_owners: HashMap<String, String>,          // exposed_tool_name → server_name
    raw_tools: HashMap<String, McpTool>,           // exposed_tool_name → original MCP tool def
}

impl ToolRegistry {
    pub async fn add_server(
        &mut self,
        name: String,
        client: McpClient,
        filter: &[String],  // glob patterns
    ) -> Result<()>;

    pub fn to_openai_tools(&self) -> Vec<OpenAITool>;

    pub async fn dispatch(
        &mut self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolResult>;

    pub async fn shutdown_all(&mut self) -> Result<()>;
}
```

**Namespace collision rule:** when `add_server` registers a tool, check if the bare name already exists in `tool_owners`. If yes, prefix BOTH (the existing one and the new one) with their server names: `hands__read_file` and `local__read_file`. The OpenAI spec allows function names matching `^[a-zA-Z0-9_-]{1,64}$`, so `server__tool` is valid. Document this in the run log so the LLM's tool calls trace back cleanly.

If no collision, register the tool under its bare name. Most cases hit no collision.

**Dispatch routing:** when the LLM emits a tool call for `browser_navigate`, look up `tool_owners["browser_navigate"]` to get the server name (`"hands"`), get that server's `McpClient` from `clients`, call `tools/call` on it with the original (un-prefixed) tool name. The prefix is purely an LLM-facing label.

For v1 with one server, the registry holds one client and the prefix logic is dead code. For v1.5 with N servers, the same registry handles N. **No agent-loop changes between v1 and v1.5.**

### 3.5 Agent loop

Generic over the model AND over the set of MCP servers. ~60 lines.

```
load model config + task
build ToolRegistry:
    for each server name in task.mcp_servers (or model.mcp_servers default):
        spawn server, complete initialize
        list_tools, apply tool_filter
        registry.add_server(name, client, filter)
build initial messages: [system, user]
tools_for_llm = registry.to_openai_tools()
loop:
    response = openai_client.chat(messages, tools_for_llm)
    log_event(jsonl, "llm_response", response)
    if response.tool_calls is empty:
        log_event(jsonl, "final_answer", response.content)
        break
    messages.push(assistant_message(response))
    for call in response.tool_calls:
        result = registry.dispatch(call.function.name, parsed_args).await
        log_event(jsonl, "tool_result", call.id, result)
        messages.push(tool_message(call.id, result))
    if iteration_count > MAX_ITERATIONS:
        return error("max iterations exceeded")
end
registry.shutdown_all()
write shared_state.md from event stream
```

That's the whole driver. Note the loop is identical for one MCP server or N — the registry hides the multiplexing, so the code path that proves out v1 is the exact same code path that handles v1.5.

---

## 4. Tool registration mechanism (THE CORE)

This is the crux. Tools are defined by the MCP server (hands). The LLM (gpt-oss-20B via vLLM) needs to know about them in OpenAI format. Conversion happens at driver startup, once per run.

### 4.1 What MCP gives us

After `tools/list`, hands returns an array of tool definitions:

```json
{
  "name": "browser_navigate",
  "description": "Navigate the browser to a URL...",
  "inputSchema": {
    "type": "object",
    "properties": {
      "url": { "type": "string", "description": "URL to navigate to" },
      "wait_until": { "type": "string", "enum": ["load", "domcontentloaded", "networkidle"] }
    },
    "required": ["url"]
  }
}
```

`inputSchema` is JSON Schema. This matters — vLLM's tool-call parser uses this schema to validate and structure the LLM's output.

### 4.2 What OpenAI Chat Completions wants

The `tools` parameter on the request:

```json
{
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "browser_navigate",
        "description": "Navigate the browser to a URL...",
        "parameters": {
          "type": "object",
          "properties": {
            "url": { "type": "string", "description": "URL to navigate to" },
            "wait_until": { "type": "string", "enum": ["load", "domcontentloaded", "networkidle"] }
          },
          "required": ["url"]
        }
      }
    }
  ],
  "tool_choice": "auto"
}
```

### 4.3 The conversion (verbatim)

```rust
fn mcp_tool_to_openai(t: &McpTool) -> OpenAITool {
    OpenAITool {
        r#type: "function".to_string(),
        function: OpenAIFunction {
            name: t.name.clone(),
            description: t.description.clone().unwrap_or_default(),
            parameters: t.input_schema.clone(),  // JSON Schema, passed through verbatim
        },
    }
}
```

That's it. MCP's `inputSchema` IS valid JSON Schema, and OpenAI's `parameters` IS JSON Schema. No translation needed beyond renaming the field. **Do not modify the schema content under any circumstances** — vLLM's parser depends on exact schema match for argument validation.

### 4.4 Tool dispatch (the response side)

When the LLM responds with `tool_calls`:

```json
{
  "tool_calls": [
    {
      "id": "call_abc123",
      "type": "function",
      "function": {
        "name": "browser_navigate",
        "arguments": "{\"url\":\"https://example.com\"}"
      }
    }
  ]
}
```

`arguments` is a **JSON-encoded string**, not an object. Driver must parse it, then dispatch via the `ToolRegistry` (which handles namespace-prefix routing to the correct server):

```rust
let args: Value = serde_json::from_str(&call.function.arguments)?;
let result = registry.dispatch(&call.function.name, args).await?;
```

The registry strips any `<server>__` namespace prefix internally and routes to the right `McpClient`. Driver code does NOT call `mcp_client.call_tool` directly outside the registry — that's the abstraction that keeps this server-agnostic.

Then format the result as a tool message:

```json
{
  "role": "tool",
  "tool_call_id": "call_abc123",
  "content": "<stringified MCP result>"
}
```

MCP `tools/call` returns content as an array of typed parts (text, image, embedded resource). For v1, concatenate all `text`-type parts into a single string. v2 can handle images for vision-capable models.

### 4.5 Tool filtering (optional, recommended)

hands exposes 80+ tools. Most LLM context budgets shouldn't carry all of them. Add to `models.toml`:

```toml
[[models]]
name = "gpt-oss-20b"
# ...
tool_filter = ["browser_*"]   # glob patterns; matches register only these
```

For initial smoke test, register `browser_navigate`, `browser_get_text`, `browser_extract_content`, `browser_click`, `browser_type` — five tools, enough for the example.com test. Driver applies the filter after `list_tools` and before conversion.

---

## 5. gpt-oss-20B specifics

### 5.1 vLLM serve command

```
vllm serve openai/gpt-oss-20b \
  --port 8000 \
  --tensor-parallel-size 1 \
  --enable-auto-tool-choice \
  --tool-call-parser harmony
```

The `--tool-call-parser harmony` flag is critical. Without it, gpt-oss tool calls won't be parsed into the OpenAI `tool_calls` shape. The driver doesn't care which parser — it just consumes the normalized output. But the serve command must specify it.

### 5.2 Reasoning content

gpt-oss outputs reasoning into a separate channel. vLLM exposes this as `reasoning_content` on the response message (when running with appropriate flags). The driver's job:

- **Don't echo reasoning back to the LLM in the next turn.** Do not include `reasoning_content` in the assistant message you append to the history. Only include `content` and `tool_calls`.
- **Do log it to JSONL** under a `reasoning` field, for the writeup ("here's how the model thought about each step").
- If `reasoning_content` is missing and `strip_thinking_tags = true` in config, regex-strip `<|channel|>analysis...<|end|>` blocks from `content` before logging or displaying.

### 5.3 System prompt for v1

Keep minimal:

```
You are an agent with access to browser automation tools. When asked to find information online, use the tools to navigate and extract content. Provide a clear final answer once you have what you need.
```

Sophisticated prompting is v2. For v1 we want to verify the loop, not optimize prompt engineering.

### 5.4 Test prompt

```
Navigate to https://example.com and tell me the exact text of the main heading.
```

Expected loop:
1. LLM calls `browser_navigate(url="https://example.com")`
2. Tool returns success
3. LLM calls `browser_get_text(selector="h1")` or `browser_extract_content()`
4. Tool returns "Example Domain"
5. LLM responds: "The main heading is 'Example Domain'."

If this works end-to-end, v1 is done.

---

## 6. File layout

```
bakeoff-driver/
├── Cargo.toml
├── config/
│   ├── models.toml              # model registry
│   └── prompts/
│       └── system.txt           # default system prompt
├── runs/                        # gitignored, output dir
│   └── <timestamp>_<task>/
│       ├── run.jsonl            # event stream (machine-readable)
│       └── shared_state.md      # composed report (human-readable)
├── tasks/
│   └── example_smoke.json       # test task definition
├── src/
│   ├── main.rs                  # CLI entry, arg parsing
│   ├── config.rs                # TOML loading, ModelConfig + McpServerConfig + Task types
│   ├── openai.rs                # OpenAI HTTP client (§3.2)
│   ├── mcp.rs                   # MCP stdio client (§3.3)
│   ├── registry.rs              # ToolRegistry — multi-server multiplexer (§3.4)
│   ├── tools.rs                 # MCP↔OpenAI conversion (§4)
│   ├── agent.rs                 # the loop (§3.5)
│   ├── events.rs                # JSONL event types and writer
│   └── report.rs                # markdown composer
└── README.md
```

## 7. File formats

### 7.1 Task file (`tasks/*.json`)

```json
{
  "name": "example_smoke",
  "description": "Smoke test against example.com",
  "model": "gpt-oss-20b",
  "mcp_servers": ["hands"],
  "system_prompt": null,
  "user_prompt": "Navigate to https://example.com and tell me the exact text of the main heading.",
  "max_iterations": 6,
  "tool_filter": ["browser_navigate", "browser_get_text", "browser_extract_content"]
}
```

`model` is a name from `models.toml`. `mcp_servers` is an optional array of server names from `[[mcp_servers]]` in `models.toml`; if omitted, defaults to the model's configured `mcp_servers`. `tool_filter` overrides the model's default if set, and applies across all selected servers.

### 7.2 Event log (`run.jsonl`)

One JSON object per line. Schema:

```json
{"ts": "2026-05-06T19:32:11.041Z", "kind": "run_start", "task": "example_smoke", "model": "gpt-oss-20b"}
{"ts": "...", "kind": "tools_registered", "count": 3, "names": ["browser_navigate", "browser_get_text", "browser_extract_content"]}
{"ts": "...", "kind": "llm_request", "iteration": 1, "messages_tail": [...], "model": "openai/gpt-oss-20b"}
{"ts": "...", "kind": "llm_response", "iteration": 1, "content": null, "tool_calls": [...], "reasoning": "...", "usage": {...}}
{"ts": "...", "kind": "tool_call", "iteration": 1, "id": "call_abc", "name": "browser_navigate", "arguments": {...}}
{"ts": "...", "kind": "tool_result", "iteration": 1, "id": "call_abc", "ok": true, "content": "..."}
{"ts": "...", "kind": "final_answer", "iteration": 3, "content": "The main heading is 'Example Domain'."}
{"ts": "...", "kind": "run_end", "ok": true, "duration_ms": 4821, "total_tokens": 1247}
```

Append-only. Driver flushes after every line so a crashed run still has partial logs.

### 7.3 Shared state (`shared_state.md`)

Composed from the JSONL stream after the run. Template:

```markdown
# Bakeoff: <task name> — <timestamp>

**Task:** <user_prompt>

---

## Model: <model.name>

**Endpoint:** `<base_url>` · **Tools:** <count> registered

### Final answer

<content>

### Reasoning trace (<N> iterations, <duration> ms, <tokens> tokens)

#### Iteration 1
*Reasoning:* <truncated reasoning_content>
*Tool calls:*
- `browser_navigate({"url": "https://example.com"})` → `Navigated to https://example.com (200 OK)`

#### Iteration 2
*Reasoning:* ...
*Tool calls:* ...

#### Iteration 3
*Final answer.*

---
```

For multi-model bakeoffs (v2), each model gets its own `## Model:` section. The driver appends; never rewrites prior sections.

---

## 8. Implementation order

1. **Cargo project scaffold** — `cargo new --bin bakeoff-driver`, add deps: `tokio` (full), `reqwest` (json, rustls-tls), `serde` (derive), `serde_json`, `toml`, `clap` (derive), `tracing`, `tracing-subscriber`, `anyhow`, `chrono` (serde), `globset` (for tool_filter glob matching)
2. **Config loader** — `models.toml` parser including `[[mcp_servers]]` section, `Task` deserializer, validation
3. **MCP stdio client** — process spawn, framing, initialize, list_tools, call_tool. Test against hands binary directly with a unit test.
4. **OpenAI HTTP client** — chat_completion only. Test with a curl-equivalent request.
5. **Tool conversion module** — pure function, MCP tool list → OpenAI tools array. Unit-tested with a hands tool list fixture.
6. **ToolRegistry** — multi-server holder with namespace collision handling. Unit-test with two fake clients defining a colliding tool.
7. **Event types and JSONL writer** — append-only, flush per line.
8. **Agent loop** — wire registry + openai client. Hard-code a test prompt for first run.
9. **Markdown composer** — read JSONL, emit shared_state.md.
10. **CLI wrapper** — `bakeoff-driver run <task.json>` runs a task end to end.

## 9. Smoke test procedure

After build:
1. Start hands MCP server is implicit — driver spawns it as subprocess. Verify the path to hands binary in config.
2. Verify vLLM is serving gpt-oss-20B: `curl http://localhost:8000/v1/models`
3. Run: `bakeoff-driver run tasks/example_smoke.json`
4. Check `runs/<latest>/run.jsonl` exists and has at least one `tool_call` and one `final_answer` line
5. Check `runs/<latest>/shared_state.md` is readable and contains "Example Domain"

If the loop completes and the answer is correct, v1 is done.

## 10. Future expansion (post-v1)

- **Add more models:** edit `[[models]]` in `models.toml`. Driver code unchanged. Test each new model with the same `example_smoke.json` task.
- **Add more MCP servers:** edit `[[mcp_servers]]` in `models.toml`, reference by name in a model or task. Driver code unchanged — the `ToolRegistry` already handles multiplexing. Example: add `workflow` and run a task with `mcp_servers = ["hands", "workflow"]` to give the LLM browser tools AND credential-vault tools in one session.
- **Bakeoff mode:** `bakeoff-driver bakeoff <task.json> --models gpt-oss-20b,gpt-oss-120b,ministral-14b,qwen3.6-35b` — runs the same task across all models, appends each to one `shared_state.md`.
- **Breadcrumb integration:** add the autonomous MCP server to the registry alongside hands. The LLM gets breadcrumb tools natively and logs its own progress. OR: wrap each iteration in driver-level `breadcrumb_step` calls via a separate autonomous MCP client outside the registry. Either pattern works; pick based on whether you want the LLM to be aware of breadcrumbs as tools.
- **Streaming:** swap `stream: false` for `stream: true`, parse SSE deltas, accumulate tool_call chunks across deltas. Stash this for v2 — non-streaming v1 is more reliable to debug.
- **Tool result post-processing:** truncate large DOM dumps, strip ANSI, summarize. Per-tool config block.
- **Vision support:** when MCP tool results include `image` parts, encode and pass through to vision-capable models like Ministral 14B. v2.
- **Retries on tool errors:** if a tool call fails, allow the LLM one retry with the error message in context. v2.

---

## 11. Decisions left for the builder

These are explicitly NOT specified — pick what works:

- Use `rmcp` crate vs hand-roll MCP — whichever lands faster after a 5-min eval
- `clap` derive vs builder — preference call
- Whether to use `tracing` for internal logs or just `eprintln!` — fine either way for v1
- Error handling style — `anyhow::Result` everywhere is acceptable for v1; refine to typed errors in v2

## 12. Things to NOT do

- Don't add a database. JSONL files only.
- Don't add a config schema validator beyond what serde gives you for free. TOML errors on parse are fine.
- Don't try to be smart about tool argument validation. Pass the LLM's args to MCP verbatim. MCP will reject bad args and the error feeds back to the LLM naturally.
- Don't build a TUI or web UI. CLI only.
- Don't fork hands or modify it in any way. Driver is a pure consumer of MCP.
- Don't generic-ify the MCP transport (no SSE, no HTTP-MCP). Stdio only for v1.

---

## 13. Pre-flight checklist before delegating

- [ ] hands binary path confirmed and accessible from build directory
- [ ] vLLM serving gpt-oss-20B confirmed: `curl /v1/models` returns the model
- [ ] Test serve command verified to include `--enable-auto-tool-choice --tool-call-parser harmony`
- [ ] Repository forked, builder has clone access
- [ ] ARCHIVE-FIRST applied to any pre-existing files in target directory

---

## 14. Why this spec

This driver is the demo software. It owns the agent loop, the bakeoff orchestration, the audit trail. Everything else (vLLM, hands, models, other MCP servers) is infrastructure beneath it.

Getting the tool-registration mechanism right — and the `ToolRegistry` abstraction that owns it — is what makes "universal" actually true on **two axes simultaneously**:

- **Model axis:** the same code path that registers tools with gpt-oss-20B today registers them with any vLLM-served model tomorrow.
- **Server axis:** the same code path that aggregates hands' tools today aggregates tools from N MCP servers tomorrow.

That's the core artifact. Single-model-single-server is the v1 special case of a fundamentally N-model-N-server design.

The hackathon writeup gets a real line: *"Open universal MCP-aware agent harness, 100% Rust, model-agnostic via OpenAI-compatible inference, server-agnostic via standard MCP, runs against any vLLM/llama.cpp/Ollama backend on AMD or NVIDIA silicon."* This handoff is what makes that line true.
