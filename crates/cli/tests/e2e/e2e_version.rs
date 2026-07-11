//! E2E test: `zz version` and `zz --version` both work.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_version_subcommand() {
    Command::cargo_bin("zz")
        .unwrap()
        .args(["version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zerozero-cli"))
        .stdout(predicate::str::contains("v0.2.0"))
        .stdout(predicate::str::contains("Providers:"));
}

#[test]
fn e2e_version_flag() {
    Command::cargo_bin("zz")
        .unwrap()
        .args(["--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zz"));
}
