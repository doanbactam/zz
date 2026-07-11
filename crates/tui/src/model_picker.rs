//! 3-tier model picker overlay (Codex-style).
//!
//! Tier 0: Provider   (xai, openai, anthropic, ollama)
//! Tier 1: Model      (per-provider predefined list)
//! Tier 2: Effort     (none/low/medium/high — only for reasoning models)
//!
//! Navigation: ↑↓ select within tier, →/Tab/Enter advance, ← go back,
//! Esc cancel. Selecting a non-reasoning model applies immediately
//! (skips tier 2).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::App;
use crate::model_catalog::{CATALOG, EFFORT_TIERS, find_model, find_provider};
use crate::theme::{Theme, current as theme};

/// Black foreground for glyphs on colored header bars.
const ON_BAR: Color = Color::Rgb(0x00, 0x00, 0x00);

pub fn render(frame: &mut Frame, app: &App) {
    let t = theme();
    let area = centered_rect(78, 70, frame.area());

    // Breadcrumb: Provider › Model › Effort
    let breadcrumb = format!(
        " {} › {} › {} ",
        app.picker_provider.if_empty("…"),
        if app.picker_model.is_empty() {
            "…".to_string()
        } else {
            app.picker_model.clone()
        },
        app.picker_effort,
    );

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            format!(
                " Model picker — {}  (↑↓ select · →/Tab/Enter next · ← back · Esc cancel) ",
                breadcrumb
            ),
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // 3-column layout: one per tier. The active tier is highlighted.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(28),
            Constraint::Percentage(40),
            Constraint::Percentage(32),
        ])
        .split(inner);

    render_tier_providers(frame, cols[0], app, &t);
    render_tier_models(frame, cols[1], app, &t);
    render_tier_effort(frame, cols[2], app, &t);
}

fn render_tier_providers(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let active = app.model_picker_tier == 0;
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::styled(
        " PROVIDER",
        Style::default()
            .fg(if active { ON_BAR } else { t.dim })
            .bg(if active { t.brand } else { Color::Reset })
            .add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(""));

    for (i, p) in CATALOG.iter().enumerate() {
        let selected = i == app.model_picker_index && active;
        let chosen = p.id == app.picker_provider;
        let marker = if selected {
            "▶ "
        } else if chosen {
            "● "
        } else {
            "  "
        };
        let style = if selected {
            Style::default()
                .fg(t.selection_fg)
                .bg(t.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else if chosen {
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.fg)
        };
        let local_tag = if p.local { " (local)" } else { "" };
        lines.push(Line::from(vec![
            Span::styled(marker, style),
            Span::styled(p.name, style),
            Span::styled(local_tag, Style::default().fg(t.dim)),
        ]));
        // API key hint.
        let key_hint = if p.api_key_env.is_empty() {
            "no key required".to_string()
        } else {
            format!("key: {}", p.api_key_env)
        };
        lines.push(Line::styled(
            format!("    {}", key_hint),
            Style::default().fg(t.dim),
        ));
    }

    let block = Block::default()
        .borders(Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            " 1 ",
            Style::default()
                .fg(if active { ON_BAR } else { Color::Reset })
                .bg(if active { t.brand } else { Color::Reset })
                .add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_tier_models(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let active = app.model_picker_tier == 1;
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::styled(
        " MODEL",
        Style::default()
            .fg(if active { ON_BAR } else { t.dim })
            .bg(if active { t.accent } else { Color::Reset })
            .add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(""));

    let provider = find_provider(&app.picker_provider);
    if let Some(p) = provider {
        for (i, m) in p.models.iter().enumerate() {
            let selected = i == app.model_picker_index && active;
            let chosen = m.id == app.picker_model;
            let marker = if selected {
                "▶ "
            } else if chosen {
                "● "
            } else {
                "  "
            };
            let style = if selected {
                Style::default()
                    .fg(t.selection_fg)
                    .bg(t.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else if chosen {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg)
            };
            let reasoning_tag = if m.reasoning { " ✦reasoning" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(m.name, style),
                Span::styled(reasoning_tag, Style::default().fg(t.cat_agents)),
            ]));
            // Description (wrapped).
            lines.push(Line::styled(
                format!("    {}", m.description),
                Style::default().fg(t.dim),
            ));
            lines.push(Line::styled(
                format!("    id: {}", m.id),
                Style::default().fg(t.dim),
            ));
        }
    }

    let block = Block::default()
        .borders(Borders::RIGHT | Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            " 2 ",
            Style::default()
                .fg(if active { ON_BAR } else { Color::Reset })
                .bg(if active { t.accent } else { Color::Reset })
                .add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

fn render_tier_effort(frame: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let active = app.model_picker_tier == 2;
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::styled(
        " EFFORT",
        Style::default()
            .fg(if active { ON_BAR } else { t.dim })
            .bg(if active { t.warning } else { Color::Reset })
            .add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(""));

    // Only show effort tier for reasoning-capable models.
    let supports_reasoning = find_model(&app.picker_provider, &app.picker_model)
        .map(|m| m.reasoning)
        .unwrap_or(false);

    if !supports_reasoning {
        lines.push(Line::styled(
            "(not applicable — selected model has no reasoning tier)",
            Style::default().fg(t.dim),
        ));
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Press Enter/→ to apply the current selection.",
            Style::default().fg(t.dim),
        ));
    } else {
        let labels = [
            ("none", "No reasoning param (provider default)"),
            ("low", "Fast/cheap — minimal thinking"),
            ("medium", "Balanced quality/latency (default)"),
            ("high", "Deep thinking for hard tasks"),
        ];
        for (i, effort) in EFFORT_TIERS.iter().enumerate() {
            let selected = i == app.model_picker_index && active;
            let chosen = *effort == app.picker_effort;
            let marker = if selected {
                "▶ "
            } else if chosen {
                "● "
            } else {
                "  "
            };
            let style = if selected {
                Style::default()
                    .fg(t.selection_fg)
                    .bg(t.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else if chosen {
                Style::default().fg(t.warning).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg)
            };
            let (name, desc) = labels[i];
            lines.push(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(name, style),
            ]));
            lines.push(Line::styled(
                format!("    {desc}"),
                Style::default().fg(t.dim),
            ));
        }
    }

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(t.border))
        .title(Span::styled(
            " 3 ",
            Style::default()
                .fg(if active { ON_BAR } else { Color::Reset })
                .bg(if active { t.warning } else { Color::Reset })
                .add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

/// Helper trait to use `if_empty` on String in the breadcrumb.
trait IfEmpty {
    fn if_empty(&self, fallback: &str) -> String;
}
impl IfEmpty for String {
    fn if_empty(&self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self.clone()
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_if_empty() {
        assert_eq!("x".to_string().if_empty("fallback"), "x");
        assert_eq!("".to_string().if_empty("fallback"), "fallback");
    }
}
