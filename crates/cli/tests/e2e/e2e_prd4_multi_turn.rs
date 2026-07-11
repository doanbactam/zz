//! E2E test AC-5: Agent loop multi-turn with tool calls.
//!
//! Mock server returns a tool_call in the first response, then a final
//! text response after receiving the tool result. Verifies the agent
//! loop correctly executes the tool and sends the result back.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_prd4_ac5_multi_turn() {
    let server = MockServer::start().await;

    // First response: tool_call for read_file.
    let first_response = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"/etc/hostname\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );

    // Second response: final text after tool result.
    let second_response = concat!(
        r#"data: {"choices":[{"delta":{"content":"The hostname is testbox."}}]}"#,
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
        .args(["exec", "What is the hostname?"])
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

    let types: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().expect("type field"))
        .collect();

    // Should have tool.started and tool.completed events.
    assert!(
        types.contains(&"tool.started"),
        "expected tool.started in events: {:?}",
        types
    );
    assert!(
        types.contains(&"tool.completed"),
        "expected tool.completed in events: {:?}",
        types
    );

    // Should have item.completed with the final text.
    let completed = events
        .iter()
        .find(|e| e["type"] == "item.completed")
        .expect("item.completed present");
    let full_text = completed["item"]["text"]
        .as_str()
        .expect("item.text is string");
    assert!(full_text.contains("hostname"));

    // Should end with turn.completed.
    assert_eq!(types.last(), Some(&"turn.completed"));

    // Verify 2 requests were made (first with tool_call, second with tool result).
    let received = server.received_requests().await.expect("requests");
    assert!(
        received.len() >= 2,
        "expected at least 2 requests, got {}",
        received.len()
    );

    // Second request should contain a tool result message.
    let second_body: Value =
        serde_json::from_slice(&received[1].body).expect("second request body is JSON");
    let messages = second_body["messages"]
        .as_array()
        .expect("messages array in second request");
    let has_tool_result = messages.iter().any(|m| m["role"] == "tool");
    assert!(
        has_tool_result,
        "second request should contain a tool result message"
    );
}
