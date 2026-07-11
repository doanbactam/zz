//! Interactive prompt **composer** (Codex-style input box).
//!
//! - Extract state that lived flat on [`crate::app::App`].
//! - Own the input pane render path (behavior parity with former `ui::render_input`).
//! - Expose [`ComposerState::height`] for layout.
//!
//! Later PRs add soft-wrap, paste safety, queue chips, and composer-owned keymap.

mod state;
mod view;

pub use state::ComposerState;
pub use view::{floor_char_boundary, render, split_at_cursor};
