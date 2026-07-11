//! E2E test: `zz exec --provider` CLI flag.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a zz binary command with a clean environment so ambient `ZZ_*`
/// vars from the test runner's shell cannot leak into the subprocess and
/// break provider/model resolution (e.g. a stray `ZZ_MODEL=bogus` makes the
/// mock's `body_partial_json(model: gpt-4o-mini)` matcher 404). The test
/// then sets only the vars it actually needs.
fn zz_clean() -> Command {
    let mut cmd = Command::cargo_bin("zz").unwrap();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd
}

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
async fn e2e_exec_with_provider_flag() {
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
    // Use --provider openai to override default xai
    zz_clean()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.uri())
        .args([
            "exec",
            "--provider",
            "openai",
            "--model",
            "test-model",
            "test",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("session.started"));
}

#[tokio::test]
async fn e2e_exec_provider_flag_overrides_env() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"model": "gpt-4o-mini"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_response(&["hello"]).into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    // Set ZZ_PROVIDER=xai in env, but override with --provider openai
    // The model should be gpt-4o-mini (OpenAI default) not grok-4 (xAI default)
    zz_clean()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "xai")
        .env("OPENAI_BASE_URL", server.uri())
        .args(["exec", "--provider", "openai", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session.started"));
}
