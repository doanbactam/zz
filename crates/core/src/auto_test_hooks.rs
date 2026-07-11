//! Auto-test hooks — a LifecycleHooks implementation that runs `cargo test`
//! after file edits).
//!
//! When the agent uses `edit_file` or `write_file`, the post_tool hook
//! triggers a `cargo test --workspace` in the background. If tests fail,
//! the result is logged but does NOT abort the agent loop (advisory only).

use crate::hooks::{HookAction, LifecycleHooks, PostToolContext, ToolHookContext};

/// Lifecycle hooks that auto-run tests after file modifications.
pub struct AutoTestHooks {
    /// Whether to actually run tests (can be disabled).
    enabled: bool,
}

impl AutoTestHooks {
    pub fn new() -> Self {
        Self {
            enabled: std::env::var("ZZ_AUTO_TEST")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        }
    }

    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    pub const fn disabled() -> Self {
        Self { enabled: false }
    }

    /// Run `cargo test --workspace` and return the output.
    fn run_tests(&self) -> String {
        let output = std::process::Command::new("cargo")
            .args(["test", "--workspace", "--", "--quiet"])
            .output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    format!("Tests passed.\n{stdout}")
                } else {
                    format!("Tests FAILED (exit {}):\n{stderr}\n{stdout}", o.status)
                }
            }
            Err(e) => format!("Failed to run cargo test: {e}"),
        }
    }
}

impl Default for AutoTestHooks {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LifecycleHooks for AutoTestHooks {
    async fn pre_tool(&self, _ctx: &ToolHookContext) -> HookAction {
        // No pre-tool action — allow all tools.
        HookAction::Continue {
            args: _ctx.args.clone(),
        }
    }

    async fn post_tool(&self, ctx: &PostToolContext) {
        if !self.enabled {
            return;
        }
        // Only run tests after file modification tools.
        if ctx.tool_name != "edit_file" && ctx.tool_name != "write_file" {
            return;
        }
        // Run tests synchronously (post_tool is async, but we block here).
        // In a real implementation, this would be spawned as a background task.
        let result = self.run_tests();
        eprintln!("[AutoTestHooks] post_tool({}): {result}", ctx.tool_name);
    }

    async fn pre_commit(&self, _message: &str) -> bool {
        // Allow commits — tests are advisory, not blocking.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::ToolHookContext;

    #[tokio::test]
    async fn test_auto_test_hooks_disabled_by_default_env() {
        // Without ZZ_AUTO_TEST env var, hooks are disabled.
        // SAFETY: This is a test — env var mutation is safe in single-threaded test.
        unsafe {
            std::env::remove_var("ZZ_AUTO_TEST");
        }
        let hooks = AutoTestHooks::new();
        assert!(!hooks.enabled);
    }

    #[tokio::test]
    async fn test_auto_test_hooks_enabled() {
        let hooks = AutoTestHooks::enabled();
        assert!(hooks.enabled);
    }

    #[tokio::test]
    async fn test_auto_test_hooks_pre_tool_continues() {
        let hooks = AutoTestHooks::disabled();
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "test-1".to_string(),
            args: serde_json::json!({"command": "echo hi"}),
        };
        let action = hooks.pre_tool(&ctx).await;
        assert!(matches!(action, HookAction::Continue { .. }));
    }

    #[tokio::test]
    async fn test_auto_test_hooks_post_tool_skips_when_disabled() {
        let hooks = AutoTestHooks::disabled();
        let ctx = PostToolContext {
            tool_name: "edit_file".to_string(),
            tool_call_id: "test-2".to_string(),
            args: serde_json::json!({}),
            result: "ok".to_string(),
        };
        // Should not panic or hang.
        hooks.post_tool(&ctx).await;
    }

    #[tokio::test]
    async fn test_auto_test_hooks_post_tool_skips_non_file_tools() {
        let hooks = AutoTestHooks::enabled();
        let ctx = PostToolContext {
            tool_name: "bash".to_string(),
            tool_call_id: "test-3".to_string(),
            args: serde_json::json!({}),
            result: "ok".to_string(),
        };
        // bash tool should not trigger tests — no panic, no hang.
        hooks.post_tool(&ctx).await;
    }

    #[tokio::test]
    async fn test_auto_test_hooks_pre_commit_allows() {
        let hooks = AutoTestHooks::enabled();
        let result = hooks.pre_commit("test commit").await;
        assert!(result);
    }
}
