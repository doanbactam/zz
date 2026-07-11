//! E2E tests for hook events  — AC-5 + AC-6.
//!
//! AC-5: `zz exec` loads hooks from `.zerozero/hooks.toml` (cwd) and wires
//! them into run_turn. Mock server receives POST when a hook event fires.
//! No config file → NoopHooks fallback → mock receives 0 requests.
//!
//! AC-6: PreToolUse response `{"decision":"block"}` blocks tool execution.
//! Empty 200 → tool executes normally.
//!
//! Uses `assert_cmd` + `wiremock` + `tempfile`. The `zz exec` subprocess runs
//! in a temp dir (current_dir) so `.zerozero/hooks.toml` is discovered there.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build an SSE response that returns a tool_call for `read_file` on the first
/// LLM call, then a plain text completion on the second.
fn sse_with_tool_call() -> (String, String) {
    let first = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"file_path\":\"README.md\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();

    let second = concat!(
        r#"data: {"choices":[{"delta":{"content":"Done."}}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();

    (first, second)
}

/// Build a simple SSE response with just text content (no tool calls).
fn sse_text_only(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\ndata: [DONE]\n\n",
        text
    )
}

/// AC-5: config file with a PostToolUse hook → mock server receives POST
/// when the read_file tool executes.
#[tokio::test]
async fn e2e_prd99_hook_config_load() {
    let server = MockServer::start().await;

    // Hook endpoint — receives PostToolUse event.
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // LLM mock — returns a tool_call for read_file, then a text completion.
    let (first, second) = sse_with_tool_call();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(first.as_bytes(), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(second.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    // Create temp dir with .zerozero/hooks.toml + a README.md to read.
    let tmp = TempDir::new().unwrap();
    let dotzero = tmp.path().join(".zerozero");
    fs::create_dir_all(&dotzero).unwrap();
    let hook_url = format!("{}/hook", server.uri());
    let config = format!(
        r#"
[[hooks.PostToolUse]]
matcher = "read_file"
url = "{hook_url}"
timeout = 5
"#
    );
    fs::write(dotzero.join("hooks.toml"), config).unwrap();
    // Create a README.md so read_file tool succeeds.
    fs::write(tmp.path().join("README.md"), "# Test\nhello\n").unwrap();

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .current_dir(tmp.path())
        .args(["exec", "read README.md"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Verify the turn completed.
    let s = String::from_utf8(output).expect("stdout valid utf-8");
    assert!(
        s.contains("turn.completed"),
        "output should contain turn.completed: {s}"
    );

    // Verify the hook endpoint received at least 1 POST (PostToolUse event).
    let requests = server.received_requests().await.unwrap();
    let hook_posts: Vec<_> = requests
        .iter()
        .filter(|r| r.method == "POST" && r.url.path() == "/hook")
        .collect();
    assert!(
        !hook_posts.is_empty(),
        "hook endpoint should receive at least 1 POST (PostToolUse event)"
    );
    // Verify the POST body contains hook_event_name = "PostToolUse".
    let body: Value = serde_json::from_slice(&hook_posts[0].body).unwrap();
    assert_eq!(body["hook_event_name"], "PostToolUse");
    assert_eq!(body["tool_name"], "read_file");
}

/// AC-5: no config file → NoopHooks fallback → hook endpoint receives 0 POST.
#[tokio::test]
async fn e2e_prd99_hook_config_no_file_noop() {
    let server = MockServer::start().await;

    // Hook endpoint — should receive 0 requests (no config file).
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // LLM mock — text only, no tool calls.
    let sse = sse_text_only("hello");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    // Temp dir with NO .zerozero/hooks.toml.
    let tmp = TempDir::new().unwrap();

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .current_dir(tmp.path())
        .args(["exec", "hi"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout valid utf-8");
    assert!(s.contains("turn.completed"), "turn should complete: {s}");

    // Hook endpoint should receive 0 POST requests (NoopHooks fallback).
    let requests = server.received_requests().await.unwrap();
    let hook_posts: Vec<_> = requests
        .iter()
        .filter(|r| r.method == "POST" && r.url.path() == "/hook")
        .collect();
    assert!(
        hook_posts.is_empty(),
        "hook endpoint should receive 0 POST when no config file (NoopHooks fallback)"
    );
}

/// AC-6: PreToolUse hook returns `{"decision":"block","reason":"no bash"}` →
/// tool is blocked, output contains the reason.
#[tokio::test]
async fn e2e_prd99_pre_tool_block() {
    let server = MockServer::start().await;

    // PreToolUse hook endpoint — returns block decision.
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "decision": "block",
            "reason": "no bash allowed"
        })))
        .mount(&server)
        .await;

    // LLM mock — returns a tool_call for bash, then a text completion.
    let first = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"echo hi\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let second = sse_text_only("blocked");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(first.as_bytes(), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(second.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    // Temp dir with PreToolUse hook config matching "bash".
    let tmp = TempDir::new().unwrap();
    let dotzero = tmp.path().join(".zerozero");
    fs::create_dir_all(&dotzero).unwrap();
    let hook_url = format!("{}/hook", server.uri());
    let config = format!(
        r#"
[[hooks.PreToolUse]]
matcher = "bash"
url = "{hook_url}"
timeout = 5
"#
    );
    fs::write(dotzero.join("hooks.toml"), config).unwrap();

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .current_dir(tmp.path())
        .args(["exec", "run echo hi"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout valid utf-8");
    // The tool should be blocked — output contains the abort reason.
    assert!(
        s.contains("no bash allowed"),
        "output should contain block reason 'no bash allowed': {s}"
    );
    // Turn should still complete (block doesn't abort the whole turn).
    assert!(
        s.contains("turn.completed"),
        "turn should still complete after block: {s}"
    );
}

/// AC-6: PreToolUse hook returns empty 200 → tool executes normally.
#[tokio::test]
async fn e2e_prd99_pre_tool_allow() {
    let server = MockServer::start().await;

    // PreToolUse hook endpoint — returns empty 200 (allow).
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // LLM mock — returns a tool_call for bash, then a text completion.
    let first = concat!(
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"echo hi\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let second = sse_text_only("done");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(first.as_bytes(), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(second.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    // Temp dir with PreToolUse hook config matching "bash".
    let tmp = TempDir::new().unwrap();
    let dotzero = tmp.path().join(".zerozero");
    fs::create_dir_all(&dotzero).unwrap();
    let hook_url = format!("{}/hook", server.uri());
    let config = format!(
        r#"
[[hooks.PreToolUse]]
matcher = "bash"
url = "{hook_url}"
timeout = 5
"#
    );
    fs::write(dotzero.join("hooks.toml"), config).unwrap();

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .current_dir(tmp.path())
        .args(["exec", "run echo hi"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout valid utf-8");
    // Tool should execute — output should NOT contain block reason.
    assert!(
        !s.contains("aborted by hook"),
        "tool should not be aborted when hook returns empty 200: {s}"
    );
    // Turn should complete.
    assert!(s.contains("turn.completed"), "turn should complete: {s}");
    // Hook endpoint should have received a POST (PreToolUse event).
    let requests = server.received_requests().await.unwrap();
    let hook_posts: Vec<_> = requests
        .iter()
        .filter(|r| r.method == "POST" && r.url.path() == "/hook")
        .collect();
    assert!(
        !hook_posts.is_empty(),
        "hook endpoint should receive POST (PreToolUse event)"
    );
    let body: Value = serde_json::from_slice(&hook_posts[0].body).unwrap();
    assert_eq!(body["hook_event_name"], "PreToolUse");
    assert_eq!(body["tool_name"], "bash");
}
