//! The compactor (SPEC §4): a pump that builds tree nodes in a strict order
//! with a cheap model, and enforces the size of each line.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::chat::Chat;
use crate::llm::{Call, LlmError, blocks};
use crate::memory::{NODE, Node, VIEW, cut_bytes};
use crate::prompts::{COMPACT, COMPRESS, MERGE, SCALE, SCALE_INTRO};
use crate::{Provider, debug};

pub const JOBS: usize = 8;
pub const TRIES: usize = 5;
/// Flat, not exponential: the next turn waits on these nodes (SPEC §4.1).
pub const RETRY: Duration = Duration::from_secs(10);

/// Start every due node, up to `JOBS` at once (SPEC §4.1).
pub(crate) fn pump(chat: &Arc<Chat>) {
    if chat.shutdown.is_cancelled() {
        return;
    }
    let due = {
        let mut state = chat.state();
        let due = state.mem.due_nodes(&state.busy, JOBS);
        state.busy.extend(due.iter().copied());
        due
    };
    for (l, i) in due {
        let chat = chat.clone();
        tokio::spawn(async move { run(chat, l, i).await });
    }
}

async fn run(chat: Arc<Chat>, l: u32, i: u64) {
    let result = tokio::select! {
        _ = chat.shutdown.cancelled() => return,
        result = build(&chat, l, i) => result,
    };
    let saved = result.and_then(|node| {
        // Save (fsync), then memory, then refit (SPEC §4.3).
        chat.store.append_node(&node)?;
        let mut state = chat.state();
        state.mem.insert(node);
        state.mem.fit(VIEW);
        state.busy.remove(&(l, i));
        state.failed.remove(&(l, i));
        if state.failed.is_empty() {
            state.last_failure = None;
        }
        Ok(())
    });
    match saved {
        Ok(()) => chat.notify(),
        Err(err) => {
            let first = {
                let mut state = chat.state();
                state.last_failure = Some(err.clone());
                state.failed.insert((l, i))
            };
            if first {
                let n = 1u64 << l;
                tracing::warn!(target: "optchat", "summary of {}+{n} failed (retrying every {RETRY:?}): {err}", i << l);
                debug::note_compactor_failure(format!("{}+{n}: {err}", i << l));
            }
            tokio::select! {
                _ = chat.shutdown.cancelled() => return,
                _ = tokio::time::sleep(RETRY) => {}
            }
            chat.state().busy.remove(&(l, i));
        }
    }
    pump(&chat);
}

fn flatten(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// Build node `(l, i)`: free when its source fits, else one compactor call.
async fn build(chat: &Arc<Chat>, l: u32, i: u64) -> Result<Node, String> {
    let (context, step) = {
        let state = chat.state();
        let mem = &state.mem;
        if l == 0 {
            let message = &mem.root[i as usize];
            let line = message.line();
            if line.len() <= NODE {
                return Ok(Node::new(0, i, line));
            }
            (
                mem.render_context(i)?,
                format!("{SCALE_INTRO}\n{SCALE}\n\n{COMPRESS}\n{line}"),
            )
        } else {
            let a = &mem.tree[&(l - 1, 2 * i)].text;
            let b = &mem.tree[&(l - 1, 2 * i + 1)].text;
            if a.len() + 1 + b.len() <= NODE {
                return Ok(Node::new(l, i, format!("{a}\n{b}")));
            }
            (
                mem.render_context((i + 1) << l)?,
                format!(
                    "{SCALE_INTRO}\n{SCALE}\n\n{MERGE}\n{}\n{}",
                    flatten(a),
                    flatten(b)
                ),
            )
        }
    };
    debug::check_compactor_input(&context, &step);
    let (model, effort) = compactor_model(chat).await?;
    let mut call = Call::new(
        chat.credentials.clone(),
        chat.http.clone(),
        model,
        effort,
        COMPACT,
        &[],
        blocks(&context, &step),
        &format!("{}-compact", chat.cache_key),
    )?;
    let mut tries: Vec<String> = Vec::new();
    loop {
        let started = Instant::now();
        let step = call
            .step(&mut |_| {}, &chat.shutdown)
            .await
            .map_err(|e| match e {
                LlmError::Cancelled => "shut down".to_string(),
                LlmError::Failed(e) => e,
            })?;
        debug::note_request(debug::RequestKind::Compact, model, tries.len(), step.usage, started);
        let line = step.said.join("\n").trim().to_string();
        if line.is_empty() {
            return Err(format!("empty reply (stop: {})", step.stop));
        }
        tries.push(line.clone());
        if line.len() <= NODE || tries.len() >= TRIES {
            break;
        }
        call.push_user(&format!(
            "That line is {} bytes; the limit is {NODE}. It must end where it is cut here:\n{}| ← LIMIT",
            line.len(),
            cut_bytes(&line, NODE)
        ));
    }
    debug::note_tries(l, tries.iter().map(String::len).collect());
    // The first shortest try (min_by_key keeps the first of equals).
    let best = tries.into_iter().min_by_key(String::len).unwrap_or_default();
    Ok(Node::new(l, i, best))
}

/// Claude Sonnet at medium when Claude is signed in, else GPT-6 Luna at medium.
async fn compactor_model(chat: &Chat) -> Result<(&'static str, &'static str), String> {
    if chat.credentials.token(Provider::Anthropic).await.is_ok() {
        return Ok(("claude-sonnet-5-5", "medium"));
    }
    chat.credentials
        .token(Provider::OpenAI)
        .await
        .map(|_| ("gpt-6-luna", "medium"))
        .map_err(|_| "Sign in to Claude or ChatGPT so OptChat can summarize its memory".to_string())
}
