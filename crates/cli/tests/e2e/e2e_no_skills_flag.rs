//! E2E test: `zz exec --no-skills` flag skips skill loading.

use assert_cmd::Command;
use predicates::prelude::*;
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
async fn e2e_exec_no_skills_flag() {
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
        .args(["exec", "--no-skills", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session.started"));
}
