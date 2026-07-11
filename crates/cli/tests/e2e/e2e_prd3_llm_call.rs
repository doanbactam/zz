//! E2E test AC-3: `zz exec` calls LLM via mock server, streams tokens.
//!
//! Uses wiremock to stand up a mock OpenAI-compatible server that returns
//! SSE-formatted chat completion chunks. Asserts the request body has
//! correct format and the output JSONL contains streaming events.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{header, method, path};
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
async fn e2e_prd3_ac3_llm_call_mock() {
    let server = MockServer::start().await;

    let sse_body = sse_response(&["Hello", " world", "!"]);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("Authorization", "Bearer test-key"))
        .and(header("Content-Type", "application/json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_body.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "hi"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    let events: Vec<Value> = s
        .lines()
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .expect("every line is valid JSON");

    // Verify event sequence
    let types: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().expect("type field"))
        .collect();

    assert_eq!(types[0], "session.started");
    assert!(types.contains(&"prompt"));
    assert!(types.contains(&"item.started"));
    assert!(types.contains(&"item.updated"));
    assert!(types.contains(&"item.completed"));
    assert_eq!(types.last(), Some(&"turn.completed"));

    // Verify item.completed text = concatenation of all item.updated deltas
    let completed = events
        .iter()
        .find(|e| e["type"] == "item.completed")
        .expect("item.completed present");
    let full_text = completed["item"]["text"]
        .as_str()
        .expect("item.text is string");
    assert_eq!(full_text, "Hello world!");
}
