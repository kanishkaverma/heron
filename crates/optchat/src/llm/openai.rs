//! ChatGPT over the Codex Responses endpoint with the ChatGPT OAuth login
//! (IMPL.md "Wire facts"). `store: false`: every output item, reasoning
//! included with its encrypted content, is sent back verbatim.

use serde_json::{Value, json};

use super::{Block, Event, Step, ToolDef, ToolUse, Usage};
use crate::Token;

const URL: &str = "https://chatgpt.com/backend-api/codex/responses";

pub fn tools(defs: &[ToolDef]) -> Vec<Value> {
    defs.iter()
        .map(|t| {
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.schema,
            })
        })
        .collect()
}

pub fn user(blocks: &[Block]) -> Value {
    let content: Vec<Value> = blocks
        .iter()
        .filter(|b| !b.text.is_empty())
        .map(|b| json!({"type": "input_text", "text": b.text}))
        .collect();
    json!({"type": "message", "role": "user", "content": content})
}

pub fn results(results: &[(String, String, bool)], texts: &[String]) -> Vec<Value> {
    let mut items: Vec<Value> = results
        .iter()
        .map(|(id, output, _)| json!({"type": "function_call_output", "call_id": id, "output": output}))
        .collect();
    if !texts.is_empty() {
        let content: Vec<Value> = texts
            .iter()
            .map(|t| json!({"type": "input_text", "text": t}))
            .collect();
        items.push(json!({"type": "message", "role": "user", "content": content}));
    }
    items
}

/// `reasoning.context: "all_turns"` keeps earlier reasoning in the prompt
/// after a mid-run user message, so the cache still hits (SPEC §8).
pub fn body(
    model: &str,
    effort: &str,
    instructions: &str,
    tools: &[Value],
    input: &[Value],
    cache_key: &str,
) -> Value {
    let mut body = json!({
        "model": model,
        "store": false,
        "stream": true,
        "instructions": instructions,
        "input": input,
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": cache_key,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "reasoning": {"effort": effort, "summary": "auto", "context": "all_turns"},
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
    }
    body
}

pub fn request(
    http: &reqwest::Client,
    token: &Token,
    body: &Value,
    session: &str,
) -> reqwest::RequestBuilder {
    let mut request = http
        .post(URL)
        .header("authorization", format!("Bearer {}", token.access))
        .header("originator", "zeron")
        .header("OpenAI-Beta", "responses=experimental")
        .header("accept", "text/event-stream")
        .header("content-type", "application/json")
        .header("session-id", session);
    if let Some(account) = &token.account_id {
        request = request.header("chatgpt-account-id", account);
    }
    request.json(body)
}

#[derive(Default)]
pub struct Parser {
    items: Vec<(u64, Value)>,
    said: Vec<String>,
    calls: Vec<ToolUse>,
    usage: Usage,
    stop: String,
    completed: bool,
    summary_parts: u64,
}

impl Parser {
    pub fn feed(
        &mut self,
        name: &str,
        data: &str,
        on: &mut dyn FnMut(Event),
    ) -> Result<bool, String> {
        if data.trim() == "[DONE]" {
            return Ok(self.completed);
        }
        let event: Value =
            serde_json::from_str(data).map_err(|e| format!("bad event {name}: {e}"))?;
        let kind = event["type"].as_str().unwrap_or(name);
        match kind {
            "response.output_text.delta" => {
                on(Event::Text(event["delta"].as_str().unwrap_or("").to_string()));
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                on(Event::Thought(event["delta"].as_str().unwrap_or("").to_string()));
            }
            "response.reasoning_summary_part.done" => {
                self.summary_parts += 1;
                on(Event::Thought("\n\n".into()));
            }
            "response.output_item.done" => {
                let index = event["output_index"].as_u64().unwrap_or(self.items.len() as u64);
                let item = event["item"].clone();
                match item["type"].as_str().unwrap_or("") {
                    "message" => {
                        let text: String = item["content"]
                            .as_array()
                            .map(|parts| {
                                parts
                                    .iter()
                                    .filter_map(|p| p["text"].as_str().or(p["refusal"].as_str()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !text.trim().is_empty() {
                            on(Event::Said(text.clone()));
                            self.said.push(text);
                        }
                    }
                    "function_call" => {
                        let raw = item["arguments"].as_str().unwrap_or("{}");
                        let input: Value = if raw.trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(raw).map_err(|e| format!("bad tool arguments: {e}"))?
                        };
                        let call = ToolUse {
                            id: item["call_id"].as_str().unwrap_or("").to_string(),
                            name: item["name"].as_str().unwrap_or("").to_string(),
                            input,
                        };
                        on(Event::Called(call.clone()));
                        self.calls.push(call);
                    }
                    _ => {}
                }
                self.items.push((index, item));
            }
            "response.completed" | "response.incomplete" => {
                let response = &event["response"];
                if let Some(usage) = response.get("usage") {
                    let input = usage["input_tokens"].as_u64().unwrap_or(0);
                    let cached = usage
                        .pointer("/input_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let written = usage
                        .pointer("/input_tokens_details/cache_write_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    self.usage = Usage {
                        input: input.saturating_sub(cached + written),
                        output: usage["output_tokens"].as_u64().unwrap_or(0),
                        cache_read: cached,
                        cache_write: written,
                    };
                }
                self.stop = response["status"].as_str().unwrap_or("completed").to_string();
                if kind == "response.incomplete" {
                    let reason = response
                        .pointer("/incomplete_details/reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    self.stop = format!("incomplete: {reason}");
                }
                self.completed = true;
                return Ok(true);
            }
            "response.failed" => {
                let error = &event["response"]["error"];
                return Err(format!(
                    "{}: {}",
                    error["code"].as_str().unwrap_or("failed"),
                    error["message"].as_str().unwrap_or(data)
                ));
            }
            "error" => {
                return Err(format!(
                    "{}: {}",
                    event["code"].as_str().unwrap_or("error"),
                    event["message"].as_str().unwrap_or(data)
                ));
            }
            _ => {}
        }
        Ok(false)
    }

    pub fn finish(mut self) -> Result<(Step, Value), String> {
        if !self.completed {
            return Err("the stream ended before response.completed".into());
        }
        self.items.sort_by_key(|(index, _)| *index);
        let items: Vec<Value> = self.items.into_iter().map(|(_, item)| item).collect();
        Ok((
            Step {
                said: self.said,
                calls: self.calls,
                usage: self.usage,
                stop: self.stop,
            },
            Value::Array(items),
        ))
    }
}
