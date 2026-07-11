//! E2E test: `zz rewind` subcommand .
//!
//! Tests AC-3 (list checkpoints), AC-4 (truncate to seq),
//! AC-5 (only user-message seq accepted), and nonexistent session.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use zerozero_llm::ChatMessage;
use zerozero_session::SessionStore;

fn append_msg(store: &SessionStore, sid: &str, role: &str, content: &str) {
    store
        .append_message(
            sid,
            &ChatMessage {
                role: role.to_string(),
                content: content.to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
        )
        .unwrap();
}

/// Build a session DB with a known session for testing.
fn build_db(tmp: &TempDir) -> std::path::PathBuf {
    let db_path = tmp.path().join("sessions.db");
    {
        let store = SessionStore::open(&db_path).unwrap();
        store
            .create_session("test-session", "first prompt", Some("test-model"))
            .unwrap();
        // seq 0=user, 1=assistant, 2=user, 3=assistant, 4=user
        append_msg(&store, "test-session", "user", "first prompt");
        append_msg(&store, "test-session", "assistant", "first reply");
        append_msg(&store, "test-session", "user", "second prompt");
        append_msg(&store, "test-session", "assistant", "second reply");
        append_msg(&store, "test-session", "user", "third prompt");
    }
    db_path
}

/// AC-3: `zz rewind <sid>` (no --to) lists checkpoints.
#[test]
fn e2e_prd98_rewind_list() {
    let tmp = TempDir::new().unwrap();
    let db_path = build_db(&tmp);

    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["rewind", "test-session"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Checkpoints for session test-session",
        ))
        .stdout(predicate::str::contains("first prompt"))
        .stdout(predicate::str::contains("second prompt"))
        .stdout(predicate::str::contains("third prompt"));
}

/// AC-3: nonexistent session → exit 1 with error.
#[test]
fn e2e_prd98_rewind_nonexistent_session() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("sessions.db");
    // Create DB but no sessions.
    {
        let _ = SessionStore::open(&db_path).unwrap();
    }

    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["rewind", "no-such-session"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("session not found"));
}

/// AC-4: `zz rewind <sid> --to <seq>` truncates and prints remaining count.
#[test]
fn e2e_prd98_rewind_to() {
    let tmp = TempDir::new().unwrap();
    let db_path = build_db(&tmp);

    // Truncate to seq 2 (second user prompt). Should keep seq 0,1,2 = 3 msgs.
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["rewind", "test-session", "--to", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rewound to seq 2"))
        .stdout(predicate::str::contains("3 messages remaining"));

    // Verify via DB that only 3 messages remain.
    let store = SessionStore::open(&db_path).unwrap();
    let msgs = store.get_messages("test-session").unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2].content, "second prompt");
}

/// AC-5: `--to` rejects seq that is not a user message (checkpoint).
#[test]
fn e2e_prd98_rewind_checkpoint_only() {
    let tmp = TempDir::new().unwrap();
    let db_path = build_db(&tmp);

    // seq 1 is assistant → not a checkpoint → exit 1.
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["rewind", "test-session", "--to", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid checkpoint"));

    // seq 0 is user → valid checkpoint → success.
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["rewind", "test-session", "--to", "0"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rewound to seq 0"));
}

/// AC-4 edge: `--to` with seq that doesn't exist at all → exit 1.
#[test]
fn e2e_prd98_rewind_to_invalid_seq() {
    let tmp = TempDir::new().unwrap();
    let db_path = build_db(&tmp);

    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["rewind", "test-session", "--to", "99"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid checkpoint"));
}
