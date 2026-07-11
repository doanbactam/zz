//! E2E test AC-6: Tool events in JSONL output.
//!
//! Mock server returns a tool_call, verifies tool.started and
//! tool.completed events have correct schema in JSONL output.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_prd4_ac6_tool_events() {
    let server = MockServer::start().await;

    // First response: tool_call for bash.
    let first_response = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"echo test\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );

    // Second response: final text.
    let second_response = concat!(
        r#"data: {"choices":[{"delta":{"content":"Done."}}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(first_response.as_bytes(), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(second_response.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "run echo test"])
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

    // Find tool.started event.
    let tool_started = events
        .iter()
        .find(|e| e["type"] == "tool.started")
        .expect("tool.started event present");
    assert_eq!(tool_started["tool_call_id"], "call_1");
    assert_eq!(tool_started["tool_name"], "bash");
    assert!(tool_started["args"]["command"].is_string());

    // Find tool.completed event.
    let tool_completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed")
        .expect("tool.completed event present");
    assert_eq!(tool_completed["tool_call_id"], "call_1");
    assert_eq!(tool_completed["tool_name"], "bash");
    assert!(tool_completed["result"].is_string());
    // bash echo test should produce "test" in the result.
    let result = tool_completed["result"].as_str().expect("result is string");
    assert!(result.contains("test"));
}
