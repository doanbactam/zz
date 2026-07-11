//! Git + shell helpers for eval tasks ().
//!
//! All helpers are filesystem/local only so unit tests need **no network**:
//! if `repo` is an existing local path with a `.git` directory it is copied
//! into `dest` instead of being network-cloned. This lets `SwebenchTask` tests
//! use a fixture git repo built in a tempdir.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-unique counter so parallel test threads don't share temp-file names.
static PATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Prepare `repo` at `base_commit` into `dest`.
///
/// - If `repo` is a local path that already contains a `.git` directory, copy it
///   (recursively, following the worktree) into `dest` and check out `base_commit`.
/// - Otherwise attempt a real `git clone` of `repo` into `dest` then checkout.
///
/// Returns the path to the prepared repo directory (`dest`).
pub fn prepare_repo(repo: &str, base_commit: &str, dest: &Path) -> Result<PathBuf> {
    if dest.exists() {
        bail!("destination {dest:?} already exists", dest = dest);
    }
    let local = Path::new(repo);
    if local.join(".git").is_dir() {
        // Local fixture repo: copy it (avoids any network).
        copy_dir_recursive(local, dest)
            .with_context(|| format!("copying fixture repo {repo:?} -> {dest:?}"))?;
    } else {
        let status = Command::new("git")
            .args(["clone", repo, dest.to_str().unwrap()])
            .status()
            .with_context(|| format!("spawning git clone {repo:?}"))?;
        if !status.success() {
            bail!("git clone failed for {repo:?}");
        }
    }
    checkout(dest, base_commit)?;
    Ok(dest.to_path_buf())
}

/// `git checkout <commit>` in `repo_dir`.
pub fn checkout(repo_dir: &Path, commit: &str) -> Result<()> {
    let status = Command::new("git")
        .args(["checkout", "--force", commit])
        .current_dir(repo_dir)
        .status()
        .with_context(|| format!("spawning git checkout in {repo_dir:?}"))?;
    if !status.success() {
        bail!("git checkout {commit:?} failed in {repo_dir:?}");
    }
    Ok(())
}

/// Write `diff` to a temp file and `git apply` it in `repo_dir`.
pub fn apply_patch(repo_dir: &Path, diff: &str) -> Result<()> {
    if diff.trim().is_empty() {
        return Ok(()); // no-op patch (e.g. fail-case test omits the fix)
    }
    let seq = PATCH_SEQ.fetch_add(1, Ordering::SeqCst);
    let tmp =
        std::env::temp_dir().join(format!("zz-eval-patch-{}-{}.diff", std::process::id(), seq));
    std::fs::write(&tmp, diff).with_context(|| format!("writing patch to {tmp:?}"))?;
    let status = Command::new("git")
        .args(["apply", tmp.to_str().unwrap()])
        .current_dir(repo_dir)
        .status()
        .with_context(|| format!("spawning git apply in {repo_dir:?}"))?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        bail!("git apply failed in {repo_dir:?}");
    }
    Ok(())
}

/// Run a shell `command` in `repo_dir`. Returns `(exit_ok, combined_output)`.
pub fn run_shell(repo_dir: &Path, command: &str) -> Result<(bool, String)> {
    let out = Command::new("sh")
        .args(["-c", command])
        .current_dir(repo_dir)
        .output()
        .with_context(|| format!("spawning shell command in {repo_dir:?}"))?;
    let mut raw = String::from_utf8_lossy(&out.stdout).into_owned();
    raw.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), raw))
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {to:?}"))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {from:?}"))? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).with_context(|| format!("copying {src:?} -> {dst:?}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Proc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static GIT_FIXTURE_SEQ: AtomicU64 = AtomicU64::new(0);

    fn init_fixture() -> (PathBuf, String) {
        // Create a throwaway git repo, commit once, return its path + a commit sha.
        let seq = GIT_FIXTURE_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("zz-eval-gitfix-{}-{}", std::process::id(), seq));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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
        std::fs::write(dir.join("f.txt"), "v1").unwrap();
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
    fn prepare_repo_rejects_bad_commit() {
        let (dir, _sha) = init_fixture();
        let dest = std::env::temp_dir().join(format!("zz-eval-dest-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let res = prepare_repo(dir.to_str().unwrap(), "deadbeefdeadbeef", &dest);
        assert!(
            res.is_err(),
            "unknown base_commit must error, not silently succeed"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn prepare_repo_copies_local_fixture() {
        let (dir, sha) = init_fixture();
        let dest = std::env::temp_dir().join(format!("zz-eval-dest-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let out = prepare_repo(dir.to_str().unwrap(), &sha, &dest);
        assert!(out.is_ok(), "local fixture clone should succeed: {out:?}");
        assert!(dest.join("f.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dest);
    }
}
