//! OptChat's end-to-end checks (docs/optchat/IMPL.md "Verification contract").
//!
//! - `fold-sim`: replay synthetic messages through `Memory` (no network).
//! - `login <anthropic|openai> --home <dir>`: real sign-in into `<dir>/auth.json`.
//! - `live --home <dir>`: drive `OptChatHarness` against the real APIs.

use std::collections::HashSet;
use std::process::ExitCode;

use zeron_optchat::memory::{Kind, Memory, Message, NODE, Node, Part, VIEW};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("fold-sim") => fold_sim(&args[1..]),
        Some("login") => runtime().and_then(|rt| rt.block_on(login(&args[1..]))),
        Some("store-check") => store_check(&args[1..]),
        Some("live") => runtime().and_then(|rt| rt.block_on(live::live(&args[1..]))),
        _ => Err("usage: optchat-e2e <fold-sim | login <anthropic|openai> --home <dir> | live --home <dir>>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("FAIL: {err}");
            ExitCode::FAILURE
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Runtime::new().map_err(|e| e.to_string())
}

fn home_arg(args: &[String]) -> Result<std::path::PathBuf, String> {
    let home = flag(args, "--home").ok_or("--home <dir> is required")?;
    std::fs::create_dir_all(home).map_err(|e| format!("create {home}: {e}"))?;
    Ok(std::path::PathBuf::from(home))
}

// ---------------------------------------------------------------------------
// login
// ---------------------------------------------------------------------------

/// Prints `AUTH_URL <url>`, accepts a pasted code or redirect URL on stdin,
/// waits for the loopback, writes `<home>/auth.json`, prints `SIGNED_IN <label>`.
async fn login(args: &[String]) -> Result<(), String> {
    use std::io::Write as _;
    let provider = match args.first().map(String::as_str) {
        Some("anthropic") => zeron_optchat::Provider::Anthropic,
        Some("openai") => zeron_optchat::Provider::OpenAI,
        _ => return Err("usage: optchat-e2e login <anthropic|openai> --home <dir>".into()),
    };
    let home = home_arg(args)?;
    let start = zeron_optchat::auth::start_login(provider, home.join("auth.json")).await?;
    println!("AUTH_URL {}", start.url);
    println!("MODE {:?} PORT {:?}", start.mode, start.callback_port);
    let _ = std::io::stdout().flush();
    let code = start.code;
    // A blocking reader: an EOF (no terminal) leaves the loopback as the only way in.
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) && !line.trim().is_empty() {
            let _ = code.send(line);
        } else {
            std::mem::forget(code);
        }
    });
    let label = start.done.await.map_err(|e| e.to_string())??;
    println!("SIGNED_IN {label}");
    Ok(())
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|k| args.get(k + 1))
        .map(String::as_str)
}

// ---------------------------------------------------------------------------
// fold-sim
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
    /// Log-uniform in [lo, hi]: most messages small, a few huge.
    fn log_range(&mut self, lo: u64, hi: u64) -> u64 {
        let (a, b) = ((lo as f64).ln(), (hi as f64).ln());
        let unit = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        (a + (b - a) * unit).exp().round() as u64
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

const WORDS: &[&str] = &[
    "parser", "fold", "view", "cache", "merge", "user", "decided", "file", "src/main.rs", "error",
    "fixed", "test", "commit", "branch", "deploy", "tree", "node", "budget", "token", "retry",
    "compactor", "summary", "zoom", "keeps", "drops", "PR", "#4821", "review", "Victor", "build",
    "failed", "passed", "because", "the", "a", "of", "to", "and", "in", "kernel", "HVM", "Bend",
];

fn words(rng: &mut Rng, bytes: u64) -> String {
    let mut out = String::with_capacity(bytes as usize + 16);
    while (out.len() as u64) < bytes {
        if !out.is_empty() {
            out.push(if rng.chance(3) { '\n' } else { ' ' });
        }
        out.push_str(WORDS[rng.range(0, WORDS.len() as u64 - 1) as usize]);
    }
    out.truncate(bytes as usize);
    out
}

/// A synthetic chat: turns of one user message, a geometric number of tool
/// steps (mean 2: many turns are a question and a reply, a few run long),
/// and a reply.
fn synthetic_messages(rng: &mut Rng, count: u64) -> Vec<Message> {
    let mut out = Vec::with_capacity(count as usize);
    while (out.len() as u64) < count {
        let mut push = |kind: Kind, size: u64, rng: &mut Rng| {
            let i = out.len() as u64;
            if i < count {
                out.push(Message::new(i, kind, words(rng, size), String::new()));
            }
        };
        push(Kind::User, rng.log_range(10, 4_000), rng);
        let mut steps = 0;
        while steps < 40 && rng.chance(67) {
            steps += 1;
        }
        for _ in 0..steps {
            push(Kind::Tool, rng.log_range(30, 600), rng);
            push(Kind::Echo, rng.log_range(20, 30_000), rng);
        }
        push(Kind::Talk, rng.log_range(40, 3_000), rng);
    }
    out
}

/// What the compactor would write, sized like real summaries.
fn synthetic_node(rng: &mut Rng, mem: &Memory, l: u32, i: u64) -> Node {
    if l == 0 {
        let message = &mem.root[i as usize];
        if message.size as usize <= NODE {
            return Node::new(0, i, message.line());
        }
        let size = rng.range(60, NODE as u64);
        return Node::new(0, i, words(rng, size));
    }
    let a = &mem.tree[&(l - 1, 2 * i)].text;
    let b = &mem.tree[&(l - 1, 2 * i + 1)].text;
    if a.len() + 1 + b.len() <= NODE {
        return Node::new(l, i, format!("{a}\n{b}"));
    }
    let size = if rng.chance(4) {
        rng.range(NODE as u64 + 1, NODE as u64 + 40)
    } else {
        rng.range(250, NODE as u64)
    };
    Node::new(l, i, words(rng, size))
}

/// Failure 2: `new` tiles `[0, total)` and every part of `old` is in `new`
/// or covered by one of its parts (never split).
fn check_fold(old: &[Part], new: &[Part], total: u64) -> Result<(), String> {
    let mut pos = 0;
    for p in new {
        if p.start() != pos {
            return Err(format!("view does not tile: part {}+{} at {pos}", p.start(), p.n()));
        }
        pos = p.end();
    }
    if pos != total {
        return Err(format!("view covers [0, {pos}) of {total} messages"));
    }
    let mut k = 0;
    for p in old {
        while new[k].end() <= p.start() {
            k += 1;
        }
        let q = new[k];
        if q.l < p.l || q.end() < p.end() {
            return Err(format!(
                "part {}+{} was split into {}+{}",
                p.start(),
                p.n(),
                q.start(),
                q.n()
            ));
        }
    }
    Ok(())
}

/// Failure 1: over budget only while no adjacent sibling pair has a built parent.
fn check_budget(mem: &Memory) -> Result<(), String> {
    if mem.view_size() <= VIEW {
        return Ok(());
    }
    for w in mem.view.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a.l == b.l && a.i % 2 == 0 && b.i == a.i + 1 && mem.built(a.l + 1, a.i / 2) {
            return Err(format!(
                "view {} B > {VIEW} B with buildable pair {}+{}",
                mem.view_size(),
                a.start(),
                2 * a.n()
            ));
        }
    }
    Ok(())
}

struct Sim {
    mem: Memory,
    rng: Rng,
    max_after_fit: usize,
    fits: u64,
}

impl Sim {
    fn fit(&mut self) -> Result<(), String> {
        let old = self.mem.view.clone();
        self.mem.fit(VIEW);
        self.fits += 1;
        check_fold(&old, &self.mem.view, self.mem.len())?;
        check_budget(&self.mem)?;
        self.max_after_fit = self.max_after_fit.max(self.mem.view_size());
        Ok(())
    }

    /// Build up to `max` due nodes in pump order, refitting after each.
    fn pump(&mut self, max: usize) -> Result<usize, String> {
        let mut built = 0;
        while built < max {
            let due = self.mem.due_nodes(&HashSet::new(), 1);
            let Some(&(l, i)) = due.first() else { break };
            // Failure 4: the compactor's context never holds a placeholder.
            // Rendering 128 KB per node is the slow part, so sample it.
            if (self.mem.tree.len() + l as usize).is_multiple_of(61) {
                let upto = if l == 0 { i } else { (i + 1) << l };
                self.mem.render_context(upto)?;
            }
            let node = synthetic_node(&mut self.rng, &self.mem, l, i);
            self.mem.insert(node);
            self.fit()?;
            built += 1;
        }
        Ok(built)
    }
}

/// Failure 10: folding the log from message 0 over the stored tree gives the live view.
fn reload(mem: &Memory) -> Memory {
    let mut fresh = Memory::default();
    for node in mem.tree.values() {
        fresh.insert(node.clone());
    }
    for message in &mem.root {
        fresh.append(message.clone()).expect("ordered log");
        fresh.fit(VIEW);
    }
    fresh
}

fn shared_prefix_chars(a: &str, b: &str) -> usize {
    // Chunked slice compares (memcmp) keep this fast in a debug build.
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let limit = x.len().min(y.len());
    let mut end = 0;
    while end + 4096 <= limit && x[end..end + 4096] == y[end..end + 4096] {
        end += 4096;
    }
    while end < limit && x[end] == y[end] {
        end += 1;
    }
    while !a.is_char_boundary(end) {
        end -= 1;
    }
    a[..end].chars().count()
}

fn median(values: &mut [usize]) -> usize {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

fn fold_sim(args: &[String]) -> Result<(), String> {
    let count: u64 = flag(args, "--messages")
        .map(|v| v.parse().map_err(|_| format!("bad --messages {v}")))
        .transpose()?
        .unwrap_or(20_000);
    let seed: u64 = flag(args, "--seed")
        .map(|v| v.parse().map_err(|_| format!("bad --seed {v}")))
        .transpose()?
        .unwrap_or(0x0b7c_4a7e);
    let started = std::time::Instant::now();
    let mut rng = Rng(seed | 1);
    let messages = synthetic_messages(&mut rng, count);
    let mut sim = Sim {
        mem: Memory::default(),
        rng,
        max_after_fit: 0,
        fits: 0,
    };
    let mut previous: Option<String> = None;
    let mut prefixes = Vec::new();
    let mut render_chars = Vec::new();
    // Per turn, how many of the previous view's cache pieces survive whole.
    let mut pieces_read = [0usize; 5];
    let mut max_settled = 0usize;
    let mut reloads = 0;
    for message in messages {
        if message.kind == Kind::User {
            // A turn waits until every view line is summarized (SPEC §6).
            sim.pump(usize::MAX)?;
            if sim.mem.first_unbuilt() != sim.mem.len() {
                return Err(format!("settle left message {} unsummarized", sim.mem.first_unbuilt()));
            }
            max_settled = max_settled.max(sim.mem.view_size());
            let recount: usize = sim
                .mem
                .view
                .iter()
                .map(|p| sim.mem.tree[&(p.l, p.i)].text.len())
                .sum();
            if recount != sim.mem.view_size() {
                return Err(format!("view size drifted: {} vs {recount}", sim.mem.view_size()));
            }
            let view = sim.mem.render_view();
            if let Some(prev) = &previous {
                let (old, new) = (zeron_optchat::memory::cut_view(prev), zeron_optchat::memory::cut_view(&view));
                let read = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
                pieces_read[read.min(4)] += 1;
                prefixes.push(shared_prefix_chars(prev, &view));
                render_chars.push(view.chars().count());
            }
            if message.i > 0 && message.i / 5_000 != (message.i - 1) / 5_000 {
                if reload(&sim.mem).view != sim.mem.view {
                    return Err(format!("reload at {} messages differs from the live view", message.i));
                }
                reloads += 1;
            }
            previous = Some(view);
        }
        sim.mem.append(message).map_err(|e| e.to_string())?;
        sim.fit()?;
        // The compactor lags a little behind the log between turns.
        let lag = sim.rng.range(0, 3) as usize;
        sim.pump(lag)?;
    }
    sim.pump(usize::MAX)?;
    if reload(&sim.mem).view != sim.mem.view {
        return Err("final reload differs from the live view".into());
    }
    reloads += 1;
    let view = sim.mem.render_view();
    // SPEC §8: the view's cache pieces end at the last line end before each mark.
    let pieces = zeron_optchat::memory::cut_view(&view);
    let mut chars = 0;
    for (k, piece) in pieces.iter().enumerate() {
        chars += piece.chars().count();
        if k + 1 < pieces.len() {
            let mark = zeron_optchat::memory::MARKS[k];
            let next_line = view[piece.as_ptr() as usize - view.as_ptr() as usize + piece.len()..]
                .find('\n')
                .map_or(usize::MAX, |at| chars + at);
            if !piece.ends_with('\n') || chars > mark || next_line < mark {
                return Err(format!("cache piece {k} does not end at the last line end before {mark}"));
            }
        }
    }
    if pieces.concat() != view || pieces.len() != 4 {
        return Err(format!("view cut into {} pieces that do not rejoin it", pieces.len()));
    }
    println!("cache pieces        {:?} chars", pieces.iter().map(|p| p.chars().count()).collect::<Vec<_>>());
    let mean_prefix = prefixes.iter().sum::<usize>() / prefixes.len().max(1);
    let median_prefix = median(&mut prefixes);
    let median_render = median(&mut render_chars);
    let levels: std::collections::BTreeMap<u32, usize> =
        sim.mem.view.iter().fold(Default::default(), |mut acc, p| {
            *acc.entry(p.l).or_default() += 1;
            acc
        });
    println!("messages            {}", sim.mem.len());
    println!("nodes built         {}", sim.mem.tree.len());
    println!("fits checked        {}", sim.fits);
    println!("final view          {} B of part text, {} B rendered, {} lines", sim.mem.view_size(), view.len(), sim.mem.view.len());
    println!("parts per level     {levels:?}");
    println!("max view after fit  {} B (budget {VIEW} B)", sim.max_after_fit);
    println!("max settled view    {max_settled} B");
    println!("turns compared      {}", prefixes.len());
    println!("median shared prefix {median_prefix} chars of a median {median_render}-char render (mean {mean_prefix})");
    println!("view pieces read    {pieces_read:?} turns reading 0..=4 whole pieces of the previous view");
    println!("reloads matched     {reloads}");
    println!("elapsed             {:.1?}", started.elapsed());
    // Failure 3: consecutive renders share well past half the view.
    if median_prefix * 2 <= median_render {
        return Err(format!(
            "median shared prefix {median_prefix} is not past half of {median_render}"
        ));
    }
    if max_settled > VIEW {
        return Err(format!("settled view {max_settled} B exceeds {VIEW} B"));
    }
    println!("PASS fold-sim (failures 1, 2, 3, 4, 10)");
    Ok(())
}

// ---------------------------------------------------------------------------
// store-check: failures 7 and 8, offline
// ---------------------------------------------------------------------------

fn store_check(args: &[String]) -> Result<(), String> {
    use std::io::Write as _;
    use zeron_optchat::store::Store;
    let home = match flag(args, "--home") {
        Some(h) => std::path::PathBuf::from(h),
        None => std::env::temp_dir().join(format!("optchat-store-check-{}", std::process::id())),
    };
    let _ = std::fs::remove_dir_all(&home);
    let (store, mem, _) = Store::open(&home)?;
    if !mem.is_empty() {
        return Err("fresh store is not empty".into());
    }
    // Failure 8: a second writer on the same chat is refused while the first lives.
    match Store::open(&home) {
        Err(e) if e.contains("another process") => println!("ok   second writer refused: {e}"),
        Err(e) => return Err(format!("second open failed oddly: {e}")),
        Ok(_) => return Err("a second writer opened the same chat".into()),
    }
    for i in 0..3 {
        store.append_message(&Message::new(i, Kind::User, format!("message {i}\nline two"), "2026-10-05T12:00:00.000-07:00".into()))?;
    }
    store.append_node(&Node::new(0, 0, "user: message 0 line two".into()))?;
    drop(store);
    // Failure 7: a crash mid-write leaves a torn last line.
    let day = std::fs::read_dir(home.join("chat/main"))
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .next()
        .ok_or("no day file")?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&day)
        .and_then(|mut f| f.write_all(br#"{"i":3,"kind":"user","te"#))
        .map_err(|e| e.to_string())?;
    let (store, mem, reports) = Store::open(&home)?;
    println!("ok   reopened over the torn line: {reports:?}");
    if mem.len() != 3 || !reports.iter().any(|r| r.contains("torn line")) {
        return Err(format!("torn line not skipped: {} messages, {reports:?}", mem.len()));
    }
    if !reports.iter().any(|r| r.contains("newline")) {
        return Err("the torn file did not get its final newline".into());
    }
    store.append_message(&Message::new(3, Kind::Talk, "after the crash".into(), "2026-10-05T12:00:01.000-07:00".into()))?;
    drop(store);
    let (_store, mem, reports) = Store::open(&home)?;
    if mem.len() != 4 || mem.root[3].text != "after the crash" {
        return Err(format!("the append after a torn line was lost: {} messages, {reports:?}", mem.len()));
    }
    if mem.render_view() != "<chat>\n0+1|user: message 0 line two\n1+1|(not summarized yet: zoom it)\n2+1|(not summarized yet: zoom it)\n3+1|(not summarized yet: zoom it)\n</chat>" {
        return Err(format!("unexpected view after reload:\n{}", mem.render_view()));
    }
    println!("ok   append after the torn line landed on its own line; reload folds 4 messages");
    let _ = std::fs::remove_dir_all(&home);
    println!("PASS store-check (failures 7, 8)");
    Ok(())
}

mod live {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use futures::StreamExt;
    use serde_json::{Value, json};
    use tokio::sync::{mpsc, oneshot};
    use zeron_harness::{CancellationToken, Harness, RunControls, SteerMessage};
    use zeron_optchat::memory::{Kind, Message, NODE, Node, cut_bytes};
    use zeron_optchat::{Binding, FileCredentials, OptChatHarness, debug};
    use zeron_proto::{AgentEvent, DoneStatus, ReasoningLevel, RunRequest, SandboxLevel, ToolCall};

    const SONNET: &str = "claude-sonnet-5-5";
    const LUNA: &str = "gpt-6-luna";
    const SEED_NOTES: u64 = 2_400;
    const TURN_TIMEOUT: Duration = Duration::from_secs(420);

    #[derive(Clone)]
    enum Action {
        None,
        SteerOnTool(String),
        InterruptOnTool,
        InterruptAfter(Duration),
    }

    #[derive(Default, serde::Serialize)]
    struct TurnLog {
        name: String,
        model: String,
        prompt_bytes: usize,
        text: String,
        reasoning_chars: usize,
        #[serde(skip)]
        reasoning: String,
        tools: Vec<Value>,
        steered: usize,
        status: String,
        error: Option<String>,
        ms: u128,
        /// Interrupt to Done, when interrupted.
        stop_ms: Option<u128>,
        unsummarized_at_start: u64,
    }

    #[derive(serde::Serialize)]
    struct Check {
        failure: String,
        pass: bool,
        detail: String,
    }

    fn check(checks: &mut Vec<Check>, failure: &str, pass: bool, detail: String) {
        println!("{} {failure}: {detail}", if pass { "PASS" } else { "FAIL" });
        checks.push(Check {
            failure: failure.to_string(),
            pass,
            detail,
        });
    }

    fn request(prompt: &str, model: &str, cwd: &Path) -> RunRequest {
        RunRequest {
            prompt: prompt.to_string(),
            harness: Some(zeron_proto::HarnessId::OptChat),
            model: Some(model.to_string()),
            reasoning: Some(ReasoningLevel::High),
            model_options: Default::default(),
            cwd: cwd.display().to_string(),
            sandbox: SandboxLevel::DangerFullAccess,
            auto_approve: true,
            resume: None,
            attachments: Vec::new(),
            worktree: None,
            mcp: None,
        }
    }

    /// One run, driven exactly as the engine drives it.
    async fn turn(name: &str, model: &str, prompt: &str, cwd: &Path, action: Action) -> Result<TurnLog, String> {
        let harness = OptChatHarness::new();
        let (steer_tx, steer_rx) = mpsc::channel::<SteerMessage>(16);
        let interrupt = CancellationToken::new();
        let controls = RunControls {
            execution_lease: None,
            request_input: Box::new(|_| {
                let (tx, rx) = oneshot::channel();
                drop(tx);
                rx
            }),
            steering: steer_rx,
            interrupt: interrupt.clone(),
        };
        let mut log = TurnLog {
            name: name.to_string(),
            model: model.to_string(),
            prompt_bytes: prompt.len(),
            unsummarized_at_start: debug::unsummarized(),
            ..Default::default()
        };
        println!("--- {name} ({model})");
        let started = Instant::now();
        let mut stream = harness
            .run(request(prompt, model, cwd), controls)
            .await
            .map_err(|e| e.to_string())?;
        let mut interrupted_at: Option<Instant> = None;
        let mut acted = false;
        if let Action::InterruptAfter(after) = action {
            let interrupt = interrupt.clone();
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                interrupt.cancel();
            });
            interrupted_at = Some(Instant::now() + after);
        }
        loop {
            let next = tokio::time::timeout(TURN_TIMEOUT, stream.next())
                .await
                .map_err(|_| format!("{name}: no event for {TURN_TIMEOUT:?} (run hangs)"))?;
            let Some(event) = next else {
                return Err(format!("{name}: stream ended without Done"));
            };
            let event = event.map_err(|e| e.to_string())?;
            match event {
                AgentEvent::TextDelta { text } => log.text.push_str(&text),
                AgentEvent::ReasoningDelta { text } => log.reasoning.push_str(&text),
                AgentEvent::Steered { .. } => log.steered += 1,
                AgentEvent::ToolCall { id, call } => {
                    println!("    tool {}", serde_json::to_string(&call).unwrap_or_default().chars().take(160).collect::<String>());
                    log.tools.push(json!({"id": id, "call": call}));
                    if !acted && matches!(call, ToolCall::Exec { .. }) {
                        match &action {
                            Action::SteerOnTool(text) => {
                                acted = true;
                                let _ = steer_tx
                                    .send(SteerMessage {
                                        prompt: text.clone(),
                                        message_id: None,
                                    })
                                    .await;
                            }
                            Action::InterruptOnTool => {
                                acted = true;
                                tokio::time::sleep(Duration::from_millis(1500)).await;
                                interrupted_at = Some(Instant::now());
                                interrupt.cancel();
                            }
                            _ => {}
                        }
                    }
                }
                AgentEvent::Done { status, error, .. } => {
                    log.status = format!("{status:?}");
                    log.error = error;
                    if let Some(at) = interrupted_at
                        && status == DoneStatus::Interrupted
                    {
                        log.stop_ms = Some(at.elapsed().as_millis());
                    }
                    break;
                }
                _ => {}
            }
        }
        log.ms = started.elapsed().as_millis();
        log.reasoning_chars = log.reasoning.chars().count();
        println!(
            "    {} in {} ms{}: {}",
            log.status,
            log.ms,
            log.error.as_ref().map(|e| format!(" ({e})")).unwrap_or_default(),
            log.text.chars().take(300).collect::<String>().replace('\n', " ")
        );
        Ok(log)
    }

    /// An imported history (SPEC §10) with its tree prebuilt, so the view is
    /// full-size from the first turn: the cross-turn view breakpoints need a
    /// view past the 50,000-character mark.
    fn seed(home: &Path) -> Result<(), String> {
        let chat = home.join("chat");
        if chat.join("main").exists() {
            return Ok(());
        }
        std::fs::create_dir_all(chat.join("main")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(chat.join("tree")).map_err(|e| e.to_string())?;
        let services = ["billing", "search", "auth", "ingest", "reports", "mailer", "scheduler", "gateway"];
        let people = ["Ana", "Bruno", "Chen", "Dalia", "Emeka", "Farah"];
        let stores = ["Postgres 16", "Redis 7", "SQLite", "ClickHouse", "S3"];
        let date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string();
        let mut main = String::new();
        let mut texts = Vec::new();
        for k in 0..SEED_NOTES {
            let svc = services[(k % 8) as usize];
            let text = format!(
                "imported note {k}: the {svc} service keeps its state in {} on port {}; {} decided to {} it in week {} because the old setup {}.",
                stores[(k % 5) as usize],
                5000 + k % 900,
                people[(k % 6) as usize],
                ["migrate", "shard", "cache", "retire", "audit"][(k % 5) as usize],
                1 + k % 52,
                ["dropped writes under load", "cost too much", "was hard to back up", "leaked connections", "had no owner"][(k * 7 % 5) as usize],
            );
            let message = Message::new(k, Kind::Note, text, date.clone());
            texts.push(message.line());
            main.push_str(&serde_json::to_string(&message).map_err(|e| e.to_string())?);
            main.push('\n');
        }
        // Every node the pump would build, so nothing is due.
        let mut tree = String::new();
        let mut level: Vec<String> = texts;
        let mut l = 0u32;
        while !level.is_empty() {
            for (i, text) in level.iter().enumerate() {
                tree.push_str(&serde_json::to_string(&Node::new(l, i as u64, text.clone())).map_err(|e| e.to_string())?);
                tree.push('\n');
            }
            level = level
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    let joined = format!("{}\n{}", pair[0], pair[1]);
                    if joined.len() <= NODE {
                        joined
                    } else {
                        let a = pair[0].replace('\n', " ");
                        let b = pair[1].replace('\n', " ");
                        format!("{} / {}", cut_bytes(&a, 250), cut_bytes(&b, 250))
                    }
                })
                .collect();
            l += 1;
        }
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        std::fs::write(chat.join("main").join(format!("{day}.jsonl")), main).map_err(|e| e.to_string())?;
        std::fs::write(chat.join("tree").join(format!("{day}.jsonl")), tree).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn edit_entry(auth: &Path, key: &str, edit: impl FnOnce(&mut Value)) -> Result<Value, String> {
        let mut store: Value = serde_json::from_slice(&std::fs::read(auth).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let before = store[key].clone();
        edit(&mut store[key]);
        std::fs::write(auth, serde_json::to_vec_pretty(&store).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        Ok(before)
    }

    fn entry(auth: &Path, key: &str) -> Value {
        std::fs::read(auth)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| v[key].clone())
            .unwrap_or(Value::Null)
    }

    fn snippet(text: &str, from: usize, len: usize) -> String {
        text.chars().skip(from).take(len).collect()
    }

    pub async fn live(args: &[String]) -> Result<(), String> {
        let home: PathBuf = super::home_arg(args)?;
        let auth = home.join("auth.json");
        if entry(&auth, "anthropic").is_null() {
            return Err(format!(
                "{} has no anthropic login; run: optchat-e2e login anthropic --home {}",
                auth.display(),
                home.display()
            ));
        }
        let has_openai = !entry(&auth, "openai-codex").is_null();
        let workspace = home.join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
        seed(&home)?;
        zeron_optchat::bind(Binding {
            home: home.clone(),
            credentials: std::sync::Arc::new(FileCredentials::new(auth.clone())),
        });
        let started = Instant::now();
        let mut turns: Vec<TurnLog> = Vec::new();
        let mut checks: Vec<Check> = Vec::new();
        debug::load()?;
        let first_message = debug::messages().len() as u64;
        let view0 = debug::live_view().unwrap_or_default();
        println!("view at start: {} chars, {} lines", view0.chars().count(), view0.lines().count());

        // Failure 8 in the live process: the chat holds its lock.
        let second = zeron_optchat::store::Store::open(&home);
        check(&mut checks, "8 single writer", second.is_err(), format!("second Store::open while live: {:?}", second.as_ref().err()));
        drop(second);

        // T0: a problem that makes the model think, so failure 12 has thoughts to look for.
        turns.push(
            turn(
                "t0-think",
                SONNET,
                "How many integers from 1 to 500 are divisible by 3 or 5 but not by 7? Work it out carefully, then reply with the number only.",
                &workspace,
                Action::None,
            )
            .await?,
        );

        // T1: a long message whose exact words only a zoom to n = 1 can recover.
        let items: Vec<String> = (1..=40)
            .map(|k| {
                let a = ["amber", "basalt", "cobalt", "dune", "ember", "fjord", "garnet", "harbor"][k % 8];
                let b = ["kite", "lantern", "meadow", "nickel", "orchid", "pylon", "quartz", "raven"][(k * 3) % 8];
                format!("{k}. {a}-{b}-{}", 100 + k * 37 % 900)
            })
            .collect();
        let t1_prompt = format!(
            "Remember this list exactly; I will ask about it later. Reply only with 'noted' and how many items it has.\n{}",
            items.join("\n")
        );
        let t1_id = debug::messages().len() as u64;
        turns.push(turn("t1-remember", SONNET, &t1_prompt, &workspace, Action::None).await?);

        // T2: tools (multi-step call) after forcing a token refresh (failure 15).
        let refreshed_before = edit_entry(&auth, "anthropic", |e| e["expires"] = json!(0))?;
        turns.push(
            turn(
                "t2-tools",
                SONNET,
                "Using bash, create a file marker.txt in the working directory containing the line 'optchat-e2e', then list the directory and tell me what is there.",
                &workspace,
                Action::None,
            )
            .await?,
        );
        let refreshed_after = entry(&auth, "anthropic");
        let refreshed = refreshed_after["expires"].as_i64().unwrap_or(0) > chrono::Utc::now().timestamp_millis()
            && refreshed_after["access"] != refreshed_before["access"];
        check(&mut checks, "15 refresh on expiry", refreshed, "expired entry was refreshed once and written back".into());

        // T3: steered mid-run (failure 13).
        let steer = "Also: end your reply with the word PINEAPPLE in capitals.";
        let t3 = turn(
            "t3-steered",
            SONNET,
            "Use bash to run `sleep 6; echo first-done` and tell me what it printed.",
            &workspace,
            Action::SteerOnTool(steer.into()),
        )
        .await?;
        let log = debug::messages();
        let steer_logged = log.iter().position(|m| m.kind == Kind::User && m.text == steer);
        let talk_after = steer_logged.is_some_and(|at| log[at..].iter().any(|m| m.kind == Kind::Talk && m.text.contains("PINEAPPLE")));
        check(
            &mut checks,
            "13 steering delivered and logged",
            t3.steered == 1 && steer_logged.is_some() && talk_after && t3.status == "Completed",
            format!("Steered events {}, logged as user at {:?}, reply honors it: {talk_after}", t3.steered, steer_logged),
        );
        turns.push(t3);

        // T4: interrupted during a tool (failure 17).
        let t4 = turn(
            "t4-interrupt-tool",
            SONNET,
            "Use bash to run `sleep 120`, then tell me it finished.",
            &workspace,
            Action::InterruptOnTool,
        )
        .await?;
        let echo_note = debug::messages().iter().any(|m| m.kind == Kind::Echo && m.text == "(interrupted by the user)");
        check(
            &mut checks,
            "17 interrupt during a tool",
            t4.status == "Interrupted" && t4.stop_ms.is_some_and(|ms| ms < 5_000) && echo_note,
            format!("status {}, Done {:?} ms after the interrupt, interruption logged: {echo_note}", t4.status, t4.stop_ms),
        );
        turns.push(t4);

        // T5: interrupted while settling (failure 17). A long prompt that a
        // stopped run leaves unsummarized makes the next run wait in settle.
        let filler = (0..30).map(|k| format!("Point {k}: the scheduler retries failed jobs with jitter.")).collect::<Vec<_>>().join(" ");
        let t5a = turn(
            "t5a-leave-unsummarized",
            SONNET,
            &format!("Ignore this for now, I will come back to it. {filler}"),
            &workspace,
            Action::InterruptAfter(Duration::from_millis(400)),
        )
        .await?;
        turns.push(t5a);
        let pending = debug::unsummarized();
        let settle_prompt = "This message was sent while the memory was settling and then cancelled.";
        let t5 = turn("t5-interrupt-settle", SONNET, settle_prompt, &workspace, Action::InterruptAfter(Duration::from_millis(300))).await?;
        let kept = debug::messages().iter().any(|m| m.kind == Kind::User && m.text == settle_prompt);
        check(
            &mut checks,
            "17 interrupt during settle",
            t5.status == "Interrupted" && t5.stop_ms.is_some_and(|ms| ms < 2_000) && kept && t5.text.is_empty() && pending > 0,
            format!("{pending} unsummarized at start, status {}, Done {:?} ms after the interrupt, message kept unanswered: {kept}", t5.status, t5.stop_ms),
        );
        turns.push(t5);

        // T6: exact recall that needs zoom(id, 1) (failure 2's cost side; SPEC §5.3).
        let t6 = turn(
            "t6-zoom",
            SONNET,
            "Earlier in this chat I sent you a numbered list to remember. Quote item 17 exactly as I wrote it.",
            &workspace,
            Action::None,
        )
        .await?;
        let zoomed_root = t6.tools.iter().any(|t| {
            t["call"]["name"] == "zoom" && t["call"]["input"]["n"] == json!(1) && t["call"]["input"]["id"] == json!(t1_id)
        });
        let item17 = items[16].split_once(". ").map(|x| x.1).unwrap_or_default().to_string();
        check(
            &mut checks,
            "zoom to n=1 answers exact text",
            zoomed_root && t6.text.contains(&item17),
            format!("zoom({t1_id}, 1) called: {zoomed_root}; reply contains '{item17}': {}", t6.text.contains(&item17)),
        );
        turns.push(t6);

        // T7: one more Claude turn after a forced 401 (failure 15, retry path).
        let bad = edit_entry(&auth, "anthropic", |e| {
            e["access"] = json!("sk-ant-oat01-invalid-e2e");
            e["expires"] = json!(chrono::Utc::now().timestamp_millis() + 3_600_000);
        })?;
        let t7 = turn(
            "t7-after-401",
            SONNET,
            "What does marker.txt contain? Check it with a tool, then answer in one line.",
            &workspace,
            Action::None,
        )
        .await?;
        let after = entry(&auth, "anthropic");
        check(
            &mut checks,
            "15 401 retries once after refresh",
            t7.status == "Completed" && after["access"] != json!("sk-ant-oat01-invalid-e2e") && after["refresh"] != bad["refresh"],
            format!("turn {} with a rejected token; entry rewritten: {}", t7.status, after["access"] != json!("sk-ant-oat01-invalid-e2e")),
        );
        turns.push(t7);

        // Two runs at once (two Zeron chats on OptChat): one waits for the
        // other, so the log holds each turn whole, never interleaved.
        let before = debug::messages().len();
        let (ta, tb) = tokio::join!(
            turn("t7a-concurrent", SONNET, "Reply with exactly the word ALPHA and nothing else.", &workspace, Action::None),
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                turn("t7b-concurrent", SONNET, "Reply with exactly the word BRAVO and nothing else.", &workspace, Action::None).await
            },
        );
        let (ta, tb) = (ta?, tb?);
        let segment: Vec<(Kind, String)> = debug::messages()[before..].iter().map(|m| (m.kind, m.text.clone())).collect();
        let users: Vec<usize> = segment.iter().enumerate().filter(|(_, m)| m.0 == Kind::User).map(|(k, _)| k).collect();
        let whole = users.len() == 2 && segment[users[0]..users[1]].iter().any(|m| m.0 == Kind::Talk && m.1.contains("ALPHA"));
        check(
            &mut checks,
            "18 concurrent runs never interleave",
            whole && ta.status == "Completed" && tb.status == "Completed" && tb.text.contains("BRAVO") && tb.reasoning.contains("Another OptChat turn"),
            format!("log after both: {:?}; second waited: {}", segment.iter().map(|m| m.0.as_str()).collect::<Vec<_>>(), tb.reasoning.contains("Another OptChat turn")),
        );
        turns.push(ta);
        turns.push(tb);

        if has_openai {
            turns.push(
                turn(
                    "t8-luna-tools",
                    LUNA,
                    "Use bash to print today's date with `date`, then tell me in one line what we have done in this chat today.",
                    &workspace,
                    Action::None,
                )
                .await?,
            );
            turns.push(
                turn(
                    "t9-luna-recall",
                    LUNA,
                    "Which word did I ask you to end a reply with earlier today? Check the chat if you need to, and run `cat marker.txt` with bash too.",
                    &workspace,
                    Action::None,
                )
                .await?,
            );
        }

        // Let the compactor finish, then drop and reload (failure 10).
        let idle = debug::wait_idle(Duration::from_secs(300)).await;
        let live_view = debug::live_view().unwrap_or_default();
        let messages = debug::messages();
        debug::unload().await;
        let reopened = zeron_optchat::store::Store::open(&home).map(|_| ());
        debug::load()?;
        let reloaded = debug::live_view().unwrap_or_default();
        check(
            &mut checks,
            "10 reload folds the live view",
            idle && reloaded == live_view && reopened.is_ok(),
            format!(
                "compactor idle {idle}; lock released on unload: {:?}; views equal: {} ({} chars)",
                reopened.err(),
                reloaded == live_view,
                live_view.chars().count()
            ),
        );

        let evidence = debug::evidence();
        check(
            &mut checks,
            "4 compactor never sees a placeholder",
            evidence.compactor_placeholders == 0 && !evidence.compactor_failures.iter().any(|f| f.contains("not summarized")),
            format!("{} compactor inputs, {} with a placeholder", evidence.compactor_inputs, evidence.compactor_placeholders),
        );
        check(
            &mut checks,
            "5 no ids in compactor input",
            evidence.compactor_id_lines == 0 && evidence.compactor_inputs > 0,
            format!("{} id-shaped lines in {} inputs", evidence.compactor_id_lines, evidence.compactor_inputs),
        );
        check(
            &mut checks,
            "11 no turn starts unsettled",
            evidence.unsettled_turns == 0 && evidence.turns > 0,
            format!("{} of {} turns rendered an unsummarized view", evidence.unsettled_turns, evidence.turns),
        );
        let new_messages = &messages[first_message as usize..];
        let leaked: Vec<String> = turns
            .iter()
            .filter(|t| t.reasoning_chars >= 120)
            .flat_map(|t| [snippet(&t.reasoning, 20, 60), snippet(&t.reasoning, t.reasoning_chars / 2, 60)])
            .filter(|s| new_messages.iter().any(|m| m.text.contains(s.as_str())))
            .collect();
        let thought_turns = turns.iter().filter(|t| t.reasoning_chars >= 120).count();
        check(
            &mut checks,
            "12 thoughts never logged",
            leaked.is_empty() && thought_turns > 0,
            format!("{thought_turns} turns streamed thoughts; snippets found in the log: {leaked:?}"),
        );
        // Failure 14: in-call steps read the cache; later turns read the view.
        let turn_requests: Vec<&debug::Request> = evidence.requests.iter().filter(|r| r.kind == debug::RequestKind::Turn).collect();
        let step_misses: Vec<String> = turn_requests
            .iter()
            .filter(|r| r.step > 0 && r.usage.cache_read == 0)
            .map(|r| format!("call {} step {} ({})", r.call, r.step, r.model))
            .collect();
        let steps = turn_requests.iter().filter(|r| r.step > 0).count();
        check(
            &mut checks,
            "14 cache read on every step 2+",
            step_misses.is_empty() && steps > 0,
            format!("{steps} later steps; misses: {step_misses:?}"),
        );
        // A later turn can read the cache only through a view breakpoint
        // whose piece it repeats byte for byte (SPEC §8: no breakpoint sits
        // on the system prompt), so a hit is owed exactly when one survived.
        let reused: std::collections::HashMap<u64, usize> = evidence.view_pieces_reused.iter().copied().collect();
        let mut first_seen = std::collections::HashSet::new();
        let cross: Vec<(u64, String, usize, u64, u64)> = turn_requests
            .iter()
            .filter(|r| r.step == 0)
            .filter(|r| !first_seen.insert(r.model.clone()))
            .map(|r| (r.call, r.model.clone(), reused.get(&r.call).copied().unwrap_or(0), r.usage.cache_read, r.usage.prompt()))
            .collect();
        // ChatGPT's Codex endpoint rejects prompt_cache_breakpoint ("not
        // supported on this model") and its implicit cache only matches a
        // whole earlier prompt, so only Claude owes a cross-turn read.
        let owed: Vec<_> = cross.iter().filter(|c| c.2 > 0 && c.1.starts_with("claude-")).collect();
        let cross_misses: Vec<_> = owed.iter().filter(|c| c.3 == 0).collect();
        check(
            &mut checks,
            "14 cache read across turns (view breakpoints)",
            cross_misses.is_empty() && !owed.is_empty(),
            format!("first steps of later turns (call, model, view pieces repeated, cache_read, prompt): {cross:?}; misses where a piece survived: {cross_misses:?}"),
        );
        // Failure 16: bad zoom arguments answer, never panic.
        let total = debug::messages().len() as u64;
        let bad_args = [(0, 0), (1, 2), (3, 3), (u64::MAX, 1), (0, 1 << 40), (total, 1), (u64::MAX - 1, 2)];
        let answers: Vec<String> = bad_args.iter().map(|(id, n)| debug::zoom(*id, *n).unwrap_or_default()).collect();
        check(
            &mut checks,
            "16 bad zoom answers 'No line'",
            answers.iter().zip(&bad_args).all(|(a, (id, n))| *a == format!("No line {id}+{n}.")),
            format!("{answers:?}"),
        );
        let over: usize = evidence.tries.iter().filter(|(_, sizes)| sizes.iter().all(|s| *s > NODE)).count();
        let shortest_kept = evidence.tries.len();
        println!("info compactor calls {shortest_kept}, nodes still over {NODE} B after all tries: {over}");

        let report = json!({
            "home": home,
            "elapsed_ms": started.elapsed().as_millis(),
            "models": {"turns": turns.iter().map(|t| t.model.clone()).collect::<std::collections::BTreeSet<_>>(), "compactor": if evidence.requests.iter().any(|r| r.kind == debug::RequestKind::Compact && r.model == SONNET) { SONNET } else { LUNA }},
            "assertions": checks,
            "all_pass": checks.iter().all(|c| c.pass),
            "turns": turns,
            "requests": evidence.requests,
            "compactor": {
                "inputs": evidence.compactor_inputs,
                "failures": evidence.compactor_failures,
                "tries": evidence.tries,
                "over_node_after_tries": over,
            },
            "view": {"start_chars": view0.chars().count(), "end_chars": live_view.chars().count(), "end_lines": live_view.lines().count()},
        });
        let path = home.join("e2e-report.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        println!("report: {}", path.display());
        if checks.iter().all(|c| c.pass) {
            println!("PASS live");
            Ok(())
        } else {
            Err(format!("{} live assertions failed", checks.iter().filter(|c| !c.pass).count()))
        }
    }
}
