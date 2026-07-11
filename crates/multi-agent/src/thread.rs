//! Thread registry for interactive multi-agent orchestration).
//!
//! Replaces the fire-and-forget `MultiAgentOrchestrator` batch model with an
//! interactive thread model: each subagent runs in its own `spawn_local` task
//! on a shared `LocalSet` , owns its `SessionStore` (rusqlite
//! `Connection` = !Send → LocalSet), and can be steered/interrupted at
//! turn boundaries.
//!
//! ## Cancel semantics (C review §3.2, action item #1)
//!
//! Interrupt is **turn-boundary cancel**, not immediate. `interrupt_agent`
//! sets an `Arc<AtomicBool>` flag. The thread checks this flag between
//! `run_turn` calls (before each LLM call). If the flag is set, the thread
//! breaks its loop and returns `ThreadResult { status: Stopped }`.
//!
//! **Known limitations (acceptable for MVP):**
//! - Cancel during an in-flight LLM SSE stream (30+ seconds) takes effect
//!   at the next turn boundary, not mid-stream.
//! - Cancel during tool execution (e.g. bash command) takes effect after
//!   the tool completes.
//! - If future cycles need immediate mid-stream cancel → open a new GAP
//!   and add `tokio-util` `CancellationToken` (design §2.4, §9).
//!
//! Deferred cancel is **safer** than `JoinHandle::abort()` — abort mid-await
//! can leave resources inconsistent (e.g. SQLite connection mid-write).
//!
//! ## LocalSet context requirement (C review §3.2, action item #2)
//!
//! `spawn_agent` calls `tokio::task::spawn_local`, which **panics** if not
//! called within a `LocalSet` context (i.e. inside `local_set.run_until(...)`).
//! Callers must ensure they are running within a `LocalSet` before calling
//! `spawn_agent`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Result, bail};
use tokio::sync::{RwLock, mpsc, watch};
use tokio::task::JoinHandle;
use zerozero_compaction::CompactionConfig;
use zerozero_exec::Event;
use zerozero_llm::Provider;
use zerozero_sandbox::{ApprovalPolicy, SandboxPolicy};
use zerozero_tools::ToolRegistry;

/// Thread ID = String (timestamp-based, same as session_id_now pattern).
pub type ThreadId = String;

/// Prefix for subagent notification messages injected into parent's steer inbox.
///
/// Messages starting with this prefix are treated as system messages (subagent
/// completion notification). Other messages are treated as user steer messages.
/// Using a specific prefix avoids edge-case misinterpretation where a user
/// steer message might start with `[Subagent` (C review §8 note 2, action
/// item #5).
pub const SUBAGENT_NOTIFY_PREFIX: &str = "[ZZ::SUBAGENT_NOTIFY]";

/// Agent path — tree position: "root", "root.0", "root.0.1".
/// Dot-separated child index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AgentPath(String);

impl AgentPath {
    /// Create the root agent path.
    pub fn root() -> Self {
        Self("root".to_string())
    }

    /// Create a child path by appending a child index.
    pub fn child(&self, index: usize) -> Self {
        Self(format!("{}.{}", self.0, index))
    }

    /// Get the path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Thread status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    /// Thread is actively running (LLM turns in progress).
    Running,
    /// Thread was interrupted by the user (cancel flag set).
    Stopped,
    /// Thread finished successfully.
    Completed,
    /// Thread errored out.
    Failed,
}

impl AgentStatus {
    /// Convert to string for session schema storage.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Metadata for a registered thread.
#[derive(Clone, Debug)]
pub struct AgentMetadata {
    /// Unique thread ID (timestamp-based).
    pub thread_id: ThreadId,
    /// Tree position path (e.g. "root", "root.0").
    pub agent_path: AgentPath,
    /// Parent thread ID (None for root).
    pub parent_thread_id: Option<ThreadId>,
    /// Depth in the agent tree (0 = root, 1 = first-level subagent).
    pub depth: i32,
    /// Simple index nickname (e.g. "agent-0", "agent-1").
    pub nickname: String,
    /// Current status.
    pub status: AgentStatus,
    /// Last task message (the prompt or last steer message).
    pub last_task_message: Option<String>,
}

/// Result when a thread's `run_turn_with_steer` completes.
#[derive(Clone, Debug)]
pub struct ThreadResult {
    /// Thread ID that produced this result.
    pub thread_id: ThreadId,
    /// Summary = last assistant message (final answer).
    pub summary: String,
    /// Whether the thread completed successfully.
    pub success: bool,
    /// Error message if the thread failed.
    pub error: Option<String>,
    /// Final status (Completed, Stopped, or Failed).
    pub status: AgentStatus,
}

/// Handle to a spawned thread — holds channels for control.
///
/// This struct is `Send + Sync` because all fields are `Send + Sync`:
/// - `UnboundedSender<String>`: `Send + Sync`
/// - `Arc<AtomicBool>`: `Send + Sync`
/// - `JoinHandle<ThreadResult>`: `Send + Sync` (because `ThreadResult` is
///   `Send + Sync` — all fields are `String`, `bool`, `Option<String>`,
///   `AgentStatus`)
///
/// This allows storing `ThreadHandle` in a `RwLock<HashMap<ThreadId,
/// ThreadHandle>>` (C review §4, action item #6).
pub struct ThreadHandle {
    /// Sender for steer messages (inject into thread's inbox).
    pub steer_tx: mpsc::UnboundedSender<String>,
    /// Cancel flag — set to `true` by `interrupt_agent`.
    pub cancel_flag: Arc<AtomicBool>,
    /// Join handle for the spawned `spawn_local` task.
    pub join_handle: JoinHandle<ThreadResult>,
}

/// Context needed to spawn a thread (shared resources).
///
/// All fields are `Send + Sync` (Arcs + Clone-able configs), so
/// `SpawnContext` is `Clone + Send + Sync`.
#[derive(Clone)]
pub struct SpawnContext {
    /// LLM provider (shared across all threads).
    pub provider: Arc<dyn Provider>,
    /// Tool registry (shared across all threads).
    pub tools: Arc<ToolRegistry>,
    /// Sandbox policy.
    pub sandbox: SandboxPolicy,
    /// Approval policy.
    pub approval: ApprovalPolicy,
    /// Compaction configuration.
    pub compaction_config: CompactionConfig,
    /// Session database path (each thread opens its own Connection).
    pub session_db_path: Option<PathBuf>,
    /// System prompt to inject.
    pub system_prompt: Option<String>,
    /// Max turns per `run_turn` call.
    pub max_turns: u32,
    /// Reasoning effort . Defaults to `Effort::None` to preserve
    /// prior behavior for subagents.
    pub effort: zerozero_llm::Effort,
    /// Optional sink for streaming `run_turn` events (TUI routes to
    /// `AppEvent::AgentThread`). `None` in tests and headless paths.
    pub emit_thread_event: Option<Arc<dyn Fn(ThreadId, Event) + Send + Sync>>,
}

/// Central registry for interactive multi-agent threads.
///
/// Replaces `AgentRegistry` (codex-rs pattern, simplified). Manages thread
/// metadata, control handles, and the active thread watch channel.
///
/// `ThreadRegistry` is `Send + Sync` — all fields use thread-safe types
/// (`RwLock`, `watch::Sender`, `AtomicUsize`).
pub struct ThreadRegistry {
    /// Thread metadata (cloneable, safe to read under RwLock).
    threads: RwLock<HashMap<ThreadId, AgentMetadata>>,
    /// Thread control handles (not cloneable — JoinHandle).
    handles: RwLock<HashMap<ThreadId, ThreadHandle>>,
    /// Watch channel for active thread ID (TUI footer subscribes).
    active_thread: watch::Sender<ThreadId>,
    /// Total thread count (atomic, for slot reservation).
    total_count: AtomicUsize,
    /// Maximum concurrent threads (default 6, codex-rs default).
    max_threads: usize,
    /// Maximum nesting depth (default 1 — MVP, no nested subagent).
    max_depth: i32,
    /// Child counter per parent (for agent_path indexing).
    child_counter: RwLock<HashMap<ThreadId, usize>>,
}

impl ThreadRegistry {
    /// Create a new registry with root thread pre-registered.
    ///
    /// Returns `(registry, root_thread_id)`. The root thread represents the
    /// main conversation thread (depth 0, no parent).
    pub fn new(max_threads: usize, max_depth: i32) -> (Self, ThreadId) {
        let root_id = thread_id_now();
        let (active_tx, _active_rx) = watch::channel(root_id.clone());

        let mut threads = HashMap::new();
        threads.insert(
            root_id.clone(),
            AgentMetadata {
                thread_id: root_id.clone(),
                agent_path: AgentPath::root(),
                parent_thread_id: None,
                depth: 0,
                nickname: "root".to_string(),
                status: AgentStatus::Running,
                last_task_message: None,
            },
        );

        let registry = Self {
            threads: RwLock::new(threads),
            handles: RwLock::new(HashMap::new()),
            active_thread: active_tx,
            total_count: AtomicUsize::new(1), // root counts as 1
            max_threads,
            max_depth,
            child_counter: RwLock::new(HashMap::new()),
        };

        (registry, root_id)
    }

    /// Spawn a subagent thread.
    ///
    /// **LocalSet requirement:** This method calls `tokio::task::spawn_local`,
    /// which panics if not called within a `LocalSet` context. Callers must
    /// ensure they are inside `local_set.run_until(...)` before calling this
    /// method (C review §3.2, action item #2).
    ///
    /// Flow:
    /// 1. Compute depth = parent.depth + 1; reject if depth > max_depth.
    /// 2. Reserve slot atomically; reject if total_count >= max_threads.
    /// 3. Generate thread_id, agent_path, nickname.
    /// 4. Create own SessionStore (own SQLite Connection, !Send → LocalSet).
    /// 5. Create steer inbox channel + cancel flag.
    /// 6. Register metadata (status=Running) in threads map.
    /// 7. `spawn_local` the `run_turn_with_steer` task.
    /// 8. Store `ThreadHandle` in handles map.
    /// 9. Notify active_thread watch channel.
    pub async fn spawn_agent(
        &self,
        prompt: String,
        parent: ThreadId,
        ctx: &SpawnContext,
    ) -> Result<ThreadId> {
        // 1. Depth check
        let parent_depth = self.depth_of(&parent).await;
        let depth = parent_depth + 1;
        if depth > self.max_depth {
            bail!(
                "max depth ({}) exceeded: parent depth {}, would be {}",
                self.max_depth,
                parent_depth,
                depth
            );
        }

        // 2. Slot reservation (atomic)
        self.reserve_slot()?;

        // 3. Generate IDs
        let thread_id = thread_id_now();
        let agent_path = self.next_child_path(&parent).await;
        let nickname = self.next_nickname(&parent).await;

        // 4. Create per-thread SessionStore
        let session = match &ctx.session_db_path {
            Some(path) => zerozero_session::SessionStore::open(path)?,
            None => zerozero_session::SessionStore::open_in_memory()?,
        };

        // 5. Create steer channel + cancel flag
        let (steer_tx, steer_rx) = mpsc::unbounded_channel::<String>();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        // Clone the Arc so both the handle and the task share the same flag.
        let cancel_flag_for_task = Arc::clone(&cancel_flag);

        // 6. Register metadata
        let metadata = AgentMetadata {
            thread_id: thread_id.clone(),
            agent_path: agent_path.clone(),
            parent_thread_id: Some(parent.clone()),
            depth,
            nickname: nickname.clone(),
            status: AgentStatus::Running,
            last_task_message: Some(prompt.clone()),
        };
        self.threads
            .write()
            .await
            .insert(thread_id.clone(), metadata);

        // 7. Spawn the run_turn_with_steer task
        // NOTE: spawn_local panics if not within a LocalSet context.
        // Callers must ensure local_set.run_until(...) is active.
        let ctx_clone = ctx.clone();
        let thread_id_clone = thread_id.clone();
        let join_handle = tokio::task::spawn_local(async move {
            crate::steer::run_turn_with_steer(
                &prompt,
                &ctx_clone,
                session,
                steer_rx,
                cancel_flag_for_task,
                thread_id_clone,
            )
            .await
        });

        // 8. Store handle (cancel_flag is shared with the task via Arc)
        let handle = ThreadHandle {
            steer_tx,
            cancel_flag,
            join_handle,
        };
        self.handles.write().await.insert(thread_id.clone(), handle);

        // 9. Notify active thread
        let _ = self.active_thread.send(thread_id.clone());

        Ok(thread_id)
    }

    /// Steer a running thread — inject a message into its inbox.
    ///
    /// The message is queued in the thread's `mpsc::UnboundedReceiver`.
    /// The thread polls `steer_rx.try_recv()` between `run_turn` calls and
    /// appends the message as a `ChatMessage { role: "user" }`.
    ///
    /// No restart, no new thread. If the thread has already completed, the
    /// send will fail (receiver dropped) and an error is returned.
    pub async fn send_inter_agent(&self, thread_id: &ThreadId, message: String) -> Result<()> {
        let handles = self.handles.read().await;
        let handle = handles
            .get(thread_id)
            .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;
        handle
            .steer_tx
            .send(message.clone())
            .map_err(|_| anyhow::anyhow!("thread closed: {thread_id}"))?;

        // Update last_task_message in metadata
        drop(handles);
        if let Some(meta) = self.threads.write().await.get_mut(thread_id) {
            meta.last_task_message = Some(message);
        }
        Ok(())
    }

    /// Interrupt a running thread — set the cancel flag.
    ///
    /// **Cancel semantics:** This is **turn-boundary cancel**, not immediate.
    /// The thread checks `cancel_flag.load(Ordering::Relaxed)` between
    /// `run_turn` calls. If true, the thread breaks its loop and returns
    /// `ThreadResult { status: Stopped }`.
    ///
    /// Cancel does NOT take effect:
    /// - During an in-flight LLM SSE stream (takes effect at next turn boundary).
    /// - During tool execution (takes effect after tool completes).
    ///
    /// This is acceptable for MVP. Deferred cancel is safer than
    /// `JoinHandle::abort()` which can leave resources inconsistent.
    pub async fn interrupt_agent(&self, thread_id: &ThreadId) -> Result<()> {
        let handles = self.handles.read().await;
        let handle = handles
            .get(thread_id)
            .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;
        handle.cancel_flag.store(true, Ordering::Relaxed);
        drop(handles);

        // Update status to Stopped in metadata
        self.update_status(thread_id, AgentStatus::Stopped).await;
        Ok(())
    }

    /// Switch active thread (for TUI display).
    ///
    /// This only changes which thread's events are rendered in the TUI.
    /// The previously active thread continues running in the background.
    pub fn switch_thread(&self, thread_id: &ThreadId) -> Result<()> {
        self.active_thread
            .send(thread_id.clone())
            .map_err(|_| anyhow::anyhow!("active_thread watch channel closed"))
    }

    /// List all live agents (for picker UI).
    pub async fn live_agents(&self) -> Vec<AgentMetadata> {
        let threads = self.threads.read().await;
        let mut agents: Vec<AgentMetadata> = threads.values().cloned().collect();
        // Sort by agent_path for consistent tree ordering.
        agents.sort_by(|a, b| a.agent_path.as_str().cmp(b.agent_path.as_str()));
        agents
    }

    /// Subscribe to active_thread changes (for TUI footer).
    pub fn subscribe_active(&self) -> watch::Receiver<ThreadId> {
        self.active_thread.subscribe()
    }

    /// Notify parent thread when a child completes.
    ///
    /// Injects a system message into the parent's steer inbox with the
    /// `[ZZ::SUBAGENT_NOTIFY]` prefix. The parent's `run_turn_with_steer`
    /// polls its inbox and appends messages starting with this prefix as
    /// `role: "system"` (subagent completion notification), other messages
    /// as `role: "user"` (steer).
    ///
    /// Reuses the steer inbox channel (1 channel per thread, Ponytail rule).
    pub async fn notify_parent(
        &self,
        parent_thread_id: &ThreadId,
        child_thread_id: &ThreadId,
        summary: String,
    ) -> Result<()> {
        // Get child nickname for the notification message
        let nickname = {
            let threads = self.threads.read().await;
            threads
                .get(child_thread_id)
                .map(|m| m.nickname.clone())
                .unwrap_or_else(|| "unknown".to_string())
        };

        let message = format!(
            "{} Subagent '{}' (thread: {}) completed: {}",
            SUBAGENT_NOTIFY_PREFIX, nickname, child_thread_id, summary
        );

        self.send_inter_agent(parent_thread_id, message).await
    }

    // --- helpers ---

    /// Get the status of a thread.
    pub async fn get_status(&self, thread_id: &ThreadId) -> Option<AgentStatus> {
        self.threads
            .read()
            .await
            .get(thread_id)
            .map(|m| m.status.clone())
    }

    /// Update the status of a thread.
    pub async fn update_status(&self, thread_id: &ThreadId, status: AgentStatus) {
        if let Some(meta) = self.threads.write().await.get_mut(thread_id) {
            meta.status = status;
        }
    }

    /// Get the depth of a thread (returns 0 if not found, which is root depth).
    async fn depth_of(&self, parent: &ThreadId) -> i32 {
        self.threads
            .read()
            .await
            .get(parent)
            .map(|m| m.depth)
            .unwrap_or(0)
    }

    /// Reserve a slot atomically. Rejects if total_count >= max_threads.
    fn reserve_slot(&self) -> Result<()> {
        let current = self.total_count.load(Ordering::Relaxed);
        if current >= self.max_threads {
            bail!(
                "max threads ({}) exceeded: {} threads already registered",
                self.max_threads,
                current
            );
        }
        self.total_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Generate the next child agent_path for a parent.
    async fn next_child_path(&self, parent: &ThreadId) -> AgentPath {
        let mut counters = self.child_counter.write().await;
        let index = counters.entry(parent.clone()).or_insert(0);
        let path = if let Some(parent_meta) = self.threads.read().await.get(parent) {
            parent_meta.agent_path.child(*index)
        } else {
            AgentPath::root().child(*index)
        };
        *index += 1;
        path
    }

    /// Generate the next nickname for a parent's child (e.g. "agent-0").
    ///
    /// Uses the global thread count (after `reserve_slot` incremented it)
    /// to produce unique sequential nicknames. `total_count` after
    /// `reserve_slot` = 2 for the first child (root=1 + 1), so
    /// `saturating_sub(2)` = 0 → "agent-0". Second child: total_count=3,
    /// `saturating_sub(2)` = 1 → "agent-1". This fixes the off-by-one
    /// where the first child was "agent-1" instead of "agent-0"
    /// (E cold review finding 4).
    async fn next_nickname(&self, parent: &ThreadId) -> String {
        let _ = parent; // parent not needed — global counter gives unique names
        let global_count = self.total_count.load(Ordering::Relaxed);
        format!("agent-{}", global_count.saturating_sub(2))
    }
}

/// Generate a timestamp-based thread ID (ISO 8601 UTC with millisecond
/// precision).
///
/// Uses millisecond resolution (not second) to avoid ID collisions when
/// multiple threads are spawned within the same second (pending queue fix).
fn thread_id_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    iso8601_utc_millis(millis)
}

/// Format milliseconds since UNIX_EPOCH as ISO 8601 UTC with millisecond
/// precision: `YYYY-MM-DDThh:mm:ss.mmmZ`.
fn iso8601_utc_millis(millis: u128) -> String {
    let secs = (millis / 1000) as u64;
    let ms = (millis % 1000) as u64;
    let days = secs / 86400;
    let sod = secs % 86400;
    let h = sod / 3600;
    let m = (sod % 3600) / 60;
    let s = sod % 60;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_path_root() {
        let root = AgentPath::root();
        assert_eq!(root.as_str(), "root");
    }

    #[test]
    fn test_agent_path_child() {
        let root = AgentPath::root();
        let child0 = root.child(0);
        let child1 = root.child(1);
        assert_eq!(child0.as_str(), "root.0");
        assert_eq!(child1.as_str(), "root.1");
        let grandchild = child0.child(2);
        assert_eq!(grandchild.as_str(), "root.0.2");
    }

    #[test]
    fn test_agent_status_as_str() {
        assert_eq!(AgentStatus::Running.as_str(), "running");
        assert_eq!(AgentStatus::Stopped.as_str(), "stopped");
        assert_eq!(AgentStatus::Completed.as_str(), "completed");
        assert_eq!(AgentStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn test_subagent_notify_prefix() {
        assert_eq!(SUBAGENT_NOTIFY_PREFIX, "[ZZ::SUBAGENT_NOTIFY]");
    }
}
