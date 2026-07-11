use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_prd2_ac2_exec_help() {
    Command::cargo_bin("zz")
        .unwrap()
        .args(["exec", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("exec"))
        .stdout(predicate::str::contains("PROMPT"));
}
