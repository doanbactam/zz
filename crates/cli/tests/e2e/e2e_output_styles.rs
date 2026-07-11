//! E2E test: `zz exec --output <STYLE>` selects the rendered output style.
//!
//! These tests use a mock OpenAI-compatible SSE server (wiremock) so no
//! real network/LLM is required (project rule §7.1). The CLI binary itself
//! is exercised end-to-end; the per-style formatting is additionally
//! covered by the pure `zerozero_cli::output::render_events` unit tests.

use assert_cmd::Command;
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

/// Run `zz exec <prompt> --output <style>` against a mock LLM and return
/// stdout as a `String`.
async fn run_exec_style(style: &str) -> String {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(
                    sse_response(&["The answer is 42."]).into_bytes(),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let output = Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "--output", style, "what is the answer?"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).expect("stdout is valid utf-8")
}

#[tokio::test]
async fn e2e_exec_output_snippet_final_answer_only() {
    let s = run_exec_style("snippet").await;
    // The final answer text must appear...
    assert!(
        s.contains("The answer is 42."),
        "snippet should contain the final answer text"
    );
    // ...and no JSON framing should be present.
    assert!(
        !s.contains("session.started"),
        "snippet must not contain JSON event framing"
    );
    assert!(
        !s.contains("\"type\":"),
        "snippet must not contain JSON type fields"
    );
    assert!(
        !s.contains("EVENT "),
        "snippet must not contain parser EVENT lines"
    );
}

#[tokio::test]
async fn e2e_exec_output_parser_event_lines() {
    let s = run_exec_style("parser").await;
    assert!(
        s.contains("EVENT session.started"),
        "parser must emit session.started line"
    );
    assert!(
        s.contains("EVENT item.completed"),
        "parser must emit item.completed line"
    );
    assert!(
        s.contains("EVENT turn.completed"),
        "parser must emit turn.completed line"
    );
}

#[tokio::test]
async fn e2e_exec_output_jsonl_one_object_per_line() {
    let s = run_exec_style("jsonl").await;
    let first = s.lines().next().expect("at least one line");
    assert!(
        first.starts_with("{\"type\":"),
        "jsonl default emits compact JSON objects per line"
    );
    assert!(
        s.contains("session.started"),
        "jsonl must contain session.started event"
    );
}

#[tokio::test]
async fn e2e_exec_output_json_pretty_multiline() {
    let s = run_exec_style("json-pretty").await;
    assert!(
        s.contains("  \"type\":"),
        "json-pretty must indent with 2 spaces"
    );
}

#[tokio::test]
async fn e2e_exec_json_pretty_alias_still_works() {
    // `--json-pretty` is a backward-compatible alias for `--output json-pretty`.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_response(&["hi"]).into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let output = Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "--json-pretty", "test"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    assert!(
        s.contains("  \"type\":"),
        "--json-pretty alias must produce pretty-printed JSON"
    );
}
