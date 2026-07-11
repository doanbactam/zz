use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_prd1_ac3_version_flag() {
    Command::cargo_bin("zz")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("zz "));
}
