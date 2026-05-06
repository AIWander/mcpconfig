# bakeoff-driver

Universal MCP-aware agent driver. Bridges any OpenAI-compatible LLM endpoint to any MCP server.

**Status:** Scaffolding — implementation in progress. See [HANDOFF.md](./HANDOFF.md) for the full spec.

## What this is

A single Rust binary that:
1. Spawns one or more MCP servers as subprocesses, speaks MCP over stdio
2. Aggregates their tool definitions into a unified registry
3. Registers that registry with any OpenAI-compatible LLM endpoint
4. Runs an agent loop: send chat → receive tool_calls → dispatch → feed results back → repeat
5. Logs every event to JSONL and produces a human-readable Markdown report

The binary is **model-agnostic** (any vLLM/llama.cpp/Ollama-served model) and **server-agnostic** (any MCP server speaking standard stdio MCP). Adding a new model or server is a config edit, not a code change.

## Initial integration target

- **MCP server:** [hands](https://github.com/AIWander/hands)
- **LLM:** gpt-oss-20B served via vLLM with `--enable-auto-tool-choice --tool-call-parser harmony`
- **Hardware:** AMD GPU (Instinct MI300X)

See HANDOFF.md §5 for vLLM serve specifics and §9 for the smoke-test procedure.

## Quick start (post-build)

```bash
cargo build --release
./target/release/bakeoff-driver run tasks/example_smoke.json
```

Outputs land in `runs/<timestamp>_<task>/`:
- `run.jsonl` — append-only event stream (machine-readable)
- `shared_state.md` — composed report (human-readable)

## License

MIT or Apache-2.0 (TBD).
