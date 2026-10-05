//! `/memory`: the whole memory as one HTML page (SPEC §10 "Browsing"): the
//! current view, ROOT (every message), and each level of the tree, each
//! entry with its range, time span and size.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::chat::Chat;
use crate::memory::{Memory, PLACEHOLDER};

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn span(mem: &Memory, start: u64, end: u64) -> String {
    let date = |i: u64| {
        mem.root
            .get(i as usize)
            .map(|m| m.date.get(..16).unwrap_or(&m.date).replace('T', " "))
            .unwrap_or_default()
    };
    let (a, b) = (date(start), date(end.saturating_sub(1)));
    if a == b { a } else { format!("{a} — {b}") }
}

fn entry(out: &mut String, range: &str, span: &str, size: usize, text: &str) {
    let _ = write!(
        out,
        "<div class=e><div class=h><b>{}</b> <span>{}</span> <span>{} B</span></div><pre>{}</pre></div>",
        esc(range),
        esc(span),
        size,
        esc(text)
    );
}

pub fn render(mem: &Memory) -> String {
    let mut out = String::from(
        "<!doctype html><meta charset=utf-8><title>OptChat memory</title><style>\
         body{font:14px system-ui;margin:2em auto;max-width:60em}\
         pre{white-space:pre-wrap;margin:.2em 0 0;font:13px ui-monospace,monospace}\
         .e{border-top:1px solid #ddd;padding:.4em 0}.h span{color:#777;margin-left:1em}\
         summary{font-size:1.2em;font-weight:600;margin:1em 0 .4em;cursor:pointer}</style>",
    );
    let _ = write!(
        out,
        "<h1>OptChat memory</h1><p>{} messages, {} summaries, view {} lines / {} B.</p>",
        mem.len(),
        mem.tree.len(),
        mem.view.len(),
        mem.view_size()
    );
    out.push_str("<details open><summary>View</summary>");
    for part in &mem.view {
        let text = mem
            .tree
            .get(&(part.l, part.i))
            .map_or(PLACEHOLDER, |n| n.text.as_str());
        entry(
            &mut out,
            &format!("{}+{}", part.start(), part.n()),
            &span(mem, part.start(), part.end()),
            text.len(),
            text,
        );
    }
    out.push_str("</details><details><summary>ROOT: every message</summary>");
    for message in &mem.root {
        entry(
            &mut out,
            &format!("{}+0 {}", message.i, message.kind.as_str()),
            &span(mem, message.i, message.i + 1),
            message.size as usize,
            &message.text,
        );
    }
    out.push_str("</details>");
    let top = mem.tree.keys().map(|(l, _)| *l).max().unwrap_or(0);
    for level in (0..=top).rev() {
        let mut nodes: Vec<_> = mem.tree.values().filter(|n| n.l == level).collect();
        if nodes.is_empty() {
            continue;
        }
        nodes.sort_by_key(|n| n.i);
        let _ = write!(
            out,
            "<details><summary>Level {level}: {} lines of {} messages</summary>",
            nodes.len(),
            1u64 << level
        );
        for node in nodes {
            let (start, n) = (node.i << level, 1u64 << level);
            entry(
                &mut out,
                &format!("{start}+{n}"),
                &span(mem, start, start + n),
                node.text.len(),
                &node.text,
            );
        }
        out.push_str("</details>");
    }
    out
}

pub fn write(chat: &Chat) -> Result<PathBuf, String> {
    let html = render(&chat.state().mem);
    let path = chat.home.join("memory.html");
    std::fs::write(&path, html).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}
