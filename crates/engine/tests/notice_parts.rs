//! Notices reach the transcript doc through the engine, on a persistent
//! session like Pi's (the child stays warm and serves later turns from the
//! steering mailbox).
//!
//! Ways it can fail:
//! - A notice mid-turn is dropped, or lands inside the assistant's text part.
//! - A notice sent right after a parked session's `Steered` boundary, where
//!   the Pi driver holds idle cache refreshes, is dropped by the parked gate.
//! - A notice sent while parked with no boundary is folded into the doc. It
//!   is dropped today, which is why the driver holds it back.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, NoticeTone, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode,
};

const CHAT: &str = "chat-notice";

struct NoticingHarness;

#[async_trait]
impl Harness for NoticingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Noticing"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        _request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(32);
        let mut steering = controls.steering;
        let done = || AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-notice".into()),
        };
        let text = |text: &str| AgentEvent::TextDelta { text: text.into() };
        let notice = |tone, text: &str| AgentEvent::Notice {
            tone,
            text: text.into(),
        };
        tokio::spawn(async move {
            let first = [
                AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: "mock-1".into(),
                    tools: vec![],
                    cwd: "/tmp".into(),
                    session_id: "hs-notice".into(),
                    assistant_message_id: "a-1".into(),
                },
                text("first"),
                notice(
                    NoticeTone::Warning,
                    "Cache miss after 12m idle: 80k tokens re-billed (~$0.22)",
                ),
                text(" done"),
                done(),
                notice(NoticeTone::Dim, "Cache warmed: $9.99 (sent while parked)"),
            ];
            for event in first {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
            while let Some(steer) = steering.recv().await {
                let second = [
                    AgentEvent::Steered {
                        assistant_message_id: None,
                        next_assistant_message_id: steer.message_id.map(|id| format!("a-{id}")),
                    },
                    notice(NoticeTone::Dim, "Cache warmed: $0.150045"),
                    text("second"),
                    done(),
                ];
                for event in second {
                    if tx.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn run_command(core: &EngineCore, prompt: &str, message_id: &str) {
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: RunRequest {
                    mcp: None,
                    prompt: prompt.into(),
                    harness: None,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: "/tmp".into(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: true,
                    attachments: Vec::new(),
                    worktree: None,
                    resume: None,
                },
                message_id: message_id.into(),
            },
        )
        .expect("queue run command");
}

fn assistant_entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .collect()
}

async fn until(mut ready: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

#[tokio::test]
async fn notices_land_between_the_agents_text_and_at_the_head_of_a_resumed_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(NoticingHarness));
    let core = EngineCore::assemble(&dir, Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    core.workspace
        .create_space("space-notice", &core.device_id, "/tmp", None, false)
        .unwrap();
    core.workspace
        .create_chat(CHAT, Some("space-notice"), None, None, None)
        .unwrap();
    core.workspace.rename_chat(CHAT, "Pre-titled").unwrap();

    run_command(&core, "first", "msg-user-1");
    until(|| assistant_entries(&core).len() == 1, "the first turn").await;
    run_command(&core, "second", "msg-user-2");
    until(|| assistant_entries(&core).len() == 2, "the second turn").await;

    let entries = assistant_entries(&core);
    assert_eq!(
        entries[0].parts,
        vec![
            MessagePart::Text {
                id: "t0".into(),
                text: "first".into()
            },
            MessagePart::Notice {
                id: "n1".into(),
                tone: NoticeTone::Warning,
                text: "Cache miss after 12m idle: 80k tokens re-billed (~$0.22)".into()
            },
            MessagePart::Text {
                id: "t2".into(),
                text: " done".into()
            },
        ]
    );
    assert_eq!(
        entries[1].parts,
        vec![
            MessagePart::Notice {
                id: "n0".into(),
                tone: NoticeTone::Dim,
                text: "Cache warmed: $0.150045".into()
            },
            MessagePart::Text {
                id: "t1".into(),
                text: "second".into()
            },
        ]
    );
    core.shutdown().await;
}
