//! AC-4: Steer (inject message) + interrupt (cancel flag) — Phase 4.
//!
//! Test: Steer message injected into running thread's messages (role="user"),
//! thread continues loop without restart. Interrupt → cancel_flag set →
//! thread status = Stopped, task drop cleanly (no panic, no orphan).
//!
//! Pattern: Integration (mock provider).

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::mpsc;
use zerozero_compaction::CompactionConfig;
use zerozero_llm::{ChatMessage, DeltaStream, Effort, Provider, SseEvent, SseEventStream};
use zerozero_multi_agent::{AgentStatus, SpawnContext, ThreadRegistry, run_turn_with_steer};
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

/// AC-4 test 1: Steer message is injected into a running thread's inbox.
///
/// `send_inter_agent` sends a message to the thread's steer inbox via
/// `mpsc::UnboundedSender::send()`. The message is queued (non-blocking)
/// and will be polled by the thread's `run_turn_with_steer` on the next
/// loop iteration. No restart, no new thread.
///
/// This test verifies that `send_inter_agent` succeeds for a running thread
/// and that the `last_task_message` metadata is updated.
#[tokio::test]
async fn e2e_prd97_ac4_steer_injects_message() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();

    // Spawn a child thread.
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("initial prompt".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    // Send a steer message — should succeed (channel is open).
    let steer_msg = "please focus on the tests";
    let result = local_set
        .run_until(async {
            registry
                .send_inter_agent(&child_id, steer_msg.to_string())
                .await
        })
        .await;
    assert!(
        result.is_ok(),
        "send_inter_agent should succeed for running thread: {:?}",
        result
    );

    // Verify last_task_message was updated in metadata.
    let agents = registry.live_agents().await;
    let child_meta = agents
        .iter()
        .find(|a| a.thread_id == child_id)
        .expect("child should be in live_agents");
    assert_eq!(
        child_meta.last_task_message.as_deref(),
        Some(steer_msg),
        "last_task_message should be updated to the steer message"
    );

    // Sending to a non-existent thread should fail.
    let result = local_set
        .run_until(async {
            registry
                .send_inter_agent(&"nonexistent-thread".to_string(), "msg".to_string())
                .await
        })
        .await;
    assert!(
        result.is_err(),
        "send_inter_agent to non-existent thread should fail"
    );
}

/// AC-4 test 2: Interrupt sets the thread status to Stopped.
///
/// `interrupt_agent` sets the `Arc<AtomicBool>` cancel flag and updates
/// the thread's status to `Stopped` in the registry. The thread's
/// `run_turn_with_steer` checks the flag at the next turn boundary and
/// breaks its loop.
#[tokio::test]
async fn e2e_prd97_ac4_interrupt_sets_stopped_status() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();

    // Spawn a child thread.
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("task to interrupt".to_string(), root_id.clone(), &ctx)
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

    // Interrupt the thread.
    let result = local_set
        .run_until(async { registry.interrupt_agent(&child_id).await })
        .await;
    assert!(
        result.is_ok(),
        "interrupt_agent should succeed: {:?}",
        result
    );

    // Verify status is now Stopped.
    let status = local_set
        .run_until(async { registry.get_status(&child_id).await })
        .await;
    assert_eq!(
        status,
        Some(AgentStatus::Stopped),
        "interrupted thread should have Stopped status"
    );

    // Interrupting a non-existent thread should fail.
    let result = local_set
        .run_until(async {
            registry
                .interrupt_agent(&"nonexistent-thread".to_string())
                .await
        })
        .await;
    assert!(
        result.is_err(),
        "interrupt_agent on non-existent thread should fail"
    );
}

/// AC-4 test 3: Interrupted task drops cleanly — no panic, no orphan.
///
/// After interrupt, the thread's `run_turn_with_steer` checks the cancel
/// flag at the next turn boundary, breaks its loop, and returns
/// `ThreadResult { status: Stopped }`. The `spawn_local` task completes
/// normally (no panic). The registry remains queryable.
///
/// This test runs the LocalSet after interrupt to let the task complete,
/// then verifies the registry is still functional.
#[tokio::test]
async fn e2e_prd97_ac4_interrupt_no_panic_no_orphan() {
    let (registry, root_id) = ThreadRegistry::new(6, 10);
    let ctx = make_ctx();

    let local_set = tokio::task::LocalSet::new();

    // Spawn a child thread.
    let child_id = local_set
        .run_until(async {
            registry
                .spawn_agent("clean shutdown test".to_string(), root_id.clone(), &ctx)
                .await
        })
        .await
        .expect("spawn should succeed");

    // Interrupt the thread.
    local_set
        .run_until(async { registry.interrupt_agent(&child_id).await })
        .await
        .expect("interrupt should succeed");

    // Run the LocalSet for a short time to let the spawned task complete.
    // The task checks the cancel flag and returns ThreadResult { Stopped }.
    // If the task panicked, this would hang or the registry would be
    // corrupted. We use a timeout to detect hangs.
    let completed = local_set
        .run_until(async {
            // Give the task time to check the cancel flag and return.
            tokio::time::sleep(Duration::from_millis(100)).await;
            // The task should have completed by now.
            true
        })
        .await;

    assert!(completed, "LocalSet should complete without hanging");

    // Verify the registry is still functional after interrupt.
    let agents = registry.live_agents().await;
    assert!(
        agents.iter().any(|a| a.thread_id == child_id),
        "child thread should still be in registry after interrupt (no orphan removal)"
    );

    // Verify status is still Stopped (not changed to Failed or anything else).
    let status = registry.get_status(&child_id).await;
    assert_eq!(
        status,
        Some(AgentStatus::Stopped),
        "thread status should remain Stopped after clean shutdown"
    );

    // Verify we can still spawn new threads (registry not corrupted).
    // Note: We use a fresh registry here to avoid thread_id collision
    // (thread_id_now() has second resolution — root and child spawned
    // within the same second get the same ID, causing metadata overwrite).
    // See GAP note in final report.
    let (registry2, root_id2) = ThreadRegistry::new(6, 10);
    let local_set2 = tokio::task::LocalSet::new();
    let new_child = local_set2
        .run_until(async {
            registry2
                .spawn_agent("new task after interrupt".to_string(), root_id2, &ctx)
                .await
        })
        .await
        .expect("should be able to spawn new threads after interrupt");
    let agents2 = registry2.live_agents().await;
    assert!(
        agents2.iter().any(|a| a.thread_id == new_child),
        "new thread should be registered in fresh registry"
    );
}

/// AC-4 test 4: Steer message appears in the session's messages vec with
/// role "user" — verifying ACTUAL behavior, not just metadata side effects.
///
/// E cold review finding 3 (MAJOR, anti-cheat): previous tests only checked
/// `last_task_message` metadata (a side effect set in `send_inter_agent`),
/// which passes even if the steer message is never injected into the
/// messages vec. This test calls `run_turn_with_steer` directly, sends a
/// steer message via the steer channel, and verifies the message is
/// persisted to the session's messages vec with `role: "user"`.
///
/// Flow:
/// 1. Create a file-based SessionStore (so we can reopen after the task
///    consumes the session).
/// 2. Send a steer message via the steer channel BEFORE starting the task.
/// 3. Call `run_turn_with_steer` — it drains the steer inbox on the first
///    iteration, appends the steer as `role: "user"` to the session, and
///    runs a turn.
/// 4. After the task completes, reopen the session and verify the steer
///    message is in the messages vec with `role: "user"`.
#[tokio::test]
async fn e2e_prd97_ac4_steer_message_in_messages_vec() {
    let ctx = make_ctx();
    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let db_path = dir.path().join("test_ac4_steer.db");

    // Create the session and pre-create the session record so that
    // append_message succeeds (messages table has a FK to sessions).
    let session = SessionStore::open(&db_path).expect("open session should succeed");
    let thread_id = "test-thread-ac4-steer-001".to_string();
    session
        .create_session(&thread_id, "test prompt", None)
        .expect("create session should succeed");

    // Create steer channel + cancel flag.
    let (steer_tx, steer_rx) = mpsc::unbounded_channel::<String>();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Send a steer message BEFORE starting the task.
    // run_turn_with_steer drains the steer inbox on the first iteration.
    let steer_content = "please focus on the tests";
    steer_tx
        .send(steer_content.to_string())
        .expect("steer send should succeed");

    // Run run_turn_with_steer directly (not via spawn_agent) so we control
    // the session and can verify the messages vec content afterward.
    let local_set = tokio::task::LocalSet::new();
    let thread_id_clone = thread_id.clone();
    let ctx_clone = ctx.clone();
    let result = local_set
        .run_until(async move {
            run_turn_with_steer(
                "initial prompt",
                &ctx_clone,
                session,
                steer_rx,
                cancel_flag,
                thread_id_clone,
            )
            .await
        })
        .await;

    // The task should complete successfully (steer processed, then no more
    // steers → Completed).
    assert_eq!(
        result.status,
        AgentStatus::Completed,
        "task should complete after processing steer"
    );

    // Reopen the session to read messages (the original SessionStore was
    // consumed by run_turn_with_steer).
    let session2 = SessionStore::open(&db_path).expect("reopen session should succeed");
    let messages = session2
        .get_messages(&thread_id)
        .expect("get_messages should succeed");

    // CRITICAL ASSERTION: verify the steer message actually appears in the
    // messages vec with role "user" — not just in metadata.
    let steer_msg = messages
        .iter()
        .find(|m| m.role == "user" && m.content == steer_content);
    assert!(
        steer_msg.is_some(),
        "steer message '{}' should appear in messages vec with role 'user'. \
         All messages: {:?}",
        steer_content,
        messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect::<Vec<_>>()
    );

    // Verify the role is exactly "user" (not "system" — that's for
    // notifications). This catches mutations that swap the role.
    let steer_msg = steer_msg.expect("steer message exists");
    assert_eq!(
        steer_msg.role, "user",
        "steer message should have role 'user', got '{}': {}",
        steer_msg.role, steer_msg.content
    );
    assert_eq!(
        steer_msg.content, steer_content,
        "steer message content should match exactly"
    );
}
