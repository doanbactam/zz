//! E2E test AC-5: API key missing → friendly error on stderr, exit 1.
//!
//! Default provider is xAI (Grok), so the error mentions XAI_API_KEY.
//! OPENAI_API_KEY is accepted as fallback for xAI — if it is set, no
//! error is produced.
//!
//! Tests run in a temp dir to avoid picking up a .env file from the
//! project root (dotenvy loads .env from CWD).

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_prd3_ac5_no_api_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Point auth store at a non-existent path so a real ~/.config/zerozero/auth.json
    // on the developer machine cannot leak a key into this test.
    let auth = tmp.path().join("no-auth.json");
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("ZZ_AUTH_PATH", &auth)
        .env_remove("XAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .args(["exec", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("XAI_API_KEY"));
}

#[test]
fn e2e_prd3_ac5_empty_api_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let auth = tmp.path().join("no-auth.json");
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("ZZ_AUTH_PATH", &auth)
        .env("XAI_API_KEY", "")
        .env_remove("OPENAI_API_KEY")
        .args(["exec", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("XAI_API_KEY"));
}
