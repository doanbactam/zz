//! Strip ANSI escape sequences from a string).
//!
//! Used by TUI E2E tests to compare rendered output without escape codes.
//! Handles CSI sequences (colors, cursor movement), OSC sequences, and
//! simple escape sequences.

/// Remove ANSI escape sequences from a string.
///
/// Handles:
/// - CSI: `ESC [ ... <final-byte>` (colors, cursor, etc.)
/// - OSC: `ESC ] ... BEL` or `ESC ] ... ESC \`
/// - Simple: `ESC ( B`, `ESC =`, `ESC >`, etc.
pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC — start of escape sequence
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                b'[' => {
                    // CSI sequence: ESC [ ... 0x40-0x7E
                    i += 1;
                    while i < bytes.len() && !(bytes[i] >= 0x40 && bytes[i] <= 0x7e) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1; // consume final byte
                    }
                }
                b']' => {
                    // OSC sequence: ESC ] ... BEL (0x07) or ESC \
                    i += 1;
                    while i < bytes.len() && bytes[i] != 0x07 {
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    if i < bytes.len() && bytes[i] == 0x07 {
                        i += 1; // consume BEL
                    }
                }
                _ => {
                    // Simple escape: ESC <char> — consume one more byte.
                    // Some sequences like ESC ( B have 2 bytes after ESC.
                    if bytes[i] == b'(' || bytes[i] == b')' {
                        i += 2; // charset designation: ESC ( B
                    } else {
                        i += 1;
                    }
                }
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_plain_text() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn test_strip_ansi_color_codes() {
        let input = "\x1b[31mred text\x1b[0m";
        assert_eq!(strip_ansi(input), "red text");
    }

    #[test]
    fn test_strip_ansi_cursor_movement() {
        let input = "\x1b[1;1Hhello\x1b[2;3Hworld";
        assert_eq!(strip_ansi(input), "helloworld");
    }

    #[test]
    fn test_strip_ansi_multiple_codes() {
        let input = "\x1b[1;1Hhello \x1b[1;7Hworld";
        assert_eq!(strip_ansi(input), "hello world");
    }

    #[test]
    fn test_strip_ansi_osc_sequence() {
        let input = "\x1b]0;title\x07hello";
        assert_eq!(strip_ansi(input), "hello");
    }

    #[test]
    fn test_strip_ansi_osc_with_esc_backslash() {
        let input = "\x1b]0;title\x1b\\hello";
        assert_eq!(strip_ansi(input), "hello");
    }

    #[test]
    fn test_strip_ansi_simple_escape() {
        // ESC ( B — charset designation
        let input = "\x1b(Bhello";
        assert_eq!(strip_ansi(input), "hello");
    }

    #[test]
    fn test_strip_ansi_empty() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_strip_ansi_only_escape() {
        assert_eq!(strip_ansi("\x1b"), "");
    }

    #[test]
    fn test_strip_ansi_truncated_csi() {
        assert_eq!(strip_ansi("\x1b[31"), "");
    }

    #[test]
    fn test_strip_ansi_mixed() {
        let input = "\x1b[32mgreen\x1b[0m \x1b[1mbold\x1b[22m";
        assert_eq!(strip_ansi(input), "green bold");
    }
}
