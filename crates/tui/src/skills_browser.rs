//! Full-screen skills browser (`/skills`).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::app::App;
use crate::theme::current as theme;

pub fn render(frame: &mut Frame, app: &App) {
    let t = theme();
    let area = fullscreen_rect(frame.area());
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(area);

    let rows: Vec<ListItem> = app
        .skill_slash_entries
        .iter()
        .map(|e| {
            let scope = match e.scope {
                zerozero_skills::SkillScope::Project => "project",
                zerozero_skills::SkillScope::User => "user",
            };
            let hint = if e.argument_hint.is_empty() {
                "<task>".to_string()
            } else {
                e.argument_hint.clone()
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("/{}", e.name),
                    Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  /{scope}:{} ", e.name),
                    Style::default().fg(t.dim),
                ),
                Span::styled(hint, Style::default().fg(t.cat_skills)),
                Span::styled(" — ", Style::default().fg(t.dim)),
                Span::styled(e.description.clone(), Style::default().fg(t.dim)),
            ]))
        })
        .collect();

    let title = " Skills — Enter: insert /name · Esc: close ";
    let list = List::new(if rows.is_empty() {
        vec![ListItem::new(Line::styled(
            "No user-invocable skills. Add SKILL.md under .zerozero/skills/ or commands/*.md",
            Style::default().fg(t.dim),
        ))]
    } else {
        rows
    })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(
                title,
                Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
            )),
    )
    .highlight_style(
        Style::default()
            .fg(t.selection_fg)
            .bg(t.selection_bg)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    let mut state = ListState::default();
    if !app.skill_slash_entries.is_empty() {
        let sel = app
            .skills_browser_index
            .min(app.skill_slash_entries.len() - 1);
        state.select(Some(sel));
    }

    frame.render_stateful_widget(list, chunks[0], &mut state);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Reload: ", Style::default().fg(t.dim)),
            Span::styled("/reload", Style::default().fg(t.brand)),
            Span::styled("  •  Chain: ", Style::default().fg(t.dim)),
            Span::styled("/skill1 /skill2 <task>", Style::default().fg(t.warning)),
        ]))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(t.border))
                .title(Span::styled(" Skills browser ", Style::default().fg(t.dim))),
        ),
        chunks[1],
    );
}

pub fn selected_skill_name(app: &App) -> Option<String> {
    app.skill_slash_entries
        .get(app.skills_browser_index)
        .map(|e| e.name.clone())
}

fn fullscreen_rect(area: Rect) -> Rect {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(3),
            Constraint::Percentage(94),
            Constraint::Percentage(3),
        ])
        .split(area)[1]
}
