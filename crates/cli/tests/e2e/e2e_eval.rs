//! E2E test: `zz eval` runs the eval suite in structure-validation mode.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_eval_structure_validation() {
    // Run from the project root where eval/ directory exists.
    let project_root = std::env::current_dir().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(&project_root)
        .args(["eval"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Score:"))
        .stdout(predicate::str::contains("PASS"));
}

#[test]
fn e2e_eval_single_task() {
    let project_root = std::env::current_dir().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(&project_root)
        .args(["eval", "fix-typo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fix-typo"))
        .stdout(predicate::str::contains("PASS"));
}
