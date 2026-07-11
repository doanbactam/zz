use assert_cmd::Command;
use serde_json::Value;
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

/// Run `zz exec <prompt>` with a mock LLM server and parse stdout as JSON lines.
fn run_exec(prompt: &str, mock_uri: String) -> Vec<Value> {
    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", mock_uri)
        .env("ZZ_MODEL", "test-model")
        .args(["exec", prompt])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    s.lines()
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .expect("every line is valid JSON")
}

/// Check that a string matches the ISO 8601 UTC regex without pulling in
/// the `regex` crate. Pattern: `YYYY-MM-DDThh:mm:ssZ`.
fn is_iso8601_utc(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && s[..4].bytes().all(|c| c.is_ascii_digit())
        && s[5..7].bytes().all(|c| c.is_ascii_digit())
        && s[8..10].bytes().all(|c| c.is_ascii_digit())
        && s[11..13].bytes().all(|c| c.is_ascii_digit())
        && s[14..16].bytes().all(|c| c.is_ascii_digit())
        && s[17..19].bytes().all(|c| c.is_ascii_digit())
}

#[tokio::test]
async fn e2e_prd2_ac3_exec_jsonl_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(
                    sse_response(&["hello world"]).into_bytes(),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let events = run_exec("hello world", server.uri());
    assert!(
        events.len() >= 4,
        "expected at least 4 JSONL events, got {}",
        events.len()
    );
    let types: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().expect("type field is a string"))
        .collect();
    assert_eq!(
        types[0], "session.started",
        "first event must be session.started"
    );
    assert!(types.contains(&"prompt"), "events must include prompt");
    assert!(
        types.contains(&"item.completed"),
        "events must include item.completed"
    );
    assert_eq!(
        types.last(),
        Some(&"turn.completed"),
        "last event must be turn.completed"
    );
}

#[tokio::test]
async fn e2e_prd2_ac4_item_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(
                    sse_response(&["schema-check"]).into_bytes(),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let events = run_exec("schema-check", server.uri());
    let item_event = events
        .iter()
        .find(|e| e["type"] == "item.completed")
        .expect("item.completed event present");
    let item = &item_event["item"];
    assert_eq!(item["type"], "agent_message", "item.type == agent_message");
    let id = item["id"].as_str().expect("item.id is a string");
    assert!(
        id.starts_with("item_") && id[5..].chars().all(|c| c.is_ascii_digit()),
        "item.id matches item_<digits>, got {id}"
    );
    let text = item["text"].as_str().expect("item.text is a string");
    assert!(!text.is_empty(), "item.text is non-empty");
}

#[tokio::test]
async fn e2e_prd2_ac7_session_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(
                    sse_response(&["session-id-check"]).into_bytes(),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let events = run_exec("session-id-check", server.uri());
    let session_id = events[0]["session_id"]
        .as_str()
        .expect("session_id is a string");
    assert!(
        is_iso8601_utc(session_id),
        "session_id matches ISO 8601 UTC, got {session_id}"
    );
}
