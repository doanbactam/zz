//! E2E test AC-7: Streaming events in JSONL — verify sequence and
//! that item.completed.text = concatenation of all item.updated.text.

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

#[tokio::test]
async fn e2e_prd3_ac7_streaming_sequence() {
    let server = MockServer::start().await;

    let sse_body = sse_response(&["Rust", " is", " fast"]);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
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
        .args(["exec", "hello"])
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

    // Collect all item.updated deltas
    let deltas: String = events
        .iter()
        .filter(|e| e["type"] == "item.updated")
        .map(|e| e["item"]["text"].as_str().expect("delta text"))
        .collect();

    // item.completed text must equal concatenation of deltas
    let completed = events
        .iter()
        .find(|e| e["type"] == "item.completed")
        .expect("item.completed present");
    let full_text = completed["item"]["text"].as_str().expect("completed text");
    assert_eq!(
        full_text, &deltas,
        "item.completed.text = concatenation of all item.updated deltas"
    );
    assert_eq!(full_text, "Rust is fast");

    // Verify order: item.started before item.updated before item.completed
    let types: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().expect("type"))
        .collect();
    let started_idx = types
        .iter()
        .position(|&t| t == "item.started")
        .expect("item.started present");
    let first_updated_idx = types
        .iter()
        .position(|&t| t == "item.updated")
        .expect("item.updated present");
    let completed_idx = types
        .iter()
        .position(|&t| t == "item.completed")
        .expect("item.completed present");
    assert!(
        started_idx < first_updated_idx,
        "item.started before item.updated"
    );
    assert!(
        first_updated_idx < completed_idx,
        "item.updated before item.completed"
    );
}
