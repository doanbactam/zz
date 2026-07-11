//! Composer view — render the input pane (PR-1: behavior parity with former
//! `ui::render_input`).

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use crate::theme::Theme;

/// Cursor glyph foreground — true black for contrast on the brand bg block.
const CURSOR_FG: ratatui::style::Color = ratatui::style::Color::Rgb(0x00, 0x00, 0x00);

/// Render the composer into `area` (parity with the pre-extract `render_input`).
pub fn render(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let c = &app.composer;
    let title = if app.show_history_search {
        format!(
            " History search — \"{}\" (↑↓ select · Enter apply · Esc cancel) ",
            app.history_search_query
        )
    } else if c.input_buffer.starts_with('/') && !app.is_streaming {
        let token = c
            .input_buffer
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        if c.show_slash_palette {
            let ghost = crate::slash_menu::selected_invoke(app).unwrap_or_default();
            if ghost.is_empty() {
                " Composer — /menu open · ↑↓ select · Tab complete · Esc ".to_string()
            } else {
                format!(" Composer — Tab/Enter → /{ghost}  ·  ↑↓  Esc ")
            }
        } else {
            let hints = crate::slash::slash_completions(token, &app.skill_slash_entries);
            if hints.is_empty() {
                " Composer (type / for commands) ".to_string()
            } else {
                format!(
                    " Composer — Tab: /{} (+{}) ",
                    hints[0],
                    hints.len().saturating_sub(1)
                )
            }
        }
    } else if app.is_streaming {
        " Composer (streaming…) ".to_string()
    } else {
        " Composer — Enter send · Alt+Enter newline · / commands · ? help ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            title,
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        ));

    if app.is_streaming {
        let text = if let Some(q) = &c.queued_input {
            format!("(streaming… queued: {q})")
        } else {
            "(streaming…)".to_string()
        };
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(t.dim))
            .block(block);
        frame.render_widget(paragraph, area);
        return;
    }

    let input = &c.input_buffer;
    // Snap cursor to a char boundary so multi-byte Vietnamese/CJK never panics
    // or splits mid-grapheme during render.
    let cursor = floor_char_boundary(input, c.cursor_pos.min(input.len()));

    // Placeholder when empty — discoverable, no need to open help.
    if input.is_empty() {
        let placeholder = Line::from(vec![
            Span::styled(
                "> ",
                Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Send a message…  ( / for commands · ? for help · ↑ for history )",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            ),
        ]);
        frame.render_widget(Paragraph::new(placeholder).block(block), area);
        return;
    }

    // Multiline-aware rendering: split the buffer into lines, highlighting the
    // cursor on the line that contains it.
    let prompt_span = Span::styled(
        "> ",
        Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
    );

    // Find which display line the cursor lands on.
    let before_cursor = &input[..cursor];
    let cursor_line_idx = before_cursor.chars().filter(|&c| c == '\n').count();

    let input_lines: Vec<&str> = input.split('\n').collect();
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Byte offset tracking to locate the cursor within its line.
    let mut line_start_byte = 0usize;
    for (li, raw) in input_lines.iter().enumerate() {
        let is_cursor_line = li == cursor_line_idx;
        if is_cursor_line {
            let local = floor_char_boundary(raw, cursor.saturating_sub(line_start_byte));
            let (before, cursor_char, after) = split_at_cursor(raw, local);
            let mut spans: Vec<Span<'static>> = Vec::new();
            if li == 0 {
                spans.push(prompt_span.clone());
            }
            spans.push(Span::styled(before, Style::default().fg(t.fg)));
            if cursor_char.is_empty() {
                spans.push(Span::styled(" ", Style::default().bg(t.brand)));
            } else {
                spans.push(Span::styled(
                    cursor_char,
                    Style::default().bg(t.brand).fg(CURSOR_FG),
                ));
                spans.push(Span::styled(after, Style::default().fg(t.fg)));
            }
            lines.push(Line::from(spans));
        } else if li == 0 {
            lines.push(Line::from(vec![
                prompt_span.clone(),
                Span::styled((*raw).to_string(), Style::default().fg(t.fg)),
            ]));
        } else {
            lines.push(Line::styled((*raw).to_string(), Style::default().fg(t.fg)));
        }
        // Advance past this line's bytes + the '\n' separator.
        line_start_byte += raw.len() + 1;
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// Floor `idx` to the nearest UTF-8 char boundary at or before it.
pub fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Split `raw` at byte offset `local` into (before, cursor_char, after).
pub fn split_at_cursor(raw: &str, local: usize) -> (String, String, String) {
    let local = floor_char_boundary(raw, local.min(raw.len()));
    let before = raw[..local].to_string();
    let cursor_char: String = raw[local..].chars().take(1).collect();
    let after = if local + cursor_char.len() <= raw.len() {
        raw[local + cursor_char.len()..].to_string()
    } else {
        String::new()
    };
    (before, cursor_char, after)
}
