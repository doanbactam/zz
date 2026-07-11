use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_prd6_ac12_compaction_triggers() {
    let server = MockServer::start().await;

    // First response: two tool calls for bash to generate enough messages.
    let first_response = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"echo hello\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_2","function":{"name":"bash","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"command\":\"echo world\"}"}}]}}]}"#,
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
        .env("ZZ_MAX_MESSAGES", "2") // Very low threshold to trigger compaction
        .env("ZZ_KEEP_RECENT", "1") // Keep only 1 recent message
        .args(["exec", "do something"])
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

    // Should have compaction.started event.
    let has_compaction_started = events.iter().any(|e| e["type"] == "compaction.started");
    assert!(
        has_compaction_started,
        "should have compaction.started event"
    );

    // Should have compaction.completed event.
    let has_compaction_completed = events.iter().any(|e| e["type"] == "compaction.completed");
    assert!(
        has_compaction_completed,
        "should have compaction.completed event"
    );

    // Verify compaction events have correct fields.
    let started = events
        .iter()
        .find(|e| e["type"] == "compaction.started")
        .expect("compaction.started present");
    let completed = events
        .iter()
        .find(|e| e["type"] == "compaction.completed")
        .expect("compaction.completed present");

    assert!(
        started["before_messages"].as_u64().is_some(),
        "before_messages should be a number"
    );
    assert!(
        completed["after_messages"].as_u64().is_some(),
        "after_messages should be a number"
    );
    assert!(
        started["before_tokens"].as_u64().is_some(),
        "before_tokens should be a number"
    );
    assert!(
        completed["after_tokens"].as_u64().is_some(),
        "after_tokens should be a number"
    );
}
