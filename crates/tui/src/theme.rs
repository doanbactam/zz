//! Centralized truecolor UI theme system — single source of truth for all
//! UI chrome colors across the TUI.
//!
//! Design goals (industry-standard TUI parity — helix/lazygit/atuin):
//! - Truecolor (RGB) palette instead of the 16-color `Color` enum, so colors
//!   are consistent and refined on modern terminals.
//! - One semantic name per role (`brand`, `assistant`, `success`, …) so every
//!   widget pulls from the same palette — no scattered `Color::Cyan` literals.
//! - Switchable at runtime via `/ui-theme <name>` (dark default, light variant).
//! - The syntect *code* theme (markdown.rs) stays separate; this module only
//!   owns UI chrome colors.
//!
//! Palettes are inspired by Tokyo Night (dark) and Catppuccin Latte (light),
//! both widely used in modern terminal tooling.

use ratatui::style::Color;
use std::sync::OnceLock;

/// A complete UI color palette (truecolor RGB).
///
/// Every field is a semantic role — widgets reference roles, never raw colors.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,
    // --- Brand & accents ---
    /// Primary brand color (ZeroZero wordmark, prompts, active highlights).
    pub brand: Color,
    /// Secondary accent (effort, model tier 2, detail pane titles).
    pub accent: Color,
    // --- Chat roles ---
    /// User badge / user content emphasis.
    pub user: Color,
    /// Assistant badge.
    pub assistant: Color,
    /// System messages.
    pub system: Color,
    /// Thinking / reasoning (dim).
    pub thinking: Color,
    // --- Status semantics ---
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    /// In-flight / running indicator.
    pub running: Color,
    // --- UI chrome ---
    /// Primary foreground text.
    pub fg: Color,
    /// Dimmed / secondary text.
    pub dim: Color,
    /// Borders & dividers.
    pub border: Color,
    /// Faint separator between chat messages.
    pub separator: Color,
    /// Selection highlight (background).
    pub selection_bg: Color,
    /// Selection highlight (foreground).
    pub selection_fg: Color,
    /// Tool-event name color.
    pub tool: Color,
    // --- Slash category accents ---
    pub cat_general: Color,
    pub cat_session: Color,
    pub cat_model: Color,
    pub cat_files: Color,
    pub cat_context: Color,
    pub cat_agents: Color,
    pub cat_skills: Color,
    pub cat_system: Color,
}

// ---- Built-in palettes ---------------------------------------------------

/// Tokyo Night Storm — the default dark palette.
const DARK: Theme = Theme {
    name: "tokyo-night",
    brand: Color::Rgb(0x7d, 0xcf, 0xff),
    accent: Color::Rgb(0xbb, 0x9a, 0xf7),
    user: Color::Rgb(0x7d, 0xcf, 0xff),
    assistant: Color::Rgb(0xbb, 0x9a, 0xf7),
    system: Color::Rgb(0xe0, 0xaf, 0x68),
    thinking: Color::Rgb(0x56, 0x5f, 0x89),
    success: Color::Rgb(0x9e, 0xce, 0x6a),
    warning: Color::Rgb(0xe0, 0xaf, 0x68),
    danger: Color::Rgb(0xf7, 0x76, 0x8e),
    running: Color::Rgb(0xe0, 0xaf, 0x68),
    fg: Color::Rgb(0xc0, 0xca, 0xf5),
    dim: Color::Rgb(0x56, 0x5f, 0x89),
    border: Color::Rgb(0x33, 0x3a, 0x4d),
    separator: Color::Rgb(0x24, 0x2a, 0x3c),
    selection_bg: Color::Rgb(0x28, 0x45, 0x73),
    selection_fg: Color::Rgb(0xc0, 0xca, 0xf5),
    tool: Color::Rgb(0x7a, 0xa2, 0xf7),
    cat_general: Color::Rgb(0x7d, 0xcf, 0xff),
    cat_session: Color::Rgb(0x9e, 0xce, 0x6a),
    cat_model: Color::Rgb(0xbb, 0x9a, 0xf7),
    cat_files: Color::Rgb(0x7a, 0xa2, 0xf7),
    cat_context: Color::Rgb(0xe0, 0xaf, 0x68),
    cat_agents: Color::Rgb(0xff, 0x9e, 0x64),
    cat_skills: Color::Rgb(0x2a, 0xc3, 0xde),
    cat_system: Color::Rgb(0x56, 0x5f, 0x89),
};

/// Catppuccin Latte — the light palette.
const LIGHT: Theme = Theme {
    name: "catppuccin-latte",
    brand: Color::Rgb(0x04, 0xa5, 0xe5),
    accent: Color::Rgb(0x88, 0x39, 0xef),
    user: Color::Rgb(0x04, 0xa5, 0xe5),
    assistant: Color::Rgb(0x88, 0x39, 0xef),
    system: Color::Rgb(0xdf, 0x8e, 0x1d),
    thinking: Color::Rgb(0x6c, 0x6f, 0x85),
    success: Color::Rgb(0x40, 0xa0, 0x2b),
    warning: Color::Rgb(0xdf, 0x8e, 0x1d),
    danger: Color::Rgb(0xd2, 0x0f, 0x39),
    running: Color::Rgb(0xdf, 0x8e, 0x1d),
    fg: Color::Rgb(0x4c, 0x4f, 0x69),
    dim: Color::Rgb(0x6c, 0x6f, 0x85),
    border: Color::Rgb(0xbc, 0xc0, 0xcc),
    separator: Color::Rgb(0xcc, 0xe0, 0xe6),
    selection_bg: Color::Rgb(0xcc, 0xd0, 0xda),
    selection_fg: Color::Rgb(0x4c, 0x4f, 0x69),
    tool: Color::Rgb(0x1e, 0x66, 0xf5),
    cat_general: Color::Rgb(0x04, 0xa5, 0xe5),
    cat_session: Color::Rgb(0x40, 0xa0, 0x2b),
    cat_model: Color::Rgb(0x88, 0x39, 0xef),
    cat_files: Color::Rgb(0x1e, 0x66, 0xf5),
    cat_context: Color::Rgb(0xdf, 0x8e, 0x1d),
    cat_agents: Color::Rgb(0xfe, 0x64, 0x40),
    cat_skills: Color::Rgb(0x17, 0x96, 0xb8),
    cat_system: Color::Rgb(0x6c, 0x6f, 0x85),
};

/// Nord — cool arctic dark palette.
const NORD: Theme = Theme {
    name: "nord",
    brand: Color::Rgb(0x88, 0xc0, 0xd0),
    accent: Color::Rgb(0xb4, 0x8e, 0xad),
    user: Color::Rgb(0x88, 0xc0, 0xd0),
    assistant: Color::Rgb(0xb4, 0x8e, 0xad),
    system: Color::Rgb(0xeb, 0xcb, 0x8b),
    thinking: Color::Rgb(0x4c, 0x56, 0x6a),
    success: Color::Rgb(0xa3, 0xbe, 0x8c),
    warning: Color::Rgb(0xeb, 0xcb, 0x8b),
    danger: Color::Rgb(0xbf, 0x61, 0x6a),
    running: Color::Rgb(0xeb, 0xcb, 0x8b),
    fg: Color::Rgb(0xec, 0xef, 0xf4),
    dim: Color::Rgb(0x4c, 0x56, 0x6a),
    border: Color::Rgb(0x3b, 0x42, 0x52),
    separator: Color::Rgb(0x2e, 0x34, 0x40),
    selection_bg: Color::Rgb(0x43, 0x4c, 0x5e),
    selection_fg: Color::Rgb(0xec, 0xef, 0xf4),
    tool: Color::Rgb(0x81, 0xa1, 0xc1),
    cat_general: Color::Rgb(0x88, 0xc0, 0xd0),
    cat_session: Color::Rgb(0xa3, 0xbe, 0x8c),
    cat_model: Color::Rgb(0xb4, 0x8e, 0xad),
    cat_files: Color::Rgb(0x81, 0xa1, 0xc1),
    cat_context: Color::Rgb(0xeb, 0xcb, 0x8b),
    cat_agents: Color::Rgb(0xd0, 0x87, 0x70),
    cat_skills: Color::Rgb(0x8f, 0xbc, 0xbb),
    cat_system: Color::Rgb(0x4c, 0x56, 0x6a),
};

/// Rosé Pine — warm muted dark.
const ROSE_PINE: Theme = Theme {
    name: "rose-pine",
    brand: Color::Rgb(0xeb, 0xbc, 0xba),
    accent: Color::Rgb(0xc4, 0xa7, 0xe7),
    user: Color::Rgb(0x9c, 0xcf, 0xd8),
    assistant: Color::Rgb(0xc4, 0xa7, 0xe7),
    system: Color::Rgb(0xf6, 0xc1, 0x77),
    thinking: Color::Rgb(0x6e, 0x6a, 0x86),
    success: Color::Rgb(0x9c, 0xcf, 0xd8),
    warning: Color::Rgb(0xf6, 0xc1, 0x77),
    danger: Color::Rgb(0xeb, 0x6f, 0x92),
    running: Color::Rgb(0xf6, 0xc1, 0x77),
    fg: Color::Rgb(0xe0, 0xde, 0xf4),
    dim: Color::Rgb(0x6e, 0x6a, 0x86),
    border: Color::Rgb(0x26, 0x23, 0x3a),
    separator: Color::Rgb(0x1f, 0x1d, 0x2e),
    selection_bg: Color::Rgb(0x40, 0x3d, 0x52),
    selection_fg: Color::Rgb(0xe0, 0xde, 0xf4),
    tool: Color::Rgb(0x31, 0x74, 0x8f),
    cat_general: Color::Rgb(0x9c, 0xcf, 0xd8),
    cat_session: Color::Rgb(0x9c, 0xcf, 0xd8),
    cat_model: Color::Rgb(0xc4, 0xa7, 0xe7),
    cat_files: Color::Rgb(0x31, 0x74, 0x8f),
    cat_context: Color::Rgb(0xf6, 0xc1, 0x77),
    cat_agents: Color::Rgb(0xeb, 0xbc, 0xba),
    cat_skills: Color::Rgb(0xeb, 0xbc, 0xba),
    cat_system: Color::Rgb(0x6e, 0x6a, 0x86),
};

/// All built-in themes, in display order.
const BUILTINS: &[&Theme] = &[&DARK, &LIGHT, &NORD, &ROSE_PINE];

static CURRENT: OnceLock<std::sync::Mutex<&'static Theme>> = OnceLock::new();

fn store() -> &'static std::sync::Mutex<&'static Theme> {
    CURRENT.get_or_init(|| std::sync::Mutex::new(&DARK))
}

/// The active theme (cloned — cheap, all fields are `Color`).
pub fn current() -> Theme {
    (*store().lock().unwrap()).clone()
}

/// Switch the active UI theme by name. Returns the applied theme on success,
/// or an error listing available names.
pub fn set_theme(name: &str) -> Result<Theme, String> {
    for &t in BUILTINS {
        if t.name == name {
            *store().lock().unwrap() = t;
            return Ok(t.clone());
        }
    }
    let available: Vec<&str> = BUILTINS.iter().map(|t| t.name).collect();
    Err(format!(
        "UI theme '{name}' not found. Available: {}",
        available.join(", ")
    ))
}

/// Names of all built-in UI themes (for the picker).
pub fn available_names() -> Vec<&'static str> {
    BUILTINS.iter().map(|t| t.name).collect()
}

/// Whether `name` is a built-in theme.
pub fn is_known(name: &str) -> bool {
    BUILTINS.iter().any(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_dark() {
        assert_eq!(current().name, "tokyo-night");
    }

    #[test]
    fn switch_to_light_and_back() {
        set_theme("catppuccin-latte").unwrap();
        assert_eq!(current().name, "catppuccin-latte");
        set_theme("tokyo-night").unwrap();
        assert_eq!(current().name, "tokyo-night");
    }

    #[test]
    fn unknown_theme_errors() {
        let err = set_theme("nope").unwrap_err();
        assert!(err.contains("tokyo-night"));
        assert!(err.contains("catppuccin-latte"));
        assert!(err.contains("nord"));
        assert!(err.contains("rose-pine"));
    }

    #[test]
    fn all_builtins_are_known() {
        for t in BUILTINS {
            assert!(is_known(t.name));
        }
    }

    #[test]
    fn themes_use_truecolor() {
        // Every chrome color should be RGB (truecolor), not a named enum,
        // so the palette is consistent on modern terminals.
        let t = current();
        let all = [
            t.brand,
            t.accent,
            t.user,
            t.assistant,
            t.system,
            t.thinking,
            t.success,
            t.warning,
            t.danger,
            t.running,
            t.fg,
            t.dim,
            t.border,
            t.separator,
            t.selection_bg,
            t.selection_fg,
            t.tool,
        ];
        for c in all {
            assert!(
                matches!(c, Color::Rgb(_, _, _)),
                "{c:?} should be truecolor"
            );
        }
    }
}
