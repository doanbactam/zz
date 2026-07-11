//! E2E test: `zz init` creates project config directories.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn e2e_init_creates_dirs() {
    let tmp = TempDir::new().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .args(["init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ZeroZero project initialized!"));

    // Verify Grok-style skill/command layout was created
    assert!(tmp.path().join(".zerozero").exists());
    assert!(tmp.path().join(".zerozero/skills").exists());
    assert!(
        tmp.path()
            .join(".zerozero/skills/example/SKILL.md")
            .exists()
    );
    assert!(tmp.path().join(".zerozero/commands").exists());
    assert!(tmp.path().join(".zerozero/commands/hello.md").exists());
    assert!(tmp.path().join(".zerozero/plugins.toml").exists());
    assert!(tmp.path().join(".env.example").exists());
}

#[test]
fn e2e_init_idempotent() {
    let tmp = TempDir::new().unwrap();
    // Run init twice — second time should show "Exists" not create new
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .args(["init"])
        .assert()
        .success();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .args(["init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Exists:"));
}
