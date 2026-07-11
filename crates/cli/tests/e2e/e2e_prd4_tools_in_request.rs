//! E2E test AC-3: Provider sends tools in request body.
//!
//! Uses wiremock to verify the request body contains a `tools` array
//! with correct function definitions.

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
async fn e2e_prd4_ac3_tools_in_request() {
    let server = MockServer::start().await;

    let sse_body = sse_response(&["OK"]);

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
        .args(["exec", "hi"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Verify the mock server received a request with tools in the body.
    let received_requests = server.received_requests().await;
    assert!(received_requests.is_some());
    let requests = received_requests.unwrap();
    assert!(!requests.is_empty());

    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body is valid JSON");

    // tools array should be present and contain the registered tool
    // definitions (6 original + repo_map + web_search + git_commit + git_push
    // + git_worktree + git_pr + apply_patch + web_fetch = 14).
    let tools = body["tools"].as_array().expect("tools array in request");
    assert_eq!(tools.len(), 14);

    let tool_names: Vec<&str> = tools
        .iter()
        .map(|t| t["function"]["name"].as_str().expect("tool name"))
        .collect();
    assert!(tool_names.contains(&"read_file"));
    assert!(tool_names.contains(&"write_file"));
    assert!(tool_names.contains(&"edit_file"));
    assert!(tool_names.contains(&"bash"));
    assert!(tool_names.contains(&"grep"));
    assert!(tool_names.contains(&"glob"));
    assert!(tool_names.contains(&"repo_map"));
    assert!(tool_names.contains(&"web_search"));
    assert!(tool_names.contains(&"git_commit"));
    assert!(tool_names.contains(&"git_push"));

    // Verify each tool has type "function" and a parameters schema.
    for tool in tools {
        assert_eq!(tool["type"], "function");
        assert!(tool["function"]["description"].is_string());
        assert!(tool["function"]["parameters"].is_object());
    }

    // Output should still be valid JSONL.
    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    assert!(!s.is_empty());
}
