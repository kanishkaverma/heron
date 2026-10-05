//! The two model clients. Each keeps its own native transcript verbatim
//! (thinking signatures, encrypted reasoning), so every step resends exactly
//! what the previous one sent plus the new part (SPEC §8).

pub mod anthropic;
pub mod openai;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::auth::{self, CallError};
use crate::{Credentials, Provider};

/// A tool as both vendors describe it.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

/// One text block of the first user message; `cache` puts a breakpoint at its end.
pub struct Block {
    pub text: String,
    pub cache: bool,
}

/// Build the first user message: the view (or compactor context) cut into
/// cache pieces (SPEC §8), then the second block.
pub fn blocks(view: &str, second: &str) -> Vec<Block> {
    let pieces = crate::memory::cut_view(view);
    let marked = pieces.len() - 1;
    let mut out: Vec<Block> = pieces
        .into_iter()
        .enumerate()
        .map(|(k, piece)| Block {
            text: piece.to_string(),
            cache: k < marked,
        })
        .collect();
    out.push(Block {
        text: second.to_string(),
        cache: false,
    });
    out
}

#[derive(Clone, Debug)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// What a step streams: deltas for display, finished entries to log.
pub enum Event {
    Text(String),
    Thought(String),
    /// A finished reply block (logged as `talk`).
    Said(String),
    /// A finished tool call (logged as `tool`).
    Called(ToolUse),
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens neither read from nor written to the cache.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

pub struct Step {
    pub said: Vec<String>,
    pub calls: Vec<ToolUse>,
    pub usage: Usage,
    pub stop: String,
}

#[derive(Debug)]
pub enum LlmError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Cancelled => f.write_str("cancelled"),
            LlmError::Failed(e) => f.write_str(e),
        }
    }
}

pub fn provider_of(model: &str) -> Option<Provider> {
    if model.starts_with("claude-") {
        Some(Provider::Anthropic)
    } else if model.starts_with("gpt-") {
        Some(Provider::OpenAI)
    } else {
        None
    }
}

/// One model conversation: a fresh call per turn, or a compactor exchange.
pub struct Call {
    pub provider: Provider,
    pub model: String,
    effort: String,
    credentials: Arc<dyn Credentials>,
    http: reqwest::Client,
    system: String,
    tools: Vec<Value>,
    /// Native transcript: Anthropic `messages` or OpenAI `input` items.
    transcript: Vec<Value>,
    cache_key: String,
}

const MAX_TRANSIENT_RETRIES: u32 = 4;

impl Call {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        credentials: Arc<dyn Credentials>,
        http: reqwest::Client,
        model: &str,
        effort: &str,
        system: &str,
        tools: &[ToolDef],
        first: Vec<Block>,
        cache_key: &str,
    ) -> Result<Self, String> {
        let provider = provider_of(model).ok_or_else(|| format!("unknown model {model}"))?;
        let (tools, transcript) = match provider {
            Provider::Anthropic => (anthropic::tools(tools), vec![anthropic::user(&first)]),
            Provider::OpenAI => (openai::tools(tools), vec![openai::user(&first)]),
        };
        Ok(Self {
            provider,
            model: model.to_string(),
            effort: effort.to_string(),
            credentials,
            http,
            system: system.to_string(),
            tools,
            transcript,
            cache_key: cache_key.to_string(),
        })
    }

    /// Tool results, then any mid-run user messages, as the next input.
    pub fn push_results(&mut self, results: &[(String, String, bool)], texts: &[String]) {
        match self.provider {
            Provider::Anthropic => self.transcript.push(anthropic::results(results, texts)),
            Provider::OpenAI => self.transcript.extend(openai::results(results, texts)),
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.push_results(&[], &[text.to_string()]);
    }

    /// One request: stream it, append the model's output verbatim.
    pub async fn step(
        &mut self,
        on: &mut (dyn FnMut(Event) + Send),
        cancel: &CancellationToken,
    ) -> Result<Step, LlmError> {
        let body = match self.provider {
            Provider::Anthropic => {
                anthropic::body(&self.model, &self.effort, &self.system, &self.tools, &self.transcript)
            }
            Provider::OpenAI => openai::body(
                &self.model,
                &self.effort,
                &self.system,
                &self.tools,
                &self.transcript,
                &self.cache_key,
            ),
        };
        let mut attempt = 0;
        loop {
            let mut streamed = false;
            let result = self.attempt(&body, on, cancel, &mut streamed).await;
            match result {
                Err(Attempt::Transient(err)) if !streamed && attempt < MAX_TRANSIENT_RETRIES => {
                    attempt += 1;
                    let wait = Duration::from_secs(2u64 << attempt);
                    tracing::warn!(target: "optchat", "{} request failed ({err}); retry {attempt} in {wait:?}", self.model);
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(LlmError::Cancelled),
                        _ = tokio::time::sleep(wait) => {}
                    }
                }
                Err(Attempt::Transient(err) | Attempt::Fatal(err)) => return Err(LlmError::Failed(err)),
                Err(Attempt::Cancelled) => return Err(LlmError::Cancelled),
                Ok((step, output)) => {
                    match self.provider {
                        Provider::Anthropic => self.transcript.push(output),
                        Provider::OpenAI => {
                            if let Value::Array(items) = output {
                                self.transcript.extend(items);
                            }
                        }
                    }
                    return Ok(step);
                }
            }
        }
    }

    async fn attempt(
        &self,
        body: &Value,
        on: &mut (dyn FnMut(Event) + Send),
        cancel: &CancellationToken,
        streamed: &mut bool,
    ) -> Result<(Step, Value), Attempt> {
        let send = auth::authorized(self.credentials.as_ref(), self.provider, |token| {
            let request = match self.provider {
                Provider::Anthropic => anthropic::request(&self.http, &token, body),
                Provider::OpenAI => openai::request(&self.http, &token, body, &self.cache_key),
            };
            async move {
                let response = request.send().await.map_err(|e| CallError::Other(format!("transient: {e}")))?;
                let status = response.status();
                if status.is_success() {
                    return Ok(response);
                }
                let text = response.text().await.unwrap_or_default();
                let message = format!("HTTP {status}: {}", text.chars().take(2000).collect::<String>());
                if status.as_u16() == 401 {
                    Err(CallError::Unauthorized(message))
                } else if status.as_u16() == 429 && !text.contains("usage_limit") || status.as_u16() >= 500 {
                    Err(CallError::Other(format!("transient: {message}")))
                } else {
                    Err(CallError::Other(message))
                }
            }
        });
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(Attempt::Cancelled),
            r = send => r.map_err(|e| match e.strip_prefix("transient: ") {
                Some(rest) => Attempt::Transient(rest.to_string()),
                None => Attempt::Fatal(e),
            })?,
        };
        let mut events = Box::pin(sse(response));
        let mut parser = match self.provider {
            Provider::Anthropic => Parser::Anthropic(anthropic::Parser::default()),
            Provider::OpenAI => Parser::OpenAI(openai::Parser::default()),
        };
        let mut tracked = |event: Event| {
            if matches!(event, Event::Text(_) | Event::Thought(_)) {
                *streamed = true;
            }
            on(event)
        };
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(Attempt::Cancelled),
                next = events.next() => next,
            };
            let Some(frame) = next else { break };
            let (name, data) = frame.map_err(Attempt::Transient)?;
            let done = match &mut parser {
                Parser::Anthropic(p) => p.feed(&name, &data, &mut tracked),
                Parser::OpenAI(p) => p.feed(&name, &data, &mut tracked),
            }
            .map_err(|e| {
                if e.contains("overloaded") || e.contains("rate_limit") || e.contains("server_error") {
                    Attempt::Transient(e)
                } else {
                    Attempt::Fatal(e)
                }
            })?;
            if done {
                break;
            }
        }
        match parser {
            Parser::Anthropic(p) => p.finish(),
            Parser::OpenAI(p) => p.finish(),
        }
        .map_err(Attempt::Transient)
    }
}

enum Attempt {
    Transient(String),
    Fatal(String),
    Cancelled,
}

enum Parser {
    Anthropic(anthropic::Parser),
    OpenAI(openai::Parser),
}

/// Server-sent events as `(event, data)` pairs.
fn sse(
    response: reqwest::Response,
) -> impl futures::Stream<Item = Result<(String, String), String>> {
    let bytes = response.bytes_stream();
    futures::stream::unfold(
        (bytes, Vec::<u8>::new(), false),
        |(mut bytes, mut buf, mut ended)| async move {
            loop {
                if let Some((at, len)) = frame_end(&buf) {
                    let frame: Vec<u8> = buf.drain(..at + len).collect();
                    let text = String::from_utf8_lossy(&frame[..at]).into_owned();
                    let (mut name, mut data) = (String::new(), String::new());
                    for line in text.lines() {
                        if let Some(v) = line.strip_prefix("event:") {
                            name = v.trim().to_string();
                        } else if let Some(v) = line.strip_prefix("data:") {
                            if !data.is_empty() {
                                data.push('\n');
                            }
                            data.push_str(v.strip_prefix(' ').unwrap_or(v));
                        }
                    }
                    if name.is_empty() && data.is_empty() {
                        continue;
                    }
                    return Some((Ok((name, data)), (bytes, buf, ended)));
                }
                if ended {
                    return None;
                }
                match bytes.next().await {
                    Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                    Some(Err(e)) => {
                        ended = true;
                        return Some((Err(format!("stream broke: {e}")), (bytes, buf, ended)));
                    }
                    None => {
                        ended = true;
                        if !buf.iter().all(u8::is_ascii_whitespace) {
                            buf.extend_from_slice(b"\n\n");
                        }
                    }
                }
            }
        },
    )
}

fn frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|at| (at, 2));
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|at| (at, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}
