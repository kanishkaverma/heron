//! Pi's session file as the tree `/tree` shows. Read only: Zeron never writes
//! Pi's conversation files, and moving around in the tree is the extension's job.
use crate::{HarnessError, process::Command, scratch::ScratchDir};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use zeron_proto::{SessionTree, TreeEntry, TreeEntryKind};

const USER_TEXT_CAP: usize = 16 * 1024;
const PREVIEW_CAP: usize = 240;

/// Prefix of the commands Zeron's own extensions register. They are how Zeron
/// drives Pi, not something to offer in the slash menu.
pub(super) const INTERNAL_COMMAND_PREFIX: &str = "zeron-";

/// Load the extension that moves the leaf (`/zeron-tree-jump <entryId>
/// [summarize]`). Pi's RPC has no `navigate_tree`; only an extension command
/// can call `ctx.navigateTree`.
pub(super) fn install(cmd: &mut Command, scratch: &ScratchDir) -> Result<(), HarnessError> {
    let extension = scratch.path().join("zeron-tree.mjs");
    std::fs::write(&extension, include_str!("tree.mjs"))?;
    cmd.arg("--extension").arg(extension);
    Ok(())
}

struct Raw {
    id: String,
    parent: Option<String>,
    shown: Option<(TreeEntryKind, String)>,
}

pub(super) fn parse(file: &str) -> SessionTree {
    let mut raw: Vec<Raw> = vec![];
    let mut labels: HashMap<String, Option<String>> = HashMap::new();
    for line in file.lines() {
        // A line half-written by a live Pi is skipped, not fatal.
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = entry["id"].as_str() else {
            continue;
        };
        if entry["type"] == "label"
            && let Some(target) = entry["targetId"].as_str()
        {
            labels.insert(
                target.to_owned(),
                entry["label"].as_str().map(str::to_owned),
            );
        }
        raw.push(Raw {
            id: id.to_owned(),
            parent: entry["parentId"].as_str().map(str::to_owned),
            shown: shown(&entry),
        });
    }

    let index: HashMap<&str, usize> = raw
        .iter()
        .enumerate()
        .map(|(ix, r)| (r.id.as_str(), ix))
        .collect();
    // Hidden entries (tool results, labels, model changes, Zeron's own jump
    // marker) fold away: their children hang from the nearest visible ancestor.
    let mut shown_parent: Vec<Option<usize>> = vec![None; raw.len()];
    for ix in 0..raw.len() {
        let parent = raw[ix]
            .parent
            .as_deref()
            .and_then(|p| index.get(p))
            .copied()
            .filter(|&p| p < ix);
        shown_parent[ix] = parent.and_then(|p| {
            if raw[p].shown.is_some() {
                Some(p)
            } else {
                shown_parent[p]
            }
        });
    }

    // Pi resumes at the last entry of the file.
    let mut path: HashSet<usize> = HashSet::new();
    let mut cursor = (!raw.is_empty()).then(|| raw.len() - 1);
    let mut leaf = None;
    while let Some(ix) = cursor {
        path.insert(ix);
        if leaf.is_none() && raw[ix].shown.is_some() {
            leaf = Some(ix);
        }
        cursor = raw[ix]
            .parent
            .as_deref()
            .and_then(|p| index.get(p))
            .copied()
            .filter(|&p| p < ix && !path.contains(&p));
    }

    let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    for ix in (0..raw.len()).filter(|&ix| raw[ix].shown.is_some()) {
        children.entry(shown_parent[ix]).or_default().push(ix);
    }
    // The branch holding the leaf leads; the rest keep file order.
    for siblings in children.values_mut() {
        siblings.sort_by_key(|ix| !path.contains(ix));
    }

    let roots = children.remove(&None).unwrap_or_default();
    let root_depth = u16::from(roots.len() > 1);
    let mut stack: Vec<(usize, u16)> = roots.iter().rev().map(|&ix| (ix, root_depth)).collect();
    let mut entries = vec![];
    while let Some((ix, depth)) = stack.pop() {
        let (kind, text) = raw[ix]
            .shown
            .clone()
            .expect("only shown entries are queued");
        entries.push(TreeEntry {
            id: raw[ix].id.clone(),
            depth,
            kind,
            text,
            label: labels.get(&raw[ix].id).cloned().flatten(),
            on_path: path.contains(&ix),
        });
        let kids = children.get(&Some(ix)).map(Vec::as_slice).unwrap_or(&[]);
        let child_depth = depth + u16::from(kids.len() > 1);
        stack.extend(kids.iter().rev().map(|&kid| (kid, child_depth)));
    }
    SessionTree {
        leaf_id: leaf.map(|ix| raw[ix].id.clone()),
        entries,
    }
}

fn shown(entry: &Value) -> Option<(TreeEntryKind, String)> {
    match entry["type"].as_str()? {
        "message" => {
            let message = &entry["message"];
            match message["role"].as_str()? {
                "user" => Some((
                    TreeEntryKind::User,
                    cap(&joined_text(&message["content"], "\n"), USER_TEXT_CAP),
                )),
                "assistant" => {
                    let text = preview(&joined_text(&message["content"], " "));
                    let text = if !text.is_empty() {
                        text
                    } else if message["stopReason"] == "aborted" {
                        "(aborted)".into()
                    } else if let Some(error) = message["errorMessage"].as_str() {
                        preview(error)
                    } else {
                        return None;
                    };
                    Some((TreeEntryKind::Assistant, text))
                }
                _ => None,
            }
        }
        "branch_summary" => Some((
            TreeEntryKind::Summary,
            preview(entry["summary"].as_str().unwrap_or_default()),
        )),
        "compaction" => Some((
            TreeEntryKind::Compaction,
            preview(entry["summary"].as_str().unwrap_or_default()),
        )),
        _ => None,
    }
}

fn joined_text(content: &Value, separator: &str) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join(separator),
        _ => String::new(),
    }
}

fn preview(text: &str) -> String {
    cap(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        PREVIEW_CAP,
    )
}

fn cap(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}
