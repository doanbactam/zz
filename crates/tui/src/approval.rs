use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::theme::current as theme;

/// Approval modal state.
pub struct ApprovalModal {
    pub tool_name: String,
    pub args: String,
    pub danger_level: String,
    pub selected: usize, // 0 = Approve, 1 = Deny
}

impl ApprovalModal {
    pub fn new(tool_name: &str, args: &str, danger_level: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            args: args.to_string(),
            danger_level: danger_level.to_string(),
            selected: 0,
        }
    }

    pub const fn toggle(&mut self) {
        self.selected = 1 - self.selected;
    }

    pub const fn is_approve(&self) -> bool {
        self.selected == 0
    }

    pub const fn handle_left(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub const fn handle_right(&mut self) {
        if self.selected < 1 {
            self.selected += 1;
        }
    }
}

/// Render the approval modal as a centered popup.
pub fn render_approval_modal(frame: &mut Frame, modal: &ApprovalModal) {
    let t = theme();
    let area = centered_rect(60, 40, frame.area());
    frame.render_widget(Clear, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);

    let danger_color = match modal.danger_level.as_str() {
        "critical" | "warning" => t.danger,
        "caution" => t.warning,
        _ => t.success,
    };

    let title = Line::from(vec![Span::styled(
        " Approval Required ",
        Style::default().add_modifier(Modifier::BOLD).fg(t.danger),
    )]);
    frame.render_widget(
        Paragraph::new(title).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(danger_color)),
        ),
        chunks[0],
    );

    let info = vec![
        Line::from(vec![
            Span::styled(
                "Tool: ",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            ),
            Span::styled(&modal.tool_name, Style::default().fg(t.fg)),
        ]),
        Line::from(vec![
            Span::styled(
                "Danger: ",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            ),
            Span::styled(&modal.danger_level, Style::default().fg(danger_color)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "Args: ",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            ),
            Span::styled(&modal.args, Style::default().fg(t.fg)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(info).block(
            Block::default()
                .borders(Borders::LEFT | Borders::RIGHT)
                .border_style(Style::default().fg(t.border)),
        ),
        chunks[1],
    );

    let btn_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[2]);

    let approve_style = if modal.selected == 0 {
        Style::default()
            .bg(t.success)
            .fg(t.selection_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.dim)
    };
    let deny_style = if modal.selected == 1 {
        Style::default()
            .bg(t.danger)
            .fg(t.selection_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.dim)
    };

    frame.render_widget(
        Paragraph::new(" [Approve] ")
            .alignment(ratatui::layout::Alignment::Center)
            .style(approve_style),
        btn_chunks[0],
    );
    frame.render_widget(
        Paragraph::new(" [Deny] ")
            .alignment(ratatui::layout::Alignment::Center)
            .style(deny_style),
        btn_chunks[1],
    );
}

/// Create a centered rect of given width% and height%.
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
    fn test_approval_modal_new() {
        let modal = ApprovalModal::new("bash", "rm -rf /", "critical");
        assert_eq!(modal.tool_name, "bash");
        assert_eq!(modal.selected, 0);
        assert!(modal.is_approve());
    }

    #[test]
    fn test_approval_modal_toggle() {
        let mut modal = ApprovalModal::new("bash", "ls", "safe");
        assert!(modal.is_approve());
        modal.toggle();
        assert!(!modal.is_approve());
        modal.toggle();
        assert!(modal.is_approve());
    }

    #[test]
    fn test_approval_modal_left_right() {
        let mut modal = ApprovalModal::new("bash", "ls", "safe");
        modal.handle_right();
        assert_eq!(modal.selected, 1);
        modal.handle_right();
        assert_eq!(modal.selected, 1);
        modal.handle_left();
        assert_eq!(modal.selected, 0);
        modal.handle_left();
        assert_eq!(modal.selected, 0);
    }
}
