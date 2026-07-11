//! Approval gate — danger level classification + approval policy.

use serde::Serialize;

/// Danger level of a shell command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DangerLevel {
    Safe,
    Caution,
    Warning,
    Critical,
}

impl DangerLevel {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Caution => "caution",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// Approval policy controlling when to prompt the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Auto-approve everything (no prompts).
    Never,
    /// Auto-approve Safe only; deny everything else.
    Untrusted,
    /// Auto-approve Safe; prompt for Caution/Warning; deny Critical.
    OnRequest,
    /// Auto-edit mode : auto-approve read-only / safe / mildly-risky
    /// tool calls (Safe + Caution), prompt for Warning (destructive), deny
    /// Critical. Designed for a coding agent that should freely edit files
    /// but still confirm destructive shell commands.
    AutoEdit,
    /// Ask mode : prompt for EVERY command / tool call, regardless of
    /// danger level. Equivalent to `should_approve => Prompt` for all inputs.
    OnAsk,
}

/// Action to take for a command based on policy + danger level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    AutoApprove,
    Prompt,
    AutoDeny,
}

/// Why a tool call was rejected (used by the on-reject hook).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Denied outright by the approval policy (e.g. Untrusted + Warning,
    /// or AutoEdit + Critical).
    Policy,
    /// Prompted, but the user explicitly denied (answered "no").
    User,
}

/// Context passed to the on-reject hook when a tool call is rejected.
#[derive(Debug, Clone)]
pub struct RejectInfo {
    pub tool_name: String,
    pub danger_level: DangerLevel,
    pub reason: RejectReason,
}

/// Callback invoked by [`RejectHook`] when a tool call is rejected.
pub type RejectCallback = Box<dyn Fn(&RejectInfo) + Send + Sync>;

/// On-reject callback/hook .
///
/// Invoked when a tool call is rejected — either by policy (`AutoDeny`) or by
/// an explicit user denial after a prompt. The hook is purely a notification
/// side-channel (telemetry, logging, user alerts); it cannot override the
/// decision.
pub struct RejectHook {
    hook: Option<RejectCallback>,
}

impl RejectHook {
    /// Create an empty hook (no-op on reject).
    pub fn new() -> Self {
        Self { hook: None }
    }

    /// Register a callback invoked on every rejection.
    pub fn set<F>(&mut self, f: F)
    where
        F: Fn(&RejectInfo) + Send + Sync + 'static,
    {
        self.hook = Some(Box::new(f));
    }

    /// Fire the hook if a callback is registered. No-op otherwise.
    pub fn call(&self, info: &RejectInfo) {
        if let Some(h) = &self.hook {
            h(info);
        }
    }
}

impl Default for RejectHook {
    fn default() -> Self {
        Self::new()
    }
}

const CRITICAL_PATTERNS: &[&str] = &[
    "> /dev/sd",
    "dd if=",
    ":(){ :|:& };:",
    "chmod -r 777 /",
    "mv /* /dev/null",
    "> /etc/passwd",
    "mkfs",
];

const WARNING_PATTERNS: &[&str] = &[
    "rm -rf",
    "git push --force",
    "git push -f",
    "git reset --hard",
    "drop table",
    "drop database",
    "truncate table",
    "shutdown",
    "reboot",
    "halt",
    "kill -9",
    "killall",
];

const CAUTION_PATTERNS: &[&str] = &[
    "sudo",
    "curl",
    "wget",
    "chmod",
    "mv ",
    "git push",
    "git commit",
    "npm install",
    "pip install",
    "cargo install",
    "apt ",
    "yum ",
    "brew install",
];

/// Classify a shell command by danger level using pattern matching.
pub fn classify_command(command: &str) -> DangerLevel {
    let lower = command.to_lowercase();
    if CRITICAL_PATTERNS.iter().any(|p| lower.contains(p)) {
        return DangerLevel::Critical;
    }
    if WARNING_PATTERNS.iter().any(|p| lower.contains(p)) {
        return DangerLevel::Warning;
    }
    if CAUTION_PATTERNS.iter().any(|p| lower.contains(p)) {
        return DangerLevel::Caution;
    }
    DangerLevel::Safe
}

/// Determine what action to take for a command based on policy + danger level.
///
/// Exhaustive over all `(ApprovalPolicy, DangerLevel)` combinations.
pub const fn should_approve(policy: &ApprovalPolicy, level: DangerLevel) -> ApprovalAction {
    match (policy, level) {
        // Never: auto-approve everything, no prompts.
        (ApprovalPolicy::Never, _) => ApprovalAction::AutoApprove,
        // Untrusted: only Safe is allowed; everything else denied.
        (ApprovalPolicy::Untrusted, DangerLevel::Safe) => ApprovalAction::AutoApprove,
        (ApprovalPolicy::Untrusted, _) => ApprovalAction::AutoDeny,
        // OnRequest: Safe auto; Critical denied; middle ground prompts.
        (ApprovalPolicy::OnRequest, DangerLevel::Safe) => ApprovalAction::AutoApprove,
        (ApprovalPolicy::OnRequest, DangerLevel::Critical) => ApprovalAction::AutoDeny,
        (ApprovalPolicy::OnRequest, _) => ApprovalAction::Prompt,
        // AutoEdit: safe + mildly-risky auto-approved (read/edit freely);
        // destructive (Warning) prompted; Critical denied.
        (ApprovalPolicy::AutoEdit, DangerLevel::Safe | DangerLevel::Caution) => {
            ApprovalAction::AutoApprove
        }
        (ApprovalPolicy::AutoEdit, DangerLevel::Critical) => ApprovalAction::AutoDeny,
        (ApprovalPolicy::AutoEdit, DangerLevel::Warning) => ApprovalAction::Prompt,
        // OnAsk: prompt for everything.
        (ApprovalPolicy::OnAsk, _) => ApprovalAction::Prompt,
    }
}

/// Full-access guard : even when the sandbox grants full filesystem
/// access, certain commands are destructive and should raise a warning before
/// execution (e.g. `rm -rf`, `git push --force`).
///
/// Returns `Some(warning)` for destructive commands, `None` otherwise.
pub fn full_access_warn(command: &str) -> Option<&'static str> {
    match classify_command(command) {
        DangerLevel::Warning => {
            Some("destructive command under full-access sandbox: confirm before running")
        }
        DangerLevel::Critical => Some("CRITICAL destructive command under full-access sandbox"),
        DangerLevel::Safe | DangerLevel::Caution => None,
    }
}

/// Apply an approval decision, firing the on-reject hook when the call is
/// rejected.
///
/// `user_response`:
/// - `None`  → no user interaction yet; a `Prompt` is returned as-is and a
///   policy `AutoDeny` still fires the hook (reason = `Policy`).
/// - `Some(true)`  → user approved a prompt; result promoted to `AutoApprove`.
/// - `Some(false)` → user denied a prompt; hook fired (reason = `User`) and
///   result becomes `AutoDeny`.
pub fn decide(
    policy: &ApprovalPolicy,
    level: DangerLevel,
    tool_name: &str,
    reject: &RejectHook,
    user_response: Option<bool>,
) -> ApprovalAction {
    let action = should_approve(policy, level);
    match action {
        ApprovalAction::AutoDeny => {
            reject.call(&RejectInfo {
                tool_name: tool_name.to_string(),
                danger_level: level,
                reason: RejectReason::Policy,
            });
            ApprovalAction::AutoDeny
        }
        ApprovalAction::Prompt => match user_response {
            Some(true) => ApprovalAction::AutoApprove,
            Some(false) => {
                reject.call(&RejectInfo {
                    tool_name: tool_name.to_string(),
                    danger_level: level,
                    reason: RejectReason::User,
                });
                ApprovalAction::AutoDeny
            }
            None => ApprovalAction::Prompt,
        },
        ApprovalAction::AutoApprove => ApprovalAction::AutoApprove,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_safe() {
        assert_eq!(classify_command("echo hello"), DangerLevel::Safe);
        assert_eq!(classify_command("ls -la"), DangerLevel::Safe);
        assert_eq!(classify_command("cat file.txt"), DangerLevel::Safe);
    }

    #[test]
    fn test_classify_caution() {
        assert_eq!(classify_command("sudo apt update"), DangerLevel::Caution);
        assert_eq!(
            classify_command("curl http://example.com"),
            DangerLevel::Caution
        );
        assert_eq!(
            classify_command("git push origin main"),
            DangerLevel::Caution
        );
    }

    #[test]
    fn test_classify_warning() {
        assert_eq!(classify_command("rm -rf /tmp/test"), DangerLevel::Warning);
        assert_eq!(
            classify_command("git push --force origin main"),
            DangerLevel::Warning
        );
        assert_eq!(
            classify_command("git reset --hard HEAD~3"),
            DangerLevel::Warning
        );
        assert_eq!(classify_command("DROP TABLE users"), DangerLevel::Warning);
    }

    #[test]
    fn test_classify_critical() {
        assert_eq!(
            classify_command("dd if=/dev/zero of=/dev/sda"),
            DangerLevel::Critical
        );
        assert_eq!(classify_command(":(){ :|:& };:"), DangerLevel::Critical);
        assert_eq!(classify_command("chmod -R 777 /"), DangerLevel::Critical);
        assert_eq!(
            classify_command("mkfs.ext4 /dev/sda1"),
            DangerLevel::Critical
        );
    }

    #[test]
    fn test_classify_case_insensitive() {
        assert_eq!(classify_command("DROP TABLE users"), DangerLevel::Warning);
        assert_eq!(classify_command("drop table users"), DangerLevel::Warning);
    }

    #[test]
    fn test_should_approve_never() {
        assert_eq!(
            should_approve(&ApprovalPolicy::Never, DangerLevel::Critical),
            ApprovalAction::AutoApprove
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::Never, DangerLevel::Safe),
            ApprovalAction::AutoApprove
        );
    }

    #[test]
    fn test_should_approve_untrusted() {
        assert_eq!(
            should_approve(&ApprovalPolicy::Untrusted, DangerLevel::Safe),
            ApprovalAction::AutoApprove
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::Untrusted, DangerLevel::Caution),
            ApprovalAction::AutoDeny
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::Untrusted, DangerLevel::Warning),
            ApprovalAction::AutoDeny
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::Untrusted, DangerLevel::Critical),
            ApprovalAction::AutoDeny
        );
    }

    #[test]
    fn test_should_approve_on_request() {
        assert_eq!(
            should_approve(&ApprovalPolicy::OnRequest, DangerLevel::Safe),
            ApprovalAction::AutoApprove
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::OnRequest, DangerLevel::Caution),
            ApprovalAction::Prompt
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::OnRequest, DangerLevel::Warning),
            ApprovalAction::Prompt
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::OnRequest, DangerLevel::Critical),
            ApprovalAction::AutoDeny
        );
    }

    // --- extended approval modes ---

    #[test]
    fn test_should_approve_auto_edit_allows_read_blocks_rm() {
        // Read-only / safe commands auto-approved.
        assert_eq!(
            should_approve(&ApprovalPolicy::AutoEdit, DangerLevel::Safe),
            ApprovalAction::AutoApprove
        );
        // Mildly risky (caution) also auto-approved — agent edits freely.
        assert_eq!(
            should_approve(&ApprovalPolicy::AutoEdit, DangerLevel::Caution),
            ApprovalAction::AutoApprove
        );
        // Destructive (rm -rf) still prompts — NOT auto-approved.
        assert_eq!(
            should_approve(&ApprovalPolicy::AutoEdit, DangerLevel::Warning),
            ApprovalAction::Prompt
        );
        // Critical denied.
        assert_eq!(
            should_approve(&ApprovalPolicy::AutoEdit, DangerLevel::Critical),
            ApprovalAction::AutoDeny
        );
    }

    #[test]
    fn test_auto_edit_real_commands() {
        // `read`-style tool calls map to Safe → approved.
        assert_eq!(
            should_approve(&ApprovalPolicy::AutoEdit, classify_command("ls -la src/")),
            ApprovalAction::AutoApprove
        );
        // `write_file` non-critical → Safe → approved.
        assert_eq!(
            should_approve(&ApprovalPolicy::AutoEdit, classify_command("echo hi")),
            ApprovalAction::AutoApprove
        );
        // `rm -rf` → Warning → blocked (prompt).
        assert_eq!(
            should_approve(
                &ApprovalPolicy::AutoEdit,
                classify_command("rm -rf target/")
            ),
            ApprovalAction::Prompt
        );
        // force push → Warning → blocked (prompt).
        assert_eq!(
            should_approve(
                &ApprovalPolicy::AutoEdit,
                classify_command("git push --force origin main")
            ),
            ApprovalAction::Prompt
        );
    }

    #[test]
    fn test_should_approve_on_ask_blocks_everything() {
        // OnAsk prompts for EVERY danger level.
        assert_eq!(
            should_approve(&ApprovalPolicy::OnAsk, DangerLevel::Safe),
            ApprovalAction::Prompt
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::OnAsk, DangerLevel::Caution),
            ApprovalAction::Prompt
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::OnAsk, DangerLevel::Warning),
            ApprovalAction::Prompt
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::OnAsk, DangerLevel::Critical),
            ApprovalAction::Prompt
        );
    }

    #[test]
    fn test_on_ask_real_commands_all_prompt() {
        assert_eq!(
            should_approve(&ApprovalPolicy::OnAsk, classify_command("echo hello")),
            ApprovalAction::Prompt
        );
        assert_eq!(
            should_approve(&ApprovalPolicy::OnAsk, classify_command("rm -rf /")),
            ApprovalAction::Prompt
        );
    }

    #[test]
    fn test_full_access_warn_guard() {
        // Destructive commands warn even under full access.
        assert!(full_access_warn("rm -rf /").is_some());
        assert!(full_access_warn("git push --force origin main").is_some());
        assert!(full_access_warn("git reset --hard HEAD~5").is_some());
        assert!(full_access_warn("dd if=/dev/zero of=/dev/sda").is_some());
        // Benign commands do not warn.
        assert!(full_access_warn("echo hello").is_none());
        assert!(full_access_warn("cargo build").is_none());
        assert!(full_access_warn("ls -la").is_none());
    }

    #[test]
    fn test_reject_hook_policy_deny_fires() {
        let mut reject = RejectHook::new();
        let fired: std::sync::Arc<std::sync::Mutex<Option<RejectInfo>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let fired_clone = fired.clone();
        reject.set(move |info| {
            *fired_clone.lock().unwrap() = Some(RejectInfo {
                tool_name: info.tool_name.clone(),
                danger_level: info.danger_level,
                reason: info.reason,
            });
        });

        // Untrusted + Warning → AutoDeny, hook fires with Policy reason.
        let action = decide(
            &ApprovalPolicy::Untrusted,
            DangerLevel::Warning,
            "bash",
            &reject,
            None,
        );
        assert_eq!(action, ApprovalAction::AutoDeny);
        let info = fired
            .lock()
            .unwrap()
            .take()
            .expect("reject hook should have fired");
        assert_eq!(info.tool_name, "bash");
        assert_eq!(info.danger_level, DangerLevel::Warning);
        assert_eq!(info.reason, RejectReason::Policy);
    }

    #[test]
    fn test_reject_hook_user_deny_fires() {
        let mut reject = RejectHook::new();
        let fired: std::sync::Arc<std::sync::Mutex<Option<RejectReason>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let fired_clone = fired.clone();
        reject.set(move |info| {
            *fired_clone.lock().unwrap() = Some(info.reason);
        });

        // OnRequest + Warning → Prompt; user says no → AutoDeny + User reason.
        let action = decide(
            &ApprovalPolicy::OnRequest,
            DangerLevel::Warning,
            "bash",
            &reject,
            Some(false),
        );
        assert_eq!(action, ApprovalAction::AutoDeny);
        assert_eq!(*fired.lock().unwrap(), Some(RejectReason::User));
    }

    #[test]
    fn test_reject_hook_user_approve_no_fire() {
        let mut reject = RejectHook::new();
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_clone = count.clone();
        reject.set(move |_| {
            count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });

        // OnRequest + Warning → Prompt; user approves → AutoApprove, hook silent.
        let action = decide(
            &ApprovalPolicy::OnRequest,
            DangerLevel::Warning,
            "bash",
            &reject,
            Some(true),
        );
        assert_eq!(action, ApprovalAction::AutoApprove);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn test_reject_hook_empty_is_noop() {
        // No callback registered → no panic, returns the base action.
        let reject = RejectHook::new();
        let action = decide(
            &ApprovalPolicy::Untrusted,
            DangerLevel::Critical,
            "bash",
            &reject,
            None,
        );
        assert_eq!(action, ApprovalAction::AutoDeny);
    }

    // --- : Mutation coverage fix ---

    #[test]
    fn test_prd8_ac4_danger_level_as_str_all_variants() {
        // Each variant must map to the exact lowercase string.
        // Mutation `as_str -> "xyzzy"` or `""` would be caught here.
        assert_eq!(DangerLevel::Safe.as_str(), "safe");
        assert_eq!(DangerLevel::Caution.as_str(), "caution");
        assert_eq!(DangerLevel::Warning.as_str(), "warning");
        assert_eq!(DangerLevel::Critical.as_str(), "critical");
    }

    #[test]
    fn test_prd8_ac4_classify_command_safe_examples() {
        // Safe commands: no pattern match.
        assert_eq!(classify_command("echo hello"), DangerLevel::Safe);
        assert_eq!(classify_command("ls -la"), DangerLevel::Safe);
        assert_eq!(classify_command("pwd"), DangerLevel::Safe);
        assert_eq!(classify_command("cargo build"), DangerLevel::Safe);
        assert_eq!(classify_command("rustc --version"), DangerLevel::Safe);
    }

    #[test]
    fn test_prd8_ac4_classify_command_non_lowercased_input() {
        // Mixed-case input is lowercased before matching.
        assert_eq!(classify_command("RM -RF /tmp"), DangerLevel::Warning);
        assert_eq!(classify_command("DROP TABLE Users"), DangerLevel::Warning);
    }
}
