//! Pi's own session file, read the way `/tree` shows it. The fixture is a real
//! Pi session (two branches, one left with a branch summary) plus a tool turn,
//! a label, a compaction, and a final line cut off mid-write.
//! Ways it fails:
//! - Tool calls, tool results, labels, model changes or the cut-off line show
//!   up as rows, or break the structure.
//! - The fork is not indented, or the branch holding the leaf is not first.
//! - The leaf is the last complete entry, not the last row Pi shows.
//! - A label does not land on the entry it names.
//! - Whitespace in a reply is not collapsed into a one-line preview.
use zeron_harness::{Harness, PiHarness};
use zeron_proto::TreeEntryKind::{Assistant as A, Compaction as C, Summary as S, User as U};

#[tokio::test]
async fn a_real_session_file_becomes_the_tree_pi_shows() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let sessions = agent.join("sessions/--tmp-work--");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("2026-10-09_session.jsonl"),
        include_str!("fixtures/pi-session-branched.jsonl"),
    )
    .unwrap();
    let harness = PiHarness::new()
        .with_agent_dir(&agent)
        .with_session_store(dir.path().join("index"));

    let tree = harness
        .session_tree("01a11f22-8fe9-727d-968f-8be0e0fdd65e", dir.path())
        .await
        .unwrap()
        .expect("Pi sessions are trees");

    let rows: Vec<_> = tree
        .entries
        .iter()
        .map(|e| {
            (
                e.id.as_str(),
                e.kind,
                e.depth,
                e.on_path,
                e.label.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("28722ff0", U, 0, true, None),
            ("55cbdcf5", A, 0, true, None),
            ("3fcea550", U, 1, true, None),
            ("e0f580a1", A, 1, true, None),
            ("a9b9e93d", U, 1, true, None),
            ("e1b304f1", A, 1, true, Some("good-answer")),
            ("05cdb4be", S, 1, true, None),
            ("x1", U, 1, true, None),
            ("x4", A, 1, true, None),
            ("x6", C, 1, true, None),
            ("e094b7b8", U, 1, false, None),
            ("1ca603b8", A, 1, false, None),
            ("c9e06553", U, 1, false, None),
            ("cab93af3", A, 1, false, None),
        ]
    );
    assert_eq!(tree.leaf_id.as_deref(), Some("x6"));
    let text = |id: &str| {
        tree.entries
            .iter()
            .find(|e| e.id == id)
            .unwrap()
            .text
            .as_str()
    };
    assert_eq!(text("28722ff0"), "first question alpha");
    assert_eq!(text("x4"), "Done: listed files");
    assert!(
        text("05cdb4be").starts_with(
            "The user explored a different conversation branch before returning here. Summary of that exploration:"
        ),
        "{}",
        text("05cdb4be")
    );
}
