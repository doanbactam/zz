//! Agent picker popup rendering Phase 3).
//!
//! Renders a centered popup listing all live agent threads. Each row shows
//! the agent path, nickname, and status. The user navigates with Up/Down
//! and selects with Enter (handled in `app.rs handle_agent_picker_key`).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

use crate::app::App;
use crate::theme::current as theme;
use zerozero_multi_agent::AgentStatus;

/// Render the agent picker popup as a centered overlay.
pub fn render(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 50, frame.area());
    frame.render_widget(Clear, area);
    render_picker(frame, area, app);
}

fn render_picker(frame: &mut Frame, area: Rect, app: &App) {
    let t = theme();
    let items: Vec<ListItem> = app
        .live_agents
        .iter()
        .map(|meta| {
            let (status_str, status_color) = match meta.status {
                AgentStatus::Running => ("Running", t.running),
                AgentStatus::Stopped => ("Stopped", t.warning),
                AgentStatus::Completed => ("Completed", t.success),
                AgentStatus::Failed => ("Failed", t.danger),
            };
            let is_active = meta.thread_id == app.active_thread_id;
            let marker = if is_active { "● " } else { "○ " };
            let line = Line::from(vec![
                Span::styled(marker, Style::default().fg(status_color)),
                Span::styled(
                    meta.nickname.clone(),
                    Style::default().fg(t.fg).add_modifier(if is_active {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
                Span::styled(
                    format!(" ({}) ", meta.agent_path),
                    Style::default().fg(t.dim),
                ),
                Span::styled(status_str, Style::default().fg(status_color)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border))
                .title(Span::styled(
                    " Agent Threads — ↑↓ navigate · Enter select · Esc close ",
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
    state.select(Some(app.agent_picker_selected));

    frame.render_stateful_widget(list, area, &mut state);
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
