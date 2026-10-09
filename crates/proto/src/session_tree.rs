//! The conversation tree of a harness session that can branch (Pi's `/tree`).

use serde::{Deserialize, Serialize};

/// Reply of `GetSessionTree`. A wrapper, because a bare `null` reply is
/// indistinguishable from no reply on the wire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionTreeReply {
    /// `None` when the chat's harness has no session tree, or it has not run.
    pub tree: Option<SessionTree>,
}

/// What the picker shows: one row per entry a user can continue from, in
/// display order (depth-first, the branch holding the leaf listed first).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTree {
    /// The row the next message continues from. `None` for a session that
    /// has no visible entry yet.
    pub leaf_id: Option<String>,
    pub entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeEntry {
    pub id: String,
    /// Indentation. Grows only where a path forks, so a straight conversation
    /// stays flat.
    pub depth: u16,
    pub kind: TreeEntryKind,
    /// A user message carries its full text (it goes back to the composer).
    /// Every other kind carries a short preview.
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// This row is the leaf or one of its ancestors.
    pub on_path: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeEntryKind {
    /// Jumping here rewinds to just before the message and hands its text
    /// back for editing. Every other kind is a position to continue from.
    User,
    Assistant,
    /// A summary of a branch the user left.
    Summary,
    Compaction,
}
