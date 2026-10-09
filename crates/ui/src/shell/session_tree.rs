//! `/tree`: browse the conversation's branching history and jump to a point in it.
use super::*;
use zeron_proto::TreeEntry;

pub(super) struct TreePalette {
    pub(super) search: Entity<ComposerInput>,
    pub(super) notice: Option<SharedString>,
}

impl Shell {
    pub(super) fn tree_entries(&self, _cx: &App) -> Vec<TreeEntry> {
        Vec::new()
    }

    pub(super) fn tree_cursor(&self, _cx: &App) -> Option<TreeEntry> {
        None
    }
}
