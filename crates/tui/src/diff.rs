use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::theme::current as theme;

/// Diff view state.
pub struct DiffView {
    pub old_content: String,
    pub new_content: String,
    pub file_path: String,
}

impl DiffView {
    pub fn new(file_path: &str, old_content: &str, new_content: &str) -> Self {
        Self {
            old_content: old_content.to_string(),
            new_content: new_content.to_string(),
            file_path: file_path.to_string(),
        }
    }

    /// Generate unified diff lines with color coding.
    pub fn diff_lines(&self) -> Vec<Line<'static>> {
        let t = theme();
        let old_lines: Vec<String> = self.old_content.lines().map(|s| s.to_string()).collect();
        let new_lines: Vec<String> = self.new_content.lines().map(|s| s.to_string()).collect();
        let file_path = self.file_path.clone();

        let mut result: Vec<Line<'static>> = Vec::new();
        result.push(Line::from(vec![
            Span::styled("--- ", Style::default().fg(t.danger)),
            Span::styled(file_path.clone(), Style::default().fg(t.fg)),
        ]));
        result.push(Line::from(vec![
            Span::styled("+++ ", Style::default().fg(t.success)),
            Span::styled(file_path, Style::default().fg(t.fg)),
        ]));

        let max_len = old_lines.len().max(new_lines.len());
        for i in 0..max_len {
            let old = old_lines.get(i).map(|s| s.as_str()).unwrap_or("");
            let new = new_lines.get(i).map(|s| s.as_str()).unwrap_or("");

            if old == new {
                result.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(old.to_string(), Style::default().fg(t.dim)),
                ]));
            } else if !old.is_empty() && new.is_empty() {
                result.push(Line::from(vec![
                    Span::styled("- ", Style::default().fg(t.danger)),
                    Span::styled(old.to_string(), Style::default().fg(t.danger)),
                ]));
            } else if old.is_empty() && !new.is_empty() {
                result.push(Line::from(vec![
                    Span::styled("+ ", Style::default().fg(t.success)),
                    Span::styled(new.to_string(), Style::default().fg(t.success)),
                ]));
            } else {
                result.push(Line::from(vec![
                    Span::styled("- ", Style::default().fg(t.danger)),
                    Span::styled(old.to_string(), Style::default().fg(t.danger)),
                ]));
                result.push(Line::from(vec![
                    Span::styled("+ ", Style::default().fg(t.success)),
                    Span::styled(new.to_string(), Style::default().fg(t.success)),
                ]));
            }
        }

        result
    }
}

/// Render the diff view in a given area.
pub fn render_diff(frame: &mut Frame, area: Rect, diff: &DiffView) {
    let t = theme();
    let lines = diff.diff_lines();
    let title = format!(" Diff: {} ", diff.file_path);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD).fg(t.warning),
            )),
    );
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_identical() {
        let diff = DiffView::new("test.rs", "hello\nworld", "hello\nworld");
        let lines = diff.diff_lines();
        assert!(lines.len() >= 4);
        assert!(lines[2].spans.iter().any(|s| s.content.starts_with("  ")));
    }

    #[test]
    fn test_diff_added() {
        let diff = DiffView::new("test.rs", "", "new line");
        let lines = diff.diff_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content.starts_with("+ ")))
        );
    }

    #[test]
    fn test_diff_removed() {
        let diff = DiffView::new("test.rs", "old line", "");
        let lines = diff.diff_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content.starts_with("- ")))
        );
    }

    #[test]
    fn test_diff_modified() {
        let diff = DiffView::new("test.rs", "old", "new");
        let lines = diff.diff_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content.starts_with("- ")))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content.starts_with("+ ")))
        );
    }

    #[test]
    fn test_diff_header() {
        let diff = DiffView::new("main.rs", "a", "b");
        let lines = diff.diff_lines();
        assert!(lines[0].spans.iter().any(|s| s.content == "--- "));
        assert!(lines[1].spans.iter().any(|s| s.content == "+++ "));
    }
}
