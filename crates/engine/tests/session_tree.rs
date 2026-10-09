use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::{Arc, Mutex};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, ChatConfig, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionTree, SessionTreeReply, SteeringMode, TreeEntry, TreeEntryKind,
};
use zeron_rpc::methods;

/// The tree the harness reports, and the session it was asked about.
struct Branching(Arc<Mutex<Vec<(String, String)>>>);
#[async_trait]
impl Harness for Branching {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Branching"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn session_tree(
        &self,
        session_id: &str,
        cwd: &std::path::Path,
    ) -> Result<Option<SessionTree>, HarnessError> {
        self.0
            .lock()
            .unwrap()
            .push((session_id.into(), cwd.display().to_string()));
        Ok(Some(SessionTree {
            leaf_id: Some("a1".into()),
            entries: vec![TreeEntry {
                id: "a1".into(),
                depth: 0,
                kind: TreeEntryKind::Assistant,
                text: "hello".into(),
                label: None,
                on_path: true,
            }],
        }))
    }
    async fn run(
        &self,
        _: RunRequest,
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        unreachable!("the tree is read without running the agent")
    }
}

/// `GetSessionTree` answers from the chat's own harness session.
/// Ways it fails:
/// - The method is unknown to the engine.
/// - It asks the harness about no session, or one from another chat, or
///   a cwd other than the one the session was created under.
/// - A chat that has not run yet errors instead of having no tree.
/// - A chat on another device is read from this device's disk.
#[tokio::test]
async fn session_tree_is_read_from_the_chats_own_harness_session() {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Branching(asked.clone())));
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None).unwrap();
    let config = ChatConfig {
        harness: HarnessId::Mock,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
    };
    for chat in ["fresh", "ran"] {
        core.workspace
            .create_chat(chat, None, Some(&core.device_id), None, Some("/tmp".into()))
            .unwrap();
        core.workspace.set_chat_config(chat, &config).unwrap();
    }
    core.workspace
        .create_chat(
            "elsewhere",
            None,
            Some("other-device"),
            None,
            Some("/tmp".into()),
        )
        .unwrap();
    core.workspace
        .set_chat_harness_session("ran", "pi-session-1", "/work/tree");
    let client = zeron_rpc::memory_client(core.rpc_service());

    let none = client
        .call_as::<SessionTreeReply>(
            methods::GET_SESSION_TREE,
            serde_json::json!({"chatId": "fresh"}),
        )
        .await
        .unwrap();
    assert_eq!(none.tree, None);
    assert!(asked.lock().unwrap().is_empty());

    let tree = client
        .call_as::<SessionTreeReply>(
            methods::GET_SESSION_TREE,
            serde_json::json!({"chatId": "ran"}),
        )
        .await
        .unwrap()
        .tree
        .expect("a chat that ran has a tree");
    assert_eq!(tree.leaf_id.as_deref(), Some("a1"));
    assert_eq!(tree.entries[0].text, "hello");
    assert_eq!(
        *asked.lock().unwrap(),
        [("pi-session-1".to_string(), "/work/tree".to_string())]
    );

    let error = client
        .call(
            methods::GET_SESSION_TREE,
            serde_json::json!({"chatId": "elsewhere"}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("another device"), "{error}");
}
