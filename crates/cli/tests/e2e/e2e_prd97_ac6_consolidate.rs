//! AC-6: Consolidate response — Phase 6 (multi-agent + core crate).
//!
//! Test: Subagent run_turn complete → parent messages have system message
//! "[ZZ::SUBAGENT_NOTIFY] Subagent 'agent-0' completed: {summary}",
//! subagent status = Completed in registry + session schema.
//!
//! Pattern: Integration (mock provider).

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::mpsc;
use zerozero_compaction::CompactionConfig;
use zerozero_llm::{ChatMessage, DeltaStream, Effort, Provider, SseEvent, SseEventStream};
use zerozero_multi_agent::{
    AgentStatus, SUBAGENT_NOTIFY_PREFIX, SpawnContext, ThreadRegistry, run_turn_with_steer,
};
use zerozero_sandbox::{ApprovalPolicy, SandboxPolicy};
use zerozero_session::SessionStore;
use zerozero_tools::ToolRegistry;

/// Mock provider that returns a Done event immediately — no real API call.
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
        _effort: Effort,
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
        effort: Effort::None,
        emit_thread_event: None,
    })
}

/// AC-6 test 1: Parent receives notification when child completes.
///
/// When `notify_parent` is called, it injects a message with the
/// `[ZZ::SUBAGENT_NOTIFY]` prefix into the parent's steer inbox via
/// `send_inter_agent`. The message format is:
/// `[ZZ::SUBAGENT_NOTIFY] Subagent 'nickname' (thread: child_id) completed: summary`
///
/// Note: The root thread has no steer handle (no `ThreadHandle` in the
/// `handles` map — only spawned threads get handles). So we test
/// `notify_parent` with a child thread as the "parent" by using
/// max_depth=10 to allow spawning a grandchild.
#[tokio::test]
async fn e2e_prd97_ac6_parent_receives_notification() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();

    // Spawn a child (depth 1) — this will be the "parent" for notification.
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("parent task".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn child should succeed");

    // Spawn a grandchild (depth 2) from the child.
    let grandchild_id = local_set
        .run_until(async {
            registry
                .spawn_agent("grandchild task".to_string(), child_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn grandchild should succeed");

    // Call notify_parent — injects notification into child's steer inbox.
    let summary = "Task completed successfully with result X";
    let result = local_set
        .run_until(async {
            registry
                .notify_parent(&child_id, &grandchild_id, summary.to_string())
                .await
        })
        .await;
    assert!(
        result.is_ok(),
        "notify_parent should succeed when parent has a steer handle: {:?}",
        result
    );

    // Verify the notification message was injected by checking that
    // last_task_message was updated (send_inter_agent updates it).
    let agents = registry.live_agents().await;
    let parent_meta = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("parent (child) should be in live_agents");
    let last_msg = parent_meta
        .last_task_message
        .as_ref()
        .expect("last_task_message should be set");
    assert!(
        last_msg.starts_with(SUBAGENT_NOTIFY_PREFIX),
        "notification message should start with SUBAGENT_NOTIFY_PREFIX '{}', got: {}",
        SUBAGENT_NOTIFY_PREFIX,
        last_msg
    );
    assert!(
        last_msg.contains("completed"),
        "notification should contain 'completed': {}",
        last_msg
    );
    assert!(
        last_msg.contains(summary),
        "notification should contain the summary '{}': {}",
        summary,
        last_msg
    );
}

/// AC-6 test 2: Subagent status is updated to Completed in the registry.
///
/// After a subagent completes, `update_status` should set the thread's
/// status to `Completed` in the registry's in-memory metadata.
#[tokio::test]
async fn e2e_prd97_ac6_subagent_status_completed_in_registry() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();

    // Spawn a child.
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("task to complete".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    // Verify initial status is Running.
    let status = local_set
        .run_until(async { registry.get_status(&child_id).await })
        .await;
    assert_eq!(
        status,
        Some(AgentStatus::Running),
        "newly spawned thread should have Running status"
    );

    // Simulate completion — update status to Completed.
    local_set
        .run_until(async {
            registry
                .update_status(&child_id, AgentStatus::Completed)
                .await
        })
        .await;

    // Verify status is now Completed.
    let status = local_set
        .run_until(async { registry.get_status(&child_id).await })
        .await;
    assert_eq!(
        status,
        Some(AgentStatus::Completed),
        "completed thread should have Completed status in registry"
    );

    // Also verify via live_agents.
    let agents = registry.live_agents().await;
    let child_meta = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("child should be in live_agents");
    assert_eq!(
        child_meta.status,
        AgentStatus::Completed,
        "live_agents should show Completed status"
    );

    // Test Failed status as well.
    let (registry2, root_id2) = ThreadRegistry::new(6, 10);
    let local_set2 = tokio::task::LocalSet::new();
    let child2_id = local_set2
        .run_until(async {
            registry2
                .spawn_agent("failing task".to_string(), root_id2, &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    local_set2
        .run_until(async {
            registry2
                .update_status(&child2_id, AgentStatus::Failed)
                .await
        })
        .await;

    let status = local_set2
        .run_until(async { registry2.get_status(&child2_id).await })
        .await;
    assert_eq!(
        status,
        Some(AgentStatus::Failed),
        "failed thread should have Failed status in registry"
    );
}

/// AC-6 test 3: Subagent status is updated to "completed" in session schema.
///
/// After a subagent completes, `SessionStore::update_agent_status` should
/// update the session's `agent_status` column to "completed". This is the
/// persistent status (survives restart), complementing the in-memory
/// registry status.
#[tokio::test]
async fn e2e_prd97_ac6_subagent_status_completed_in_session() {
    let store = SessionStore::open_in_memory().expect("open_in_memory should succeed");

    // Create a subagent session with initial "running" status.
    store
        .create_session_with_thread(
            "subagent-session-1",
            "subagent task",
            Some("model-x"),
            Some("root-session"),
            "root.0",
            1,
            "agent-0",
        )
        .expect("create subagent session should succeed");

    // Verify initial status is "running".
    let sessions = store.list_sessions().expect("list should succeed");
    let meta = sessions
        .iter()
        .find(|s| s.id == "subagent-session-1")
        .expect("session should exist");
    assert_eq!(
        meta.agent_status.as_deref(),
        Some("running"),
        "initial agent_status should be 'running'"
    );

    // Update status to "completed" (simulating subagent completion).
    store
        .update_agent_status("subagent-session-1", "completed")
        .expect("update_agent_status should succeed");

    // Verify status is now "completed".
    let sessions = store.list_sessions().expect("list should succeed");
    let meta = sessions
        .iter()
        .find(|s| s.id == "subagent-session-1")
        .expect("session should exist");
    assert_eq!(
        meta.agent_status.as_deref(),
        Some("completed"),
        "agent_status should be 'completed' after update"
    );

    // Also test "failed" status.
    store
        .update_agent_status("subagent-session-1", "failed")
        .expect("update to failed should succeed");
    let sessions = store.list_sessions().expect("list should succeed");
    let meta = sessions
        .iter()
        .find(|s| s.id == "subagent-session-1")
        .expect("session should exist");
    assert_eq!(
        meta.agent_status.as_deref(),
        Some("failed"),
        "agent_status should be 'failed' after update"
    );

    // Also test "stopped" status (interrupted).
    store
        .update_agent_status("subagent-session-1", "stopped")
        .expect("update to stopped should succeed");
    let sessions = store.list_sessions().expect("list should succeed");
    let meta = sessions
        .iter()
        .find(|s| s.id == "subagent-session-1")
        .expect("session should exist");
    assert_eq!(
        meta.agent_status.as_deref(),
        Some("stopped"),
        "agent_status should be 'stopped' after update"
    );
}

/// AC-6 test 4: Notification message appears in the parent's messages vec
/// with role "system" + SUBAGENT_NOTIFY_PREFIX — verifying ACTUAL behavior,
/// not just metadata side effects.
///
/// E cold review finding 3 (MAJOR, anti-cheat): previous tests only checked
/// `last_task_message` metadata (a side effect set in `send_inter_agent`),
/// which passes even if the notification is never injected into the
/// parent's messages vec. This test calls `run_turn_with_steer` directly
/// (simulating the parent thread), sends a notification via the steer
/// channel, and verifies the message is persisted to the session's
/// messages vec with `role: "system"` and content starting with
/// SUBAGENT_NOTIFY_PREFIX.
///
/// Flow:
/// 1. Create a file-based SessionStore (so we can reopen after the task
///    consumes the session).
/// 2. Send a notification message (with SUBAGENT_NOTIFY_PREFIX) via the
///    steer channel BEFORE starting the task.
/// 3. Call `run_turn_with_steer` — it drains the steer inbox on the first
///    iteration, classifies the message as a notification (starts with
///    SUBAGENT_NOTIFY_PREFIX), appends it as `role: "system"` to the
///    session, and runs a turn.
/// 4. After the task completes, reopen the session and verify the
///    notification message is in the messages vec with `role: "system"`.
#[tokio::test]
async fn e2e_prd97_ac6_notification_in_messages_vec() {
    let ctx = make_ctx();
    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let db_path = dir.path().join("test_ac6_notify.db");

    // Create the session and pre-create the session record.
    let session = SessionStore::open(&db_path).expect("open session should succeed");
    let thread_id = "test-thread-ac6-notify-001".to_string();
    session
        .create_session(&thread_id, "parent task", None)
        .expect("create session should succeed");

    // Create steer channel + cancel flag.
    let (steer_tx, steer_rx) = mpsc::unbounded_channel::<String>();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Send a notification message (with SUBAGENT_NOTIFY_PREFIX) BEFORE
    // starting the task. run_turn_with_steer drains the steer inbox on
    // the first iteration and classifies it as a notification.
    let child_thread_id = "test-child-001";
    let summary = "Task completed successfully with result X";
    let notification = format!(
        "{} Subagent 'agent-0' (thread: {}) completed: {}",
        SUBAGENT_NOTIFY_PREFIX, child_thread_id, summary
    );
    steer_tx
        .send(notification.clone())
        .expect("notification send should succeed");

    // Run run_turn_with_steer directly (simulating the parent thread).
    let local_set = tokio::task::LocalSet::new();
    let thread_id_clone = thread_id.clone();
    let ctx_clone = ctx.clone();
    let result = local_set
        .run_until(async move {
            run_turn_with_steer(
                "parent initial prompt",
                &ctx_clone,
                session,
                steer_rx,
                cancel_flag,
                thread_id_clone,
            )
            .await
        })
        .await;

    // The task should complete (notification processed, no user steer →
    // after first turn, no more steers → Completed).
    assert_eq!(
        result.status,
        AgentStatus::Completed,
        "parent task should complete after processing notification"
    );

    // Reopen the session to read messages.
    let session2 = SessionStore::open(&db_path).expect("reopen session should succeed");
    let messages = session2
        .get_messages(&thread_id)
        .expect("get_messages should succeed");

    // CRITICAL ASSERTION: verify the notification message actually appears
    // in the messages vec with role "system" — not just in metadata.
    let notif_msg = messages
        .iter()
        .find(|m| m.role == "system" && m.content.starts_with(SUBAGENT_NOTIFY_PREFIX));
    assert!(
        notif_msg.is_some(),
        "notification message should appear in messages vec with role 'system' \
         and start with SUBAGENT_NOTIFY_PREFIX '{}'. All messages: {:?}",
        SUBAGENT_NOTIFY_PREFIX,
        messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect::<Vec<_>>()
    );

    // Verify the role is exactly "system" (not "user" — that's for steers).
    // This catches mutations that swap the role.
    let notif_msg = notif_msg.expect("notification message exists");
    assert_eq!(
        notif_msg.role, "system",
        "notification message should have role 'system', got '{}': {}",
        notif_msg.role, notif_msg.content
    );

    // Verify the content contains the SUBAGENT_NOTIFY_PREFIX.
    assert!(
        notif_msg.content.starts_with(SUBAGENT_NOTIFY_PREFIX),
        "notification content should start with '{}': {}",
        SUBAGENT_NOTIFY_PREFIX,
        notif_msg.content
    );

    // Verify the content contains "completed" and the summary.
    assert!(
        notif_msg.content.contains("completed"),
        "notification should contain 'completed': {}",
        notif_msg.content
    );
    assert!(
        notif_msg.content.contains(summary),
        "notification should contain summary '{}': {}",
        summary,
        notif_msg.content
    );
}
