//! Docked fuzzy slash command palette (opened while typing `/`).
//!
//! Sits above the input/status rows (Codex/Claude-style), not a full-screen
//! takeover. Two-pane list + detail, category grouping when the query is empty,
//! shared ranking with Tab/↑↓ so selection never desyncs from the list.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::App;
use crate::slash::{SlashCategory, SlashMenuItem, all_menu_items, ranked_for_palette};
use crate::theme::{Theme, current as theme};

/// Color per category tag (used in list + detail) — pulled from the active theme.
const fn category_color(t: &Theme, cat: SlashCategory) -> Color {
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
}

/// Number of selectable command rows for the current buffer query.
pub fn ranked_len(entries: &[zerozero_skills::SkillSlashEntry], input: &str) -> usize {
    let items = all_menu_items(entries);
    ranked_for_palette(&items, slash_query(input)).len()
}

/// Build the flat list of *display rows* (group headers + items) for the
/// current query, preserving the selection mapping.
struct PaletteRows {
    /// (item_index, is_header) per row; item_index is None for header rows.
    rows: Vec<(Option<usize>, bool)>,
    /// Index into `rows` of the currently selected item row.
    selected_row: usize,
}

fn build_palette_rows(
    items: &[SlashMenuItem],
    ranked: &[(usize, i32)],
    selected_item_row: usize,
    query: &str,
) -> PaletteRows {
    let mut rows: Vec<(Option<usize>, bool)> = Vec::new();
    let mut last_cat: Option<SlashCategory> = None;
    let mut selected_row = 0usize;

    let show_groups = query.trim().is_empty();

    for (item_row, &(idx, _)) in ranked.iter().enumerate() {
        let cat = items[idx].category;
        if show_groups && Some(cat) != last_cat {
            // Header row carries the first item's index so the renderer can
            // read the category label.
            rows.push((Some(idx), true));
            last_cat = Some(cat);
        }
        if item_row == selected_item_row {
            selected_row = rows.len();
        }
        rows.push((Some(idx), false));
    }
    PaletteRows { rows, selected_row }
}

pub fn render(frame: &mut Frame, app: &App) {
    let t = theme();
    // Dock above input+status so chat stays visible (not a full-screen takeover).
    let area = docked_rect(frame.area());
    let query = slash_query(&app.composer.input_buffer);
    let items = all_menu_items(&app.skill_slash_entries);
    let ranked = ranked_for_palette(&items, query);

    // Clamp selection to valid item-row range.
    let n_items = ranked.len();
    let selected_item_row = if n_items == 0 {
        0
    } else {
        app.composer.slash_menu_index.min(n_items - 1)
    };

    let palette = build_palette_rows(&items, &ranked, selected_item_row, query);

    // Ghost completion for the selected command (shown in title).
    let ghost = ranked
        .get(selected_item_row)
        .map(|&(idx, _)| items[idx].invoke.as_str())
        .unwrap_or("");

    // Two-pane layout: list (left) + detail (right).
    let filter_label = if query.is_empty() {
        "type to filter".to_string()
    } else {
        query.to_string()
    };
    let title = if ghost.is_empty() {
        format!(" /  filter: {filter_label}  ·  ↑↓/C-n/C-p  Tab  Enter  Esc ")
    } else {
        format!(" /  → /{ghost}  ·  filter: {filter_label}  ·  ↑↓/C-n/C-p  Tab  Enter  Esc ")
    };
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            title,
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(inner);

    render_list_pane(frame, cols[0], &items, &palette, query, &t);
    render_detail_pane(frame, cols[1], &items, &palette, &t);
}

fn render_list_pane(
    frame: &mut Frame,
    area: Rect,
    items: &[SlashMenuItem],
    palette: &PaletteRows,
    query: &str,
    t: &Theme,
) {
    let show_groups = query.trim().is_empty();

    // Compute scroll so the selected row stays visible.
    let visible_h = area.height.saturating_sub(2) as usize; // borders
    let total_rows = palette.rows.len();
    let sel = palette.selected_row;
    let half = visible_h / 2;
    let scroll = if total_rows <= visible_h || sel < half {
        0
    } else if sel + half >= total_rows {
        total_rows.saturating_sub(visible_h)
    } else {
        sel - half
    };

    let mut lines: Vec<Line<'_>> = Vec::new();
    for row_idx in scroll..total_rows.min(scroll + visible_h) {
        let (opt_idx, is_header) = &palette.rows[row_idx];
        let is_selected = row_idx == palette.selected_row;
        if *is_header {
            if let Some(idx) = opt_idx {
                let cat = items[*idx].category;
                lines.push(Line::from(vec![Span::styled(
                    format!(" {} ", cat.label().to_uppercase()),
                    Style::default()
                        .fg(Color::Rgb(0x00, 0x00, 0x00))
                        .bg(category_color(t, cat))
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            continue;
        }
        let Some(idx) = opt_idx else { continue };
        let item = &items[*idx];
        let marker = if is_selected { "▶ " } else { "  " };
        let invoke_style = if is_selected {
            Style::default()
                .fg(t.selection_fg)
                .bg(t.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD)
        };
        let desc_style = if is_selected {
            Style::default().fg(t.selection_fg).bg(t.selection_bg)
        } else {
            Style::default().fg(t.dim)
        };
        let cat_tag = if show_groups {
            String::new()
        } else {
            format!(" [{}]", item.category.label())
        };
        let mut spans = vec![Span::styled(marker, invoke_style)];
        spans.push(Span::styled(format!("/{}", item.invoke), invoke_style));
        spans.push(Span::styled(
            cat_tag,
            Style::default().fg(category_color(t, item.category)),
        ));
        spans.push(Span::styled("  ", desc_style));
        // Truncate description to fit.
        let max_desc = area.width.saturating_sub(8) as usize;
        let desc: String = item.description.chars().take(max_desc).collect();
        spans.push(Span::styled(desc, desc_style));
        lines.push(Line::from(spans));
    }

    if lines.is_empty() {
        lines.push(Line::styled(
            "(no matching commands)",
            Style::default().fg(t.dim),
        ));
    }

    let block = Block::default()
        .borders(Borders::LEFT | Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            " Commands ",
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_detail_pane(
    frame: &mut Frame,
    area: Rect,
    items: &[SlashMenuItem],
    palette: &PaletteRows,
    t: &Theme,
) {
    // Find the selected item.
    let selected_item: Option<&SlashMenuItem> =
        palette
            .rows
            .get(palette.selected_row)
            .and_then(|(opt, is_header)| {
                if *is_header {
                    None
                } else {
                    opt.and_then(|idx| items.get(idx))
                }
            });

    let block = Block::default()
        .borders(Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            " Detail ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ));

    let lines = match selected_item {
        None => vec![Line::styled(
            "(select a command)",
            Style::default().fg(t.dim),
        )],
        Some(item) => build_detail_lines(item, t),
    };

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

fn build_detail_lines(item: &SlashMenuItem, t: &Theme) -> Vec<Line<'static>> {
    let cat_color = category_color(t, item.category);
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Category tag + source.
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", item.category.label()),
            Style::default()
                .fg(Color::Rgb(0x00, 0x00, 0x00))
                .bg(cat_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("[{}]", item.source), Style::default().fg(t.dim)),
    ]));
    lines.push(Line::raw(""));

    // Usage signature.
    lines.push(Line::from(vec![
        Span::styled(
            "Usage  ",
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            item.usage.clone(),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
    ]));

    // Description.
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "What   ",
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        ),
        Span::styled(item.description.clone(), Style::default().fg(t.fg)),
    ]));

    // Argument hints (valid values).
    if !item.args_hint.is_empty() {
        lines.push(Line::raw(""));
        let mut spans = vec![Span::styled(
            "Args   ",
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        )];
        for (i, v) in item.args_hint.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" | ", Style::default().fg(t.dim)));
            }
            spans.push(Span::styled(v.clone(), Style::default().fg(t.warning)));
        }
        lines.push(Line::from(spans));
    }

    // Example.
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "Example",
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(item.example.clone(), Style::default().fg(t.success)),
    ]));

    lines
}

pub fn slash_query(input_buffer: &str) -> &str {
    input_buffer
        .strip_prefix('/')
        .map(|s| s.split_whitespace().next().unwrap_or(""))
        .unwrap_or("")
}

pub fn selected_invoke(app: &App) -> Option<String> {
    let items = all_menu_items(&app.skill_slash_entries);
    let ranked = ranked_for_palette(&items, slash_query(&app.composer.input_buffer));
    if ranked.is_empty() {
        return None;
    }
    let sel = app.composer.slash_menu_index.min(ranked.len() - 1);
    Some(items[ranked[sel].0].invoke.clone())
}

/// Selected menu item (full metadata) for Tab/Enter apply.
pub fn selected_item(app: &App) -> Option<SlashMenuItem> {
    let items = all_menu_items(&app.skill_slash_entries);
    let ranked = ranked_for_palette(&items, slash_query(&app.composer.input_buffer));
    if ranked.is_empty() {
        return None;
    }
    let sel = app.composer.slash_menu_index.min(ranked.len() - 1);
    Some(items[ranked[sel].0].clone())
}

/// Dock the palette just above the input + status rows so the chat stays visible.
fn docked_rect(area: Rect) -> Rect {
    // Reserve ~5 rows at bottom (input + status + margin).
    let bottom_reserve = 5u16.min(area.height.saturating_sub(4));
    let max_h = ((area.height as u32 * 42) / 100).clamp(8, 16) as u16;
    let height = max_h.min(area.height.saturating_sub(bottom_reserve));
    let y = area
        .height
        .saturating_sub(bottom_reserve.saturating_add(height));
    Rect {
        x: area.x,
        y: area.y.saturating_add(y),
        width: area.width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::ranked_for_palette;

    #[test]
    fn test_ranked_empty_query_groups_by_category() {
        let items = all_menu_items(&[]);
        let ranked = ranked_for_palette(&items, "");
        assert!(!ranked.is_empty());
        // Categories must be non-decreasing in order when query is empty.
        let mut prev = 0u8;
        for &(idx, _) in &ranked {
            let o = items[idx].category.order();
            assert!(o >= prev, "category order regressed: {prev} -> {o}");
            prev = o;
        }
    }

    #[test]
    fn test_ranked_nonempty_query_keeps_relevance() {
        let items = all_menu_items(&[]);
        let ranked = ranked_for_palette(&items, "model");
        assert!(ranked.iter().any(|&(idx, _)| items[idx].invoke == "model"));
    }

    #[test]
    fn test_build_palette_rows_includes_headers_when_empty_query() {
        let items = all_menu_items(&[]);
        let ranked = ranked_for_palette(&items, "");
        let palette = build_palette_rows(&items, &ranked, 0, "");
        assert!(palette.rows.iter().any(|(_, h)| *h), "expected header rows");
    }

    #[test]
    fn test_build_palette_rows_no_headers_when_query() {
        let items = all_menu_items(&[]);
        let ranked = ranked_for_palette(&items, "help");
        let palette = build_palette_rows(&items, &ranked, 0, "help");
        assert!(
            !palette.rows.iter().any(|(_, h)| *h),
            "no header rows when filtering"
        );
    }

    #[test]
    fn test_detail_lines_include_usage_and_example() {
        let items = all_menu_items(&[]);
        let model = items.iter().find(|i| i.invoke == "model").unwrap();
        let lines = build_detail_lines(model, &theme());
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("");
        assert!(joined.contains("/model"), "usage missing: {joined}");
        assert!(joined.contains("grok-4"), "example missing: {joined}");
    }

    #[test]
    fn test_effort_has_args_hint() {
        let items = all_menu_items(&[]);
        let effort = items.iter().find(|i| i.invoke == "effort").unwrap();
        assert_eq!(effort.args_hint, vec!["none", "low", "medium", "high"]);
    }
}
