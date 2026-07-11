//! Sandbox + approval gate for ZeroZero .
//!
//! Two layers:
//! - **Sandbox**: Landlock (filesystem) + seccomp (network) on Linux.
//! - **Approval**: classify command danger level, prompt before execution.

mod approval;

#[cfg(target_os = "linux")]
mod landlock;

#[cfg(target_os = "linux")]
mod seccomp;

pub mod network;

pub use approval::{
    ApprovalAction, ApprovalPolicy, DangerLevel, RejectHook, RejectInfo, RejectReason,
    classify_command, decide, full_access_warn, should_approve,
};
pub use network::{NetPolicy, build_net_command, bwrap_available, userns_enabled};

use std::path::PathBuf;

/// Sandbox policy controlling filesystem + network access.
#[derive(Debug, Clone)]
pub enum SandboxPolicy {
    /// Read-only: no write access anywhere (except /dev/null).
    ReadOnly,
    /// Workspace write: read everywhere, write only workspace_dir + /tmp + /dev/null.
    WorkspaceWrite { workspace_dir: PathBuf },
    /// Full access: no restrictions.
    FullAccess,
}

/// Apply sandbox restrictions to the current thread.
///
/// Call before spawning a child process so only the child inherits restrictions.
/// On non-Linux: no-op.
/// On Linux: PR_SET_NO_NEW_PRIVS → Landlock (filesystem) → seccomp (network).
pub fn apply_sandbox(policy: &SandboxPolicy) -> anyhow::Result<()> {
    if matches!(policy, SandboxPolicy::FullAccess) {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        seccomp::set_no_new_privs()?;
        landlock::apply_landlock(policy)?;
        seccomp::apply_seccomp()?;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = policy;
    }

    Ok(())
}

/// Return the list of writable roots for the given policy.
pub fn writable_roots(policy: &SandboxPolicy) -> Vec<PathBuf> {
    match policy {
        SandboxPolicy::ReadOnly => vec![PathBuf::from("/dev/null")],
        SandboxPolicy::WorkspaceWrite { workspace_dir } => {
            vec![
                workspace_dir.clone(),
                PathBuf::from("/tmp"),
                PathBuf::from("/dev/null"),
            ]
        }
        SandboxPolicy::FullAccess => vec![],
    }
}

/// Validate that a write path is allowed under the sandbox policy.
/// Userspace check — defense-in-depth for in-process file tools.
/// Does NOT replace kernel sandbox (Landlock) for child processes.
pub fn validate_write_path(policy: &SandboxPolicy, path: &std::path::Path) -> anyhow::Result<()> {
    match policy {
        SandboxPolicy::ReadOnly => {
            anyhow::bail!("write denied: sandbox is read-only")
        }
        SandboxPolicy::FullAccess => Ok(()),
        SandboxPolicy::WorkspaceWrite { .. } => {
            // Canonicalize path. If path doesn't exist yet (new file),
            // canonicalize parent and rejoin filename.
            let canonical = match path.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    let parent = path
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("write denied: no parent directory"))?;
                    let parent_canonical = parent.canonicalize().map_err(|e| {
                        anyhow::anyhow!("write denied: cannot canonicalize parent: {e}")
                    })?;
                    let filename = path
                        .file_name()
                        .ok_or_else(|| anyhow::anyhow!("write denied: invalid filename"))?;
                    parent_canonical.join(filename)
                }
            };
            for root in writable_roots(policy) {
                let root_canonical = root.canonicalize().unwrap_or(root);
                if canonical.starts_with(&root_canonical) {
                    return Ok(());
                }
            }
            anyhow::bail!("write denied: path {:?} outside writable roots", canonical)
        }
    }
}

/// Build a pre_exec closure that applies sandbox restrictions in the
/// child process after fork, before exec. Linux only.
///
/// The parent (tokio runtime) is never restricted — only the child
/// inherits Landlock + seccomp rules. Call via:
/// ```ignore
/// let hook = make_pre_exec_hook(sandbox.clone());
/// unsafe { cmd.pre_exec(hook); }
/// ```
#[cfg(target_os = "linux")]
pub fn make_pre_exec_hook(
    policy: std::sync::Arc<SandboxPolicy>,
) -> impl FnMut() -> std::io::Result<()> + Send + Sync + 'static {
    move || {
        apply_sandbox(&policy)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_construction() {
        let p = SandboxPolicy::ReadOnly;
        assert!(matches!(p, SandboxPolicy::ReadOnly));

        let p = SandboxPolicy::WorkspaceWrite {
            workspace_dir: PathBuf::from("/workspace"),
        };
        assert!(matches!(p, SandboxPolicy::WorkspaceWrite { .. }));

        let p = SandboxPolicy::FullAccess;
        assert!(matches!(p, SandboxPolicy::FullAccess));
    }

    #[test]
    fn test_writable_roots_read_only() {
        let p = SandboxPolicy::ReadOnly;
        let roots = writable_roots(&p);
        assert!(roots.contains(&PathBuf::from("/dev/null")));
    }

    #[test]
    fn test_writable_roots_workspace_write() {
        let p = SandboxPolicy::WorkspaceWrite {
            workspace_dir: PathBuf::from("/myworkspace"),
        };
        let roots = writable_roots(&p);
        assert!(roots.contains(&PathBuf::from("/myworkspace")));
        assert!(roots.contains(&PathBuf::from("/tmp")));
        assert!(roots.contains(&PathBuf::from("/dev/null")));
    }

    #[test]
    fn test_writable_roots_full_access() {
        let p = SandboxPolicy::FullAccess;
        let roots = writable_roots(&p);
        assert!(roots.is_empty());
    }

    #[test]
    fn test_apply_sandbox_full_access_noop() {
        let p = SandboxPolicy::FullAccess;
        apply_sandbox(&p).expect("full access should be no-op");
    }

    // --- validate_write_path tests  ---

    #[test]
    fn test_validate_write_path_readonly_deny() {
        let p = SandboxPolicy::ReadOnly;
        let path = std::path::Path::new("/tmp/whatever");
        let result = validate_write_path(&p, path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("read-only"));
    }

    #[test]
    fn test_validate_write_path_full_access_allow() {
        let p = SandboxPolicy::FullAccess;
        let path = std::path::Path::new("/anywhere/file.txt");
        validate_write_path(&p, path).expect("full access should allow");
    }

    #[test]
    fn test_validate_write_path_workspace_allow_tmp() {
        let p = SandboxPolicy::WorkspaceWrite {
            workspace_dir: PathBuf::from("/nonexistent_workspace"),
        };
        // /tmp is always in writable_roots for WorkspaceWrite.
        let path = std::path::Path::new("/tmp/zerozero_test_validate.txt");
        // Path may not exist yet — validate_write_path canonicalizes parent.
        // /tmp exists, so this should pass.
        validate_write_path(&p, path).expect("/tmp should be writable");
    }

    #[test]
    fn test_validate_write_path_workspace_deny_outside() {
        let p = SandboxPolicy::WorkspaceWrite {
            workspace_dir: PathBuf::from("/tmp/zerozero_fake_workspace_12345"),
        };
        // /etc is NOT in writable roots (unless workspace_dir is /etc).
        let path = std::path::Path::new("/etc/zerozero_sandbox_block_test");
        let result = validate_write_path(&p, path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("write denied"));
    }

    #[test]
    fn test_validate_write_path_workspace_new_file_in_workspace() {
        let dir = std::env::temp_dir().join("zerozero_validate_test_dir");
        let _ = std::fs::create_dir_all(&dir);
        let p = SandboxPolicy::WorkspaceWrite {
            workspace_dir: dir.clone(),
        };
        // New file that doesn't exist yet — parent (dir) canonicalizes OK.
        let path = dir.join("newfile.txt");
        validate_write_path(&p, &path).expect("new file in workspace should be allowed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
