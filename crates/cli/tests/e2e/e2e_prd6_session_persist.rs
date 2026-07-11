use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_prd6_ac10_session_persist() {
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

    // Use temp dir for session DB.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("test_sessions.db");

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["exec", "test prompt"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Verify events include session.started.
    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    let events: Vec<Value> = s
        .lines()
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .expect("every line is valid JSON");

    let has_session_started = events.iter().any(|e| e["type"] == "session.started");
    assert!(has_session_started, "should have session.started event");

    // Verify SQLite DB was created and has the session.
    assert!(db_path.exists(), "session DB file should exist");

    let store = zerozero_session::SessionStore::open(&db_path).unwrap();
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1, "should have 1 session");
    assert_eq!(sessions[0].prompt, "test prompt");
    assert!(
        sessions[0].message_count >= 2,
        "should have at least 2 messages (user + assistant), got {}",
        sessions[0].message_count
    );

    // Verify messages can be retrieved.
    let messages = store.get_messages(&sessions[0].id).unwrap();
    assert!(messages.len() >= 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content, "test prompt");
}
