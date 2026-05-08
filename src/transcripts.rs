use anyhow::Result;
use chrono::Utc;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};

/// Collects events during a run and writes a markdown transcript on completion.
pub struct TranscriptCollector {
    dir: PathBuf,
    model: String,
    run_id: String,
    user_prompt: String,
    iterations: Vec<IterationRecord>,
    current_iteration: u32,
    final_answer: Option<String>,
    duration_ms: u64,
    total_tokens: u32,
    slug: Option<String>,
}

struct IterationRecord {
    number: u32,
    reasoning: Option<String>,
    tool_calls: Vec<ToolCallRecord>,
    content: Option<String>,
}

struct ToolCallRecord {
    name: String,
    args: String,
    ok: bool,
    content_preview: String,
}

impl TranscriptCollector {
    pub fn new() -> Self {
        let dir =
            std::env::var("TRANSCRIPT_DIR").unwrap_or_else(|_| "/opt/cpc/transcripts".to_string());
        Self {
            dir: PathBuf::from(dir),
            model: String::new(),
            run_id: String::new(),
            user_prompt: String::new(),
            iterations: Vec::new(),
            current_iteration: 0,
            final_answer: None,
            duration_ms: 0,
            total_tokens: 0,
            slug: None,
        }
    }

    /// Feed an event into the collector. Call this for every event during the run.
    pub fn feed(&mut self, kind: &str, data: &Value) {
        match kind {
            "run_start" => {
                self.model = data
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.run_id = data
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.user_prompt = data
                    .get("user_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            "llm_request" => {
                let iter_num = data.get("iteration").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                self.current_iteration = iter_num;
                self.iterations.push(IterationRecord {
                    number: iter_num,
                    reasoning: None,
                    tool_calls: Vec::new(),
                    content: None,
                });
            }
            "llm_response" => {
                if let Some(iter) = self.iterations.last_mut() {
                    iter.reasoning = data
                        .get("reasoning")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    iter.content = data
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
            "tool_call" => {
                let name = data
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = data
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}")
                    .to_string();

                // Derive slug from first browser_navigate URL
                if self.slug.is_none() && name == "browser_navigate" {
                    if let Some(url) = serde_json::from_str::<Value>(&args)
                        .ok()
                        .and_then(|a| a.get("url").and_then(|u| u.as_str()).map(String::from))
                    {
                        self.slug = Some(slug_from_url(&url));
                    }
                }
                // Derive slug from first tool_call name if no URL slug yet
                if self.slug.is_none() {
                    self.slug = Some(slug_from_tool_name(&name));
                }

                if let Some(iter) = self.iterations.last_mut() {
                    iter.tool_calls.push(ToolCallRecord {
                        name,
                        args,
                        ok: true,
                        content_preview: String::new(),
                    });
                }
            }
            "tool_result" => {
                let ok = data.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                let content = data.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let preview: String = content.chars().take(200).collect();

                if let Some(iter) = self.iterations.last_mut() {
                    if let Some(tc) = iter.tool_calls.last_mut() {
                        tc.ok = ok;
                        tc.content_preview = preview;
                    }
                }
            }
            "final_answer" => {
                self.final_answer = data
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            "run_end" => {
                self.duration_ms = data
                    .get("duration_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                self.total_tokens = data
                    .get("total_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
            }
            _ => {}
        }
    }

    /// Write the transcript to disk. Returns the path on success.
    /// Errors are logged but never propagated — transcript failure must not break runs.
    pub fn write(&self) -> Option<PathBuf> {
        match self.write_inner() {
            Ok(path) => {
                info!("transcript written to {}", path.display());
                Some(path)
            }
            Err(e) => {
                error!("failed to write transcript: {}", e);
                None
            }
        }
    }

    fn write_inner(&self) -> Result<PathBuf> {
        let now = Utc::now();
        let date_str = now.format("%Y-%m-%d").to_string();
        let time_str = now.format("%H%M%S").to_string();
        let slug = self.slug.as_deref().unwrap_or("untitled");

        let date_dir = self.dir.join(&date_str);
        fs::create_dir_all(&date_dir)?;

        let filename = format!("{}_{}.md", time_str, slug);
        let path = date_dir.join(&filename);

        let iterations_count = self.iterations.len();
        let mut md = String::new();

        md.push_str(&format!(
            "# {} — {} UTC\n\n",
            slug,
            now.format("%Y-%m-%d %H:%M:%S")
        ));
        md.push_str(&format!("**Model:** {}\n", self.model));
        md.push_str(&format!("**Run ID:** {}\n", self.run_id));
        md.push_str(&format!(
            "**Duration:** {}ms ({} iterations)\n",
            self.duration_ms, iterations_count
        ));
        md.push_str(&format!("**Tokens:** {}\n\n", self.total_tokens));

        md.push_str("## User prompt\n");
        md.push_str(&format!("> {}\n\n", self.user_prompt));

        md.push_str("## Agent trace\n");
        for iter in &self.iterations {
            md.push_str(&format!("### Iteration {}\n", iter.number));
            let reasoning = iter.reasoning.as_deref().unwrap_or("(none)");
            md.push_str(&format!("*Reasoning:* {}\n\n", reasoning));

            for tc in &iter.tool_calls {
                md.push_str(&format!("- `tool_call`: `{}({})`\n", tc.name, tc.args));
                md.push_str(&format!(
                    "- `tool_result`: ok={}, content_preview=\"{}\"\n\n",
                    tc.ok, tc.content_preview
                ));
            }

            if let Some(content) = &iter.content {
                if !content.is_empty() && iter.tool_calls.is_empty() {
                    md.push_str(&format!("*Content:* {}\n\n", content));
                }
            }
        }

        md.push_str("## Final answer\n");
        md.push_str(self.final_answer.as_deref().unwrap_or("(no answer)"));
        md.push('\n');

        fs::write(&path, &md)?;
        Ok(path)
    }
}

/// Extract a slug from a URL hostname: strip scheme, take hostname, replace dots with `_`.
pub fn slug_from_url(url: &str) -> String {
    // Try to parse the hostname
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let hostname = without_scheme.split('/').next().unwrap_or("untitled");
    // Remove port if present
    let hostname = hostname.split(':').next().unwrap_or(hostname);
    hostname.replace(['.', '-'], "_")
}

/// Convert a tool name to a slug (lowercase, underscores).
pub fn slug_from_tool_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug_from_url_basic() {
        assert_eq!(slug_from_url("https://amd.com/products"), "amd_com");
        assert_eq!(slug_from_url("https://pokeapi.co/api/v2"), "pokeapi_co");
        assert_eq!(slug_from_url("http://localhost:8080/foo"), "localhost");
    }

    #[test]
    fn test_slug_from_url_no_scheme() {
        assert_eq!(slug_from_url("example.org/page"), "example_org");
    }

    #[test]
    fn test_slug_from_tool_name() {
        assert_eq!(slug_from_tool_name("api_list"), "api_list");
        assert_eq!(slug_from_tool_name("browser_navigate"), "browser_navigate");
        assert_eq!(slug_from_tool_name("My-Tool.v2"), "my_tool_v2");
    }

    #[test]
    fn test_collector_slug_priority() {
        let mut c = TranscriptCollector::new();

        // First: a tool_call that is NOT browser_navigate
        c.feed(
            "tool_call",
            &serde_json::json!({"name": "api_list", "arguments": "{}"}),
        );
        assert_eq!(c.slug.as_deref(), Some("api_list"));

        // A later browser_navigate should NOT override (first slug wins)
        c.feed(
            "tool_call",
            &serde_json::json!({
                "name": "browser_navigate",
                "arguments": "{\"url\":\"https://amd.com\"}"
            }),
        );
        assert_eq!(c.slug.as_deref(), Some("api_list"));
    }

    #[test]
    fn test_collector_slug_browser_navigate_first() {
        let mut c = TranscriptCollector::new();
        c.feed(
            "tool_call",
            &serde_json::json!({
                "name": "browser_navigate",
                "arguments": "{\"url\":\"https://pokeapi.co/api/v2\"}"
            }),
        );
        assert_eq!(c.slug.as_deref(), Some("pokeapi_co"));
    }

    #[test]
    fn test_collector_default_slug() {
        let c = TranscriptCollector::new();
        assert_eq!(c.slug, None); // write() will use "untitled"
    }

    #[test]
    fn test_transcript_format() {
        let mut c = TranscriptCollector::new();
        c.feed(
            "run_start",
            &serde_json::json!({
                "model": "test-model",
                "run_id": "abc123",
                "user_prompt": "Find AMD GPU specs"
            }),
        );
        c.feed("llm_request", &serde_json::json!({"iteration": 1}));
        c.feed(
            "llm_response",
            &serde_json::json!({
                "iteration": 1,
                "content": null,
                "reasoning": "I should search for GPU specs",
                "tool_calls": [{"id": "t1"}]
            }),
        );
        c.feed(
            "tool_call",
            &serde_json::json!({
                "iteration": 1,
                "id": "t1",
                "name": "browser_navigate",
                "arguments": "{\"url\":\"https://amd.com/gpus\"}"
            }),
        );
        c.feed(
            "tool_result",
            &serde_json::json!({
                "iteration": 1,
                "id": "t1",
                "ok": true,
                "content": "Page loaded successfully"
            }),
        );
        c.feed("llm_request", &serde_json::json!({"iteration": 2}));
        c.feed(
            "llm_response",
            &serde_json::json!({
                "iteration": 2,
                "content": "The AMD Instinct MI300X has 192GB HBM3.",
                "reasoning": null
            }),
        );
        c.feed(
            "final_answer",
            &serde_json::json!({"content": "The AMD Instinct MI300X has 192GB HBM3."}),
        );
        c.feed(
            "run_end",
            &serde_json::json!({
                "ok": true,
                "duration_ms": 5432,
                "total_tokens": 1500,
                "iterations": 2
            }),
        );

        // Write to a temp dir
        let tmp = std::env::temp_dir().join("mcpconfig_transcript_test");
        let _ = fs::remove_dir_all(&tmp);
        // Override dir for test
        let mut c2 = c;
        c2.dir = tmp.clone();
        let path = c2.write_inner().unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# amd_com"));
        assert!(content.contains("**Model:** test-model"));
        assert!(content.contains("**Run ID:** abc123"));
        assert!(content.contains("**Duration:** 5432ms"));
        assert!(content.contains("**Tokens:** 1500"));
        assert!(content.contains("> Find AMD GPU specs"));
        assert!(content.contains("### Iteration 1"));
        assert!(content.contains("I should search for GPU specs"));
        assert!(content.contains("`browser_navigate("));
        assert!(content.contains("ok=true"));
        assert!(content.contains("### Iteration 2"));
        assert!(content.contains("## Final answer"));
        assert!(content.contains("AMD Instinct MI300X has 192GB HBM3"));

        let _ = fs::remove_dir_all(&tmp);
    }
}
