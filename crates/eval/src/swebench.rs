//! SWE-bench Verified task model + executor.
//!
//! Mirrors the official `SWEbenchInstance` schema so a ZeroZero eval run can
//! reproduce the harness flow: clone → checkout base → apply gold patch →
//! apply test patch → run `test_command` → verify FAIL_TO_PASS / PASS_TO_PASS.

use crate::git;
use crate::{EvalOutcome, EvalTask};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// A SWE-bench Verified instance, executable as an [`EvalTask`].
///
/// Fields map 1:1 to the dataset schema.
/// `repo` may be a `owner/name` for real network cloning, OR a local filesystem path
/// to a fixture git repo (used by tests to avoid network).
#[derive(Debug, Clone)]
pub struct SwebenchTask {
    /// Unique instance id, e.g. `django__django-12345`.
    pub instance_id: String,
    /// `owner/name` OR local path to a git repo.
    pub repo: String,
    /// Git SHA the issue was filed at.
    pub base_commit: String,
    /// Gold solution patch (unified diff text).
    pub patch: String,
    /// Test patch adding/modifying the FAIL_TO_PASS tests (unified diff text).
    pub test_patch: String,
    /// Shell command that runs the relevant test suite.
    pub test_command: String,
    /// Tests that must fail before and pass after the gold patch.
    pub fail_to_pass: Vec<String>,
    /// Tests that must pass both before and after (regression guards).
    pub pass_to_pass: Vec<String>,
}

impl EvalTask for SwebenchTask {
    fn kind(&self) -> &str {
        "swebench"
    }

    fn run(&self, workdir: &Path) -> Result<EvalOutcome> {
        let repo_dir = workdir.join("repo");
        // 1. clone / copy fixture + checkout base_commit
        git::prepare_repo(&self.repo, &self.base_commit, &repo_dir)
            .with_context(|| format!("preparing repo for {}", self.instance_id))?;
        // 2. apply gold patch
        git::apply_patch(&repo_dir, &self.patch)
            .with_context(|| format!("applying gold patch for {}", self.instance_id))?;
        // 3. apply test patch
        git::apply_patch(&repo_dir, &self.test_patch)
            .with_context(|| format!("applying test patch for {}", self.instance_id))?;
        // 4. run test command
        let (_ok, raw) = git::run_shell(&repo_dir, &self.test_command)
            .with_context(|| format!("running test_command for {}", self.instance_id))?;
        // 5. parse per-test results and build outcome
        let results = parse_test_results(&raw);
        Ok(EvalOutcome::from_results(
            &self.fail_to_pass,
            &self.pass_to_pass,
            &results,
            raw,
        ))
    }
}

/// Parse `PASS:<name>` / `FAIL:<name>` lines emitted by the test harness into a
/// name → passed map. The fixture `test_command` emits these lines; a production
/// adapter would plug in `pytest`/`cargo test` output parsers here (out of scope).
pub fn parse_test_results(output: &str) -> HashMap<String, bool> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("PASS:") {
            map.insert(name.trim().to_string(), true);
        } else if let Some(name) = line.strip_prefix("FAIL:") {
            map.insert(name.trim().to_string(), false);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskSpec;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command as Proc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_SEQ: AtomicU64 = AtomicU64::new(0);

    /// Build a local fixture git repo containing a buggy `calc.sh` and return its path.
    fn build_fixture_repo() -> PathBuf {
        let seq = FIXTURE_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "zz-swebench-fixture-{}-{}",
            std::process::id(),
            seq
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // buggy implementation: add returns wrong result
        fs::write(
            dir.join("calc.sh"),
            "#!/bin/sh\necho $(( $1 - $2 ))  # BUG: should be +\n",
        )
        .unwrap();
        Proc::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .unwrap();
        Proc::new("git")
            .args(["config", "user.email", "test@zerozero.dev"])
            .current_dir(&dir)
            .status()
            .unwrap();
        Proc::new("git")
            .args(["config", "user.name", "zz test"])
            .current_dir(&dir)
            .status()
            .unwrap();
        Proc::new("git")
            .args(["add", "."])
            .current_dir(&dir)
            .status()
            .unwrap();
        Proc::new("git")
            .args(["commit", "-q", "-m", "init buggy calc"])
            .current_dir(&dir)
            .status()
            .unwrap();
        dir
    }

    /// Gold patch that fixes `calc.sh` (turns `-` into `+`).
    fn gold_patch() -> String {
        concat!(
            "diff --git a/calc.sh b/calc.sh\n",
            "--- a/calc.sh\n",
            "+++ b/calc.sh\n",
            "@@ -1,2 +1,2 @@\n",
            " #!/bin/sh\n",
            "-echo $(( $1 - $2 ))  # BUG: should be +\n",
            "+echo $(( $1 + $2 ))\n",
        )
        .to_string()
    }

    /// Test patch that adds `run_tests.sh` emitting FAIL before / PASS after the fix.
    fn test_patch() -> String {
        concat!(
            "diff --git a/run_tests.sh b/run_tests.sh\n",
            "new file mode 100755\n",
            "--- /dev/null\n",
            "+++ b/run_tests.sh\n",
            "@@ -0,0 +1,4 @@\n",
            "+#!/bin/sh\n",
            "+res=$(sh calc.sh 2 3)\n",
            "+[ \"$res\" = \"5\" ] && echo PASS:add || echo FAIL:add\n",
            "+echo PASS:keep\n",
        )
        .to_string()
    }

    fn task(repo: &str, base_commit: &str, patch: String) -> SwebenchTask {
        SwebenchTask {
            instance_id: "fixture__calc".into(),
            repo: repo.to_string(),
            base_commit: base_commit.to_string(),
            patch,
            test_patch: test_patch(),
            test_command: "sh run_tests.sh".into(),
            fail_to_pass: vec!["add".into()],
            pass_to_pass: vec!["keep".into()],
        }
    }

    /// Resolve the HEAD sha of the fixture repo (used as base_commit).
    fn head_sha(dir: &Path) -> String {
        String::from_utf8(
            Proc::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    #[test]
    fn swebench_happy_path_local_fixture() {
        let repo = build_fixture_repo();
        let base = head_sha(&repo);
        let t = task(repo.to_str().unwrap(), &base, gold_patch());
        let work = std::env::temp_dir().join(format!(
            "zz-swebench-work-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&work);
        let out = t.run(&work).expect("run");
        assert!(out.resolved, "expected resolved=true, got {out:?}");
        assert!(out.failed_ftp.is_empty(), "ftp should pass: {out:?}");
        assert!(out.failed_ptp.is_empty(), "ptp should pass: {out:?}");
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn swebench_fail_case_no_fix() {
        let repo = build_fixture_repo();
        let base = head_sha(&repo);
        // patch empty -> bug remains -> "add" FAIL_TO_PASS stays failing.
        let t = task(repo.to_str().unwrap(), &base, String::new());
        let work = std::env::temp_dir().join(format!("zz-swebench-work2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        let out = t.run(&work).expect("run");
        assert!(
            !out.resolved,
            "expected unresolved when bug not fixed: {out:?}"
        );
        assert_eq!(out.failed_ftp, vec!["add".to_string()]);
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn swebench_resolved_via_taskspec() {
        let repo = build_fixture_repo();
        let base = head_sha(&repo);
        let spec = TaskSpec::Swebench(task(repo.to_str().unwrap(), &base, gold_patch()));
        let work = std::env::temp_dir().join(format!("zz-swebench-work3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        let out = spec.run(&work).expect("run");
        assert!(out.resolved);
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn parse_test_results_detects_pass_and_fail() {
        let m = parse_test_results("PASS:add\nFAIL:broken\nPASS:keep\n");
        assert_eq!(m.get("add"), Some(&true));
        assert_eq!(m.get("broken"), Some(&false));
        assert_eq!(m.get("keep"), Some(&true));
        assert_eq!(m.get("missing"), None);
    }
}
