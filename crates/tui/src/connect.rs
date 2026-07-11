//! `/connect` overlay — OpenCode-style provider API-key entry in the TUI.
//!
//! Flow:
//! 1. Pick a provider (↑↓ · Enter)
//! 2. Type the API key (masked) · Enter to save · Esc to cancel
//!
//! Keys are written to the auth store (`~/.config/zerozero/auth.json` /
//! `ZZ_AUTH_PATH`) via [`zerozero_llm::AuthStore`]. Environment variables
//! still win at resolution time.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::App;
use crate::theme::current as theme;
use zerozero_llm::{AuthStore, KeySource, PROVIDERS, key_source};

/// How many providers are listed in the picker.
pub fn provider_count() -> usize {
    PROVIDERS.len()
}

/// Status label for a provider's key (no secret leaked).
pub fn status_for(provider_id: &str) -> (&'static str, bool) {
    match key_source(provider_id) {
        KeySource::Env => ("set (env)", true),
        KeySource::AuthStore => ("set (auth)", true),
        KeySource::LegacyFallback => ("set (legacy)", true),
        KeySource::NotRequired => ("n/a local", true),
        KeySource::Missing => ("missing", false),
    }
}

/// Save `key` for `provider_id` into the auth store. Returns a user-facing
/// status line (never includes the raw key).
pub fn save_provider_key(provider_id: &str, key: &str) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("empty API key — nothing saved".to_string());
    }
    let spec = zerozero_llm::find_provider(provider_id)
        .ok_or_else(|| format!("unknown provider '{provider_id}'"))?;
    if !spec.requires_key {
        return Ok(format!(
            "Provider '{}' is local — no API key required.",
            spec.id
        ));
    }
    let mut store = AuthStore::load().map_err(|e| e.to_string())?;
    store.set(spec.id, key);
    store.save().map_err(|e| e.to_string())?;
    Ok(format!(
        "Saved API key for '{}' → {}",
        spec.id,
        zerozero_llm::auth_path().display()
    ))
}

/// Remove a stored key for `provider_id`.
pub fn remove_provider_key(provider_id: &str) -> Result<String, String> {
    let spec = zerozero_llm::find_provider(provider_id)
        .ok_or_else(|| format!("unknown provider '{provider_id}'"))?;
    let mut store = AuthStore::load().map_err(|e| e.to_string())?;
    if store.remove(spec.id) {
        store.save().map_err(|e| e.to_string())?;
        Ok(format!("Removed stored key for '{}'", spec.id))
    } else {
        Ok(format!(
            "No stored key for '{}' in auth.json (env keys are unchanged)",
            spec.id
        ))
    }
}

/// Mask a secret for display (`••••` + last 4 chars if long enough).
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    if n <= 4 {
        return "•".repeat(n);
    }
    let tail: String = chars[n - 4..].iter().collect();
    format!("{}{tail}", "•".repeat(n - 4))
}

/// Fully mask (no tail leak) while typing.
pub fn mask_typing(key: &str) -> String {
    "•".repeat(key.chars().count())
}

/// Render the connect overlay.
pub fn render(frame: &mut Frame, app: &App) {
    let t = theme();
    let area = centered_rect(72, 70, frame.area());
    frame.render_widget(Clear, area);

    match app.connect_stage {
        0 => render_provider_list(frame, area, app, &t),
        _ => render_key_entry(frame, area, app, &t),
    }
}

fn render_provider_list(frame: &mut Frame, area: Rect, app: &App, t: &crate::theme::Theme) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        " Select a provider, then enter its API key (OpenCode /connect parity)",
        Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
    )]));
    lines.push(Line::raw(""));

    for (i, p) in PROVIDERS.iter().enumerate() {
        let (status, ok) = status_for(p.id);
        let selected = i == app.connect_index;
        let style = if selected {
            Style::default().fg(t.selection_fg).bg(t.selection_bg)
        } else {
            Style::default().fg(t.fg)
        };
        let marker = if selected { "› " } else { "  " };
        let status_style = if selected {
            style
        } else if ok {
            Style::default().fg(t.success)
        } else {
            Style::default().fg(t.warning)
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{:<12}", p.id),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {:<22}", p.name), style),
            Span::styled(status.to_string(), status_style),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        " ↑↓ select · Enter connect · d remove stored key · Esc cancel ",
        Style::default().fg(t.dim),
    )]));

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.brand))
            .title(Span::styled(
                " /connect — providers ",
                Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(paragraph, area);
}

fn render_key_entry(frame: &mut Frame, area: Rect, app: &App, t: &crate::theme::Theme) {
    let provider = if app.connect_provider.is_empty() {
        "provider"
    } else {
        app.connect_provider.as_str()
    };
    let spec = zerozero_llm::find_provider(provider);
    let env_name = spec.map(|s| s.api_key_env).unwrap_or("API_KEY");
    let base = spec.map(|s| s.default_base_url).unwrap_or("");
    let requires = spec.map(|s| s.requires_key).unwrap_or(true);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" Provider: ", Style::default().fg(t.dim)),
        Span::styled(
            provider.to_string(),
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        ),
    ]));
    if !base.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" Endpoint: ", Style::default().fg(t.dim)),
            Span::styled(base.to_string(), Style::default().fg(t.fg)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled(" Env var:  ", Style::default().fg(t.dim)),
        Span::styled(env_name.to_string(), Style::default().fg(t.accent)),
    ]));
    lines.push(Line::raw(""));

    if !requires {
        lines.push(Line::from(vec![Span::styled(
            " This provider is local — no API key needed. Press Esc to go back.",
            Style::default().fg(t.success),
        )]));
    } else {
        let masked = mask_typing(&app.connect_key_buffer);
        let cursor = Span::styled(" ", Style::default().bg(t.brand));
        lines.push(Line::from(vec![Span::styled(
            " Paste or type your API key:",
            Style::default().fg(t.fg),
        )]));
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                "  > ",
                Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if masked.is_empty() {
                    "(empty)".to_string()
                } else {
                    masked
                },
                if app.connect_key_buffer.is_empty() {
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC)
                } else {
                    Style::default().fg(t.fg)
                },
            ),
            cursor,
        ]));
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![Span::styled(
            " Key is never shown in chat. Stored in auth.json (env still wins).",
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        )]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        " Enter save · Backspace edit · Esc back ",
        Style::default().fg(t.dim),
    )]));

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.brand))
            .title(Span::styled(
                format!(" /connect — {provider} "),
                Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(paragraph, area);
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
    fn mask_typing_hides_all() {
        // "sk-abc" is 6 ASCII chars → 6 bullets.
        assert_eq!(mask_typing("sk-abc").chars().count(), 6);
        assert_eq!(mask_typing(""), "");
    }

    #[test]
    fn mask_key_keeps_tail() {
        let m = mask_key("xai-abcdefghij");
        assert!(m.ends_with("ghij"), "{m}");
        assert!(!m.contains("xai-abcd"));
    }

    #[test]
    fn save_and_remove_via_store_path() {
        // Avoid ZZ_AUTH_PATH races with parallel tests — use explicit path I/O.
        let dir = std::env::temp_dir().join(format!(
            "zz-connect-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("auth.json");
        let mut store = AuthStore::new();
        store.set("xai", "xai-test-key-1234");
        store.save_to(&path).unwrap();
        assert!(path.exists());
        let loaded = AuthStore::load_from(&path).unwrap();
        assert_eq!(loaded.get("xai"), Some("xai-test-key-1234"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
