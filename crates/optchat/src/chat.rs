//! The one chat of this process: memory + store + compactor, created lazily
//! from [`crate::binding`] on first use and shared by every run.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use sha2::Digest;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::Credentials;
use crate::memory::{Kind, Memory, Message, VIEW};
use crate::store::Store;

pub(crate) struct State {
    pub mem: Memory,
    pub busy: HashSet<(u32, u64)>,
    /// Nodes whose failure was already reported (reported once each).
    pub failed: HashSet<(u32, u64)>,
}

pub(crate) struct Chat {
    pub home: PathBuf,
    pub credentials: Arc<dyn Credentials>,
    pub http: reqwest::Client,
    pub store: Store,
    state: Mutex<State>,
    /// Bumped on every change to the memory: settle waits on it.
    changed: watch::Sender<u64>,
    /// Stops the compactor when the chat is unloaded.
    pub shutdown: CancellationToken,
    /// The user's instructions file, read once so the system prompt stays
    /// byte-identical for the life of the process.
    pub instructions: Option<String>,
    /// OpenAI routing key: stable per chat so consecutive turns share a cache.
    pub cache_key: String,
}

static CHAT: Mutex<Option<Arc<Chat>>> = Mutex::new(None);

/// The process's chat, opened from the binding on first use.
pub(crate) fn chat() -> Result<Arc<Chat>, String> {
    let mut slot = CHAT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(chat) = slot.as_ref() {
        return Ok(chat.clone());
    }
    let binding = crate::binding().ok_or("OptChat has no memory home bound")?;
    let chat = Chat::open(binding.home.clone(), binding.credentials.clone())?;
    *slot = Some(chat.clone());
    drop(slot);
    crate::compactor::pump(&chat);
    Ok(chat)
}

/// The loaded chat, if any (no lazy open).
pub(crate) fn loaded() -> Option<Arc<Chat>> {
    CHAT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Drop the chat: stop the compactor, wait for its tasks to let go, release
/// the lock. The next run folds the memory again from disk.
pub(crate) async fn unload() {
    let Some(chat) = CHAT.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    chat.shutdown.cancel();
    for _ in 0..600 {
        if Arc::strong_count(&chat) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl Chat {
    fn open(home: PathBuf, credentials: Arc<dyn Credentials>) -> Result<Arc<Chat>, String> {
        let (store, mem, reports) = Store::open(&home)?;
        for report in reports {
            tracing::warn!(target: "optchat", "{report}");
        }
        let instructions = std::fs::read_to_string(home.join("AGENTS.md"))
            .ok()
            .filter(|s| !s.trim().is_empty());
        let digest = sha2::Sha256::digest(home.as_os_str().as_encoded_bytes());
        let cache_key = format!(
            "optchat-{}",
            digest[..8].iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Chat {
            home,
            credentials,
            http,
            store,
            state: Mutex::new(State {
                mem,
                busy: HashSet::new(),
                failed: HashSet::new(),
            }),
            changed: watch::channel(0).0,
            shutdown: CancellationToken::new(),
            instructions,
            cache_key,
        }))
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn notify(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    /// Log one message: fsync it, append it, refit, wake the compactor.
    pub fn log(self: &Arc<Self>, kind: Kind, text: &str) -> Result<u64, String> {
        let i = {
            let mut state = self.state();
            let i = state.mem.len();
            let date = chrono::Local::now()
                .format("%Y-%m-%dT%H:%M:%S%.3f%:z")
                .to_string();
            let message = Message::new(i, kind, text.to_string(), date);
            // Disk first, under the state lock, so ids land in order.
            self.store.append_message(&message)?;
            state.mem.append(message)?;
            state.mem.fit(VIEW);
            i
        };
        self.notify();
        crate::compactor::pump(self);
        Ok(i)
    }

    /// SPEC §6: resolves true once every view line is a summary, false if
    /// `cancel` fires first.
    pub async fn settle(&self, cancel: &CancellationToken) -> bool {
        let mut changes = self.changed.subscribe();
        loop {
            {
                let state = self.state();
                if state.mem.first_unbuilt() == state.mem.len() {
                    return true;
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => return false,
                changed = changes.changed() => if changed.is_err() { return false },
            }
        }
    }

    pub fn system_prompt(&self) -> String {
        let mut system = format!("{}\n\n{}", crate::prompts::MASTER, crate::prompts::VIEW_DOC);
        if let Some(instructions) = &self.instructions {
            system.push_str("\n\n");
            system.push_str(instructions);
        }
        system
    }

    /// SPEC §7.1 `date(id)`.
    pub fn date(&self, id: u64) -> String {
        let state = self.state();
        match state.mem.root.get(id as usize) {
            Some(message) => chrono::DateTime::parse_from_rfc3339(&message.date)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%A %Y-%m-%d %H:%M:%S %:z")
                        .to_string()
                })
                .unwrap_or_else(|_| message.date.clone()),
            None => format!("No message {id}."),
        }
    }
}
