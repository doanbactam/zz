//! E2E test: `zz session show <id>` renders a saved session transcript
//! as readable markdown parity with Codex `session show`).

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_session_show_renders_markdown() {
    let server = MockServer::start().await;

    // Single response: no tool calls, just text.
    let response = concat!(
        r#"data: {"choices":[{"delta":{"content":"Hello!"}}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(response.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("show_sessions.db");

    // Create a session by running an exec turn that persists to the DB.
    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["exec", "test prompt"])
        .assert()
        .success();

    assert!(db_path.exists(), "session DB file should exist");

    // List sessions to obtain the (uuid) id.
    let list_out = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list_str = String::from_utf8(list_out).expect("stdout utf-8");
    // The id is the first whitespace-delimited token of the first data row.
    let id = list_str
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('-') && !l.starts_with("ID"))
        .find(|l| l.split_whitespace().count() >= 3)
        .and_then(|l| l.split_whitespace().next())
        .expect("should parse a session id from `zz sessions`")
        .to_string();

    // `zz session show <id>` must render markdown with role sections.
    Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["session", "show", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("## user"))
        .stdout(predicate::str::contains("## assistant"))
        .stdout(predicate::str::contains("test prompt"));
}

#[tokio::test]
async fn e2e_session_export_json() {
    let server = MockServer::start().await;
    let response = concat!(
        r#"data: {"choices":[{"delta":{"content":"Hi"}}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(response.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("export_sessions.db");

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["exec", "another prompt"])
        .assert()
        .success();

    let list_out = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list_str = String::from_utf8(list_out).expect("stdout utf-8");
    let id = list_str
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('-') && !l.starts_with("ID"))
        .find(|l| l.split_whitespace().count() >= 3)
        .and_then(|l| l.split_whitespace().next())
        .expect("should parse a session id")
        .to_string();

    // `zz session export <id>` must emit valid JSON with a messages array.
    let export_out = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["session", "export", &id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let export_str = String::from_utf8(export_out).expect("stdout utf-8");
    let v: Value = serde_json::from_str(&export_str).expect("export is valid JSON");
    assert!(
        v.get("messages").is_some(),
        "export JSON should have messages"
    );
}

/// Spawn a mock OpenAI SSE server + run `zz exec` once to create a session
/// in `db_path`, returning the session id (first token of `zz sessions`).
async fn create_one_session(db_path: &std::path::Path, prompt: &str) -> String {
    let server = MockServer::start().await;
    let response = concat!(
        r#"data: {"choices":[{"delta":{"content":"Hi"}}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(response.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["exec", prompt])
        .assert()
        .success();

    let list_out = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list_str = String::from_utf8(list_out).expect("stdout utf-8");
    list_str
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('-') && !l.starts_with("ID"))
        .find(|l| l.split_whitespace().count() >= 3)
        .and_then(|l| l.split_whitespace().next())
        .expect("should parse a session id")
        .to_string()
}

#[tokio::test]
async fn e2e_session_delete_removes_session() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("delete_sessions.db");
    let id = create_one_session(&db_path, "prompt to delete").await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["session", "delete", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted session"));

    // After delete, the session must be gone (show fails / empty).
    let after = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let after_str = String::from_utf8(after).expect("stdout utf-8");
    assert!(
        !after_str.contains(&id),
        "deleted session id should not appear in list"
    );
}

#[tokio::test]
async fn e2e_session_prune_removes_all() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("prune_sessions.db");
    let _ = create_one_session(&db_path, "first").await;
    let _ = create_one_session(&db_path, "second distinct prompt").await;

    // Count sessions before prune.
    let before_out = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let before_str = String::from_utf8(before_out).expect("stdout utf-8");
    let before_count = before_str
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with('-')
                && !t.starts_with("ID")
                && !t.starts_with("No sessions")
        })
        .count();
    assert!(
        before_count >= 1,
        "expected at least 1 session, got: {before_str}"
    );

    Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["session", "prune"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned"));

    let after = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let after_str = String::from_utf8(after).expect("stdout utf-8");
    assert!(
        after_str
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with('-')
                    && !t.starts_with("ID")
                    && !t.starts_with("No sessions")
            })
            .count()
            == 0,
        "sessions list should be empty after prune, got: {after_str}"
    );
}

#[tokio::test]
async fn e2e_sessions_json_lists_session() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("json_sessions.db");
    let id = create_one_session(&db_path, "json prompt").await;

    let out = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["sessions", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out_str = String::from_utf8(out).expect("utf-8");
    let v: Value = serde_json::from_str(&out_str).expect("sessions --json is valid JSON array");
    let arr = v.as_array().expect("top-level JSON is an array");
    assert!(!arr.is_empty(), "expected at least one session");
    assert!(
        arr.iter()
            .any(|s| s.get("id").and_then(|x| x.as_str()) == Some(id.as_str())),
        "JSON array should contain the created session id"
    );
    assert!(
        arr[0].get("message_count").is_some(),
        "each entry has message_count"
    );
}
