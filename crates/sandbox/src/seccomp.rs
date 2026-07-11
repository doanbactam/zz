//! seccomp network syscall filter (Linux only).

use anyhow::Result;

/// Set PR_SET_NO_NEW_PRIVS on the current thread.
/// Required before applying seccomp filters.
pub fn set_no_new_privs() -> Result<()> {
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        anyhow::bail!(
            "PR_SET_NO_NEW_PRIVS failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Blocked network-related syscalls.
const BLOCKED_SYSCALLS: &[i64] = &[
    libc::SYS_socket,
    libc::SYS_connect,
    libc::SYS_bind,
    libc::SYS_listen,
    libc::SYS_accept,
    libc::SYS_accept4,
    libc::SYS_sendto,
    libc::SYS_recvfrom,
    libc::SYS_sendmsg,
    libc::SYS_recvmsg,
    libc::SYS_socketpair,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
    libc::SYS_process_vm_writev,
];

/// Install seccomp filter that blocks network syscalls on the current thread.
pub fn apply_seccomp() -> Result<()> {
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
    use std::convert::TryInto;

    let mut rules: std::collections::BTreeMap<i64, Vec<seccompiler::SeccompRule>> =
        std::collections::BTreeMap::new();
    for &syscall in BLOCKED_SYSCALLS {
        rules.insert(syscall, vec![]);
    }

    let filter: BpfProgram = SeccompFilter::new(
        rules.into_iter().collect(),
        SeccompAction::Allow,    // mismatch_action: allow non-blocked syscalls
        SeccompAction::Errno(1), // match_action: return EPERM for blocked
        std::env::consts::ARCH
            .try_into()
            .map_err(|e| anyhow::anyhow!("arch conversion failed: {e:?}"))?,
    )
    .map_err(|e| anyhow::anyhow!("seccomp filter creation failed: {e}"))?
    .try_into()
    .map_err(|e| anyhow::anyhow!("seccomp filter conversion failed: {e}"))?;

    seccompiler::apply_filter(&filter).map_err(|e| anyhow::anyhow!("seccomp apply failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocked_syscalls_nonempty() {
        assert!(!BLOCKED_SYSCALLS.is_empty());
    }

    #[test]
    fn test_blocked_syscalls_contains_socket() {
        assert!(BLOCKED_SYSCALLS.contains(&libc::SYS_socket));
    }

    #[test]
    fn test_blocked_syscalls_contains_connect() {
        assert!(BLOCKED_SYSCALLS.contains(&libc::SYS_connect));
    }

    #[test]
    fn test_blocked_syscalls_contains_io_uring() {
        assert!(BLOCKED_SYSCALLS.contains(&libc::SYS_io_uring_setup));
        assert!(BLOCKED_SYSCALLS.contains(&libc::SYS_io_uring_enter));
    }
}
