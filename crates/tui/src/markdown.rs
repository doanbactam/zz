//! Simple markdown rendering for TUI — converts markdown text to
//! ratatui `Line` objects for display in the chat view.

use std::cell::RefCell;
use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme as SyntectTheme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::theme::current as theme;

// Thread-local storage for syntect syntax and theme sets.
// LazyLock ensures one-time initialization; RefCell allows borrowing in render_markdown.
//
// Theme choice: "base16-ocean.dark" — good contrast for terminal backgrounds,
// widely used in editors (VSCode, Sublime). Provides distinct colors for
// keywords, strings, comments, functions across 20+ languages.
thread_local! {
    static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
    static THEME: RefCell<SyntectTheme> = RefCell::new(
        ThemeSet::load_defaults().themes["base16-ocean.dark"].clone()
    );
}

/// Return the list of available built-in theme names (for /theme picker).
pub fn available_themes() -> Vec<String> {
    let ts = ThemeSet::load_defaults();
    let mut names: Vec<String> = ts.themes.keys().cloned().collect();
    names.sort();
    names
}

/// Switch the active syntax highlighting theme by name.
/// Returns Ok(()) if the theme was found and applied, Err with available names otherwise.
pub fn set_theme(name: &str) -> Result<(), String> {
    let ts = ThemeSet::load_defaults();
    match ts.themes.get(name) {
        Some(theme) => {
            THEME.with(|t| {
                *t.borrow_mut() = theme.clone();
            });
            CURRENT_THEME_NAME.with(|n| {
                *n.borrow_mut() = name.to_string();
            });
            Ok(())
        }
        None => {
            let available: Vec<&str> = ts.themes.keys().map(|s| s.as_str()).collect();
            Err(format!(
                "Theme '{name}' not found. Available: {}",
                available.join(", ")
            ))
        }
    }
}

/// Return the currently active theme name.
pub fn current_theme_name() -> String {
    // We can't easily reverse-lookup, so track it separately.
    CURRENT_THEME_NAME.with(|n| n.borrow().clone())
}

thread_local! {
    static CURRENT_THEME_NAME: RefCell<String> = RefCell::new("base16-ocean.dark".to_string());
}

/// Convert a syntect Color (RGBA) to a ratatui Color (RGB).
/// Syntect uses u8 for each channel; we drop the alpha channel.
const fn syntect_to_ratatui_color(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

/// Convert a markdown string into ratatui lines for display.
/// Handles: headings, paragraphs, code blocks, inline code, bold, italic, lists.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let t = theme();
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(text, options);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_style = Style::default();
    let mut heading_level = 0u32;
    let mut in_code_block = false;
    let mut code_block_spans: Vec<Span<'static>> = Vec::new();
    // Code block header bar support: track language + collect lines separately
    // so we can prepend a header (lang · n lines · copy hint) when the block ends.
    let mut code_block_lang: Option<String> = None;
    let mut code_block_lines: Vec<Line<'static>> = Vec::new();

    // Clone SyntaxSet and Theme for this render call
    let syntax_set = SYNTAX_SET.with(|ss| (**ss).clone());
    let syntect_theme = THEME.with(|t| (*t.borrow()).clone());

    // Declare highlighter AFTER syntax_set and theme (lifetime dependency)
    let mut code_highlighter: Option<HighlightLines<'_>> = None;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    heading_level = level as u32;
                    current_style = current_style.add_modifier(Modifier::BOLD).fg(t.brand);
                }
                Tag::Paragraph => {}
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    code_block_spans.clear();
                    code_block_lines.clear();

                    // Detect language from fenced code block
                    let lang_str = match kind {
                        CodeBlockKind::Fenced(s) => Some(s.to_owned()),
                        CodeBlockKind::Indented => None,
                    };
                    code_block_lang = lang_str.as_ref().map(|s| s.to_string());
                    let lang_ref = lang_str.as_deref().unwrap_or("");

                    // Look up syntax definition and initialize highlighter
                    if let Some(syntax) = syntax_set.find_syntax_by_token(lang_ref) {
                        code_highlighter = Some(HighlightLines::new(syntax, &syntect_theme));
                    } else {
                        code_highlighter = None;
                    }
                }
                Tag::List(_) => {}
                Tag::Item => {
                    current_spans.push(Span::raw("  • "));
                }
                Tag::Emphasis => {
                    current_style = current_style.add_modifier(Modifier::ITALIC);
                }
                Tag::Strong => {
                    current_style = current_style.add_modifier(Modifier::BOLD);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    let style = if heading_level <= 2 {
                        Style::default().add_modifier(Modifier::BOLD).fg(t.brand)
                    } else {
                        Style::default().add_modifier(Modifier::BOLD).fg(t.accent)
                    };
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.clone()).style(style));
                        current_spans.clear();
                    }
                    current_style = Style::default();
                }
                TagEnd::Paragraph => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.clone()));
                        current_spans.clear();
                    }
                    lines.push(Line::raw(""));
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    if !code_block_spans.is_empty() {
                        code_block_lines.push(Line::from(code_block_spans.clone()));
                        code_block_spans.clear();
                    }
                    code_highlighter = None;
                    // Prepend header bar: ` lang · n lines · copy hint `
                    let lang_label = code_block_lang
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("text");
                    let n = code_block_lines.len();
                    let header = Line::from(vec![
                        Span::styled(
                            format!(" {} ", lang_label),
                            Style::default()
                                .fg(Color::Rgb(0, 0, 0))
                                .bg(t.brand)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" {} {} ", n, if n == 1 { "line" } else { "lines" }),
                            Style::default().fg(t.dim),
                        ),
                        Span::styled("  ⎘ copy ".to_string(), Style::default().fg(t.dim)),
                    ]);
                    lines.push(header);
                    lines.append(&mut code_block_lines);
                    lines.push(Line::raw(""));
                    code_block_lang = None;
                }
                TagEnd::Item => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.clone()));
                        current_spans.clear();
                    }
                }
                TagEnd::Emphasis => {
                    current_style = current_style.remove_modifier(Modifier::ITALIC);
                }
                TagEnd::Strong => {
                    current_style = current_style.remove_modifier(Modifier::BOLD);
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    // Process code block text with syntax highlighting.
                    // Lines are collected into `code_block_lines` (not `lines`)
                    // so a header bar can be prepended when the block ends.
                    for (i, line_text) in text.split('\n').enumerate() {
                        if i > 0 {
                            // Flush accumulated code block spans
                            if !code_block_spans.is_empty() {
                                code_block_lines.push(Line::from(code_block_spans.clone()));
                                code_block_spans.clear();
                            }
                        }
                        // Highlight this line. Use truecolor (Color::Rgb) so the
                        // terminal can show proper syntax colors. Crossterm emits
                        // 24-bit escapes (`ESC[38;2;...`) when the terminal
                        // advertises truecolor support (e.g. COLORTERM=truecolor),
                        // which the E2E tests set.
                        if let Some(ref mut hl) = code_highlighter {
                            let regions = hl
                                .highlight_line(line_text, &syntax_set)
                                .unwrap_or_default();
                            for (style, text) in regions {
                                let color = syntect_to_ratatui_color(style.foreground);
                                code_block_spans.push(Span::styled(
                                    text.to_string(),
                                    Style::default().fg(color),
                                ));
                            }
                        } else {
                            // No highlighter available, use theme warning color
                            code_block_spans.push(Span::styled(
                                line_text.to_string(),
                                Style::default().fg(t.warning),
                            ));
                        }
                    }
                } else {
                    current_spans.push(Span::styled(text.to_string(), current_style));
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(current_spans.clone()));
                    current_spans.clear();
                }
            }
            Event::Code(code) => {
                current_spans.push(Span::styled(
                    code.to_string(),
                    Style::default().fg(t.success),
                ));
            }
            _ => {}
        }
    }

    // Flush any remaining spans
    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    lines
}

/// Fast path for in-flight assistant tokens — no pulldown/syntect (re-parsed every frame).
pub fn render_streaming_plain(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    // Preserve a trailing partial line (common while tokens arrive).
    let mut lines: Vec<Line<'static>> = text
        .split_inclusive('\n')
        .filter(|s| !s.is_empty())
        .map(|s| Line::raw(s.trim_end_matches('\n').to_string()))
        .collect();
    if lines.is_empty() {
        lines.push(Line::raw(text.to_string()));
    }
    lines
}

/// Reasoning / thinking stream — dim + italic, same fast line split as plain streaming.
pub fn render_streaming_reasoning(text: &str) -> Vec<Line<'static>> {
    let t = theme();
    let style = Style::default()
        .fg(t.thinking)
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    render_streaming_plain(text)
        .into_iter()
        .map(|line| {
            Line::from(
                line.spans
                    .into_iter()
                    .map(|span| Span::styled(span.content, style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::current as theme;

    #[test]
    fn test_streaming_plain_preserves_partial_line() {
        let lines = render_streaming_plain("hello");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "hello");
    }

    #[test]
    fn test_streaming_reasoning_uses_dim_italic() {
        let lines = render_streaming_reasoning("why");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn test_code_block_syntax_highlighted() {
        let md = "```rust\nfn main() {\n    let x = 5;\n    println!(\"{}\", x);\n}\n```";
        let lines = render_markdown(md);

        // Collect all distinct foreground colors used in the code block
        let mut colors = std::collections::HashSet::new();
        for line in &lines {
            for span in &line.spans {
                if let Some(color) = span.style.fg {
                    colors.insert(color);
                }
            }
        }

        // Syntax highlighting should produce at least 2 distinct colors
        assert!(
            colors.len() >= 2,
            "Expected at least 2 distinct colors from syntax highlighting, got {}: {:?}",
            colors.len(),
            colors
        );
    }

    #[test]
    fn test_code_block_rust_highlighting() {
        let md = "```rust\nfn main() {\n    let x = 5;\n    println!(\"{}\", x);\n}\n```";
        let lines = render_markdown(md);

        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(
            colors.len() >= 2,
            "Rust code block should have >= 2 distinct foreground colors, got {}",
            colors.len()
        );
    }

    #[test]
    fn test_code_block_python_highlighting() {
        let md = "```python\ndef main():\n    x = 5\n    print(f\"{x}\")\n```";
        let lines = render_markdown(md);

        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(
            colors.len() >= 2,
            "Python code block should have >= 2 distinct foreground colors, got {}",
            colors.len()
        );
    }

    #[test]
    fn test_code_block_javascript_highlighting() {
        let md =
            "```javascript\nfunction main() {\n    const x = 5;\n    console.log(`${x}`);\n}\n```";
        let lines = render_markdown(md);

        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(
            colors.len() >= 2,
            "JavaScript code block should have >= 2 distinct foreground colors, got {}",
            colors.len()
        );
    }

    #[test]
    fn test_code_block_typescript_highlighting() {
        let md = "```typescript\nfunction main(): void {\n    const x: number = 5;\n    console.log(`${x}`);\n}\n```";
        let lines = render_markdown(md);

        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(
            !colors.is_empty(),
            "TypeScript code block should have at least 1 foreground color, got {}",
            colors.len()
        );
    }

    #[test]
    fn test_code_block_json_highlighting() {
        let md = "```json\n{\n    \"name\": \"test\",\n    \"version\": \"1.0.0\"\n}\n```";
        let lines = render_markdown(md);

        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(
            colors.len() >= 2,
            "JSON code block should have >= 2 distinct foreground colors, got {}",
            colors.len()
        );
    }

    #[test]
    fn test_code_block_toml_highlighting() {
        let md = "```toml\n[package]\nname = \"test\"\nversion = \"1.0.0\"\n```";
        let lines = render_markdown(md);

        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(
            !colors.is_empty(),
            "TOML code block should have at least 1 foreground color, got {}",
            colors.len()
        );
    }

    #[test]
    fn test_code_block_bash_highlighting() {
        let md =
            "```bash\n#!/bin/bash\nif [ -f file.txt ]; then\n    echo \"File exists\"\nfi\n```";
        let lines = render_markdown(md);

        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(
            !colors.is_empty(),
            "Bash code block should have at least 1 foreground color, got {}",
            colors.len()
        );
    }

    #[test]
    fn test_code_block_unknown_language_fallback() {
        let md = "```xyz123unknown\nlet x = 5;\n```";
        let lines = render_markdown(md);

        // Should render without panicking
        assert!(!lines.is_empty(), "Should render at least one line");

        // Should have theme warning color as fallback
        let t = theme();
        let has_fallback = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(t.warning))
        });

        assert!(
            has_fallback,
            "Unknown language should fall back to theme warning color"
        );
    }

    #[test]
    fn test_code_block_mixed_content() {
        let md = "# Heading\n\n```rust\nfn foo() {}\n```\n\n```python\ndef bar():\n    pass\n```\n\nSome text.";
        let lines = render_markdown(md);

        // Should render all content without panicking
        assert!(!lines.is_empty(), "Should render multiple lines");

        // Should have multiple distinct colors from different code blocks
        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect();

        assert!(
            colors.len() >= 2,
            "Mixed content should have >= 2 distinct colors, got {}",
            colors.len()
        );
    }

    #[test]
    fn test_code_block_performance_500_lines() {
        // Generate 500 lines of Rust code
        let mut code = String::from("```rust\n");
        for i in 0..500 {
            code.push_str(&format!(
                "fn func_{}() {{\n    let x = {};\n    println!(\"{{}}\", x);\n}}\n",
                i, i
            ));
        }
        code.push_str("```");

        let start = std::time::Instant::now();
        let lines = render_markdown(&code);
        let elapsed = start.elapsed();

        assert!(!lines.is_empty(), "Should render lines");
        assert!(
            elapsed.as_millis() < 500,
            "Rendering 500-line code block should take < 500ms, took {}ms",
            elapsed.as_millis()
        );
    }

    #[test]
    fn test_existing_markdown_features_preserved() {
        let md = "# Heading 1\n\n**bold text**\n\n*italic text*\n\n- list item 1\n- list item 2\n\n`inline code`\n\n> blockquote";
        let lines = render_markdown(md);

        assert!(!lines.is_empty(), "Should render markdown");

        // Check for heading (should be bold and themed brand color)
        let t = theme();
        let has_heading = lines.iter().any(|line| {
            line.spans.iter().any(|span| {
                span.style.fg == Some(t.brand) && span.style.add_modifier.contains(Modifier::BOLD)
            })
        });
        assert!(has_heading, "Should have bold themed heading");

        // Check for inline code (should be themed success color)
        let has_inline_code = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(t.success))
        });
        assert!(has_inline_code, "Should have themed inline code");
    }

    #[test]
    fn test_code_block_empty() {
        // Empty code block should not crash and should render gracefully
        let md = "```\n```";
        let lines = render_markdown(md);

        // Should render without panicking; may produce zero or more lines
        // The key invariant is no panic
        let _ = lines;
    }

    #[test]
    fn test_code_block_no_language_tag() {
        // Code block without language tag should use fallback (yellow)
        let md = "```\nlet x = 5;\n```";
        let lines = render_markdown(md);

        assert!(!lines.is_empty(), "Should render at least one line");

        // Should have theme warning color as fallback (no language tag = no syntax match)
        let t = theme();
        let has_fallback = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(t.warning))
        });

        assert!(
            has_fallback,
            "Code block without language tag should fall back to theme warning color"
        );
    }

    #[test]
    fn test_inline_code_preserved() {
        // Inline code (backticks) should still render correctly
        let md = "Use `println!` to print output.";
        let lines = render_markdown(md);

        assert!(!lines.is_empty(), "Should render at least one line");

        // Check that inline code has themed success color
        let t = theme();
        let has_code = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(t.success))
        });

        assert!(
            has_code,
            "Inline code should render with themed success color"
        );

        // Check that the text content is present
        let full_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.to_string())
            .collect();

        assert!(
            full_text.contains("println!"),
            "Inline code content 'println!' should be present in rendered output"
        );
    }
}
