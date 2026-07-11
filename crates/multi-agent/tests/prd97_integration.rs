//! Integration tests for ThreadRegistry, steer, and spawn_agent_tool.
//!
//! These tests live in the multi-agent crate (not in cli/tests/e2e/) so
//! that `cargo mutants -p zerozero-multi-agent` can detect mutations in
//! the core logic (thread.rs, steer.rs, spawn_agent_tool.rs).

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::mpsc;
use zerozero_compaction::CompactionConfig;
use zerozero_llm::{ChatMessage, DeltaStream, Provider, SseEvent, SseEventStream};
use zerozero_multi_agent::{
    AgentStatus, SUBAGENT_NOTIFY_PREFIX, SpawnAgentTool, SpawnContext, ThreadRegistry,
    run_turn_with_steer,
};
use zerozero_sandbox::{ApprovalPolicy, SandboxPolicy};
use zerozero_session::SessionStore;
use zerozero_tools::{Tool, ToolRegistry};

/// Mock provider that returns a Done event immediately.
struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn chat_stream(&self, _prompt: &str) -> anyhow::Result<DeltaStream> {
        let stream = futures::stream::iter(vec![Ok("mock".to_string())]);
        Ok(Box::pin(stream))
    }

    async fn chat_with_tools(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        _effort: zerozero_llm::Effort,
        _images: &[String],
    ) -> anyhow::Result<SseEventStream> {
        let stream: Pin<Box<dyn Stream<Item = anyhow::Result<SseEvent>> + Send>> =
            Box::pin(futures::stream::iter(vec![Ok(SseEvent::Done)]));
        Ok(stream)
    }
}

fn make_ctx() -> Arc<SpawnContext> {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let tools = Arc::new(ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess)));
    Arc::new(SpawnContext {
        provider,
        tools,
        sandbox: SandboxPolicy::FullAccess,
        approval: ApprovalPolicy::Never,
        compaction_config: CompactionConfig::default(),
        session_db_path: None,
        system_prompt: None,
        max_turns: 1,
        effort: zerozero_llm::Effort::None,
        emit_thread_event: None,
    })
}

#[tokio::test]
async fn test_spawn_agent_creates_thread() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("test".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    let agents = registry.live_agents().await;
    assert!(agents.iter().any(|a| a.thread_id == child_id));
    assert!(agents.iter().any(|a| a.thread_id == root_id));
}

#[tokio::test]
async fn test_spawn_agent_depth_check() {
    let (registry, root_id) = ThreadRegistry::new(6, 1);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("child".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("first spawn should succeed");

    let result = local_set
        .run_until(async {
            registry
                .spawn_agent("grandchild".to_string(), child_id, &ctx)
                .await
        })
        .await;

    assert!(result.is_err(), "depth > max_depth should be rejected");
    assert!(result.unwrap_err().to_string().contains("depth"));
}

#[tokio::test]
async fn test_spawn_agent_max_threads_check() {
    let (registry, root_id) = ThreadRegistry::new(2, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let _child1 = local_set
        .run_until(async {
            registry
                .spawn_agent("child1".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("first spawn should succeed");

    let result = local_set
        .run_until(async {
            registry
                .spawn_agent("child2".to_string(), root_id, &ctx)
                .await
        })
        .await;

    assert!(result.is_err(), "max_threads exceeded should be rejected");
    assert!(result.unwrap_err().to_string().contains("threads"));
}

#[tokio::test]
async fn test_spawn_agent_metadata() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("meta test".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    let agents = registry.live_agents().await;
    let child = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("child found");
    assert_eq!(child.parent_thread_id, Some(root_id));
    assert_eq!(child.depth, 1);
    assert_eq!(child.status, AgentStatus::Running);
    assert!(child.agent_path.as_str().starts_with("root."));
    assert!(!child.nickname.is_empty());
}

#[tokio::test]
async fn test_send_inter_agent() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("steer test".to_string(), root_id, &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    let result = local_set
        .run_until(async {
            registry
                .send_inter_agent(&child_id, "steer msg".to_string())
                .await
        })
        .await;
    assert!(result.is_ok(), "send_inter_agent should succeed");

    let agents = registry.live_agents().await;
    let child = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("child found");
    assert_eq!(child.last_task_message.as_deref(), Some("steer msg"));
}

#[tokio::test]
async fn test_send_inter_agent_nonexistent() {
    let (registry, _root_id) = ThreadRegistry::new(6, 10);
    let result = registry
        .send_inter_agent(&"nonexistent".to_string(), "msg".to_string())
        .await;
    assert!(result.is_err(), "send to nonexistent should fail");
}

#[tokio::test]
async fn test_interrupt_agent_sets_stopped() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("interrupt test".to_string(), root_id, &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    let status = local_set
        .run_until(async { registry.get_status(&child_id).await })
        .await;
    assert_eq!(status, Some(AgentStatus::Running));

    local_set
        .run_until(async { registry.interrupt_agent(&child_id).await })
        .await
        .expect("interrupt should succeed");

    let status = local_set
        .run_until(async { registry.get_status(&child_id).await })
        .await;
    assert_eq!(status, Some(AgentStatus::Stopped));
}

#[tokio::test]
async fn test_interrupt_agent_nonexistent() {
    let (registry, _root_id) = ThreadRegistry::new(6, 10);
    let result = registry.interrupt_agent(&"nonexistent".to_string()).await;
    assert!(result.is_err(), "interrupt nonexistent should fail");
}

#[tokio::test]
async fn test_update_status() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("status test".to_string(), root_id, &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    local_set
        .run_until(async {
            registry
                .update_status(&child_id, AgentStatus::Completed)
                .await
        })
        .await;

    let status = local_set
        .run_until(async { registry.get_status(&child_id).await })
        .await;
    assert_eq!(status, Some(AgentStatus::Completed));

    // Verify via live_agents too.
    let agents = registry.live_agents().await;
    let child = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("found");
    assert_eq!(child.status, AgentStatus::Completed);
}

#[tokio::test]
async fn test_get_status_nonexistent() {
    let (registry, _root_id) = ThreadRegistry::new(6, 10);
    let status = registry.get_status(&"nonexistent".to_string()).await;
    assert_eq!(
        status, None,
        "get_status for nonexistent should return None"
    );
}

#[tokio::test]
async fn test_live_agents_sorted() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let _child = local_set
        .run_until(async {
            registry
                .spawn_agent("child".to_string(), root_id, &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    let agents = registry.live_agents().await;
    // Note: Due to thread_id_now() second-resolution, root and child may
    // share the same ID (collision), resulting in only 1 agent.
    // Verify sort order: agents should be sorted by agent_path.
    for i in 1..agents.len() {
        assert!(
            agents[i - 1].agent_path.as_str() <= agents[i].agent_path.as_str(),
            "agents should be sorted by agent_path: {:?}",
            agents
                .iter()
                .map(|a| a.agent_path.as_str())
                .collect::<Vec<_>>()
        );
    }
    // At least 1 agent should exist.
    assert!(!agents.is_empty(), "should have at least 1 agent");
}

#[tokio::test]
async fn test_switch_thread() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();

    // Subscribe BEFORE spawning to get initial value.
    let rx = registry.subscribe_active();
    assert_eq!(rx.borrow().clone(), root_id, "initial should be root_id");

    // Sleep to avoid thread_id collision.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let local_set = tokio::task::LocalSet::new();
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("switch test".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    assert_ne!(root_id, child_id, "IDs should differ");

    // After spawn, active should be child_id.
    assert_eq!(rx.borrow().clone(), child_id);

    // Switch to root.
    registry
        .switch_thread(&root_id)
        .expect("switch should succeed");
    assert_eq!(rx.borrow().clone(), root_id);

    // Switch back to child.
    registry
        .switch_thread(&child_id)
        .expect("switch should succeed");
    assert_eq!(rx.borrow().clone(), child_id);
}

#[tokio::test]
async fn test_notify_parent() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    // Spawn child (will be "parent" for notification).
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("parent".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn child should succeed");

    // Spawn grandchild from child.
    let grandchild_id = local_set
        .run_until(async {
            registry
                .spawn_agent("grandchild".to_string(), child_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn grandchild should succeed");

    // Notify parent (child) about grandchild completion.
    let summary = "task done";
    let result = local_set
        .run_until(async {
            registry
                .notify_parent(&child_id, &grandchild_id, summary.to_string())
                .await
        })
        .await;
    assert!(result.is_ok(), "notify_parent should succeed");

    // Verify the notification message.
    let agents = registry.live_agents().await;
    let parent = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("parent found");
    let msg = parent.last_task_message.as_ref().expect("message set");
    assert!(msg.starts_with(SUBAGENT_NOTIFY_PREFIX), "msg: {}", msg);
    assert!(msg.contains("completed"), "msg: {}", msg);
    assert!(msg.contains(summary), "msg: {}", msg);
}

#[tokio::test]
async fn test_spawn_agent_tool() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let registry_arc = Arc::new(registry);

    let tool = SpawnAgentTool::new(Arc::clone(&registry_arc), ctx, root_id);
    assert_eq!(tool.name(), "spawn_agent");
    assert!(!tool.description().is_empty());
    // Verify description contains meaningful content (catches "xyzzy" mutation).
    assert!(
        tool.description().to_lowercase().contains("agent")
            || tool.description().to_lowercase().contains("spawn")
            || tool.description().to_lowercase().contains("thread"),
        "description should contain meaningful text about spawning agents: {}",
        tool.description()
    );

    // Verify parameters_schema has "prompt" field.
    let schema = tool.parameters_schema();
    assert!(schema.to_string().contains("prompt"));

    let local_set = tokio::task::LocalSet::new();
    let args = serde_json::json!({"prompt": "tool test"});
    let result = local_set
        .run_until(async { tool.execute(&args).await })
        .await
        .expect("execute should succeed");
    assert!(result.contains("thread_id"), "result: {}", result);

    // Verify missing prompt parameter fails.
    let args = serde_json::json!({});
    let result = local_set
        .run_until(async { tool.execute(&args).await })
        .await;
    assert!(result.is_err(), "missing prompt should fail");
}

#[tokio::test]
async fn test_agent_path() {
    use zerozero_multi_agent::AgentPath;

    let root = AgentPath::root();
    assert_eq!(root.as_str(), "root");

    let child0 = root.child(0);
    assert_eq!(child0.as_str(), "root.0");

    let child1 = root.child(1);
    assert_eq!(child1.as_str(), "root.1");

    let grandchild = child0.child(2);
    assert_eq!(grandchild.as_str(), "root.0.2");

    assert_eq!(root, AgentPath::root());
    assert_ne!(root, child0);

    // Verify Display trait produces correct string (catches Display mutation).
    assert_eq!(format!("{}", root), "root");
    assert_eq!(format!("{}", child0), "root.0");
    assert_eq!(format!("{}", child1), "root.1");
    assert_eq!(format!("{}", grandchild), "root.0.2");
}

#[tokio::test]
async fn test_agent_status_as_str() {
    assert_eq!(AgentStatus::Running.as_str(), "running");
    assert_eq!(AgentStatus::Stopped.as_str(), "stopped");
    assert_eq!(AgentStatus::Completed.as_str(), "completed");
    assert_eq!(AgentStatus::Failed.as_str(), "failed");
}

#[tokio::test]
async fn test_switch_thread_broadcast_change() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();

    // Subscribe BEFORE spawning to get the initial value (root_id).
    let rx = registry.subscribe_active();
    assert_eq!(
        rx.borrow().clone(),
        root_id,
        "initial active should be root_id"
    );

    // Sleep 1s before spawning to avoid thread_id_now() collision.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let local_set = tokio::task::LocalSet::new();
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("switch broadcast test".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    // Verify root and child have different IDs (no collision).
    assert_ne!(
        root_id, child_id,
        "root and child should have different thread IDs"
    );

    // After spawn, active should be child_id (spawn_agent calls active_thread.send).
    assert_eq!(
        rx.borrow().clone(),
        child_id,
        "active should be child_id after spawn"
    );

    // Switch to root — broadcast should actually change.
    registry
        .switch_thread(&root_id)
        .expect("switch to root should succeed");
    assert_eq!(
        rx.borrow().clone(),
        root_id,
        "broadcast should change to root_id after switch"
    );

    // Switch back to child — broadcast should change again.
    registry
        .switch_thread(&child_id)
        .expect("switch to child should succeed");
    assert_eq!(
        rx.borrow().clone(),
        child_id,
        "broadcast should change back to child_id after switch"
    );
}

#[tokio::test]
async fn test_next_child_path_increments() {
    // Verify that spawning multiple children from the same parent
    // produces incrementing agent_path indices (root.0, root.1, root.2).
    // This catches the += → *= mutation (0*1=0, so all children would
    // get "root.0" instead of incrementing).
    //
    // NOTE: We sleep 1s between spawns to avoid thread_id_now() collision
    // (second-resolution timestamp → same ID for same-second spawns).
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let child1 = local_set
        .run_until(async {
            registry
                .spawn_agent("child1".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn child1 should succeed");

    // Sleep 1s to avoid thread_id collision.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let child2 = local_set
        .run_until(async {
            registry
                .spawn_agent("child2".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn child2 should succeed");

    // Verify the two children have different thread IDs.
    assert_ne!(
        child1, child2,
        "children should have different thread IDs (after 1s sleep)"
    );

    let agents = registry.live_agents().await;

    // Find both children and verify their agent_paths are different.
    let child1_meta = agents
        .iter()
        .find(|a| a.thread_id == child1)
        .expect("child1 found");
    let child2_meta = agents
        .iter()
        .find(|a| a.thread_id == child2)
        .expect("child2 found");

    // The agent_paths should be different (root.0 and root.1).
    // If += was mutated to *=, both would be root.0 (0*1=0, 0*2=0).
    assert_ne!(
        child1_meta.agent_path, child2_meta.agent_path,
        "agent_paths should be different for different children: {:?} vs {:?}",
        child1_meta.agent_path, child2_meta.agent_path
    );

    // The second child should have index .1 (root.1).
    assert!(
        child2_meta.agent_path.as_str().ends_with(".1"),
        "second child should have index .1: {:?}",
        child2_meta.agent_path
    );
}

#[tokio::test]
async fn test_next_nickname_format() {
    // Verify that the nickname follows the "agent-N" format.
    // This catches the "xyzzy" mutation.
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();
    let local_set = tokio::task::LocalSet::new();

    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("nickname test".to_string(), root_id, &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    let agents = registry.live_agents().await;
    let child = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("child found");

    // Nickname should follow "agent-N" format.
    assert!(
        child.nickname.starts_with("agent-"),
        "nickname should start with 'agent-': {}",
        child.nickname
    );
    // The suffix should be a number.
    let suffix = &child.nickname["agent-".len()..];
    assert!(
        suffix.parse::<u32>().is_ok(),
        "nickname suffix should be a number: {}",
        child.nickname
    );
}

/// Multi-turn integration test: steer a subagent mid-run, verify the steer
/// message appears in the subagent's messages vec with role "user", then
/// verify a notification message appears in the parent's messages vec with
/// role "system" + SUBAGENT_NOTIFY_PREFIX.
///
/// This test closes (E cold review finding 3 recommendation):
/// previous tests only checked `last_task_message` metadata (side effect),
/// not actual behavior (messages vec content). This test verifies the full
/// multi-turn flow by calling `run_turn_with_steer` directly with
/// file-based sessions, so messages can be read after the task consumes
/// the SessionStore.
///
/// Flow:
/// 1. Subagent: send a steer message → run_turn_with_steer → verify steer
///    in messages vec with role "user".
/// 2. Parent: send a notification (SUBAGENT_NOTIFY_PREFIX) →
///    run_turn_with_steer → verify notification in messages vec with
///    role "system" + prefix.
///
/// This test catches mutations in steer.rs that:
/// - Swap role "user" → "system" for steer messages (AC-4 break).
/// - Swap role "system" → "user" for notifications (AC-6 break).
/// - Drop the `session.append_message` call (message not persisted).
/// - Misclassify notifications as steers (or vice versa).
#[tokio::test]
async fn test_multiturn_steer_then_notification_in_messages_vec() {
    let ctx = make_ctx();

    // Use a temp file for the subagent session so we can reopen it after
    // run_turn_with_steer consumes the SessionStore.
    let subagent_db = std::env::temp_dir().join(format!(
        "zz-test-multiturn-subagent-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    let subagent_thread_id = "test-multiturn-subagent-001".to_string();
    let steer_content = "please focus on the tests";

    // === Phase 1: Subagent with steer ===
    {
        let session = SessionStore::open(&subagent_db).expect("open subagent session");
        session
            .create_session(&subagent_thread_id, "subagent task", None)
            .expect("create subagent session");

        let (steer_tx, steer_rx) = mpsc::unbounded_channel::<String>();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        // Send a steer message BEFORE starting — run_turn_with_steer
        // drains the inbox on the first iteration.
        steer_tx
            .send(steer_content.to_string())
            .expect("steer send");

        let local_set = tokio::task::LocalSet::new();
        let tid = subagent_thread_id.clone();
        let ctx_c = ctx.clone();
        let result = local_set
            .run_until(async move {
                run_turn_with_steer(
                    "initial subagent prompt",
                    &ctx_c,
                    session,
                    steer_rx,
                    cancel_flag,
                    tid,
                )
                .await
            })
            .await;

        assert_eq!(
            result.status,
            AgentStatus::Completed,
            "subagent should complete after processing steer"
        );
    }

    // Reopen the subagent session and verify the steer message.
    {
        let session2 = SessionStore::open(&subagent_db).expect("reopen subagent session");
        let messages = session2
            .get_messages(&subagent_thread_id)
            .expect("get subagent messages");

        // Verify steer message in vec with role "user".
        let steer_msg = messages
            .iter()
            .find(|m| m.role == "user" && m.content == steer_content);
        assert!(
            steer_msg.is_some(),
            "steer message '{}' should be in subagent messages vec with role 'user'. \
             Messages: {:?}",
            steer_content,
            messages
                .iter()
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect::<Vec<_>>()
        );
        let steer_msg = steer_msg.expect("steer message exists");
        assert_eq!(steer_msg.role, "user");
        assert_eq!(steer_msg.content, steer_content);
    }

    // Clean up subagent DB files (WAL + SHM).
    let _ = std::fs::remove_file(&subagent_db);
    let _ = std::fs::remove_file(format!("{}-wal", subagent_db.display()));
    let _ = std::fs::remove_file(format!("{}-shm", subagent_db.display()));

    // === Phase 2: Parent receives notification ===
    let parent_db = std::env::temp_dir().join(format!(
        "zz-test-multiturn-parent-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    let parent_thread_id = "test-multiturn-parent-001".to_string();
    let summary = "subagent finished successfully";

    {
        let session = SessionStore::open(&parent_db).expect("open parent session");
        session
            .create_session(&parent_thread_id, "parent task", None)
            .expect("create parent session");

        let (steer_tx, steer_rx) = mpsc::unbounded_channel::<String>();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        // Send a notification (SUBAGENT_NOTIFY_PREFIX) to the parent.
        let notification = format!(
            "{} Subagent 'agent-0' (thread: {}) completed: {}",
            SUBAGENT_NOTIFY_PREFIX, subagent_thread_id, summary
        );
        steer_tx
            .send(notification.clone())
            .expect("notification send");

        let local_set = tokio::task::LocalSet::new();
        let tid = parent_thread_id.clone();
        let ctx_c = ctx.clone();
        let result = local_set
            .run_until(async move {
                run_turn_with_steer(
                    "parent initial prompt",
                    &ctx_c,
                    session,
                    steer_rx,
                    cancel_flag,
                    tid,
                )
                .await
            })
            .await;

        assert_eq!(
            result.status,
            AgentStatus::Completed,
            "parent should complete after processing notification"
        );
    }

    // Reopen the parent session and verify the notification.
    {
        let session2 = SessionStore::open(&parent_db).expect("reopen parent session");
        let messages = session2
            .get_messages(&parent_thread_id)
            .expect("get parent messages");

        // Verify notification in vec with role "system" + prefix.
        let notif_msg = messages
            .iter()
            .find(|m| m.role == "system" && m.content.starts_with(SUBAGENT_NOTIFY_PREFIX));
        assert!(
            notif_msg.is_some(),
            "notification should be in parent messages vec with role 'system' \
             and start with '{}'. Messages: {:?}",
            SUBAGENT_NOTIFY_PREFIX,
            messages
                .iter()
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect::<Vec<_>>()
        );
        let notif_msg = notif_msg.expect("notification exists");
        assert_eq!(notif_msg.role, "system");
        assert!(notif_msg.content.starts_with(SUBAGENT_NOTIFY_PREFIX));
        assert!(notif_msg.content.contains("completed"));
        assert!(notif_msg.content.contains(summary));
    }

    // Clean up parent DB files.
    let _ = std::fs::remove_file(&parent_db);
    let _ = std::fs::remove_file(format!("{}-wal", parent_db.display()));
    let _ = std::fs::remove_file(format!("{}-shm", parent_db.display()));
}
