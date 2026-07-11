//! Agent tree view rendering Phase 5).
//!
//! Renders an indented list of all agent threads sorted by `agent_path`.
//! Each row shows the agent path, nickname, and status icon:
//! - `▶` Running
//! - `■` Stopped
//! - `✓` Completed
//! - `✗` Failed

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use crate::theme::{Theme, current as theme};
use zerozero_multi_agent::AgentStatus;

/// Render the agent tree view as a panel.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = theme();
    let lines = build_tree_lines(app, &t);
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(
                " Agents ",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(paragraph, area);
}

/// Build the tree view lines from the live agents list.
fn build_tree_lines(app: &App, t: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Agents are already sorted by agent_path in live_agents().
    for meta in &app.live_agents {
        let depth = meta.agent_path.as_str().matches('.').count();
        let indent = "  ".repeat(depth);

        let (status_icon, status_color) = match meta.status {
            AgentStatus::Running => ("▶", t.running),
            AgentStatus::Stopped => ("■", t.warning),
            AgentStatus::Completed => ("✓", t.success),
            AgentStatus::Failed => ("✗", t.danger),
        };

        let is_active = meta.thread_id == app.active_thread_id;
        let prefix = if is_active { "* " } else { "  " };

        let line = Line::from(vec![
            Span::raw(prefix),
            Span::raw(indent.clone()),
            Span::styled(status_icon, Style::default().fg(status_color)),
            Span::styled(
                format!(" {} ", meta.nickname),
                Style::default()
                    .fg(if is_active { t.fg } else { t.dim })
                    .add_modifier(if is_active {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(format!("({})", meta.agent_path), Style::default().fg(t.dim)),
        ]);
        lines.push(line);
    }

    if lines.is_empty() {
        lines.push(Line::styled("  (no agents)", Style::default().fg(t.dim)));
    }

    lines
}
