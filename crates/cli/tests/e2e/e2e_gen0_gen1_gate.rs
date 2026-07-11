//! → gate test.
//!
//! Proves the bootstrap gate: `zz exec "<prompt>"` calls the LLM (mocked),
//! the LLM responds with a `write_file` tool_call, the agent loop executes
//! the tool which modifies a file on disk, then the LLM (mocked) returns a
//! final text response. Stdout is emitted as JSON lines.
//!
//! This is the minimum viable loop that demonstrates ZeroZero can act as a
//! coding agent: prompt → LLM → tool → file change → JSON output.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_gen0_gen1_gate() {
    let server = MockServer::start().await;
    let temp_dir = tempfile::tempdir().expect("create temp dir");

    // First response: tool_call for write_file with a relative path.
    let first_response = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"write_file","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"output.txt\",\"content\":\"hello world\\n\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );

    // Second response: final text after the tool result.
    let second_response = concat!(
        r#"data: {"choices":[{"delta":{"content":"File created successfully."}}]}"#,
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
        .env("ZZ_SANDBOX", "full-access")
        .current_dir(temp_dir.path())
        .args(["exec", "Create a file called output.txt with hello world"])
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

    // Assert the full event sequence of the→1 gate.
    assert!(
        types.contains(&"session.started"),
        "expected session.started in events: {:?}",
        types
    );
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
    assert!(
        types.contains(&"item.completed"),
        "expected item.completed in events: {:?}",
        types
    );
    assert!(
        types.contains(&"turn.completed"),
        "expected turn.completed in events: {:?}",
        types
    );

    // Assert the file was actually created on disk with the expected content.
    let written = std::fs::read_to_string(temp_dir.path().join("output.txt"))
        .expect("output.txt was created by write_file tool");
    assert_eq!(written, "hello world\n");

    // Verify at least 2 requests were made to the mock server.
    let received = server.received_requests().await.expect("requests");
    assert!(
        received.len() >= 2,
        "expected at least 2 requests, got {}",
        received.len()
    );

    // Second request should contain a tool result message (role "tool").
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
