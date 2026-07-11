//! E2E test: `zz exec --include <file>` injects file contents into the
//! prompt context parity with Codex `--include`). Real exec path
//! + mock LLM; asserts the transcript/export carry the included file text.

use assert_cmd::Command;
use serde_json::Value;
use std::io::Write;
use tempfile::NamedTempFile;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_exec_include_injects_file_context() {
    let server = MockServer::start().await;
    let response = concat!(
        r#"data: {"choices":[{"delta":{"content":"ok"}}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(response.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let mut cf = NamedTempFile::new().unwrap();
    write!(cf, "const PI = 3.14;").unwrap();
    let ctx_path = cf.path().to_str().unwrap().to_string();

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("include_sessions.db");

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["exec", "review the constant", "--include", &ctx_path])
        .assert()
        .success();

    let sessions = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let sessions_str = String::from_utf8(sessions).expect("utf-8");
    let id = sessions_str
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with('-')
                && !t.starts_with("ID")
                && !t.starts_with("No sessions")
        })
        .find(|l| l.split_whitespace().count() >= 3)
        .and_then(|l| l.split_whitespace().next())
        .expect("session id parsed")
        .to_string();

    let shown = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["session", "show", &id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let shown_str = String::from_utf8(shown).expect("utf-8");
    assert!(
        shown_str.contains("const PI = 3.14;"),
        "transcript should contain the included file text, got: {shown_str}"
    );
    assert!(
        shown_str.contains("## Included files"),
        "transcript should contain the Included files section, got: {shown_str}"
    );

    let exported = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .args(["session", "export", &id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&exported).expect("export is valid JSON");
    let blob = v.to_string();
    assert!(
        blob.contains("const PI = 3.14;"),
        "export JSON should contain the included file text"
    );
}
