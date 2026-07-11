//! E2E test: `zz exec --tee-file <file>` writes rendered output to file.

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

#[tokio::test]
async fn e2e_exec_output_writes_to_file() {
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
    let output_path = tmp.path().join("output.jsonl");

    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "--tee-file", "output.jsonl", "test"])
        .assert()
        .success();

    // Verify the file was created and contains JSONL
    assert!(output_path.exists(), "Output file should exist");
    let content = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        content.contains("session.started"),
        "Output file should contain session.started event"
    );
}

#[tokio::test]
async fn e2e_exec_output_and_stdout() {
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
    let output_path = tmp.path().join("out.jsonl");

    let output = Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "--tee-file", "out.jsonl", "test"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Both stdout and file should have content
    let stdout_str = String::from_utf8(output).unwrap();
    assert!(
        stdout_str.contains("session.started"),
        "stdout should have events"
    );
    assert!(output_path.exists(), "File should exist");
    let file_content = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        file_content.contains("session.started"),
        "File should have events"
    );
}
