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
    for _ in 0..1500 {
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
    core: zeron_engine::EngineCore,
    window: gpui::WindowHandle<Shell>,
}

/// The Pi chat `c` the shell opens on.
struct PiChat<'a> {
    cwd: &'a str,
    model: Option<&'a str>,
    /// The harness session it has already run, if any.
    session: Option<&'a str>,
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// A shell on `chat`, run by `pi`. The caller has entered a runtime.
fn fixture(cx: &mut TestAppContext, pi: Arc<dyn Harness>, chat: PiChat) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let registry = zeron_engine::HarnessRegistry::new();
    registry.register(pi);
    let core = zeron_engine::EngineCore::assemble(
        &dir.path().join("engine"),
        Arc::new(registry),
        HarnessId::Pi,
        None,
    )
    .unwrap();
    core.workspace
        .create_chat(
            "c",
            None,
            Some(&core.device_id),
            None,
            Some(chat.cwd.into()),
        )
        .unwrap();
    core.workspace.rename_chat("c", "A Pi chat").unwrap();
    core.workspace
        .set_chat_config(
            "c",
            &ChatConfig {
                harness: HarnessId::Pi,
                model: chat.model.map(str::to_owned),
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            },
        )
        .unwrap();
    if let Some(session) = chat.session {
        core.workspace
            .set_chat_harness_session("c", session, chat.cwd);
    }

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
        core,
        window,
    }
}

fn fake_pi(tree: SessionTree, runs: &Arc<Mutex<Vec<RunRequest>>>) -> Arc<dyn Harness> {
    Arc::new(FakePi {
        runs: runs.clone(),
        tree,
    })
}

const FAKE_CHAT: PiChat = PiChat {
    cwd: "/work",
    model: None,
    session: Some("pi-session-1"),
};

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
    let runs = Arc::new(Mutex::new(Vec::new()));
    let Fixture { _dir, core, window } = fixture(cx, fake_pi(six_rows(), &runs), FAKE_CHAT);
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
    } = fixture(cx, fake_pi(long, &Default::default()), FAKE_CHAT);
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

/// Real Pi (a local mock provider, no network) behind the real engine and
/// shell: browse, jump, and see what the conversation and the next turn do.
fn isolated_pi(dir: &std::path::Path) -> zeron_harness::PiHarness {
    use std::os::unix::fs::PermissionsExt;
    let agent = dir.join("agent");
    std::fs::create_dir_all(agent.join("extensions")).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"retry":{"enabled":false}}"#,
    )
    .unwrap();
    std::fs::write(
        agent.join("extensions/probe.ts"),
        include_str!("../../../harness/tests/fixtures/pi-rpc-probe.ts"),
    )
    .unwrap();
    let exe = zeron_harness::PiHarness::new()
        .resolve_executable()
        .expect("Pi CLI installed");
    let quote =
        |p: &std::path::Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let wrapper = dir.join("pi-probe");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport PI_CODING_AGENT_DIR={}\nexec {} \"$@\"\n",
            quote(&agent),
            quote(&exe)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    zeron_harness::PiHarness::new()
        .with_executable(wrapper)
        .with_agent_dir(&agent)
        .with_session_store(dir.join("index"))
}

/// The conversation as the chat's transcript holds it: who said what.
fn transcript(core: &zeron_engine::EngineCore) -> Vec<(zeron_doc::MessageRole, String)> {
    core.doc_host
        .open("c")
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
        .into_iter()
        .map(|entry| {
            let text = entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    zeron_doc::MessagePart::Text { text, .. } => Some(text.trim()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            (entry.role, text)
        })
        .collect()
}

/// Ways it fails:
/// - The jump does not reach Pi as a jump, or Pi never answers, so the
///   transcript shows nothing of it.
/// - The transcript does not say where the conversation went.
/// - The turn after a jump still carries the turns that were left behind, or
///   the abandoned turns vanish from the palette instead of staying as a branch.
/// - The message jumped back to does not come back into the composer.
#[gpui::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
fn a_jump_in_a_real_pi_chat_changes_what_the_next_turn_sees(cx: &mut TestAppContext) {
    use zeron_doc::MessageRole::{Assistant, User};
    let runtime = runtime();
    let _guard = runtime.enter();
    let work = tempfile::tempdir().unwrap();
    let Fixture {
        _dir, core, window, ..
    } = fixture(
        cx,
        Arc::new(isolated_pi(work.path())),
        PiChat {
            cwd: work.path().to_str().unwrap(),
            model: Some("zeron-probe/mock"),
            session: None,
        },
    );
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap()
    };
    let say = |cx: &mut TestAppContext, text: &str| {
        let replies = |core: &zeron_engine::EngineCore| {
            transcript(core)
                .iter()
                .filter(|(role, _)| *role == Assistant)
                .count()
        };
        let before = replies(&core);
        window
            .update(cx, |shell, _, cx| {
                shell
                    .composer
                    .update(cx, |composer, cx| composer.send_aside(text.into(), cx))
            })
            .unwrap();
        settle(cx, "the agent to answer", |_| {
            replies(&core) == before + 1
                && core
                    .doc_host
                    .open("c")
                    .unwrap()
                    .doc()
                    .read_entries()
                    .unwrap()
                    .last()
                    .is_some_and(|entry| entry.status == Some(zeron_doc::MessageStatus::Complete))
        });
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.begin_pending_send("c", "done", chrono::Utc::now());
                    state.end_pending_send("c", "done");
                });
            })
            .unwrap();
    };
    let rows = |cx: &mut TestAppContext| -> Vec<String> {
        window
            .update(cx, |shell, _, cx| {
                shell.tree_entries(cx).into_iter().map(|e| e.text).collect()
            })
            .unwrap()
    };
    let open_tree = |cx: &mut TestAppContext, rows_expected: usize| {
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
                .update(cx, |shell, _, cx| {
                    shell.tree_entries(cx).len() == rows_expected
                })
                .unwrap()
        });
    };

    draw(cx);
    for text in ["one", "two", "three"] {
        say(cx, text);
    }
    open_tree(cx, 6);
    assert_eq!(
        rows(cx),
        ["one", "MOCK:one", "two", "MOCK:two", "three", "MOCK:three"]
    );
    let two = window
        .update(cx, |shell, _, cx| shell.tree_entries(cx)[2].id.clone())
        .unwrap();
    // The parked Pi treats output in the first second after a turn as that
    // turn's tail, so wait the way a person browsing the palette would.
    std::thread::sleep(std::time::Duration::from_millis(1300));
    cx.simulate_keystrokes(window.into(), "up up up enter");
    settle(cx, "Pi to answer the jump", |_| {
        transcript(&core).len() == 8
    });
    let timeline = transcript(&core);
    assert_eq!(
        timeline[6..],
        [
            (User, format!("/zeron-tree-jump {two}")),
            (
                Assistant,
                "Went back to before “two”. Continue from here, or edit and resend it.".into()
            ),
        ],
        "the transcript says where the conversation went"
    );
    window
        .update(cx, |shell, _, cx| {
            assert_eq!(shell.composer.read(cx).draft_text(cx), "two");
        })
        .unwrap();
    window
        .update(cx, |shell, _, cx| {
            shell.state.update(cx, |state, _| {
                state.begin_pending_send("c", "done", chrono::Utc::now());
                state.end_pending_send("c", "done");
            });
        })
        .unwrap();

    say(cx, "ctx?");
    assert_eq!(
        transcript(&core).last().unwrap().1,
        "MOCK:ctx=one|ctx?",
        "the model no longer sees the turns jumped over"
    );
    open_tree(cx, 8);
    assert_eq!(
        rows(cx),
        [
            "one",
            "MOCK:one",
            "ctx?",
            "MOCK:ctx=one|ctx?",
            "two",
            "MOCK:two",
            "three",
            "MOCK:three"
        ],
        "the new branch leads, and the turns left behind stay reachable"
    );

    let artifact = std::env::var_os("ZERON_E2E_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("zeron-tree-ui-e2e"));
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(
        artifact.join("transcript.txt"),
        transcript(&core)
            .iter()
            .map(|(role, text)| format!("{role:?}: {text}\n"))
            .collect::<String>(),
    )
    .unwrap();
    runtime.block_on(core.shutdown());
}
