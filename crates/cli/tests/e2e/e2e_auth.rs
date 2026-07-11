//! E2E: `zz auth` login / list / logout and key injection into exec.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn zz_clean() -> Command {
    let mut cmd = Command::cargo_bin("zz").unwrap();
    // Clear ambient ZZ_*/API keys but keep OS networking vars (Windows
    // Winsock breaks with a bare env_clear → os error 10106).
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default());
    for key in [
        "SYSTEMROOT",
        "SystemRoot",
        "WINDIR",
        "windir",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "HOME",
    ] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
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

#[test]
fn e2e_auth_login_list_logout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let auth = tmp.path().join("auth.json");

    zz_clean()
        .env("ZZ_AUTH_PATH", &auth)
        .args(["auth", "login", "xai", "--key", "xai-test-secret"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved API key"));

    assert!(auth.exists(), "auth.json should be created");
    let raw = std::fs::read_to_string(&auth).unwrap();
    assert!(raw.contains("xai-test-secret"));
    assert!(raw.contains("\"type\"") || raw.contains("api"));

    zz_clean()
        .env("ZZ_AUTH_PATH", &auth)
        .args(["auth", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("xai"))
        .stdout(predicate::str::contains("auth.json"));

    zz_clean()
        .env("ZZ_AUTH_PATH", &auth)
        .args(["auth", "logout", "xai"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed"));

    let raw = std::fs::read_to_string(&auth).unwrap();
    assert!(!raw.contains("xai-test-secret"));
}

#[tokio::test]
async fn e2e_auth_store_used_when_env_missing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_response(&["ok"]).into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let auth = tmp.path().join("auth.json");

    // Persist key via auth store only (no XAI_API_KEY env).
    zz_clean()
        .env("ZZ_AUTH_PATH", &auth)
        .args(["auth", "login", "xai", "--key", "from-auth-store"])
        .assert()
        .success();

    zz_clean()
        .current_dir(tmp.path())
        .env("ZZ_AUTH_PATH", &auth)
        .env("XAI_BASE_URL", server.uri())
        .env_remove("XAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .args(["exec", "--model", "test-model", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session.started"));
}

#[test]
fn e2e_auth_unknown_provider() {
    let tmp = tempfile::TempDir::new().unwrap();
    let auth = tmp.path().join("auth.json");
    zz_clean()
        .env("ZZ_AUTH_PATH", &auth)
        .args(["auth", "login", "not-a-provider", "--key", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown provider"));
}
