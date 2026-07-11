//! AC-1: ThreadRegistry + spawn_agent — Phase 1 (multi-agent crate).
//!
//! Test: ThreadRegistry spawn creates a thread, depth > max_depth rejects,
//! total_count >= max_threads rejects, thread registered in registry map.
//! spawn_agent tool callable from LLM (TUI) + headless (`zz exec`).
//!
//! Pattern: Unit + integration (mock provider, LocalSet for spawn_local).

use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use zerozero_compaction::CompactionConfig;
use zerozero_llm::{ChatMessage, DeltaStream, Effort, Provider, SseEvent, SseEventStream};
use zerozero_multi_agent::{AgentStatus, SpawnAgentTool, SpawnContext, ThreadRegistry};
use zerozero_sandbox::{ApprovalPolicy, SandboxPolicy};
use zerozero_tools::{Tool, ToolRegistry};

/// Mock provider that returns a fixed response — no real API call.
struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn chat_stream(&self, _prompt: &str) -> anyhow::Result<DeltaStream> {
        let stream = futures::stream::iter(vec![Ok("mock response".to_string())]);
        Ok(Box::pin(stream))
    }

    async fn chat_with_tools(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        _effort: Effort,
        _images: &[String],
    ) -> anyhow::Result<SseEventStream> {
        // Return a stream with a single Done event so run_turn completes.
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<SseEvent>> + Send>> =
            Box::pin(futures::stream::iter(vec![Ok(SseEvent::Done)]));
        Ok(stream)
    }
}

use std::pin::Pin;

/// Build a SpawnContext with mock provider for testing.
fn make_ctx() -> Arc<SpawnContext> {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let tools = Arc::new(ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess)));
    Arc::new(SpawnContext {
        provider,
        tools,
        sandbox: SandboxPolicy::FullAccess,
        approval: ApprovalPolicy::Never,
        compaction_config: CompactionConfig::default(),
        session_db_path: None, // in-memory
        system_prompt: None,
        max_turns: 1,
        effort: Effort::None,
        emit_thread_event: None,
    })
}

/// AC-1 test 1: spawn_agent creates a thread and returns a ThreadId.
#[tokio::test]
async fn e2e_prd97_ac1_spawn_agent() {
    let (registry, root_id) = ThreadRegistry::new(6, 1);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("test prompt".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn_agent should succeed");

    // The spawned thread should be registered in the registry.
    let agents = registry.live_agents().await;
    assert!(
        agents.iter().any(|a| a.thread_id == child_id),
        "spawned thread {child_id} should be in live_agents"
    );

    // Root should also still be present.
    assert!(
        agents.iter().any(|a| a.thread_id == root_id),
        "root thread should still be in live_agents"
    );
}

/// AC-1 test 2: depth > max_depth is rejected.
///
/// With max_depth=1, spawning from a depth-1 child (which would create a
/// depth-2 grandchild) should fail.
#[tokio::test]
async fn e2e_prd97_ac1_depth_exceeds_max_rejected() {
    let (registry, root_id) = ThreadRegistry::new(6, 1);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("child prompt".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("first spawn should succeed");

    // Now try to spawn from the child (depth 1 → depth 2 > max_depth 1).
    let result = local_set
        .run_until(async {
            registry
                .spawn_agent("grandchild prompt".to_string(), child_id.clone(), &ctx)
                .await
        })
        .await;

    assert!(
        result.is_err(),
        "spawning grandchild (depth 2 > max_depth 1) should be rejected"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("max depth") || err_msg.contains("depth"),
        "error should mention depth: {err_msg}"
    );
}

/// AC-1 test 3: total_count >= max_threads is rejected.
///
/// With max_threads=2 (root counts as 1), spawning 2 children should fill
/// the cap, and the 2nd spawn should fail with "max threads".
///
/// Note: max_depth is set high (10) to isolate the max_threads cap test
/// from the depth cap. The depth cap is tested separately in test 2.
#[tokio::test]
async fn e2e_prd97_ac1_max_threads_exceeded_rejected() {
    let (registry, root_id) = ThreadRegistry::new(2, 10);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();

    // Spawn child 1 (total_count: 1 root + 1 child = 2 = max_threads).
    let _child1 = local_set
        .run_until(async {
            registry
                .spawn_agent("child 1".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("first spawn should succeed");

    // Spawn child 2 should fail (total_count already at max_threads=2).
    let result = local_set
        .run_until(async {
            registry
                .spawn_agent("child 2".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await;

    assert!(
        result.is_err(),
        "spawning when total_count >= max_threads should be rejected"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("max threads") || err_msg.contains("threads"),
        "error should mention threads: {err_msg}"
    );
}

/// AC-1 test 4: spawned thread is registered in the registry with correct
/// metadata (parent_thread_id, depth, agent_path, status=Running).
/// Also verifies the spawn_agent tool is callable.
#[tokio::test]
async fn e2e_prd97_ac1_thread_registered_in_registry() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("register test".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    let agents = registry.live_agents().await;
    let child_meta = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("child should be in live_agents");

    // Verify metadata fields.
    assert_eq!(
        child_meta.parent_thread_id,
        Some(root_id.clone()),
        "child's parent_thread_id should be root"
    );
    assert_eq!(
        child_meta.depth, 1,
        "child depth should be 1 (root depth 0 + 1)"
    );
    assert_eq!(
        child_meta.status,
        AgentStatus::Running,
        "newly spawned thread should have Running status"
    );
    assert!(
        child_meta.agent_path.as_str().starts_with("root."),
        "child agent_path should start with 'root.', got: {}",
        child_meta.agent_path
    );
    assert!(
        !child_meta.nickname.is_empty(),
        "child should have a non-empty nickname"
    );

    // Also verify the spawn_agent tool is callable and returns a thread_id.
    // Use a fresh registry to avoid ID collision with the first spawn
    // (thread_id_now() has second resolution — see GAP note in report).
    let (tool_registry, tool_root_id) = ThreadRegistry::new(6, 10);
    let tool_registry_arc = Arc::new(tool_registry);
    let tool = SpawnAgentTool::new(Arc::clone(&tool_registry_arc), ctx, tool_root_id);
    assert_eq!(tool.name(), "spawn_agent");
    assert!(!tool.description().is_empty());

    let args = serde_json::json!({"prompt": "tool test prompt"});
    let result = local_set
        .run_until(async { tool.execute(&args).await })
        .await
        .expect("tool execute should succeed");
    assert!(
        result.contains("thread_id"),
        "tool result should contain thread_id: {result}"
    );
}
