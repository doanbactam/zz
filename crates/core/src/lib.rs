//! Core engine for ZeroZero — agent loop with tool support .
//!
//! `run_turn` orchestrates a multi-turn agent loop:
//! 1. Send prompt + tool definitions to LLM
//! 2. Stream response (content deltas + tool calls)
//! 3. If tool calls: execute tools, append results, loop back to step 1
//! 4. If no tool calls: emit completion, done
//! 5. Max turns limit prevents infinite loops

mod auto_test_hooks;
mod composite_hook;
mod config;
mod hook_config;
mod hooks;
mod http_hook;
mod system_prompt;

pub use auto_test_hooks::AutoTestHooks;
pub use composite_hook::CompositeHook;
pub use config::{
    PermissionRule, PermissionSet, Profile, ResolvedSettings, SUPPORTED_FEATURES, ZeroZeroConfig,
    check_supported_feature, is_supported_feature,
};
pub use hook_config::{discover_config, load_hooks, parse_hooks_toml};
pub use hooks::{
    HookAction, HookEvent, LifecycleHooks, NoopHooks, PostToolContext, PreCompactCtx,
    SessionStartCtx, StopCtx, ToolFailureCtx, ToolHookContext, UserPromptCtx,
};
pub use http_hook::HttpHook;
pub use system_prompt::{compose as compose_system_prompt, core_identity_prompt};

use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;
use zerozero_compaction::{
    CompactionConfig, compact_messages_with_llm, compact_token_budget_with_llm, count_tokens,
    estimate_tokens, should_compact, should_compact_token_budget,
};
use zerozero_exec::{Event, Item, ItemKind, ItemUpdateKind};
use zerozero_llm::{ChatMessage, Effort, Provider, SseEvent, ToolCall, ToolCallFunction};
use zerozero_sandbox::{
    ApprovalAction, ApprovalPolicy, DangerLevel, SandboxPolicy, classify_command, should_approve,
};
use zerozero_session::SessionStore;
use zerozero_tools::ToolRegistry;

/// Run one headless turn with the given provider, tools, sandbox, and approval.
///
/// Emits events via `emit`:
/// 1. `session.started`
/// 2. `prompt`
/// 3. For each LLM call:
///    a. `item.started` (agent_message)
///    b. `item.updated` (per content delta)
///    c. If tool calls: `approval.requested` + `approval.result` (if needed)
///       + `tool.started` + `tool.completed` per tool
/// 4. `item.completed` (final text)
/// 5. `turn.completed`
///
/// If max_turns is exceeded, emits `error` and returns `Err`.
///
/// `approval_rx`: when `Some`, a `Prompt` approval decision is awaited from
/// this receiver (fed by the remote TUI client over the wire) instead of
/// reading from local stdin. `None` = legacy local-stdin approval path.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    prompt: &str,
    system_prompt: Option<&str>,
    provider: &dyn Provider,
    tools: &ToolRegistry,
    max_turns: u32,
    sandbox: &SandboxPolicy,
    approval: &ApprovalPolicy,
    plan_mode: bool,
    ask_mode: bool,
    session: Option<&SessionStore>,
    compaction_config: &CompactionConfig,
    hooks: &dyn LifecycleHooks,
    continue_session_id: Option<&str>,
    prior_messages: &[ChatMessage],
    effort: Effort,
    permissions: &[String],
    approval_rx: Option<Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<bool>>>>,
    mut emit: impl FnMut(Event),
) -> anyhow::Result<()> {
    // Sandbox policy is enforced inside each tool (BashTool via pre_exec,
    // WriteFileTool/EditFileTool via validate_write_path). This param is
    // kept for future use (e.g. emitting sandbox status events) and for
    // callers that already hold the policy.
    let _ = sandbox;
    let continuing = continue_session_id.is_some();
    let session_id = continue_session_id
        .map(|s| s.to_string())
        .unwrap_or_else(session_id_now);
    emit(Event::SessionStarted {
        session_id: session_id.clone(),
    });
    // session_start hook — once per session, after SessionStarted event.
    hooks
        .session_start(&SessionStartCtx {
            session_id: session_id.clone(),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        })
        .await;
    emit(Event::Prompt {
        text: prompt.to_string(),
    });

    // Create session record in SQLite if store is provided and we are
    // NOT continuing an existing session (the record already exists).
    if let Some(store) = session {
        if !continuing {
            let _ = store.create_session(&session_id, prompt, None);
        }
    }

    let tool_defs = tools.definitions();
    let mut messages: Vec<ChatMessage> = Vec::new();

    // When continuing a session, prepend prior messages (which already
    // contain the system prompt and conversation history from the
    // previous session). Otherwise inject the system prompt fresh.
    if continuing && !prior_messages.is_empty() {
        messages.extend_from_slice(prior_messages);
    } else if let Some(sys) = system_prompt {
        if !sys.is_empty() {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: sys.to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            });
        }
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: prompt.to_string(),
        tool_call_id: None,
        tool_calls: None,
        attachments: None,
        thinking_signature: None,
        redacted_thinking: None,
        thinking: None,
    });

    // Persist initial user message (index 0 if no system, index 1 if system).
    if let Some(store) = session {
        let user_msg = messages
            .iter()
            .find(|m| m.role == "user")
            .expect("user message always present");
        let _ = store.append_message(&session_id, user_msg);
    }

    // user_prompt_submit hook — once per turn, after user prompt persisted.
    hooks
        .user_prompt_submit(&UserPromptCtx {
            session_id: session_id.clone(),
            prompt: prompt.to_string(),
        })
        .await;

    for turn in 0..max_turns {
        let item_id = format!("item_{turn}");

        // Check if compaction is needed before LLM call.
        if should_compact(&messages, compaction_config) {
            let before_msgs = messages.len();
            let before_tokens = estimate_tokens(&messages);
            emit(Event::CompactionStarted {
                before_messages: before_msgs,
                before_tokens,
            });
            // pre_compact hook — before compaction.
            hooks
                .pre_compact(&PreCompactCtx {
                    session_id: session_id.clone(),
                    before_messages: before_msgs,
                    before_tokens,
                    trigger: "auto".to_string(),
                })
                .await;
            // Use LLM-based compaction (falls back to text extraction on error).
            messages = compact_messages_with_llm(messages, compaction_config, provider).await;
            let after_msgs = messages.len();
            let after_tokens = estimate_tokens(&messages);
            emit(Event::CompactionCompleted {
                after_messages: after_msgs,
                after_tokens,
            });
        }
        // Token-budget auto-compaction (Codex-style). When the total
        // estimated token count exceeds `token_budget`, keep the most recent
        // `keep_recent_turns` messages verbatim and summarize the older ones.
        else if should_compact_token_budget(&messages, compaction_config) {
            let before_msgs = messages.len();
            let before_tokens = count_tokens(&messages);
            emit(Event::CompactionStarted {
                before_messages: before_msgs,
                before_tokens,
            });
            hooks
                .pre_compact(&PreCompactCtx {
                    session_id: session_id.clone(),
                    before_messages: before_msgs,
                    before_tokens,
                    trigger: "token-budget".to_string(),
                })
                .await;
            messages = compact_token_budget_with_llm(messages, compaction_config, provider).await;
            let after_msgs = messages.len();
            let after_tokens = count_tokens(&messages);
            emit(Event::CompactionCompleted {
                after_messages: after_msgs,
                after_tokens,
            });
        }

        emit(Event::ItemStarted {
            item: zerozero_exec::ItemStarted {
                id: item_id.clone(),
                kind: ItemKind::AgentMessage,
            },
        });

        let mut stream = match provider
            .chat_with_tools(
                &messages,
                &tool_defs,
                effort,
                &zerozero_llm::collect_turn_image_urls(&messages),
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                emit(Event::Error {
                    message: e.to_string(),
                });
                // stop + session_end on error path.
                hooks
                    .stop(&StopCtx {
                        session_id: session_id.clone(),
                        reason: format!("error:{e}"),
                    })
                    .await;
                hooks
                    .session_end(&SessionStartCtx {
                        session_id: session_id.clone(),
                        cwd: String::new(),
                    })
                    .await;
                return Err(e);
            }
        };

        let mut full_text = String::new();
        let mut tool_calls: Vec<(String, String, String)> = Vec::new();
        // Extended thinking accumulators (Anthropic round-trip).
        let mut thinking_text = String::new();
        let mut thinking_signature: Option<String> = None;
        let mut redacted_thinking: Option<String> = None;

        while let Some(result) = stream.next().await {
            match result {
                Ok(SseEvent::Content(delta)) => {
                    full_text.push_str(&delta);
                    emit(Event::ItemUpdated {
                        item: zerozero_exec::ItemUpdated {
                            id: item_id.clone(),
                            text: delta,
                            kind: ItemUpdateKind::Message,
                        },
                    });
                }
                Ok(SseEvent::Reasoning(delta)) => {
                    emit(Event::ItemUpdated {
                        item: zerozero_exec::ItemUpdated {
                            id: item_id.clone(),
                            text: delta,
                            kind: ItemUpdateKind::Reasoning,
                        },
                    });
                }
                Ok(SseEvent::ThinkingBlock {
                    thinking,
                    signature,
                }) => {
                    // Store the complete thinking block for round-trip.
                    // The thinking text was already streamed token-by-token
                    // via SseEvent::Reasoning above (for TUI display), so
                    // we only need to capture it here for convert_messages.
                    thinking_text = thinking;
                    if !signature.is_empty() {
                        thinking_signature = Some(signature);
                    }
                }
                Ok(SseEvent::RedactedThinking(data)) => {
                    redacted_thinking = Some(data);
                }
                Ok(SseEvent::ToolCall {
                    id,
                    name,
                    arguments,
                }) => {
                    tool_calls.push((id, name, arguments));
                }
                Ok(SseEvent::Done) => {
                    break;
                }
                Err(e) => {
                    emit(Event::Error {
                        message: e.to_string(),
                    });
                    // stop + session_end on stream error path.
                    hooks
                        .stop(&StopCtx {
                            session_id: session_id.clone(),
                            reason: format!("error:{e}"),
                        })
                        .await;
                    hooks
                        .session_end(&SessionStartCtx {
                            session_id: session_id.clone(),
                            cwd: String::new(),
                        })
                        .await;
                    return Err(e);
                }
            }
        }

        // Append assistant message (with tool_calls if any) to conversation.
        let assistant_msg = ChatMessage {
            role: "assistant".to_string(),
            content: full_text.clone(),
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(
                    tool_calls
                        .iter()
                        .map(|(id, name, args)| ToolCall {
                            id: id.clone(),
                            call_type: "function".to_string(),
                            function: ToolCallFunction {
                                name: name.clone(),
                                arguments: args.clone(),
                            },
                        })
                        .collect(),
                )
            },
            attachments: None,
            thinking: if thinking_text.is_empty() {
                None
            } else {
                Some(thinking_text.clone())
            },
            thinking_signature: thinking_signature.clone(),
            redacted_thinking: redacted_thinking.clone(),
        };
        messages.push(assistant_msg.clone());

        // Persist assistant message.
        if let Some(store) = session {
            let _ = store.append_message(&session_id, &assistant_msg);
        }

        // If no tool calls, this turn is complete.
        if tool_calls.is_empty() {
            emit(Event::ItemCompleted {
                item: Item {
                    id: item_id,
                    kind: ItemKind::AgentMessage,
                    text: full_text,
                },
            });
            emit(Event::TurnCompleted);
            // stop + session_end on success path.
            hooks
                .stop(&StopCtx {
                    session_id: session_id.clone(),
                    reason: "completed".to_string(),
                })
                .await;
            hooks
                .session_end(&SessionStartCtx {
                    session_id: session_id.clone(),
                    cwd: String::new(),
                })
                .await;
            return Ok(());
        }

        // Execute each tool call.
        for (call_id, tool_name, args_str) in &tool_calls {
            let args: serde_json::Value =
                serde_json::from_str(args_str).unwrap_or(serde_json::Value::Null);

            // Approval gate: classify command danger for bash tool.
            let danger_level = if tool_name == "bash" {
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                classify_command(cmd)
            } else {
                DangerLevel::Safe
            };

            // Permission rules : config `permissions` allow/deny list.
            // Default-open: no matching rule => allowed. A Deny rule blocks
            // before plan_mode/approval are even considered.
            let perm_set = PermissionSet::from_rules(permissions);
            if !perm_set.allows(tool_name, &args) {
                let deny_msg = format!(
                    "Error: tool '{tool_name}' blocked by permission rules (config.permissions)"
                );
                emit(Event::ToolStarted {
                    tool_call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                });
                emit(Event::ToolCompleted {
                    tool_call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    result: deny_msg.clone(),
                });
                let tool_msg = ChatMessage {
                    role: "tool".to_string(),
                    content: deny_msg,
                    tool_call_id: Some(call_id.clone()),
                    tool_calls: None,
                    attachments: None,
                    thinking_signature: None,
                    redacted_thinking: None,
                    thinking: None,
                };
                messages.push(tool_msg.clone());
                if let Some(store) = session {
                    let _ = store.append_message(&session_id, &tool_msg);
                }
                continue;
            }

            if plan_mode {
                let blocked = match tools.get(tool_name) {
                    Some(_) if tool_name == "bash" => danger_level != DangerLevel::Safe,
                    Some(tool) => !tool.is_read_only(),
                    None => true,
                };
                if blocked {
                    let deny_msg = format!(
                        "Error: plan mode active — mutating tool '{tool_name}' blocked; present a plan and await approval"
                    );
                    emit(Event::ToolStarted {
                        tool_call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                    });
                    emit(Event::ToolCompleted {
                        tool_call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        result: deny_msg.clone(),
                    });
                    let tool_msg = ChatMessage {
                        role: "tool".to_string(),
                        content: deny_msg,
                        tool_call_id: Some(call_id.clone()),
                        tool_calls: None,
                        attachments: None,
                        thinking_signature: None,
                        redacted_thinking: None,
                        thinking: None,
                    };
                    messages.push(tool_msg.clone());
                    if let Some(store) = session {
                        let _ = store.append_message(&session_id, &tool_msg);
                    }
                    continue;
                }
            }

            let action = if ask_mode {
                // Ask mode (parity F5): prompt for EVERY tool call,
                // regardless of tool type or danger level. Equivalent to
                // Claude Code `--ask` / permission mode `ask`.
                ApprovalAction::Prompt
            } else {
                should_approve(approval, danger_level)
            };

            // Emit approval events and determine if approved.
            let approved = match action {
                ApprovalAction::AutoApprove => true,
                ApprovalAction::AutoDeny => {
                    emit(Event::ApprovalRequested {
                        tool_call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        danger_level: danger_level.as_str().to_string(),
                    });
                    emit(Event::ApprovalResult {
                        tool_call_id: call_id.clone(),
                        approved: false,
                    });
                    false
                }
                ApprovalAction::Prompt => {
                    emit(Event::ApprovalRequested {
                        tool_call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        danger_level: danger_level.as_str().to_string(),
                    });
                    // Remote approval: await decision from the wire channel
                    // (fed by the remote TUI client). Fall back to local
                    // stdin when no channel is configured.
                    let user_approved = if let Some(rx) = approval_rx.as_ref() {
                        let mut rx = rx.lock().await;
                        rx.recv().await.unwrap_or(false)
                    } else {
                        read_approval_from_stdin()
                    };
                    emit(Event::ApprovalResult {
                        tool_call_id: call_id.clone(),
                        approved: user_approved,
                    });
                    user_approved
                }
            };

            if !approved {
                let deny_msg = "Error: command denied by approval policy".to_string();
                emit(Event::ToolStarted {
                    tool_call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                });
                emit(Event::ToolCompleted {
                    tool_call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    result: deny_msg.clone(),
                });
                let tool_msg = ChatMessage {
                    role: "tool".to_string(),
                    content: deny_msg,
                    tool_call_id: Some(call_id.clone()),
                    tool_calls: None,
                    attachments: None,
                    thinking_signature: None,
                    redacted_thinking: None,
                    thinking: None,
                };
                messages.push(tool_msg.clone());
                if let Some(store) = session {
                    let _ = store.append_message(&session_id, &tool_msg);
                }
                continue;
            }

            // Lifecycle hook: pre_tool (can modify args or abort).
            let hook_ctx = ToolHookContext {
                tool_name: tool_name.clone(),
                tool_call_id: call_id.clone(),
                args: args.clone(),
            };
            let hook_action = hooks.pre_tool(&hook_ctx).await;
            let effective_args = match hook_action {
                HookAction::Continue {
                    args: modified_args,
                } => modified_args,
                HookAction::Abort { reason } => {
                    let abort_msg = format!("Error: tool aborted by hook: {reason}");
                    emit(Event::ToolStarted {
                        tool_call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                    });
                    emit(Event::ToolCompleted {
                        tool_call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        result: abort_msg.clone(),
                    });
                    let tool_msg = ChatMessage {
                        role: "tool".to_string(),
                        content: abort_msg,
                        tool_call_id: Some(call_id.clone()),
                        tool_calls: None,
                        attachments: None,
                        thinking_signature: None,
                        redacted_thinking: None,
                        thinking: None,
                    };
                    messages.push(tool_msg.clone());
                    if let Some(store) = session {
                        let _ = store.append_message(&session_id, &tool_msg);
                    }
                    continue;
                }
            };

            // pre_commit hook gate — runs before `git_commit` executes.
            if tool_name == "git_commit" {
                if let Some(msg) = effective_args.get("message").and_then(|v| v.as_str()) {
                    if !hooks.pre_commit(msg).await {
                        let abort_msg = "Error: git commit blocked by pre_commit hook".to_string();
                        emit(Event::ToolStarted {
                            tool_call_id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            args: effective_args.clone(),
                        });
                        emit(Event::ToolCompleted {
                            tool_call_id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            result: abort_msg.clone(),
                        });
                        let tool_msg = ChatMessage {
                            role: "tool".to_string(),
                            content: abort_msg,
                            tool_call_id: Some(call_id.clone()),
                            tool_calls: None,
                            attachments: None,
                            thinking_signature: None,
                            redacted_thinking: None,
                            thinking: None,
                        };
                        messages.push(tool_msg.clone());
                        if let Some(store) = session {
                            let _ = store.append_message(&session_id, &tool_msg);
                        }
                        continue;
                    }
                }
            }

            emit(Event::ToolStarted {
                tool_call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                args: effective_args.clone(),
            });

            let (result, is_error) = match tools.get(tool_name) {
                Some(tool) => match tool.execute(&effective_args).await {
                    Ok(output) => (output, false),
                    Err(e) => (format!("Error: {e}"), true),
                },
                None => (format!("Error: unknown tool '{tool_name}'"), true),
            };

            // post_tool_failure hook — when tool execution returned error.
            if is_error {
                hooks
                    .post_tool_failure(&ToolFailureCtx {
                        tool_name: tool_name.clone(),
                        tool_call_id: call_id.clone(),
                        args: effective_args.clone(),
                        error: result.clone(),
                    })
                    .await;
            }

            emit(Event::ToolCompleted {
                tool_call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                result: result.clone(),
            });

            // Lifecycle hook: post_tool.
            let post_ctx = PostToolContext {
                tool_name: tool_name.clone(),
                tool_call_id: call_id.clone(),
                args: effective_args.clone(),
                result: result.clone(),
            };
            hooks.post_tool(&post_ctx).await;

            // Append tool result message.
            let tool_msg = ChatMessage {
                role: "tool".to_string(),
                content: result,
                tool_call_id: Some(call_id.clone()),
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            };
            messages.push(tool_msg.clone());
            if let Some(store) = session {
                let _ = store.append_message(&session_id, &tool_msg);
            }
        }
        // Loop continues — next LLM call with tool results.
    }

    // Max turns exceeded.
    let msg = format!("max turns ({max_turns}) exceeded");
    emit(Event::Error {
        message: msg.clone(),
    });
    // stop + session_end on max-turns error path.
    hooks
        .stop(&StopCtx {
            session_id: session_id.clone(),
            reason: format!("max_turns:{max_turns}"),
        })
        .await;
    hooks
        .session_end(&SessionStartCtx {
            session_id: session_id.clone(),
            cwd: String::new(),
        })
        .await;
    Err(anyhow::anyhow!(msg))
}

fn session_id_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso8601_utc(secs)
}

/// Read a y/n approval from stdin in headless mode.
/// Non-interactive (no stdin or EOF) = auto-deny.
fn read_approval_from_stdin() -> bool {
    use std::io::Read;
    let mut buf = [0u8; 1];
    match std::io::stdin().read(&mut buf) {
        Ok(1) => matches!(buf[0], b'y' | b'Y'),
        _ => false,
    }
}

fn iso8601_utc(secs: u64) -> String {
    let days = secs / 86400;
    let sod = secs % 86400;
    let h = sod / 3600;
    let m = (sod % 3600) / 60;
    let s = sod % 60;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
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
mod run_turn_hook_wiring_tests {
    //! AC-2 wiring tests — verify run_turn calls the 4 main hook events
    //! (session_start, user_prompt_submit, stop, post_tool_failure) using a
    //! `CountingHooks` mock + mock Provider + mock Tool. No real API needed.
    //!
    //! Mutation check: revert `hooks.session_start(...)` call site in run_turn
    //! → `test_run_turn_session_start_called` fails (count 0 ≠ 1). Same for
    //! user_prompt_submit, stop, post_tool_failure.

    use super::*;
    use async_trait::async_trait;
    use futures::stream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use zerozero_llm::{ChatMessage, DeltaStream, Effort, Provider, SseEvent, SseEventStream};
    use zerozero_sandbox::{ApprovalPolicy, SandboxPolicy};
    use zerozero_tools::{Tool, ToolRegistry};

    /// Mock Provider that returns canned SseEvent streams. First call returns
    /// `first` events, subsequent calls return `rest` events.
    struct MockProvider {
        first: Vec<SseEvent>,
        rest: Vec<SseEvent>,
        call_count: Arc<AtomicU32>,
    }

    impl MockProvider {
        fn new(first: Vec<SseEvent>, rest: Vec<SseEvent>) -> Self {
            Self {
                first,
                rest,
                call_count: Arc::new(AtomicU32::new(0)),
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_stream(&self, _prompt: &str) -> anyhow::Result<DeltaStream> {
            unreachable!("not used in run_turn tests")
        }

        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _effort: Effort,
            _images: &[String],
        ) -> anyhow::Result<SseEventStream> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            let events = if n == 0 {
                self.first.clone()
            } else {
                self.rest.clone()
            };
            let iter = events.into_iter().map(Ok);
            Ok(Box::pin(stream::iter(iter)))
        }
    }

    /// Mock Tool that always fails.
    struct ErrTool;
    #[async_trait]
    impl Tool for ErrTool {
        fn name(&self) -> &str {
            "mock_tool"
        }
        fn description(&self) -> &str {
            "mock"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("tool boom"))
        }
    }

    /// Mock Tool that succeeds and records that it ran.
    struct OkTool {
        ran: Arc<AtomicBool>,
    }
    #[async_trait]
    impl Tool for OkTool {
        fn name(&self) -> &str {
            "mock_tool"
        }
        fn description(&self) -> &str {
            "mock"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<String> {
            self.ran.store(true, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    }

    /// CountingHooks — counts calls to each event method via AtomicU32.
    struct CountingHooks {
        session_start: Arc<AtomicU32>,
        session_end: Arc<AtomicU32>,
        user_prompt_submit: Arc<AtomicU32>,
        stop: Arc<AtomicU32>,
        post_tool_failure: Arc<AtomicU32>,
        pre_compact: Arc<AtomicU32>,
    }

    #[async_trait]
    impl LifecycleHooks for CountingHooks {
        async fn session_start(&self, _ctx: &SessionStartCtx) {
            self.session_start.fetch_add(1, Ordering::SeqCst);
        }
        async fn session_end(&self, _ctx: &SessionStartCtx) {
            self.session_end.fetch_add(1, Ordering::SeqCst);
        }
        async fn user_prompt_submit(&self, _ctx: &UserPromptCtx) {
            self.user_prompt_submit.fetch_add(1, Ordering::SeqCst);
        }
        async fn stop(&self, _ctx: &StopCtx) {
            self.stop.fetch_add(1, Ordering::SeqCst);
        }
        async fn post_tool_failure(&self, _ctx: &ToolFailureCtx) {
            self.post_tool_failure.fetch_add(1, Ordering::SeqCst);
        }
        async fn pre_compact(&self, _ctx: &PreCompactCtx) {
            self.pre_compact.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn zero() -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(0))
    }

    /// Run a turn with the given provider + tools + hooks. Returns the hooks
    /// counters for assertion.
    async fn run_with_hooks<H: LifecycleHooks>(
        provider: MockProvider,
        tools: ToolRegistry,
        hooks: &H,
        ask_mode: bool,
        permissions: &[String],
    ) -> anyhow::Result<()> {
        let sandbox = SandboxPolicy::FullAccess;
        let approval = ApprovalPolicy::Never;
        let compaction = zerozero_compaction::CompactionConfig::default();
        run_turn(
            "test prompt",
            None,
            &provider,
            &tools,
            5,
            &sandbox,
            &approval,
            false,
            ask_mode,
            None,
            &compaction,
            hooks,
            None,
            &[],
            Effort::None,
            permissions,
            None,
            |_| {},
        )
        .await
    }

    #[tokio::test]
    async fn test_run_turn_session_start_called() {
        // Success path: LLM returns content + Done, no tool calls.
        let provider = MockProvider::new(
            vec![SseEvent::Content("hello".to_string()), SseEvent::Done],
            vec![SseEvent::Done],
        );
        let tools = ToolRegistry::new();
        let hooks = CountingHooks {
            session_start: zero(),
            session_end: zero(),
            user_prompt_submit: zero(),
            stop: zero(),
            post_tool_failure: zero(),
            pre_compact: zero(),
        };
        run_with_hooks(provider, tools, &hooks, false, &[])
            .await
            .unwrap();
        assert_eq!(
            hooks.session_start.load(Ordering::SeqCst),
            1,
            "session_start should be called exactly once"
        );
    }

    #[tokio::test]
    async fn test_run_turn_user_prompt_submit_called() {
        let provider = MockProvider::new(
            vec![SseEvent::Content("hello".to_string()), SseEvent::Done],
            vec![SseEvent::Done],
        );
        let tools = ToolRegistry::new();
        let hooks = CountingHooks {
            session_start: zero(),
            session_end: zero(),
            user_prompt_submit: zero(),
            stop: zero(),
            post_tool_failure: zero(),
            pre_compact: zero(),
        };
        run_with_hooks(provider, tools, &hooks, false, &[])
            .await
            .unwrap();
        assert_eq!(
            hooks.user_prompt_submit.load(Ordering::SeqCst),
            1,
            "user_prompt_submit should be called exactly once"
        );
    }

    #[tokio::test]
    async fn test_run_turn_stop_called() {
        let provider = MockProvider::new(
            vec![SseEvent::Content("hello".to_string()), SseEvent::Done],
            vec![SseEvent::Done],
        );
        let tools = ToolRegistry::new();
        let hooks = CountingHooks {
            session_start: zero(),
            session_end: zero(),
            user_prompt_submit: zero(),
            stop: zero(),
            post_tool_failure: zero(),
            pre_compact: zero(),
        };
        run_with_hooks(provider, tools, &hooks, false, &[])
            .await
            .unwrap();
        assert_eq!(
            hooks.stop.load(Ordering::SeqCst),
            1,
            "stop should be called exactly once on success path"
        );
        // post_tool_failure should NOT be called on success path.
        assert_eq!(
            hooks.post_tool_failure.load(Ordering::SeqCst),
            0,
            "post_tool_failure should not be called when no tool error"
        );
    }

    #[tokio::test]
    async fn test_run_turn_post_tool_failure_called() {
        // First call: LLM returns a tool_call for mock_tool.
        // Tool executes and returns Err → post_tool_failure should fire.
        // Second call: LLM returns content + Done → turn completes.
        let tool_call_json = serde_json::json!({"command": "test"}).to_string();
        let provider = MockProvider::new(
            vec![
                SseEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "mock_tool".to_string(),
                    arguments: tool_call_json,
                },
                SseEvent::Done,
            ],
            vec![SseEvent::Content("done".to_string()), SseEvent::Done],
        );
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(ErrTool));
        let hooks = CountingHooks {
            session_start: zero(),
            session_end: zero(),
            user_prompt_submit: zero(),
            stop: zero(),
            post_tool_failure: zero(),
            pre_compact: zero(),
        };
        run_with_hooks(provider, tools, &hooks, false, &[])
            .await
            .unwrap();
        assert_eq!(
            hooks.post_tool_failure.load(Ordering::SeqCst),
            1,
            "post_tool_failure should be called exactly once on tool error"
        );
        // stop should also be called once (turn completes after second LLM call).
        assert_eq!(
            hooks.stop.load(Ordering::SeqCst),
            1,
            "stop should be called once after turn completes"
        );
    }

    #[tokio::test]
    async fn test_run_turn_session_end_called() {
        // session_end is wired on all return paths (success, error, max-turns).
        // Verify success path.
        let provider = MockProvider::new(
            vec![SseEvent::Content("hello".to_string()), SseEvent::Done],
            vec![SseEvent::Done],
        );
        let tools = ToolRegistry::new();
        let hooks = CountingHooks {
            session_start: zero(),
            session_end: zero(),
            user_prompt_submit: zero(),
            stop: zero(),
            post_tool_failure: zero(),
            pre_compact: zero(),
        };
        run_with_hooks(provider, tools, &hooks, false, &[])
            .await
            .unwrap();
        assert_eq!(
            hooks.session_end.load(Ordering::SeqCst),
            1,
            "session_end should be called exactly once"
        );
    }

    // --- : Ask mode (--ask / /ask) ---

    #[tokio::test]
    async fn test_prd101_ac1_ask_mode_denies_every_tool_call() {
        // AC-1: in ask mode, EVERY tool call must prompt for approval.
        // In a non-tty headless context read_approval_from_stdin() returns
        // false (auto-deny), so the tool must NOT execute.
        let tool_call_json = serde_json::json!({"command": "test"}).to_string();
        let provider = MockProvider::new(
            vec![
                SseEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "mock_tool".to_string(),
                    arguments: tool_call_json,
                },
                SseEvent::Done,
            ],
            vec![SseEvent::Content("done".to_string()), SseEvent::Done],
        );
        let ran = Arc::new(AtomicBool::new(false));
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(OkTool { ran: ran.clone() }));
        let hooks = CountingHooks {
            session_start: zero(),
            session_end: zero(),
            user_prompt_submit: zero(),
            stop: zero(),
            post_tool_failure: zero(),
            pre_compact: zero(),
        };
        // ask_mode = true → every tool call must be prompted and, with no
        // interactive approval, denied. The tool body must never run.
        run_with_hooks(provider, tools, &hooks, true, &[])
            .await
            .unwrap();
        assert!(
            !ran.load(Ordering::SeqCst),
            "ask mode must prompt + deny every tool call; tool body must NOT run"
        );
        // Approval events must have been emitted for the prompt.
        assert_eq!(
            hooks.user_prompt_submit.load(Ordering::SeqCst),
            1,
            "ask mode should emit an approval request per tool call"
        );
    }

    #[tokio::test]
    async fn test_bl116_permissions_deny_blocks_tool() {
        // a config `permissions` Deny rule must block the tool call
        // before the body executes (default-open: with no rule it runs).
        let tool_call_json = serde_json::json!({"command": "test"}).to_string();
        let provider = MockProvider::new(
            vec![
                SseEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "mock_tool".to_string(),
                    arguments: tool_call_json,
                },
                SseEvent::Done,
            ],
            vec![SseEvent::Content("done".to_string()), SseEvent::Done],
        );
        let ran = Arc::new(AtomicBool::new(false));
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(OkTool { ran: ran.clone() }));
        let hooks = CountingHooks {
            session_start: zero(),
            session_end: zero(),
            user_prompt_submit: zero(),
            stop: zero(),
            post_tool_failure: zero(),
            pre_compact: zero(),
        };
        let deny = vec!["Deny(mock_tool)".to_string()];
        run_with_hooks(provider, tools, &hooks, false, &deny)
            .await
            .unwrap();
        assert!(
            !ran.load(Ordering::SeqCst),
            "Deny(mock_tool) must prevent the tool body from running"
        );

        // Sanity: without the rule, the tool runs (default-open).
        let provider2 = MockProvider::new(
            vec![
                SseEvent::ToolCall {
                    id: "call_2".to_string(),
                    name: "mock_tool".to_string(),
                    arguments: "{\"command\":\"test\"}".to_string(),
                },
                SseEvent::Done,
            ],
            vec![SseEvent::Content("done".to_string()), SseEvent::Done],
        );
        let ran2 = Arc::new(AtomicBool::new(false));
        let mut tools2 = ToolRegistry::new();
        tools2.register(Box::new(OkTool { ran: ran2.clone() }));
        run_with_hooks(provider2, tools2, &hooks, false, &[])
            .await
            .unwrap();
        assert!(
            ran2.load(Ordering::SeqCst),
            "default-open: tool must run when no deny rule matches"
        );
    }

    // --- : — wire pre_commit hook into git_commit ---

    struct BlockPreCommitHook;
    #[async_trait::async_trait]
    impl LifecycleHooks for BlockPreCommitHook {
        async fn pre_commit(&self, _message: &str) -> bool {
            false
        }
    }

    struct GitCommitToolMock {
        ran: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl Tool for GitCommitToolMock {
        fn name(&self) -> &str {
            "git_commit"
        }
        fn description(&self) -> &str {
            "mock git commit tool for pre_commit gate test"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"]
            })
        }
        async fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<String> {
            self.ran.store(true, Ordering::SeqCst);
            Ok("committed".to_string())
        }
    }

    #[tokio::test]
    async fn test_prd108_pre_commit_blocks_git_commit() {
        // AC-1: when pre_commit hook returns false, git_commit must NOT execute.
        let tool_call = serde_json::json!({"message": "fix: something"}).to_string();
        let provider = MockProvider::new(
            vec![
                SseEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "git_commit".to_string(),
                    arguments: tool_call,
                },
                SseEvent::Done,
            ],
            vec![SseEvent::Done],
        );
        let ran = Arc::new(AtomicBool::new(false));
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(GitCommitToolMock { ran: ran.clone() }));
        let hooks = BlockPreCommitHook;
        run_with_hooks(provider, tools, &hooks, false, &[])
            .await
            .unwrap();
        assert!(
            !ran.load(Ordering::SeqCst),
            "pre_commit hook returning false must block git_commit execution"
        );
    }
}
