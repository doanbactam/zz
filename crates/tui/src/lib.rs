pub mod agent_nav;
pub mod agent_picker;
pub mod agent_tree;
pub mod app;
pub mod approval;
pub mod composer;
pub mod connect;
pub mod diff;
pub mod event;
pub mod markdown;
pub mod model_catalog;
pub mod model_picker;
pub mod skills_browser;
pub mod slash;
pub mod slash_menu;
pub mod status_indicator;
pub mod strip_ansi;
pub mod theme;
pub mod ui;

use std::io::{Stdout, stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use app::{App, KeyAction, SlashAction};
use crossterm::{
    event::{Event as CrosstermEvent, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use event::AppEvent;
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use zerozero_compaction::CompactionConfig;
use zerozero_exec::Event;
use zerozero_llm::Provider;
use zerozero_sandbox::{ApprovalPolicy, SandboxPolicy};
use zerozero_tools::ToolRegistry;

type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Run the async TUI app. Entry point for `zz` (no subcommand).
#[allow(clippy::too_many_arguments)]
pub async fn run_async(
    provider: std::sync::Arc<dyn Provider>,
    tools: ToolRegistry,
    sandbox: SandboxPolicy,
    approval: ApprovalPolicy,
    compaction_config: CompactionConfig,
    skill_names: Vec<String>,
    skill_dirs: Vec<std::path::PathBuf>,
    plugin_names: Vec<String>,
    plugin_dirs: Vec<std::path::PathBuf>,
    session_db_path: Option<std::path::PathBuf>,
    system_prompt: Option<String>,
    provider_factory: Box<dyn Fn(String, String) -> Box<dyn Provider> + Send + Sync>,
    initial_model: String,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Set terminal title (Codex parity).
    execute!(stdout, crossterm::terminal::SetTitle("ZeroZero"))?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = run_app(
        terminal,
        provider,
        tools,
        sandbox,
        approval,
        compaction_config,
        skill_names,
        skill_dirs,
        plugin_names,
        plugin_dirs,
        session_db_path,
        system_prompt,
        provider_factory,
        initial_model,
    )
    .await;

    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    // Reset terminal title on exit.
    execute!(std::io::stdout(), crossterm::terminal::SetTitle(""))?;

    result
}

#[allow(clippy::too_many_arguments)]
async fn run_app(
    mut terminal: TuiTerminal,
    provider: std::sync::Arc<dyn Provider>,
    mut tools: ToolRegistry,
    sandbox: SandboxPolicy,
    approval: ApprovalPolicy,
    compaction_config: CompactionConfig,
    skill_names: Vec<String>,
    skill_dirs: Vec<std::path::PathBuf>,
    plugin_names: Vec<String>,
    plugin_dirs: Vec<std::path::PathBuf>,
    session_db_path: Option<std::path::PathBuf>,
    system_prompt: Option<String>,
    provider_factory: Box<dyn Fn(String, String) -> Box<dyn Provider> + Send + Sync>,
    initial_model: String,
) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new();
    app.set_skill_dirs(skill_dirs);
    if !skill_names.is_empty() && app.skill_names.is_empty() {
        app.set_skills(skill_names);
    }
    app.set_plugins(plugin_names);
    app.set_plugin_dirs(plugin_dirs);
    if let Some(path) = &session_db_path {
        app.set_session_db_path(path.clone());
    }
    // : store initial model name for /model command.
    app.model = initial_model;
    // 3-tier picker: record the active provider id at startup.
    app.provider = std::env::var("ZZ_PROVIDER").unwrap_or_else(|_| "xai".to_string());

    // : Create ThreadRegistry for interactive multi-agent.
    // The root thread ID is set as the active thread in the App.
    let (thread_registry, root_thread_id) = zerozero_multi_agent::ThreadRegistry::new(6, 1);
    app.set_active_thread_id(root_thread_id.clone());

    // : provider is mutable so /model can hot-swap it.
    // The provider_factory is wrapped in Arc so the event loop can call
    // it to rebuild the provider with a new model name.
    let mut provider: Arc<dyn Provider> = provider;
    let provider_factory = Arc::new(provider_factory);

    // fix: Wire SpawnAgentTool into ToolRegistry so the LLM
    // can call `spawn_agent` to create subagent threads.
    //
    // The SpawnContext provides tools for *subagents* (not the main thread).
    // Subagents get the standard tool set (without SpawnAgentTool itself,
    // since max_depth=1 prevents sub-subagent spawning). The main thread's
    // ToolRegistry gets SpawnAgentTool registered below.
    let thread_registry = Arc::new(thread_registry);
    let emit_thread_event: Arc<dyn Fn(zerozero_multi_agent::ThreadId, Event) + Send + Sync> = {
        let tx = tx.clone();
        Arc::new(move |tid, event| {
            let _ = tx.send(AppEvent::AgentThread(tid, event));
        })
    };
    let spawn_ctx = zerozero_multi_agent::SpawnContext {
        provider: Arc::clone(&provider),
        tools: Arc::new(ToolRegistry::standard(Arc::new(sandbox.clone()))),
        sandbox: sandbox.clone(),
        approval,
        compaction_config: compaction_config.clone(),
        session_db_path: session_db_path.clone(),
        system_prompt: system_prompt.clone(),
        max_turns: 10,
        effort: zerozero_llm::Effort::None,
        emit_thread_event: Some(emit_thread_event),
    };
    let spawn_tool = zerozero_multi_agent::SpawnAgentTool::new(
        Arc::clone(&thread_registry),
        Arc::new(spawn_ctx),
        root_thread_id,
    );
    tools.register(Box::new(spawn_tool));
    let tools = Arc::new(tools);

    // Spawn crossterm event reader task (Send — safe with tokio::spawn).
    // Drop KeyEventKind::Release here so they never reach the UI (Windows
    // emits Press+Release per key; handling both = double characters).
    let event_tx = tx.clone();
    tokio::spawn(async move {
        let mut reader = crossterm::event::EventStream::new();
        while let Some(Ok(event)) = reader.next().await {
            if let CrosstermEvent::Key(key) = &event {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
            }
            if event_tx.send(AppEvent::Crossterm(event)).is_err() {
                break;
            }
        }
    });

    // Spawn a 250ms ticker for spinner animation during streaming.
    let tick_tx = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            interval.tick().await;
            if tick_tx.send(AppEvent::Tick).is_err() {
                break;
            }
        }
    });

    // Use LocalSet so we can spawn_local the LLM task (run_turn future
    // is !Send because SessionStore contains RefCell).
    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            // Initial frame before the first event.
            terminal.draw(|f| ui::render(f, &app))?;
            // Windows can emit two KeyEventKind::Press for one physical tap
            // (console + VT / duplicate KEY_EVENT). Drop same-char presses
            // within a short window — shorter than human double-tap (~100ms)
            // and first auto-repeat delay (~400ms), longer than OS dup (~1ms).
            let mut last_char_press: Option<(char, Instant)> = None;
            const CHAR_DEDUP: Duration = Duration::from_millis(40);
            loop {
                // Wait for a meaningful event. Idle `Tick`s arrive every 250ms even
                // when not streaming — redrawing on those caused status/chat flicker
                // ("text flicker") on Windows terminals.
                let event = loop {
                    match rx.recv().await {
                        Some(AppEvent::Tick) if !app.is_streaming => continue,
                        other => break other,
                    }
                };
                match event {
                    Some(app_event) => {
                        match app_event {
                            AppEvent::Crossterm(CrosstermEvent::Key(key)) => {
                                // Belt-and-suspenders: only Press (stream already
                                // drops Release; handle_key also checks is_press).
                                if key.kind != KeyEventKind::Press {
                                    continue;
                                }
                                if let KeyCode::Char(c) = key.code {
                                    if !c.is_control() {
                                        if let Some((prev, t0)) = last_char_press {
                                            if prev == c && t0.elapsed() < CHAR_DEDUP {
                                                // Duplicate Press of the same char.
                                                continue;
                                            }
                                        }
                                        last_char_press = Some((c, Instant::now()));
                                    }
                                }
                                let action = app.handle_key(key);
                                match action {
                                    KeyAction::Submit => {
                                        // Defer API-key gate to first message so users can
                                        // open the TUI and run /connect without a pre-set key.
                                        let active_provider = if app.provider.is_empty() {
                                            "xai".to_string()
                                        } else {
                                            app.provider.clone()
                                        };
                                        if !zerozero_llm::has_api_key(&active_provider) {
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "No API key for provider '{active_provider}'. \
                                                     Opening /connect — paste your key and press Enter.\n\
                                                     (CLI alternative: zz connect {active_provider})"
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            // Keep the draft so the user can re-submit after connecting.
                                            app.open_connect(Some(&active_provider));
                                            // Fall through to redraw (do not `continue` — draw is at loop end).
                                        } else {
                                        let prompt = app.composer.input_buffer.clone();
                                        app.composer.input_buffer.clear();
                                        app.composer.cursor_pos = 0;
                                        app.is_streaming = true;
                                        app.status_indicator.start();
                                        app.footer_mode = app::FooterMode::Running;
                                        // record prompt in history.
                                        app.record_prompt(&prompt);
                                        let images = app.composer.pending_images.clone();
                                        let prior_messages = app.messages.clone();
                                        app.messages.push(zerozero_llm::ChatMessage {
                                            role: "user".to_string(),
                                            content: prompt.clone(),
                                            tool_call_id: None,
                                            tool_calls: None,
                                            attachments: if images.is_empty() {
                                                None
                                            } else {
                                                Some(
                                                    images
                                                        .iter()
                                                        .map(|u| {
                                                            zerozero_llm::ImageAttachment::from_data_url(
                                                                u.clone(),
                                                            )
                                                        })
                                                        .collect(),
                                                )
                                            },
                                            thinking_signature: None,
                                            redacted_thinking: None,
                                            thinking: None,
                                        });
                                        app.record_message_timestamp();
                                        app.composer.pending_images.clear();

                                        // Spawn LLM task on local set (!Send OK).
                                        let core_tx = tx.clone();
                                        let provider = std::sync::Arc::clone(&provider);
                                        let tools = Arc::clone(&tools);
                                        let sandbox = sandbox.clone();
                                        let compaction_config = compaction_config.clone();
                                        let system_prompt = system_prompt.clone();
                                        let effort = app.effort;
                                        let ask_mode = app.ask_mode;
                                        app.persist_active_chat();
                                        tokio::task::spawn_local(async move {
                                            let emit = move |event: Event| {
                                                let _ = core_tx.send(AppEvent::Core(event));
                                            };
                                            let _ = zerozero_core::run_turn(
                                                &prompt,
                                                system_prompt.as_deref(),
                                                &*provider,
                                                &tools,
                                                10,
                                                &sandbox,
                                                &approval,
                                                false,
                                                ask_mode,
                                                None,
                                                &compaction_config,
                                                &zerozero_core::NoopHooks,
                                                None,
                                                &prior_messages,
                                                effort,
                                                &[],
                                                None,
                                                emit,
                                            )
                                            .await;
                                        });
                                        }
                                    }
                                    KeyAction::Quit => break,
                                    KeyAction::Slash(action) => match action {
                                        SlashAction::Quit => break,
                                        SlashAction::ClearChat => {
                                            app.messages.clear();
                                            app.streaming_text.clear();
                                            app.streaming_reasoning_text.clear();
                                            app.tool_events.clear();
                                            app.scroll_chat_to_bottom();
                                            app.persist_active_chat();
                                        }
                                        SlashAction::ToggleDiff => {
                                            app.show_diff = !app.show_diff;
                                        }
                                        SlashAction::ShowHelp => {
                                            app.show_help_overlay = true;
                                            app.help_scroll = 0;
                                        }
                                        SlashAction::ShowMessage(msg) => {
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: msg,
                                                tool_call_id: None,
                                                tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                        }
                                        SlashAction::OpenAgentPicker => {
                                            // fix: Refresh
                                            // live_agents snapshot when opening
                                            // the picker so it shows current
                                            // thread state.
                                            app.set_live_agents(
                                                thread_registry.live_agents().await,
                                            );
                                            app.show_agent_picker = true;
                                            app.agent_picker_selected = 0;
                                        }
                                        SlashAction::SetEffort(effort) => {
                                            // : update effort
                                            // state for subsequent run_turn
                                            // calls. Show confirmation.
                                            app.effort = effort;
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!("Effort set to: {effort}"),
                                                tool_call_id: None,
                                                tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                        }
                                        SlashAction::SetModel(model_name) => {
                                            // : Rebuild
                                            // provider with new model and
                                            // swap the Arc. Subsequent
                                            // run_turn calls use the new
                                            // model. Subagents (spawn_ctx)
                                            // keep the original provider —
                                            // acceptable for MVP.
                                            let ptype = if app.provider.is_empty() {
                                                crate::model_catalog::detect_provider_for_model(
                                                    &model_name,
                                                )
                                                .to_string()
                                            } else {
                                                app.provider.clone()
                                            };
                                            let new_provider =
                                                provider_factory(ptype.clone(), model_name.clone());
                                            provider = Arc::from(new_provider);
                                            app.provider = ptype;
                                            app.model = model_name.clone();
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!("Model set to: {model_name}"),
                                                tool_call_id: None,
                                                tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                        }
                                        SlashAction::SetModelFull {
                                            provider: ptype,
                                            model: model_name,
                                            effort,
                                        } => {
                                            // 3-tier picker: rebuild provider
                                            // with the new provider type +
                                            // model, swap the Arc, and apply
                                            // the chosen effort.
                                            let new_provider =
                                                provider_factory(ptype.clone(), model_name.clone());
                                            provider = Arc::from(new_provider);
                                            app.provider = ptype.clone();
                                            app.model = model_name.clone();
                                            app.effort = effort;
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Model: {model_name} ({ptype}) · effort: {effort}"
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                        }
                                        SlashAction::OpenModelPicker => {
                                            app.open_model_picker();
                                        }
                                        SlashAction::ToggleAsk => {
                                            // : toggle ask
                                            // mode. The new value flows into the
                                            // next run_turn call's ask_mode arg.
                                            app.ask_mode = !app.ask_mode;
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Ask mode: {}",
                                                    if app.ask_mode { "ON" } else { "OFF" }
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                        }
                                        SlashAction::Find(query) => {
                                            // fuzzy file-path search over the cwd.
                                            let cwd = std::env::current_dir().unwrap_or_default();
                                            let results =
                                                zerozero_tools::fuzzy_find_files(&cwd, &query, 20);
                                            let msg = if results.is_empty() {
                                                format!("No files matching '{query}'")
                                            } else {
                                                let mut out =
                                                    format!("Files matching '{query}':\n");
                                                for path in &results {
                                                    out.push_str(&format!("  {path}\n"));
                                                }
                                                out
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: msg,
                                                tool_call_id: None,
                                                tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                        }
                                        SlashAction::Rewind(path) => {
                                            //B : restore file
                                            // from its shadow snapshot.
                                            let result = zerozero_tools::snapshot::rewind(
                                                std::path::Path::new(&path),
                                            );
                                            let msg = match result {
                                                Ok(()) => format!("Rewound {path}"),
                                                Err(e) => format!("Rewind failed: {e}"),
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: msg,
                                                tool_call_id: None,
                                                tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                        }
                                        SlashAction::OpenSkillsBrowser => {
                                            app.show_skills_browser = true;
                                            app.skills_browser_index = 0;
                                        }
                                        SlashAction::InvokeSkillChain(names, args) => {
                                            if let Some(skill_block) =
                                                app.skill_prompt_blocks(&names, &args)
                                            {
                                                let turn_system = match system_prompt.as_deref() {
                                                    Some(base) => {
                                                        Some(format!("{base}\n\n{skill_block}"))
                                                    }
                                                    None => Some(skill_block),
                                                };
                                                let label = format!(
                                                    "{} {}",
                                                    names
                                                        .iter()
                                                        .map(|n| format!("/{n}"))
                                                        .collect::<Vec<_>>()
                                                        .join(" "),
                                                    args
                                                );
                                                app.is_streaming = true;
                                        app.status_indicator.start();
                                        app.footer_mode = app::FooterMode::Running;
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "user".to_string(),
                                                    content: label,
                                                    tool_call_id: None,
                                                    tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                                let core_tx = tx.clone();
                                                let provider = std::sync::Arc::clone(&provider);
                                                let tools = Arc::clone(&tools);
                                                let sandbox = sandbox.clone();
                                                let compaction_config = compaction_config.clone();
                                                let effort = app.effort;
                                                let ask_mode = app.ask_mode;
                                                let prompt = args.clone();
                                                tokio::task::spawn_local(async move {
                                                    let emit = move |event: Event| {
                                                        let _ = core_tx.send(AppEvent::Core(event));
                                                    };
                                                    let _ = zerozero_core::run_turn(
                                                        &prompt,
                                                        turn_system.as_deref(),
                                                        &*provider,
                                                        &tools,
                                                        10,
                                                        &sandbox,
                                                        &approval,
                                                        false,
                                                        ask_mode,
                                                        None,
                                                        &compaction_config,
                                                        &zerozero_core::NoopHooks,
                                                        None,
                                                        &[],
                                                        effort,
                                                        &[],
                                                        None,
                                                        emit,
                                                    )
                                                    .await;
                                                });
                                            } else {
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "system".to_string(),
                                                    content: "Skill chain: not found".to_string(),
                                                    tool_call_id: None,
                                                    tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                            }
                                        }
                                        SlashAction::InvokeSkill(name, args) => {
                                            if args.is_empty() {
                                                let msg =
                                                    format!("Usage: /{name} <task>  (see /skills)");
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "system".to_string(),
                                                    content: msg,
                                                    tool_call_id: None,
                                                    tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                            } else if let Some(skill_block) =
                                                app.skill_prompt_block(&name, &args)
                                            {
                                                let turn_system = match system_prompt.as_deref() {
                                                    Some(base) => {
                                                        Some(format!("{base}\n\n{skill_block}"))
                                                    }
                                                    None => Some(skill_block),
                                                };
                                                let label = format!("/{name} {args}");
                                                app.is_streaming = true;
                                        app.status_indicator.start();
                                        app.footer_mode = app::FooterMode::Running;
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "user".to_string(),
                                                    content: label.clone(),
                                                    tool_call_id: None,
                                                    tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                                let core_tx = tx.clone();
                                                let provider = std::sync::Arc::clone(&provider);
                                                let tools = Arc::clone(&tools);
                                                let sandbox = sandbox.clone();
                                                let compaction_config = compaction_config.clone();
                                                let effort = app.effort;
                                                let ask_mode = app.ask_mode;
                                                let prompt = args.clone();
                                                tokio::task::spawn_local(async move {
                                                    let emit = move |event: Event| {
                                                        let _ = core_tx.send(AppEvent::Core(event));
                                                    };
                                                    let _ = zerozero_core::run_turn(
                                                        &prompt,
                                                        turn_system.as_deref(),
                                                        &*provider,
                                                        &tools,
                                                        10,
                                                        &sandbox,
                                                        &approval,
                                                        false,
                                                        ask_mode,
                                                        None,
                                                        &compaction_config,
                                                        &zerozero_core::NoopHooks,
                                                        None,
                                                        &[],
                                                        effort,
                                                        &[],
                                                        None,
                                                        emit,
                                                    )
                                                    .await;
                                                });
                                            } else {
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "system".to_string(),
                                                    content: format!("Skill not found: {name}"),
                                                    tool_call_id: None,
                                                    tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                            }
                                        }
                                        SlashAction::Compact => {
                                            // : manual token-budget
                                            // compaction. Summarize all but the most
                                            // recent `keep_recent_turns` messages into a
                                            // single system summary message. If nothing
                                            // was compacted (too few messages), report so.
                                            let before = app.messages.len();
                                            let before_tokens =
                                                zerozero_compaction::count_tokens(&app.messages);
                                            let compacted =
                                                zerozero_compaction::compact_token_budget(
                                                    app.messages.clone(),
                                                    &compaction_config,
                                                    zerozero_compaction::fallback_summary,
                                                );
                                            let after = compacted.len();
                                            app.messages = compacted;
                                            app.persist_active_chat();
                                            if after < before {
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "system".to_string(),
                                                    content: format!(
                                                        "Context compacted: {} messages → {} \
                                                         ({} tokens estimated before)",
                                                        before, after, before_tokens
                                                    ),
                                                    tool_call_id: None,
                                                    tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                            } else {
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "system".to_string(),
                                                    content: "Nothing to compact (conversation \
                                                               is within the recent-turns window)"
                                                        .to_string(),
                                                    tool_call_id: None,
                                                    tool_calls: None,
    attachments: None,
    thinking_signature: None,
    redacted_thinking: None,
    thinking: None,
});
                                            }
                                        }
                                        SlashAction::Image(path) => {
                                            // attach an image to the next message.
                                            let p = std::path::Path::new(&path);
                                            match std::fs::read(p) {
                                                Ok(bytes) => {
                                                    let mime = zerozero_llm::mime_from_path(p);
                                                    let url = zerozero_llm::ImageAttachment::from_bytes_with_name(
                                                        Some(path.clone()),
                                                        mime,
                                                        &bytes,
                                                    )
                                                    .data_url;
                                                    app.composer.pending_images.push(url.clone());
                                                    app.messages.push(zerozero_llm::ChatMessage {
                                                        role: "system".to_string(),
                                                        content: format!(
                                                            "📎 Attached image ({} bytes). It will be sent with your next message.",
                                                            bytes.len()
                                                        ),
                                                        tool_call_id: None,
                                                        tool_calls: None,
                                                        attachments: None,
                                                        thinking_signature: None,
                                                        redacted_thinking: None,
                                                        thinking: None,
                                                    });
                                                }
                                                Err(e) => {
                                                    app.messages.push(zerozero_llm::ChatMessage {
                                                        role: "system".to_string(),
                                                        content: format!("Failed to read image {path}: {e}"),
                                                        tool_call_id: None,
                                                        tool_calls: None,
                                                        attachments: None,
                                                        thinking_signature: None,
                                                        redacted_thinking: None,
                                                        thinking: None,
                                                    });
                                                }
                                            }
                                        }
                                        SlashAction::Unimage => {
                                            app.composer.pending_images.clear();
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: "Cleared pending image attachments.".to_string(),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                        }
                                        SlashAction::CopyOutput => {
                                            // /copy — same as Ctrl+O.
                                            let msg = match app.latest_assistant_output() {
                                                Some(text) => {
                                                    match arboard::Clipboard::new() {
                                                        Ok(mut cb) => {
                                                            match cb.set_text(text) {
                                                                Ok(()) => "Copied to clipboard.".to_string(),
                                                                Err(e) => format!("Clipboard error: {e}"),
                                                            }
                                                        }
                                                        Err(e) => format!("Clipboard unavailable: {e}"),
                                                    }
                                                }
                                                None => "No assistant output to copy.".to_string(),
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: msg,
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                        }
                                        SlashAction::OpenThemePicker => {
                                            // /theme (no arg) — open picker.
                                            app.show_theme_picker = true;
                                            app.theme_picker_index = 0;
                                        }
                                        SlashAction::SetTheme(name) => {
                                            // /theme <name> — set theme directly.
                                            let msg = match crate::markdown::set_theme(&name) {
                                                Ok(()) => format!("Theme set to: {name}"),
                                                Err(e) => e,
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: msg,
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                        }
                                        SlashAction::SetUiTheme(name) => {
                                            // /ui-theme <name> — switch the UI chrome palette.
                                            let msg = match crate::theme::set_theme(&name) {
                                                Ok(t) => format!("UI theme set to: {}", t.name),
                                                Err(e) => e,
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: msg,
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                        }
                                        SlashAction::OpenConnect(provider) => {
                                            app.open_connect(provider.as_deref());
                                            app.composer.input_buffer.clear();
                                            app.composer.cursor_pos = 0;
                                            app.composer.show_slash_palette = false;
                                        }
                                        SlashAction::ProviderConnected { provider: ptype, message } => {
                                            // Rebuild LLM client so the new key is used immediately.
                                            let model = if app.model.is_empty() {
                                                zerozero_llm::provider_spec(&ptype)
                                                    .default_model
                                                    .to_string()
                                            } else {
                                                app.model.clone()
                                            };
                                            let new_provider =
                                                provider_factory(ptype.clone(), model.clone());
                                            provider = Arc::from(new_provider);
                                            app.provider = ptype.clone();
                                            app.model = model.clone();
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "{message}\nActive provider set to {ptype} · model {model}. You can chat now."
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                        }
                                        SlashAction::ProviderLoggedOut(msg) => {
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: msg,
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        // Grok CLI TUI parity slash actions.
                                        SlashAction::EnterPlan(desc) => {
                                            app.session_mode = app::SessionMode::Plan;
                                            if !desc.is_empty() {
                                                app.plan_text = format!("# Plan\n\n{desc}\n");
                                                let plan_path = std::path::Path::new(".zz/plan.md");
                                                let _ = std::fs::create_dir_all(".zz");
                                                let _ = std::fs::write(plan_path, &app.plan_text);
                                            }
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Plan mode activated.{}\nPlan file: .zz/plan.md\nUse /view-plan to see it. Shift+Tab to cycle modes.",
                                                    if desc.is_empty() { String::new() } else { format!(" Description: {desc}") }
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ViewPlan => {
                                            let content = if app.plan_text.is_empty() {
                                                "No plan set. Use /plan <description> to create one.".to_string()
                                            } else {
                                                app.plan_text.clone()
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!("Current plan:\n\n{content}"),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ToggleAlwaysApprove => {
                                            app.always_approve = !app.always_approve;
                                            app.session_mode = if app.always_approve {
                                                app::SessionMode::AlwaysApprove
                                            } else {
                                                app::SessionMode::Normal
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Always-approve mode: {}",
                                                    if app.always_approve { "ON — tool calls auto-approved" } else { "OFF" }
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ToggleMultiline => {
                                            app.multiline = !app.multiline;
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Multiline input: {}\n{}",
                                                    if app.multiline { "ON" } else { "OFF" },
                                                    if app.multiline {
                                                        "Enter = newline, Ctrl+Enter = submit"
                                                    } else {
                                                        "Enter = submit, Alt+Enter = newline"
                                                    }
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ShowContext => {
                                            let tokens = app.token_count();
                                            let msg_count = app.messages.len();
                                            let context_limit = 128_000; // typical context window
                                            let pct = (tokens * 100)
                                                .checked_div(context_limit)
                                                .map(|v| v.min(100))
                                                .unwrap_or(0);
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Context usage:\n  Messages: {msg_count}\n  Estimated tokens: ~{tokens}\n  Context window: ~{context_limit}\n  Usage: {pct}%"
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ToggleCompactMode => {
                                            app.compact_mode = !app.compact_mode;
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Compact mode: {}",
                                                    if app.compact_mode { "ON — denser layout" } else { "OFF" }
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ToggleTimestamps => {
                                            app.show_timestamps = !app.show_timestamps;
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Timestamps: {}",
                                                    if app.show_timestamps { "ON" } else { "OFF" }
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ToggleVimMode => {
                                            app.vim_mode = !app.vim_mode;
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Vim mode: {}\n{}",
                                                    if app.vim_mode { "ON" } else { "OFF" },
                                                    if app.vim_mode {
                                                        "j/k scroll, g/G top/bottom, Ctrl+U/D half-page"
                                                    } else {
                                                        "Use PgUp/PgDn to scroll"
                                                    }
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ShowShortcuts => {
                                            app.show_shortcuts_overlay = true;
                                        }
                                        SlashAction::ExportConversation => {
                                            let content = app.export_conversation();
                                            let timestamp = std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .map(|d| d.as_secs())
                                                .unwrap_or(0);
                                            let export_dir = std::env::var("HOME")
                                                .map(|h| format!("{h}/.zz"))
                                                .unwrap_or_else(|_| ".zz".to_string());
                                            let _ = std::fs::create_dir_all(&export_dir);
                                            let export_path = format!("{export_dir}/export_{timestamp}.txt");
                                            match std::fs::write(&export_path, &content) {
                                                Ok(()) => {
                                                    app.messages.push(zerozero_llm::ChatMessage {
                                                        role: "system".to_string(),
                                                        content: format!("Conversation exported to: {export_path}"),
                                                        tool_call_id: None,
                                                        tool_calls: None,
                                                        attachments: None,
                                                        thinking_signature: None,
                                                        redacted_thinking: None,
                                                        thinking: None,
                                                    });
                                                }
                                                Err(e) => {
                                                    app.messages.push(zerozero_llm::ChatMessage {
                                                        role: "system".to_string(),
                                                        content: format!("Export failed: {e}"),
                                                        tool_call_id: None,
                                                        tool_calls: None,
                                                        attachments: None,
                                                        thinking_signature: None,
                                                        redacted_thinking: None,
                                                        thinking: None,
                                                    });
                                                }
                                            }
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::ShowTranscript => {
                                            let content = app.export_conversation();
                                            let pager = std::env::var("PAGER")
                                                .unwrap_or_else(|_| "less".to_string());
                                            let mut child = std::process::Command::new(&pager)
                                                .stdin(std::process::Stdio::piped())
                                                .spawn();
                                            match child.as_mut() {
                                                Ok(proc) => {
                                                    if let Some(mut stdin) = proc.stdin.take() {
                                                        use std::io::Write;
                                                        let _ = stdin.write_all(content.as_bytes());
                                                    }
                                                    let _ = proc.wait();
                                                    // No message needed — user saw the transcript
                                                }
                                                Err(e) => {
                                                    app.messages.push(zerozero_llm::ChatMessage {
                                                        role: "system".to_string(),
                                                        content: format!("Failed to launch pager '{pager}': {e}"),
                                                        tool_call_id: None,
                                                        tool_calls: None,
                                                        attachments: None,
                                                        thinking_signature: None,
                                                        redacted_thinking: None,
                                                        thinking: None,
                                                    });
                                                    app.record_message_timestamp();
                                                }
                                            }
                                        }
                                        // /status — show model, approval, tokens, cwd.
                                        SlashAction::ShowStatus => {
                                            let provider = if app.provider.is_empty() {
                                                "xai"
                                            } else {
                                                app.provider.as_str()
                                            };
                                            let model = if app.model.is_empty() {
                                                "default"
                                            } else {
                                                app.model.as_str()
                                            };
                                            let tokens = app.token_count();
                                            let cwd = std::env::current_dir()
                                                .map(|p| p.display().to_string())
                                                .unwrap_or_else(|_| "(unknown)".to_string());
                                            let approval = if app.always_approve {
                                                "always-approve"
                                            } else {
                                                "on-request"
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!(
                                                    "Status:\n  Provider: {provider}\n  Model: {model}\n  Approval: {approval}\n  Tokens: ~{tokens}\n  Working dir: {cwd}\n  Session: {}",
                                                    if app.session_id.is_empty() { "(none)" } else { &app.session_id }
                                                ),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        // /new — start a fresh session.
                                        SlashAction::NewSession => {
                                            app.messages.clear();
                                            app.streaming_text.clear();
                                            app.streaming_reasoning_text.clear();
                                            app.tool_events.clear();
                                            app.message_timestamps.clear();
                                            app.session_id = format!("zz-{}", std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .map(|d| d.as_secs())
                                                .unwrap_or(0));
                                            app.scroll_chat_to_bottom();
                                            app.persist_active_chat();
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!("New session started: {}", app.session_id),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        // !cmd — run local shell command.
                                        SlashAction::ShellCommand(cmd) => {
                                            let output = std::process::Command::new("sh")
                                                .arg("-c")
                                                .arg(&cmd)
                                                .output();
                                            let body = match output {
                                                Ok(o) => {
                                                    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                                                    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                                                    let mut s = format!("$ {cmd}\n");
                                                    if !stdout.is_empty() {
                                                        s.push_str(&stdout);
                                                    }
                                                    if !stderr.is_empty() {
                                                        s.push_str(&stderr);
                                                    }
                                                    s
                                                }
                                                Err(e) => format!("$ {cmd}\n[error: {e}]"),
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: body,
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        // Esc Esc — edit previous user message.
                                        SlashAction::EditPreviousMessage => {
                                            // Find the last user message, remove it, put content back in composer.
                                            if let Some(pos) = app.messages.iter().rposition(|m| m.role == "user") {
                                                let msg = app.messages.remove(pos);
                                                // Also remove its timestamp if present.
                                                if pos < app.message_timestamps.len() {
                                                    app.message_timestamps.remove(pos);
                                                }
                                                app.composer.input_buffer = msg.content;
                                                app.composer.cursor_pos = app.composer.input_buffer.chars().count();
                                                app.scroll_chat_to_bottom();
                                            }
                                        }
                                        // /init — generate AGENTS.md scaffold.
                                        SlashAction::InitProject => {
                                            let path = std::path::Path::new("AGENTS.md");
                                            if path.exists() {
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "system".to_string(),
                                                    content: "AGENTS.md already exists — not overwriting.".to_string(),
                                                    tool_call_id: None,
                                                    tool_calls: None,
                                                    attachments: None,
                                                    thinking_signature: None,
                                                    redacted_thinking: None,
                                                    thinking: None,
                                                });
                                            } else {
                                                let project_name = std::path::Path::new(".")
                                                    .canonicalize()
                                                    .ok()
                                                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                                                    .unwrap_or_else(|| "my-project".to_string());
                                                let scaffold = format!(
                                                    "# AGENTS.md\n\n\
## Project: {project_name}\n\n\
## Build\n\
- `cargo build`\n\
- `cargo test`\n\
- `cargo clippy --workspace --all-targets -- -D warnings`\n\n\
## Conventions\n\
- Rust idiomatic, clippy clean\n\
- Tests via `cargo test`\n\
- Follow existing patterns in the codebase\n"
                                                );
                                                match std::fs::write(path, &scaffold) {
                                                    Ok(_) => {
                                                        app.messages.push(zerozero_llm::ChatMessage {
                                                            role: "system".to_string(),
                                                            content: "AGENTS.md created with project scaffold.".to_string(),
                                                            tool_call_id: None,
                                                            tool_calls: None,
                                                            attachments: None,
                                                            thinking_signature: None,
                                                            redacted_thinking: None,
                                                            thinking: None,
                                                        });
                                                    }
                                                    Err(e) => {
                                                        app.messages.push(zerozero_llm::ChatMessage {
                                                            role: "system".to_string(),
                                                            content: format!("Failed to create AGENTS.md: {e}"),
                                                            tool_call_id: None,
                                                            tool_calls: None,
                                                            attachments: None,
                                                            thinking_signature: None,
                                                            redacted_thinking: None,
                                                            thinking: None,
                                                        });
                                                    }
                                                }
                                            }
                                            app.record_message_timestamp();
                                        }
                                        // /review — show pending git changes.
                                        SlashAction::ReviewChanges => {
                                            let stat = std::process::Command::new("git")
                                                .args(["diff", "--stat"])
                                                .output();
                                            let diff = std::process::Command::new("git")
                                                .args(["diff"])
                                                .output();
                                            let body = match (&stat, &diff) {
                                                (Ok(s), Ok(d)) => {
                                                    let stat_out = String::from_utf8_lossy(&s.stdout).to_string();
                                                    let diff_out = String::from_utf8_lossy(&d.stdout).to_string();
                                                    if stat_out.is_empty() && diff_out.is_empty() {
                                                        "No pending changes (clean working tree).".to_string()
                                                    } else {
                                                        format!("Pending changes:\n{stat_out}\n\n{diff_out}")
                                                    }
                                                }
                                                _ => "Failed to run git diff — not a git repo?".to_string(),
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: body,
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        // /keymap — display current keybindings.
                                        SlashAction::ShowKeymap => {
                                            let keymap = "\
Keybindings (Codex parity):\n\
  Enter          Send prompt / steer mid-turn\n\
  Tab            Queue follow-up while running\n\
  Ctrl+G         Open $EDITOR for long prompt\n\
  Ctrl+J         Insert newline (reliable)\n\
  Ctrl+L         Clear screen\n\
  Ctrl+T         Open transcript overlay\n\
  Ctrl+.         Show shortcuts overlay\n\
  Ctrl+M         Toggle multiline mode\n\
  Esc            Interrupt running turn\n\
  Esc Esc        Edit previous message\n\
  Shift+Tab     approval mode\n\
  Up/Down        Draft history navigation\n\
  PgUp/PgDn      Half-page scroll\n\
  Ctrl+Up/Dn     Line scroll (3 lines)\n\
  Ctrl+Home/End  Jump to top/bottom\n\
  @query         Fuzzy file search\n\
  !cmd           Local shell command\n\
  /              Slash command popup\n\
  t              Toggle tool output collapse\n\
  ?              Show help\n";
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: keymap.to_string(),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        // /sandbox — show sandbox mode.
                                        SlashAction::ShowSandbox => {
                                            let mode = if app.always_approve {
                                                "disabled (always-approve mode)"
                                            } else {
                                                "enabled (on-request approval)"
                                            };
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "system".to_string(),
                                                content: format!("Sandbox: {mode}\nUse /always-approve to toggle."),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            app.record_message_timestamp();
                                        }
                                        SlashAction::None => {}
                                    },
                                    KeyAction::SwitchAgentPrev => {
                                        // fix: Alt+Left —
                                        // switch to previous agent thread.
                                        // Refresh live_agents first to get
                                        // the current snapshot, then navigate
                                        // with wraparound via agent_nav.
                                        app.set_live_agents(thread_registry.live_agents().await);
                                        if let Some(tid) = crate::agent_nav::prev_agent(
                                            &app.live_agents,
                                            &app.active_thread_id,
                                        ) {
                                            let _ = thread_registry.switch_thread(&tid);
                                            app.set_active_thread_id(tid);
                                        }
                                    }
                                    KeyAction::SwitchAgentNext => {
                                        // fix: Alt+Right —
                                        // switch to next agent thread.
                                        app.set_live_agents(thread_registry.live_agents().await);
                                        if let Some(tid) = crate::agent_nav::next_agent(
                                            &app.live_agents,
                                            &app.active_thread_id,
                                        ) {
                                            let _ = thread_registry.switch_thread(&tid);
                                            app.set_active_thread_id(tid);
                                        }
                                    }
                                    KeyAction::SwitchToApprovalSource => {
                                        // Press 'o' — switch to the thread
                                        // that sent the pending approval request.
                                        if let Some(approval) = app.pending_approval.take() {
                                            app.set_active_thread_id(approval.source_thread_id);
                                        }
                                    }
                                    KeyAction::SelectAgent(index) => {
                                        // Enter in picker — switch to selected.
                                        if let Some(tid) =
                                            crate::agent_nav::agent_at(&app.live_agents, index)
                                        {
                                            app.set_active_thread_id(tid);
                                        }
                                    }
                                    KeyAction::CancelStreaming => {
                                        // Esc while streaming — mark as
                                        // interrupted. The background task will
                                        // finish but its output is discarded.
                                        app.messages.push(zerozero_llm::ChatMessage {
                                            role: "system".to_string(),
                                            content: "[interrupted]".to_string(),
                                            tool_call_id: None,
                                            tool_calls: None,
                                            attachments: None,
                                            thinking_signature: None,
                                            redacted_thinking: None,
                                            thinking: None,
                                        });
                                        app.persist_active_chat();
                                    }
                                    KeyAction::QueueInput => {
                                        // Tab while streaming — input is
                                        // already stored in app.composer.queued_input by
                                        // handle_key. Nothing more to do here.
                                    }
                                    KeyAction::ClearScreen => {
                                        // Ctrl+L — clear terminal screen.
                                        let _ = terminal.clear();
                                        app.needs_clear = false;
                                    }
                                    KeyAction::CopyOutput => {
                                        // Ctrl+O or /copy — copy latest
                                        // assistant output to system clipboard.
                                        let msg = match app.latest_assistant_output() {
                                            Some(text) => {
                                                match arboard::Clipboard::new() {
                                                    Ok(mut cb) => {
                                                        match cb.set_text(text.clone()) {
                                                            Ok(()) => "Copied to clipboard.".to_string(),
                                                            Err(e) => format!("Clipboard error: {e}"),
                                                        }
                                                    }
                                                    Err(e) => format!("Clipboard unavailable: {e}"),
                                                }
                                            }
                                            None => "No assistant output to copy.".to_string(),
                                        };
                                        app.messages.push(zerozero_llm::ChatMessage {
                                            role: "system".to_string(),
                                            content: msg,
                                            tool_call_id: None,
                                            tool_calls: None,
                                            attachments: None,
                                            thinking_signature: None,
                                            redacted_thinking: None,
                                            thinking: None,
                                        });
                                    }
                                    KeyAction::OpenEditor => {
                                        // Ctrl+E — open $EDITOR with the
                                        // current input buffer. Read back on exit.
                                        let editor = std::env::var("EDITOR")
                                            .unwrap_or_else(|_| "vi".to_string());
                                        let tmp = tempfile::NamedTempFile::new()
                                            .map(|f| f.into_temp_path());
                                        match tmp {
                                            Ok(path) => {
                                                if !app.composer.input_buffer.is_empty() {
                                                    let _ = std::fs::write(&path, &app.composer.input_buffer);
                                                }
                                                let result = std::process::Command::new(&editor)
                                                    .arg(&path)
                                                    .status();
                                                if let Ok(status) = result {
                                                    if status.success() {
                                                        match std::fs::read_to_string(&path) {
                                                            Ok(content) => {
                                                                app.composer.input_buffer = content.trim_end().to_string();
                                                                app.sync_cursor_to_end();
                                                            }
                                                            Err(e) => {
                                                                app.messages.push(zerozero_llm::ChatMessage {
                                                                    role: "system".to_string(),
                                                                    content: format!("Editor read error: {e}"),
                                                                    tool_call_id: None,
                                                                    tool_calls: None,
                                                                    attachments: None,
                                                                    thinking_signature: None,
                                                                    redacted_thinking: None,
                                                                    thinking: None,
                                                                });
                                                                app.record_message_timestamp();
                                                            }
                                                        }
                                                    }
                                                }
                                                let _ = std::fs::remove_file(&path);
                                            }
                                            Err(e) => {
                                                app.messages.push(zerozero_llm::ChatMessage {
                                                    role: "system".to_string(),
                                                    content: format!("Cannot create temp file: {e}"),
                                                    tool_call_id: None,
                                                    tool_calls: None,
                                                    attachments: None,
                                                    thinking_signature: None,
                                                    redacted_thinking: None,
                                                    thinking: None,
                                                });
                                                app.record_message_timestamp();
                                            }
                                        }
                                    }
                                    KeyAction::CycleMode => {
                                        // Shift+Tab — mode already cycled
                                        // in handle_key. Show confirmation message.
                                        app.messages.push(zerozero_llm::ChatMessage {
                                            role: "system".to_string(),
                                            content: format!(
                                                "Mode: {}{}",
                                                app.session_mode.label(),
                                                match app.session_mode {
                                                    app::SessionMode::Plan => " — plan before editing",
                                                    app::SessionMode::AlwaysApprove => " — auto-approve all tools",
                                                    app::SessionMode::Normal => "",
                                                }
                                            ),
                                            tool_call_id: None,
                                            tool_calls: None,
                                            attachments: None,
                                            thinking_signature: None,
                                            redacted_thinking: None,
                                            thinking: None,
                                        });
                                        app.record_message_timestamp();
                                    }
                                    KeyAction::ToggleMultiline => {
                                        // Ctrl+M — multiline already toggled
                                        // in handle_key. Show confirmation.
                                        app.messages.push(zerozero_llm::ChatMessage {
                                            role: "system".to_string(),
                                            content: format!(
                                                "Multiline: {}",
                                                if app.multiline { "ON (Enter=newline, Ctrl+Enter=submit)" } else { "OFF (Enter=submit)" }
                                            ),
                                            tool_call_id: None,
                                            tool_calls: None,
                                            attachments: None,
                                            thinking_signature: None,
                                            redacted_thinking: None,
                                            thinking: None,
                                        });
                                        app.record_message_timestamp();
                                    }
                                    KeyAction::ShowShortcuts => {
                                        // Ctrl+. — overlay already shown in handle_key.
                                    }
                                    KeyAction::None => {}
                                }
                            }
                            // Bracketed paste — insert pasted text at cursor.
                            AppEvent::Crossterm(CrosstermEvent::Paste(text)) => {
                                for ch in text.chars() {
                                    app.composer.input_buffer.insert(app.composer.cursor_pos, ch);
                                    app.composer.cursor_pos += 1;
                                }
                                // Auto-enable multiline if paste contains newlines.
                                if text.contains('\n') {
                                    app.multiline = true;
                                }
                                app.update_slash_palette();
                            }
                            AppEvent::Core(event) => {
                                let tid = app.active_thread_id.clone();
                                let was_streaming = app.is_streaming;
                                app.apply_chat_event(&tid, event);
                                // Auto-submit queued input when streaming ends.
                                if was_streaming && !app.is_streaming {
                                    if let Some(queued) = app.composer.queued_input.take() {
                                        if !queued.is_empty() {
                                            app.composer.input_buffer = queued;
                                            app.sync_cursor_to_end();
                                            // Re-dispatch as a submit.
                                            let prompt = app.composer.input_buffer.clone();
                                            app.composer.input_buffer.clear();
                                            app.composer.cursor_pos = 0;
                                            app.is_streaming = true;
                                        app.status_indicator.start();
                                        app.footer_mode = app::FooterMode::Running;
                                            app.record_prompt(&prompt);
                                            let prior_messages = app.messages.clone();
                                            app.messages.push(zerozero_llm::ChatMessage {
                                                role: "user".to_string(),
                                                content: prompt.clone(),
                                                tool_call_id: None,
                                                tool_calls: None,
                                                attachments: None,
                                                thinking_signature: None,
                                                redacted_thinking: None,
                                                thinking: None,
                                            });
                                            let core_tx = tx.clone();
                                            let provider = std::sync::Arc::clone(&provider);
                                            let tools = Arc::clone(&tools);
                                            let sandbox = sandbox.clone();
                                            let compaction_config = compaction_config.clone();
                                            let system_prompt = system_prompt.clone();
                                            let effort = app.effort;
                                            let ask_mode = app.ask_mode;
                                            app.persist_active_chat();
                                            tokio::task::spawn_local(async move {
                                                let emit = move |event: Event| {
                                                    let _ = core_tx.send(AppEvent::Core(event));
                                                };
                                                let _ = zerozero_core::run_turn(
                                                    &prompt,
                                                    system_prompt.as_deref(),
                                                    &*provider,
                                                    &tools,
                                                    10,
                                                    &sandbox,
                                                    &approval,
                                                    false,
                                                    ask_mode,
                                                    None,
                                                    &compaction_config,
                                                    &zerozero_core::NoopHooks,
                                                    None,
                                                    &prior_messages,
                                                    effort,
                                                    &[],
                                                    None,
                                                    emit,
                                                )
                                                .await;
                                            });
                                        }
                                    }
                                }
                            }
                            AppEvent::AgentThread(tid, event) => {
                                app.apply_chat_event(&tid, event);
                            }
                            AppEvent::AgentStatusChanged(tid, status) => {
                                // Update thread status in registry.
                                thread_registry.update_status(&tid, status).await;
                                // Refresh live agents cache.
                                app.set_live_agents(thread_registry.live_agents().await);
                            }
                            AppEvent::AgentApprovalRequest {
                                source_thread_id,
                                tool_call_id,
                                tool_name,
                                args,
                                danger_level,
                            } => {
                                // Show approval overlay for inactive thread.
                                app.pending_approval = Some(app::PendingApproval {
                                    source_thread_id,
                                    tool_call_id,
                                    tool_name,
                                    args,
                                    danger_level,
                                });
                            }
                            AppEvent::Tick if app.is_streaming => {
                                // Advance spinner animation during streaming.
                                app.tick_spinner();
                            }
                            _ => {}
                        }

                        if app.should_quit {
                            break;
                        }
                    }
                    None => break,
                }
                // Redraw only after a real state change (key, stream delta, spinner tick…).
                terminal.draw(|f| ui::render(f, &app))?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::slash::BUILTIN_SPECS;

    #[test]
    fn test_help_overlay_model_command() {
        // Palette overhaul: /model spec must describe mid-session switching.
        let model = BUILTIN_SPECS.iter().find(|s| s.invoke == "model").unwrap();
        assert!(
            model.description.contains("switch"),
            "/model description should mention switching: {}",
            model.description
        );
        assert!(
            model.usage.contains("/model"),
            "/model usage should contain the signature: {}",
            model.usage
        );
        assert!(
            !model.description.contains("display only"),
            "/model should not say 'display only' anymore"
        );
    }

    #[test]
    fn test_help_overlay_effort_has_args_hint() {
        let effort = BUILTIN_SPECS.iter().find(|s| s.invoke == "effort").unwrap();
        assert_eq!(effort.args_hint, &["none", "low", "medium", "high"]);
    }
}
