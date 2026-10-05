//! The log, the tree and the view (SPEC §1-§6). Pure: no IO, no async, so
//! the fold can be replayed with no network (`optchat-e2e fold-sim`).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Target size of one summary line, in bytes.
pub const NODE: usize = 512;
/// Budget of the view, in bytes of part text.
pub const VIEW: usize = 128_000;
/// What an unbuilt part shows (display and fail-safe only, SPEC §6).
pub const PLACEHOLDER: &str = "(not summarized yet: zoom it)";
/// Cache breakpoints inside the rendered view, in characters (SPEC §8).
pub const MARKS: [usize; 3] = [50_000, 80_000, 100_000];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    User,
    Talk,
    Tool,
    Echo,
    Note,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Talk => "talk",
            Kind::Tool => "tool",
            Kind::Echo => "echo",
            Kind::Note => "note",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub i: u64,
    pub kind: Kind,
    pub text: String,
    pub size: u64,
    pub date: String,
}

impl Message {
    pub fn new(i: u64, kind: Kind, text: String, date: String) -> Self {
        let size = (kind.as_str().len() + 2 + text.len()) as u64;
        Self {
            i,
            kind,
            text,
            size,
            date,
        }
    }

    /// `kind: text`, the form every size and every level-0 input is measured in.
    pub fn line(&self) -> String {
        format!("{}: {}", self.kind.as_str(), self.text)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub l: u32,
    pub i: u64,
    pub text: String,
    pub size: u64,
}

impl Node {
    pub fn new(l: u32, i: u64, text: String) -> Self {
        let size = text.len() as u64;
        Self { l, i, text, size }
    }
}

/// One view entry: tree node `(l, i)`, covering messages `[i·2^l, (i+1)·2^l)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Part {
    pub l: u32,
    pub i: u64,
}

impl Part {
    pub fn start(self) -> u64 {
        self.i << self.l
    }
    pub fn n(self) -> u64 {
        1u64 << self.l
    }
    pub fn end(self) -> u64 {
        self.start() + self.n()
    }
}

#[derive(Default, Clone)]
pub struct Memory {
    pub root: Vec<Message>,
    pub tree: HashMap<(u32, u64), Node>,
    pub view: Vec<Part>,
    /// Per level, the lowest unbuilt node index: keeps the pump's scan
    /// proportional to the frontier instead of the whole tree.
    low: Vec<u64>,
    /// `view_size()`, kept current by append, insert and fit.
    size: usize,
    /// Nodes whose text holds a line break: the rest render with one copy.
    multiline: HashSet<(u32, u64)>,
}

impl Memory {
    pub fn len(&self) -> u64 {
        self.root.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    pub fn built(&self, l: u32, i: u64) -> bool {
        self.tree.contains_key(&(l, i))
    }

    /// Append message `root.len()` and its level-0 part. The caller fits.
    pub fn append(&mut self, message: Message) -> Result<(), String> {
        if message.i != self.len() {
            return Err(format!(
                "message {} appended at position {}",
                message.i,
                self.len()
            ));
        }
        self.view.push(Part { l: 0, i: message.i });
        self.size += self.part_size(Part { l: 0, i: message.i });
        self.root.push(message);
        Ok(())
    }

    /// Record a built node. The caller fits.
    pub fn insert(&mut self, node: Node) {
        let (l, i) = (node.l, node.i);
        let size = node.text.len();
        let multiline = node.text.contains(['\n', '\r']);
        if self.tree.insert((l, i), node).is_some() {
            return;
        }
        if multiline {
            self.multiline.insert((l, i));
        }
        // Only level-0 parts can be unbuilt in the view (a parent enters
        // only once built), so only they swap a placeholder for text.
        if l == 0 && i < self.len() {
            self.size = self.size + size - PLACEHOLDER.len();
        }
        self.advance_low(l);
    }

    fn advance_low(&mut self, l: u32) {
        let level = l as usize;
        if self.low.len() <= level {
            self.low.resize(level + 1, 0);
        }
        while self.tree.contains_key(&(l, self.low[level])) {
            self.low[level] += 1;
        }
    }

    fn part_size(&self, part: Part) -> usize {
        match self.tree.get(&(part.l, part.i)) {
            Some(node) => node.text.len(),
            None => PLACEHOLDER.len(),
        }
    }

    /// Bytes of part text in the view (an unbuilt part counts its placeholder).
    pub fn view_size(&self) -> usize {
        self.size
    }

    /// SPEC §5.2: while over budget, merge the most due adjacent sibling pair
    /// whose parent is built. Never splits. Returns the number of merges.
    pub fn fit(&mut self, budget: usize) -> usize {
        let total = self.len();
        let mut merges = 0;
        while self.size > budget {
            // best = (index of a, age, level) maximizing age / 2^(l+2).
            let mut best: Option<(usize, u64, u32)> = None;
            for k in 0..self.view.len().saturating_sub(1) {
                let (a, b) = (self.view[k], self.view[k + 1]);
                if a.l != b.l || a.i % 2 != 0 || b.i != a.i + 1 || !self.built(a.l + 1, a.i / 2) {
                    continue;
                }
                let age = total - a.start();
                let more_due = match best {
                    None => true,
                    // age/2^(l+2) > best_age/2^(best_l+2), cross-multiplied.
                    Some((_, best_age, best_l)) => {
                        (age as u128) << best_l > (best_age as u128) << a.l
                    }
                };
                if more_due {
                    best = Some((k, age, a.l));
                }
            }
            let Some((k, _, _)) = best else { break };
            let (a, b) = (self.view[k], self.view[k + 1]);
            let parent = Part {
                l: a.l + 1,
                i: a.i / 2,
            };
            self.size = self.size + self.part_size(parent) - self.part_size(a) - self.part_size(b);
            self.view.splice(k..k + 2, [parent]);
            merges += 1;
        }
        merges
    }

    /// First message whose view line is unbuilt; `len()` when all are built.
    pub fn first_unbuilt(&self) -> u64 {
        // Unbuilt view parts are exactly the unbuilt level-0 nodes (no
        // ancestor of an unbuilt node can be built), so the first one is
        // the lowest unbuilt level-0 index.
        self.low.first().copied().unwrap_or(0).min(self.len())
    }

    /// Sources present: the message (level 0) or both children.
    pub fn ready(&self, l: u32, i: u64) -> bool {
        if l == 0 {
            i < self.len()
        } else {
            self.built(l - 1, 2 * i) && self.built(l - 1, 2 * i + 1)
        }
    }

    /// The pump's scan (SPEC §4.1): nodes that are unbuilt, not busy, ready,
    /// and whose whole context is summarized, in level order, at most `max`.
    pub fn due_nodes(&self, busy: &HashSet<(u32, u64)>, max: usize) -> Vec<(u32, u64)> {
        let total = self.len();
        let first = self.first_unbuilt();
        let mut out = Vec::new();
        let mut l = 0u32;
        while l < 63 && (1u64 << l) <= total {
            let mut i = self.low.get(l as usize).copied().unwrap_or(0);
            while (i + 1) << l <= total {
                if out.len() + busy.len() >= max {
                    return out;
                }
                let end = if l == 0 { i } else { (i + 1) << l };
                if end > first {
                    break;
                }
                if !self.built(l, i) && !busy.contains(&(l, i)) && self.ready(l, i) {
                    out.push((l, i));
                }
                i += 1;
            }
            l += 1;
        }
        out
    }

    fn part_text(&self, part: Part) -> &str {
        self.tree
            .get(&(part.l, part.i))
            .map_or(PLACEHOLDER, |node| node.text.as_str())
    }

    fn push_flat(out: &mut String, text: &str) {
        let mut pieces = text.split('\n');
        if let Some(first) = pieces.next() {
            out.push_str(first);
        }
        for piece in pieces {
            out.push(' ');
            out.push_str(piece);
        }
        if text.contains('\r') {
            // Rare: a carriage return becomes a space too.
            let start = out.len() - text.len();
            let flat = out[start..].replace('\r', " ");
            out.truncate(start);
            out.push_str(&flat);
        }
    }

    fn push_line(&self, out: &mut String, part: Part) {
        out.push_str(&part.start().to_string());
        out.push('+');
        out.push_str(&part.n().to_string());
        out.push('|');
        if self.multiline.contains(&(part.l, part.i)) {
            Self::push_flat(out, self.part_text(part));
        } else {
            out.push_str(self.part_text(part));
        }
        out.push('\n');
    }

    /// The view as every call sees it: `<chat>`, one `id+n|text` line per
    /// part, `</chat>`.
    pub fn render_view(&self) -> String {
        let mut out = String::with_capacity(self.view_size() + 16 * self.view.len() + 16);
        out.push_str("<chat>\n");
        for part in &self.view {
            self.push_line(&mut out, *part);
        }
        out.push_str("</chat>");
        out
    }

    /// The compactor's context block: the view's lines ending at or before
    /// message `upto`, bare (no ids, SPEC §4.2). `Err` if one is unbuilt,
    /// which the pump's rule 3 must make impossible.
    pub fn render_context(&self, upto: u64) -> Result<String, String> {
        let mut out = String::from("<chat>\n");
        for part in self.view.iter().take_while(|p| p.end() <= upto) {
            let Some(node) = self.tree.get(&(part.l, part.i)) else {
                return Err(format!(
                    "context line {}+{} is not summarized",
                    part.start(),
                    part.n()
                ));
            };
            if self.multiline.contains(&(part.l, part.i)) {
                Self::push_flat(&mut out, &node.text);
            } else {
                out.push_str(&node.text);
            }
            out.push('\n');
        }
        out.push_str("</chat>");
        Ok(out)
    }

    /// SPEC §7.1. Never panics: bad arguments answer `No line id+n.`
    pub fn zoom(&self, id: u64, n: u64) -> String {
        let no = || format!("No line {id}+{n}.");
        if n == 0 || !n.is_power_of_two() || !id.is_multiple_of(n) {
            return no();
        }
        match id.checked_add(n) {
            Some(end) if end <= self.len() => {}
            _ => return no(),
        }
        if n == 1 {
            let m = &self.root[id as usize];
            return format!("{id}+0|{}: {}", m.kind.as_str(), m.text);
        }
        let l = n.trailing_zeros() - 1;
        let first = 2 * id / n;
        let mut out = String::new();
        for i in [first, first + 1] {
            self.push_line(&mut out, Part { l, i });
        }
        out.pop();
        out
    }
}

/// Cut the rendered view at the last line end before each mark (chars),
/// skipping marks past its end. Returns the pieces; their concatenation is
/// the view. Each piece but the last gets a cache breakpoint.
pub fn cut_view(view: &str) -> Vec<&str> {
    let mut cuts = Vec::new();
    let mut last_newline: Option<usize> = None;
    let mut marks = MARKS.iter().peekable();
    for (chars, (byte, ch)) in view.char_indices().enumerate() {
        while let Some(&&mark) = marks.peek() {
            if chars < mark {
                break;
            }
            if let Some(at) = last_newline
                && cuts.last() != Some(&at)
            {
                cuts.push(at);
            }
            marks.next();
        }
        if marks.peek().is_none() {
            break;
        }
        if ch == '\n' {
            last_newline = Some(byte + 1);
        }
    }
    let mut pieces = Vec::new();
    let mut from = 0;
    for cut in cuts {
        if cut > from && cut < view.len() {
            pieces.push(&view[from..cut]);
            from = cut;
        }
    }
    pieces.push(&view[from..]);
    pieces
}

/// Cut `text` to at most `max` bytes without splitting a UTF-8 character.
pub fn cut_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
