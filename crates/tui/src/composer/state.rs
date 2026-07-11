//! Composer state — buffer, cursor, slash palette, queue, draft history, images.
//!
//! Extract fields that used to live flat on `App` so later PRs can own
//! keymap/paste/soft-wrap without touching the whole TUI.

/// Interactive prompt composer state (Codex-style input box).
#[derive(Debug, Clone, Default)]
pub struct ComposerState {
    /// Text currently being edited.
    pub input_buffer: String,
    /// Cursor byte offset (always on a UTF-8 char boundary after edits).
    pub cursor_pos: usize,
    /// Whether the slash command palette is open (token phase of `/…`).
    pub show_slash_palette: bool,
    /// Selected row in the slash palette (into `ranked_for_palette` order).
    pub slash_menu_index: usize,
    /// Draft history for Up/Down navigation (Codex parity).
    pub draft_history: Vec<String>,
    /// Index while browsing draft history (`None` = not browsing).
    pub draft_history_index: Option<usize>,
    /// Follow-up queued via Tab while streaming.
    pub queued_input: Option<String>,
    /// Pending image attachments (data-URLs) for the next message.
    pub pending_images: Vec<String>,
}

impl ComposerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the buffer and move the cursor to the end.
    pub fn set_input_buffer(&mut self, s: String) {
        self.input_buffer = s;
        self.sync_cursor_to_end();
    }

    /// Sync `cursor_pos` to end of `input_buffer`.
    pub fn sync_cursor_to_end(&mut self) {
        self.cursor_pos = self.input_buffer.len();
    }

    /// Clear buffer + cursor + slash palette (after submit / slash run).
    pub fn clear_input(&mut self) {
        self.input_buffer.clear();
        self.cursor_pos = 0;
        self.show_slash_palette = false;
        self.slash_menu_index = 0;
    }

    /// Byte offset of the previous UTF-8 char boundary before `cursor_pos`.
    pub fn prev_char_boundary(&self) -> usize {
        let mut i = self.cursor_pos;
        if i == 0 {
            return 0;
        }
        i -= 1;
        while i > 0 && !self.input_buffer.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// Byte offset of the next UTF-8 char boundary after `cursor_pos`.
    pub fn next_char_boundary(&self) -> usize {
        let mut i = self.cursor_pos + 1;
        while i < self.input_buffer.len() && !self.input_buffer.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    /// Height of the composer pane (borders + body). Parity with old `input_height`.
    ///
    /// Soft-wrap redesign lands in PR-2; PR-1 keeps newline-based growth.
    pub fn height(&self, is_streaming: bool) -> u16 {
        if is_streaming {
            return 3;
        }
        let newlines = self.input_buffer.chars().filter(|&c| c == '\n').count() as u16;
        3 + newlines.min(7)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_streaming_is_three() {
        let mut c = ComposerState::new();
        c.input_buffer = "a\nb\nc\n".into();
        assert_eq!(c.height(true), 3);
    }

    #[test]
    fn height_grows_with_newlines() {
        let mut c = ComposerState::new();
        assert_eq!(c.height(false), 3);
        c.input_buffer = "a\nb".into();
        assert_eq!(c.height(false), 4);
    }

    #[test]
    fn set_input_buffer_syncs_cursor() {
        let mut c = ComposerState::new();
        c.set_input_buffer("hi".into());
        assert_eq!(c.cursor_pos, 2);
    }

    #[test]
    fn clear_input_resets_slash() {
        let mut c = ComposerState::new();
        c.input_buffer = "/help".into();
        c.show_slash_palette = true;
        c.slash_menu_index = 3;
        c.clear_input();
        assert!(c.input_buffer.is_empty());
        assert!(!c.show_slash_palette);
        assert_eq!(c.slash_menu_index, 0);
    }
}
