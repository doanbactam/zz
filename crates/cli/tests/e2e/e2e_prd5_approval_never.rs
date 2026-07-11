use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_prd5_ac7_approval_never_allows_dangerous() {
    let server = MockServer::start().await;

    // First response: tool_call for bash with dangerous command.
    let first_response = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"rm -rf /tmp/zz_test_dir\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );

    // Second response: final text after tool result.
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
        .env("ZZ_APPROVAL", "never")
        .args(["exec", "Delete temp files"])
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

    // With ZZ_APPROVAL=never, there should be NO approval.requested events.
    let has_approval = events.iter().any(|e| e["type"] == "approval.requested");
    assert!(
        !has_approval,
        "approval.requested should NOT appear when ZZ_APPROVAL=never"
    );

    // Should have tool.completed with actual command output (not denied).
    let tool_completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed")
        .expect("tool.completed present");
    let result = tool_completed["result"].as_str().expect("result is string");
    assert!(
        !result.contains("denied"),
        "command should not be denied under 'never' policy: {result}"
    );

    // Should end with turn.completed.
    let types: Vec<&str> = events
        .iter()
        .map(|e| e["type"].as_str().expect("type field"))
        .collect();
    assert_eq!(types.last(), Some(&"turn.completed"));
}
