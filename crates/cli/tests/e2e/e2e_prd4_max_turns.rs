//! E2E test AC-7: Max turns limit.
//!
//! Mock server always returns a tool_call, so the agent loop will
//! keep looping. With ZZ_MAX_TURNS=2, the agent should stop after
//! 2 turns and emit an error event.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_prd4_ac7_max_turns() {
    let server = MockServer::start().await;

    // Always return a tool_call — never a final text response.
    let tool_response = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"echo loop\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(tool_response.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_MAX_TURNS", "2")
        .args(["exec", "keep looping"])
        .assert()
        .get_output()
        .clone();

    // Should exit with code 1 (error).
    assert!(
        !output.status.success(),
        "expected non-zero exit code for max turns exceeded"
    );

    let s = String::from_utf8(output.stdout).expect("stdout is valid utf-8");
    let events: Vec<Value> = s
        .lines()
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .expect("every line is valid JSON");

    let types: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().expect("type field"))
        .collect();

    // Should contain an error event with "max turns" message.
    let error_event = events
        .iter()
        .find(|e| e["type"] == "error")
        .expect("error event present");
    let message = error_event["message"]
        .as_str()
        .expect("error message is string");
    assert!(
        message.contains("max turns"),
        "error message should mention max turns: {message}"
    );

    // Should NOT have turn.completed (error instead).
    assert!(
        !types.contains(&"turn.completed"),
        "should not have turn.completed when max turns exceeded"
    );
}
