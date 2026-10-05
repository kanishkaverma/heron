//! `OptChatHarness`: Zeron's `Harness` contract over the one chat.

use async_trait::async_trait;
use futures::stream::BoxStream;
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SlashCommand,
    SteeringMode,
};

use crate::turn::{self, SESSION_ID, Turn};
use crate::{DISPLAY_NAME, OptChatHarness, Provider, REASONING_LEVELS};

const MEMORY_COMMAND: &str = "memory";

#[async_trait]
impl Harness for OptChatHarness {
    fn id(&self) -> HarnessId {
        HarnessId::OptChat
    }
    fn display_name(&self) -> &str {
        DISPLAY_NAME
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &REASONING_LEVELS
    }
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(turn::models())
    }
    fn fallback_models(&self) -> Vec<Model> {
        turn::models()
    }
    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        Ok(vec![SlashCommand {
            name: MEMORY_COMMAND.into(),
            description: "Write the whole memory (view, log, tree) as one HTML page".into(),
            input_hint: None,
        }])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (Ok(event), rx))
        });
        let chat = crate::chat::chat().map_err(HarnessError::Protocol)?;
        let model = match request.model.as_deref() {
            Some(m) if turn::MODELS.iter().any(|(id, _)| *id == m) => m.to_string(),
            _ if chat.credentials.token(Provider::Anthropic).await.is_ok() => "claude-opus-5-5".into(),
            _ => "gpt-6-sol".into(),
        };
        let assistant = uuid::Uuid::new_v4().to_string();
        let _ = tx.send(AgentEvent::SessionStarted {
            harness: HarnessId::OptChat,
            model: model.clone(),
            tools: crate::tools::defs().iter().map(|t| t.name.to_string()).collect(),
            cwd: request.cwd.clone(),
            session_id: SESSION_ID.into(),
            assistant_message_id: assistant.clone(),
        });
        if request.prompt.trim() == format!("/{MEMORY_COMMAND}") {
            let (status, text, error) = match crate::export::write(&chat) {
                Ok(path) => (DoneStatus::Completed, format!("Wrote the memory to {}", path.display()), None),
                Err(err) => (DoneStatus::Errored, String::new(), Some(err)),
            };
            if !text.is_empty() {
                let _ = tx.send(AgentEvent::TextDelta { text });
            }
            let _ = tx.send(AgentEvent::Done {
                status,
                result: None,
                error,
                session_id: Some(SESSION_ID.into()),
            });
            return Ok(Box::pin(stream));
        }
        // Dropping the stream stops the run like an interrupt.
        let cancel = controls.interrupt.child_token();
        let watcher = {
            let (tx, cancel) = (tx.clone(), cancel.clone());
            tokio::spawn(async move {
                tx.closed().await;
                cancel.cancel();
            })
        };
        let lease = controls.execution_lease;
        let turn = Turn {
            chat,
            prompt: request.prompt,
            model,
            effort: turn::effort(request.reasoning).to_string(),
            cwd: request.cwd.into(),
            steering: controls.steering,
            cancel,
            tx,
        };
        tokio::spawn(async move {
            turn.run(assistant).await;
            watcher.abort();
            drop(lease);
        });
        Ok(Box::pin(stream))
    }
}
