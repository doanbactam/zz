use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_prd1_ac2_help_flag() {
    Command::cargo_bin("zz")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("zz"));
}
