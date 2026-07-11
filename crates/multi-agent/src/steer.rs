//! Steer + interrupt wrapper for `run_turn` Phase 4).
//!
//! `run_turn_with_steer` wraps `zerozero_core::run_turn` to add:
//! - **Steer inbox:** `mpsc::UnboundedReceiver<String>` polled between
//!   `run_turn` calls. Steer messages are appended as `role: "user"`.
//!   Subagent notification messages (prefix `[ZZ::SUBAGENT_NOTIFY]`) are
//!   appended as `role: "system"`.
//! - **Cancel flag:** `Arc<AtomicBool>` checked between `run_turn` calls.
//!   If set, the loop breaks and returns `ThreadResult { status: Stopped }`.
//!
//! ## Cancel semantics (C review §3.2, action item #1)
//!
//! Cancel is **turn-boundary** — checked between `run_turn` calls, not
//! mid-LLM-stream or mid-tool. See `thread.rs` module docs for details.
//!
//! ## `run_turn` signature mapping (C review §4, action item #3)
//!
//! `run_turn` has 14 parameters (confirmed `crates/core/src/lib.rs:43`).
//! The design §3.2 pseudocode `run_turn(&messages, ...)` is shorthand.
//! This wrapper maps all 14 parameters correctly:
//! 1. `prompt` — the steer message or initial prompt
//! 2. `system_prompt` — from `SpawnContext`
//! 3. `provider` — from `SpawnContext` (deref Arc)
//! 4. `tools` — from `SpawnContext` (deref Arc)
//! 5. `max_turns` — from `SpawnContext`
//! 6. `sandbox` — from `SpawnContext`
//! 7. `approval` — from `SpawnContext`
//! 8. `plan_mode` — `false` (MVP)
//! 9. `session` — `Some(&session)` (per-thread SessionStore)
//! 10. `compaction_config` — from `SpawnContext`
//! 11. `hooks` — `NoopHooks` (MVP)
//! 12. `continue_session_id` — `Some(thread_id)` for subsequent calls
//! 13. `prior_messages` — accumulated messages from session
//! 14. `emit` — no-op (caller provides routing via separate mechanism)
//!
//! ## `emit` callback routing (C review §4, action item #4)
//!
//! When `SpawnContext::emit_thread_event` is set (TUI), this wrapper forwards
//! all `run_turn` events to that sink; the TUI routes them as
//! `AppEvent::AgentThread`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use zerozero_core::{LifecycleHooks, NoopHooks, run_turn};
use zerozero_exec::Event;
use zerozero_llm::ChatMessage;
use zerozero_session::SessionStore;

use crate::thread::{AgentStatus, SUBAGENT_NOTIFY_PREFIX, SpawnContext, ThreadId, ThreadResult};

/// Run `run_turn` with steer inbox + cancel flag support.
///
/// This wrapper manages a loop that:
/// 1. Checks the cancel flag (turn-boundary cancel).
/// 2. Drains the steer inbox — collects all pending messages.
/// 3. Calls `run_turn` with the current prompt + prior messages.
/// 4. After `run_turn` completes, drains steer inbox again.
/// 5. If steer messages arrived, uses the last one as the new prompt and
///    loops (continue session).
/// 6. If no steer and `run_turn` completed, returns `ThreadResult`.
pub async fn run_turn_with_steer(
    prompt: &str,
    ctx: &SpawnContext,
    session: SessionStore,
    mut steer_rx: mpsc::UnboundedReceiver<String>,
    cancel_flag: Arc<AtomicBool>,
    thread_id: ThreadId,
) -> ThreadResult {
    let mut current_prompt = prompt.to_string();
    let mut is_first_call = true;
    let mut last_summary = String::new();

    loop {
        // 1. Check cancel flag (turn-boundary cancel).
        // NOTE: This is NOT immediate cancel. If the flag was set during
        // an in-flight LLM stream or tool execution, it takes effect here
        // at the next turn boundary. See thread.rs module docs.
        if cancel_flag.load(Ordering::Relaxed) {
            return ThreadResult {
                thread_id: thread_id.clone(),
                summary: last_summary,
                success: false,
                error: Some("interrupted by user".to_string()),
                status: AgentStatus::Stopped,
            };
        }

        // 2. Drain steer inbox before each run_turn call.
        // Messages starting with SUBAGENT_NOTIFY_PREFIX → system role
        // (subagent completion notification, per design §3.7 / AC-6).
        // Other messages → user role (steer, per design §3.5 / AC-4).
        //
        // Both notification and steer messages are persisted to the session
        // and added to prior_messages so the LLM sees them in context.
        // This fixes E cold review findings 1 (CRITICAL) and 2 (MAJOR):
        // previously notifications were dropped and steer messages were
        // only set as current_prompt without being injected into the
        // messages vec.
        let mut notifications: Vec<String> = Vec::new();
        let mut steers: Vec<String> = Vec::new();
        while let Ok(msg) = steer_rx.try_recv() {
            if msg.starts_with(SUBAGENT_NOTIFY_PREFIX) {
                notifications.push(msg);
            } else {
                steers.push(msg);
            }
        }

        // If this is not the first call and no user steer arrived,
        // the thread is complete (no new work). Notifications alone
        // don't restart the loop — they're context-only signals.
        if !is_first_call && steers.is_empty() {
            return ThreadResult {
                thread_id: thread_id.clone(),
                summary: last_summary,
                success: true,
                error: None,
                status: AgentStatus::Completed,
            };
        }

        // 3. Load prior messages from session (for continue_session).
        // Load BEFORE appending new messages so prior_messages doesn't
        // include the newly injected messages (avoids duplication in
        // run_turn's internal messages vec).
        let mut prior_messages: Vec<ChatMessage> = if is_first_call {
            Vec::new()
        } else {
            session.get_messages(&thread_id).unwrap_or_default()
        };

        // 3a. Persist notification messages to session with role "system"
        // and add to prior_messages (AC-6: parent receives system message).
        for notif in &notifications {
            let system_msg = ChatMessage {
                role: "system".to_string(),
                content: notif.clone(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            };
            let _ = session.append_message(&thread_id, &system_msg);
            prior_messages.push(system_msg);
        }

        // 3b. Persist steer messages to session with role "user" and add
        // to prior_messages (AC-4: steer injects into messages vec with
        // role "user"). Also set the last steer as current_prompt for
        // run_turn's prompt parameter.
        for steer in &steers {
            let user_msg = ChatMessage {
                role: "user".to_string(),
                content: steer.clone(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            };
            let _ = session.append_message(&thread_id, &user_msg);
            prior_messages.push(user_msg);
            current_prompt = steer.clone();
        }

        // 4. Call run_turn — forward streaming events when TUI wired a sink.
        let emit_sink = ctx.emit_thread_event.clone();
        let tid_emit = thread_id.clone();
        let emit = move |event: Event| {
            if let Some(sink) = &emit_sink {
                sink(tid_emit.clone(), event);
            }
        };

        let result = run_turn(
            &current_prompt,
            ctx.system_prompt.as_deref(),
            &*ctx.provider,
            &ctx.tools,
            ctx.max_turns,
            &ctx.sandbox,
            &ctx.approval,
            false, // plan_mode = false (MVP)
            false, // ask_mode = false (MVP)
            Some(&session),
            &ctx.compaction_config,
            &NoopHooks as &dyn LifecycleHooks,
            if is_first_call {
                None
            } else {
                Some(&thread_id)
            },
            &prior_messages,
            ctx.effort,
            &[],
            None,
            emit,
        )
        .await;

        is_first_call = false;

        match result {
            Ok(()) => {
                // run_turn completed successfully. Get the last assistant
                // message as the summary.
                let messages = session.get_messages(&thread_id).unwrap_or_default();
                last_summary = messages
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| m.content.clone())
                    .unwrap_or_default();

                // Loop back to check cancel + drain steer inbox.
                // If no steer messages, the next iteration will return Completed.
                continue;
            }
            Err(e) => {
                let error_msg = e.to_string();

                // Check if cancelled during run_turn.
                if cancel_flag.load(Ordering::Relaxed) {
                    return ThreadResult {
                        thread_id: thread_id.clone(),
                        summary: last_summary,
                        success: false,
                        error: Some("interrupted by user".to_string()),
                        status: AgentStatus::Stopped,
                    };
                }

                // Check for steer messages (allow retry after error).
                // Persist steer messages to session with role "user" (AC-4)
                // and notification messages with role "system" (AC-6).
                let mut has_retry_steer = false;
                while let Ok(msg) = steer_rx.try_recv() {
                    if msg.starts_with(SUBAGENT_NOTIFY_PREFIX) {
                        let system_msg = ChatMessage {
                            role: "system".to_string(),
                            content: msg,
                            tool_call_id: None,
                            tool_calls: None,
                            attachments: None,
                            thinking_signature: None,
                            redacted_thinking: None,
                            thinking: None,
                        };
                        let _ = session.append_message(&thread_id, &system_msg);
                    } else {
                        let user_msg = ChatMessage {
                            role: "user".to_string(),
                            content: msg.clone(),
                            tool_call_id: None,
                            tool_calls: None,
                            attachments: None,
                            thinking_signature: None,
                            redacted_thinking: None,
                            thinking: None,
                        };
                        let _ = session.append_message(&thread_id, &user_msg);
                        current_prompt = msg;
                        has_retry_steer = true;
                    }
                }
                if has_retry_steer {
                    continue;
                }

                return ThreadResult {
                    thread_id: thread_id.clone(),
                    summary: last_summary,
                    success: false,
                    error: Some(error_msg),
                    status: AgentStatus::Failed,
                };
            }
        }
    }
}
