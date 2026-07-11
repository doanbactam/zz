//! Multi-agent orchestration for ZeroZero.
//!
//! `MultiAgentOrchestrator` — fire-and-forget batch execution
//! (parallel/sequential).
//!
//!): Interactive multi-agent thread model —
//! `ThreadRegistry`, `spawn_agent`, steer, interrupt, `/agent` TUI switch.
//! The `MultiAgentOrchestrator` is kept as a thin wrapper for backward
//! compatibility (existing `zz multi` command).

pub mod spawn_agent_tool;
pub mod steer;
pub mod thread;

pub use spawn_agent_tool::SpawnAgentTool;
pub use steer::run_turn_with_steer;
pub use thread::{
    AgentMetadata, AgentPath, AgentStatus, SUBAGENT_NOTIFY_PREFIX, SpawnContext, ThreadHandle,
    ThreadId, ThreadRegistry, ThreadResult,
};

use std::sync::Arc;
use zerozero_compaction::CompactionConfig;
use zerozero_exec::Event;
use zerozero_llm::Provider;
use zerozero_sandbox::{ApprovalPolicy, SandboxPolicy};
use zerozero_tools::ToolRegistry;

/// Task definition for a sub-agent.
#[derive(Clone)]
pub struct AgentTask {
    pub id: String,
    pub prompt: String,
    pub max_turns: u32,
}

/// Result of a sub-agent run.
#[derive(Clone)]
pub struct AgentResult {
    pub id: String,
    pub events: Vec<Event>,
    pub success: bool,
    pub error: Option<String>,
}

/// Orchestrator that runs multiple agents in parallel.
pub struct MultiAgentOrchestrator {
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    sandbox: SandboxPolicy,
    approval: ApprovalPolicy,
    compaction_config: CompactionConfig,
}

impl MultiAgentOrchestrator {
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        sandbox: SandboxPolicy,
        approval: ApprovalPolicy,
        compaction_config: CompactionConfig,
    ) -> Self {
        Self {
            provider,
            tools,
            sandbox,
            approval,
            compaction_config,
        }
    }

    /// Run multiple agent tasks in parallel using LocalSet.
    ///
    /// Each task gets its own independent session (no shared state).
    /// Results are collected in order.
    pub async fn run_parallel(&self, tasks: Vec<AgentTask>) -> Vec<AgentResult> {
        let local_set = tokio::task::LocalSet::new();

        let mut handles = Vec::new();
        for task in tasks {
            let provider = Arc::clone(&self.provider);
            let tools = Arc::clone(&self.tools);
            let sandbox = self.sandbox.clone();
            let approval = self.approval;
            let compaction_config = self.compaction_config.clone();

            let handle = local_set.spawn_local(async move {
                run_single_agent(task, provider, tools, sandbox, approval, compaction_config).await
            });
            handles.push(handle);
        }

        local_set
            .run_until(async {
                let mut results = Vec::new();
                for handle in handles {
                    match handle.await {
                        Ok(result) => results.push(result),
                        Err(e) => results.push(AgentResult {
                            id: "unknown".to_string(),
                            events: vec![],
                            success: false,
                            error: Some(format!("Task panicked: {e}")),
                        }),
                    }
                }
                results
            })
            .await
    }

    /// Run tasks sequentially (one after another).
    pub async fn run_sequential(&self, tasks: Vec<AgentTask>) -> Vec<AgentResult> {
        let mut results = Vec::new();

        for task in tasks {
            let provider = Arc::clone(&self.provider);
            let tools = Arc::clone(&self.tools);
            let sandbox = self.sandbox.clone();
            let approval = self.approval;
            let compaction_config = self.compaction_config.clone();

            let result =
                run_single_agent(task, provider, tools, sandbox, approval, compaction_config).await;
            results.push(result);
        }

        results
    }
}

async fn run_single_agent(
    task: AgentTask,
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    sandbox: SandboxPolicy,
    approval: ApprovalPolicy,
    compaction_config: CompactionConfig,
) -> AgentResult {
    let emit = |_event: Event| {};

    let result = zerozero_core::run_turn(
        &task.prompt,
        None,
        &*provider,
        &tools,
        task.max_turns,
        &sandbox,
        &approval,
        false,
        false,
        None,
        &compaction_config,
        &zerozero_core::NoopHooks,
        None,
        &[],
        zerozero_llm::Effort::None,
        &[],
        None,
        emit,
    )
    .await;

    match result {
        Ok(()) => AgentResult {
            id: task.id,
            events: vec![],
            success: true,
            error: None,
        },
        Err(e) => AgentResult {
            id: task.id,
            events: vec![],
            success: false,
            error: Some(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerozero_llm::OpenAIProvider;

    #[test]
    fn test_agent_task_clone() {
        let task = AgentTask {
            id: "task-1".to_string(),
            prompt: "Fix the bug".to_string(),
            max_turns: 5,
        };
        let cloned = task.clone();
        assert_eq!(task.id, cloned.id);
        assert_eq!(task.prompt, cloned.prompt);
        assert_eq!(task.max_turns, cloned.max_turns);
    }

    #[test]
    fn test_agent_result_clone() {
        let result = AgentResult {
            id: "task-1".to_string(),
            events: vec![],
            success: true,
            error: None,
        };
        let cloned = result.clone();
        assert_eq!(result.id, cloned.id);
        assert!(cloned.success);
    }

    #[test]
    fn test_orchestrator_construction() {
        let provider: Arc<dyn Provider> = Arc::new(OpenAIProvider::new(
            "key".to_string(),
            "http://localhost".to_string(),
            "model".to_string(),
        ));
        let tools = Arc::new(ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess)));
        let orchestrator = MultiAgentOrchestrator::new(
            provider,
            tools,
            SandboxPolicy::FullAccess,
            ApprovalPolicy::Never,
            CompactionConfig::default(),
        );
        // Just verify it constructs
        let _ = orchestrator;
    }
}
