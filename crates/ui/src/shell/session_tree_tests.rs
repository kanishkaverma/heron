//! `/tree` in a Pi chat, through the real shell and composer against a real
//! engine (only the agent is faked). Ways it fails:
//! - `/tree` is not offered in a Pi chat, or is offered in another agent's.
//! - The palette shows another chat's tree, loses the fork's indentation, or
//!   starts the cursor anywhere but the current position.
//! - Typing does not narrow the rows.
//! - Enter on a user message does not reach the agent as a jump to that
//!   message, or does not hand its text back to the composer.
//! - Enter on a reply hands text back, or replaces what the user already typed.
//! - Shift+Enter jumps without asking for a summary of the branch left behind.
//! - A jump is sent while the agent is still working.
//! - Escape leaves the palette open.
use super::*;
use crate::{
    settings,
    state::{AppState, EngineBootConfig, EngineHandle},
    theme::Theme,
};
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use gpui::TestAppContext;
use std::sync::{Arc, Mutex};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, ChatConfig, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionTree, SteeringMode, TreeEntry, TreeEntryKind,
};

struct FakePi {
    runs: Arc<Mutex<Vec<RunRequest>>>,
    tree: SessionTree,
}

#[async_trait]
impl Harness for FakePi {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Fake Pi"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn session_tree(
        &self,
        session_id: &str,
        _: &std::path::Path,
    ) -> Result<Option<SessionTree>, HarnessError> {
        assert_eq!(session_id, "pi-session-1");
        Ok(Some(self.tree.clone()))
    }
    async fn run(
        &self,
        request: RunRequest,
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.runs.lock().unwrap().push(request);
        Ok(futures::stream::iter([Ok(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("pi-session-1".into()),
        })])
        .boxed())
    }
}

fn settle(cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    for _ in 0..300 {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

struct Fixture {
    _dir: tempfile::TempDir,
    runs: Arc<Mutex<Vec<RunRequest>>>,
    core: zeron_engine::EngineCore,
    window: gpui::WindowHandle<Shell>,
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// A shell on a Pi chat `c` whose session is `tree`. The caller has entered
/// a runtime.
fn fixture(cx: &mut TestAppContext, tree: SessionTree) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let runs = Arc::new(Mutex::new(Vec::new()));
    let registry = zeron_engine::HarnessRegistry::new();
    registry.register(Arc::new(FakePi {
        runs: runs.clone(),
        tree,
    }));
    let core = zeron_engine::EngineCore::assemble(
        &dir.path().join("engine"),
        Arc::new(registry),
        HarnessId::Pi,
        None,
    )
    .unwrap();
    core.workspace
        .create_chat("c", None, Some(&core.device_id), None, Some("/work".into()))
        .unwrap();
    core.workspace
        .set_chat_config(
            "c",
            &ChatConfig {
                harness: HarnessId::Pi,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            },
        )
        .unwrap();
    core.workspace
        .set_chat_harness_session("c", "pi-session-1", "/work");

    cx.executor().allow_parking();
    cx.update(|cx| {
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let engine = EngineHandle::from_test_client(zeron_rpc::memory_client(core.rpc_service()));
    let rows = core.workspace.read_chats().unwrap();
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(
            state,
            EngineBootConfig {
                data_dir: dir.path().into(),
                ipc_port: 0,
                edge_url: "http://127.0.0.1:1".into(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: HarnessId::Pi,
            },
            cx,
        )
    });
    window
        .update(cx, |shell, _, cx| {
            shell.state.update(cx, |state, cx| {
                state.chats = rows;
                state.set_test_engine(engine);
                state.selected_chat = Some("c".into());
                cx.notify();
            });
            shell.debug_gate = Some(crate::state::GatePhase::Ready);
        })
        .unwrap();
    Fixture {
        _dir: dir,
        runs,
        core,
        window,
    }
}

fn six_rows() -> SessionTree {
    let entry = |id: &str, depth, kind, text: &str, on_path| TreeEntry {
        id: id.into(),
        depth,
        kind,
        text: text.into(),
        label: None,
        on_path,
    };
    use TreeEntryKind::{Assistant as A, User as U};
    SessionTree {
        leaf_id: Some("a2".into()),
        entries: vec![
            entry("u1", 0, U, "one", true),
            entry("a1", 0, A, "reply one", true),
            entry("u2", 1, U, "two (new)", true),
            entry("a2", 1, A, "reply two (new)", true),
            entry("u3", 1, U, "two (old)", false),
            entry("a3", 1, A, "reply two (old)", false),
        ],
    }
}

#[gpui::test]
fn tree_palette_jumps_and_hands_the_message_back(cx: &mut TestAppContext) {
    let runtime = runtime();
    let _guard = runtime.enter();
    let Fixture {
        _dir,
        runs,
        core,
        window,
    } = fixture(cx, six_rows());
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap()
    };
    let open_tree = |cx: &mut TestAppContext| {
        window
            .update(cx, |shell, _, cx| {
                shell.pending_workspace_command = Some(crate::composer::WorkspaceCommand::Tree);
                cx.notify();
            })
            .unwrap();
        draw(cx);
        settle(cx, "the tree to load", |cx| {
            draw(cx);
            window
                .update(cx, |shell, _, cx| shell.tree_entries(cx).len() == 6)
                .unwrap()
        });
    };
    let visible = |cx: &mut TestAppContext| -> Vec<String> {
        window
            .update(cx, |shell, _, cx| {
                shell.tree_entries(cx).into_iter().map(|e| e.id).collect()
            })
            .unwrap()
    };
    let cursor = |cx: &mut TestAppContext| -> Option<String> {
        window
            .update(cx, |shell, _, cx| shell.tree_cursor(cx).map(|e| e.id))
            .unwrap()
    };
    let draft = |cx: &mut TestAppContext| -> String {
        window
            .update(cx, |shell, _, cx| shell.composer.read(cx).draft_text(cx))
            .unwrap()
    };
    let prompts = || -> Vec<String> {
        runs.lock()
            .unwrap()
            .iter()
            .map(|r| r.prompt.clone())
            .collect()
    };

    draw(cx);
    cx.run_until_parked();
    draw(cx);
    open_tree(cx);
    assert_eq!(visible(cx), ["u1", "a1", "u2", "a2", "u3", "a3"]);
    assert_eq!(cursor(cx).as_deref(), Some("a2"), "starts at where you are");
    window
        .update(cx, |shell, _, cx| {
            let search = shell.tree_palette.as_ref().unwrap().search.clone();
            search.update(cx, |search, cx| search.set_text("old", cx));
        })
        .unwrap();
    assert_eq!(visible(cx), ["u3", "a3"]);
    window
        .update(cx, |shell, _, cx| {
            let search = shell.tree_palette.as_ref().unwrap().search.clone();
            search.update(cx, |search, cx| search.set_text("", cx));
        })
        .unwrap();
    draw(cx);

    // A user message: jump to before it, and its text comes back to edit.
    cx.simulate_keystrokes(window.into(), "up enter");
    settle(cx, "the jump to reach the agent", |_| !prompts().is_empty());
    assert_eq!(prompts(), ["/zeron-tree-jump u2"]);
    assert_eq!(
        runs.lock().unwrap()[0].resume.as_deref(),
        Some("pi-session-1")
    );
    assert_eq!(draft(cx), "two (new)");
    window
        .update(cx, |shell, _, _| assert!(shell.tree_palette.is_none()))
        .unwrap();
    let transcript = core
        .doc_host
        .open("c")
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert_eq!(transcript[0].role, zeron_doc::MessageRole::User);
    assert_eq!(
        transcript[0].parts,
        [zeron_doc::MessagePart::Text {
            id: "t0".into(),
            text: "/zeron-tree-jump u2".into()
        }],
        "the jump is a line in the conversation"
    );

    // The app clears its working indicator when the engine reports the turn
    // done; this test has no session watcher, so it does the same by hand.
    let agent_idle = |cx: &mut TestAppContext| {
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.begin_pending_send("c", "done", chrono::Utc::now());
                    state.end_pending_send("c", "done");
                });
            })
            .unwrap()
    };
    let type_draft = |cx: &mut TestAppContext, text: &str| {
        window
            .update(cx, |shell, _, cx| {
                shell
                    .composer
                    .update(cx, |composer, cx| composer.prefill(text, cx))
            })
            .unwrap()
    };

    // A reply: jump there, and nothing comes back to the composer.
    agent_idle(cx);
    type_draft(cx, "");
    open_tree(cx);
    cx.simulate_keystrokes(window.into(), "down down enter");
    settle(cx, "the second jump", |_| prompts().len() == 2);
    assert_eq!(prompts()[1], "/zeron-tree-jump a3");
    assert_eq!(draft(cx), "");

    // A user message with something typed already: it stays, and Shift+Enter
    // asks for a summary of the branch left behind.
    agent_idle(cx);
    type_draft(cx, "my draft");
    open_tree(cx);
    cx.simulate_keystrokes(window.into(), "down shift-enter");
    settle(cx, "the third jump", |_| prompts().len() == 3);
    assert_eq!(prompts()[2], "/zeron-tree-jump u3 summarize");
    assert_eq!(draft(cx), "my draft");

    // Not while the agent is working.
    window
        .update(cx, |shell, _, cx| {
            shell.state.update(cx, |state, _| {
                state.begin_pending_send("c", "running", chrono::Utc::now())
            });
        })
        .unwrap();
    open_tree(cx);
    cx.simulate_keystrokes(window.into(), "up enter");
    cx.run_until_parked();
    assert_eq!(prompts().len(), 3, "no jump while working");
    window
        .update(cx, |shell, _, _| {
            assert!(shell.tree_palette.as_ref().unwrap().notice.is_some())
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "escape");
    window
        .update(cx, |shell, _, _| assert!(shell.tree_palette.is_none()))
        .unwrap();
    runtime.block_on(core.shutdown());
}

/// A long conversation puts the current position far down the list.
/// Ways it fails:
/// - The palette opens at the top with the highlighted current position
///   scrolled out of sight, so Enter acts on a row nobody can see.
#[gpui::test]
fn the_current_position_is_in_view_when_the_palette_opens(cx: &mut TestAppContext) {
    let runtime = runtime();
    let _guard = runtime.enter();
    let mut long = six_rows();
    long.entries = (0..150)
        .flat_map(|turn| {
            let entry = |kind, text: String| TreeEntry {
                id: format!("{turn}-{text}"),
                depth: 0,
                kind,
                text,
                label: None,
                on_path: true,
            };
            [
                entry(TreeEntryKind::User, "question".into()),
                entry(TreeEntryKind::Assistant, "answer".into()),
            ]
        })
        .collect();
    long.leaf_id = long.entries.last().map(|entry| entry.id.clone());
    let rows = long.entries.len();
    let Fixture {
        _dir, core, window, ..
    } = fixture(cx, long);
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap()
    };
    window
        .update(cx, |shell, _, cx| {
            shell.pending_workspace_command = Some(crate::composer::WorkspaceCommand::Tree);
            cx.notify();
        })
        .unwrap();
    draw(cx);
    settle(cx, "the tree to load", |cx| {
        draw(cx);
        window
            .update(cx, |shell, _, cx| shell.tree_entries(cx).len() == rows)
            .unwrap()
    });
    draw(cx);
    draw(cx);
    let (top, bottom) = window
        .update(cx, |shell, _, _| {
            let scroll = &shell.tree_palette.as_ref().unwrap().scroll;
            (scroll.top_item(), scroll.bottom_item())
        })
        .unwrap();
    assert!(
        (top..=bottom).contains(&(rows - 1)),
        "rows {top}..={bottom} are in view, the current position is row {}",
        rows - 1
    );
    runtime.block_on(core.shutdown());
}
