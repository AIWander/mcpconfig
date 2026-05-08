use anyhow::Result;
use serde_json::Value;
use std::fs;
use std::io::BufRead;
use std::path::Path;

/// Read a run.jsonl and compose a shared_state.md report.
pub fn compose_report(run_dir: &Path) -> Result<String> {
    let jsonl_path = run_dir.join("run.jsonl");
    let file = fs::File::open(&jsonl_path)?;
    let reader = std::io::BufReader::new(file);

    let events: Vec<Value> = reader
        .lines()
        .filter_map(|line| line.ok())
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();

    let mut md = String::new();

    // Extract run metadata
    let run_start = events.iter().find(|e| e["kind"] == "run_start");
    let task_name = run_start
        .and_then(|e| e["task"].as_str())
        .unwrap_or("unknown");
    let model_name = run_start
        .and_then(|e| e["model"].as_str())
        .unwrap_or("unknown");
    let timestamp = run_start
        .and_then(|e| e["ts"].as_str())
        .unwrap_or("unknown");

    // Find user prompt
    let user_prompt = run_start
        .and_then(|e| e["user_prompt"].as_str())
        .unwrap_or("");

    // Find tools_registered
    let tools_event = events.iter().find(|e| e["kind"] == "tools_registered");
    let tool_count = tools_event.and_then(|e| e["count"].as_u64()).unwrap_or(0);

    // Find final_answer
    let final_answer = events.iter().find(|e| e["kind"] == "final_answer");
    let answer_text = final_answer
        .and_then(|e| e["content"].as_str())
        .unwrap_or("(no answer)");

    // Find run_end
    let run_end = events.iter().find(|e| e["kind"] == "run_end");
    let duration_ms = run_end.and_then(|e| e["duration_ms"].as_u64()).unwrap_or(0);
    let total_tokens = run_end
        .and_then(|e| e["total_tokens"].as_u64())
        .unwrap_or(0);

    // Count iterations
    let max_iteration = events
        .iter()
        .filter_map(|e| e["iteration"].as_u64())
        .max()
        .unwrap_or(0);

    // Header
    md.push_str(&format!("# Bakeoff: {} - {}\n\n", task_name, timestamp));
    if !user_prompt.is_empty() {
        md.push_str(&format!("**Task:** {}\n\n", user_prompt));
    }
    md.push_str("---\n\n");

    // Model section
    let base_url = run_start.and_then(|e| e["base_url"].as_str()).unwrap_or("");
    md.push_str(&format!("## Model: {}\n\n", model_name));
    if !base_url.is_empty() {
        md.push_str(&format!(
            "**Endpoint:** `{}` · **Tools:** {} registered\n\n",
            base_url, tool_count
        ));
    } else {
        md.push_str(&format!("**Tools:** {} registered\n\n", tool_count));
    }

    // Final answer
    md.push_str("### Final answer\n\n");
    md.push_str(answer_text);
    md.push_str("\n\n");

    // Reasoning trace
    md.push_str(&format!(
        "### Reasoning trace ({} iterations, {} ms, {} tokens)\n\n",
        max_iteration, duration_ms, total_tokens
    ));

    // Group events by iteration
    for iter in 1..=max_iteration {
        md.push_str(&format!("#### Iteration {}\n\n", iter));

        // Reasoning from llm_response
        let llm_resp = events
            .iter()
            .find(|e| e["kind"] == "llm_response" && e["iteration"].as_u64() == Some(iter));
        if let Some(resp) = llm_resp {
            if let Some(reasoning) = resp["reasoning"].as_str() {
                if !reasoning.is_empty() {
                    let truncated = if reasoning.len() > 500 {
                        format!("{}...", &reasoning[..500])
                    } else {
                        reasoning.to_string()
                    };
                    md.push_str(&format!("*Reasoning:* {}\n\n", truncated));
                }
            }
        }

        // Tool calls and results
        let calls: Vec<&Value> = events
            .iter()
            .filter(|e| e["kind"] == "tool_call" && e["iteration"].as_u64() == Some(iter))
            .collect();

        if calls.is_empty() {
            if let Some(fa) = events
                .iter()
                .find(|e| e["kind"] == "final_answer" && e["iteration"].as_u64() == Some(iter))
            {
                md.push_str(&format!(
                    "*Final answer.* {}\n\n",
                    fa["content"].as_str().unwrap_or("")
                ));
            }
        } else {
            md.push_str("*Tool calls:*\n");
            for call in calls {
                let name = call["name"].as_str().unwrap_or("?");
                let args = &call["arguments"];
                let call_id = call["id"].as_str().unwrap_or("");

                // Find matching result
                let result = events
                    .iter()
                    .find(|e| e["kind"] == "tool_result" && e["id"].as_str() == Some(call_id));
                let result_text = result
                    .and_then(|r| r["content"].as_str())
                    .unwrap_or("(no result)");
                let truncated_result = if result_text.len() > 200 {
                    format!("{}...", &result_text[..200])
                } else {
                    result_text.to_string()
                };

                md.push_str(&format!(
                    "- `{}({})` -> `{}`\n",
                    name, args, truncated_result
                ));
            }
            md.push_str("\n");
        }
    }

    md.push_str("---\n");

    Ok(md)
}

/// Write the shared_state.md report to the run directory.
pub fn write_report(run_dir: &Path) -> Result<()> {
    let report = compose_report(run_dir)?;
    let path = run_dir.join("shared_state.md");
    fs::write(path, report)?;
    Ok(())
}
