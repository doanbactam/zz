//! Landlock filesystem restrictions (Linux only).

use crate::SandboxPolicy;
use anyhow::Result;

/// Apply Landlock filesystem rules on the current thread.
pub fn apply_landlock(policy: &SandboxPolicy) -> Result<()> {
    use landlock::{
        ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr,
    };

    let abi = ABI::V1;
    let access_all = AccessFs::from_all(abi);
    let access_read = AccessFs::from_read(abi);
    let access_write = AccessFs::from_write(abi);

    let writable = crate::writable_roots(policy);

    let mut created = Ruleset::default().handle_access(access_all)?.create()?;

    // Allow read access to the entire filesystem.
    let root_fd = PathFd::new("/")?;
    created = created.add_rule(PathBeneath::new(root_fd, access_read))?;

    // Allow write access only to writable roots.
    for root in &writable {
        let fd = PathFd::new(root)?;
        created = created.add_rule(PathBeneath::new(fd, access_write))?;
    }

    created.restrict_self()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_writable_roots_read_only() {
        let p = SandboxPolicy::ReadOnly;
        let roots = crate::writable_roots(&p);
        assert!(roots.contains(&PathBuf::from("/dev/null")));
    }

    #[test]
    fn test_writable_roots_workspace_write() {
        let p = SandboxPolicy::WorkspaceWrite {
            workspace_dir: PathBuf::from("/tmp"),
        };
        let roots = crate::writable_roots(&p);
        assert!(roots.contains(&PathBuf::from("/tmp")));
    }
}
