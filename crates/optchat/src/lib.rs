//! OptChat: one endless chat whose history is the agent's memory, stored as
//! a binary tree of one-line summaries. See `docs/optchat/SPEC.md`.
//!
//! This crate owns the memory (log, tree, compactor, view), the two model
//! clients, and the turn loop. It is exposed to Zeron as an in-process
//! `Harness`. The engine supplies the one thing it can't own: live OAuth
//! tokens, through [`Credentials`], bound once with [`bind`].

pub mod memory;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::stream::BoxStream;
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{AgentEvent, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Claude Pro/Max OAuth (the Claude Code login).
    Anthropic,
    /// ChatGPT OAuth (the Codex login).
    OpenAI,
}

/// A usable access token. `account_id` is the ChatGPT account id
/// (`chatgpt-account-id` header); `None` for Anthropic.
#[derive(Clone, Debug)]
pub struct Token {
    pub access: String,
    pub account_id: Option<String>,
}

/// Hands out fresh tokens, refreshing and persisting them as needed.
/// `Err` carries a user-facing reason such as "Sign in to Claude".
#[async_trait]
pub trait Credentials: Send + Sync + 'static {
    async fn token(&self, provider: Provider) -> Result<Token, String>;
}

/// OptChat's own login store, `home/auth.json`, in pi's shape:
/// `{"anthropic": {type:"oauth", access, refresh, expires},
///   "openai-codex": {type:"oauth", access, refresh, expires, accountId}}`.
/// The engine's Accounts page writes entries (sign in / out); this reads
/// them and refreshes expired tokens in place. Both writers hold an `flock`
/// on the sidecar `auth.json.lock`.
pub struct FileCredentials {
    pub path: PathBuf,
}

impl FileCredentials {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl Credentials for FileCredentials {
    async fn token(&self, provider: Provider) -> Result<Token, String> {
        Err(format!("Sign in to {provider:?} for OptChat"))
    }
}

/// What the host binds before the first run.
#[derive(Clone)]
pub struct Binding {
    /// The memory lives in `home/chat/` (spec §2).
    pub home: PathBuf,
    pub credentials: Arc<dyn Credentials>,
}

static BINDING: OnceLock<Binding> = OnceLock::new();

/// Bind the process-wide memory home and token source. The first call wins:
/// one process owns one chat (the spec's single-writer rule).
pub fn bind(binding: Binding) {
    let _ = BINDING.set(binding);
}

pub fn binding() -> Option<&'static Binding> {
    BINDING.get()
}

pub const DISPLAY_NAME: &str = "OptChat";

pub const REASONING_LEVELS: [ReasoningLevel; 4] = [
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
];

#[derive(Default)]
pub struct OptChatHarness;

impl OptChatHarness {
    pub fn new() -> Self {
        Self
    }
}

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
        Ok(Vec::new())
    }
    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        Err(HarnessError::Protocol("OptChat is not implemented yet".into()))
    }
}
