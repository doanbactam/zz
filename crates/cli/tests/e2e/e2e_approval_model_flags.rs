//! E2E test: `zz exec --approval` and `--model` CLI flags.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{body_partial_json, method, path};
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
async fn e2e_exec_with_approval_flag() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_response(&["hello"]).into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "--approval", "never", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session.started"));
}

#[tokio::test]
async fn e2e_exec_with_model_flag() {
    let server = MockServer::start().await;
    // Verify the model name is sent in the request body
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"model": "custom-model-123"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_response(&["hello"]).into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .args(["exec", "--model", "custom-model-123", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session.started"));
}
