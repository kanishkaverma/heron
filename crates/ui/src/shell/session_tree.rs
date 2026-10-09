//! `/tree`: browse the conversation's branching history and jump to a point in it.
//!
//! The tree is read from the agent's own session. A jump is an ordinary
//! message to the agent (`/zeron-tree-jump <id>`), so it reaches whichever
//! process holds the session and leaves a line in the transcript.
use super::command_palette::{self, EnterPress};
use super::*;
use zeron_proto::{SessionTree, SessionTreeReply, TreeEntry, TreeEntryKind};

/// What the palette knows about the tree: one state, never two at once.
enum Load {
    Loading,
    Ready(SessionTree),
    Failed(SharedString),
}

pub(super) struct TreePalette {
    chat_id: String,
    pub(super) search: Entity<ComposerInput>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    focus_pending: bool,
    scroll: gpui::ScrollHandle,
    load: Load,
    /// The row under the cursor, by id so narrowing the list cannot strand it.
    cursor: Option<String>,
    pub(super) notice: Option<SharedString>,
    enter_press: EnterPress,
    task: Option<gpui::Task<()>>,
    _search_events: Subscription,
}

fn kind_label(kind: TreeEntryKind) -> &'static str {
    match kind {
        TreeEntryKind::User => "You",
        TreeEntryKind::Assistant => "Agent",
        TreeEntryKind::Summary => "Summary",
        TreeEntryKind::Compaction => "Compaction",
    }
}

fn load_failure(error: &zeron_rpc::RpcError) -> SharedString {
    use zeron_rpc::RpcError;
    match error {
        RpcError::UnknownMethod(_) => {
            "This chat's device needs a newer Zeron to browse its history.".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The chat's device is unreachable.".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => {
            "Couldn't read this conversation's history.".into()
        }
    }
}

impl Shell {
    pub(super) fn open_session_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        self.command_palette = None;
        let search = cx.new(|cx| {
            ComposerInput::with_context("Search this conversation…", "PaletteSearch", cx)
        });
        let events = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited)
                && let Some(palette) = this.tree_palette.as_mut()
            {
                palette.notice = None;
                palette.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                cx.notify();
            }
        });
        let task = self.load_session_tree(&chat_id, cx);
        self.tree_palette = Some(TreePalette {
            chat_id,
            search,
            focus: cx.focus_handle(),
            previous_focus: window.focused(cx),
            focus_pending: true,
            scroll: gpui::ScrollHandle::new(),
            load: Load::Loading,
            cursor: None,
            notice: None,
            enter_press: EnterPress::default(),
            task,
            _search_events: events,
        });
        cx.notify();
    }

    fn load_session_tree(
        &mut self,
        chat_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Task<()>> {
        let state = self.state.read(cx);
        let mut params = serde_json::json!({ "chatId": chat_id });
        if let Some(chat) = state.selected_chat_row()
            && state.local_device_id.as_deref() != Some(chat.device_id.as_str())
        {
            params["targetDeviceId"] = chat.device_id.clone().into();
        }
        let engine = state.engine().cloned();
        let chat_id = chat_id.to_owned();
        Some(cx.spawn(async move |this, cx| {
            let reply = match engine {
                Some(engine) => engine
                    .client()
                    .call(methods::GET_SESSION_TREE, params)
                    .await
                    .and_then(|value| {
                        serde_json::from_value::<SessionTreeReply>(value)
                            .map_err(|e| zeron_rpc::RpcError::Failed(e.to_string()))
                    }),
                None => Err(zeron_rpc::RpcError::Closed),
            };
            this.update(cx, |this, cx| {
                let Some(palette) = this
                    .tree_palette
                    .as_mut()
                    .filter(|palette| palette.chat_id == chat_id)
                else {
                    return;
                };
                palette.load = match reply {
                    Ok(SessionTreeReply { tree: Some(tree) }) => {
                        palette.cursor = tree
                            .leaf_id
                            .clone()
                            .or_else(|| tree.entries.last().map(|entry| entry.id.clone()));
                        Load::Ready(tree)
                    }
                    Ok(SessionTreeReply { tree: None }) => {
                        Load::Failed("Nothing to browse yet. Send a message first.".into())
                    }
                    Err(error) => Load::Failed(load_failure(&error)),
                };
                cx.notify();
            })
            .ok();
        }))
    }

    pub(super) fn close_session_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = self.tree_palette.take() {
            if let Some(focus) = palette.previous_focus {
                window.focus(&focus, cx);
            }
            cx.notify();
        }
    }

    /// The rows the search leaves, in tree order. Every word must appear in
    /// the row's text or label.
    pub(super) fn tree_entries(&self, cx: &App) -> Vec<TreeEntry> {
        let Some(palette) = &self.tree_palette else {
            return Vec::new();
        };
        let Load::Ready(tree) = &palette.load else {
            return Vec::new();
        };
        let query = palette.search.read(cx).text().to_lowercase();
        tree.entries
            .iter()
            .filter(|entry| {
                let haystack = format!(
                    "{} {}",
                    entry.text.to_lowercase(),
                    entry.label.as_deref().unwrap_or("").to_lowercase()
                );
                query.split_whitespace().all(|word| haystack.contains(word))
            })
            .cloned()
            .collect()
    }

    pub(super) fn tree_cursor(&self, cx: &App) -> Option<TreeEntry> {
        let palette = self.tree_palette.as_ref()?;
        let entries = self.tree_entries(cx);
        entries
            .iter()
            .find(|entry| Some(&entry.id) == palette.cursor.as_ref())
            .or(entries.first())
            .cloned()
    }

    fn tree_leaf_id(&self) -> Option<&str> {
        match &self.tree_palette.as_ref()?.load {
            Load::Ready(tree) => tree.leaf_id.as_deref(),
            _ => None,
        }
    }

    fn move_tree_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let entries = self.tree_entries(cx);
        let Some(current) = self.tree_cursor(cx) else {
            return;
        };
        let at = entries.iter().position(|e| e.id == current.id).unwrap_or(0);
        let next = (at as isize + delta).rem_euclid(entries.len() as isize) as usize;
        if let Some(palette) = self.tree_palette.as_mut() {
            palette.cursor = Some(entries[next].id.clone());
            palette.scroll.scroll_to_item(next);
            cx.notify();
        }
    }

    fn set_tree_notice(&mut self, notice: &'static str, cx: &mut Context<Self>) {
        if let Some(palette) = self.tree_palette.as_mut() {
            palette.notice = Some(notice.into());
            cx.notify();
        }
    }

    /// Jump to `entry`. A user message is a rewind to just before it, and its
    /// text comes back for editing unless something is already typed.
    fn jump_to_tree_entry(
        &mut self,
        entry: TreeEntry,
        summarize: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.composer.read(cx).run_live(cx) {
            self.set_tree_notice("Wait for the agent to finish, then jump.", cx);
            return;
        }
        if self.composer.read(cx).holds_staged_extras(cx) {
            self.set_tree_notice("Send or remove the attachments first, then jump.", cx);
            return;
        }
        if self.tree_leaf_id() == Some(entry.id.as_str()) {
            self.close_session_tree(window, cx);
            return;
        }
        let command = if summarize {
            format!("/zeron-tree-jump {} summarize", entry.id)
        } else {
            format!("/zeron-tree-jump {}", entry.id)
        };
        self.close_session_tree(window, cx);
        self.composer.update(cx, |composer, cx| {
            composer.send_aside(command, cx);
            if entry.kind == TreeEntryKind::User
                && !entry.text.is_empty()
                && composer.draft_text(cx).trim().is_empty()
            {
                composer.prefill(&entry.text, cx);
            }
        });
    }

    fn activate_tree_cursor(
        &mut self,
        summarize: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(entry) = self.tree_cursor(cx) {
            self.jump_to_tree_entry(entry, summarize, window, cx);
        }
    }

    pub(super) fn render_tree_palette(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let entries = self.tree_entries(cx);
        let cursor = self.tree_cursor(cx);
        let leaf = self.tree_leaf_id().map(str::to_owned);
        let palette = self.tree_palette.as_mut()?;
        if std::mem::take(&mut palette.focus_pending) {
            window.focus(&palette.search.focus_handle(cx), cx);
        }
        let search = palette.search.clone();
        let query = search.read(cx).text().to_string();
        let focus = palette.focus.clone();
        let scroll = palette.scroll.clone();
        let notice = palette.notice.clone();
        let theme = Theme::of(cx).for_popup();

        let body = match &palette.load {
            Load::Loading => div()
                .p(px(12.0))
                .child(popover::skeleton_rows(
                    "tree-loading",
                    &theme,
                    4,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            Load::Failed(message) => {
                command_palette::palette_empty(&theme, message.clone(), "").into_any_element()
            }
            Load::Ready(_) if entries.is_empty() => command_palette::palette_empty(
                &theme,
                "No matches",
                "Try words from a message in this conversation.",
            )
            .into_any_element(),
            Load::Ready(_) => {
                let rows = entries.iter().enumerate().map(|(ix, entry)| {
                    let active = cursor.as_ref().is_some_and(|c| c.id == entry.id);
                    let id = entry.id.clone();
                    let is_current = leaf.as_deref() == Some(entry.id.as_str());
                    let text = transcript::single_line(&entry.text);
                    let text = if text.is_empty() {
                        "(no text)".into()
                    } else {
                        text
                    };
                    popover::menu_row(&theme, active, format!("tree-row-{ix}"))
                        .id(("tree-row", ix))
                        .flex_none()
                        .rounded(px(popover::PALETTE_ITEM_RADIUS))
                        .role(gpui::Role::Button)
                        .aria_label(format!("{}: {text}", kind_label(entry.kind)))
                        .min_h(px(30.0))
                        .py(px(4.0))
                        .pl(px(8.0 + 16.0 * f32::from(entry.depth)))
                        .text_color(if entry.on_path {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .when(ix == 0, |row| row.mt(px(8.0)))
                        .when(ix + 1 == entries.len(), |row| row.mb(px(8.0)))
                        .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                            if let Some(palette) = this.tree_palette.as_mut()
                                && palette.cursor.as_deref() != Some(id.as_str())
                            {
                                palette.cursor = Some(id.clone());
                                cx.notify();
                            }
                        }))
                        .on_click({
                            let entry = entry.clone();
                            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                                this.jump_to_tree_entry(
                                    entry.clone(),
                                    event.modifiers().shift,
                                    window,
                                    cx,
                                )
                            })
                        })
                        .child(
                            div()
                                .flex_none()
                                .size(px(6.0))
                                .rounded_full()
                                .border_1()
                                .border_color(theme.text_muted)
                                .when(entry.on_path, |dot| dot.bg(theme.text_muted)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(64.0))
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(kind_label(entry.kind)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(popover::search_highlight(
                                    text.into(),
                                    Some(&query),
                                    &theme,
                                )),
                        )
                        .when_some(entry.label.clone(), |row, label| {
                            row.child(popover::kbd_hint(&theme, &label))
                        })
                        .when(is_current, |row| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .child("Current"),
                            )
                        })
                });
                let list = div()
                    .id("tree-results")
                    .min_h_0()
                    .max_h(px(command_palette::palette_results_height(viewport)))
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .flex()
                    .flex_col()
                    .gap(px(SIDEBAR_LIST_GAP))
                    .children(rows);
                command_palette::palette_results_fade(list, &scroll).into_any_element()
            }
        };

        let enter_hint = match cursor.as_ref() {
            Some(c) if leaf.as_deref() == Some(c.id.as_str()) => "Close",
            Some(c) if c.kind == TreeEntryKind::User => "Edit this message",
            _ => "Continue from here",
        };
        let card = command_palette::palette_card("tree-palette", &focus, viewport, &theme)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => this.move_tree_cursor(-1, cx),
                    "down" => this.move_tree_cursor(1, cx),
                    "enter" => {
                        let activate = this
                            .tree_palette
                            .as_mut()
                            .is_some_and(|palette| palette.enter_press.press(event.is_held));
                        if !activate {
                            cx.stop_propagation();
                            return;
                        }
                        let summarize = event.keystroke.modifiers.shift;
                        this.activate_tree_cursor(summarize, window, cx);
                    }
                    "escape" => this.close_session_tree(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, _, cx| {
                if event.keystroke.key == "enter" {
                    if let Some(palette) = this.tree_palette.as_mut() {
                        palette.enter_press.release();
                    }
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.close_session_tree(window, cx)),
            )
            .child(command_palette::palette_header(
                &theme,
                search.into_any_element(),
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child("Conversation history"),
            ))
            .child(body)
            .when_some(notice, |card, notice| {
                card.child(
                    div()
                        .px(px(16.0))
                        .py(px(6.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.danger_muted)
                        .child(notice),
                )
            })
            .child(
                command_palette::palette_footer()
                    .child(command_palette::command_key_hint(&theme, "↑ ↓", "Navigate"))
                    .child(command_palette::command_key_hint(&theme, "↵", enter_hint))
                    .child(command_palette::command_key_hint(
                        &theme,
                        "⇧↵",
                        "and summarize what you leave",
                    ))
                    .child(command_palette::command_key_hint(&theme, "Esc", "Close")),
            );
        Some(command_palette::palette_overlay(viewport, card))
    }
}
