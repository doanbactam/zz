//! Slash command parser for the TUI.
//!
//! When the user types input starting with `/`, it is treated as a slash
//! command rather than a prompt.

use zerozero_skills::{SkillRegistry, SkillSlashEntry};

/// A parsed slash command.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashCommand {
    /// `/help` — show help text.
    Help,
    /// `/clear` — clear conversation.
    Clear,
    /// `/quit` — quit (same as `q`).
    Quit,
    /// `/model <name>` — change model (display only).
    Model(String),
    /// `/provider <name>` — display current provider info.
    Provider(String),
    /// `/diff` — toggle diff view.
    Diff,
    /// `/sandbox <mode>` — display sandbox mode info.
    Sandbox(String),
    /// `/sessions` — display sessions info.
    Sessions,
    /// `/skills` — list loaded skills.
    Skills,
    /// `/plugins` — list loaded plugins.
    Plugins,
    /// `/reload` — reload skills from disk.
    Reload,
    /// `/reload-plugins` — reload plugins from disk.
    ReloadPlugins,
    /// `/diff-sessions <id-a> <id-b>` — compare two sessions.
    DiffSessions(String, String),
    /// `/agent` — switch active agent thread .
    Agent,
    /// `/effort <level>` — set reasoning effort).
    Effort(String),
    /// `/ask` — toggle ask mode (prompt before every tool call)..
    Ask,
    /// `/rewind <path>` — restore a file from its shadow snapshot.B.
    Rewind(String),
    /// `/find <query>` — fuzzy file-path search over the cwd..
    Find(String),
    /// `/compact` — manually trigger token-budget compaction .
    Compact,
    /// `/image <path>` — attach an image to the next message .
    Image(String),
    /// `/unimage` — clear all pending image attachments .
    Unimage,
    /// `/copy` — copy latest assistant output to clipboard .
    Copy,
    /// `/theme <name>` — set syntax highlighting theme or open picker .
    Theme(String),
    /// `/ui-theme <name>` — switch the UI chrome palette (dark/light).
    UiTheme(String),
    /// `/connect` / `/auth` — OpenCode-style provider API key entry in TUI.
    /// Optional arg is a provider id to skip the picker (`/connect xai`).
    Connect(String),
    /// `/logout [provider]` — remove a stored API key from auth.json.
    Logout(String),
    /// `/plan [desc]` — enter plan mode (Grok CLI parity).
    Plan(String),
    /// `/view-plan` — show current plan file .
    ViewPlan,
    /// `/always-approve` — toggle always-approve mode .
    AlwaysApprove,
    /// `/multiline` — toggle multiline input mode .
    Multiline,
    /// `/context` — view context usage .
    Context,
    /// `/compact-mode` — toggle denser UI layout .
    CompactMode,
    /// `/timestamps` — toggle message timestamps .
    Timestamps,
    /// `/vim-mode` — toggle vim-style scrollback keys .
    VimMode,
    /// `/shortcuts` — show keyboard shortcuts overlay .
    Shortcuts,
    /// `/export` — export conversation to file .
    Export,
    /// `/transcript` — view full transcript in $PAGER .
    Transcript,
    /// `/status` — show model, approval, tokens, working dir .
    Status,
    /// `/new` — start a fresh session .
    New,
    /// `/init` — generate AGENTS.md scaffold .
    Init,
    /// `/review` — show pending git changes for review .
    Review,
    /// `/keymap` — display current keybindings .
    Keymap,
    /// Unrecognized built-in — command token + optional args (may match a skill).
    Unknown(String, String),
}

/// Built-in slash names (skills with the same bare name are shadowed; use `project:name`).
pub const BUILTIN_SLASH_NAMES: &[&str] = &[
    "help",
    "clear",
    "quit",
    "exit",
    "model",
    "provider",
    "diff",
    "sandbox",
    "sessions",
    "skills",
    "plugins",
    "reload",
    "reload-plugins",
    "diff-sessions",
    "agent",
    "effort",
    "ask",
    "rewind",
    "find",
    "compact",
    "image",
    "unimage",
    "copy",
    "theme",
    "ui-theme",
    "connect",
    "auth",
    "logout",
    "disconnect",
    "plan",
    "view-plan",
    "always-approve",
    "multiline",
    "ml",
    "context",
    "compact-mode",
    "timestamps",
    "vim-mode",
    "shortcuts",
    "export",
    "transcript",
    "status",
    "new",
    "init",
    "review",
    "keymap",
    "sandbox",
];

pub fn is_builtin_slash(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    BUILTIN_SLASH_NAMES.iter().any(|n| *n == c)
}

/// Parse a line of input into a `SlashCommand`.
///
/// Returns `None` if the input does not start with `/`.
/// Returns `Some(SlashCommand::Unknown(..))` if the command is not recognized.
/// Matching is case-insensitive.
pub fn parse(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let body = &trimmed[1..];
    let mut parts = body.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").to_lowercase();
    let arg = parts.next().map(|s| s.trim()).filter(|s| !s.is_empty());

    Some(match cmd.as_str() {
        "help" => SlashCommand::Help,
        "clear" => SlashCommand::Clear,
        "quit" | "exit" => SlashCommand::Quit,
        "diff" => SlashCommand::Diff,
        "sessions" => SlashCommand::Sessions,
        "skills" => SlashCommand::Skills,
        "plugins" => SlashCommand::Plugins,
        "reload" => SlashCommand::Reload,
        "reload-plugins" => SlashCommand::ReloadPlugins,
        "diff-sessions" => {
            let args = arg.unwrap_or_default();
            let mut parts = args.splitn(2, char::is_whitespace);
            let a = parts.next().unwrap_or("").to_string();
            let b = parts.next().unwrap_or("").to_string();
            SlashCommand::DiffSessions(a, b)
        }
        "model" => SlashCommand::Model(arg.unwrap_or_default().to_string()),
        "provider" => SlashCommand::Provider(arg.unwrap_or_default().to_string()),
        "sandbox" => SlashCommand::Sandbox(arg.unwrap_or_default().to_string()),
        "agent" => SlashCommand::Agent,
        "effort" => SlashCommand::Effort(arg.unwrap_or_default().to_string()),
        "ask" => SlashCommand::Ask,
        "rewind" => SlashCommand::Rewind(arg.unwrap_or_default().to_string()),
        "find" => SlashCommand::Find(arg.unwrap_or_default().to_string()),
        "compact" => SlashCommand::Compact,
        "image" => SlashCommand::Image(arg.unwrap_or_default().to_string()),
        "unimage" => SlashCommand::Unimage,
        "copy" => SlashCommand::Copy,
        "theme" => SlashCommand::Theme(arg.unwrap_or_default().to_string()),
        "ui-theme" => SlashCommand::UiTheme(arg.unwrap_or_default().to_string()),
        "connect" | "auth" => SlashCommand::Connect(arg.unwrap_or_default().to_string()),
        "logout" | "disconnect" => SlashCommand::Logout(arg.unwrap_or_default().to_string()),
        "plan" => SlashCommand::Plan(arg.unwrap_or_default().to_string()),
        "view-plan" => SlashCommand::ViewPlan,
        "always-approve" => SlashCommand::AlwaysApprove,
        "multiline" | "ml" => SlashCommand::Multiline,
        "context" => SlashCommand::Context,
        "compact-mode" => SlashCommand::CompactMode,
        "timestamps" => SlashCommand::Timestamps,
        "vim-mode" => SlashCommand::VimMode,
        "shortcuts" => SlashCommand::Shortcuts,
        "export" => SlashCommand::Export,
        "transcript" => SlashCommand::Transcript,
        "status" => SlashCommand::Status,
        "new" => SlashCommand::New,
        "init" => SlashCommand::Init,
        "review" => SlashCommand::Review,
        "keymap" => SlashCommand::Keymap,
        other => SlashCommand::Unknown(other.to_string(), arg.unwrap_or_default().to_string()),
    })
}

/// Resolve `/<token> <args>` against loaded skills (registry).
pub fn match_skill_registry(
    cmd_token: &str,
    args: &str,
    registry: &SkillRegistry,
) -> Option<(String, String)> {
    if is_builtin_slash(cmd_token) {
        return None;
    }
    let skill = registry.resolve_slash_token(cmd_token)?;
    Some((skill.name.clone(), args.to_string()))
}

/// True when Enter should execute the slash line instead of applying palette completion.
pub fn is_slash_submittable(
    input: &str,
    registry: &SkillRegistry,
    entries: &[SkillSlashEntry],
) -> bool {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return false;
    }
    let body = trimmed.strip_prefix('/').unwrap_or("").trim_start();
    if body.is_empty() {
        return false;
    }
    if parse_skill_chain_line(trimmed, registry).is_some() {
        return true;
    }
    let (token, _) = split_first_token(body);
    if token.is_empty() {
        return false;
    }
    match parse(trimmed) {
        None => false,
        Some(SlashCommand::Unknown(cmd, _)) => {
            !is_partial_builtin_token(&cmd) && !is_partial_menu_token(&cmd, entries)
        }
        Some(_) => true,
    }
}

fn is_partial_builtin_token(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    if is_builtin_slash(&c) {
        return false;
    }
    BUILTIN_SLASH_NAMES
        .iter()
        .any(|name| name.starts_with(&c) && *name != c)
}

fn is_partial_menu_token(cmd: &str, entries: &[SkillSlashEntry]) -> bool {
    let c = cmd.to_ascii_lowercase();
    all_menu_items(entries).iter().any(|item| {
        let invoke = item.invoke.to_ascii_lowercase();
        invoke.starts_with(&c) && invoke != c
    })
}

pub const MAX_SKILL_CHAIN: usize = 6;

/// Category tag for grouping slash commands in the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlashCategory {
    General,
    Session,
    Model,
    Files,
    Context,
    Agents,
    Skills,
    System,
}

impl SlashCategory {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Session => "Session",
            Self::Model => "Model",
            Self::Files => "Files",
            Self::Context => "Context",
            Self::Agents => "Agents",
            Self::Skills => "Skills",
            Self::System => "System",
        }
    }

    /// Stable ordering for grouping (lower = shown first).
    pub const fn order(&self) -> u8 {
        match self {
            Self::General => 0,
            Self::Session => 1,
            Self::Model => 2,
            Self::Files => 3,
            Self::Context => 4,
            Self::Agents => 5,
            Self::Skills => 6,
            Self::System => 7,
        }
    }
}

/// One row in the full-screen `/` menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashMenuItem {
    pub invoke: String,
    pub description: String,
    pub source: &'static str,
    /// Grouping tag.
    pub category: SlashCategory,
    /// Full usage signature, e.g. `/model [name]`.
    pub usage: String,
    /// Valid argument values (if finite), e.g. effort levels.
    pub args_hint: Vec<String>,
    /// A concrete example invocation.
    pub example: String,
}

/// Static spec for a built-in slash command (compile-time metadata).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinSlashSpec {
    pub invoke: &'static str,
    pub description: &'static str,
    pub category: SlashCategory,
    pub usage: &'static str,
    pub args_hint: &'static [&'static str],
    pub example: &'static str,
}

/// Built-in slash commands with rich metadata (palette overhaul).
pub const BUILTIN_SPECS: &[BuiltinSlashSpec] = &[
    BuiltinSlashSpec {
        invoke: "help",
        description: "Show slash command help (overlay)",
        category: SlashCategory::General,
        usage: "/help",
        args_hint: &[],
        example: "/help",
    },
    BuiltinSlashSpec {
        invoke: "clear",
        description: "Clear the conversation history",
        category: SlashCategory::Session,
        usage: "/clear",
        args_hint: &[],
        example: "/clear",
    },
    BuiltinSlashSpec {
        invoke: "quit",
        description: "Quit the TUI (same as 'q')",
        category: SlashCategory::General,
        usage: "/quit",
        args_hint: &[],
        example: "/quit",
    },
    BuiltinSlashSpec {
        invoke: "model",
        description: "Show or switch the active model mid-session",
        category: SlashCategory::Model,
        usage: "/model [name]",
        args_hint: &[],
        example: "/model grok-4",
    },
    BuiltinSlashSpec {
        invoke: "provider",
        description: "Show current provider info",
        category: SlashCategory::Model,
        usage: "/provider [name]",
        args_hint: &[],
        example: "/provider openai",
    },
    BuiltinSlashSpec {
        invoke: "diff",
        description: "Toggle the diff view pane",
        category: SlashCategory::Session,
        usage: "/diff",
        args_hint: &[],
        example: "/diff",
    },
    BuiltinSlashSpec {
        invoke: "sandbox",
        description: "Show sandbox mode info",
        category: SlashCategory::System,
        usage: "/sandbox [mode]",
        args_hint: &[],
        example: "/sandbox read-only",
    },
    BuiltinSlashSpec {
        invoke: "sessions",
        description: "List saved sessions",
        category: SlashCategory::Session,
        usage: "/sessions",
        args_hint: &[],
        example: "/sessions",
    },
    BuiltinSlashSpec {
        invoke: "skills",
        description: "Open the full-screen skills browser",
        category: SlashCategory::Skills,
        usage: "/skills",
        args_hint: &[],
        example: "/skills",
    },
    BuiltinSlashSpec {
        invoke: "plugins",
        description: "List loaded plugins",
        category: SlashCategory::Skills,
        usage: "/plugins",
        args_hint: &[],
        example: "/plugins",
    },
    BuiltinSlashSpec {
        invoke: "reload",
        description: "Reload skills from disk",
        category: SlashCategory::Skills,
        usage: "/reload",
        args_hint: &[],
        example: "/reload",
    },
    BuiltinSlashSpec {
        invoke: "reload-plugins",
        description: "Reload plugins from disk",
        category: SlashCategory::Skills,
        usage: "/reload-plugins",
        args_hint: &[],
        example: "/reload-plugins",
    },
    BuiltinSlashSpec {
        invoke: "agent",
        description: "Switch the active agent thread",
        category: SlashCategory::Agents,
        usage: "/agent",
        args_hint: &[],
        example: "/agent",
    },
    BuiltinSlashSpec {
        invoke: "effort",
        description: "Set reasoning effort level",
        category: SlashCategory::Model,
        usage: "/effort <level>",
        args_hint: &["none", "low", "medium", "high"],
        example: "/effort high",
    },
    BuiltinSlashSpec {
        invoke: "ask",
        description: "Toggle ask mode (confirm before every tool call)",
        category: SlashCategory::System,
        usage: "/ask",
        args_hint: &[],
        example: "/ask",
    },
    BuiltinSlashSpec {
        invoke: "rewind",
        description: "Restore a file from its shadow snapshot",
        category: SlashCategory::Files,
        usage: "/rewind <path>",
        args_hint: &[],
        example: "/rewind src/main.rs",
    },
    BuiltinSlashSpec {
        invoke: "find",
        description: "Fuzzy file-path search over the cwd",
        category: SlashCategory::Files,
        usage: "/find <query>",
        args_hint: &[],
        example: "/find main",
    },
    BuiltinSlashSpec {
        invoke: "compact",
        description: "Manually trigger context compaction",
        category: SlashCategory::Context,
        usage: "/compact",
        args_hint: &[],
        example: "/compact",
    },
    BuiltinSlashSpec {
        invoke: "image",
        description: "Attach an image to the next message",
        category: SlashCategory::Files,
        usage: "/image <path>",
        args_hint: &[],
        example: "/image ./screenshot.png",
    },
    BuiltinSlashSpec {
        invoke: "unimage",
        description: "Clear all pending image attachments",
        category: SlashCategory::Files,
        usage: "/unimage",
        args_hint: &[],
        example: "/unimage",
    },
    BuiltinSlashSpec {
        invoke: "copy",
        description: "Copy latest assistant output to clipboard",
        category: SlashCategory::General,
        usage: "/copy",
        args_hint: &[],
        example: "/copy",
    },
    BuiltinSlashSpec {
        invoke: "theme",
        description: "Open the syntax-highlight theme picker",
        category: SlashCategory::System,
        usage: "/theme [name]",
        args_hint: &[],
        example: "/theme",
    },
    BuiltinSlashSpec {
        invoke: "ui-theme",
        description: "Switch the UI chrome palette (dark/light)",
        category: SlashCategory::System,
        usage: "/ui-theme <name>",
        args_hint: &["tokyo-night", "catppuccin-latte", "nord", "rose-pine"],
        example: "/ui-theme catppuccin-latte",
    },
    BuiltinSlashSpec {
        invoke: "connect",
        description: "Connect a provider — enter API key in TUI (OpenCode parity)",
        category: SlashCategory::Model,
        usage: "/connect [provider]",
        args_hint: &[
            "xai",
            "openai",
            "anthropic",
            "gemini",
            "openrouter",
            "groq",
            "deepseek",
            "ollama",
        ],
        example: "/connect xai",
    },
    BuiltinSlashSpec {
        invoke: "auth",
        description: "Alias for /connect — manage provider API keys",
        category: SlashCategory::Model,
        usage: "/auth [provider]",
        args_hint: &[],
        example: "/auth",
    },
    BuiltinSlashSpec {
        invoke: "logout",
        description: "Remove a stored API key from auth.json",
        category: SlashCategory::Model,
        usage: "/logout [provider]",
        args_hint: &[],
        example: "/logout xai",
    },
    // Grok CLI TUI parity commands.
    BuiltinSlashSpec {
        invoke: "plan",
        description: "Enter plan mode — plan before editing (Shift+Tab to cycle)",
        category: SlashCategory::Session,
        usage: "/plan [description]",
        args_hint: &[],
        example: "/plan refactor the auth module",
    },
    BuiltinSlashSpec {
        invoke: "view-plan",
        description: "Show the current plan file (.zz/plan.md)",
        category: SlashCategory::Session,
        usage: "/view-plan",
        args_hint: &[],
        example: "/view-plan",
    },
    BuiltinSlashSpec {
        invoke: "always-approve",
        description: "Toggle always-approve mode — auto-approve all tool calls",
        category: SlashCategory::Session,
        usage: "/always-approve",
        args_hint: &[],
        example: "/always-approve",
    },
    BuiltinSlashSpec {
        invoke: "multiline",
        description: "Toggle multiline input (Enter=newline, Ctrl+Enter=submit)",
        category: SlashCategory::Session,
        usage: "/multiline",
        args_hint: &[],
        example: "/multiline",
    },
    BuiltinSlashSpec {
        invoke: "ml",
        description: "Alias for /multiline",
        category: SlashCategory::Session,
        usage: "/ml",
        args_hint: &[],
        example: "/ml",
    },
    BuiltinSlashSpec {
        invoke: "context",
        description: "Show context usage — token count and window percentage",
        category: SlashCategory::Session,
        usage: "/context",
        args_hint: &[],
        example: "/context",
    },
    BuiltinSlashSpec {
        invoke: "compact-mode",
        description: "Toggle compact UI mode — denser layout, less padding",
        category: SlashCategory::Session,
        usage: "/compact-mode",
        args_hint: &[],
        example: "/compact-mode",
    },
    BuiltinSlashSpec {
        invoke: "timestamps",
        description: "Toggle message timestamps in chat",
        category: SlashCategory::Session,
        usage: "/timestamps",
        args_hint: &[],
        example: "/timestamps",
    },
    BuiltinSlashSpec {
        invoke: "vim-mode",
        description: "Toggle vim-style scrollback keys (j/k/g/G/Ctrl+U/D)",
        category: SlashCategory::Session,
        usage: "/vim-mode",
        args_hint: &[],
        example: "/vim-mode",
    },
    BuiltinSlashSpec {
        invoke: "shortcuts",
        description: "Show keyboard shortcuts overlay (also Ctrl+.)",
        category: SlashCategory::Session,
        usage: "/shortcuts",
        args_hint: &[],
        example: "/shortcuts",
    },
    BuiltinSlashSpec {
        invoke: "export",
        description: "Export conversation to ~/.zz/export-<timestamp>.txt",
        category: SlashCategory::Session,
        usage: "/export",
        args_hint: &[],
        example: "/export",
    },
    BuiltinSlashSpec {
        invoke: "transcript",
        description: "View full transcript in $PAGER (less/more)",
        category: SlashCategory::Session,
        usage: "/transcript",
        args_hint: &[],
        example: "/transcript",
    },
    // Codex TUI parity — status & session.
    BuiltinSlashSpec {
        invoke: "status",
        description: "Show model, approval policy, token count, working dir",
        category: SlashCategory::Session,
        usage: "/status",
        args_hint: &[],
        example: "/status",
    },
    BuiltinSlashSpec {
        invoke: "new",
        description: "Start a new session (clears conversation)",
        category: SlashCategory::Session,
        usage: "/new",
        args_hint: &[],
        example: "/new",
    },
    // Codex TUI parity — rendering polish.
    BuiltinSlashSpec {
        invoke: "init",
        description: "Generate AGENTS.md scaffold in current directory",
        category: SlashCategory::Session,
        usage: "/init",
        args_hint: &[],
        example: "/init",
    },
    // Codex TUI parity — edge cases & polish.
    BuiltinSlashSpec {
        invoke: "review",
        description: "Show pending git changes for review",
        category: SlashCategory::Session,
        usage: "/review",
        args_hint: &[],
        example: "/review",
    },
    BuiltinSlashSpec {
        invoke: "keymap",
        description: "Display current TUI keybindings",
        category: SlashCategory::Session,
        usage: "/keymap",
        args_hint: &[],
        example: "/keymap",
    },
    BuiltinSlashSpec {
        invoke: "sandbox",
        description: "Show or toggle sandbox mode",
        category: SlashCategory::Session,
        usage: "/sandbox",
        args_hint: &[],
        example: "/sandbox",
    },
];

pub fn all_menu_items(entries: &[SkillSlashEntry]) -> Vec<SlashMenuItem> {
    let mut items: Vec<SlashMenuItem> = BUILTIN_SPECS
        .iter()
        .map(|s| SlashMenuItem {
            invoke: s.invoke.to_string(),
            description: s.description.to_string(),
            source: "builtin",
            category: s.category,
            usage: s.usage.to_string(),
            args_hint: s.args_hint.iter().map(|v| (*v).to_string()).collect(),
            example: s.example.to_string(),
        })
        .collect();
    for e in entries {
        if is_builtin_slash(&e.name) {
            continue;
        }
        let scope = match e.scope {
            zerozero_skills::SkillScope::Project => "project",
            zerozero_skills::SkillScope::User => "user",
        };
        let hint = if e.argument_hint.is_empty() {
            "task".to_string()
        } else {
            e.argument_hint.clone()
        };
        let usage = format!("/{} <{hint}>", e.name);
        let example = if e.description.is_empty() {
            format!("/{} {hint}", e.name)
        } else {
            format!("/{} — {}", e.name, e.description)
        };
        let args_hint = if e.argument_hint.is_empty() {
            Vec::new()
        } else {
            vec![e.argument_hint.clone()]
        };
        items.push(SlashMenuItem {
            invoke: e.name.clone(),
            description: e.description.clone(),
            source: "skill",
            category: SlashCategory::Skills,
            usage: usage.clone(),
            args_hint: args_hint.clone(),
            example: example.clone(),
        });
        items.push(SlashMenuItem {
            invoke: format!("{scope}:{}", e.name),
            description: e.description.clone(),
            source: "skill",
            category: SlashCategory::Skills,
            usage: format!("/{scope}:{} <{hint}>", e.name),
            args_hint,
            example: format!("/{scope}:{} <{hint}>", e.name),
        });
    }
    items
}

/// Fuzzy subsequence score; `-1` = no match.
pub fn fuzzy_score(needle: &str, haystack: &str) -> i32 {
    let needle_chars: Vec<char> = needle
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if needle_chars.is_empty() {
        return 0;
    }
    let hay: Vec<char> = haystack.to_ascii_lowercase().chars().collect();
    let mut ni = 0;
    let mut score = 0i32;
    let mut prev: Option<usize> = None;
    for (hi, hc) in hay.iter().enumerate() {
        if ni < needle_chars.len() && *hc == needle_chars[ni] {
            score += 1;
            if prev.map(|p| p + 1 == hi).unwrap_or(false) {
                score += 3;
            }
            if hi == 0 || hay.get(hi - 1).map(|c| c.is_whitespace()).unwrap_or(true) {
                score += 2;
            }
            prev = Some(hi);
            ni += 1;
        }
    }
    if ni < needle_chars.len() {
        return -1;
    }
    score
}

pub fn fuzzy_filter_menu(items: &[SlashMenuItem], query: &str) -> Vec<(usize, i32)> {
    let q = query.trim();
    let mut ranked: Vec<(usize, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let blob = format!(
                "{} {} {} {}",
                item.invoke, item.description, item.source, item.usage
            );
            let s = fuzzy_score(q, &blob);
            if s >= 0 {
                Some((i, s))
            } else if q.is_empty() {
                Some((i, 0))
            } else {
                None
            }
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// Canonical palette ranking shared by render, ↑↓, Tab, and Enter.
///
/// Empty query → category groups then A–Z. Non-empty → fuzzy score only.
pub fn ranked_for_palette(items: &[SlashMenuItem], query: &str) -> Vec<(usize, i32)> {
    let mut ranked = fuzzy_filter_menu(items, query);
    if query.trim().is_empty() {
        ranked.sort_by(|a, b| {
            let ca = items[a.0].category.order();
            let cb = items[b.0].category.order();
            ca.cmp(&cb)
                .then_with(|| items[a.0].invoke.cmp(&items[b.0].invoke))
        });
    }
    ranked
}

/// Exec/headless: skill chain or single `/skill <task>`.
pub fn try_skill_exec_prompt(
    input: &str,
    registry: &SkillRegistry,
) -> Option<(Vec<String>, String)> {
    if let Some((names, task)) = parse_skill_chain_line(input, registry) {
        if !task.is_empty() {
            return Some((names, task));
        }
    }
    let parsed = parse(input.trim())?;
    let SlashCommand::Unknown(cmd, args) = parsed else {
        return None;
    };
    let (name, task) = match_skill_registry(&cmd, &args, registry)?;
    if task.is_empty() {
        return None;
    }
    Some((vec![name], task))
}

/// `/a /b /c task` — Claude-style skill chain (2..=6 skills).
pub fn parse_skill_chain_line(
    input: &str,
    registry: &SkillRegistry,
) -> Option<(Vec<String>, String)> {
    let mut rest = input.trim();
    if !rest.starts_with('/') {
        return None;
    }
    let mut names = Vec::new();
    while names.len() < MAX_SKILL_CHAIN {
        rest = rest.trim_start();
        if !rest.starts_with('/') {
            break;
        }
        let body = &rest[1..];
        let (token, after) = split_first_token(body);
        if token.is_empty() {
            break;
        }
        if names.is_empty() && is_builtin_slash(token) {
            return None;
        }
        let Some(skill) = registry.resolve_slash_token(token) else {
            break;
        };
        names.push(skill.name.clone());
        rest = after;
    }
    if names.len() < 2 {
        return None;
    }
    let task = rest.trim().to_string();
    if task.is_empty() {
        return None;
    }
    Some((names, task))
}

fn split_first_token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    if let Some(i) = s.find(char::is_whitespace) {
        (&s[..i], &s[i..])
    } else {
        (s, "")
    }
}

pub fn format_skill_chain_blocks(registry: &SkillRegistry, names: &[String], task: &str) -> String {
    let mut blocks = String::new();
    for name in names {
        if let Some(skill) = registry.get(name) {
            blocks.push_str(&format!(
                "## Skill: {}\n{}\n\n{}\n\n",
                skill.name, skill.description, skill.content
            ));
        }
    }
    blocks.push_str(&format!("**ARGUMENTS:** {task}"));
    blocks
}

/// Completions for input buffer (fuzzy on current token after `/`).
pub fn slash_completions(token: &str, entries: &[SkillSlashEntry]) -> Vec<String> {
    let items = all_menu_items(entries);
    ranked_for_palette(&items, token)
        .into_iter()
        .map(|(i, _)| items[i].invoke.clone())
        .collect()
}

/// Apply Tab completion using [`ranked_for_palette`] (same order as the UI).
pub fn apply_slash_tab(
    input: &str,
    entries: &[SkillSlashEntry],
    menu_index: usize,
) -> Option<String> {
    if !input.starts_with('/') {
        return None;
    }
    let rest = &input[1..];
    // After a space, Tab does not rewrite the command token.
    if rest.chars().any(char::is_whitespace) {
        return None;
    }
    let token = rest;
    let items = all_menu_items(entries);
    let ranked = ranked_for_palette(&items, token);
    if ranked.is_empty() {
        return None;
    }
    let idx = menu_index.min(ranked.len() - 1);
    let item = &items[ranked[idx].0];
    // Always leave a trailing space so the user can type args immediately.
    Some(format!("/{} ", item.invoke))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_help() {
        assert_eq!(parse("/help"), Some(SlashCommand::Help));
    }

    #[test]
    fn test_parse_clear() {
        assert_eq!(parse("/clear"), Some(SlashCommand::Clear));
    }

    #[test]
    fn test_parse_quit() {
        assert_eq!(parse("/quit"), Some(SlashCommand::Quit));
    }

    #[test]
    fn test_parse_diff() {
        assert_eq!(parse("/diff"), Some(SlashCommand::Diff));
    }

    #[test]
    fn test_parse_model() {
        assert_eq!(
            parse("/model grok-4"),
            Some(SlashCommand::Model("grok-4".into()))
        );
    }

    #[test]
    fn test_parse_provider() {
        assert_eq!(
            parse("/provider openai"),
            Some(SlashCommand::Provider("openai".into()))
        );
    }

    #[test]
    fn test_parse_unknown() {
        assert_eq!(
            parse("/foobar"),
            Some(SlashCommand::Unknown("foobar".into(), String::new()))
        );
    }

    #[test]
    fn test_parse_skill_slash_with_args() {
        assert_eq!(
            parse("/ponytail fix it"),
            Some(SlashCommand::Unknown("ponytail".into(), "fix it".into()))
        );
    }

    #[test]
    fn test_parse_not_slash() {
        assert_eq!(parse("hello"), None);
    }

    #[test]
    fn test_parse_case_insensitive() {
        assert_eq!(parse("/HELP"), Some(SlashCommand::Help));
    }

    #[test]
    fn test_parse_sessions() {
        assert_eq!(parse("/sessions"), Some(SlashCommand::Sessions));
    }

    #[test]
    fn test_parse_sandbox() {
        assert_eq!(
            parse("/sandbox read-only"),
            Some(SlashCommand::Sandbox("read-only".into()))
        );
    }

    #[test]
    fn test_parse_model_no_arg() {
        assert_eq!(parse("/model"), Some(SlashCommand::Model(String::new())));
    }

    #[test]
    fn test_parse_with_leading_whitespace() {
        assert_eq!(parse("  /help  "), Some(SlashCommand::Help));
    }

    #[test]
    fn test_parse_skills() {
        assert_eq!(parse("/skills"), Some(SlashCommand::Skills));
    }

    #[test]
    fn test_parse_plugins() {
        assert_eq!(parse("/plugins"), Some(SlashCommand::Plugins));
    }

    #[test]
    fn test_parse_reload() {
        assert_eq!(parse("/reload"), Some(SlashCommand::Reload));
    }

    #[test]
    fn test_parse_reload_plugins() {
        assert_eq!(parse("/reload-plugins"), Some(SlashCommand::ReloadPlugins));
    }

    #[test]
    fn test_parse_ui_theme() {
        assert_eq!(
            parse("/ui-theme catppuccin-latte"),
            Some(SlashCommand::UiTheme("catppuccin-latte".to_string()))
        );
        assert_eq!(
            parse("/ui-theme"),
            Some(SlashCommand::UiTheme(String::new()))
        );
        // ui-theme is a registered builtin name.
        assert!(is_builtin_slash("ui-theme"));
    }

    #[test]
    fn test_parse_connect_and_auth() {
        assert_eq!(
            parse("/connect"),
            Some(SlashCommand::Connect(String::new()))
        );
        assert_eq!(
            parse("/connect xai"),
            Some(SlashCommand::Connect("xai".into()))
        );
        assert_eq!(parse("/auth"), Some(SlashCommand::Connect(String::new())));
        assert_eq!(
            parse("/auth groq"),
            Some(SlashCommand::Connect("groq".into()))
        );
        assert_eq!(
            parse("/logout openai"),
            Some(SlashCommand::Logout("openai".into()))
        );
        assert_eq!(
            parse("/disconnect"),
            Some(SlashCommand::Logout(String::new()))
        );
        assert!(is_builtin_slash("connect"));
        assert!(is_builtin_slash("auth"));
    }

    #[test]
    fn test_fuzzy_score() {
        assert!(fuzzy_score("pt", "ponytail lazy") > 0);
        assert_eq!(fuzzy_score("zzz", "ponytail"), -1);
    }

    #[test]
    fn test_skill_chain_parse() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".devin/skills");
        for name in ["ponytail", "tdd"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            std::fs::write(
                dir.join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n---\nbody"),
            )
            .unwrap();
        }
        let mut reg = SkillRegistry::new();
        reg.load_from_dir(&dir).unwrap();
        let (names, task) = parse_skill_chain_line("/ponytail /tdd ship it", &reg).expect("chain");
        assert_eq!(names, vec!["ponytail", "tdd"]);
        assert_eq!(task, "ship it");
    }

    #[test]
    fn test_slash_completions() {
        let entries = vec![SkillSlashEntry {
            name: "ponytail".into(),
            scope: zerozero_skills::SkillScope::Project,
            description: String::new(),
            argument_hint: String::new(),
        }];
        let c = slash_completions("po", &entries);
        assert!(c.iter().any(|s| s == "ponytail"));
        let scoped = slash_completions("project:po", &entries);
        assert!(scoped.iter().any(|s| s == "project:ponytail"));
    }

    #[test]
    fn test_match_skill_registry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".devin/skills");
        std::fs::create_dir_all(dir.join("ponytail")).unwrap();
        std::fs::write(
            dir.join("ponytail/SKILL.md"),
            "---\nname: ponytail\ndescription: d\n---\nbody",
        )
        .unwrap();
        let mut reg = SkillRegistry::new();
        reg.load_from_dir(&dir).unwrap();
        assert_eq!(
            match_skill_registry("ponytail", "go", &reg),
            Some(("ponytail".to_string(), "go".to_string()))
        );
        assert_eq!(
            match_skill_registry("project:ponytail", "go", &reg),
            Some(("ponytail".to_string(), "go".to_string()))
        );
    }

    #[test]
    fn test_is_slash_submittable() {
        let reg = SkillRegistry::new();
        let entries: Vec<SkillSlashEntry> = vec![];
        assert!(!is_slash_submittable("/", &reg, &entries));
        assert!(!is_slash_submittable("/mod", &reg, &entries));
        assert!(is_slash_submittable("/help", &reg, &entries));
        assert!(is_slash_submittable("/model grok-4", &reg, &entries));
        assert!(is_slash_submittable("/foobar", &reg, &entries));
    }

    #[test]
    fn test_ranked_for_palette_matches_tab_order() {
        let items = all_menu_items(&[]);
        let ranked = ranked_for_palette(&items, "mod");
        let tab = apply_slash_tab("/mod", &[], 0).unwrap();
        assert_eq!(tab, format!("/{} ", items[ranked[0].0].invoke));
    }

    #[test]
    fn test_ranked_empty_query_is_category_sorted() {
        let items = all_menu_items(&[]);
        let ranked = ranked_for_palette(&items, "");
        let mut prev = 0u8;
        for &(idx, _) in &ranked {
            let o = items[idx].category.order();
            assert!(o >= prev);
            prev = o;
        }
    }

    #[test]
    fn test_apply_slash_tab_noop_after_space() {
        assert!(apply_slash_tab("/model ", &[], 0).is_none());
    }

    // --- : /effort slash command — full test body ---

    #[test]
    fn test_slash_parse_effort() {
        assert_eq!(
            parse("/effort high"),
            Some(SlashCommand::Effort("high".to_string()))
        );
        assert_eq!(
            parse("/effort low"),
            Some(SlashCommand::Effort("low".to_string()))
        );
        assert_eq!(
            parse("/effort medium"),
            Some(SlashCommand::Effort("medium".to_string()))
        );
        assert_eq!(
            parse("/effort none"),
            Some(SlashCommand::Effort("none".to_string()))
        );
        assert_eq!(parse("/effort"), Some(SlashCommand::Effort(String::new())));
        assert_eq!(
            parse("/effort invalid"),
            Some(SlashCommand::Effort("invalid".to_string()))
        );
        assert_eq!(
            parse("/EFFORT high"),
            Some(SlashCommand::Effort("high".to_string()))
        );
    }

    #[test]
    fn test_parse_model_empty() {
        assert_eq!(
            parse("/model"),
            Some(SlashCommand::Model(String::new())),
            "/model with no arg must parse to Model(\"\")"
        );
        assert_eq!(
            parse("/model   "),
            Some(SlashCommand::Model(String::new())),
            "/model with only whitespace must parse to Model(\"\")"
        );
    }

    #[test]
    fn test_parse_ask() {
        assert_eq!(parse("/ask"), Some(SlashCommand::Ask));
        assert_eq!(parse("/ASK"), Some(SlashCommand::Ask));
        assert_eq!(parse("/ask   "), Some(SlashCommand::Ask));
    }

    #[test]
    fn test_parse_rewind() {
        assert_eq!(
            parse("/rewind src/main.rs"),
            Some(SlashCommand::Rewind("src/main.rs".to_string()))
        );
        assert_eq!(
            parse("/REWIND src/main.rs"),
            Some(SlashCommand::Rewind("src/main.rs".to_string()))
        );
        assert_eq!(parse("/rewind"), Some(SlashCommand::Rewind(String::new())));
    }

    #[test]
    fn test_parse_find() {
        assert_eq!(parse("/find main"), Some(SlashCommand::Find("main".into())));
        assert_eq!(
            parse("/find src/foo.rs"),
            Some(SlashCommand::Find("src/foo.rs".into()))
        );
        assert_eq!(parse("/find"), Some(SlashCommand::Find(String::new())));
    }

    #[test]
    fn test_parse_compact() {
        assert_eq!(parse("/compact"), Some(SlashCommand::Compact));
        assert_eq!(parse("/COMPACT"), Some(SlashCommand::Compact));
        assert_eq!(parse("/compact   "), Some(SlashCommand::Compact));
        // /compact must not be treated as an unknown builtin.
        assert!(is_builtin_slash("compact"));
    }

    #[test]
    fn test_parse_image() {
        // /image <path> attaches an image; /unimage clears.
        assert_eq!(
            parse("/image /tmp/cat.png"),
            Some(SlashCommand::Image("/tmp/cat.png".into()))
        );
        assert_eq!(parse("/IMAGE x"), Some(SlashCommand::Image("x".into())));
        assert_eq!(parse("/image"), Some(SlashCommand::Image(String::new())));
        assert_eq!(parse("/unimage"), Some(SlashCommand::Unimage));
        assert!(is_builtin_slash("image"));
        assert!(is_builtin_slash("unimage"));
    }
}
