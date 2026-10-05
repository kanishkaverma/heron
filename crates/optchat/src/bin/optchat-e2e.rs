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
