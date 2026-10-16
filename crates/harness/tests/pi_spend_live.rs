//! Real native Pi with a local priced provider: Pi's footer usage line and its
//! cache-miss notices reach the transcript as notices. No network or API spend.
//!
//! Writes the turns' transcript text to `$CARGO_TARGET_TMPDIR/pi-spend-transcript.md`.
#![cfg(unix)]
use futures::StreamExt;
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::sync::{mpsc, oneshot};
use zeron_harness::{CancellationToken, Harness, PiHarness, RunControls, SteerMessage};
use zeron_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel};

fn isolated_pi() -> (tempfile::TempDir, PiHarness) {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(agent.join("extensions")).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"retry":{"enabled":false},"showCacheMissNotices":true}"#,
    )
    .unwrap();
    std::fs::write(
        agent.join("extensions/spend.ts"),
        include_str!("fixtures/pi-spend-probe.ts"),
    )
    .unwrap();
    let exe = PiHarness::new()
        .resolve_executable()
        .expect("Pi CLI installed");
    let quote =
        |p: &std::path::Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let wrapper = dir.path().join("pi-spend");
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
        .with_session_store(dir.path().join("index"));
    (dir, harness)
}

/// Runs `prompt`, then each of `follow_ups` as a new turn on the same live
/// process once the previous one is done. Returns each turn's transcript text.
async fn run(
    harness: &PiHarness,
    cwd: &std::path::Path,
    session: &mut Option<String>,
    prompt: &str,
    follow_ups: &[&str],
) -> Vec<String> {
    run_turns(
        harness,
        cwd,
        session,
        prompt,
        follow_ups,
        "zeron-spend/priced",
    )
    .await
}

async fn run_model(
    harness: &PiHarness,
    cwd: &std::path::Path,
    session: &mut Option<String>,
    prompt: &str,
    model: &str,
) -> Vec<String> {
    run_turns(harness, cwd, session, prompt, &[], model).await
}

async fn run_turns(
    harness: &PiHarness,
    cwd: &std::path::Path,
    session: &mut Option<String>,
    prompt: &str,
    follow_ups: &[&str],
    model: &str,
) -> Vec<String> {
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
        prompt: prompt.into(),
        harness: None,
        model: Some(model.into()),
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
    let mut stream = harness.run(request, controls).await.unwrap();
    let mut follow_ups = follow_ups.iter();
    let mut steer = Some(steer);
    let mut turns = vec![String::new()];
    tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                AgentEvent::SessionStarted { session_id, .. } => *session = Some(session_id),
                AgentEvent::TextDelta { text } => turns.last_mut().unwrap().push_str(&text),
                AgentEvent::Notice { tone, text } => {
                    let line = format!("\n[{tone:?}] {text}");
                    turns.last_mut().unwrap().push_str(&line);
                }
                AgentEvent::Done { status, error, .. } => {
                    assert_eq!(status, DoneStatus::Completed, "{error:?}");
                    match follow_ups.next() {
                        Some(next) => {
                            turns.push(String::new());
                            steer
                                .as_ref()
                                .unwrap()
                                .send(SteerMessage {
                                    prompt: (*next).into(),
                                    message_id: None,
                                    attachments: vec![],
                                    config: None,
                                })
                                .await
                                .unwrap();
                        }
                        None => drop(steer.take()),
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("native Pi run must settle");
    turns
}

#[tokio::test]
#[ignore = "requires Pi installed; uses only a local priced mock provider"]
async fn real_pi_reports_turn_cost_and_cache_misses() {
    let (dir, harness) = isolated_pi();
    let cwd = dir.path();
    let mut session = None;
    let mut transcript = vec![];
    for (prompt, follow_ups) in [("warm", &[][..]), ("hit", &[]), ("miss", &["idle"])] {
        let turns = run(&harness, cwd, &mut session, prompt, follow_ups).await;
        let prompts = std::iter::once(prompt).chain(follow_ups.iter().copied());
        transcript.extend(prompts.zip(turns));
    }
    let report = transcript
        .iter()
        .map(|(prompt, text)| format!("## > {prompt}\n{text}\n"))
        .collect::<String>();
    let artifact = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("pi-spend-transcript.md");
    std::fs::write(&artifact, &report).unwrap();
    eprintln!("{report}\nartifact: {}", artifact.display());

    let text = |prompt: &str| &transcript.iter().find(|(p, _)| *p == prompt).unwrap().1;
    assert_eq!(
        text("warm"),
        "MOCK:warm\n[Dim] ↑200 ↓500 · W30k · cache 0% · $0.121 turn · $0.121 session"
    );
    assert_eq!(
        text("hit"),
        "MOCK:hit\n[Dim] ↑200 ↓500 · R30k · cache 99% · $0.017 turn · $0.138 session"
    );
    assert_eq!(
        text("miss"),
        "MOCK:miss\n[Warning] Cache miss: 30k tokens re-billed (~$0.08)\n[Dim] ↑30k ↓500 · $0.098 turn · $0.236 session"
    );
    assert_eq!(
        text("idle"),
        "MOCK:idle\n[Warning] Cache miss after 7m idle: 30k tokens re-billed (~$0.10)\n[Dim] ↑200 ↓500 · W30k · cache 0% · $0.121 turn · $0.356 session"
    );
}

/// Opt-in: `PI_SPEND_REAL_MODEL=anthropic/claude-sonnet-5-5` runs two turns
/// against your real Pi provider (your own auth; extensions, skills and MCP
/// off; sessions in a temp dir). The second turn must read Pi's system prompt
/// from the provider's cache.
#[tokio::test]
#[ignore = "requires Pi installed, PI_SPEND_REAL_MODEL, and provider auth; spends a few cents"]
async fn real_provider_reports_cache_reads() {
    let Ok(model) = std::env::var("PI_SPEND_REAL_MODEL") else {
        panic!(
            "set PI_SPEND_REAL_MODEL, e.g. anthropic/claude-sonnet-5-5 (Haiku caches only prompts over 4k tokens)"
        );
    };
    let dir = tempfile::tempdir().unwrap();
    let exe = PiHarness::new()
        .resolve_executable()
        .expect("Pi CLI installed");
    let wrapper = dir.path().join("pi-real");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{}' --no-extensions --no-skills --no-mcp --no-context-files --session-dir '{}' \"$@\"\n",
            exe.display(),
            dir.path().join("sessions").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let harness = PiHarness::new()
        .with_executable(wrapper)
        .with_session_store(dir.path().join("index"));
    let mut session = None;
    let mut transcript = vec![];
    for prompt in [
        "Reply with just the word one.",
        "Reply with just the word two.",
    ] {
        let mut turns = run_model(&harness, dir.path(), &mut session, prompt, &model).await;
        transcript.push((prompt, turns.remove(0)));
    }
    let report = transcript
        .iter()
        .map(|(prompt, text)| format!("## > {prompt}\n{text}\n"))
        .collect::<String>();
    let artifact =
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("pi-spend-real-transcript.md");
    std::fs::write(&artifact, &report).unwrap();
    eprintln!("{report}\nartifact: {}", artifact.display());
    let second = &transcript[1].1;
    assert!(
        second.contains(" · R"),
        "second turn reads the cache: {second}"
    );
    assert!(second.contains(" session"), "{second}");
}
