//! Real native Pi, isolated settings, and a local provider (no network/API spend).
#![cfg(unix)]
use futures::StreamExt;
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::sync::{mpsc, oneshot};
use zeron_harness::{CancellationToken, Harness, PiHarness, RunControls, SteerMessage};
use zeron_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel, UserInputAnswer};

fn isolated_pi() -> (tempfile::TempDir, PiHarness) {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let agent = cwd.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"retry":{"enabled":false}}"#,
    )
    .unwrap();
    let extensions = agent.join("extensions");
    std::fs::create_dir_all(&extensions).unwrap();
    std::fs::write(
        extensions.join("probe.ts"),
        include_str!("fixtures/pi-rpc-probe.ts"),
    )
    .unwrap();
    let exe = PiHarness::new()
        .resolve_executable()
        .expect("Pi CLI installed");
    let quote =
        |p: &std::path::Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let wrapper = cwd.join("pi-probe");
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
    let harness = PiHarness::new()
        .with_executable(wrapper)
        .with_agent_dir(&agent)
        .with_session_store(cwd.join("index"));
    (dir, harness)
}

#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn real_pi_mock_lifecycle() {
    let (dir, harness) = isolated_pi();
    let cwd = dir.path();
    let mut session = None;
    for (prompt, expected) in [
        ("/probe-noop", DoneStatus::Completed),
        ("hello", DoneStatus::Completed),
        ("/probe-new", DoneStatus::Completed),
        ("resume", DoneStatus::Completed),
        ("/probe-noop", DoneStatus::Completed),
        ("/probe-input", DoneStatus::Completed),
        ("error", DoneStatus::Errored),
        ("slow", DoneStatus::Interrupted),
        ("steering slow", DoneStatus::Completed),
        ("/probe-new", DoneStatus::Completed),
        ("/probe-metadata", DoneStatus::Completed),
    ] {
        let (steer, steering) = mpsc::channel(8);
        let interrupt = CancellationToken::new();
        let controls = RunControls {
            realtime: None,
            execution_lease: None,
            steering,
            interrupt: interrupt.clone(),
            request_input: Box::new(|questions| {
                let (tx, rx) = oneshot::channel();
                tx.send(vec![UserInputAnswer {
                    question_id: questions[0].id.clone(),
                    labels: vec!["local answer".into()],
                }])
                .unwrap();
                rx
            }),
            turn: Default::default(),
        };
        let request = RunRequest {
            prompt: prompt.into(),
            harness: None,
            model: Some("zeron-probe/mock".into()),
            reasoning: None,
            model_options: Default::default(),
            cwd: cwd.display().to_string(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            resume: session.clone(),
            attachments: vec![],
            worktree: None,
            mcp: None,
        };
        let previous_session = session.clone();
        let mut stream = harness.run(request, controls).await.unwrap();
        let mut sender = Some(steer);
        let mut done = 0;
        let mut confirmed = 0;
        let mut text = String::new();
        tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(event) = stream.next().await {
                match event.unwrap() {
                    AgentEvent::SessionStarted { session_id, .. } => {
                        if let Some(old) = &session {
                            if prompt != "/probe-new" {
                                assert_eq!(old, &session_id);
                            }
                        }
                        session = Some(session_id);
                        if prompt == "steering slow" {
                            sender
                                .take()
                                .unwrap()
                                .send(SteerMessage {
                                    prompt: "redirect".into(),
                                    message_id: None,
                                    attachments: Vec::new(),
                                    config: None,
                                })
                                .await
                                .unwrap();
                        } else {
                            sender.take();
                        }
                        if prompt == "slow" {
                            let token = interrupt.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_millis(150)).await;
                                token.cancel();
                            });
                        }
                    }
                    AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                    AgentEvent::Steered { .. } => confirmed += 1,
                    AgentEvent::Done {
                        status,
                        error,
                        session_id,
                        ..
                    } => {
                        assert_eq!(
                            session_id, session,
                            "Done must publish the current native session identity"
                        );
                        assert_eq!(status, expected, "{prompt}: {error:?}");
                        done += 1;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("native Pi run must settle");
        assert_eq!(done, 1, "{prompt}: {text}");
        if prompt == "/probe-new" {
            assert_ne!(session, previous_session);
        }
        if prompt == "/probe-input" {
            assert!(text.contains("answer:local answer"), "{text}");
        }
        if prompt == "steering slow" {
            assert_eq!(confirmed, 1);
            assert!(text.contains("MOCK:redirect"), "{text}");
        }
        if matches!(prompt, "hello" | "resume") {
            assert_eq!(text, format!("MOCK:{prompt}"));
        }
    }
    // Unsaved extension entries are not equivalent to an empty conversation, so
    // the UUID is not recreated. Pi has no public RPC to restore them; the chat
    // continues in a new session and says so instead of failing every message.
    let (_, steering) = mpsc::channel(1);
    let controls = RunControls {
        realtime: None,
        execution_lease: None,
        steering,
        interrupt: CancellationToken::new(),
        request_input: Box::new(|_| oneshot::channel().1),
        turn: Default::default(),
    };
    let request = RunRequest {
        prompt: "after loss".into(),
        harness: None,
        model: Some("zeron-probe/mock".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        resume: session.clone(),
        attachments: vec![],
        worktree: None,
        mcp: None,
    };
    let events: Vec<_> = tokio::time::timeout(
        Duration::from_secs(20),
        harness
            .run(request, controls)
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect(),
    )
    .await
    .expect("native Pi run must settle");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Error { message }
            if message.contains("without the previous context"))),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Done { status, session_id, .. }
            if *status == DoneStatus::Completed && session_id.is_some() && *session_id != session)),
        "{events:?}"
    );
}

async fn wait_probe_lines(path: &std::path::Path, count: usize) -> Vec<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let lines: Vec<_> = std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            if lines.len() >= count {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Pi probe must reach the expected barrier")
}

#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn real_pi_steering_bursts_share_the_next_model_call() {
    let (dir, harness) = isolated_pi();
    let cwd = dir.path();
    let (tx, steering) = mpsc::channel(8);
    let controls = RunControls {
        realtime: None,
        execution_lease: None,
        steering,
        interrupt: CancellationToken::new(),
        request_input: Box::new(|_| oneshot::channel().1),
        turn: Default::default(),
    };
    let request = RunRequest {
        prompt: "burst hold".into(),
        harness: None,
        model: Some("zeron-probe/mock".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        resume: None,
        attachments: vec![],
        worktree: None,
        mcp: None,
    };
    let mut stream = harness.run(request, controls).await.unwrap();
    let calls = cwd.join("probe-model-calls.jsonl");
    let inputs = cwd.join("probe-inputs.jsonl");
    assert_eq!(
        wait_probe_lines(&calls, 1).await[0],
        serde_json::json!(["burst hold"])
    );
    let burst: Vec<_> = (0..40).map(|i| format!("burst-{}", i / 2)).collect();
    for (i, prompt) in burst.iter().enumerate() {
        tx.send(SteerMessage {
            prompt: prompt.clone(),
            message_id: Some(format!("burst-user-{i}")),
            attachments: Vec::new(),
            config: None,
        })
        .await
        .unwrap();
    }
    assert_eq!(
        wait_probe_lines(&inputs, burst.len()).await,
        burst
            .iter()
            .map(|s| serde_json::json!(s))
            .collect::<Vec<_>>()
    );
    assert_eq!(wait_probe_lines(&calls, 1).await.len(), 1);
    std::fs::write(cwd.join("probe-release-initial"), "").unwrap();
    let snapshot = wait_probe_lines(&calls, 2).await;
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot[1], serde_json::json!(burst));
    // Anything arriving after the next call began belongs to the following
    // step, rather than being claimed as part of the already-running call.
    let late = vec!["late-1", "late-2", "late-3"];
    for prompt in &late {
        tx.send(SteerMessage::text(*prompt)).await.unwrap();
    }
    drop(tx);
    wait_probe_lines(&inputs, burst.len() + late.len()).await;
    assert_eq!(wait_probe_lines(&calls, 2).await.len(), 2);
    std::fs::write(cwd.join("probe-release-burst"), "").unwrap();
    let snapshot = wait_probe_lines(&calls, 3).await;
    assert_eq!(snapshot.len(), 3);
    assert_eq!(snapshot[2], serde_json::json!(late));
    std::fs::write(cwd.join("probe-release-late"), "").unwrap();
    let mut confirmed = 0;
    let mut done = vec![];
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                AgentEvent::Steered { .. } => confirmed += 1,
                AgentEvent::Done { status, .. } => done.push(status),
                AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(confirmed, burst.len() + late.len());
    assert_eq!(done, vec![DoneStatus::Completed]);
    assert_eq!(
        text,
        format!(
            "MOCK:burst holdMOCK:{}MOCK:{}",
            burst.join("|"),
            late.join("|")
        )
    );
    assert_eq!(wait_probe_lines(&calls, 3).await.len(), 3);
}

/// `/reload` is a TUI-only built-in that Pi's RPC `get_commands` never lists.
/// Ways it fails: the menu lacks it; it reaches the model as text ("MOCK:/reload");
/// the reply is a raw JSON dump; a resource added since the last run stays
/// missing from the menu; the turn never completes; it shadows an extension's
/// own `reload` command.
#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn real_pi_reload_refreshes_commands_without_prompting_the_model() {
    let (dir, harness) = isolated_pi();
    let cwd = dir.path();
    let listed = harness.commands_for(cwd).await.unwrap();
    assert!(listed.iter().any(|c| c.name == "reload"), "{listed:?}");
    assert!(!listed.iter().any(|c| c.name == "fresh-template"));

    let prompts = cwd.join("agent/prompts");
    std::fs::create_dir_all(&prompts).unwrap();
    std::fs::write(
        prompts.join("fresh-template.md"),
        "---\ndescription: Added after discovery\n---\nSay hi\n",
    )
    .unwrap();

    let (_steer, steering) = mpsc::channel(8);
    let controls = RunControls {
        realtime: None,
        execution_lease: None,
        steering,
        interrupt: CancellationToken::new(),
        request_input: Box::new(|_| oneshot::channel().1),
        turn: Default::default(),
    };
    drop(_steer);
    let request = RunRequest {
        prompt: "/reload".into(),
        harness: None,
        model: Some("zeron-probe/mock".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        resume: None,
        attachments: vec![],
        worktree: None,
        mcp: None,
    };
    let events: Vec<AgentEvent> = tokio::time::timeout(
        Duration::from_secs(20),
        harness
            .run(request, controls)
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect(),
    )
    .await
    .expect("/reload turn completes");
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!text.contains("MOCK:"), "reached the model: {text}");
    assert!(!text.contains("\"commands\""), "raw JSON reply: {text}");
    assert!(text.contains("Reloaded"), "{text}");
    let refreshed = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AvailableCommands { commands } => Some(commands),
            _ => None,
        })
        .last()
        .unwrap();
    assert!(
        refreshed.iter().any(|c| c.name == "fresh-template"),
        "{refreshed:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
    let artifact = std::env::var_os("ZERON_E2E_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("zeron-pi-reload-e2e"));
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(
        artifact.join("reload-events.txt"),
        events
            .iter()
            .map(|e| format!("{e:?}\n"))
            .collect::<String>(),
    )
    .unwrap();
}

/// One Zeron turn in a fresh Pi process: Zeron may reap a parked runner at any
/// time, so every turn here proves what survives a process boundary.
async fn turn(
    harness: &PiHarness,
    cwd: &std::path::Path,
    prompt: &str,
    resume: Option<&str>,
) -> (Vec<AgentEvent>, String) {
    let (steer, steering) = mpsc::channel(8);
    drop(steer);
    let controls = RunControls {
        realtime: None,
        execution_lease: None,
        steering,
        interrupt: CancellationToken::new(),
        request_input: Box::new(|_| oneshot::channel().1),
        turn: Default::default(),
    };
    let request = RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: Some("zeron-probe/mock".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        resume: resume.map(str::to_owned),
        attachments: vec![],
        worktree: None,
        mcp: None,
    };
    let events: Vec<AgentEvent> = tokio::time::timeout(
        Duration::from_secs(30),
        harness
            .run(request, controls)
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect(),
    )
    .await
    .unwrap_or_else(|_| panic!("turn {prompt:?} completes"));
    let session = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Done {
                session_id: Some(id),
                ..
            } => Some(id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("turn {prompt:?} reports its session: {events:?}"));
    (events, session)
}

fn reply(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn rows(tree: &zeron_proto::SessionTree) -> Vec<(char, u16, bool, &str)> {
    use zeron_proto::TreeEntryKind::*;
    tree.entries
        .iter()
        .map(|e| {
            let kind = match e.kind {
                User => 'U',
                Assistant => 'A',
                Summary => 'S',
                Compaction => 'C',
            };
            (kind, e.depth, e.on_path, e.text.as_str())
        })
        .collect()
}

/// `/tree`: browse a Pi session's tree and jump to a point in it.
/// Ways it fails:
/// - The tree is unreadable, loses ids, or shows tool/label/custom entries.
/// - A fork is not indented, or the branch holding the leaf is not listed first.
/// - The jump reaches the model as text instead of moving the leaf.
/// - The jump only moves Pi's in-memory leaf, so the next turn (a new process)
///   continues from the old tip and the model still sees the abandoned turns.
/// - Jumping to a user message keeps that message in context instead of
///   rewinding to before it.
/// - The internal jump command leaks into the slash menu.
/// - A summarising jump adds no summary, or the summary never reaches the next
///   turn's context.
/// - A jump to an id that is not in the session corrupts it instead of failing.
#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn real_pi_tree_jump_survives_new_processes() {
    let (dir, harness) = isolated_pi();
    let cwd = dir.path();
    let mut session = String::new();
    for prompt in ["one", "two", "three"] {
        session = turn(
            &harness,
            cwd,
            prompt,
            (!session.is_empty()).then_some(&*session),
        )
        .await
        .1;
    }

    let tree = harness
        .session_tree(&session, cwd)
        .await
        .unwrap()
        .expect("a Pi session is a tree");
    assert_eq!(
        rows(&tree),
        [
            ('U', 0, true, "one"),
            ('A', 0, true, "MOCK:one"),
            ('U', 0, true, "two"),
            ('A', 0, true, "MOCK:two"),
            ('U', 0, true, "three"),
            ('A', 0, true, "MOCK:three"),
        ]
    );
    assert_eq!(tree.leaf_id.as_deref(), Some(tree.entries[5].id.as_str()));

    let two = tree.entries[2].id.clone();
    let (events, jumped) = turn(
        &harness,
        cwd,
        &format!("/zeron-tree-jump {two}"),
        Some(&session),
    )
    .await;
    assert_eq!(jumped, session, "a jump stays in the same session");
    let said = reply(&events);
    assert!(!said.contains("MOCK:"), "reached the model: {said}");
    assert!(said.contains("two"), "names where it went: {said}");
    for event in &events {
        if let AgentEvent::AvailableCommands { commands } = event {
            assert!(
                !commands.iter().any(|c| c.name.starts_with("zeron-tree")),
                "internal command leaked into the menu: {commands:?}"
            );
        }
    }

    let (events, _) = turn(&harness, cwd, "ctx?", Some(&session)).await;
    assert_eq!(
        reply(&events).trim(),
        "MOCK:ctx=one|ctx?",
        "the abandoned turns left the model's context"
    );

    let tree = harness.session_tree(&session, cwd).await.unwrap().unwrap();
    assert_eq!(
        rows(&tree),
        [
            ('U', 0, true, "one"),
            ('A', 0, true, "MOCK:one"),
            ('U', 1, true, "ctx?"),
            ('A', 1, true, "MOCK:ctx=one|ctx?"),
            ('U', 1, false, "two"),
            ('A', 1, false, "MOCK:two"),
            ('U', 1, false, "three"),
            ('A', 1, false, "MOCK:three"),
        ]
    );
    assert_eq!(tree.leaf_id.as_deref(), Some(tree.entries[3].id.as_str()));

    let old_tip = tree.entries[7].id.clone();
    let (events, _) = turn(
        &harness,
        cwd,
        &format!("/zeron-tree-jump {old_tip} summarize"),
        Some(&session),
    )
    .await;
    assert!(reply(&events).contains("summary"), "{}", reply(&events));
    let tree = harness.session_tree(&session, cwd).await.unwrap().unwrap();
    let summary = tree
        .entries
        .iter()
        .find(|e| e.kind == zeron_proto::TreeEntryKind::Summary)
        .expect("the abandoned branch was summarised");
    assert!(summary.on_path && tree.leaf_id.as_deref() == Some(summary.id.as_str()));
    let (events, _) = turn(&harness, cwd, "ctx?", Some(&session)).await;
    let context = reply(&events);
    assert!(
        context.contains("ctx=one|two|three|") && context.contains("summary"),
        "the branch followed, plus the summary of the one left: {context}"
    );

    let (events, _) = turn(
        &harness,
        cwd,
        "/zeron-tree-jump no-such-entry",
        Some(&session),
    )
    .await;
    assert!(
        reply(&events).contains("no-such-entry"),
        "{}",
        reply(&events)
    );
    let (events, _) = turn(&harness, cwd, "ctx?", Some(&session)).await;
    assert!(
        reply(&events).contains("ctx=one|two|three|"),
        "{}",
        reply(&events)
    );

    let artifact = std::env::var_os("ZERON_E2E_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("zeron-pi-tree-e2e"));
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(
        artifact.join("tree.json"),
        serde_json::to_string_pretty(&tree).unwrap(),
    )
    .unwrap();
}

async fn next_turn(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, zeron_harness::HarnessError>> + Unpin),
) -> Vec<AgentEvent> {
    let mut events = vec![];
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            let done = matches!(event, AgentEvent::Done { .. });
            events.push(event);
            if done {
                break;
            }
        }
    })
    .await
    .expect("turn completes");
    events
}

/// A jump sent to a Pi that Zeron keeps parked between turns takes effect in
/// that same process, and survives into the next one.
/// Ways it fails:
/// - The jump, arriving as a steer into an idle runner, reaches the model
///   as text, or never completes the turn.
/// - The parked process keeps answering from the old tip while the file says
///   otherwise (or the reverse).
#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn real_pi_tree_jump_in_a_parked_runner() {
    let (dir, harness) = isolated_pi();
    let cwd = dir.path();
    let (steer, steering) = mpsc::channel(8);
    let controls = RunControls {
        realtime: None,
        execution_lease: None,
        steering,
        interrupt: CancellationToken::new(),
        request_input: Box::new(|_| oneshot::channel().1),
        turn: Default::default(),
    };
    let request = RunRequest {
        prompt: "one".into(),
        harness: None,
        model: Some("zeron-probe/mock".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        resume: None,
        attachments: vec![],
        worktree: None,
        mcp: None,
    };
    let mut stream = harness.run(request, controls).await.unwrap();
    let say = |text: String| {
        let steer = steer.clone();
        async move {
            steer
                .send(SteerMessage {
                    prompt: text,
                    message_id: None,
                    attachments: Vec::new(),
                    config: None,
                })
                .await
                .unwrap();
        }
    };

    let first = next_turn(&mut stream).await;
    let session = match first.last() {
        Some(AgentEvent::Done {
            session_id: Some(id),
            ..
        }) => id.clone(),
        other => panic!("first turn ends with its session: {other:?}"),
    };
    say("two".into()).await;
    assert!(reply(&next_turn(&mut stream).await).contains("MOCK:two"));

    let tree = harness.session_tree(&session, cwd).await.unwrap().unwrap();
    let two = tree.entries[2].id.clone();
    assert_eq!(tree.entries[2].text, "two");
    say(format!("/zeron-tree-jump {two}")).await;
    let jump = reply(&next_turn(&mut stream).await);
    assert!(jump.contains("two") && !jump.contains("MOCK:"), "{jump}");

    say("ctx?".into()).await;
    assert_eq!(
        reply(&next_turn(&mut stream).await).trim(),
        "MOCK:ctx=one|ctx?",
        "the parked process continues from the jump"
    );
    drop(say);
    drop(steer);
    while stream.next().await.is_some() {}

    let (events, _) = turn(&harness, cwd, "ctx?", Some(&session)).await;
    assert_eq!(
        reply(&events).trim(),
        "MOCK:ctx=one|ctx?|ctx?",
        "and so does the next process"
    );
}

fn id_of(tree: &zeron_proto::SessionTree, text: &str) -> String {
    tree.entries
        .iter()
        .find(|e| e.text == text)
        .unwrap_or_else(|| panic!("no row {text:?} in {:?}", rows(tree)))
        .id
        .clone()
}

/// Jumps at the edges of a conversation, in a fresh Pi process per turn.
/// Ways it fails:
/// - Jumping to the very first message leaves the old turns in the model's
///   context, or leaves the session unreadable.
/// - A summarising jump away from a point that holds no conversation, only
///   the bookkeeping every new Pi process and every jump leaves behind, adds a
///   "nothing to summarise" summary to the context, or says a summary was made.
#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn real_pi_tree_jumps_to_the_first_message_and_never_summarises_nothing() {
    let (dir, harness) = isolated_pi();
    let cwd = dir.path();
    let mut session = String::new();
    for prompt in ["one", "two", "three"] {
        session = turn(
            &harness,
            cwd,
            prompt,
            (!session.is_empty()).then_some(&*session),
        )
        .await
        .1;
    }
    let tree = harness.session_tree(&session, cwd).await.unwrap().unwrap();
    let (one, two, three) = (
        id_of(&tree, "one"),
        id_of(&tree, "two"),
        id_of(&tree, "MOCK:three"),
    );

    turn(
        &harness,
        cwd,
        &format!("/zeron-tree-jump {two}"),
        Some(&session),
    )
    .await;
    let (events, _) = turn(
        &harness,
        cwd,
        &format!("/zeron-tree-jump {three} summarize"),
        Some(&session),
    )
    .await;
    let said = reply(&events);
    assert!(
        !said.contains("summary of the branch"),
        "claims a summary nobody made: {said}"
    );
    let (events, _) = turn(&harness, cwd, "ctx?", Some(&session)).await;
    assert_eq!(
        reply(&events).trim(),
        "MOCK:ctx=one|two|three|ctx?",
        "nothing was abandoned, so nothing is summarised into the context"
    );
    let tree = harness.session_tree(&session, cwd).await.unwrap().unwrap();
    assert!(
        tree.entries
            .iter()
            .all(|e| e.kind != zeron_proto::TreeEntryKind::Summary),
        "{:?}",
        rows(&tree)
    );

    let (events, _) = turn(
        &harness,
        cwd,
        &format!("/zeron-tree-jump {one}"),
        Some(&session),
    )
    .await;
    assert!(reply(&events).contains("one"), "{}", reply(&events));
    let (events, _) = turn(&harness, cwd, "ctx?", Some(&session)).await;
    assert_eq!(
        reply(&events).trim(),
        "MOCK:ctx=ctx?",
        "the first message is a rewind to an empty conversation"
    );
    let tree = harness.session_tree(&session, cwd).await.unwrap().unwrap();
    assert_eq!(
        tree.leaf_id.as_deref(),
        Some(id_of(&tree, "MOCK:ctx=ctx?").as_str())
    );
}
