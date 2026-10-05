//! Claude over the Messages API with the Claude Code OAuth identity
//! (IMPL.md "Wire facts").

use serde_json::{Value, json};

use super::{Block, Event, Step, ToolDef, ToolUse, Usage};
use crate::Token;

const URL: &str = "https://api.anthropic.com/v1/messages";
/// OAuth requests must open the system prompt with exactly this block.
const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const MAX_TOKENS: u64 = 64_000;

fn ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

pub fn tools(defs: &[ToolDef]) -> Vec<Value> {
    defs.iter()
        .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema}))
        .collect()
}

pub fn user(blocks: &[Block]) -> Value {
    let content: Vec<Value> = blocks
        .iter()
        .filter(|b| !b.text.is_empty())
        .map(|b| {
            let mut block = json!({"type": "text", "text": b.text});
            if b.cache {
                block["cache_control"] = ephemeral();
            }
            block
        })
        .collect();
    json!({"role": "user", "content": content})
}

pub fn results(results: &[(String, String, bool)], texts: &[String]) -> Value {
    let mut content: Vec<Value> = results
        .iter()
        .map(|(id, output, is_error)| {
            json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": if output.is_empty() { "(no output)" } else { output.as_str() },
                "is_error": is_error,
            })
        })
        .collect();
    content.extend(
        texts
            .iter()
            .map(|t| json!({"type": "text", "text": t})),
    );
    json!({"role": "user", "content": content})
}

/// Breakpoints: up to three in the view (on its first user message) plus
/// the request end (top-level automatic `cache_control`). Anthropic allows
/// four, so the system blocks carry none (SPEC §8).
pub fn body(model: &str, effort: &str, system: &str, tools: &[Value], messages: &[Value]) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": [
            {"type": "text", "text": IDENTITY},
            {"type": "text", "text": system},
        ],
        "messages": messages,
        "thinking": {"type": "adaptive", "display": "summarized"},
        "output_config": {"effort": effort},
        "cache_control": ephemeral(),
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
    }
    body
}

pub fn request(http: &reqwest::Client, token: &Token, body: &Value) -> reqwest::RequestBuilder {
    http.post(URL)
        .header("authorization", format!("Bearer {}", token.access))
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
        .header("user-agent", "claude-cli/2.1.280")
        .header("x-app", "cli")
        .header("accept", "text/event-stream")
        .header("content-type", "application/json")
        .json(body)
}

#[derive(Default)]
pub struct Parser {
    /// Content blocks as the API sent them, deltas applied.
    blocks: Vec<Value>,
    partial_json: Vec<String>,
    said: Vec<String>,
    calls: Vec<ToolUse>,
    usage: Usage,
    stop: String,
    stopped: bool,
}

impl Parser {
    fn usage_from(&mut self, usage: &Value) {
        let get = |k: &str| usage.get(k).and_then(Value::as_u64);
        if let Some(v) = get("input_tokens") {
            self.usage.input = v;
        }
        if let Some(v) = get("cache_read_input_tokens") {
            self.usage.cache_read = v;
        }
        if let Some(v) = get("cache_creation_input_tokens") {
            self.usage.cache_write = v;
        }
        if let Some(v) = get("output_tokens") {
            self.usage.output = v;
        }
    }

    /// Returns true at `message_stop`.
    pub fn feed(
        &mut self,
        name: &str,
        data: &str,
        on: &mut dyn FnMut(Event),
    ) -> Result<bool, String> {
        let event: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) if name == "ping" => return Ok(false),
            Err(e) => return Err(format!("bad event {name}: {e}")),
        };
        let kind = event["type"].as_str().unwrap_or(name);
        match kind {
            "message_start" => {
                if let Some(usage) = event.pointer("/message/usage") {
                    self.usage_from(usage);
                }
            }
            "content_block_start" => {
                let index = event["index"].as_u64().unwrap_or(self.blocks.len() as u64) as usize;
                let mut block = event["content_block"].clone();
                if block["type"] == "tool_use" {
                    block["input"] = json!({});
                }
                while self.blocks.len() <= index {
                    self.blocks.push(Value::Null);
                    self.partial_json.push(String::new());
                }
                self.blocks[index] = block;
            }
            "content_block_delta" => {
                let index = event["index"].as_u64().unwrap_or(0) as usize;
                let delta = &event["delta"];
                let Some(block) = self.blocks.get_mut(index) else {
                    return Err("delta for an unknown block".into());
                };
                let append = |block: &mut Value, field: &str, text: &str| {
                    let old = block[field].as_str().unwrap_or("").to_string();
                    block[field] = Value::String(old + text);
                };
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or("");
                        append(block, "text", text);
                        on(Event::Text(text.to_string()));
                    }
                    "thinking_delta" => {
                        let text = delta["thinking"].as_str().unwrap_or("");
                        append(block, "thinking", text);
                        on(Event::Thought(text.to_string()));
                    }
                    "signature_delta" => {
                        append(block, "signature", delta["signature"].as_str().unwrap_or(""));
                    }
                    "input_json_delta" => {
                        self.partial_json[index].push_str(delta["partial_json"].as_str().unwrap_or(""));
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = event["index"].as_u64().unwrap_or(0) as usize;
                let Some(block) = self.blocks.get_mut(index) else {
                    return Ok(false);
                };
                match block["type"].as_str().unwrap_or("") {
                    "tool_use" => {
                        let raw = std::mem::take(&mut self.partial_json[index]);
                        let input: Value = if raw.trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(&raw).map_err(|e| format!("bad tool input JSON: {e}"))?
                        };
                        block["input"] = input.clone();
                        let call = ToolUse {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                            input,
                        };
                        on(Event::Called(call.clone()));
                        self.calls.push(call);
                    }
                    "text" => {
                        if let Some(map) = block.as_object_mut() {
                            // Citations are not resent; the text is the reply.
                            map.remove("citations");
                        }
                        let text = block["text"].as_str().unwrap_or("").to_string();
                        if !text.trim().is_empty() {
                            on(Event::Said(text.clone()));
                            self.said.push(text);
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(stop) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = stop.to_string();
                }
                if let Some(usage) = event.get("usage") {
                    self.usage_from(usage);
                }
            }
            "message_stop" => {
                self.stopped = true;
                return Ok(true);
            }
            "error" => {
                let error = &event["error"];
                return Err(format!(
                    "{}: {}",
                    error["type"].as_str().unwrap_or("error"),
                    error["message"].as_str().unwrap_or(data)
                ));
            }
            _ => {}
        }
        Ok(false)
    }

    /// The step and the assistant message to append to the transcript.
    pub fn finish(self) -> Result<(Step, Value), String> {
        if !self.stopped {
            return Err("the stream ended before message_stop".into());
        }
        let content: Vec<Value> = self
            .blocks
            .into_iter()
            .filter(|b| !b.is_null())
            // The API rejects empty text blocks when they are sent back.
            .filter(|b| !(b["type"] == "text" && b["text"].as_str().is_none_or(|t| t.is_empty())))
            .collect();
        let message = json!({"role": "assistant", "content": content});
        Ok((
            Step {
                said: self.said,
                calls: self.calls,
                usage: self.usage,
                stop: self.stop,
            },
            message,
        ))
    }
}
