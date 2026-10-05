//! Process-wide evidence for the e2e driver (`optchat-e2e live`): per-request
//! usage and the invariant counters behind IMPL.md's failure list. Cheap
//! enough to keep on in production; nothing here changes behavior.

use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;

use crate::llm::Usage;
use crate::memory::PLACEHOLDER;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RequestKind {
    Turn,
    Compact,
}

#[derive(Clone, Debug, Serialize)]
pub struct Request {
    pub kind: RequestKind,
    pub model: String,
    /// Turn requests: the run's call number and step within it. Compactor
    /// requests: the try number.
    pub call: u64,
    pub step: usize,
    pub usage: Usage,
    pub ms: u128,
}

#[derive(Default, Clone, Debug, Serialize)]
pub struct Evidence {
    pub requests: Vec<Request>,
    pub compactor_inputs: u64,
    /// Failure 4: compactor inputs holding a placeholder line.
    pub compactor_placeholders: u64,
    /// Failure 5: compactor input lines that start like `id+n|`.
    pub compactor_id_lines: u64,
    pub compactor_failures: Vec<String>,
    /// Per compactor call: (level, bytes of each try).
    pub tries: Vec<(u32, Vec<usize>)>,
    /// Failure 11: turns whose view was rendered with an unsummarized line.
    pub unsettled_turns: u64,
    pub turns: u64,
}

static EVIDENCE: Mutex<Option<Evidence>> = Mutex::new(None);
static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn with<R>(f: impl FnOnce(&mut Evidence) -> R) -> R {
    let mut slot = EVIDENCE.lock().unwrap_or_else(|e| e.into_inner());
    f(slot.get_or_insert_with(Evidence::default))
}

pub fn evidence() -> Evidence {
    with(|e| e.clone())
}

pub(crate) fn next_call() -> u64 {
    CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

pub(crate) fn note_request(kind: RequestKind, model: &str, step: usize, usage: Usage, started: Instant) {
    note_turn_request(kind, model, 0, step, usage, started);
}

pub(crate) fn note_turn_request(
    kind: RequestKind,
    model: &str,
    call: u64,
    step: usize,
    usage: Usage,
    started: Instant,
) {
    with(|e| {
        e.requests.push(Request {
            kind,
            model: model.to_string(),
            call,
            step,
            usage,
            ms: started.elapsed().as_millis(),
        })
    });
}

fn looks_like_id_line(line: &str) -> bool {
    let Some((head, _)) = line.split_once('|') else {
        return false;
    };
    let Some((id, n)) = head.split_once('+') else {
        return false;
    };
    !id.is_empty()
        && !n.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && n.bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn check_compactor_input(context: &str, step: &str) {
    let placeholders = (context.contains(PLACEHOLDER) || step.contains(PLACEHOLDER)) as u64;
    let ids = context
        .lines()
        .chain(step.lines())
        .filter(|l| looks_like_id_line(l))
        .count() as u64;
    with(|e| {
        e.compactor_inputs += 1;
        e.compactor_placeholders += placeholders;
        e.compactor_id_lines += ids;
    });
}

pub(crate) fn note_compactor_failure(failure: String) {
    with(|e| e.compactor_failures.push(failure));
}

pub(crate) fn note_tries(level: u32, sizes: Vec<usize>) {
    with(|e| e.tries.push((level, sizes)));
}

pub(crate) fn note_turn(settled: bool) {
    with(|e| {
        e.turns += 1;
        e.unsettled_turns += (!settled) as u64;
    });
}

/// The live view as rendered now, and its parts.
pub fn live_view() -> Option<String> {
    let chat = crate::chat::loaded()?;
    let state = chat.state();
    Some(state.mem.render_view())
}

/// Every logged message (kind, text), in order.
pub fn messages() -> Vec<crate::memory::Message> {
    crate::chat::loaded()
        .map(|chat| chat.state().mem.root.clone())
        .unwrap_or_default()
}

/// Wait until the compactor has nothing left to start or finish.
pub async fn wait_idle(timeout: std::time::Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(chat) = crate::chat::loaded() {
            let state = chat.state();
            if state.busy.is_empty() && state.mem.due_nodes(&state.busy, 1).is_empty() {
                return true;
            }
        } else {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    false
}

/// Drop the process's chat (memory, store, compactor) and release its lock.
pub async fn unload() {
    crate::chat::unload().await;
}

/// Open the chat from the binding now (what the first run would do).
pub fn load() -> Result<(), String> {
    crate::chat::chat().map(|_| ())
}
