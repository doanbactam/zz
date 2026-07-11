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
async fn e2e_prd2_ac5_stdin_prompt_dash() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(
                    sse_response(&["response"]).into_bytes(),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "-"])
        .write_stdin("from stdin")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    let prompt_event: Value = s
        .lines()
        .map(serde_json::from_str::<Value>)
        .filter_map(Result::ok)
        .find(|v| v["type"] == "prompt")
        .expect("prompt event present");
    assert_eq!(
        prompt_event["text"].as_str().unwrap(),
        "from stdin",
        "prompt text comes from stdin (no trailing newline from write_stdin)"
    );
}

#[tokio::test]
async fn e2e_prd2_ac5_stdin_prompt_omitted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(
                    sse_response(&["response"]).into_bytes(),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec"])
        .write_stdin("no args prompt")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    assert!(
        s.contains(r#""text":"no args prompt""#),
        "JSONL contains the stdin prompt text"
    );
}
