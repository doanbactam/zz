//! E2E test: `zz exec --dry-run` shows config without calling API.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_exec_dry_run_no_api_call() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Note: no API key set, no mock server — dry run should NOT need them
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_MODEL", "test-model")
        .args(["exec", "--dry-run", "test prompt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"))
        .stdout(predicate::str::contains("Provider:"))
        .stdout(predicate::str::contains("Model:"))
        .stdout(predicate::str::contains("no API call was made"));
}

#[test]
fn e2e_exec_dry_run_shows_sandbox() {
    let tmp = tempfile::TempDir::new().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .args(["exec", "--dry-run", "--sandbox", "full-access", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Sandbox:"))
        .stdout(predicate::str::contains("full-access"));
}
