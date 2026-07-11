//! E2E test AC-6: LLM HTTP error → error event on stdout, exit 1.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_prd3_ac6_llm_http_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "x"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    let error_event: Value = s
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "error")
        .expect("error event present in JSONL output");
    assert!(
        error_event["message"]
            .as_str()
            .expect("error message is string")
            .contains("500"),
        "error message contains HTTP status"
    );
}
