//! E2E test: `zz doctor` prints local diagnostics parity with
//! Codex `doctor`). No network / LLM — pure introspection.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_doctor_prints_diagnostics() {
    // Clear provider keys so output is deterministic-ish; assert structural markers.
    Command::cargo_bin("zz")
        .unwrap()
        .env_remove("XAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ZZ_PROVIDER")
        .env_remove("ZZ_MODEL")
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zz doctor"))
        .stdout(predicate::str::contains("Provider"))
        .stdout(predicate::str::contains("Model"))
        .stdout(predicate::str::contains("API keys"))
        .stdout(predicate::str::contains("Config"))
        .stdout(predicate::str::contains("Tools"))
        .stdout(predicate::str::contains("registered"))
        .stdout(predicate::str::contains("Status"));
}
