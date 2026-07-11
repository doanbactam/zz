//! E2E test: `zz exec --continue` flag.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse_response(chunks: &[&str]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
            chunk
        ));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

#[tokio::test]
async fn e2e_exec_continue_nonexistent_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_response(&["hello"]).into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    // Continue a non-existent session — should still work (start fresh).
    // No ZZ_SESSION_DB is set, so --continue gracefully degrades to a
    // fresh turn (with a warning on stderr).
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "--continue", "nonexistent-session-id", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session.started"));
}

#[tokio::test]
async fn e2e_exec_continue_with_session_db() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_response(&["world"]).into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("sessions.db");

    // First, create a session in the DB with some prior messages.
    {
        let store = zerozero_session::SessionStore::open(&db_path).unwrap();
        store
            .create_session("prev-session", "previous prompt", Some("test-model"))
            .unwrap();
        store
            .append_message(
                "prev-session",
                &zerozero_llm::ChatMessage {
                    role: "user".to_string(),
                    content: "previous prompt".to_string(),
                    tool_call_id: None,
                    tool_calls: None,
                    attachments: None,
                    thinking_signature: None,
                    redacted_thinking: None,
                    thinking: None,
                },
            )
            .unwrap();
        store
            .append_message(
                "prev-session",
                &zerozero_llm::ChatMessage {
                    role: "assistant".to_string(),
                    content: "previous answer".to_string(),
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

    // Now continue that session with a new prompt.
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["exec", "--continue", "prev-session", "follow up"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session.started"));
}
