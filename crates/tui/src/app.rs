use std::collections::HashMap;

use crate::composer::ComposerState;
use crate::slash::{self, SlashCommand};
#[cfg(test)]
use crossterm::event::KeyEventKind;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use zerozero_exec::{Event, ItemUpdateKind};
use zerozero_llm::{ChatMessage, Effort};
use zerozero_multi_agent::ThreadId;

/// Per-thread chat + streaming state (background agents keep streaming).
#[derive(Clone, Debug, Default)]
pub struct ThreadChatState {
    pub messages: Vec<ChatMessage>,
    pub streaming_text: String,
    pub streaming_reasoning_text: String,
    pub is_streaming: bool,
    pub session_id: String,
    /// Tool call events for the current turn (cleared on ItemStarted).
    /// Rendered as styled cards in the chat view.
    pub tool_events: Vec<ToolEventDisplay>,
}

/// One tool call event for display in the chat view.
#[derive(Clone, Debug)]
pub struct ToolEventDisplay {
    pub name: String,
    pub status: ToolStatus,
    /// Optional result preview (for completed tool calls).
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Done,
    Error,
}

impl ThreadChatState {
    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::SessionStarted { session_id } => {
                self.session_id = session_id;
            }
            Event::ItemStarted { .. } => {
                self.streaming_text.clear();
                self.streaming_reasoning_text.clear();
                self.tool_events.clear();
                self.is_streaming = true;
            }
            Event::ItemUpdated { item } => {
                self.is_streaming = true;
                match item.kind {
                    ItemUpdateKind::Reasoning => {
                        self.streaming_reasoning_text.push_str(&item.text);
                    }
                    ItemUpdateKind::Message => {
                        self.streaming_text.push_str(&item.text);
                    }
                }
            }
            Event::ItemCompleted { item } => {
                // Preserve reasoning: push a "thinking" message before the
                // assistant message so the reasoning trail survives after
                // the streaming buffers are cleared.
                if !self.streaming_reasoning_text.is_empty() {
                    self.messages.push(ChatMessage {
                        role: "thinking".to_string(),
                        content: std::mem::take(&mut self.streaming_reasoning_text),
                        tool_call_id: None,
                        tool_calls: None,
                        attachments: None,
                        thinking_signature: None,
                        redacted_thinking: None,
                        thinking: None,
                    });
                }
                self.messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: item.text,
                    tool_call_id: None,
                    tool_calls: None,
                    attachments: None,
                    thinking_signature: None,
                    redacted_thinking: None,
                    thinking: None,
                });
                self.streaming_text.clear();
                self.streaming_reasoning_text.clear();
                self.tool_events.clear();
            }
            Event::ToolStarted { tool_name, .. } => {
                // Structured tool call tracking for card-style display.
                self.tool_events.push(ToolEventDisplay {
                    name: tool_name,
                    status: ToolStatus::Running,
                    preview: None,
                });
            }
            Event::ToolCompleted {
                tool_name, result, ..
            } => {
                let preview = if result.len() > 200 {
                    format!("{}...", &result[..200])
                } else {
                    result
                };
                // Update the matching running event to Done, or push a new one.
                if let Some(ev) = self
                    .tool_events
                    .iter_mut()
                    .rev()
                    .find(|e| e.name == tool_name && e.status == ToolStatus::Running)
                {
                    ev.status = ToolStatus::Done;
                    ev.preview = Some(preview);
                } else {
                    self.tool_events.push(ToolEventDisplay {
                        name: tool_name,
                        status: ToolStatus::Done,
                        preview: Some(preview),
                    });
                }
            }
            Event::TurnCompleted => {
                self.is_streaming = false;
            }
            Event::Error { message } => {
                // Mark any running tool as errored, and surface the error.
                for ev in self.tool_events.iter_mut() {
                    if ev.status == ToolStatus::Running {
                        ev.status = ToolStatus::Error;
                        ev.preview = Some(message.clone());
                    }
                }
                self.streaming_text
                    .push_str(&format!("\n[Error: {message}]\n"));
                self.is_streaming = false;
            }
            _ => {}
        }
    }
}

/// Footer hint mode (Codex CLI parity).
/// Determines which contextual shortcuts are shown below the composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FooterMode {
    #[default]
    Idle,
    Running,
    Queued,
}

impl FooterMode {
    pub fn hint_text(&self) -> &'static str {
        match self {
            Self::Idle => " Enter to send · / for commands · ? for help · Ctrl+. shortcuts ",
            Self::Running => " Esc to interrupt · Tab to queue follow-up ",
            Self::Queued => " Queued — will send when turn completes ",
        }
    }
}

/// Session mode (Grok CLI parity).
/// Normal = default, Plan = plan-first, AlwaysApprove = skip approval prompts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    Normal,
    Plan,
    AlwaysApprove,
}

impl SessionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Plan => "plan",
            Self::AlwaysApprove => "approve",
        }
    }

    ///to the next mode (Shift+Tab).
    pub fn next(self) -> Self {
        match self {
            Self::Normal => Self::Plan,
            Self::Plan => Self::AlwaysApprove,
            Self::AlwaysApprove => Self::Normal,
        }
    }
}

/// TUI application state.
pub struct App {
    /// Conversation history (user + assistant + tool messages).
    pub messages: Vec<ChatMessage>,
    /// Current streaming assistant text (accumulated deltas).
    pub streaming_text: String,
    /// Streaming reasoning / thinking tokens (separate style in UI).
    pub streaming_reasoning_text: String,
    /// Tool call events for the current turn (card-style display).
    pub tool_events: Vec<ToolEventDisplay>,
    /// Interactive prompt composer (buffer, cursor, slash, queue, images).
    pub composer: ComposerState,
    /// Whether LLM is currently streaming.
    pub is_streaming: bool,
    /// Session ID for display.
    pub session_id: String,
    /// Whether to quit.
    pub should_quit: bool,
    /// Current diff view (if any) for write_file/edit_file results.
    pub diff_view: Option<crate::diff::DiffView>,
    /// Whether the diff view panel is shown.
    pub show_diff: bool,
    /// Loaded skill names (for /skills command).
    pub skill_names: Vec<String>,
    /// User-invocable skills for `/name` completion (refreshed from disk).
    pub skill_slash_entries: Vec<zerozero_skills::SkillSlashEntry>,
    /// Loaded plugin names (for /plugins command).
    pub plugin_names: Vec<String>,
    /// Skill directories for hot-reload.
    pub skill_dirs: Vec<std::path::PathBuf>,
    /// Plugin directories for hot-reload.
    pub plugin_dirs: Vec<std::path::PathBuf>,
    /// Session database path for /sessions command.
    pub session_db_path: Option<std::path::PathBuf>,
    // ---  multi-agent fields ---
    /// Active thread ID (the thread whose events are rendered).
    pub active_thread_id: ThreadId,
    /// Whether the agent picker popup is shown.
    pub show_agent_picker: bool,
    /// Selected index in the agent picker list.
    pub agent_picker_selected: usize,
    /// Cached list of live agents (for picker display).
    pub live_agents: Vec<zerozero_multi_agent::AgentMetadata>,
    /// Whether the agent tree view is shown.
    pub show_agent_tree: bool,
    /// Pending approval request from an inactive thread (if any).
    pub pending_approval: Option<PendingApproval>,
    /// Reasoning effort level). Default Medium.
    /// Updated via `/effort <level>` slash command.
    pub effort: Effort,
    /// Current model name). Updated via `/model` slash command.
    pub model: String,
    /// Current provider id (3-tier picker). Mirrors `ZZ_PROVIDER` at startup;
    /// updated when the user switches provider via the picker.
    pub provider: String,
    /// Whether the 3-tier model picker overlay is shown (`/model` no-arg).
    pub show_model_picker: bool,
    /// Active tier in the model picker (0=provider, 1=model, 2=effort).
    pub model_picker_tier: u8,
    /// Selected index within the current tier of the model picker.
    pub model_picker_index: usize,
    /// Picker-local selections: chosen provider id (tier 0), chosen model id
    /// (tier 1), chosen effort (tier 2). Cleared on open/apply/cancel.
    pub picker_provider: String,
    pub picker_model: String,
    pub picker_effort: Effort,
    /// Ask mode). When true, every tool call requires
    /// user confirmation (parity `--ask` / `/ask`). Toggled via `/ask`.
    pub ask_mode: bool,
    /// Full-screen skills browser (`/skills`).
    pub show_skills_browser: bool,
    pub skills_browser_index: usize,
    /// Chat/streaming state per agent thread (includes root).
    thread_chats: HashMap<ThreadId, ThreadChatState>,
    // --- TUI enhancement batch  ---
    /// Prompt history for Ctrl+R search (all submitted prompts).
    pub prompt_history: Vec<String>,
    /// Whether Ctrl+R history search mode is active.
    pub show_history_search: bool,
    /// Current query in history search mode.
    pub history_search_query: String,
    /// Selected index in history search results.
    pub history_search_index: usize,
    /// Whether the theme picker overlay is shown (`/theme`).
    pub show_theme_picker: bool,
    /// Selected index in the theme picker.
    pub theme_picker_index: usize,
    /// Spinner tick counter for streaming animation.
    pub spinner_tick: u32,
    /// Whether the screen needs clearing (Ctrl+L).
    pub needs_clear: bool,
    /// Whether the `/help` overlay is shown (palette overhaul).
    pub show_help_overlay: bool,
    /// Vertical scroll offset of the `/help` overlay.
    pub help_scroll: usize,
    /// Chat viewport scroll: lines **up from the bottom** (0 = follow live
    /// stream / stick to latest message — Codex/Claude parity).
    pub chat_scroll: u16,
    /// `/connect` overlay visible (OpenCode-style API key entry).
    pub show_connect: bool,
    /// Connect flow stage: 0 = pick provider, 1 = enter API key.
    pub connect_stage: u8,
    /// Selected index in the provider list.
    pub connect_index: usize,
    /// Provider id chosen for key entry.
    pub connect_provider: String,
    /// Buffered API key while typing (never written to chat history).
    pub connect_key_buffer: String,
    // --- Grok CLI TUI parity ---
    /// Session mode (Normal/Plan/AlwaysApprove). Shift+Tab cycles.
    pub session_mode: SessionMode,
    /// Plan file content (populated by /plan, shown by /view-plan).
    pub plan_text: String,
    /// Always-approve mode (skip all tool approval prompts).
    pub always_approve: bool,
    /// Multiline input mode (Enter = newline, Ctrl+Enter = submit).
    pub multiline: bool,
    /// Compact UI mode (reduced padding/spacing).
    pub compact_mode: bool,
    /// Show timestamps on chat messages.
    pub show_timestamps: bool,
    /// Vim-style scrollback keybindings.
    pub vim_mode: bool,
    /// Keyboard shortcuts overlay visible.
    pub show_shortcuts_overlay: bool,
    /// Timestamps for each message (parallel to `messages` vec).
    /// Used when `show_timestamps` is on to render `[HH:MM:SS]` prefixes.
    pub message_timestamps: Vec<std::time::Instant>,
    // --- Codex TUI parity — status & footer ---
    /// Status indicator (elapsed timer for streaming turns).
    pub status_indicator: crate::status_indicator::StatusIndicator,
    /// Footer hint mode (Idle/Running/Queued).
    pub footer_mode: FooterMode,
    // --- Codex TUI parity — input & interaction ---
    /// `@` file search mode — when true, typing searches for files.
    pub show_file_search: bool,
    /// File search query (after `@`).
    pub file_search_query: String,
    /// File search results (fuzzy-matched file paths).
    pub file_search_results: Vec<String>,
    /// Selected index in file search results.
    pub file_search_index: usize,
    /// Last Esc press time (for Esc Esc backtrack detection).
    pub last_esc_press: Option<std::time::Instant>,
    // --- Codex TUI parity — rendering polish ---
    /// Collapsed tool event indices (for collapsible tool output).
    pub collapsed_tools: std::collections::HashSet<usize>,
}

/// Pending approval request from an inactive thread .
#[derive(Clone, Debug)]
pub struct PendingApproval {
    pub source_thread_id: ThreadId,
    pub tool_call_id: String,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub danger_level: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            streaming_text: String::new(),
            streaming_reasoning_text: String::new(),
            tool_events: Vec::new(),
            composer: ComposerState::new(),
            is_streaming: false,
            session_id: String::new(),
            should_quit: false,
            diff_view: None,
            show_diff: false,
            skill_names: Vec::new(),
            skill_slash_entries: Vec::new(),
            plugin_names: Vec::new(),
            skill_dirs: Vec::new(),
            plugin_dirs: Vec::new(),
            session_db_path: None,
            active_thread_id: String::new(),
            show_agent_picker: false,
            agent_picker_selected: 0,
            live_agents: Vec::new(),
            show_agent_tree: false,
            pending_approval: None,
            effort: Effort::Medium,
            model: String::new(),
            provider: String::new(),
            show_model_picker: false,
            model_picker_tier: 0,
            model_picker_index: 0,
            picker_provider: String::new(),
            picker_model: String::new(),
            picker_effort: Effort::None,
            ask_mode: false,
            show_skills_browser: false,
            skills_browser_index: 0,
            thread_chats: HashMap::new(),
            prompt_history: Vec::new(),
            show_history_search: false,
            history_search_query: String::new(),
            history_search_index: 0,
            show_theme_picker: false,
            theme_picker_index: 0,
            spinner_tick: 0,
            needs_clear: false,
            show_help_overlay: false,
            help_scroll: 0,
            chat_scroll: 0,
            show_connect: false,
            connect_stage: 0,
            connect_index: 0,
            connect_provider: String::new(),
            connect_key_buffer: String::new(),
            session_mode: SessionMode::Normal,
            plan_text: String::new(),
            always_approve: false,
            multiline: false,
            compact_mode: false,
            show_timestamps: false,
            vim_mode: false,
            show_shortcuts_overlay: false,
            message_timestamps: Vec::new(),
            status_indicator: crate::status_indicator::StatusIndicator::new(),
            footer_mode: FooterMode::Idle,
            show_file_search: false,
            file_search_query: String::new(),
            file_search_results: Vec::new(),
            file_search_index: 0,
            last_esc_press: None,
            collapsed_tools: std::collections::HashSet::new(),
        }
    }

    /// Fuzzy file search — returns up to 10 file paths matching `query`.
    /// Uses a simple substring match (case-insensitive) over files in the
    /// current working directory (non-recursive, top-level only for speed).
    pub fn fuzzy_find_files(&self, query: &str) -> Vec<String> {
        if query.is_empty() {
            return Vec::new();
        }
        let q = query.to_ascii_lowercase();
        let mut results: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(".") {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.to_ascii_lowercase().contains(&q) {
                        results.push(name.to_string());
                        if results.len() >= 10 {
                            break;
                        }
                    }
                }
            }
        }
        results
    }

    /// Open the `/connect` overlay. Optional `provider` skips the list.
    pub fn open_connect(&mut self, provider: Option<&str>) {
        self.show_connect = true;
        self.connect_key_buffer.clear();
        if let Some(raw) = provider.filter(|s| !s.is_empty()) {
            if let Some(spec) = zerozero_llm::find_provider(raw) {
                self.connect_provider = spec.id.to_string();
                self.connect_stage = if spec.requires_key { 1 } else { 0 };
                // Pre-select matching index for stage 0 return.
                if let Some(i) = zerozero_llm::PROVIDERS.iter().position(|p| p.id == spec.id) {
                    self.connect_index = i;
                }
                if !spec.requires_key {
                    // Local provider: stay on list with a system-style message later.
                    self.connect_stage = 0;
                }
                return;
            }
        }
        self.connect_stage = 0;
        self.connect_provider.clear();
        // Pre-select current app provider if known.
        if let Some(i) = zerozero_llm::PROVIDERS
            .iter()
            .position(|p| p.id == self.provider || (self.provider.is_empty() && p.id == "xai"))
        {
            self.connect_index = i;
        } else {
            self.connect_index = 0;
        }
    }

    /// Close connect overlay and wipe the key buffer.
    pub fn close_connect(&mut self) {
        self.show_connect = false;
        self.connect_stage = 0;
        self.connect_key_buffer.clear();
        self.connect_provider.clear();
    }

    /// Record a timestamp for the latest message.
    /// Call after pushing to `self.messages`.
    pub fn record_message_timestamp(&mut self) {
        self.message_timestamps.push(std::time::Instant::now());
        // Keep parallel with messages vec — trim if somehow longer.
        if self.message_timestamps.len() > self.messages.len() {
            self.message_timestamps.truncate(self.messages.len());
        }
    }

    /// Format a timestamp for display as `[HH:MM:SS]`.
    /// Returns empty string if no timestamp recorded for the given index.
    pub fn format_timestamp(&self, index: usize) -> String {
        if !self.show_timestamps || index >= self.message_timestamps.len() {
            return String::new();
        }
        let elapsed = self.message_timestamps[index].elapsed();
        // Use elapsed seconds to build a relative timestamp.
        // For a real clock time, we'd need SystemTime, but Instant is
        // sufficient for display and doesn't require system clock access.
        let secs = elapsed.as_secs();
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        format!("[{h:02}:{m:02}:{s:02}] ")
    }

    /// Export conversation to a string.
    pub fn export_conversation(&self) -> String {
        let mut out = String::new();
        out.push_str("ZeroZero conversation export\n");
        out.push_str(&format!("Session: {}\n", self.session_id));
        out.push_str(&format!("Messages: {}\n", self.messages.len()));
        out.push_str("---\n\n");
        for msg in &self.messages {
            let role = match msg.role.as_str() {
                "assistant" | "agent" => "Assistant",
                "thinking" => "Thinking",
                "system" => "System",
                _ => "You",
            };
            out.push_str(&format!("{role}: {}\n\n", msg.content));
        }
        out
    }

    /// Stick the chat viewport to the latest content.
    pub fn scroll_chat_to_bottom(&mut self) {
        self.chat_scroll = 0;
    }

    /// Scroll the chat view up (older content). `lines` is typically half a page.
    pub fn scroll_chat_up(&mut self, lines: u16) {
        self.chat_scroll = self.chat_scroll.saturating_add(lines);
    }

    /// Scroll the chat view down (newer content). Reaches 0 = live follow.
    pub fn scroll_chat_down(&mut self, lines: u16) {
        self.chat_scroll = self.chat_scroll.saturating_sub(lines);
    }

    pub fn skill_prompt_blocks(&self, names: &[String], args: &str) -> Option<String> {
        let registry = self.skill_registry();
        Some(slash::format_skill_chain_blocks(&registry, names, args))
    }

    /// Persist the visible chat fields into `thread_chats` for the active thread.
    pub fn persist_active_chat(&mut self) {
        let tid = self.active_thread_id.clone();
        if tid.is_empty() {
            return;
        }
        self.save_view_to_thread(&tid);
    }

    fn save_view_to_thread(&mut self, tid: &ThreadId) {
        self.thread_chats.insert(
            tid.clone(),
            ThreadChatState {
                messages: self.messages.clone(),
                streaming_text: self.streaming_text.clone(),
                streaming_reasoning_text: self.streaming_reasoning_text.clone(),
                is_streaming: self.is_streaming,
                session_id: self.session_id.clone(),
                tool_events: self.tool_events.clone(),
            },
        );
    }

    fn load_view_from_thread(&mut self, tid: &ThreadId) {
        let st = self.thread_chats.get(tid).cloned().unwrap_or_default();
        self.messages = st.messages;
        self.streaming_text = st.streaming_text;
        self.streaming_reasoning_text = st.streaming_reasoning_text;
        self.is_streaming = st.is_streaming;
        self.session_id = st.session_id;
        self.tool_events = st.tool_events;
        // Sync status indicator + footer mode with streaming state.
        if st.is_streaming {
            if !self.status_indicator.is_streaming {
                self.status_indicator.start();
            }
            self.footer_mode = FooterMode::Running;
        } else {
            self.status_indicator.stop();
            self.footer_mode = FooterMode::Idle;
        }
    }

    /// Switch displayed thread; preserves streaming buffers per thread.
    pub fn set_active_thread_id(&mut self, thread_id: ThreadId) {
        let prev = self.active_thread_id.clone();
        if !prev.is_empty() {
            self.save_view_to_thread(&prev);
        }
        self.active_thread_id = thread_id.clone();
        self.load_view_from_thread(&thread_id);
    }

    /// Apply a core engine event to a thread (active or background).
    pub fn apply_chat_event(&mut self, tid: &ThreadId, event: Event) {
        let show_diff = matches!(
            &event,
            Event::ToolCompleted {
                tool_name,
                ..
            } if tool_name == "write_file" || tool_name == "edit_file"
        );
        let diff_payload = if show_diff {
            if let Event::ToolCompleted {
                tool_name, result, ..
            } = &event
            {
                Some((tool_name.clone(), result.clone()))
            } else {
                None
            }
        } else {
            None
        };

        let entry = self.thread_chats.entry(tid.clone()).or_default();
        entry.apply_event(event);
        if tid == &self.active_thread_id {
            self.load_view_from_thread(tid);
            if let Some((tool_name, result)) = diff_payload {
                self.diff_view = Some(crate::diff::DiffView::new(&tool_name, "", &result));
                self.show_diff = true;
            }
        }
    }

    /// Update the cached live agents list (called from lib.rs event loop).
    pub fn set_live_agents(&mut self, agents: Vec<zerozero_multi_agent::AgentMetadata>) {
        self.live_agents = agents;
    }

    /// Set the loaded skill names (called from lib.rs after loading skills).
    pub fn set_skills(&mut self, names: Vec<String>) {
        self.skill_names = names;
    }

    /// Set the loaded plugin names (called from lib.rs after loading plugins).
    pub fn set_plugins(&mut self, names: Vec<String>) {
        self.plugin_names = names;
    }

    /// Set skill directories for hot-reload.
    pub fn set_skill_dirs(&mut self, dirs: Vec<std::path::PathBuf>) {
        self.skill_dirs = dirs;
        self.refresh_skill_cache();
    }

    /// Reload skill name + slash lists from `skill_dirs`.
    pub fn refresh_skill_cache(&mut self) {
        let mut registry = zerozero_skills::SkillRegistry::new();
        let _ = registry.load_from_dirs(&self.skill_dirs);
        self.skill_names = registry.list();
        self.skill_slash_entries = registry.slash_entries();
    }

    fn skill_registry(&self) -> zerozero_skills::SkillRegistry {
        let mut registry = zerozero_skills::SkillRegistry::new();
        let _ = registry.load_from_dirs(&self.skill_dirs);
        registry
    }

    /// Set plugin directories for hot-reload.
    pub fn set_plugin_dirs(&mut self, dirs: Vec<std::path::PathBuf>) {
        self.plugin_dirs = dirs;
    }

    /// Reload plugins from disk. Returns the new count.
    pub fn reload_plugins(&mut self) -> anyhow::Result<usize> {
        let mut plugins = Vec::new();
        for dir in &self.plugin_dirs {
            plugins.extend(zerozero_plugins::discover_plugins_dir(Some(dir)));
        }
        let count = plugins.len();
        self.plugin_names = plugins.into_iter().map(|p| p.name).collect();
        Ok(count)
    }

    /// Set session database path.
    pub fn set_session_db_path(&mut self, path: std::path::PathBuf) {
        self.session_db_path = Some(path);
    }

    /// Format sessions list for display.
    pub fn sessions_list_text(&self) -> String {
        let Some(path) = &self.session_db_path else {
            return "Sessions: (no session database configured)".to_string();
        };
        let store = match zerozero_session::SessionStore::open(path) {
            Ok(s) => s,
            Err(e) => return format!("Sessions: (failed to open: {e})"),
        };
        let sessions = match store.list_sessions() {
            Ok(s) => s,
            Err(e) => return format!("Sessions: (failed to list: {e})"),
        };
        if sessions.is_empty() {
            return "No sessions found.".to_string();
        }
        let mut text = format!("Sessions ({}):\n", sessions.len());
        for s in &sessions {
            let model = s.model.as_deref().unwrap_or("?");
            text.push_str(&format!(
                "  {} | {} | {} msgs | {}\n",
                &s.id[..8.min(s.id.len())],
                &s.created_at[..10.min(s.created_at.len())],
                s.message_count,
                model,
            ));
        }
        text
    }

    /// Compare two sessions and return diff summary.
    pub fn compare_sessions(&self, id_a: &str, id_b: &str) -> anyhow::Result<String> {
        let Some(path) = &self.session_db_path else {
            anyhow::bail!("No session database configured");
        };
        let store = zerozero_session::SessionStore::open(path)?;
        store.compare_sessions(id_a, id_b)
    }

    /// Load one skill's text for slash invocation (`/<name> <task>`).
    pub fn skill_prompt_block(&self, name: &str, args: &str) -> Option<String> {
        let mut registry = zerozero_skills::SkillRegistry::new();
        let _ = registry.load_from_dirs(&self.skill_dirs);
        registry.get(name).map(|s| {
            format!(
                "## Skill: {}\n{}\n\n{}\n\n**ARGUMENTS:** {}",
                s.name, s.description, s.content, args
            )
        })
    }

    /// Reload skills from disk. Returns the new count.
    pub fn reload_skills(&mut self) -> anyhow::Result<usize> {
        let mut registry = zerozero_skills::SkillRegistry::new();
        let count = registry.load_from_dirs(&self.skill_dirs)?;
        self.skill_names = registry.list();
        self.skill_slash_entries = registry.slash_entries();
        Ok(count)
    }

    /// Format skills list for display.
    pub fn skills_list_text(&self) -> String {
        if self.skill_names.is_empty() {
            return "No skills loaded.".to_string();
        }
        let mut text = format!(
            "Loaded skills ({}). Invoke with /name <task> or /project:name /user:name:\n",
            self.skill_names.len()
        );
        for e in &self.skill_slash_entries {
            let hint = if e.argument_hint.is_empty() {
                "<task>".to_string()
            } else {
                e.argument_hint.clone()
            };
            let scope = match e.scope {
                zerozero_skills::SkillScope::Project => "project",
                zerozero_skills::SkillScope::User => "user",
            };
            text.push_str(&format!(
                "  /{} {}  (also /{}:{})\n",
                e.name, hint, scope, e.name
            ));
        }
        for name in &self.skill_names {
            if self
                .skill_slash_entries
                .iter()
                .any(|e| e.name.eq_ignore_ascii_case(name))
            {
                continue;
            }
            text.push_str(&format!("  • {name}\n"));
        }
        text
    }

    /// Format plugins list for display.
    pub fn plugins_list_text(&self) -> String {
        if self.plugin_names.is_empty() {
            return "No plugins loaded.".to_string();
        }
        let mut text = format!("Loaded plugins ({}):\n", self.plugin_names.len());
        for name in &self.plugin_names {
            text.push_str(&format!("  • {name}\n"));
        }
        text
    }

    /// Handle a crossterm key event.
    pub fn handle_key(&mut self, key: KeyEvent) -> KeyAction {
        // ratatui FAQ / Windows: every physical key yields Press then Release.
        // Handling anything but Press inserts each character twice ("double input").
        // Windows auto-repeat still arrives as additional Press events (not
        // Release/Repeat), so holding a key keeps working.
        if !key.is_press() {
            return KeyAction::None;
        }

        // `/help` overlay takes priority when open.
        if self.show_help_overlay {
            return self.handle_help_overlay_key(key);
        }
        // Shortcuts overlay takes priority when open.
        if self.show_shortcuts_overlay {
            if key.code == KeyCode::Esc || key.code == KeyCode::Char('q') {
                self.show_shortcuts_overlay = false;
                return KeyAction::None;
            }
            if key.code == KeyCode::Char('j') || key.code == KeyCode::Down {
                // Scroll down (simple — no scroll state needed for small list)
                return KeyAction::None;
            }
            if key.code == KeyCode::Char('k') || key.code == KeyCode::Up {
                return KeyAction::None;
            }
            // Ignore other keys while overlay is open.
            return KeyAction::None;
        }
        // `/connect` API-key overlay takes priority when open.
        if self.show_connect {
            return self.handle_connect_key(key);
        }
        // 3-tier model picker takes priority when open.
        if self.show_model_picker {
            return self.handle_model_picker_key(key);
        }
        // Theme picker takes priority when open.
        if self.show_theme_picker {
            return self.handle_theme_picker_key(key);
        }
        // History search mode takes priority.
        if self.show_history_search {
            return self.handle_history_search_key(key);
        }
        if self.show_skills_browser {
            return self.handle_skills_browser_key(key);
        }
        if self.composer.show_slash_palette && self.composer.input_buffer.starts_with('/') {
            if let Some(action) = self.handle_slash_palette_key(key) {
                return action;
            }
        }
        // Agent picker navigation takes priority when picker is open.
        if self.show_agent_picker {
            return self.handle_agent_picker_key(key);
        }

        // Alt+Left / Alt+Right — switch prev/next agent thread.
        if key.code == KeyCode::Left && key.modifiers.contains(KeyModifiers::ALT) {
            return KeyAction::SwitchAgentPrev;
        }
        if key.code == KeyCode::Right && key.modifiers.contains(KeyModifiers::ALT) {
            return KeyAction::SwitchAgentNext;
        }

        // Press 'o' when pending approval from inactive thread.
        if key.code == KeyCode::Char('o') && self.pending_approval.is_some() {
            return KeyAction::SwitchToApprovalSource;
        }

        // Ctrl+C always quits.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return KeyAction::Quit;
        }

        // Chat scroll (PageUp/PageDown / Ctrl+Up/Down) — free while no overlay
        // steals focus. Does not conflict with draft history (plain Up/Down).
        if key.code == KeyCode::PageUp {
            self.scroll_chat_up(8);
            return KeyAction::None;
        }
        if key.code == KeyCode::PageDown {
            self.scroll_chat_down(8);
            return KeyAction::None;
        }
        if key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.scroll_chat_up(3);
            return KeyAction::None;
        }
        if key.code == KeyCode::Down && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.scroll_chat_down(3);
            return KeyAction::None;
        }
        if key.code == KeyCode::Home && key.modifiers.contains(KeyModifiers::CONTROL) {
            // Jump to oldest content (large offset; clamped at render).
            self.chat_scroll = u16::MAX;
            return KeyAction::None;
        }
        if key.code == KeyCode::End && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.scroll_chat_to_bottom();
            return KeyAction::None;
        }

        // Ctrl+L — clear screen (not conversation).
        if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.needs_clear = true;
            return KeyAction::ClearScreen;
        }

        // Ctrl+O — copy latest assistant output to clipboard.
        if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return KeyAction::CopyOutput;
        }

        // Ctrl+R — enter prompt history search mode.
        if key.code == KeyCode::Char('r')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !self.is_streaming
        {
            self.show_history_search = true;
            self.history_search_query.clear();
            self.history_search_index = 0;
            return KeyAction::None;
        }

        // Ctrl+E — open external editor for current input.
        if key.code == KeyCode::Char('e')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !self.is_streaming
        {
            return KeyAction::OpenEditor;
        }

        // Ctrl+. — show keyboard shortcuts overlay.
        if key.code == KeyCode::Char('.') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.show_shortcuts_overlay = true;
            return KeyAction::None;
        }

        // Ctrl+T — open transcript overlay (Codex parity).
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return KeyAction::Slash(SlashAction::ShowTranscript);
        }

        // `t` key — toggle collapse on last tool event (Codex parity).
        if key.code == KeyCode::Char('t')
            && key.modifiers.is_empty()
            && !self.is_streaming
            && !self.tool_events.is_empty()
            && self.composer.input_buffer.is_empty()
        {
            let last_idx = self.tool_events.len() - 1;
            if self.collapsed_tools.contains(&last_idx) {
                self.collapsed_tools.remove(&last_idx);
            } else {
                self.collapsed_tools.insert(last_idx);
            }
            return KeyAction::None;
        }

        // Ctrl+M — toggle multiline input mode.
        if key.code == KeyCode::Char('m') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.multiline = !self.multiline;
            return KeyAction::ToggleMultiline;
        }

        // Shift+Tab — cycle session mode Normal→Plan→AlwaysApprove.
        if key.code == KeyCode::BackTab {
            self.session_mode = self.session_mode.next();
            if self.session_mode == SessionMode::AlwaysApprove {
                self.always_approve = true;
            } else if self.session_mode == SessionMode::Normal {
                self.always_approve = false;
            }
            return KeyAction::CycleMode;
        }

        // Esc — cancel streaming if active, otherwise close overlays.
        if key.code == KeyCode::Esc {
            if self.is_streaming {
                self.is_streaming = false;
                self.status_indicator.stop();
                self.footer_mode = FooterMode::Idle;
                self.streaming_text.clear();
                self.streaming_reasoning_text.clear();
                self.composer.queued_input = None;
                return KeyAction::CancelStreaming;
            }
            self.show_diff = false;
            self.composer.show_slash_palette = false;
            // Esc Esc — edit previous user message (Codex backtrack).
            if self.composer.input_buffer.is_empty() {
                let now = std::time::Instant::now();
                let is_double_esc = self
                    .last_esc_press
                    .map(|t| now.duration_since(t).as_millis() < 500)
                    .unwrap_or(false);
                if is_double_esc {
                    self.last_esc_press = None;
                    return KeyAction::Slash(SlashAction::EditPreviousMessage);
                }
                self.last_esc_press = Some(now);
            } else {
                self.last_esc_press = None;
            }
            return KeyAction::None;
        }

        // Tab while streaming — queue follow-up input.
        if key.code == KeyCode::Tab && self.is_streaming && !self.composer.input_buffer.is_empty() {
            self.composer.queued_input = Some(self.composer.input_buffer.clone());
            self.composer.input_buffer.clear();
            self.composer.cursor_pos = 0;
            return KeyAction::QueueInput;
        }

        if key.code == KeyCode::Char('d')
            && !self.is_streaming
            && self.diff_view.is_some()
            && self.composer.input_buffer.is_empty()
        {
            self.show_diff = !self.show_diff;
            return KeyAction::None;
        }
        if key.code == KeyCode::Char('q')
            && !self.is_streaming
            && self.composer.input_buffer.is_empty()
        {
            self.should_quit = true;
            return KeyAction::Quit;
        }
        // Vim mode scrollback keys — only when vim_mode is on,
        // not streaming, and input buffer is empty (so they don't conflict
        // with text editing).
        if self.vim_mode && !self.is_streaming && self.composer.input_buffer.is_empty() {
            match key.code {
                KeyCode::Char('j') => {
                    self.scroll_chat_down(1);
                    return KeyAction::None;
                }
                KeyCode::Char('k') => {
                    self.scroll_chat_up(1);
                    return KeyAction::None;
                }
                KeyCode::Char('g') => {
                    self.chat_scroll = u16::MAX;
                    return KeyAction::None;
                }
                KeyCode::Char('G') => {
                    self.scroll_chat_to_bottom();
                    return KeyAction::None;
                }
                _ => {}
            }
            // Ctrl+U / Ctrl+D in vim mode = half-page scroll (override the
            // existing line-kill behavior only when buffer is empty).
            if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.scroll_chat_up(8);
                return KeyAction::None;
            }
            if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.scroll_chat_down(8);
                return KeyAction::None;
            }
        }
        // Alt+Enter inserts a newline (multiline composer). Must run before the
        // submit-on-Enter handler below, which only checks `key.code == Enter`.
        if key.code == KeyCode::Enter
            && key.modifiers.contains(KeyModifiers::ALT)
            && !self.is_streaming
        {
            self.composer
                .input_buffer
                .insert(self.composer.cursor_pos, '\n');
            self.composer.cursor_pos += 1;
            self.update_slash_palette();
            return KeyAction::None;
        }
        // Ctrl+J inserts a newline (reliable across terminals, Codex parity).
        if key.code == KeyCode::Char('j')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !self.is_streaming
        {
            self.composer
                .input_buffer
                .insert(self.composer.cursor_pos, '\n');
            self.composer.cursor_pos += 1;
            self.update_slash_palette();
            return KeyAction::None;
        }
        // In multiline mode, Enter inserts a newline.
        // Ctrl+Enter (or Alt+Enter above) submits.
        if key.code == KeyCode::Enter
            && self.multiline
            && !self.is_streaming
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.composer
                .input_buffer
                .insert(self.composer.cursor_pos, '\n');
            self.composer.cursor_pos += 1;
            self.update_slash_palette();
            return KeyAction::None;
        }
        // Ctrl+Enter submits in multiline mode.
        if key.code == KeyCode::Enter
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !self.is_streaming
        {
            // Fall through to the submit handler below (don't return here).
        }
        if key.code == KeyCode::Enter && !self.is_streaming {
            // New turn → follow the live stream.
            self.scroll_chat_to_bottom();
            if self.composer.input_buffer.starts_with('/') {
                let reg = self.skill_registry();
                if let Some((names, task)) =
                    slash::parse_skill_chain_line(&self.composer.input_buffer, &reg)
                {
                    self.composer.input_buffer.clear();
                    self.composer.cursor_pos = 0;
                    self.composer.show_slash_palette = false;
                    return KeyAction::Slash(SlashAction::InvokeSkillChain(names, task));
                }
                let action = match slash::parse(&self.composer.input_buffer) {
                    Some(SlashCommand::Help) => SlashAction::ShowHelp,
                    Some(SlashCommand::Clear) => SlashAction::ClearChat,
                    Some(SlashCommand::Quit) => SlashAction::Quit,
                    Some(SlashCommand::Diff) => SlashAction::ToggleDiff,
                    Some(SlashCommand::Model(name)) => {
                        if name.is_empty() {
                            SlashAction::OpenModelPicker
                        } else {
                            SlashAction::SetModel(name)
                        }
                    }
                    Some(SlashCommand::Provider(name)) => {
                        if name.is_empty() {
                            let p = if self.provider.is_empty() {
                                "xai".to_string()
                            } else {
                                self.provider.clone()
                            };
                            let (status, _) = crate::connect::status_for(&p);
                            SlashAction::ShowMessage(format!(
                                "Provider: {p} · model: {} · key: {status}\nTip: /connect to add or change an API key in the TUI.",
                                if self.model.is_empty() {
                                    "default"
                                } else {
                                    self.model.as_str()
                                }
                            ))
                        } else {
                            SlashAction::ShowMessage(format!(
                                "Provider '{name}'. Use /connect {name} to enter an API key."
                            ))
                        }
                    }
                    Some(SlashCommand::Connect(name)) => {
                        if name.is_empty() {
                            SlashAction::OpenConnect(None)
                        } else {
                            SlashAction::OpenConnect(Some(name))
                        }
                    }
                    Some(SlashCommand::Logout(name)) => {
                        let id = if name.is_empty() {
                            if self.provider.is_empty() {
                                "xai".to_string()
                            } else {
                                self.provider.clone()
                            }
                        } else {
                            name
                        };
                        match crate::connect::remove_provider_key(&id) {
                            Ok(msg) => SlashAction::ProviderLoggedOut(msg),
                            Err(e) => SlashAction::ShowMessage(format!("Logout failed: {e}")),
                        }
                    }
                    Some(SlashCommand::Sandbox(mode)) => {
                        if mode.is_empty() {
                            SlashAction::ShowSandbox
                        } else {
                            SlashAction::ShowMessage(format!("Sandbox mode: {mode}"))
                        }
                    }
                    Some(SlashCommand::Sessions) => {
                        SlashAction::ShowMessage(self.sessions_list_text())
                    }
                    Some(SlashCommand::Skills) => SlashAction::OpenSkillsBrowser,
                    Some(SlashCommand::Plugins) => {
                        SlashAction::ShowMessage(self.plugins_list_text())
                    }
                    Some(SlashCommand::Reload) => match self.reload_skills() {
                        Ok(count) => SlashAction::ShowMessage(format!(
                            "Reloaded {count} skill(s) from {} dir(s)",
                            self.skill_dirs.len()
                        )),
                        Err(e) => SlashAction::ShowMessage(format!("Reload failed: {e}")),
                    },
                    Some(SlashCommand::ReloadPlugins) => match self.reload_plugins() {
                        Ok(count) => SlashAction::ShowMessage(format!(
                            "Reloaded {count} plugin(s) from {} dir(s)",
                            self.plugin_dirs.len()
                        )),
                        Err(e) => SlashAction::ShowMessage(format!("Reload failed: {e}")),
                    },
                    Some(SlashCommand::DiffSessions(a, b)) => match self.compare_sessions(&a, &b) {
                        Ok(text) => SlashAction::ShowMessage(text),
                        Err(e) => SlashAction::ShowMessage(format!("Diff failed: {e}")),
                    },
                    Some(SlashCommand::Agent) => SlashAction::OpenAgentPicker,
                    Some(SlashCommand::Effort(level)) => {
                        if level.is_empty() {
                            SlashAction::ShowMessage(format!("Current effort: {}", self.effort))
                        } else {
                            match level.parse::<Effort>() {
                                Ok(effort) => SlashAction::SetEffort(effort),
                                Err(e) => SlashAction::ShowMessage(e),
                            }
                        }
                    }
                    Some(SlashCommand::Ask) => SlashAction::ToggleAsk,
                    Some(SlashCommand::Find(query)) => {
                        if query.is_empty() {
                            SlashAction::ShowMessage("Usage: /find <query>".to_string())
                        } else {
                            SlashAction::Find(query)
                        }
                    }
                    Some(SlashCommand::Rewind(path)) => {
                        if path.is_empty() {
                            SlashAction::ShowMessage("Usage: /rewind <path>".to_string())
                        } else {
                            SlashAction::Rewind(path)
                        }
                    }
                    Some(SlashCommand::Compact) => SlashAction::Compact,
                    Some(SlashCommand::Image(path)) => {
                        // the `/image` command is gated behind the
                        // `image-composer` feature flag. When disabled,
                        // `/image` is rejected with a clear message instead
                        // of attaching an image.
                        if !zerozero_core::ZeroZeroConfig::load()
                            .feature_is_enabled("image-composer")
                        {
                            SlashAction::ShowMessage(
                                "The /image command is disabled (feature flag 'image-composer' is off). \
                                 Enable it with `zz config feature enable image-composer`."
                                    .to_string(),
                            )
                        } else if path.is_empty() {
                            SlashAction::ShowMessage("Usage: /image <path>".to_string())
                        } else {
                            SlashAction::Image(path)
                        }
                    }
                    Some(SlashCommand::Unimage) => SlashAction::Unimage,
                    Some(SlashCommand::Copy) => SlashAction::CopyOutput,
                    Some(SlashCommand::Theme(name)) => {
                        if name.is_empty() {
                            SlashAction::OpenThemePicker
                        } else {
                            SlashAction::SetTheme(name)
                        }
                    }
                    Some(SlashCommand::UiTheme(name)) => SlashAction::SetUiTheme(name),
                    Some(SlashCommand::Plan(desc)) => SlashAction::EnterPlan(desc),
                    Some(SlashCommand::ViewPlan) => SlashAction::ViewPlan,
                    Some(SlashCommand::AlwaysApprove) => SlashAction::ToggleAlwaysApprove,
                    Some(SlashCommand::Multiline) => SlashAction::ToggleMultiline,
                    Some(SlashCommand::Context) => SlashAction::ShowContext,
                    Some(SlashCommand::CompactMode) => SlashAction::ToggleCompactMode,
                    Some(SlashCommand::Timestamps) => SlashAction::ToggleTimestamps,
                    Some(SlashCommand::VimMode) => SlashAction::ToggleVimMode,
                    Some(SlashCommand::Shortcuts) => SlashAction::ShowShortcuts,
                    Some(SlashCommand::Export) => SlashAction::ExportConversation,
                    Some(SlashCommand::Transcript) => SlashAction::ShowTranscript,
                    Some(SlashCommand::Status) => SlashAction::ShowStatus,
                    Some(SlashCommand::New) => SlashAction::NewSession,
                    Some(SlashCommand::Init) => SlashAction::InitProject,
                    Some(SlashCommand::Review) => SlashAction::ReviewChanges,
                    Some(SlashCommand::Keymap) => SlashAction::ShowKeymap,
                    Some(SlashCommand::Unknown(cmd, arg)) => {
                        let reg = self.skill_registry();
                        if let Some((name, args)) = slash::match_skill_registry(&cmd, &arg, &reg) {
                            SlashAction::InvokeSkill(name, args)
                        } else {
                            SlashAction::ShowMessage(format!("Unknown command: /{cmd}"))
                        }
                    }
                    None => SlashAction::None,
                };
                self.composer.input_buffer.clear();
                self.composer.cursor_pos = 0;
                self.composer.show_slash_palette = false;
                if matches!(action, SlashAction::OpenSkillsBrowser) {
                    self.show_skills_browser = true;
                    self.skills_browser_index = 0;
                }
                return KeyAction::Slash(action);
            }
            // `!` prefix — run local shell command (not sent to model).
            if let Some(cmd) = self.composer.input_buffer.strip_prefix('!') {
                let cmd = cmd.trim().to_string();
                self.composer.input_buffer.clear();
                self.composer.cursor_pos = 0;
                if cmd.is_empty() {
                    return KeyAction::None;
                }
                return KeyAction::Slash(SlashAction::ShellCommand(cmd));
            }
            // `@` prefix — fuzzy file search + attach to prompt.
            if self.composer.input_buffer.starts_with('@') {
                let query = &self.composer.input_buffer[1..];
                self.file_search_query = query.to_string();
                self.show_file_search = true;
                self.file_search_results = self.fuzzy_find_files(query);
                self.file_search_index = 0;
                return KeyAction::None;
            }
            if !self.composer.input_buffer.is_empty() {
                return KeyAction::Submit;
            }
            return KeyAction::None;
        }
        if key.code == KeyCode::Tab
            && !self.is_streaming
            && self.composer.input_buffer.starts_with('/')
        {
            if let Some(next) = slash::apply_slash_tab(
                &self.composer.input_buffer,
                &self.skill_slash_entries,
                self.composer.slash_menu_index,
            ) {
                self.composer.input_buffer = next;
                self.sync_cursor_to_end();
            }
            return KeyAction::None;
        }
        // Draft history navigation (Up/Down) when not in slash palette.
        if !self.is_streaming && !self.composer.input_buffer.starts_with('/') {
            match key.code {
                KeyCode::Up => {
                    if !self.composer.draft_history.is_empty() {
                        let new_idx = match self.composer.draft_history_index {
                            None => self.composer.draft_history.len() - 1,
                            Some(0) => 0,
                            Some(i) => i - 1,
                        };
                        self.composer.draft_history_index = Some(new_idx);
                        self.composer.input_buffer = self.composer.draft_history[new_idx].clone();
                        self.sync_cursor_to_end();
                        return KeyAction::None;
                    }
                }
                KeyCode::Down => {
                    if let Some(i) = self.composer.draft_history_index {
                        if i + 1 >= self.composer.draft_history.len() {
                            self.composer.draft_history_index = None;
                            self.composer.input_buffer.clear();
                            self.composer.cursor_pos = 0;
                        } else {
                            self.composer.draft_history_index = Some(i + 1);
                            self.composer.input_buffer = self.composer.draft_history[i + 1].clone();
                            self.sync_cursor_to_end();
                        }
                        return KeyAction::None;
                    }
                }
                _ => {}
            }
        }
        // Readline-style cursor movement (only when not streaming).
        if !self.is_streaming {
            match key.code {
                KeyCode::Left => {
                    if self.composer.cursor_pos > 0 {
                        self.composer.cursor_pos = self.prev_char_boundary();
                    }
                    return KeyAction::None;
                }
                KeyCode::Right => {
                    if self.composer.cursor_pos < self.composer.input_buffer.len() {
                        self.composer.cursor_pos = self.next_char_boundary();
                    }
                    return KeyAction::None;
                }
                KeyCode::Home => {
                    self.composer.cursor_pos = 0;
                    return KeyAction::None;
                }
                KeyCode::End => {
                    self.sync_cursor_to_end();
                    return KeyAction::None;
                }
                _ => {}
            }
            // Ctrl+A: move to start (Home equivalent).
            if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.composer.cursor_pos = 0;
                return KeyAction::None;
            }
            // Ctrl+K: kill to end of line.
            if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.composer
                    .input_buffer
                    .truncate(self.composer.cursor_pos);
                self.update_slash_palette();
                return KeyAction::None;
            }
            // Ctrl+U: kill to start of line.
            if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.composer.input_buffer.drain(..self.composer.cursor_pos);
                self.composer.cursor_pos = 0;
                self.update_slash_palette();
                return KeyAction::None;
            }
            // Ctrl+W: delete previous word.
            if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.delete_prev_word();
                self.update_slash_palette();
                return KeyAction::None;
            }
        }
        if key.code == KeyCode::Backspace {
            if self.composer.cursor_pos > 0 {
                let prev = self.prev_char_boundary();
                self.composer
                    .input_buffer
                    .drain(prev..self.composer.cursor_pos);
                self.composer.cursor_pos = prev;
            }
            self.update_slash_palette();
            return KeyAction::None;
        }
        if key.code == KeyCode::Delete {
            if self.composer.cursor_pos < self.composer.input_buffer.len() {
                let next = self.next_char_boundary();
                self.composer
                    .input_buffer
                    .drain(self.composer.cursor_pos..next);
            }
            self.update_slash_palette();
            return KeyAction::None;
        }
        if let KeyCode::Char(c) = key.code {
            // Ignore control characters (except those handled above via modifiers).
            if c.is_control() {
                return KeyAction::None;
            }
            if !self.is_streaming {
                if c == '/' && self.composer.input_buffer.is_empty() {
                    self.composer.show_slash_palette = true;
                    self.composer.slash_menu_index = 0;
                }
                self.composer
                    .input_buffer
                    .insert(self.composer.cursor_pos, c);
                self.composer.cursor_pos = self.next_char_boundary();
                self.update_slash_palette();
            }
        }
        KeyAction::None
    }

    /// Update slash palette visibility based on current input buffer.
    ///
    /// - Open while the buffer is `/…` and the user is still editing the
    ///   **command token** (no space yet). After a space, close the palette so
    ///   args typing is unobstructed (Codex/Claude behavior).
    /// - **Do not** reset `slash_menu_index` on every keystroke (that wiped
    ///   ↑↓ selection and made the menu feel broken). Only clamp to list len;
    ///   reset to 0 when opening from closed or when the list shrinks past.
    pub fn update_slash_palette(&mut self) {
        let was_open = self.composer.show_slash_palette;
        let starts = self.composer.input_buffer.starts_with('/');
        let token_phase = starts
            && !self
                .composer
                .input_buffer
                .get(1..)
                .map(|s| s.chars().any(char::is_whitespace))
                .unwrap_or(false);
        self.composer.show_slash_palette = token_phase;
        if !self.composer.show_slash_palette {
            if was_open {
                self.composer.slash_menu_index = 0;
            }
            return;
        }
        let n =
            crate::slash_menu::ranked_len(&self.skill_slash_entries, &self.composer.input_buffer);
        if n == 0 {
            self.composer.slash_menu_index = 0;
        } else if !was_open {
            // Fresh open → best/first match.
            self.composer.slash_menu_index = 0;
        } else {
            self.composer.slash_menu_index = self.composer.slash_menu_index.min(n - 1);
        }
    }

    fn prev_char_boundary(&self) -> usize {
        self.composer.prev_char_boundary()
    }

    fn next_char_boundary(&self) -> usize {
        self.composer.next_char_boundary()
    }

    /// Delete the previous word (whitespace-delimited) before the cursor.
    fn delete_prev_word(&mut self) {
        let mut start = self.composer.cursor_pos;
        // Skip trailing whitespace.
        while start > 0 {
            let prev = {
                let mut p = start - 1;
                while p > 0 && !self.composer.input_buffer.is_char_boundary(p) {
                    p -= 1;
                }
                p
            };
            if self.composer.input_buffer[prev..start].trim().is_empty() {
                start = prev;
            } else {
                break;
            }
        }
        // Delete the word.
        while start > 0 {
            let prev = {
                let mut p = start - 1;
                while p > 0 && !self.composer.input_buffer.is_char_boundary(p) {
                    p -= 1;
                }
                p
            };
            let ch = &self.composer.input_buffer[prev..start];
            if !ch.trim().is_empty() {
                start = prev;
            } else {
                break;
            }
        }
        self.composer
            .input_buffer
            .drain(start..self.composer.cursor_pos);
        self.composer.cursor_pos = start;
    }

    fn handle_slash_palette_key(&mut self, key: KeyEvent) -> Option<KeyAction> {
        // Same ranking as the rendered list (category order when empty).
        let items = slash::all_menu_items(&self.skill_slash_entries);
        let ranked = slash::ranked_for_palette(
            &items,
            crate::slash_menu::slash_query(&self.composer.input_buffer),
        );
        let len = ranked.len();
        match key.code {
            KeyCode::Up => {
                if len > 0 {
                    self.composer.slash_menu_index = if self.composer.slash_menu_index == 0 {
                        len - 1
                    } else {
                        self.composer.slash_menu_index - 1
                    };
                }
                Some(KeyAction::None)
            }
            KeyCode::Down => {
                if len > 0 {
                    self.composer.slash_menu_index = (self.composer.slash_menu_index + 1) % len;
                }
                Some(KeyAction::None)
            }
            // Ctrl+P / Ctrl+N — vim/emacs nav without stealing letter keys for filter.
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if len > 0 {
                    self.composer.slash_menu_index = if self.composer.slash_menu_index == 0 {
                        len - 1
                    } else {
                        self.composer.slash_menu_index - 1
                    };
                }
                Some(KeyAction::None)
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if len > 0 {
                    self.composer.slash_menu_index = (self.composer.slash_menu_index + 1) % len;
                }
                Some(KeyAction::None)
            }
            KeyCode::Tab => {
                //selection then complete (second Tab → next match).
                if len > 1 {
                    let current = self
                        .composer
                        .input_buffer
                        .strip_prefix('/')
                        .unwrap_or("")
                        .trim_end()
                        .to_string();
                    let cur_invoke = ranked
                        .get(self.composer.slash_menu_index.min(len - 1))
                        .map(|&(i, _)| items[i].invoke.as_str())
                        .unwrap_or("");
                    // If buffer already matches current selection, advance.
                    if current == cur_invoke || current == format!("{cur_invoke} ") {
                        self.composer.slash_menu_index = (self.composer.slash_menu_index + 1) % len;
                    }
                }
                if let Some(next) = slash::apply_slash_tab(
                    &self.composer.input_buffer,
                    &self.skill_slash_entries,
                    self.composer.slash_menu_index,
                ) {
                    self.composer.input_buffer = next;
                    self.sync_cursor_to_end();
                    // Completing inserts a trailing space → close palette (token done).
                    self.composer.show_slash_palette = false;
                }
                Some(KeyAction::None)
            }
            KeyCode::Enter => {
                let reg = self.skill_registry();
                if slash::is_slash_submittable(
                    &self.composer.input_buffer,
                    &reg,
                    &self.skill_slash_entries,
                ) {
                    // Fully-formed command → fall through to normal Enter submit.
                    return None;
                }
                let Some(item) = crate::slash_menu::selected_item(self) else {
                    return Some(KeyAction::None);
                };
                if item.invoke == "skills" {
                    self.composer.show_slash_palette = false;
                    self.composer.input_buffer.clear();
                    self.composer.cursor_pos = 0;
                    self.show_skills_browser = true;
                    self.skills_browser_index = 0;
                    return Some(KeyAction::Slash(SlashAction::OpenSkillsBrowser));
                }
                // Complete selection into the buffer; keep open only if still
                // token-phase (we add a trailing space so palette closes).
                self.composer.input_buffer = format!("/{} ", item.invoke);
                self.sync_cursor_to_end();
                self.composer.show_slash_palette = false;
                // Zero-arg commands can execute immediately on second Enter;
                // first Enter only completes (predictable).
                Some(KeyAction::None)
            }
            KeyCode::Esc => {
                self.composer.show_slash_palette = false;
                // Keep the typed filter so the user can edit; do not wipe buffer.
                Some(KeyAction::None)
            }
            _ => None,
        }
    }

    fn handle_skills_browser_key(&mut self, key: KeyEvent) -> KeyAction {
        let len = self.skill_slash_entries.len();
        match key.code {
            KeyCode::Up if len > 0 => {
                self.skills_browser_index = if self.skills_browser_index == 0 {
                    len - 1
                } else {
                    self.skills_browser_index - 1
                };
                KeyAction::None
            }
            KeyCode::Down if len > 0 => {
                self.skills_browser_index = (self.skills_browser_index + 1) % len;
                KeyAction::None
            }
            KeyCode::Enter => {
                if let Some(name) = crate::skills_browser::selected_skill_name(self) {
                    self.composer.input_buffer = format!("/{name} ");
                    self.sync_cursor_to_end();
                    self.composer.show_slash_palette = true;
                }
                self.show_skills_browser = false;
                KeyAction::None
            }
            KeyCode::Esc => {
                self.show_skills_browser = false;
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    /// Open the 3-tier model picker, pre-selecting the current provider/model.
    pub fn open_model_picker(&mut self) {
        let provider_id = if self.provider.is_empty() {
            crate::model_catalog::detect_provider_for_model(&self.model)
        } else {
            self.provider.as_str()
        };
        self.picker_provider = provider_id.to_string();
        // Try to pre-select the current model within the provider.
        self.picker_model = if crate::model_catalog::find_model(provider_id, &self.model).is_some()
        {
            self.model.clone()
        } else if let Some(p) = crate::model_catalog::find_provider(provider_id) {
            p.models
                .first()
                .map(|m| m.id.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.picker_effort = self.effort;
        self.model_picker_tier = 0;
        self.model_picker_index = 0;
        self.show_model_picker = true;
    }

    /// Number of entries in the current picker tier.
    fn model_picker_tier_len(&self) -> usize {
        match self.model_picker_tier {
            0 => crate::model_catalog::CATALOG.len(),
            1 => crate::model_catalog::find_provider(&self.picker_provider)
                .map(|p| p.models.len())
                .unwrap_or(0),
            2 if crate::model_catalog::find_model(&self.picker_provider, &self.picker_model)
                .map(|m| m.reasoning)
                .unwrap_or(false) =>
            {
                crate::model_catalog::EFFORT_TIERS.len()
            }
            _ => 0,
        }
    }

    /// 3-tier model picker key handling (Codex-style).
    fn handle_model_picker_key(&mut self, key: KeyEvent) -> KeyAction {
        let len = self.model_picker_tier_len();
        match key.code {
            KeyCode::Esc => {
                self.show_model_picker = false;
                KeyAction::None
            }
            KeyCode::Up if len > 0 => {
                self.model_picker_index = if self.model_picker_index == 0 {
                    len - 1
                } else {
                    self.model_picker_index - 1
                };
                KeyAction::None
            }
            KeyCode::Down if len > 0 => {
                self.model_picker_index = (self.model_picker_index + 1) % len;
                KeyAction::None
            }
            KeyCode::Left => {
                // Move back a tier (provider ← model ← effort).
                if self.model_picker_tier > 0 {
                    self.model_picker_tier -= 1;
                    self.model_picker_index = match self.model_picker_tier {
                        0 => crate::model_catalog::CATALOG
                            .iter()
                            .position(|p| p.id == self.picker_provider)
                            .unwrap_or(0),
                        1 => crate::model_catalog::find_provider(&self.picker_provider)
                            .and_then(|p| p.models.iter().position(|m| m.id == self.picker_model))
                            .unwrap_or(0),
                        _ => 0,
                    };
                }
                KeyAction::None
            }
            KeyCode::Right | KeyCode::Tab | KeyCode::Enter => self.advance_model_picker_tier(),
            _ => KeyAction::None,
        }
    }

    /// Advance to the next tier, or apply if on the last tier.
    fn advance_model_picker_tier(&mut self) -> KeyAction {
        let len = self.model_picker_tier_len();
        if len == 0 {
            // No entries in this tier (e.g. effort for non-reasoning model) → apply.
            return self.apply_model_picker();
        }
        let idx = self.model_picker_index.min(len - 1);
        match self.model_picker_tier {
            0 => {
                // Select provider, advance to model tier.
                if let Some(p) = crate::model_catalog::CATALOG.get(idx) {
                    self.picker_provider = p.id.to_string();
                    // Reset model to first model of the new provider.
                    self.picker_model = p
                        .models
                        .first()
                        .map(|m| m.id.to_string())
                        .unwrap_or_default();
                    self.model_picker_tier = 1;
                    self.model_picker_index = 0;
                }
                KeyAction::None
            }
            1 => {
                // Select model, advance to effort tier (if reasoning) or apply.
                if let Some(p) = crate::model_catalog::find_provider(&self.picker_provider) {
                    if let Some(m) = p.models.get(idx) {
                        self.picker_model = m.id.to_string();
                        self.picker_effort = m.default_effort;
                        if m.reasoning {
                            self.model_picker_tier = 2;
                            self.model_picker_index = crate::model_catalog::EFFORT_TIERS
                                .iter()
                                .position(|e| *e == m.default_effort)
                                .unwrap_or(0);
                            return KeyAction::None;
                        }
                        // Non-reasoning model → apply immediately.
                        return self.apply_model_picker();
                    }
                }
                KeyAction::None
            }
            2 => {
                // Select effort, apply.
                if let Some(effort) = crate::model_catalog::EFFORT_TIERS.get(idx) {
                    self.picker_effort = *effort;
                }
                self.apply_model_picker()
            }
            _ => KeyAction::None,
        }
    }

    /// Apply the picker selection: emit SetModelFull + SetEffort, close picker.
    fn apply_model_picker(&mut self) -> KeyAction {
        let provider = self.picker_provider.clone();
        let model = self.picker_model.clone();
        let effort = self.picker_effort;
        self.show_model_picker = false;
        // We emit both actions via a single Slash action by piggy-backing on
        // SetModelFull; the event loop applies effort from picker_effort.
        KeyAction::Slash(SlashAction::SetModelFull {
            provider,
            model,
            effort,
        })
    }

    /// `/help` overlay key handling (palette overhaul).
    const fn handle_help_overlay_key(&mut self, key: KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                self.show_help_overlay = false;
                self.help_scroll = 0;
                KeyAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help_scroll = self.help_scroll.saturating_add(1);
                KeyAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
                KeyAction::None
            }
            KeyCode::Char('g') => {
                self.help_scroll = 0;
                KeyAction::None
            }
            KeyCode::Char('G') => {
                // Jump near the end; exact max computed at render time.
                self.help_scroll = usize::MAX / 4;
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    /// Handle keys in the `/connect` overlay (provider list → key entry).
    fn handle_connect_key(&mut self, key: KeyEvent) -> KeyAction {
        match self.connect_stage {
            0 => self.handle_connect_provider_list_key(key),
            _ => self.handle_connect_key_entry_key(key),
        }
    }

    fn handle_connect_provider_list_key(&mut self, key: KeyEvent) -> KeyAction {
        let len = crate::connect::provider_count();
        match key.code {
            KeyCode::Esc => {
                self.close_connect();
                KeyAction::None
            }
            KeyCode::Up | KeyCode::Char('k') if len > 0 => {
                self.connect_index = if self.connect_index == 0 {
                    len - 1
                } else {
                    self.connect_index - 1
                };
                KeyAction::None
            }
            KeyCode::Down | KeyCode::Char('j') if len > 0 => {
                self.connect_index = (self.connect_index + 1) % len;
                KeyAction::None
            }
            KeyCode::Char('d') | KeyCode::Delete if len > 0 => {
                let id = zerozero_llm::PROVIDERS[self.connect_index].id;
                match crate::connect::remove_provider_key(id) {
                    Ok(msg) => KeyAction::Slash(SlashAction::ProviderLoggedOut(msg)),
                    Err(e) => {
                        KeyAction::Slash(SlashAction::ShowMessage(format!("Logout failed: {e}")))
                    }
                }
            }
            KeyCode::Enter if len > 0 => {
                let spec = &zerozero_llm::PROVIDERS[self.connect_index];
                self.connect_provider = spec.id.to_string();
                if !spec.requires_key {
                    self.close_connect();
                    return KeyAction::Slash(SlashAction::ShowMessage(format!(
                        "Provider '{}' is local — no API key required. Set ZZ_PROVIDER={} to use it.",
                        spec.id, spec.id
                    )));
                }
                self.connect_stage = 1;
                self.connect_key_buffer.clear();
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    fn handle_connect_key_entry_key(&mut self, key: KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Esc => {
                // Back to provider list (keep selection).
                self.connect_stage = 0;
                self.connect_key_buffer.clear();
                KeyAction::None
            }
            KeyCode::Enter => {
                let provider = self.connect_provider.clone();
                let key_val = self.connect_key_buffer.clone();
                match crate::connect::save_provider_key(&provider, &key_val) {
                    Ok(msg) => {
                        self.close_connect();
                        KeyAction::Slash(SlashAction::ProviderConnected {
                            provider,
                            message: msg,
                        })
                    }
                    Err(e) => {
                        // Stay on the form so the user can retry.
                        KeyAction::Slash(SlashAction::ShowMessage(format!(
                            "Could not save key: {e}"
                        )))
                    }
                }
            }
            KeyCode::Backspace => {
                self.connect_key_buffer.pop();
                KeyAction::None
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                // Accept printable characters (API keys are ASCII-ish).
                if !c.is_control() {
                    self.connect_key_buffer.push(c);
                }
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    /// Handle keys in the theme picker overlay.
    fn handle_theme_picker_key(&mut self, key: KeyEvent) -> KeyAction {
        let themes = crate::markdown::available_themes();
        let len = themes.len();
        match key.code {
            KeyCode::Up if len > 0 => {
                self.theme_picker_index = if self.theme_picker_index == 0 {
                    len - 1
                } else {
                    self.theme_picker_index - 1
                };
                KeyAction::None
            }
            KeyCode::Down if len > 0 => {
                self.theme_picker_index = (self.theme_picker_index + 1) % len;
                KeyAction::None
            }
            KeyCode::Enter if len > 0 => {
                let name = themes[self.theme_picker_index].clone();
                self.show_theme_picker = false;
                KeyAction::Slash(SlashAction::SetTheme(name))
            }
            KeyCode::Esc => {
                self.show_theme_picker = false;
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    /// Handle keys in the Ctrl+R history search mode.
    fn handle_history_search_key(&mut self, key: KeyEvent) -> KeyAction {
        let filtered = self.filtered_prompt_history();
        let len = filtered.len();
        match key.code {
            KeyCode::Esc => {
                self.show_history_search = false;
                KeyAction::None
            }
            KeyCode::Enter => {
                if let Some(entry) =
                    filtered.get(self.history_search_index.min(len.saturating_sub(1)))
                {
                    self.composer.input_buffer = entry.clone();
                    self.sync_cursor_to_end();
                }
                self.show_history_search = false;
                KeyAction::None
            }
            KeyCode::Up if len > 0 => {
                self.history_search_index = if self.history_search_index == 0 {
                    len - 1
                } else {
                    self.history_search_index - 1
                };
                KeyAction::None
            }
            KeyCode::Down if len > 0 => {
                self.history_search_index = (self.history_search_index + 1) % len;
                KeyAction::None
            }
            KeyCode::Backspace => {
                self.history_search_query.pop();
                self.history_search_index = 0;
                KeyAction::None
            }
            KeyCode::Char(c) => {
                self.history_search_query.push(c);
                self.history_search_index = 0;
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    /// Return prompt history entries matching the current search query.
    pub fn filtered_prompt_history(&self) -> Vec<String> {
        let q = self.history_search_query.to_ascii_lowercase();
        self.prompt_history
            .iter()
            .rev()
            .filter(|p| q.is_empty() || p.to_ascii_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    /// Sync composer cursor to end of buffer.
    pub fn sync_cursor_to_end(&mut self) {
        self.composer.sync_cursor_to_end();
    }

    /// Set composer buffer and sync cursor to end.
    pub fn set_input_buffer(&mut self, s: String) {
        self.composer.set_input_buffer(s);
    }

    /// Clear composer buffer and reset cursor.
    pub fn clear_input_buffer(&mut self) {
        self.composer.clear_input();
    }

    /// Push a prompt to history (called on submit).
    pub fn record_prompt(&mut self, prompt: &str) {
        if !prompt.trim().is_empty() && !prompt.starts_with('/') {
            // Avoid consecutive duplicates.
            if self.prompt_history.last().map(|p| p.as_str()) != Some(prompt) {
                self.prompt_history.push(prompt.to_string());
                if self.prompt_history.len() > 500 {
                    self.prompt_history.remove(0);
                }
            }
            self.composer.draft_history.push(prompt.to_string());
            if self.composer.draft_history.len() > 100 {
                self.composer.draft_history.remove(0);
            }
        }
        self.composer.draft_history_index = None;
    }

    /// Return the latest assistant message content (for /copy).
    pub fn latest_assistant_output(&self) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" || m.role == "agent")
            .map(|m| m.content.clone())
    }

    /// Estimated token count for the current conversation.
    pub fn token_count(&self) -> usize {
        zerozero_compaction::count_tokens(&self.messages)
    }

    /// Spinner character for the current tick.
    ///
    /// Uses ASCII frames only. Braille spinners (⠋⠙…) are **ambiguous-width**
    /// on Windows Terminal — some frames render 1 cell, some 2 → the whole chat
    /// line shifts every tick ("text jumping" while streaming).
    pub fn spinner_char(&self) -> char {
        const FRAMES: &[char] = &['|', '/', '-', '\\'];
        FRAMES[(self.spinner_tick as usize) % FRAMES.len()]
    }

    /// Advance the spinner tick (called on each render or timer).
    pub const fn tick_spinner(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    /// Handle a core engine event on the active thread (tests + legacy callers).
    pub fn handle_core_event(&mut self, event: Event) {
        let tid = self.active_thread_id.clone();
        if tid.is_empty() {
            // Unit tests without thread id — apply directly to visible fields.
            let mut st = ThreadChatState {
                messages: self.messages.clone(),
                streaming_text: self.streaming_text.clone(),
                streaming_reasoning_text: self.streaming_reasoning_text.clone(),
                is_streaming: self.is_streaming,
                session_id: self.session_id.clone(),
                tool_events: self.tool_events.clone(),
            };
            st.apply_event(event.clone());
            if let Event::ToolCompleted {
                tool_name, result, ..
            } = &event
            {
                if tool_name == "write_file" || tool_name == "edit_file" {
                    self.diff_view = Some(crate::diff::DiffView::new(tool_name, "", result));
                    self.show_diff = true;
                }
            }
            self.messages = st.messages;
            self.streaming_text = st.streaming_text;
            self.streaming_reasoning_text = st.streaming_reasoning_text;
            self.is_streaming = st.is_streaming;
            self.session_id = st.session_id;
            self.tool_events = st.tool_events;
            return;
        }
        self.apply_chat_event(&tid, event.clone());
        if let Event::ToolCompleted {
            tool_name, result, ..
        } = event
        {
            if tool_name == "write_file" || tool_name == "edit_file" {
                self.diff_view = Some(crate::diff::DiffView::new(&tool_name, "", &result));
                self.show_diff = true;
            }
        }
    }

    /// Build display text for the chat area: completed messages + streaming text.
    pub fn display_text(&self) -> String {
        let mut text = String::new();
        for msg in &self.messages {
            text.push_str(&format!("{}: {}\n\n", msg.role, msg.content));
        }
        if !self.streaming_text.is_empty() {
            text.push_str(&format!("assistant: {}", self.streaming_text));
        }
        text
    }

    /// Handle key events when the agent picker is open .
    ///
    /// Keys:
    /// - Up/Down: navigate selection
    /// - Enter: select agent (returns `SelectAgent(index)`)
    /// - Esc: close picker
    fn handle_agent_picker_key(&mut self, key: KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Esc => {
                self.show_agent_picker = false;
                KeyAction::None
            }
            KeyCode::Up => {
                if self.agent_picker_selected > 0 {
                    self.agent_picker_selected -= 1;
                }
                KeyAction::None
            }
            KeyCode::Down => {
                if self.agent_picker_selected + 1 < self.live_agents.len() {
                    self.agent_picker_selected += 1;
                }
                KeyAction::None
            }
            KeyCode::Enter => {
                let selected = self.agent_picker_selected;
                self.show_agent_picker = false;
                KeyAction::SelectAgent(selected)
            }
            _ => KeyAction::None,
        }
    }

    /// Get the active agent label for footer display .
    ///
    /// Returns a string like `[agent-0 (root.0) | Running]` or empty if
    /// only the root thread exists.
    pub fn agent_footer_label(&self) -> String {
        if self.live_agents.len() <= 1 {
            return String::new();
        }
        let active = self
            .live_agents
            .iter()
            .find(|a| a.thread_id == self.active_thread_id);
        match active {
            Some(meta) => {
                let status_icon = match meta.status {
                    zerozero_multi_agent::AgentStatus::Running => "Running",
                    zerozero_multi_agent::AgentStatus::Stopped => "Stopped",
                    zerozero_multi_agent::AgentStatus::Completed => "Completed",
                    zerozero_multi_agent::AgentStatus::Failed => "Failed",
                };
                format!(
                    "[{} ({}) | {}]",
                    meta.nickname, meta.agent_path, status_icon
                )
            }
            None => String::new(),
        }
    }
}

/// Action returned by key handling.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    None,
    Submit,
    Quit,
    /// A slash command was entered.
    Slash(SlashAction),
    /// Switch to previous agent thread (Alt+Left).
    SwitchAgentPrev,
    /// Switch to next agent thread (Alt+Right).
    SwitchAgentNext,
    /// Switch to the thread that sent a pending approval request (press 'o').
    SwitchToApprovalSource,
    /// Select an agent in the picker (Enter in picker mode).
    SelectAgent(usize),
    /// Cancel/interrupt the current streaming turn (Esc while streaming).
    CancelStreaming,
    /// Queue the current input for auto-submission when the turn completes (Tab while streaming).
    QueueInput,
    /// Clear the terminal screen (Ctrl+L).
    ClearScreen,
    /// Copy the latest assistant output to clipboard (Ctrl+O or /copy).
    CopyOutput,
    /// Open external editor ($EDITOR) for the current input (Ctrl+E).
    OpenEditor,
    ///session mode Normal→Plan→AlwaysApprove (Shift+Tab).
    CycleMode,
    /// Toggle multiline input mode (Ctrl+M).
    ToggleMultiline,
    /// Show keyboard shortcuts overlay (Ctrl+.).
    ShowShortcuts,
}

/// Action produced by a parsed slash command.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashAction {
    None,
    Quit,
    ClearChat,
    ToggleDiff,
    ShowHelp,
    /// Display a message in the chat area.
    ShowMessage(String),
    /// Open the agent picker popup .
    OpenAgentPicker,
    /// Set reasoning effort). Carries the parsed level,
    /// or signals an error via ShowMessage when the level is invalid.
    SetEffort(Effort),
    /// Set the LLM model mid-session). Carries the new
    /// model name. The provider is rebuilt by the event loop handler.
    SetModel(String),
    /// 3-tier model picker: switch both provider and model mid-session.
    /// Carries `(provider_id, model_id, effort)`. The provider is rebuilt
    /// by the event loop handler using the new provider type, and the effort
    /// is applied alongside the model switch.
    SetModelFull {
        provider: String,
        model: String,
        effort: Effort,
    },
    /// Open the 3-tier model picker overlay (`/model` with no arg).
    OpenModelPicker,
    /// Toggle ask mode). When on, every tool call
    /// Toggle ask mode). TUI toggles the session flag.
    ToggleAsk,
    /// Rewind a file from its shadow snapshot (B).
    /// Carries the file path.
    Rewind(String),
    /// Fuzzy file-path search over cwd . Carries the query.
    Find(String),
    /// Manually trigger token-budget compaction).
    /// Carries the token count before compaction for the status message.
    Compact,
    /// Attach an image to the next message . Carries the file path.
    Image(String),
    /// Clear all pending image attachments .
    Unimage,
    /// Run a turn with a loaded skill (`/<skill-name> <task>`).
    InvokeSkill(String, String),
    /// Chain skills then run (`/a /b <task>`).
    InvokeSkillChain(Vec<String>, String),
    /// Full-screen skills browser.
    OpenSkillsBrowser,
    /// Copy the latest assistant output to clipboard /copy).
    CopyOutput,
    /// Open the theme picker overlay /theme).
    OpenThemePicker,
    /// Set the syntax highlighting theme /theme <name>).
    SetTheme(String),
    /// Switch the UI chrome palette (/ui-theme <name>).
    SetUiTheme(String),
    /// Open the `/connect` provider/key overlay (OpenCode parity).
    OpenConnect(Option<String>),
    /// Provider key was saved — rebuild LLM client with this provider id.
    ProviderConnected {
        provider: String,
        message: String,
    },
    /// Logout result message only (no provider rebuild required).
    ProviderLoggedOut(String),
    // --- Grok CLI TUI parity ---
    /// Enter plan mode with optional description (`/plan [desc]`).
    EnterPlan(String),
    /// View current plan (`/view-plan`).
    ViewPlan,
    /// Toggle always-approve mode (`/always-approve`).
    ToggleAlwaysApprove,
    /// Toggle multiline input (`/multiline`).
    ToggleMultiline,
    /// Show context usage (`/context`).
    ShowContext,
    /// Toggle compact UI mode (`/compact-mode`).
    ToggleCompactMode,
    /// Toggle message timestamps (`/timestamps`).
    ToggleTimestamps,
    /// Toggle vim-style scrollback keys (`/vim-mode`).
    ToggleVimMode,
    /// Show keyboard shortcuts overlay (`/shortcuts`).
    ShowShortcuts,
    /// Export conversation to file (`/export`).
    ExportConversation,
    /// Show full transcript in $PAGER (`/transcript` or Ctrl+T).
    ShowTranscript,
    // --- Codex TUI parity — status & footer ---
    /// Show status info — model, approval, tokens, cwd (`/status`).
    ShowStatus,
    /// Start a new session (`/new`).
    NewSession,
    // --- Codex TUI parity — input & interaction ---
    /// Run a local shell command (`!cmd` prefix).
    ShellCommand(String),
    /// Edit previous user message (Esc Esc backtrack).
    EditPreviousMessage,
    // --- Codex TUI parity — rendering polish ---
    /// Generate AGENTS.md scaffold (`/init`).
    InitProject,
    // --- Codex TUI parity — edge cases & polish ---
    /// Show pending git changes for review (`/review`).
    ReviewChanges,
    /// Display current keybindings (`/keymap`).
    ShowKeymap,
    /// Show or toggle sandbox mode (`/sandbox`).
    ShowSandbox,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use zerozero_exec::{Event, Item, ItemKind};

    #[test]
    fn test_app_new() {
        let app = App::new();
        assert!(app.messages.is_empty());
        assert!(app.streaming_text.is_empty());
        assert!(app.composer.input_buffer.is_empty());
        assert!(!app.is_streaming);
        assert!(!app.should_quit);
    }

    // --- : App ask mode state — full test body ---

    /// AC-3: /ask toggles app.ask_mode. Typing /ask produces ToggleAsk;
    /// the lib.rs handler flips app.ask_mode; a second /ask flips it back.
    #[test]
    fn test_app_ask_mode_toggle() {
        // Default ask_mode = false.
        let app = App::new();
        assert!(!app.ask_mode, "App::new() default ask_mode must be false");

        // Simulate typing "/ask" + Enter → ToggleAsk.
        let mut app = App::new();
        for ch in "/ask".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            action,
            KeyAction::Slash(SlashAction::ToggleAsk),
            "/ask must produce ToggleAsk"
        );

        // Simulate the lib.rs handler: flip app.ask_mode from ToggleAsk.
        if let KeyAction::Slash(SlashAction::ToggleAsk) = action {
            app.ask_mode = !app.ask_mode;
        }
        assert!(app.ask_mode, "ask_mode must be true after first toggle");

        // Same app: second /ask toggles back off.
        for ch in "/ask".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action2 = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            action2,
            KeyAction::Slash(SlashAction::ToggleAsk),
            "second /ask must also produce ToggleAsk"
        );
        if let KeyAction::Slash(SlashAction::ToggleAsk) = action2 {
            app.ask_mode = !app.ask_mode;
        }
        assert!(!app.ask_mode, "ask_mode must be false after second toggle");
    }

    // --- : App effort state — full test body ---
    // Written by B (Test Author, Round 7) based on + design only.

    /// AC-6: App default effort is Medium.
    /// app.effort. Typing /effort high produces SetEffort action.
    #[test]
    fn test_app_effort_state() {
        // Default effort = Medium (TUI user-facing default).
        let app = App::new();
        assert_eq!(
            app.effort,
            Effort::Medium,
            "App::new() default effort must be Medium"
        );

        // Simulate typing "/effort high" + Enter → SetEffort(High).
        let mut app = App::new();
        for ch in "/effort high".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            action,
            KeyAction::Slash(SlashAction::SetEffort(Effort::High)),
            "/effort high must produce SetEffort(High)"
        );

        // Simulate the lib.rs handler: update app.effort from SetEffort.
        if let KeyAction::Slash(SlashAction::SetEffort(effort)) = action {
            app.effort = effort;
        }
        assert_eq!(
            app.effort,
            Effort::High,
            "app.effort must be High after SetEffort"
        );

        // Invalid level → ShowMessage (not SetEffort), effort unchanged.
        let mut app = App::new();
        app.effort = Effort::Low; // set to something non-default
        for ch in "/effort xhigh".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::ShowMessage(msg)) => {
                assert!(
                    msg.contains("invalid") || msg.contains("xhigh"),
                    "invalid effort should produce error message, got: {msg}"
                );
            }
            other => panic!("Expected ShowMessage for invalid effort, got: {other:?}"),
        }
        assert_eq!(
            app.effort,
            Effort::Low,
            "effort must be unchanged when level is invalid"
        );

        // /effort with no arg → ShowMessage "Current effort: <level>".
        let mut app = App::new();
        app.effort = Effort::High;
        for ch in "/effort".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::ShowMessage(msg)) => {
                assert!(
                    msg.contains("Current effort"),
                    "/effort no-arg should show current effort, got: {msg}"
                );
                assert!(
                    msg.contains("high"),
                    "/effort no-arg should show effort level, got: {msg}"
                );
            }
            other => panic!("Expected ShowMessage for /effort no-arg, got: {other:?}"),
        }
    }

    #[test]
    fn test_handle_key_char_input() {
        let mut app = App::new();
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(app.handle_key(key), KeyAction::None);
        assert_eq!(app.composer.input_buffer, "h");
        assert_eq!(app.composer.cursor_pos, 1);
    }

    #[test]
    fn test_handle_key_backspace() {
        let mut app = App::new();
        app.set_input_buffer("hello".to_string());
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        app.handle_key(key);
        assert_eq!(app.composer.input_buffer, "hell");
        assert_eq!(app.composer.cursor_pos, 4);
    }

    #[test]
    fn test_handle_key_enter_submit() {
        let mut app = App::new();
        app.set_input_buffer("test prompt".to_string());
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.handle_key(key), KeyAction::Submit);
    }

    #[test]
    fn test_handle_key_enter_empty() {
        let mut app = App::new();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.handle_key(key), KeyAction::None);
    }

    #[test]
    fn test_handle_key_enter_while_streaming() {
        let mut app = App::new();
        app.set_input_buffer("test".to_string());
        app.is_streaming = true;
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.handle_key(key), KeyAction::None);
    }

    #[test]
    fn test_alt_enter_inserts_newline_not_submit() {
        // Alt+Enter must insert a newline into the composer instead of
        // submitting the prompt (multiline composer support).
        let mut app = App::new();
        app.set_input_buffer("line one".to_string());
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(app.handle_key(key), KeyAction::None);
        assert_eq!(app.composer.input_buffer, "line one\n");
        assert_eq!(app.composer.cursor_pos, "line one\n".len());
        // Plain Enter still submits.
        let key2 = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.handle_key(key2), KeyAction::Submit);
    }

    #[test]
    fn test_handle_key_quit_q() {
        let mut app = App::new();
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(app.handle_key(key), KeyAction::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn test_handle_key_quit_ctrl_c() {
        let mut app = App::new();
        app.is_streaming = true;
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.handle_key(key), KeyAction::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn test_handle_core_event_session_started() {
        let mut app = App::new();
        app.handle_core_event(Event::SessionStarted {
            session_id: "test-session".to_string(),
        });
        assert_eq!(app.session_id, "test-session");
    }

    #[test]
    fn test_apply_chat_event_background_thread() {
        let mut app = App::new();
        app.set_active_thread_id("root".to_string());
        app.apply_chat_event(
            &"root".to_string(),
            Event::ItemUpdated {
                item: zerozero_exec::ItemUpdated {
                    id: "i".to_string(),
                    text: "visible".to_string(),
                    kind: ItemUpdateKind::Message,
                },
            },
        );
        app.set_active_thread_id("child".to_string());
        assert!(app.streaming_text.is_empty());
        app.apply_chat_event(
            &"root".to_string(),
            Event::ItemUpdated {
                item: zerozero_exec::ItemUpdated {
                    id: "i".to_string(),
                    text: "bg".to_string(),
                    kind: ItemUpdateKind::Message,
                },
            },
        );
        app.set_active_thread_id("root".to_string());
        assert!(app.streaming_text.contains("visible"));
        assert!(app.streaming_text.contains("bg"));
    }

    #[test]
    fn test_handle_core_event_reasoning_updated() {
        let mut app = App::new();
        app.handle_core_event(Event::ItemUpdated {
            item: zerozero_exec::ItemUpdated {
                id: "item_0".to_string(),
                text: "hmm".to_string(),
                kind: ItemUpdateKind::Reasoning,
            },
        });
        assert_eq!(app.streaming_reasoning_text, "hmm");
        assert!(app.streaming_text.is_empty());
    }

    #[test]
    fn test_handle_core_event_item_updated() {
        let mut app = App::new();
        app.handle_core_event(Event::ItemUpdated {
            item: zerozero_exec::ItemUpdated {
                id: "item_0".to_string(),
                text: "Hello".to_string(),
                kind: ItemUpdateKind::Message,
            },
        });
        assert_eq!(app.streaming_text, "Hello");
    }

    #[test]
    fn test_handle_core_event_item_completed() {
        let mut app = App::new();
        app.streaming_text = "streaming text".to_string();
        app.handle_core_event(Event::ItemCompleted {
            item: Item {
                id: "item_0".to_string(),
                kind: ItemKind::AgentMessage,
                text: "final text".to_string(),
            },
        });
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].role, "assistant");
        assert_eq!(app.messages[0].content, "final text");
        assert!(app.streaming_text.is_empty());
    }

    #[test]
    fn test_handle_core_event_tool_started() {
        let mut app = App::new();
        app.handle_core_event(Event::ToolStarted {
            tool_call_id: "call_1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({}),
        });
        // Structured tool event: card-style display (not text in streaming_text).
        assert_eq!(app.tool_events.len(), 1);
        assert_eq!(app.tool_events[0].name, "bash");
        assert_eq!(app.tool_events[0].status, ToolStatus::Running);
    }

    #[test]
    fn test_handle_core_event_tool_completed() {
        let mut app = App::new();
        app.handle_core_event(Event::ToolCompleted {
            tool_call_id: "call_1".to_string(),
            tool_name: "bash".to_string(),
            result: "hello world".to_string(),
        });
        // Tool completed → Done status + preview.
        assert_eq!(app.tool_events.len(), 1);
        assert_eq!(app.tool_events[0].name, "bash");
        assert_eq!(app.tool_events[0].status, ToolStatus::Done);
        assert_eq!(app.tool_events[0].preview.as_deref(), Some("hello world"));
    }

    #[test]
    fn test_handle_core_event_turn_completed() {
        let mut app = App::new();
        app.is_streaming = true;
        app.handle_core_event(Event::TurnCompleted);
        assert!(!app.is_streaming);
    }

    #[test]
    fn test_handle_core_event_error() {
        let mut app = App::new();
        app.is_streaming = true;
        app.handle_core_event(Event::Error {
            message: "something went wrong".to_string(),
        });
        assert!(app.streaming_text.contains("[Error: something went wrong]"));
        assert!(!app.is_streaming);
    }

    #[test]
    fn test_display_text_empty() {
        let app = App::new();
        assert!(app.display_text().is_empty());
    }

    #[test]
    fn test_display_text_with_messages() {
        let mut app = App::new();
        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: "hi there".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        let text = app.display_text();
        assert!(text.contains("user: hello"));
        assert!(text.contains("assistant: hi there"));
    }

    #[test]
    fn test_display_text_with_streaming() {
        let mut app = App::new();
        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.streaming_text = "I am".to_string();
        let text = app.display_text();
        assert!(text.contains("user: hello"));
        assert!(text.contains("assistant: I am"));
    }

    #[test]
    fn test_tool_completed_write_file_sets_diff_view() {
        let mut app = App::new();
        app.handle_core_event(Event::ToolCompleted {
            tool_call_id: "call_1".to_string(),
            tool_name: "write_file".to_string(),
            result: "File written: src/main.rs".to_string(),
        });
        assert!(app.diff_view.is_some());
        assert!(app.show_diff);
        let diff = app.diff_view.as_ref().unwrap();
        assert_eq!(diff.file_path, "write_file");
        assert_eq!(diff.new_content, "File written: src/main.rs");
    }

    #[test]
    fn test_tool_completed_edit_file_sets_diff_view() {
        let mut app = App::new();
        app.handle_core_event(Event::ToolCompleted {
            tool_call_id: "call_2".to_string(),
            tool_name: "edit_file".to_string(),
            result: "File edited: lib.rs".to_string(),
        });
        assert!(app.diff_view.is_some());
        assert!(app.show_diff);
    }

    #[test]
    fn test_tool_completed_other_tool_no_diff_view() {
        let mut app = App::new();
        app.handle_core_event(Event::ToolCompleted {
            tool_call_id: "call_3".to_string(),
            tool_name: "bash".to_string(),
            result: "ok".to_string(),
        });
        assert!(app.diff_view.is_none());
        assert!(!app.show_diff);
    }

    #[test]
    fn test_handle_key_d_toggles_diff() {
        let mut app = App::new();
        app.diff_view = Some(crate::diff::DiffView::new("a.rs", "", "x"));
        app.show_diff = true;
        let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        app.handle_key(key);
        assert!(!app.show_diff);
        app.handle_key(key);
        assert!(app.show_diff);
    }

    #[test]
    fn test_handle_key_d_no_diff_view_no_toggle() {
        let mut app = App::new();
        let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        app.handle_key(key);
        assert!(!app.show_diff);
        // 'd' falls through to char input when no diff view present.
        assert_eq!(app.composer.input_buffer, "d");
    }

    #[test]
    fn test_handle_key_esc_closes_diff() {
        let mut app = App::new();
        app.diff_view = Some(crate::diff::DiffView::new("a.rs", "", "x"));
        app.show_diff = true;
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        app.handle_key(key);
        assert!(!app.show_diff);
        // diff_view itself remains.
        assert!(app.diff_view.is_some());
    }

    fn type_input(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    #[test]
    fn test_slash_help() {
        let mut app = App::new();
        type_input(&mut app, "/help");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.handle_key(key), KeyAction::Slash(SlashAction::ShowHelp));
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_quit() {
        let mut app = App::new();
        type_input(&mut app, "/quit");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.handle_key(key), KeyAction::Slash(SlashAction::Quit));
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_clear() {
        let mut app = App::new();
        type_input(&mut app, "/clear");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.handle_key(key),
            KeyAction::Slash(SlashAction::ClearChat)
        );
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_diff() {
        let mut app = App::new();
        type_input(&mut app, "/diff");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.handle_key(key),
            KeyAction::Slash(SlashAction::ToggleDiff)
        );
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_model() {
        let mut app = App::new();
        type_input(&mut app, "/model grok-4");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.handle_key(key),
            KeyAction::Slash(SlashAction::SetModel("grok-4".to_string()))
        );
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_unknown() {
        let mut app = App::new();
        type_input(&mut app, "/foobar");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = app.handle_key(key);
        assert!(matches!(
            action,
            KeyAction::Slash(SlashAction::ShowMessage(_))
        ));
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_does_not_submit_as_prompt() {
        let mut app = App::new();
        type_input(&mut app, "/help");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = app.handle_key(key);
        assert_ne!(action, KeyAction::Submit);
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_skills_empty() {
        let mut app = App::new();
        type_input(&mut app, "/skills");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = app.handle_key(key);
        match action {
            KeyAction::Slash(SlashAction::OpenSkillsBrowser) => {}
            other => panic!("expected OpenSkillsBrowser, got {other:?}"),
        }
        assert!(app.show_skills_browser);
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_invoke_skill() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".devin/skills");
        std::fs::create_dir_all(dir.join("ponytail")).unwrap();
        std::fs::write(
            dir.join("ponytail/SKILL.md"),
            "---\nname: ponytail\ndescription: d\n---\nbody",
        )
        .unwrap();
        let mut app = App::new();
        app.set_skill_dirs(vec![dir]);
        type_input(&mut app, "/ponytail ship it");
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            action,
            KeyAction::Slash(SlashAction::InvokeSkill(
                "ponytail".to_string(),
                "ship it".to_string()
            ))
        );
    }

    #[test]
    fn test_slash_skills_with_data() {
        let mut app = App::new();
        app.set_skills(vec!["my-skill".to_string(), "tdd".to_string()]);
        type_input(&mut app, "/skills");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = app.handle_key(key);
        match action {
            KeyAction::Slash(SlashAction::OpenSkillsBrowser) => {}
            other => panic!("expected OpenSkillsBrowser, got {other:?}"),
        }
        assert!(app.show_skills_browser);
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_plugins_empty() {
        let mut app = App::new();
        type_input(&mut app, "/plugins");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = app.handle_key(key);
        match action {
            KeyAction::Slash(SlashAction::ShowMessage(msg)) => {
                assert_eq!(msg, "No plugins loaded.");
            }
            other => panic!("expected ShowMessage, got {other:?}"),
        }
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_slash_plugins_with_data() {
        let mut app = App::new();
        app.set_plugins(vec!["my-plugin".to_string()]);
        type_input(&mut app, "/plugins");
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = app.handle_key(key);
        match action {
            KeyAction::Slash(SlashAction::ShowMessage(msg)) => {
                assert!(msg.contains("my-plugin"), "msg: {msg}");
                assert!(msg.contains("Loaded plugins (1)"), "msg: {msg}");
            }
            other => panic!("expected ShowMessage, got {other:?}"),
        }
        assert!(app.composer.input_buffer.is_empty());
    }

    // --- : /model slash command — test stubs ---
    // Written by A (Coder, Round 6) — skeleton only, NO assert bodies.
    // B (Test Author, Round 7) will fill in assert bodies based on.

    /// AC-1: App default model is empty string.
    #[test]
    fn test_app_model_default_empty() {
        let app = App::new();
        assert_eq!(
            app.model, "",
            "App::new() default model must be empty string"
        );
    }

    /// AC-1: /model <name> produces SlashAction::SetModel(name).
    #[test]
    fn test_slash_action_set_model() {
        // Simulate typing "/model grok-4.3" + Enter → SetModel("grok-4.3").
        let mut app = App::new();
        for ch in "/model grok-4.3".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            action,
            KeyAction::Slash(SlashAction::SetModel("grok-4.3".to_string())),
            "/model grok-4.3 must produce SetModel(\"grok-4.3\")"
        );
        assert!(
            app.composer.input_buffer.is_empty(),
            "input buffer must be cleared after slash command"
        );

        // Simulate the lib.rs handler: update app.model from SetModel.
        if let KeyAction::Slash(SlashAction::SetModel(name)) = action {
            app.model = name;
        }
        assert_eq!(
            app.model, "grok-4.3",
            "app.model must be updated to the new model name"
        );

        // Different model name to ensure the carried value is dynamic
        // (mutation-resistant — not a hardcoded constant).
        let mut app2 = App::new();
        for ch in "/model o3-mini".chars() {
            app2.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action2 = app2.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            action2,
            KeyAction::Slash(SlashAction::SetModel("o3-mini".to_string())),
            "/model o3-mini must produce SetModel(\"o3-mini\")"
        );
    }

    /// AC-2: /model with no arg shows current model via ShowMessage.
    #[test]
    fn test_model_no_arg_shows_current() {
        // 3-tier picker: /model with no arg → OpenModelPicker (not ShowMessage).
        let mut app = App::new();
        app.model = "grok-4.3".to_string();
        for ch in "/model".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::OpenModelPicker) => {
                // Expected: picker opens.
            }
            other => panic!("Expected OpenModelPicker for /model no-arg, got: {other:?}"),
        }
        assert!(
            app.composer.input_buffer.is_empty(),
            "input buffer must be cleared after slash command"
        );

        // Empty model → still opens picker (detects provider from empty model).
        let mut app2 = App::new();
        for ch in "/model".chars() {
            app2.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action2 = app2.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action2 {
            KeyAction::Slash(SlashAction::OpenModelPicker) => {}
            other => {
                panic!("Expected OpenModelPicker for /model no-arg (empty model), got: {other:?}")
            }
        }
    }

    // --- TUI enhancement batch tests ---

    #[test]
    fn test_esc_cancels_streaming() {
        let mut app = App::new();
        app.is_streaming = true;
        app.streaming_text = "partial".to_string();
        let action = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::CancelStreaming);
        assert!(!app.is_streaming, "is_streaming must be false after Esc");
        assert!(
            app.streaming_text.is_empty(),
            "streaming_text must be cleared"
        );
    }

    #[test]
    fn test_esc_closes_overlay_when_not_streaming() {
        let mut app = App::new();
        app.show_diff = true;
        let action = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::None);
        assert!(!app.show_diff, "diff must be closed after Esc");
    }

    #[test]
    fn test_tab_queues_input_while_streaming() {
        let mut app = App::new();
        app.is_streaming = true;
        app.set_input_buffer("next prompt".to_string());
        let action = app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::QueueInput);
        assert_eq!(app.composer.queued_input.as_deref(), Some("next prompt"));
        assert!(
            app.composer.input_buffer.is_empty(),
            "input buffer must be cleared after queue"
        );
    }

    #[test]
    fn test_tab_does_not_queue_empty_input() {
        let mut app = App::new();
        app.is_streaming = true;
        app.composer.input_buffer.clear();
        let action = app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::None);
        assert!(app.composer.queued_input.is_none());
    }

    #[test]
    fn test_ctrl_l_clears_screen() {
        let mut app = App::new();
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::ClearScreen);
        assert!(app.needs_clear, "needs_clear must be set");
    }

    #[test]
    fn test_ctrl_o_copies_output() {
        let mut app = App::new();
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::CopyOutput);
    }

    #[test]
    fn test_ctrl_r_enters_history_search() {
        let mut app = App::new();
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::None);
        assert!(app.show_history_search, "history search must be active");
    }

    #[test]
    fn test_ctrl_e_opens_editor() {
        let mut app = App::new();
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::OpenEditor);
    }

    #[test]
    fn test_draft_history_navigation() {
        let mut app = App::new();
        app.composer.draft_history = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];

        // Up → go to last entry ("third").
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.composer.input_buffer, "third");

        // Up again → "second".
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.composer.input_buffer, "second");

        // Down → back to "third".
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.composer.input_buffer, "third");

        // Down again → past end → clear.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(app.composer.input_buffer.is_empty());
        assert!(app.composer.draft_history_index.is_none());
    }

    #[test]
    fn test_record_prompt_avoids_duplicates() {
        let mut app = App::new();
        app.record_prompt("hello");
        app.record_prompt("hello"); // duplicate
        assert_eq!(
            app.prompt_history.len(),
            1,
            "consecutive duplicates must be skipped"
        );
        app.record_prompt("world");
        assert_eq!(app.prompt_history.len(), 2);
    }

    #[test]
    fn test_record_prompt_ignores_slash_commands() {
        let mut app = App::new();
        app.record_prompt("/help");
        assert!(
            app.prompt_history.is_empty(),
            "slash commands must not be recorded"
        );
        assert!(app.composer.draft_history.is_empty());
    }

    #[test]
    fn test_latest_assistant_output() {
        let mut app = App::new();
        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "hi".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: "hello there".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        assert_eq!(
            app.latest_assistant_output().as_deref(),
            Some("hello there")
        );
    }

    #[test]
    fn test_latest_assistant_output_none() {
        let app = App::new();
        assert!(app.latest_assistant_output().is_none());
    }

    #[test]
    fn test_filtered_prompt_history() {
        let mut app = App::new();
        app.prompt_history = vec![
            "fix bug".to_string(),
            "add feature".to_string(),
            "fix tests".to_string(),
        ];
        app.history_search_query = "fix".to_string();
        let filtered = app.filtered_prompt_history();
        assert_eq!(filtered.len(), 2);
        // Reversed order — most recent first.
        assert_eq!(filtered[0], "fix tests");
        assert_eq!(filtered[1], "fix bug");
    }

    #[test]
    fn test_connect_slash_opens_overlay() {
        let mut app = App::new();
        for ch in "/connect".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::OpenConnect(None)) => {}
            other => panic!("expected OpenConnect(None), got {other:?}"),
        }
        // Simulate event-loop handling.
        app.open_connect(None);
        assert!(app.show_connect);
        assert_eq!(app.connect_stage, 0);
    }

    #[test]
    fn test_connect_key_entry_saves() {
        let dir = std::env::temp_dir().join(format!(
            "zz-tui-connect-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("auth.json");
        unsafe {
            std::env::set_var("ZZ_AUTH_PATH", &path);
        }
        let mut app = App::new();
        app.open_connect(Some("xai"));
        assert!(app.show_connect);
        assert_eq!(app.connect_stage, 1);
        assert_eq!(app.connect_provider, "xai");
        for ch in "xai-secret-key".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(app.connect_key_buffer, "xai-secret-key");
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::ProviderConnected { provider, message }) => {
                assert_eq!(provider, "xai");
                assert!(message.contains("Saved"), "{message}");
            }
            other => panic!("expected ProviderConnected, got {other:?}"),
        }
        assert!(!app.show_connect);
        assert!(app.connect_key_buffer.is_empty());
        assert!(path.exists());
        unsafe {
            std::env::remove_var("ZZ_AUTH_PATH");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slash_palette_down_preserves_index_while_typing_filter() {
        // Typing `/` opens palette; ↓ moves selection; typing more letters must
        // NOT always force index back to 0 (old bug — felt "half-broken").
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(app.composer.show_slash_palette);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let idx_after_nav = app.composer.slash_menu_index;
        assert!(idx_after_nav >= 1, "expected ↓ to move selection");
        // Typing a letter that keeps multiple matches should clamp, not hard-reset
        // when the list still has enough rows.
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        // Index must remain valid; may clamp if filter shrinks list.
        let n = crate::slash_menu::ranked_len(&app.skill_slash_entries, &app.composer.input_buffer);
        if n > 0 {
            assert!(app.composer.slash_menu_index < n);
        }
    }

    #[test]
    fn test_slash_palette_closes_after_space() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(app.composer.show_slash_palette);
        for ch in "help".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            !app.composer.show_slash_palette,
            "palette should close once args phase starts"
        );
    }

    #[test]
    fn test_slash_tab_completes_selected() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(
            app.composer.input_buffer.starts_with("/help")
                || app.composer.input_buffer.starts_with("/"),
            "tab should complete toward a command, got {}",
            app.composer.input_buffer
        );
        assert!(
            app.composer.input_buffer.ends_with(' ') || !app.composer.show_slash_palette,
            "completion ends token phase"
        );
    }

    #[test]
    fn test_chat_scroll_keys() {
        let mut app = App::new();
        assert_eq!(app.chat_scroll, 0);
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.chat_scroll, 8);
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.chat_scroll, 16);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.chat_scroll, 8);
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(app.chat_scroll, 0);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(app.chat_scroll, u16::MAX);
        app.scroll_chat_to_bottom();
        assert_eq!(app.chat_scroll, 0);
    }

    #[test]
    fn test_spinner_char_cycles() {
        let mut app = App::new();
        let c0 = app.spinner_char();
        app.tick_spinner();
        let c1 = app.spinner_char();
        assert_ne!(c0, c1, "spinner must advance on tick");
    }

    #[test]
    fn test_key_release_does_not_insert_char() {
        // Windows emits Press + Release; Release must not double-insert.
        let mut app = App::new();
        let press = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(press.is_press());
        app.handle_key(press);
        assert_eq!(app.composer.input_buffer, "a");

        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_key(release);
        assert_eq!(
            app.composer.input_buffer, "a",
            "KeyEventKind::Release must not insert a second character"
        );
        assert_eq!(app.composer.cursor_pos, 1);
    }

    #[test]
    fn test_key_repeat_does_not_insert_char() {
        // Only Press is accepted — kitty Repeat must not type.
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let mut repeat = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        app.handle_key(repeat);
        assert_eq!(app.composer.input_buffer, "x");
    }

    #[test]
    fn test_only_press_kind_inserts() {
        let mut app = App::new();
        for kind in [
            KeyEventKind::Release,
            KeyEventKind::Repeat,
            KeyEventKind::Press,
        ] {
            let mut key = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
            key.kind = kind;
            app.handle_key(key);
        }
        // Only the Press should have inserted.
        assert_eq!(app.composer.input_buffer, "z");
    }

    #[test]
    fn test_copy_slash_command() {
        let mut app = App::new();
        for ch in "/copy".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::CopyOutput) => {}
            other => panic!("Expected CopyOutput for /copy, got: {other:?}"),
        }
    }

    #[test]
    fn test_theme_slash_command_no_arg_opens_picker() {
        let mut app = App::new();
        for ch in "/theme".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::OpenThemePicker) => {}
            other => panic!("Expected OpenThemePicker for /theme (no arg), got: {other:?}"),
        }
    }

    #[test]
    fn test_theme_slash_command_with_arg() {
        let mut app = App::new();
        for ch in "/theme base16-ocean.dark".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::SetTheme(name)) => {
                assert_eq!(name, "base16-ocean.dark");
            }
            other => panic!("Expected SetTheme for /theme <name>, got: {other:?}"),
        }
    }

    // --- Grok CLI TUI parity tests ---

    #[test]
    fn test_session_mode_default_is_normal() {
        let app = App::new();
        assert_eq!(app.session_mode, SessionMode::Normal);
        assert!(!app.always_approve);
        assert!(!app.multiline);
        assert!(!app.compact_mode);
        assert!(!app.show_timestamps);
        assert!(!app.vim_mode);
        assert!(!app.show_shortcuts_overlay);
    }

    #[test]
    fn test_session_mode_cycle() {
        assert_eq!(SessionMode::Normal.next(), SessionMode::Plan);
        assert_eq!(SessionMode::Plan.next(), SessionMode::AlwaysApprove);
        assert_eq!(SessionMode::AlwaysApprove.next(), SessionMode::Normal);
    }

    #[test]
    fn test_session_mode_label() {
        assert_eq!(SessionMode::Normal.label(), "normal");
        assert_eq!(SessionMode::Plan.label(), "plan");
        assert_eq!(SessionMode::AlwaysApprove.label(), "approve");
    }

    #[test]
    fn test_shift_tab_cycles_mode() {
        let mut app = App::new();
        assert_eq!(app.session_mode, SessionMode::Normal);

        // Shift+Tab → Plan
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.session_mode, SessionMode::Plan);
        assert!(!app.always_approve);

        // Shift+Tab → AlwaysApprove
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.session_mode, SessionMode::AlwaysApprove);
        assert!(app.always_approve);

        // Shift+Tab → Normal
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.session_mode, SessionMode::Normal);
        assert!(!app.always_approve);
    }

    #[test]
    fn test_ctrl_m_toggles_multiline() {
        let mut app = App::new();
        assert!(!app.multiline);

        // Ctrl+M → multiline ON
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::ToggleMultiline);
        assert!(app.multiline);

        // Ctrl+M → multiline OFF
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::ToggleMultiline);
        assert!(!app.multiline);
    }

    #[test]
    fn test_ctrl_dot_opens_shortcuts() {
        let mut app = App::new();
        assert!(!app.show_shortcuts_overlay);

        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL));
        assert!(app.show_shortcuts_overlay);

        // Esc closes it.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_shortcuts_overlay);
    }

    #[test]
    fn test_plan_slash_command() {
        let mut app = App::new();
        for ch in "/plan refactor the auth module".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::EnterPlan(desc)) => {
                assert_eq!(desc, "refactor the auth module");
            }
            other => panic!("Expected EnterPlan, got: {other:?}"),
        }
    }

    #[test]
    fn test_plan_slash_no_arg() {
        let mut app = App::new();
        for ch in "/plan".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::EnterPlan(desc)) => {
                assert!(desc.is_empty());
            }
            other => panic!("Expected EnterPlan with empty desc, got: {other:?}"),
        }
    }

    #[test]
    fn test_view_plan_slash_command() {
        let mut app = App::new();
        for ch in "/view-plan".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ViewPlan));
    }

    #[test]
    fn test_always_approve_slash_command() {
        let mut app = App::new();
        for ch in "/always-approve".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ToggleAlwaysApprove));
    }

    #[test]
    fn test_multiline_slash_command() {
        let mut app = App::new();
        for ch in "/multiline".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ToggleMultiline));
    }

    #[test]
    fn test_ml_alias_slash_command() {
        let mut app = App::new();
        for ch in "/ml".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ToggleMultiline));
    }

    #[test]
    fn test_context_slash_command() {
        let mut app = App::new();
        for ch in "/context".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ShowContext));
    }

    #[test]
    fn test_compact_mode_slash_command() {
        let mut app = App::new();
        for ch in "/compact-mode".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ToggleCompactMode));
    }

    #[test]
    fn test_timestamps_slash_command() {
        let mut app = App::new();
        for ch in "/timestamps".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ToggleTimestamps));
    }

    #[test]
    fn test_vim_mode_slash_command() {
        let mut app = App::new();
        for ch in "/vim-mode".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ToggleVimMode));
    }

    #[test]
    fn test_shortcuts_slash_command() {
        let mut app = App::new();
        for ch in "/shortcuts".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ShowShortcuts));
    }

    #[test]
    fn test_export_slash_command() {
        let mut app = App::new();
        for ch in "/export".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ExportConversation));
    }

    #[test]
    fn test_transcript_slash_command() {
        let mut app = App::new();
        for ch in "/transcript".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ShowTranscript));
    }

    #[test]
    fn test_multiline_enter_inserts_newline() {
        let mut app = App::new();
        app.multiline = true;
        // Type "hello"
        for ch in "hello".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        // Enter should insert newline, not submit
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::None);
        assert!(app.composer.input_buffer.contains('\n'));
        assert_eq!(app.composer.input_buffer, "hello\n");
    }

    #[test]
    fn test_multiline_ctrl_enter_submits() {
        let mut app = App::new();
        app.multiline = true;
        // Type "hello"
        for ch in "hello".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        // Ctrl+Enter should submit (fall through to submit handler)
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::Submit);
    }

    #[test]
    fn test_vim_mode_j_k_scroll() {
        let mut app = App::new();
        app.vim_mode = true;
        assert_eq!(app.chat_scroll, 0);

        // 'k' scrolls up (older content)
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(app.chat_scroll > 0);

        // 'j' scrolls down (newer content)
        let prev = app.chat_scroll;
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(app.chat_scroll < prev);
    }

    #[test]
    fn test_vim_mode_g_jump_to_top() {
        let mut app = App::new();
        app.vim_mode = true;
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.chat_scroll, u16::MAX);
    }

    #[test]
    fn test_vim_mode_upper_g_jump_to_bottom() {
        let mut app = App::new();
        app.vim_mode = true;
        app.chat_scroll = 100;
        app.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert_eq!(app.chat_scroll, 0);
    }

    #[test]
    fn test_export_conversation_format() {
        let mut app = App::new();
        app.session_id = "test-123".to_string();
        app.messages.push(zerozero_llm::ChatMessage {
            role: "user".to_string(),
            content: "Hello".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.messages.push(zerozero_llm::ChatMessage {
            role: "assistant".to_string(),
            content: "Hi there!".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        let exported = app.export_conversation();
        assert!(exported.contains("test-123"));
        assert!(exported.contains("You: Hello"));
        assert!(exported.contains("Assistant: Hi there!"));
    }

    #[test]
    fn test_record_message_timestamp() {
        let mut app = App::new();
        assert!(app.message_timestamps.is_empty());

        app.messages.push(zerozero_llm::ChatMessage {
            role: "user".to_string(),
            content: "test".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.record_message_timestamp();
        assert_eq!(app.message_timestamps.len(), 1);
    }

    #[test]
    fn test_format_timestamp_empty_when_disabled() {
        let mut app = App::new();
        app.messages.push(zerozero_llm::ChatMessage {
            role: "user".to_string(),
            content: "test".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.record_message_timestamp();
        // show_timestamps is false by default → empty string
        assert_eq!(app.format_timestamp(0), String::new());
    }

    #[test]
    fn test_format_timestamp_shown_when_enabled() {
        let mut app = App::new();
        app.show_timestamps = true;
        app.messages.push(zerozero_llm::ChatMessage {
            role: "user".to_string(),
            content: "test".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.record_message_timestamp();
        let ts = app.format_timestamp(0);
        assert!(
            ts.starts_with('['),
            "timestamp should start with '[', got: {ts}"
        );
        assert!(
            ts.ends_with("] "),
            "timestamp should end with '] ', got: {ts}"
        );
    }

    // --- Codex TUI parity — status & footer tests ---

    #[test]
    fn test_footer_mode_default_is_idle() {
        let app = App::new();
        assert_eq!(app.footer_mode, FooterMode::Idle);
    }

    #[test]
    fn test_footer_mode_idle_hint() {
        let mode = FooterMode::Idle;
        assert!(mode.hint_text().contains("Enter to send"));
        assert!(mode.hint_text().contains("Ctrl+."));
    }

    #[test]
    fn test_footer_mode_running_hint() {
        let mode = FooterMode::Running;
        assert!(mode.hint_text().contains("Esc to interrupt"));
        assert!(mode.hint_text().contains("Tab to queue"));
    }

    #[test]
    fn test_footer_mode_queued_hint() {
        let mode = FooterMode::Queued;
        assert!(mode.hint_text().contains("Queued"));
        assert!(mode.hint_text().contains("turn completes"));
    }

    #[test]
    fn test_status_indicator_default_idle() {
        let app = App::new();
        assert!(!app.status_indicator.is_streaming);
        assert!(app.status_indicator.start_time.is_none());
    }

    #[test]
    fn test_status_indicator_start_stop() {
        let mut app = App::new();
        app.status_indicator.start();
        assert!(app.status_indicator.is_streaming);
        assert!(app.status_indicator.start_time.is_some());

        app.status_indicator.stop();
        assert!(!app.status_indicator.is_streaming);
        assert!(app.status_indicator.start_time.is_none());
    }

    #[test]
    fn test_status_indicator_text_idle() {
        let app = App::new();
        assert_eq!(app.status_indicator.status_text('⏺'), "");
    }

    #[test]
    fn test_status_indicator_text_streaming() {
        let mut app = App::new();
        app.status_indicator.start();
        let text = app.status_indicator.status_text('⏺');
        assert!(text.contains("Working"));
        assert!(text.contains("esc to interrupt"));
        assert!(text.contains("⏺"));
    }

    #[test]
    fn test_ctrl_j_inserts_newline() {
        let mut app = App::new();
        for ch in "hello".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::None);
        assert!(app.composer.input_buffer.contains('\n'));
        assert_eq!(app.composer.input_buffer, "hello\n");
    }

    #[test]
    fn test_status_slash_command() {
        let mut app = App::new();
        for ch in "/status".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ShowStatus));
    }

    #[test]
    fn test_new_slash_command() {
        let mut app = App::new();
        for ch in "/new".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::NewSession));
    }

    // --- Codex TUI parity — input & interaction tests ---

    #[test]
    fn test_ctrl_t_opens_transcript() {
        let mut app = App::new();
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(action, KeyAction::Slash(SlashAction::ShowTranscript));
    }

    #[test]
    fn test_shell_mode_prefix() {
        let mut app = App::new();
        for ch in "!ls".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            action,
            KeyAction::Slash(SlashAction::ShellCommand("ls".to_string()))
        );
        assert!(app.composer.input_buffer.is_empty());
    }

    #[test]
    fn test_shell_mode_empty_command() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::None);
    }

    #[test]
    fn test_at_mention_triggers_file_search() {
        let mut app = App::new();
        for ch in "@read".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let _ = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.show_file_search);
        assert_eq!(app.file_search_query, "read");
    }

    #[test]
    fn test_esc_esc_backtrack_empty_composer() {
        let mut app = App::new();
        app.messages.push(zerozero_llm::ChatMessage {
            role: "user".to_string(),
            content: "hello world".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.record_message_timestamp();

        // First Esc — sets last_esc_press
        let action1 = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action1, KeyAction::None);
        assert!(app.last_esc_press.is_some());

        // Second Esc (immediately) — triggers backtrack
        let action2 = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action2, KeyAction::Slash(SlashAction::EditPreviousMessage));
    }

    #[test]
    fn test_esc_single_does_not_backtrack() {
        let mut app = App::new();
        let action = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::None);
        assert!(app.last_esc_press.is_some());
        // No second Esc → no backtrack
    }

    #[test]
    fn test_esc_esc_does_not_trigger_with_text() {
        let mut app = App::new();
        for ch in "hello".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::None);
        // last_esc_press should be None because composer is not empty
        assert!(app.last_esc_press.is_none());
    }

    #[test]
    fn test_fuzzy_find_files_empty_query() {
        let app = App::new();
        assert!(app.fuzzy_find_files("").is_empty());
    }

    #[test]
    fn test_fuzzy_find_files_finds_cargo_toml() {
        let app = App::new();
        let results = app.fuzzy_find_files("cargo");
        assert!(
            results.iter().any(|r| r.contains("Cargo")),
            "should find Cargo.toml: {results:?}"
        );
    }

    // --- Codex TUI parity — rendering polish tests ---

    #[test]
    fn test_init_slash_command() {
        let mut app = App::new();
        for ch in "/init".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::InitProject));
    }

    #[test]
    fn test_collapse_tool_toggle() {
        let mut app = App::new();
        app.tool_events.push(crate::app::ToolEventDisplay {
            name: "read_file".to_string(),
            status: crate::app::ToolStatus::Done,
            preview: Some("file contents".to_string()),
        });
        assert!(app.collapsed_tools.is_empty());

        // Press `t` — should collapse the last tool
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(action, KeyAction::None);
        assert!(app.collapsed_tools.contains(&0));

        // Press `t` again — should uncollapse
        let action2 = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(action2, KeyAction::None);
        assert!(!app.collapsed_tools.contains(&0));
    }

    #[test]
    fn test_collapse_tool_no_tools() {
        let mut app = App::new();
        // No tool events — `t` should not crash or toggle
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(action, KeyAction::None);
        assert!(app.collapsed_tools.is_empty());
    }

    #[test]
    fn test_collapse_tool_ignored_while_streaming() {
        let mut app = App::new();
        app.is_streaming = true;
        app.tool_events.push(crate::app::ToolEventDisplay {
            name: "read_file".to_string(),
            status: crate::app::ToolStatus::Running,
            preview: None,
        });
        let _action = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        // While streaming, `t` should not toggle collapse
        assert!(app.collapsed_tools.is_empty());
    }

    #[test]
    fn test_collapse_tool_ignored_with_text_in_composer() {
        let mut app = App::new();
        app.tool_events.push(crate::app::ToolEventDisplay {
            name: "read_file".to_string(),
            status: crate::app::ToolStatus::Done,
            preview: None,
        });
        for ch in "hello".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let _action = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(app.collapsed_tools.is_empty());
    }

    // --- Codex TUI parity — edge cases & polish tests ---

    #[test]
    fn test_review_slash_command() {
        let mut app = App::new();
        for ch in "/review".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ReviewChanges));
    }

    #[test]
    fn test_keymap_slash_command() {
        let mut app = App::new();
        for ch in "/keymap".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ShowKeymap));
    }

    #[test]
    fn test_sandbox_slash_command_no_arg() {
        let mut app = App::new();
        for ch in "/sandbox".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, KeyAction::Slash(SlashAction::ShowSandbox));
    }

    #[test]
    fn test_sandbox_slash_command_with_arg() {
        let mut app = App::new();
        for ch in "/sandbox read-only".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            KeyAction::Slash(SlashAction::ShowMessage(msg)) => {
                assert!(msg.contains("read-only"));
            }
            other => panic!("expected ShowMessage, got {other:?}"),
        }
    }
}
