//! `spawn_agent` tool — LLM-callable tool for spawning subagent threads
//! Phase 1, §3.8).
//!
//! When registered in the `ToolRegistry`, the LLM can call `spawn_agent`
//! with a `prompt` parameter to create a subagent thread. The subagent
//! runs in its own context with its own session.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::RwLock;
use zerozero_tools::Tool;

use crate::thread::{SpawnContext, ThreadId, ThreadRegistry};

/// LLM-callable tool that spawns a subagent thread via `ThreadRegistry`.
///
/// Tracks the current thread ID (the thread that is calling the tool) so
/// that the spawned subagent's parent is set correctly.
pub struct SpawnAgentTool {
    registry: Arc<ThreadRegistry>,
    ctx: Arc<SpawnContext>,
    /// Thread ID of the thread currently calling this tool.
    /// Updated by the TUI/exec layer when switching active threads.
    current_thread_id: Arc<RwLock<ThreadId>>,
}

impl SpawnAgentTool {
    /// Create a new `SpawnAgentTool`.
    pub fn new(
        registry: Arc<ThreadRegistry>,
        ctx: Arc<SpawnContext>,
        current_thread_id: ThreadId,
    ) -> Self {
        Self {
            registry,
            ctx,
            current_thread_id: Arc::new(RwLock::new(current_thread_id)),
        }
    }

    /// Get a clone of the current thread ID handle (for updating from TUI).
    pub fn current_thread_id_handle(&self) -> Arc<RwLock<ThreadId>> {
        Arc::clone(&self.current_thread_id)
    }
}

#[async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Spawn a subagent to work on a sub-task in the background. \
         The subagent runs in its own context with its own session. \
         Use /agent to switch between threads."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task prompt for the subagent"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required parameter: prompt"))?;

        let parent = self.current_thread_id.read().await.clone();
        let thread_id = self
            .registry
            .spawn_agent(prompt.to_string(), parent, &self.ctx)
            .await?;

        Ok(format!(
            "Spawned subagent (thread_id: {thread_id}). Use /agent to switch to it."
        ))
    }
}
