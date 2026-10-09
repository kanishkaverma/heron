//! Pi's cache notices reach Zeron. Real Pi in RPC mode, a local provider whose
//! usage, cost and cache lifetime the test controls (fixtures/pi-cache-probe.ts),
//! no network and no spend.
//!
//! Ways it can fail:
//! - A cache miss after a long idle is billed but never reported (the first
//!   request of a resumed session has no in-process history to compare with).
//! - The miss is reported with Pi's wrong label (idle vs model switch), token
//!   count or cost.
//! - A warm cache, or a miss under Pi's noise floor, is reported as a miss.
//! - `showCacheMissNotices` (Pi's own setting) is ignored in either direction.
//! - A refresh Pi paid for during a long turn is not reported, or is reported
//!   as assistant prose instead of a notice.
//! - Idle refreshes (Pi keeps a warm session's cache alive between turns and
//!   bills it) pile up unseen because the engine drops events while parked.
//! - Idle refreshes run when no run holds the session open.
//!
//! Run: `cargo test -p zeron-harness --test pi_cache_notices -- --ignored --test-threads=1`
//! Artifact: `$ZERON_E2E_ARTIFACT_DIR` or `$TMPDIR/zeron-pi-cache-e2e/`, one
//! event log per scenario plus `idle-warming.txt` with the lifetime finding.
#![cfg(unix)]
use futures::{StreamExt, stream::BoxStream};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use zeron_harness::{
    CancellationToken, Harness, HarnessError, PiHarness, RunControls, SteerMessage,
};
use zeron_proto::{AgentEvent, DoneStatus, NoticeTone, RunRequest, SandboxLevel};

const NOTICES_ON: &str = r#"{"retry":{"enabled":false},"showCacheMissNotices":true}"#;
const NOTICES_OFF: &str = r#"{"retry":{"enabled":false}}"#;
const NOTICES_IDLE_WARMING: &str =
    r#"{"retry":{"enabled":false},"showCacheMissNotices":true,"cacheWarming":"idle"}"#;

fn isolated_pi(settings: &str) -> (tempfile::TempDir, PiHarness) {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let agent = cwd.join("agent");
    std::fs::create_dir_all(agent.join("extensions")).unwrap();
    std::fs::write(agent.join("settings.json"), settings).unwrap();
    std::fs::write(
        agent.join("extensions/cache.ts"),
        include_str!("fixtures/pi-cache-probe.ts"),
    )
    .unwrap();
    let exe = PiHarness::new()
        .resolve_executable()
        .expect("Pi CLI installed");
    let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
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

type Events = BoxStream<'static, Result<AgentEvent, HarnessError>>;

/// One engine-held run: the stream plus the steering mailbox that keeps the
/// Pi process alive between turns, exactly as the engine's RunHandle does.
struct Live {
    events: Events,
    mailbox: Option<mpsc::Sender<SteerMessage>>,
    session: Option<String>,
}

impl Live {
    async fn start(harness: &PiHarness, cwd: &Path, prompt: &str, resume: Option<String>) -> Self {
        let (mailbox, steering) = mpsc::channel(8);
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
            model: Some("zeron-cache/cached".into()),
            reasoning: None,
            model_options: Default::default(),
            cwd: cwd.display().to_string(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            resume,
            attachments: vec![],
            worktree: None,
            mcp: None,
        };
        Self {
            events: harness.run(request, controls).await.unwrap(),
            mailbox: Some(mailbox),
            session: None,
        }
    }

    async fn until_done(&mut self) -> Vec<AgentEvent> {
        let mut seen = vec![];
        tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(event) = self.events.next().await {
                let event = event.unwrap();
                if let AgentEvent::SessionStarted { session_id, .. } = &event {
                    self.session = Some(session_id.clone());
                }
                let done = matches!(&event, AgentEvent::Done { status, error, .. } if {
                    assert_eq!(*status, DoneStatus::Completed, "{error:?}");
                    true
                });
                seen.push(event);
                if done {
                    return;
                }
            }
            panic!("run ended before Done: {seen:?}");
        })
        .await
        .expect("Pi turn must settle");
        seen
    }

    async fn say(&self, prompt: &str) {
        self.mailbox
            .as_ref()
            .unwrap()
            .send(SteerMessage::text(prompt))
            .await
            .unwrap();
    }
}

/// A turn in a process that exits when it settles (nothing holds the mailbox).
async fn one_shot(
    harness: &PiHarness,
    cwd: &Path,
    prompt: &str,
    resume: Option<String>,
) -> (Vec<AgentEvent>, String) {
    let mut live = Live::start(harness, cwd, prompt, resume).await;
    live.mailbox.take();
    let events = live.until_done().await;
    (events, live.session.take().expect("SessionStarted"))
}

fn notices(events: &[AgentEvent]) -> Vec<(NoticeTone, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Notice { tone, text } => Some((*tone, text.clone())),
            _ => None,
        })
        .collect()
}

fn session_files(agent: &Path) -> Vec<PathBuf> {
    let mut files = vec![];
    let mut dirs = vec![agent.join("sessions")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                files.push(path);
            }
        }
    }
    files
}

fn session_entries(agent: &Path, session: &str) -> (PathBuf, Vec<serde_json::Value>) {
    session_files(agent)
        .into_iter()
        .find_map(|file| {
            let entries: Vec<serde_json::Value> = std::fs::read_to_string(&file)
                .ok()?
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            (entries.first()?["id"] == session).then_some((file, entries))
        })
        .expect("session file")
}

/// Moves every assistant message back in time, as if the user walked away.
fn age_session(agent: &Path, session: &str, by: Duration) {
    let (file, mut entries) = session_entries(agent, session);
    for entry in &mut entries {
        if let Some(ts) = entry["message"]["timestamp"].as_i64() {
            entry["message"]["timestamp"] = (ts - by.as_millis() as i64).into();
        }
    }
    let lines: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
    std::fs::write(file, lines.join("\n") + "\n").unwrap();
}

fn warm_entries(agent: &Path, session: &str) -> usize {
    session_entries(agent, session)
        .1
        .iter()
        .filter(|e| e["type"] == "usage" && e["kind"] == "cache_warm")
        .count()
}

fn warm_calls(cwd: &Path) -> usize {
    std::fs::read_to_string(cwd.join("probe-warm-calls.jsonl"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn artifact_dir() -> PathBuf {
    let dir = std::env::var_os("ZERON_E2E_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("zeron-pi-cache-e2e"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(name: &str, events: &[AgentEvent]) {
    let log: String = events.iter().map(|e| format!("{e:?}\n")).collect();
    std::fs::write(artifact_dir().join(format!("{name}.txt")), log).unwrap();
}

/// A warm session resumed after the user walked away for 12 minutes.
async fn resumed_after_idle(settings: &str, second_prompt: &str, name: &str) -> Vec<AgentEvent> {
    let (dir, harness) = isolated_pi(settings);
    let cwd = dir.path();
    let (_, session) = one_shot(&harness, cwd, "usage 100 80000 0", None).await;
    age_session(&cwd.join("agent"), &session, Duration::from_secs(12 * 60));
    let (events, _) = one_shot(&harness, cwd, second_prompt, Some(session)).await;
    record(name, &events);
    events
}

#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn a_cache_miss_after_idle_is_noticed_per_pis_own_setting() {
    // The whole 80100-token prompt is re-billed: 80100 x ($3 - $0.30)/M.
    let miss = resumed_after_idle(NOTICES_ON, "usage 80100 0 0", "miss-notices-on").await;
    assert_eq!(
        notices(&miss),
        vec![(
            NoticeTone::Warning,
            "Cache miss after 12m idle: 80k tokens re-billed (~$0.22)".to_string()
        )]
    );
    let silenced = resumed_after_idle(NOTICES_OFF, "usage 80100 0 0", "miss-notices-off").await;
    assert_eq!(notices(&silenced), vec![]);
    let hit = resumed_after_idle(NOTICES_ON, "usage 200 80100 0", "hit-notices-on").await;
    assert_eq!(notices(&hit), vec![]);
}

#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn a_cache_refresh_paid_for_during_a_long_turn_is_noticed() {
    let (dir, harness) = isolated_pi(NOTICES_ON);
    let cwd = dir.path();
    let (_, session) = one_shot(&harness, cwd, "usage 100 80000 0", None).await;
    let (events, _) = one_shot(
        &harness,
        cwd,
        "usage 100 80000 0 slow",
        Some(session.clone()),
    )
    .await;
    record("refresh-during-turn", &events);
    let refreshes = warm_entries(&cwd.join("agent"), &session);
    assert!(refreshes >= 1, "Pi never refreshed during the slow turn");
    // 80100 cached tokens x $0.30/M + 1 output token x $15/M.
    assert_eq!(
        notices(&events),
        vec![(NoticeTone::Dim, "Cache warmed: $0.024045".to_string()); refreshes]
    );
}

#[tokio::test]
#[ignore = "requires Pi >= 0.85.1 installed; uses only a local mock provider"]
async fn idle_refreshes_run_only_while_a_run_holds_the_session_and_surface_with_the_next_turn() {
    // Nothing holds the mailbox: the process exits at settle, so Pi never refreshes.
    let (dir, harness) = isolated_pi(NOTICES_IDLE_WARMING);
    let cwd = dir.path();
    one_shot(&harness, cwd, "usage 100 500000 0", None).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let closed = warm_calls(cwd);

    // The engine's persistent session keeps the mailbox open between turns.
    let (dir, harness) = isolated_pi(NOTICES_IDLE_WARMING);
    let cwd = dir.path();
    let agent = cwd.join("agent");
    let mut live = Live::start(&harness, cwd, "usage 100 500000 0", None).await;
    let first = live.until_done().await;
    let session = live.session.clone().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while warm_calls(cwd) == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("idle refresh never fired while the mailbox was open");
    let held = warm_calls(cwd);
    std::fs::write(
        artifact_dir().join("idle-warming.txt"),
        format!(
            "cacheWarming=idle, 500100-token prompt, 11s cache lifetime\n\
             mailbox closed after settle: {closed} idle refresh requests in 3s\n\
             mailbox held open after settle: {held}+ idle refresh requests\n"
        ),
    )
    .unwrap();
    assert_eq!(closed, 0, "a closed run still refreshed the cache");

    let before = warm_entries(&agent, &session);
    live.say("usage 100 500000 0").await;
    let second = live.until_done().await;
    let after = warm_entries(&agent, &session);
    record("idle-first-turn", &first);
    record("idle-second-turn", &second);
    assert!(before >= 1, "no refresh was persisted while idle");

    let reported = notices(&second);
    assert!(
        (before..=after).contains(&reported.len()),
        "{before}..={after} refreshes persisted, {} reported: {second:?}",
        reported.len()
    );
    assert!(
        reported
            .iter()
            .all(|n| n == &(NoticeTone::Dim, "Cache warmed: $0.150045".to_string())),
        "{reported:?}"
    );
    let position = |pred: &dyn Fn(&AgentEvent) -> bool| second.iter().position(|e| pred(e));
    let steered = position(&|e| matches!(e, AgentEvent::Steered { .. })).expect("Steered");
    let notice = position(&|e| matches!(e, AgentEvent::Notice { .. })).expect("Notice");
    let text = position(&|e| matches!(e, AgentEvent::TextDelta { .. })).expect("reply text");
    assert!(
        steered < notice && notice < text,
        "idle refreshes belong at the head of the next turn: {second:?}"
    );
    assert!(notices(&first).is_empty());
}
