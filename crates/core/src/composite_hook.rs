//! Composite hook — fan-out wrapper for multiple hooks .
//!
//! `run_turn` accepts `&dyn LifecycleHooks` (1 trait object). Config can have
//! many `[[hooks.X]]` entries → many `HttpHook` instances. `CompositeHook`
//! wraps them in a single trait object. `pre_tool`: first `Abort` wins (short-
//! circuit); other methods iterate all hooks.

use crate::hooks::{
    HookAction, LifecycleHooks, PostToolContext, PreCompactCtx, SessionStartCtx, StopCtx,
    ToolFailureCtx, ToolHookContext, UserPromptCtx,
};

pub struct CompositeHook {
    hooks: Vec<Box<dyn LifecycleHooks>>,
}

impl CompositeHook {
    pub fn new(hooks: Vec<Box<dyn LifecycleHooks>>) -> Self {
        Self { hooks }
    }

    pub fn empty() -> Self {
        Self { hooks: vec![] }
    }
}

#[async_trait::async_trait]
impl LifecycleHooks for CompositeHook {
    async fn pre_tool(&self, ctx: &ToolHookContext) -> HookAction {
        let mut args = ctx.args.clone();
        for h in &self.hooks {
            let action = h
                .pre_tool(&ToolHookContext {
                    tool_name: ctx.tool_name.clone(),
                    tool_call_id: ctx.tool_call_id.clone(),
                    args: args.clone(),
                })
                .await;
            match action {
                HookAction::Abort { .. } => return action,
                HookAction::Continue { args: a } => args = a,
            }
        }
        HookAction::Continue { args }
    }

    async fn post_tool(&self, ctx: &PostToolContext) {
        for h in &self.hooks {
            h.post_tool(ctx).await;
        }
    }

    async fn pre_commit(&self, message: &str) -> bool {
        for h in &self.hooks {
            if !h.pre_commit(message).await {
                return false;
            }
        }
        true
    }

    async fn session_start(&self, ctx: &SessionStartCtx) {
        for h in &self.hooks {
            h.session_start(ctx).await;
        }
    }

    async fn session_end(&self, ctx: &SessionStartCtx) {
        for h in &self.hooks {
            h.session_end(ctx).await;
        }
    }

    async fn user_prompt_submit(&self, ctx: &UserPromptCtx) {
        for h in &self.hooks {
            h.user_prompt_submit(ctx).await;
        }
    }

    async fn stop(&self, ctx: &StopCtx) {
        for h in &self.hooks {
            h.stop(ctx).await;
        }
    }

    async fn post_tool_failure(&self, ctx: &ToolFailureCtx) {
        for h in &self.hooks {
            h.post_tool_failure(ctx).await;
        }
    }

    async fn pre_compact(&self, ctx: &PreCompactCtx) {
        for h in &self.hooks {
            h.pre_compact(ctx).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_composite_empty_noop() {
        let composite = CompositeHook::empty();
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "c1".to_string(),
            args: json!({}),
        };
        let action = composite.pre_tool(&ctx).await;
        match action {
            HookAction::Continue { .. } => {}
            HookAction::Abort { .. } => panic!("empty composite should continue"),
        }
    }

    #[tokio::test]
    async fn test_composite_first_abort_wins() {
        struct AbortHook;
        #[async_trait]
        impl LifecycleHooks for AbortHook {
            async fn pre_tool(&self, _ctx: &ToolHookContext) -> HookAction {
                HookAction::Abort {
                    reason: "first".to_string(),
                }
            }
        }
        struct NeverCalledHook;
        #[async_trait]
        impl LifecycleHooks for NeverCalledHook {
            async fn pre_tool(&self, _ctx: &ToolHookContext) -> HookAction {
                panic!("should not be called after Abort");
            }
        }
        let composite = CompositeHook::new(vec![Box::new(AbortHook), Box::new(NeverCalledHook)]);
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "c1".to_string(),
            args: json!({}),
        };
        let action = composite.pre_tool(&ctx).await;
        match action {
            HookAction::Abort { reason } => assert_eq!(reason, "first"),
            HookAction::Continue { .. } => panic!("should abort"),
        }
    }

    #[tokio::test]
    async fn test_composite_fanout_session_start() {
        let count1 = Arc::new(AtomicU32::new(0));
        let count2 = Arc::new(AtomicU32::new(0));
        let c1 = count1.clone();
        let c2 = count2.clone();
        struct H1(Arc<AtomicU32>);
        #[async_trait]
        impl LifecycleHooks for H1 {
            async fn session_start(&self, _ctx: &SessionStartCtx) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let composite = CompositeHook::new(vec![Box::new(H1(c1)), Box::new(H1(c2))]);
        composite
            .session_start(&SessionStartCtx {
                session_id: "s1".to_string(),
                cwd: String::new(),
            })
            .await;
        assert_eq!(count1.load(Ordering::SeqCst), 1);
        assert_eq!(count2.load(Ordering::SeqCst), 1);
    }
}
