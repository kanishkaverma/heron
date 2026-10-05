//! The turn loop (SPEC §7) mapped onto one Zeron run (IMPL.md "Mapping onto
//! Zeron's harness contract").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeron_harness::SteerMessage;
use zeron_proto::{AgentEvent, DoneStatus};

use crate::chat::Chat;
use crate::debug::{self, RequestKind};
use crate::llm::{Call, Event, LlmError, blocks, provider_of};
use crate::memory::Kind;
use crate::tools;
use crate::{Provider, REASONING_LEVELS};

pub const SESSION_ID: &str = "optchat";

pub struct Turn {
    pub chat: Arc<Chat>,
    pub prompt: String,
    pub model: String,
    pub effort: String,
    pub cwd: PathBuf,
    pub steering: mpsc::Receiver<SteerMessage>,
    pub cancel: CancellationToken,
    pub tx: mpsc::UnboundedSender<AgentEvent>,
}

pub fn effort(level: Option<zeron_proto::ReasoningLevel>) -> &'static str {
    use zeron_proto::ReasoningLevel as R;
    match level {
        Some(R::Minimal | R::Low) => "low",
        Some(R::Medium) => "medium",
        Some(R::XHigh | R::Max | R::Ultra | R::Ultracode | R::Ultrathink) => "xhigh",
        Some(R::High) | None => "high",
    }
}

pub fn context_window(model: &str) -> u64 {
    match provider_of(model) {
        Some(Provider::OpenAI) => 272_000,
        _ => 1_000_000,
    }
}

pub const MODELS: [(&str, &str); 4] = [
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("gpt-6-sol", "GPT-6 Sol"),
    ("gpt-6-luna", "GPT-6 Luna"),
];

pub fn models() -> Vec<zeron_proto::Model> {
    MODELS
        .iter()
        .map(|(id, label)| zeron_proto::Model {
            id: id.to_string(),
            label: label.to_string(),
            description: None,
            reasoning_levels: REASONING_LEVELS.to_vec(),
            options: Vec::new(),
        })
        .collect()
}

/// Why a run stopped early.
enum Stop {
    Interrupted,
    Failed(String),
}

impl Turn {
    fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }

    /// Steering that has arrived, each confirmed with a `Steered` boundary.
    fn take_steering(&mut self, assistant: &mut String) -> Vec<String> {
        let mut texts = Vec::new();
        while let Ok(steer) = self.steering.try_recv() {
            let next = uuid::Uuid::new_v4().to_string();
            self.emit(AgentEvent::Steered {
                assistant_message_id: Some(std::mem::replace(assistant, next.clone())),
                next_assistant_message_id: Some(next),
            });
            texts.push(steer.prompt);
        }
        texts
    }

    /// Untaken messages stay in the log, unanswered (SPEC §6, §7).
    fn keep_untaken(&mut self, queue: Vec<String>, assistant: &mut String) {
        let mut texts = queue;
        texts.extend(self.take_steering(assistant));
        for text in texts {
            if let Err(err) = self.chat.log(Kind::User, &text) {
                tracing::error!(target: "optchat", "could not log a message: {err}");
            }
        }
    }

    pub async fn run(mut self, assistant_id: String) {
        let mut assistant = assistant_id;
        let status = match self.drive(&mut assistant).await {
            Ok(()) => (DoneStatus::Completed, None),
            Err(Stop::Interrupted) => (DoneStatus::Interrupted, None),
            Err(Stop::Failed(err)) => {
                self.emit(AgentEvent::Error {
                    message: err.clone(),
                });
                (DoneStatus::Errored, Some(err))
            }
        };
        self.emit(AgentEvent::Done {
            status: status.0,
            result: None,
            error: status.1,
            session_id: Some(SESSION_ID.into()),
        });
    }

    async fn drive(&mut self, assistant: &mut String) -> Result<(), Stop> {
        let mut queue = vec![std::mem::take(&mut self.prompt)];
        let mut call_no = 0u64;
        loop {
            queue.extend(self.take_steering(assistant));
            if queue.is_empty() {
                return Ok(());
            }
            // SPEC §6: no turn starts while a view line is unsummarized.
            let cancel = self.cancel.clone();
            if !self.chat.settle(&cancel).await {
                self.keep_untaken(queue, assistant);
                return Err(Stop::Interrupted);
            }
            let texts = std::mem::take(&mut queue);
            // Render BEFORE logging the new messages: they go whole in block 2.
            let view = {
                let state = self.chat.state();
                debug::note_turn(state.mem.first_unbuilt() == state.mem.len());
                state.mem.render_view()
            };
            for text in &texts {
                self.chat.log(Kind::User, text).map_err(Stop::Failed)?;
            }
            call_no = debug::next_call().max(call_no + 1);
            let mut call = Call::new(
                self.chat.credentials.clone(),
                self.chat.http.clone(),
                &self.model,
                &self.effort,
                &self.chat.system_prompt(),
                &tools::defs(),
                blocks(&view, &texts.join("\n\n")),
                &self.chat.cache_key,
            )
            .map_err(Stop::Failed)?;
            self.call(&mut call, call_no, assistant).await?;
        }
    }

    /// One fresh model call: steps until the model stops calling tools.
    async fn call(&mut self, call: &mut Call, call_no: u64, assistant: &mut String) -> Result<(), Stop> {
        let mut step_no = 0usize;
        loop {
            let started = Instant::now();
            let chat = self.chat.clone();
            let tx = self.tx.clone();
            let mut log_error: Option<String> = None;
            let mut on = |event: Event| match event {
                Event::Text(text) => {
                    let _ = tx.send(AgentEvent::TextDelta { text });
                }
                // Thoughts are shown, never logged (SPEC §2).
                Event::Thought(text) => {
                    let _ = tx.send(AgentEvent::ReasoningDelta { text });
                }
                Event::Said(text) => {
                    if let Err(err) = chat.log(Kind::Talk, &text) {
                        log_error.get_or_insert(err);
                    }
                }
                Event::Called(use_) => {
                    let line = format!("{} {}", use_.name, use_.input);
                    if let Err(err) = chat.log(Kind::Tool, &line) {
                        log_error.get_or_insert(err);
                    }
                    let _ = tx.send(AgentEvent::ToolCall {
                        id: use_.id.clone(),
                        call: tools::display(&use_),
                    });
                }
            };
            let cancel = self.cancel.clone();
            let result = call.step(&mut on, &cancel).await;
            if let Some(err) = log_error {
                return Err(Stop::Failed(err));
            }
            let step = match result {
                Ok(step) => step,
                Err(LlmError::Cancelled) => {
                    self.keep_untaken(Vec::new(), assistant);
                    return Err(Stop::Interrupted);
                }
                Err(LlmError::Failed(err)) => return Err(Stop::Failed(err)),
            };
            debug::note_turn_request(RequestKind::Turn, &self.model, call_no, step_no, step.usage, started);
            self.emit(AgentEvent::Usage {
                input_tokens: step.usage.prompt(),
                output_tokens: step.usage.output,
            });
            self.emit(AgentEvent::ContextUsage {
                tokens: Some(step.usage.prompt() + step.usage.output),
                window: Some(context_window(&self.model)),
            });
            step_no += 1;
            if step.calls.is_empty() {
                return Ok(());
            }
            let mut results = Vec::new();
            for use_ in &step.calls {
                let cancel = self.cancel.clone();
                let outcome = tokio::select! {
                    _ = cancel.cancelled() => None,
                    outcome = tools::run(&self.chat, &self.cwd, use_) => Some(outcome),
                };
                let Some(outcome) = outcome else {
                    let note = "(interrupted by the user)".to_string();
                    let _ = self.chat.log(Kind::Echo, &note);
                    self.emit(AgentEvent::ToolResult {
                        id: use_.id.clone(),
                        is_error: true,
                        output: Some(note),
                        diff: None,
                    });
                    self.keep_untaken(Vec::new(), assistant);
                    return Err(Stop::Interrupted);
                };
                let (text, is_error) = match outcome {
                    Ok(text) => (tools::cap(text), false),
                    Err(text) => (tools::cap(text), true),
                };
                self.chat.log(Kind::Echo, &text).map_err(Stop::Failed)?;
                self.emit(AgentEvent::ToolResult {
                    id: use_.id.clone(),
                    is_error,
                    output: Some(text.clone()),
                    diff: None,
                });
                results.push((use_.id.clone(), text, is_error));
            }
            // Mid-run messages reach the model at this tool boundary, logged first.
            let steering = self.take_steering(assistant);
            for text in &steering {
                self.chat.log(Kind::User, text).map_err(Stop::Failed)?;
            }
            call.push_results(&results, &steering);
        }
    }
}
