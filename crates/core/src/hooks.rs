//! Lifecycle hooks for ZeroZero agent loop ().
//!
//! Hooks allow custom logic at specific points in the agent loop:
//! - `pre_tool`: before a tool executes (can modify args or abort)
//! - `post_tool`: after a tool executes (can run lint, test, etc.)
//! - `pre_commit`: before a git commit (can run checks)
//! - `session_start` / `session_end`: once per session
//! - `user_prompt_submit` / `stop`: once per turn
//! - `post_tool_failure`: per tool call error
//! - `pre_compact`: before compaction

use serde_json::Value;

/// Result of a pre-tool hook.
pub enum HookAction {
    /// Allow the tool to proceed (optionally with modified args).
    Continue { args: Value },
    /// Abort the tool call with a message.
    Abort { reason: String },
}

/// Context passed to hooks.
pub struct ToolHookContext {
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: Value,
}

/// Context passed to post-tool hooks.
pub struct PostToolContext {
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: Value,
    pub result: String,
}

/// Event kind — maps config section name to trait method .
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    Stop,
    PostToolUseFailure,
    PreCompact,
}

/// Once-per-session context .
pub struct SessionStartCtx {
    pub session_id: String,
    pub cwd: String,
}

/// Once-per-turn context — after user prompt, before LLM call .
pub struct UserPromptCtx {
    pub session_id: String,
    pub prompt: String,
}

/// Once-per-turn context — turn end .
pub struct StopCtx {
    pub session_id: String,
    pub reason: String,
}

/// Per-tool-call context — tool execute returned Err .
pub struct ToolFailureCtx {
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: Value,
    pub error: String,
}

/// Before compaction context .
pub struct PreCompactCtx {
    pub session_id: String,
    pub before_messages: usize,
    pub before_tokens: usize,
    pub trigger: String,
}

/// Trait for lifecycle hooks. Implement one or more methods.
/// Default implementations are no-ops.
#[async_trait::async_trait]
pub trait LifecycleHooks: Send + Sync {
    /// Called before a tool executes. Can modify args or abort.
    async fn pre_tool(&self, ctx: &ToolHookContext) -> HookAction {
        HookAction::Continue {
            args: ctx.args.clone(),
        }
    }

    /// Called after a tool executes successfully.
    async fn post_tool(&self, _ctx: &PostToolContext) {}

    /// Called before a git commit (if semi-auto commit is enabled).
    /// Return false to abort the commit.
    async fn pre_commit(&self, _message: &str) -> bool {
        true
    }

    /// Called once per session, after SessionStarted event .
    async fn session_start(&self, _ctx: &SessionStartCtx) {}

    /// Called once per session, at the end of run_turn .
    async fn session_end(&self, _ctx: &SessionStartCtx) {}

    /// Called once per turn, after user prompt is submitted .
    async fn user_prompt_submit(&self, _ctx: &UserPromptCtx) {}

    /// Called once per turn, at turn end .
    async fn stop(&self, _ctx: &StopCtx) {}

    /// Called per tool call when tool execution returns an error .
    async fn post_tool_failure(&self, _ctx: &ToolFailureCtx) {}

    /// Called before compaction .
    async fn pre_compact(&self, _ctx: &PreCompactCtx) {}
}

/// A no-op hook implementation that does nothing. This is the default.
pub struct NoopHooks;

#[async_trait::async_trait]
impl LifecycleHooks for NoopHooks {}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    #[tokio::test]
    async fn test_noop_hooks_pre_tool_continues() {
        let hooks = NoopHooks;
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "call_1".to_string(),
            args: json!({"command": "ls"}),
        };
        let action = hooks.pre_tool(&ctx).await;
        match action {
            HookAction::Continue { args } => {
                assert_eq!(args["command"], "ls");
            }
            HookAction::Abort { .. } => panic!("NoopHooks should not abort"),
        }
    }

    #[tokio::test]
    async fn test_noop_hooks_post_tool_no_error() {
        let hooks = NoopHooks;
        let ctx = PostToolContext {
            tool_name: "bash".to_string(),
            tool_call_id: "call_1".to_string(),
            args: json!({}),
            result: "output".to_string(),
        };
        // Should not panic or error
        hooks.post_tool(&ctx).await;
    }

    #[tokio::test]
    async fn test_noop_hooks_pre_commit_allows() {
        let hooks = NoopHooks;
        assert!(hooks.pre_commit("fix: update code").await);
    }

    #[tokio::test]
    async fn test_custom_hook_can_abort() {
        struct AbortHook;
        #[async_trait]
        impl LifecycleHooks for AbortHook {
            async fn pre_tool(&self, _ctx: &ToolHookContext) -> HookAction {
                HookAction::Abort {
                    reason: "blocked".to_string(),
                }
            }
        }
        let hooks = AbortHook;
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "call_1".to_string(),
            args: json!({}),
        };
        let action = hooks.pre_tool(&ctx).await;
        match action {
            HookAction::Abort { reason } => assert_eq!(reason, "blocked"),
            HookAction::Continue { .. } => panic!("Should abort"),
        }
    }

    #[tokio::test]
    async fn test_custom_hook_can_modify_args() {
        struct ModifyHook;
        #[async_trait]
        impl LifecycleHooks for ModifyHook {
            async fn pre_tool(&self, ctx: &ToolHookContext) -> HookAction {
                let mut args = ctx.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("injected".to_string(), json!(true));
                }
                HookAction::Continue { args }
            }
        }
        let hooks = ModifyHook;
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "call_1".to_string(),
            args: json!({"command": "ls"}),
        };
        let action = hooks.pre_tool(&ctx).await;
        match action {
            HookAction::Continue { args } => {
                assert_eq!(args["command"], "ls");
                assert_eq!(args["injected"], true);
            }
            HookAction::Abort { .. } => panic!("Should not abort"),
        }
    }

    // NoopHooks 6 new event methods — default no-op, no panic.
    #[tokio::test]
    async fn test_noop_hooks_session_start_noop() {
        let hooks = NoopHooks;
        hooks
            .session_start(&SessionStartCtx {
                session_id: "s1".to_string(),
                cwd: "/tmp".to_string(),
            })
            .await;
    }

    #[tokio::test]
    async fn test_noop_hooks_session_end_noop() {
        let hooks = NoopHooks;
        hooks
            .session_end(&SessionStartCtx {
                session_id: "s1".to_string(),
                cwd: String::new(),
            })
            .await;
    }

    #[tokio::test]
    async fn test_noop_hooks_user_prompt_submit_noop() {
        let hooks = NoopHooks;
        hooks
            .user_prompt_submit(&UserPromptCtx {
                session_id: "s1".to_string(),
                prompt: "hello".to_string(),
            })
            .await;
    }

    #[tokio::test]
    async fn test_noop_hooks_stop_noop() {
        let hooks = NoopHooks;
        hooks
            .stop(&StopCtx {
                session_id: "s1".to_string(),
                reason: "completed".to_string(),
            })
            .await;
    }

    #[tokio::test]
    async fn test_noop_hooks_post_tool_failure_noop() {
        let hooks = NoopHooks;
        hooks
            .post_tool_failure(&ToolFailureCtx {
                tool_name: "bash".to_string(),
                tool_call_id: "c1".to_string(),
                args: json!({}),
                error: "boom".to_string(),
            })
            .await;
    }

    #[tokio::test]
    async fn test_noop_hooks_pre_compact_noop() {
        let hooks = NoopHooks;
        hooks
            .pre_compact(&PreCompactCtx {
                session_id: "s1".to_string(),
                before_messages: 10,
                before_tokens: 5000,
                trigger: "auto".to_string(),
            })
            .await;
    }

    // backward-compat — AutoTestHooks inherit 6 new defaults.
    #[tokio::test]
    async fn test_backward_compat_auto_test_hooks() {
        use crate::AutoTestHooks;
        let hooks = AutoTestHooks::new();
        // Existing methods still work.
        let ctx = ToolHookContext {
            tool_name: "edit_file".to_string(),
            tool_call_id: "c1".to_string(),
            args: json!({}),
        };
        let _ = hooks.pre_tool(&ctx).await;
        // New methods are no-op (inherited default) — no panic.
        hooks
            .session_start(&SessionStartCtx {
                session_id: "s1".to_string(),
                cwd: String::new(),
            })
            .await;
        hooks
            .stop(&StopCtx {
                session_id: "s1".to_string(),
                reason: "completed".to_string(),
            })
            .await;
    }
}
