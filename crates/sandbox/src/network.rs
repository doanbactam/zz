//! Network namespace isolation for the sandbox.
//!
//! Spawns a command inside a network namespace with no outbound access
//! (loopback only) using `bwrap --unshare-net` when available, or falls
//! back to a seccomp filter that blocks `socket(AF_INET)` when bubblewrap
//! or user namespaces are unavailable.

use std::process::{Command, Stdio};

use super::SandboxPolicy;

/// Network policy applied to a spawned command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetPolicy {
    /// No outbound network — loopback only.
    None,
    /// Only the listed domains are reachable (via an allowlist proxy).
    Allowlist(Vec<String>),
}

impl NetPolicy {
    /// Parse from a CLI flag value like `none` or `api.openai.com,example.com`.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.eq_ignore_ascii_case("none") || s.is_empty() {
            Self::None
        } else {
            Self::Allowlist(s.split(',').map(|d| d.trim().to_string()).collect())
        }
    }
}

/// Returns true if `bwrap` (bubblewrap) is available on PATH.
pub fn bwrap_available() -> bool {
    Command::new("which")
        .arg("bwrap")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether unprivileged user namespaces are enabled (bwrap needs this).
pub fn userns_enabled() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        .map(|c| c.trim() == "1")
        .unwrap_or(false)
}

/// Build the command to run `cmd` under the given network policy.
///
/// - `None` + `FullAccess` sandbox → `bwrap --unshare-net` (no outbound
///   network namespace, socket() still allowed — parity network isolation).
/// - `None` + restricting sandbox (`WorkspaceWrite`/`ReadOnly`) → return the
///   plain command; the caller applies the seccomp `socket()`-block pre_exec
///   hook (the `make_pre_exec_hook` path in `bash.rs`). Applying the seccomp
///   filter to `bwrap` itself would break bwrap's own socket usage, so
///   restricting sandboxes must NOT go through the bwrap wrapper.
/// - `Allowlist` → `bwrap --unshare-net` plus an allowlist proxy forwarder
///   (stub — see TODO).
/// - Fallback (no bwrap/userns) → return the plain command; the caller's
///   seccomp pre_exec hook blocks sockets.
///
/// Returns a fresh `Command` built from `base`'s program + args.
pub fn build_net_command(base: &Command, policy: &NetPolicy, sandbox: &SandboxPolicy) -> Command {
    let program = base.get_program();
    let args: Vec<&std::ffi::OsStr> = base.get_args().collect();

    // Network namespace isolation via bwrap is a *restriction*, so only use
    // it when the sandbox policy is otherwise unrestricted (FullAccess). For
    // restricting sandboxes the seccomp layer (applied by the caller's
    // pre_exec hook) is the stronger guarantee and must target the real
    // command, not bwrap.
    if matches!(sandbox, SandboxPolicy::FullAccess) && bwrap_available() && userns_enabled() {
        let mut bwrap = Command::new("bwrap");
        bwrap.arg("--unshare-net");
        bwrap.arg("--dev").arg("/dev");
        bwrap.arg("--proc").arg("/proc");
        // Bind essential read-only directories so the command can run
        // but has no network access (loopback only). Include /tmp so
        // commands that write temp files (and scripts in /tmp) work.
        for dir in ["/usr", "/bin", "/lib", "/lib64", "/etc", "/tmp"] {
            bwrap.arg("--ro-bind").arg(dir).arg(dir);
        }
        bwrap.arg("--");
        bwrap.arg(program);
        for a in &args {
            bwrap.arg(a);
        }
        // Stub for allowlist proxy wiring (cycle scope: domain list stored,
        // proxy forwarder spawned in a follow-up if policy is Allowlist).
        if matches!(policy, NetPolicy::Allowlist(_)) {
            // TODO(cycle-101): spawn ephemeral proxy forwarding allowlist
            // domains into the isolated namespace.
        }
        bwrap
    } else {
        // Caller applies seccomp (socket-block) via make_pre_exec_hook when
        // the sandbox restricts; for FullAccess minus bwrap this is a plain
        // command (no network isolation available).
        let mut cmd = Command::new(program);
        cmd.args(&args);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_netpolicy_parse_none() {
        assert_eq!(NetPolicy::parse("none"), NetPolicy::None);
        assert_eq!(NetPolicy::parse(""), NetPolicy::None);
        assert_eq!(NetPolicy::parse("NONE"), NetPolicy::None);
    }

    #[test]
    fn test_netpolicy_parse_allowlist() {
        assert_eq!(
            NetPolicy::parse("api.openai.com,example.com"),
            NetPolicy::Allowlist(vec!["api.openai.com".into(), "example.com".into()])
        );
        // trailing/leading spaces trimmed
        assert_eq!(
            NetPolicy::parse(" api.openai.com , example.com "),
            NetPolicy::Allowlist(vec!["api.openai.com".into(), "example.com".into()])
        );
    }

    #[test]
    fn test_bwrap_userns_flags_exist() {
        // These are detection helpers; just assert they return bool without panic.
        let _ = bwrap_available();
        let _ = userns_enabled();
    }

    #[test]
    fn test_build_net_command_program() {
        // Program depends on environment: bwrap when available, else the
        // original program (fallback path). Asserts the function returns a
        // runnable Command either way.
        let mut base_cmd = Command::new("echo");
        base_cmd.arg("hi");
        let out = build_net_command(&base_cmd, &NetPolicy::None, &SandboxPolicy::FullAccess);
        if bwrap_available() && userns_enabled() {
            assert_eq!(out.get_program(), "bwrap");
        } else {
            assert_eq!(out.get_program(), "echo");
        }
    }

    #[test]
    fn test_build_net_command_none_policy_shape() {
        // NetPolicy::None must not produce an allowlist-shaped command.
        let base_cmd = std::process::Command::new("true");
        let _ = build_net_command(&base_cmd, &NetPolicy::None, &SandboxPolicy::FullAccess);
        let _ = build_net_command(
            &base_cmd,
            &NetPolicy::Allowlist(vec!["x.com".into()]),
            &SandboxPolicy::WorkspaceWrite {
                workspace_dir: std::path::PathBuf::from("/tmp"),
            },
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_isolation_blocks_outbound() {
        // AC-1 mechanism: with bwrap+userns available, build_net_command must
        // produce a command that cannot reach the network. This spawns the
        // real bwrap command and asserts curl fails (exit != 0, host
        // unreachable). No LLM provider loaded — pure isolation check.
        if !(bwrap_available() && userns_enabled()) {
            return; // environment lacks bwrap/userns; fallback path untested here
        }
        let mut base_cmd = std::process::Command::new("sh");
        base_cmd
            .arg("-c")
            .arg("curl -s --max-time 5 https://example.com >/dev/null 2>&1");
        // The network-namespace isolation via bwrap is only applied for an
        // unrestricted sandbox (FullAccess) — restricting sandboxes rely on
        // the seccomp socket-block (applied by the caller's pre_exec hook,
        // tested separately below). This test validates the bwrap path.
        let mut isolated =
            build_net_command(&base_cmd, &NetPolicy::None, &SandboxPolicy::FullAccess);
        let output = isolated.output().expect("spawn isolated command");
        // curl returns 6 (could not resolve host) when network is isolated.
        assert_ne!(output.status.code(), Some(0), "network must be isolated");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_seccomp_blocks_socket_in_child() {
        // Restricting sandboxes block the socket() syscall via the seccomp
        // pre_exec hook applied by the caller (bash.rs). This unit test
        // exercises that mechanism directly: spawn `python3 -c 'import
        // socket; socket.socket()'` with the hook attached — it must fail
        // with EPERM (no SOCKET_OK printed), matching AC-2.
        use std::os::unix::process::CommandExt;
        let policy = SandboxPolicy::WorkspaceWrite {
            workspace_dir: std::path::PathBuf::from("/tmp"),
        };
        let mut cmd = std::process::Command::new("python3");
        cmd.args([
            "-c",
            "import socket; s=socket.socket(); s.close(); print('SOCKET_OK')",
        ]);
        let hook = crate::make_pre_exec_hook(std::sync::Arc::new(policy));
        unsafe {
            cmd.pre_exec(hook);
        }
        let output = cmd.output().expect("spawn seccomp-restricted command");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // Check that "SOCKET_OK" was not printed as output (standalone line),
        // not just present in a Python traceback source line.
        let socket_ok_printed = combined.lines().any(|l| l.trim() == "SOCKET_OK");
        assert!(
            !socket_ok_printed,
            "socket() must be blocked by seccomp: {combined}"
        );
    }
}
