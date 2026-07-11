//! E2E tests: restore bugfix-logic-error + refactor-error-handling
//! eval task files.

use std::path::PathBuf;
use std::process::Command;

/// Return the workspace root (repo root) by walking up from CARGO_MANIFEST_DIR.
/// CARGO_MANIFEST_DIR for this crate is crates/cli/, so repo root is 2 levels up.
fn repo_root() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    PathBuf::from(manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .expect("could not find repo root from CARGO_MANIFEST_DIR")
        .to_path_buf()
}

/// Helper: assert all required task files exist in a task directory (relative
/// to repo root).
fn assert_task_files_present(task_dir: &str) {
    let base = repo_root().join(task_dir);
    let required = [
        "task.md",
        "verify.sh",
        "scoring.md",
        "expected/src/lib.rs",
        "fixture/src/lib.rs",
        "expected/Cargo.toml",
        "fixture/Cargo.toml",
    ];
    for rel in &required {
        let p = base.join(rel);
        assert!(
            p.exists(),
            "AC fail: required file missing: {}",
            p.display()
        );
    }
}

/// Helper: run a command in the repo root, return (success, stdout).
fn run_cmd_in_repo_root(args: &[&str]) -> (bool, String) {
    let output = Command::new(args[0])
        .args(&args[1..])
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("failed to run {:?}: {}", args, e));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();
    if !success {
        eprintln!(
            "cmd {:?} exit {:?}\nstdout:\n{}\nstderr:\n{}",
            args, output.status, stdout, stderr
        );
    }
    (success, stdout)
}

#[test]
fn e2e_prd92_ac1_bugfix_logic_error_task_files_present() {
    // AC-1: eval/tasks/bugfix-logic-error/ contains the required task files.
    assert_task_files_present("eval/tasks/bugfix-logic-error");
}

#[test]
fn e2e_prd92_ac2_refactor_error_handling_task_files_present() {
    // AC-2: eval/tasks/refactor-error-handling/ contains the required task files.
    assert_task_files_present("eval/tasks/refactor-error-handling");
}

#[test]
fn e2e_prd92_ac3_zz_eval_pass() {
    // AC-3: `zz eval` exits 0 and prints "Score:" + "PASS" (structure-validation
    // mode exercises eval/run.sh which validates all eval task dirs including
    // the 2 restored ones). Uses assert_cmd::cargo_bin("zz") like e2e_eval.rs
    // to locate the built binary.
    let mut cmd = assert_cmd::Command::cargo_bin("zz").expect("zz binary not found");
    let output = cmd
        .current_dir(repo_root())
        .args(["eval"])
        .output()
        .expect("failed to run zz eval");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "AC-3 fail: zz eval did not exit 0\nstdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("Score:"),
        "AC-3 fail: zz eval output missing 'Score:'\nstdout:\n{}",
        stdout
    );
    assert!(
        stdout.contains("PASS"),
        "AC-3 fail: zz eval output missing 'PASS'\nstdout:\n{}",
        stdout
    );
}

#[test]
fn e2e_prd92_ac4_eval_run_sh_score_9_9() {
    // AC-4: `bash eval/run.sh` prints `Score: 9 / 9` and exits 0.
    let (success, stdout) = run_cmd_in_repo_root(&["bash", "eval/run.sh"]);
    assert!(
        success,
        "AC-4 fail: eval/run.sh did not exit 0\nstdout:\n{}",
        stdout
    );
    assert!(
        stdout.contains("Score: 9 / 9") || stdout.contains("Score: 9/9"),
        "AC-4 fail: score not 9/9\nstdout:\n{}",
        stdout
    );
}

#[test]
fn e2e_prd92_ac5_no_regression_fmt_clippy() {
    // AC-5: `cargo fmt --check` exits 0 AND
    // `cargo clippy --all-targets -- -D warnings` exits 0 (regression guard).
    let (fmt_ok, fmt_out) = run_cmd_in_repo_root(&["cargo", "fmt", "--check"]);
    assert!(
        fmt_ok,
        "AC-5 fail: cargo fmt --check did not exit 0\n{}",
        fmt_out
    );
    let (clippy_ok, clippy_out) =
        run_cmd_in_repo_root(&["cargo", "clippy", "--all-targets", "--", "-D", "warnings"]);
    assert!(
        clippy_ok,
        "AC-5 fail: cargo clippy --all-targets -- -D warnings did not exit 0\n{}",
        clippy_out
    );
}
