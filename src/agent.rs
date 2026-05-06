use anyhow::{bail, Result};
use serde_json::json;
use tracing::info;

use crate::config::{ModelConfig, Task};
use crate::events::EventWriter;
use crate::openai::{ChatCompletionRequest, Message, OpenAIClient, Usage};
use crate::registry::ToolRegistry;

/// Run the agent loop for a task.
pub async fn run_agent_loop(
    task: &Task,
    model: &ModelConfig,
    client: &OpenAIClient,
    registry: &mut ToolRegistry,
    events: &mut EventWriter,
    system_prompt: &str,
) -> Result<AgentResult> {
    let tools_for_llm = registry.to_openai_tools();
    let tool_names: Vec<String> = tools_for_llm.iter().map(|t| t.function.name.clone()).collect();

    events.log(
        "tools_registered",
        json!({"count": tool_names.len(), "names": tool_names}),
    )?;

    // Build initial messages
    let mut messages: Vec<Message> = Vec::new();

    if model.system_prompt_strategy == "first_user_turn" {
        messages.push(Message {
            role: "user".to_string(),
            content: Some(format!("{}\n\n{}", system_prompt, task.user_prompt)),
            tool_calls: None,
            tool_call_id: None,
        });
    } else {
        messages.push(Message {
            role: "system".to_string(),
            content: Some(system_prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
        messages.push(Message {
            role: "user".to_string(),
            content: Some(task.user_prompt.clone()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    let start = std::time::Instant::now();
    let mut total_usage = Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    };
    let mut iterations = 0;

    loop {
        iterations += 1;
        if iterations > task.max_iterations {
            bail!("max iterations ({}) exceeded", task.max_iterations);
        }

        events.log(
            "llm_request",
            json!({
                "iteration": iterations,
                "model": model.model_id,
                "message_count": messages.len()
            }),
        )?;

        let request = ChatCompletionRequest {
            model: model.model_id.clone(),
            messages: messages.clone(),
            tools: if tools_for_llm.is_empty() {
                None
            } else {
                Some(tools_for_llm.clone())
            },
            tool_choice: if tools_for_llm.is_empty() {
                None
            } else {
                Some("auto".to_string())
            },
            max_tokens: Some(model.max_tokens),
            temperature: Some(model.temperature),
            stream: false,
        };

        let response = client.chat_completion(&request).await?;

        if let Some(usage) = &response.usage {
            total_usage.prompt_tokens += usage.prompt_tokens;
            total_usage.completion_tokens += usage.completion_tokens;
            total_usage.total_tokens += usage.total_tokens;
        }

        let choice = response
            .choices
            .first()
            .ok_or_else(|| anyhow::anyhow!("no choices in LLM response"))?;

        let content = choice.message.content.clone();
        let reasoning = choice.message.reasoning_content.clone();
        let tool_calls = choice.message.tool_calls.clone().unwrap_or_default();

        // Optionally strip thinking tags from content
        let display_content = if model.strip_thinking_tags {
            content.as_deref().map(strip_thinking_tags).map(String::from)
        } else {
            content.clone()
        };

        events.log(
            "llm_response",
            json!({
                "iteration": iterations,
                "content": display_content,
                "tool_calls": tool_calls,
                "reasoning": reasoning,
                "usage": response.usage
            }),
        )?;

        if tool_calls.is_empty() {
            // Final answer
            let final_content = display_content.unwrap_or_default();
            events.log(
                "final_answer",
                json!({"iteration": iterations, "content": final_content}),
            )?;

            let duration = start.elapsed();
            return Ok(AgentResult {
                final_answer: final_content,
                iterations,
                duration_ms: duration.as_millis() as u64,
                total_usage,
            });
        }

        // Build assistant message (without reasoning_content — don't echo it back)
        messages.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
        });

        // Dispatch each tool call
        for call in &tool_calls {
            info!(
                "iteration {}: calling {}({})",
                iterations, call.function.name, call.function.arguments
            );

            events.log(
                "tool_call",
                json!({
                    "iteration": iterations,
                    "id": call.id,
                    "name": call.function.name,
                    "arguments": call.function.arguments
                }),
            )?;

            let args: serde_json::Value =
                serde_json::from_str(&call.function.arguments).unwrap_or(json!({}));

            let result = registry.dispatch(&call.function.name, args).await;

            let (ok, content_str) = match result {
                Ok(r) => (!r.is_error, r.text_content()),
                Err(e) => (false, format!("Error: {}", e)),
            };

            events.log(
                "tool_result",
                json!({
                    "iteration": iterations,
                    "id": call.id,
                    "ok": ok,
                    "content": content_str
                }),
            )?;

            messages.push(Message {
                role: "tool".to_string(),
                content: Some(content_str),
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
            });
        }
    }
}

fn strip_thinking_tags(content: &str) -> &str {
    // Strip <|channel|>analysis...<|end|> blocks
    // For v1 simplicity, if the entire content is wrapped, strip it
    // Otherwise return as-is (regex stripping is v2)
    content
}

pub struct AgentResult {
    pub final_answer: String,
    pub iterations: u32,
    pub duration_ms: u64,
    pub total_usage: Usage,
}
