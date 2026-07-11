//! TUI rendering — industry-standard layout with a centralized truecolor theme.
//!
//! Layout (top → bottom):
//! ```text
//! ┌ Header (1 line) — brand · model · effort · ask · agents · mode
//! ├ Chat pane (flex, min 3) — messages, tool cards, streaming
//! │  [optional] agent tree (right 28%) · diff view (bottom 42%)
//! ├ Status indicator (1 line, only when streaming) — spinner · elapsed · interrupt
//! ├ Input pane (grows with content, min 3) — placeholder + inline hints
//! ├ Footer (1 line) — contextual shortcut hints
//! └ Status bar (1 line) — session · state · diff · tokens · hints
//! ```
//!
//! All colors come from [`crate::theme`] — no scattered `Color::Cyan` literals.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use crate::theme::{Theme, current as theme};

/// Render the full TUI: header + chat + status indicator + composer + footer + status bar.
pub fn render(frame: &mut Frame, app: &App) {
    let t = theme();

    // Status indicator line (1 line when streaming, 0 when idle).
    let status_h = if app.is_streaming { 1u16 } else { 0u16 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                 // header
            Constraint::Min(3),                    // chat
            Constraint::Length(status_h),          // status indicator (conditional)
            Constraint::Length(input_height(app)), // composer
            Constraint::Length(1),                 // footer hints
            Constraint::Length(1),                 // status bar
        ])
        .split(frame.area());

    render_header(frame, chunks[0], app, &t);

    // If agent tree view is shown, split chat area.
    if app.show_agent_tree {
        let sub = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(chunks[1]);
        render_chat(frame, sub[0], app, &t);
        crate::agent_tree::render(frame, sub[1], app);
    } else {
        render_chat(frame, chunks[1], app, &t);
    }
    // Status indicator (only visible when streaming).
    if status_h > 0 {
        render_status_indicator(frame, chunks[2], app, &t);
    }
    crate::composer::render(frame, chunks[3], app, &t);
    // Footer hints.
    render_footer(frame, chunks[4], app, &t);
    render_status(frame, chunks[5], app, &t);

    // Agent picker popup (overlay on top of everything).
    if app.show_agent_picker {
        crate::agent_picker::render(frame, app);
    }

    if app.show_skills_browser {
        crate::skills_browser::render(frame, app);
    } else if app.composer.show_slash_palette && app.composer.input_buffer.starts_with('/') {
        crate::slash_menu::render(frame, app);
    }

    // Theme picker overlay.
    if app.show_theme_picker {
        render_theme_picker(frame, app, &t);
    }

    // Palette overhaul: `/help` overlay.
    if app.show_help_overlay {
        render_help_overlay(frame, app, &t);
    }

    // 3-tier model picker overlay (`/model` with no arg).
    if app.show_model_picker {
        crate::model_picker::render(frame, app);
    }

    // `/connect` provider + API key overlay (OpenCode parity).
    if app.show_connect {
        crate::connect::render(frame, app);
    }

    // History search overlay.
    if app.show_history_search {
        render_history_search(frame, app, &t);
    }

    // Approval overlay for inactive thread (overlay).
    if let Some(approval) = &app.pending_approval {
        render_approval_overlay(frame, approval, &t);
    }

    // Keyboard shortcuts overlay.
    if app.show_shortcuts_overlay {
        render_shortcuts_overlay(frame, app, &t);
    }
}

/// Composer pane height — delegated to [`crate::composer::ComposerState::height`].
fn input_height(app: &App) -> u16 {
    app.composer.height(app.is_streaming)
}

fn render_chat(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    if app.show_diff {
        if let Some(diff) = app.diff_view.as_ref() {
            // Split chat area: chat (top 58%) + diff view (bottom 42%).
            let sub = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(area);
            render_chat_pane(frame, sub[0], app, t);
            crate::diff::render_diff(frame, sub[1], diff);
            return;
        }
    }
    render_chat_pane(frame, area, app, t);
}

fn render_chat_pane(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    // Inner width (minus borders + 1 char left padding for content alignment).
    let inner_w = area.width.saturating_sub(2) as usize;

    // Build lines with manual wrapping so line_count matches displayed rows.
    // This prevents scroll miscalculation that caused flicker with ratatui's
    // Wrap (which adds rows at render time, not counted by our scroll math).
    let lines = build_chat_lines(app, inner_w, t);
    let line_count = lines.len();
    let visible_height = area.height.saturating_sub(2) as usize;
    // `chat_scroll` = lines up from bottom (0 = stick to latest).
    let max_scroll = line_count.saturating_sub(visible_height);
    let from_bottom = (app.chat_scroll as usize).min(max_scroll);
    let scroll = max_scroll.saturating_sub(from_bottom);

    // Title with message count + scroll affordance.
    let msg_count = app.messages.len();
    let title = if max_scroll > 0 && from_bottom > 0 {
        format!(
            " Chat · {} msgs · ↑{} · PgUp/PgDn scroll ",
            msg_count, from_bottom
        )
    } else if line_count > visible_height {
        format!(" Chat · {} msgs · end ", msg_count)
    } else if msg_count > 0 {
        format!(" Chat · {} msgs ", msg_count)
    } else {
        " Chat ".to_string()
    };

    let paragraph = Paragraph::new(lines).scroll((scroll as u16, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(
                title,
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            )),
    );

    frame.render_widget(paragraph, area);
}

/// Build the chat area content as styled ratatui lines (Claude/Codex-style).
///
/// Each message has a role badge line (icon + label) followed by the content
/// rendered through markdown (assistant) or plain text (user). System messages
/// are inline dim italic. Thinking is dim italic with a 💭 label. Streaming
/// shows a spinner char beside the Assistant badge. A faint separator is
/// drawn between top-level messages for visual rhythm.
///
/// `width` is the inner width of the chat pane (minus borders). Lines longer
/// than `width` are manually wrapped so the line count matches the displayed
/// row count — this prevents scroll miscalculation and flicker.
fn build_chat_lines(app: &App, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Empty-state welcome (Claude/OpenCode-style) — only when the chat is idle.
    let chat_empty = app.messages.is_empty()
        && app.streaming_text.is_empty()
        && app.streaming_reasoning_text.is_empty()
        && app.tool_events.is_empty();
    if chat_empty {
        lines.extend(build_welcome_lines(app, width, t));
        if width > 0 {
            wrap_lines(&mut lines, width);
        }
        return lines;
    }

    for (i, msg) in app.messages.iter().enumerate() {
        if i > 0 {
            // Faint separator between messages (industry-standard rhythm).
            // Skip separator in compact mode for denser layout.
            if !app.compact_mode {
                lines.push(separator_line(width, t));
            }
        }
        // Timestamp prefix when enabled.
        let ts = app.format_timestamp(i);
        let ts_span = if ts.is_empty() {
            Span::raw("")
        } else {
            Span::styled(ts, Style::default().fg(t.dim))
        };
        match msg.role.as_str() {
            "assistant" | "agent" => {
                // Badge: ◆ Assistant (accent bold)
                lines.push(Line::from(vec![
                    ts_span,
                    Span::styled(
                        "◆ ",
                        Style::default()
                            .fg(t.assistant)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "Assistant",
                        Style::default()
                            .fg(t.assistant)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.extend(crate::markdown::render_markdown(&msg.content));
            }
            "thinking" => {
                // Persisted reasoning from a completed turn — dim italic
                // with 💭 label, same style as live streaming reasoning.
                lines.push(Line::from(vec![
                    ts_span,
                    Span::styled(
                        "💭 ",
                        Style::default().fg(t.thinking).add_modifier(Modifier::DIM),
                    ),
                    Span::styled(
                        "thinking",
                        Style::default()
                            .fg(t.thinking)
                            .add_modifier(Modifier::DIM | Modifier::ITALIC),
                    ),
                ]));
                for line in msg.content.split('\n') {
                    lines.push(Line::styled(
                        line.to_string(),
                        Style::default()
                            .fg(t.thinking)
                            .add_modifier(Modifier::DIM | Modifier::ITALIC),
                    ));
                }
            }
            "system" => {
                // Inline dim italic: ⚙ system message
                lines.push(Line::from(vec![
                    ts_span,
                    Span::styled(
                        "⚙ ",
                        Style::default().fg(t.system).add_modifier(Modifier::DIM),
                    ),
                    Span::styled(
                        msg.content.clone(),
                        Style::default()
                            .fg(t.system)
                            .add_modifier(Modifier::ITALIC | Modifier::DIM),
                    ),
                ]));
            }
            _ => {
                // User: ● You (brand bold), content plain text below
                lines.push(Line::from(vec![
                    ts_span,
                    Span::styled(
                        "● ",
                        Style::default().fg(t.user).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "You",
                        Style::default().fg(t.user).add_modifier(Modifier::BOLD),
                    ),
                ]));
                // Render user content as plain text lines (no markdown).
                for line in msg.content.split('\n') {
                    lines.push(Line::styled(line.to_string(), Style::default().fg(t.fg)));
                }
            }
        }
    }

    // Tool call events — compact single-line cards (▸ name · status · preview).
    if !app.tool_events.is_empty() {
        if !lines.is_empty() && !matches!(lines.last().map(|l| l.spans.is_empty()), Some(true)) {
            lines.push(separator_line(width, t));
        }
        for (idx, ev) in app.tool_events.iter().enumerate() {
            let (status_icon, status_label, status_color) = match ev.status {
                crate::app::ToolStatus::Running => ("⏺", "running", t.running),
                crate::app::ToolStatus::Done => ("✓", "done", t.success),
                crate::app::ToolStatus::Error => ("✗", "error", t.danger),
            };
            // Collapsed tool output — show only header, not preview.
            let is_collapsed = app.collapsed_tools.contains(&idx);
            let mut spans = vec![
                Span::styled(
                    if is_collapsed { "▸ " } else { "▾ " },
                    Style::default().fg(t.tool).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    ev.name.clone(),
                    Style::default().fg(t.tool).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(
                    status_icon,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {status_label}"),
                    Style::default().fg(status_color),
                ),
            ];
            if !is_collapsed {
                if let Some(preview) = &ev.preview {
                    let one_line: String = preview.split('\n').next().unwrap_or("").to_string();
                    let max = width.saturating_sub(20);
                    let shown: String = one_line.chars().take(max).collect();
                    if !shown.is_empty() {
                        spans.push(Span::styled("  ", Style::default()));
                        spans.push(Span::styled(shown, Style::default().fg(t.dim)));
                    }
                }
            } // end if !is_collapsed
            lines.push(Line::from(spans));
        }
    }

    // Thinking / reasoning stream — dim italic with 💭 label.
    if !app.streaming_reasoning_text.is_empty() {
        if !lines.is_empty() {
            lines.push(separator_line(width, t));
        }
        lines.push(Line::from(vec![
            Span::styled(
                "💭 ",
                Style::default().fg(t.thinking).add_modifier(Modifier::DIM),
            ),
            Span::styled(
                "thinking",
                Style::default()
                    .fg(t.thinking)
                    .add_modifier(Modifier::DIM | Modifier::ITALIC),
            ),
        ]));
        lines.extend(crate::markdown::render_streaming_reasoning(
            &app.streaming_reasoning_text,
        ));
    }

    // In-flight answer: badge with spinner when streaming, plain lines
    // (markdown only after `item.completed`).
    if !app.streaming_text.is_empty() {
        if !lines.is_empty() && app.streaming_reasoning_text.is_empty() {
            lines.push(separator_line(width, t));
        }
        // Always reserve 2 columns (` X`) so toggling/animating the spinner
        // never shifts the message body horizontally.
        let spinner = if app.is_streaming {
            format!(" {}", app.spinner_char())
        } else {
            "  ".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(
                "◆ ",
                Style::default()
                    .fg(t.assistant)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Assistant",
                Style::default()
                    .fg(t.assistant)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                spinner,
                Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
            ),
        ]));
        if app.is_streaming {
            let mut stream_lines = crate::markdown::render_streaming_plain(&app.streaming_text);
            // Streaming shimmer — append a blinking cursor at the end.
            if app.spinner_tick % 2 == 0 {
                if let Some(last) = stream_lines.last_mut() {
                    last.spans
                        .push(Span::styled("▋", Style::default().fg(t.brand)));
                }
            }
            lines.extend(stream_lines);
        } else {
            lines.extend(crate::markdown::render_markdown(&app.streaming_text));
        }
    }

    // Manual wrap: split any line wider than `width` into multiple lines
    // so the line count matches the displayed row count. This is critical
    // for stable scroll calculation — ratatui's Wrap adds rows at render
    // time which our scroll math can't see, causing flicker.
    if width > 0 {
        wrap_lines(&mut lines, width);
    }

    lines
}

/// A faint horizontal separator line filling `width` columns.
fn separator_line(width: usize, t: &Theme) -> Line<'static> {
    let w = width.max(1);
    Line::styled(
        format!(" {}", "─".repeat(w.saturating_sub(2))),
        Style::default().fg(t.separator),
    )
}

/// Display-column width of a single Unicode scalar (matches ratatui `Span::width`).
fn char_display_width(c: char) -> usize {
    Span::raw(c.to_string()).width()
}

/// Wrap lines in-place so no line exceeds `width` **display columns**.
///
/// Must use Unicode display width (not char count). Counting chars caused
/// scroll-row mismatch vs the terminal for wide glyphs (CJK/emoji) → chat
/// content "jumped" / flickered while streaming or resizing.
fn wrap_lines(lines: &mut Vec<Line<'static>>, width: usize) {
    if width == 0 {
        return;
    }
    let mut result: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    for line in lines.drain(..) {
        if line.width() <= width {
            result.push(line);
            continue;
        }
        let mut current_spans: Vec<Span<'static>> = Vec::new();
        let mut current_w = 0usize;
        for span in line.spans {
            let span_w = span.width();
            if current_w + span_w <= width {
                current_spans.push(span);
                current_w += span_w;
                continue;
            }
            // Split this span by display columns.
            let style = span.style;
            let chars: Vec<char> = span.content.chars().collect();
            let mut idx = 0;
            while idx < chars.len() {
                let remaining = width.saturating_sub(current_w);
                if remaining == 0 {
                    if !current_spans.is_empty() {
                        result.push(Line::from(std::mem::take(&mut current_spans)));
                    }
                    current_w = 0;
                    continue;
                }
                // Fit as many chars as possible into `remaining` columns.
                let mut take = 0usize;
                let mut take_w = 0usize;
                while idx + take < chars.len() {
                    let cw = char_display_width(chars[idx + take]);
                    if take_w + cw > remaining {
                        break;
                    }
                    take_w += cw;
                    take += 1;
                }
                if take == 0 {
                    // Next char is wider than remaining space.
                    if current_w == 0 {
                        // Force one char on its own line (avoid infinite loop).
                        take = 1;
                        take_w = char_display_width(chars[idx]);
                    } else {
                        result.push(Line::from(std::mem::take(&mut current_spans)));
                        current_w = 0;
                        continue;
                    }
                }
                let chunk: String = chars[idx..idx + take].iter().collect();
                current_spans.push(Span::styled(chunk, style));
                current_w += take_w;
                idx += take;
                if idx < chars.len() {
                    result.push(Line::from(std::mem::take(&mut current_spans)));
                    current_w = 0;
                }
            }
        }
        if !current_spans.is_empty() {
            result.push(Line::from(current_spans));
        }
    }
    *lines = result;
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let sep = Span::styled(" │ ", Style::default().fg(t.border));
    let mut spans = vec![
        Span::styled(
            " ZeroZero ",
            Style::default()
                .fg(Color::Rgb(0x00, 0x00, 0x00))
                .bg(t.brand)
                .add_modifier(Modifier::BOLD),
        ),
        sep.clone(),
    ];

    // Always show provider/model (defaults when unset).
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
    spans.push(Span::styled(
        format!("{provider}/"),
        Style::default().fg(t.dim),
    ));
    spans.push(Span::styled(
        format!("{model} "),
        Style::default().fg(t.success).add_modifier(Modifier::BOLD),
    ));
    spans.push(sep.clone());

    spans.push(Span::styled(
        format!("effort:{} ", app.effort),
        Style::default().fg(t.accent),
    ));
    spans.push(sep.clone());

    spans.push(Span::styled(
        format!("ask:{} ", if app.ask_mode { "on" } else { "off" }),
        Style::default().fg(if app.ask_mode { t.warning } else { t.dim }),
    ));
    spans.push(sep.clone());

    // Session mode indicator (normal/plan/approve).
    spans.push(Span::styled(
        format!("mode:{} ", app.session_mode.label()),
        Style::default().fg(match app.session_mode {
            crate::app::SessionMode::Normal => t.dim,
            crate::app::SessionMode::Plan => t.accent,
            crate::app::SessionMode::AlwaysApprove => t.warning,
        }),
    ));

    // Multiline / vim / compact indicators (only when active).
    if app.multiline || app.vim_mode || app.compact_mode || app.show_timestamps {
        spans.push(sep.clone());
        let mut tags = Vec::new();
        if app.multiline {
            tags.push("ml");
        }
        if app.vim_mode {
            tags.push("vim");
        }
        if app.compact_mode {
            tags.push("compact");
        }
        if app.show_timestamps {
            tags.push("ts");
        }
        spans.push(Span::styled(
            format!("{} ", tags.join(",")),
            Style::default().fg(t.accent),
        ));
    }

    if app.live_agents.len() > 1 {
        spans.push(sep.clone());
        spans.push(Span::styled(
            format!("{} agents ", app.live_agents.len()),
            Style::default().fg(t.tool),
        ));
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_status(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    // Fixed-width fields so digit growth (tokens, scroll) does not shift the
    // rest of the status bar every frame ("number jumping").
    let streaming_status = if app.is_streaming {
        format!("{} streaming", app.spinner_char())
    } else if app.chat_scroll > 0 {
        format!("↑{:>4}", app.chat_scroll)
    } else {
        "idle".to_string()
    };
    let diff_status = if app.show_diff { "on" } else { "off" };
    let tokens = app.token_count();

    // Agent footer label (shown when multiple threads exist).
    let agent_label = app.agent_footer_label();

    let dot = Span::styled(" · ", Style::default().fg(t.border));
    let mut spans = vec![Span::raw(" ")];

    if !agent_label.is_empty() {
        spans.push(Span::styled(agent_label, Style::default().fg(t.tool)));
        spans.push(dot.clone());
    }

    spans.push(Span::styled("session ", Style::default().fg(t.dim)));
    spans.push(Span::styled(
        if app.session_id.is_empty() {
            "(none)".to_string()
        } else {
            // Truncate long session ids for narrow terminals.
            let id = &app.session_id;
            if id.len() > 18 {
                format!("{}…", &id[..16])
            } else {
                id.clone()
            }
        },
        Style::default().fg(t.brand),
    ));
    spans.push(dot.clone());

    spans.push(Span::styled(
        streaming_status,
        if app.is_streaming {
            Style::default().fg(t.success)
        } else if app.chat_scroll > 0 {
            Style::default().fg(t.warning)
        } else {
            Style::default().fg(t.dim)
        },
    ));
    spans.push(dot.clone());

    spans.push(Span::styled(
        // Right-align in 6 columns: ~1 … ~999999 without shifting neighbors.
        format!("~{tokens:>6} tok"),
        Style::default().fg(t.accent),
    ));

    if app.show_diff {
        spans.push(dot.clone());
        spans.push(Span::styled(
            format!("diff:{diff_status}"),
            Style::default().fg(t.warning),
        ));
    }

    // Compact key binding hints on the right side of the status bar.
    spans.push(dot.clone());
    spans.push(Span::styled("/ · ? · PgUp/Dn ", Style::default().fg(t.dim)));

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

// Status indicator line (above composer when streaming).
fn render_status_indicator(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let text = app.status_indicator.status_text(app.spinner_char());
    if text.is_empty() {
        return;
    }
    let line = Line::from(vec![Span::styled(text, Style::default().fg(t.success))]);
    frame.render_widget(Paragraph::new(line), area);
}

// Footer hints (below composer, contextual).
fn render_footer(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let hint = app.footer_mode.hint_text();
    let line = Line::from(vec![Span::styled(hint, Style::default().fg(t.dim))]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Welcome / empty-state content when the chat has no messages yet.
///
/// Purely derived from `app` fields (no env/auth reads) so TestBackend
/// snapshots stay deterministic across machines.
fn build_welcome_lines(app: &App, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let provider = if app.provider.is_empty() {
        "xai".to_string()
    } else {
        app.provider.clone()
    };
    let model = if app.model.is_empty() {
        "default".to_string()
    } else {
        app.model.clone()
    };

    let w = width.max(40);
    let pad = |s: &str| -> String {
        // Soft center for short lines within the chat pane.
        let len = s.chars().count();
        if len >= w {
            return s.to_string();
        }
        let left = (w - len) / 2;
        format!("{}{}", " ".repeat(left), s)
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::raw(""));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        pad("◆  ZeroZero"),
        Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![Span::styled(
        pad("CLI coding agent"),
        Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
    )]));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        pad(&format!("{provider}  ·  {model}  ·  effort:{}", app.effort)),
        Style::default().fg(t.fg),
    )]));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        pad("Get started"),
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    )]));
    for tip in [
        "Type a task and press Enter",
        "/connect   enter provider API key in TUI",
        "/model     switch provider · model · effort",
        "/help      all slash commands",
        "PgUp/PgDn  scroll chat history",
        "Ctrl+R     search past prompts",
    ] {
        lines.push(Line::from(vec![Span::styled(
            pad(&format!("  · {tip}")),
            Style::default().fg(t.dim),
        )]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        pad("Press / anytime for the command palette"),
        Style::default().fg(t.brand).add_modifier(Modifier::ITALIC),
    )]));
    lines
}

/// Render the approval overlay for a pending approval from an inactive
/// thread Phase 5).
///
/// Shows: `[Approval from agent-0 (root.0)] Run bash? [y/n]`
/// + hint `press 'o' to open thread`.
fn render_approval_overlay(frame: &mut Frame, approval: &crate::app::PendingApproval, t: &Theme) {
    let area = centered_rect(70, 30, frame.area());

    let text = format!(
        "Approval from thread {}\n  Tool: {} (danger: {})\n  Args: {}\n\n  Press 'o' to switch to this thread, then approve/deny.",
        approval.source_thread_id, approval.tool_name, approval.danger_level, approval.args
    );

    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(t.warning))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.warning))
                .title(" Pending Approval (inactive thread) "),
        );

    frame.render_widget(paragraph, area);
}

/// Render the theme picker overlay.
fn render_theme_picker(frame: &mut Frame, app: &App, t: &Theme) {
    let themes = crate::markdown::available_themes();
    let current = crate::markdown::current_theme_name();
    let area = centered_rect(60, 50, frame.area());

    let mut lines: Vec<Line<'_>> = Vec::new();
    for (i, name) in themes.iter().enumerate() {
        let style = if i == app.theme_picker_index {
            Style::default().fg(t.selection_fg).bg(t.selection_bg)
        } else if name == &current {
            Style::default().fg(t.success)
        } else {
            Style::default().fg(t.fg)
        };
        let marker = if name == &current { " ★" } else { "  " };
        lines.push(Line::styled(format!("{marker} {name}"), style));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.brand))
            .title(" Code theme (↑↓ select · Enter apply · Esc cancel) "),
    );
    frame.render_widget(paragraph, area);
}

/// Render the history search overlay.
fn render_history_search(frame: &mut Frame, app: &App, t: &Theme) {
    let filtered = app.filtered_prompt_history();
    let area = centered_rect(70, 40, frame.area());

    let mut lines: Vec<Line<'_>> = Vec::new();
    if filtered.is_empty() {
        lines.push(Line::styled(
            "(no matching history)",
            Style::default().fg(t.dim),
        ));
    }
    for (i, entry) in filtered.iter().take(20).enumerate() {
        let style = if i == app.history_search_index {
            Style::default().fg(t.selection_fg).bg(t.selection_bg)
        } else {
            Style::default().fg(t.dim)
        };
        let preview: String = entry.chars().take(80).collect();
        lines.push(Line::styled(preview, style));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.warning))
            .title(format!(
                " Prompt history — \"{}\" (↑↓ select · Enter apply · Esc cancel) ",
                app.history_search_query
            )),
    );
    frame.render_widget(paragraph, area);
}

/// Palette overhaul: render the `/help` overlay (scrollable, grouped).
fn render_help_overlay(frame: &mut Frame, app: &App, t: &Theme) {
    let area = centered_rect(82, 80, frame.area());
    let lines = build_help_lines(t);
    let total = lines.len();
    let visible_h = area.height.saturating_sub(2) as usize;

    // Clamp scroll to valid range (handles the `G` jump-to-bottom case).
    let max_scroll = total.saturating_sub(visible_h);
    let scroll = app.help_scroll.min(max_scroll);

    let paragraph = Paragraph::new(lines).scroll((scroll as u16, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.brand))
            .title(format!(
                " Slash command help  ({}/{})  ↑↓/jk scroll · g/G top/bottom · Esc/Enter/q close ",
                scroll, max_scroll
            )),
    );
    frame.render_widget(paragraph, area);
}

/// Build the scrollable content of the `/help` overlay from `BUILTIN_SPECS`
/// (single source of truth) plus key bindings.
fn build_help_lines(t: &Theme) -> Vec<Line<'static>> {
    use crate::slash::{BUILTIN_SPECS, SlashCategory};

    let cat_color = |cat: SlashCategory| -> Color {
        match cat {
            SlashCategory::General => t.cat_general,
            SlashCategory::Session => t.cat_session,
            SlashCategory::Model => t.cat_model,
            SlashCategory::Files => t.cat_files,
            SlashCategory::Context => t.cat_context,
            SlashCategory::Agents => t.cat_agents,
            SlashCategory::Skills => t.cat_skills,
            SlashCategory::System => t.cat_system,
        }
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Group specs by category order.
    let mut specs: Vec<&crate::slash::BuiltinSlashSpec> = BUILTIN_SPECS.iter().collect();
    specs.sort_by_key(|s| (s.category.order(), s.invoke));

    let mut last_cat: Option<SlashCategory> = None;
    for spec in &specs {
        if Some(spec.category) != last_cat {
            if last_cat.is_some() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::from(vec![Span::styled(
                format!(" {} ", spec.category.label().to_uppercase()),
                Style::default()
                    .fg(Color::Rgb(0x00, 0x00, 0x00))
                    .bg(cat_color(spec.category))
                    .add_modifier(Modifier::BOLD),
            )]));
            last_cat = Some(spec.category);
        }

        // Usage line.
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<20}", spec.usage),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(spec.description, Style::default().fg(t.dim)),
        ]));

        // Args hint line (if any).
        if !spec.args_hint.is_empty() {
            let mut spans = vec![Span::raw("    values: ")];
            for (i, v) in spec.args_hint.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" | ", Style::default().fg(t.dim)));
                }
                spans.push(Span::styled(*v, Style::default().fg(t.warning)));
            }
            lines.push(Line::from(spans));
        }

        // Example line.
        lines.push(Line::from(vec![
            Span::raw("    e.g. "),
            Span::styled(spec.example, Style::default().fg(t.success)),
        ]));
    }

    // Key bindings section.
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " KEY BINDINGS",
        Style::default()
            .fg(Color::Rgb(0x00, 0x00, 0x00))
            .bg(t.brand)
            .add_modifier(Modifier::BOLD),
    ));
    let bindings: &[(&str, &str)] = &[
        ("Esc", "Cancel streaming / close overlay"),
        ("Tab (streaming)", "Queue follow-up input for next turn"),
        ("Up/Down", "Draft history navigation"),
        ("PgUp / PgDn", "Scroll chat history"),
        ("Ctrl+Up/Down", "Scroll chat (fine)"),
        ("Ctrl+Home/End", "Jump to top / bottom of chat"),
        ("Ctrl+R", "Search prompt history"),
        ("Ctrl+L", "Clear screen (not conversation)"),
        ("Ctrl+O", "Copy latest output to clipboard"),
        ("Ctrl+E", "Open external editor ($EDITOR)"),
        ("Alt+Left/Right", "Switch agent thread"),
        ("/  (anytime)", "Open the slash command palette"),
        ("?", "Open this help overlay"),
    ];
    for (key, desc) in bindings {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<18}", key),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(*desc, Style::default().fg(t.dim)),
        ]));
    }

    lines
}

/// Helper: create a centered rectangle of given width/height percentages.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

// Keyboard shortcuts overlay (Ctrl+.).
fn render_shortcuts_overlay(frame: &mut Frame, _app: &App, t: &Theme) {
    let area = centered_rect(70, 70, frame.area());

    let mut lines: Vec<Line<'static>> = Vec::new();

    let header_style = Style::default().fg(t.brand).add_modifier(Modifier::BOLD);
    let key_style = Style::default().fg(t.accent).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(t.fg);
    let dim_style = Style::default().fg(t.dim);

    lines.push(Line::from(vec![Span::styled(
        "Keyboard Shortcuts",
        header_style,
    )]));
    lines.push(Line::styled("Press Esc or q to close", dim_style));
    lines.push(Line::raw(""));

    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "Essentials",
            &[
                ("Enter", "Send prompt (or newline in multiline mode)"),
                ("Ctrl+Enter", "Submit (in multiline mode)"),
                ("Esc", "Cancel running turn / close overlay"),
                ("Ctrl+C", "Quit"),
                ("Ctrl+D", "Quit"),
                ("q", "Quit (when input empty)"),
            ],
        ),
        (
            "Mode & Input",
            &[
                ("Shift+Tab", "Cycle mode: Normal -> Plan -> AlwaysApprove"),
                ("Ctrl+M", "Toggle multiline input"),
                ("Ctrl+R", "Search prompt history"),
                ("Ctrl+E", "Open external editor ($EDITOR)"),
                ("Ctrl+L", "Clear screen"),
                ("Ctrl+.", "Show this shortcuts overlay"),
            ],
        ),
        (
            "Chat & Output",
            &[
                ("PgUp / PgDn", "Scroll chat half page up/down"),
                ("Ctrl+Up/Dn", "Scroll chat 3 lines"),
                ("Ctrl+Home", "Jump to top"),
                ("Ctrl+End", "Jump to bottom"),
                ("Ctrl+O", "Copy latest assistant output"),
                ("Tab", "Queue follow-up while streaming"),
            ],
        ),
        (
            "Vim Mode (when enabled)",
            &[
                ("j / k", "Scroll down/up one line"),
                ("g / G", "Go to top / bottom"),
                ("Ctrl+U / Ctrl+D", "Scroll half page up/down"),
            ],
        ),
        (
            "Agent",
            &[
                ("Alt+Left/Right", "Switch prev/next agent thread"),
                ("o", "Jump to approval source thread"),
            ],
        ),
    ];

    for (title, items) in sections {
        lines.push(Line::from(vec![Span::styled(*title, header_style)]));
        for (key, desc) in *items {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<20} ", key), key_style),
                Span::styled(*desc, desc_style),
            ]));
        }
        lines.push(Line::raw(""));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.brand)),
    );
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use zerozero_llm::ChatMessage;

    fn render_to_string(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        format!("{:?}", terminal.backend().buffer())
    }

    #[test]
    fn test_render_empty_app() {
        let app = App::new();
        let content = render_to_string(&app);
        insta::assert_snapshot!(content);
    }

    #[test]
    fn test_render_with_messages() {
        let mut app = App::new();
        app.session_id = "test-session".to_string();
        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "hello world".to_string(),
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
        let content = render_to_string(&app);
        insta::assert_snapshot!(content);
    }

    #[test]
    fn test_render_streaming() {
        let mut app = App::new();
        app.session_id = "test-session".to_string();
        app.is_streaming = true;
        app.streaming_text = "I am generating".to_string();
        let content = render_to_string(&app);
        insta::assert_snapshot!(content);
    }

    #[test]
    fn test_render_input_buffer() {
        let mut app = App::new();
        app.set_input_buffer("fix the bug".to_string());
        let content = render_to_string(&app);
        insta::assert_snapshot!(content);
    }

    #[test]
    fn test_render_with_diff_view() {
        let mut app = App::new();
        app.session_id = "test-session".to_string();
        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "edit the file".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.diff_view = Some(crate::diff::DiffView::new(
            "src/main.rs",
            "fn main() {}",
            "fn main() {\n    println!(\"hi\");\n}",
        ));
        app.show_diff = true;
        let content = render_to_string(&app);
        insta::assert_snapshot!(content);
    }

    #[test]
    fn test_wrap_lines_uses_display_width_not_char_count() {
        // Fullwidth digits are 2 columns each in Unicode width.
        let wide = "１２３４５"; // 5 chars, 10 columns
        let mut lines = vec![Line::raw(wide.to_string())];
        wrap_lines(&mut lines, 6);
        assert!(
            lines.len() >= 2,
            "10-column string at width 6 must wrap (got {} lines)",
            lines.len()
        );
        for line in &lines {
            assert!(
                line.width() <= 6,
                "wrapped line width {} exceeds 6: {:?}",
                line.width(),
                line
            );
        }
    }

    #[test]
    fn test_wrap_lines_stable_for_ascii() {
        let mut lines = vec![Line::raw("hello world, this is a long line".to_string())];
        wrap_lines(&mut lines, 10);
        let first: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect();
        let mut lines2 = vec![Line::raw("hello world, this is a long line".to_string())];
        wrap_lines(&mut lines2, 10);
        let second: Vec<String> = lines2
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(first, second, "wrap must be deterministic");
        assert_eq!(first.concat(), "hello world, this is a long line");
    }

    #[test]
    fn test_chat_view_renders_markdown_for_agent() {
        let mut app = App::new();
        app.session_id = "test-session".to_string();
        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "show me code".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        app.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: "Run `cargo build` to compile.".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        let content = render_to_string(&app);
        // The inline code `cargo build` should be styled with the theme success color.
        let t = crate::theme::current();
        assert!(
            content.contains(&format!("{:?}", t.success)),
            "expected inline code to be styled with theme success color {:?}, got:\n{content}",
            t.success
        );
        // The code text itself should appear in the rendered output.
        assert!(content.contains("cargo build"));
        // The user message should appear with the "You" badge (not "user:").
        assert!(content.contains("show me code"));
        assert!(content.contains("You"));
    }

    #[test]
    fn test_chat_view_renders_system_message_styled() {
        let mut app = App::new();
        app.session_id = "test-session".to_string();
        app.messages.push(ChatMessage {
            role: "system".to_string(),
            content: "session restored".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });
        let content = render_to_string(&app);
        // System messages should be rendered in yellow-ish (warning) truecolor.
        // The system role uses the theme `system` color which is a truecolor.
        assert!(
            content.contains("session restored"),
            "system content should appear"
        );
    }
}
