//! ZeroZero eval task abstractions ().
//!
//! Provides a typed [`EvalTask`] contract plus a [`TaskSpec`] dispatch enum so the
//! eval runner can hold heterogeneous task types. The first concrete type is
//! [`SwebenchTask`] (SWE-bench Verified instance model) — see [`swebench`].

pub mod git;
pub mod swebench;

use std::path::Path;

/// Outcome of running a single eval task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    /// `true` iff every FAIL_TO_PASS test passes AND every PASS_TO_PASS test passes.
    pub resolved: bool,
    /// FAIL_TO_PASS test names that did NOT pass after the patch.
    pub failed_ftp: Vec<String>,
    /// PASS_TO_PASS test names that regressed (did not pass).
    pub failed_ptp: Vec<String>,
    /// Raw captured stdout+stderr from `test_command`.
    pub raw: String,
}

impl EvalOutcome {
    /// Build an outcome from a parsed per-test result map.
    pub fn from_results(
        fail_to_pass: &[String],
        pass_to_pass: &[String],
        results: &std::collections::HashMap<String, bool>,
        raw: String,
    ) -> Self {
        let failed_ftp: Vec<String> = fail_to_pass
            .iter()
            .filter(|t| results.get(*t) != Some(&true))
            .cloned()
            .collect();
        let failed_ptp: Vec<String> = pass_to_pass
            .iter()
            .filter(|t| results.get(*t) != Some(&true))
            .cloned()
            .collect();
        let resolved = failed_ftp.is_empty() && failed_ptp.is_empty();
        Self {
            resolved,
            failed_ftp,
            failed_ptp,
            raw,
        }
    }
}

/// Common contract for all eval task types.
pub trait EvalTask {
    /// Stable task type tag, e.g. `"swebench"`.
    fn kind(&self) -> &str;

    /// Execute the task inside `workdir` and return an outcome.
    fn run(&self, workdir: &Path) -> anyhow::Result<EvalOutcome>;
}

/// Dispatch enum so the eval runner can hold heterogeneous tasks.
pub enum TaskSpec {
    /// A SWE-bench Verified instance.
    Swebench(swebench::SwebenchTask),
}

impl TaskSpec {
    /// Run the wrapped task inside `workdir`.
    pub fn run(&self, workdir: &Path) -> anyhow::Result<EvalOutcome> {
        match self {
            Self::Swebench(t) => t.run(workdir),
        }
    }

    /// Tag of the wrapped task type.
    pub fn kind(&self) -> &str {
        match self {
            Self::Swebench(t) => t.kind(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swebench::SwebenchTask;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command as Proc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static LIB_FIXTURE_SEQ: AtomicU64 = AtomicU64::new(0);

    /// Build a local fixture git repo (no network) returning its path + head sha.
    fn build_fixture_repo() -> (PathBuf, String) {
        let seq = LIB_FIXTURE_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("zz-libspec-fixture-{}-{}", std::process::id(), seq));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("probe.sh"), "#!/bin/sh\necho ok\n").unwrap();
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
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&dir)
            .status()
            .unwrap();
        let sha = String::from_utf8(
            Proc::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&dir)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        (dir, sha)
    }

    #[test]
    fn taskspec_dispatches_swebench_happy_path() {
        // Local fixture repo: test_command emits PASS lines directly (no network).
        let (repo, base) = build_fixture_repo();
        let task = SwebenchTask {
            instance_id: "fixture__happy".into(),
            repo: repo.to_str().unwrap().to_string(),
            base_commit: base,
            patch: String::new(),
            test_patch: String::new(),
            test_command: "echo PASS:add && echo PASS:keep".into(),
            fail_to_pass: vec!["add".into()],
            pass_to_pass: vec!["keep".into()],
        };
        let spec = TaskSpec::Swebench(task);
        assert_eq!(spec.kind(), "swebench");
        let out = spec
            .run(&std::env::temp_dir().join(format!("zz-libspec-work1-{}", std::process::id())))
            .expect("run");
        assert!(out.resolved, "expected resolved true, got {out:?}");
        assert!(out.failed_ftp.is_empty());
        assert!(out.failed_ptp.is_empty());
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn taskspec_reports_unresolved_on_missing_ftp() {
        let (repo, base) = build_fixture_repo();
        let task = SwebenchTask {
            instance_id: "fixture__missing".into(),
            repo: repo.to_str().unwrap().to_string(),
            base_commit: base,
            patch: String::new(),
            test_patch: String::new(),
            // "add" never reported as PASS -> must be unresolved.
            test_command: "echo PASS:keep".into(),
            fail_to_pass: vec!["add".into()],
            pass_to_pass: vec!["keep".into()],
        };
        let spec = TaskSpec::Swebench(task);
        let out = spec
            .run(&std::env::temp_dir().join(format!("zz-libspec-work2-{}", std::process::id())))
            .expect("run");
        assert!(!out.resolved);
        assert_eq!(out.failed_ftp, vec!["add".to_string()]);
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn outcome_from_results_empty_is_resolved() {
        let mut results = HashMap::new();
        results.insert("a".to_string(), true);
        let o = EvalOutcome::from_results(&["a".into()], &[], &results, String::new());
        assert!(o.resolved);
    }
}
