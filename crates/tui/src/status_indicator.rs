//! Status indicator widget — Codex CLI parity .
//!
//! Shows a live status line above the composer when streaming:
//! ` ⏺ Working · 47s · (esc to interrupt)`
//!
//! The elapsed timer formats compactly:
//! - Under 1 minute: "47s"
//! - Under 1 hour: "4m 07s"
//! - 1 hour+: "1h 02m 09s"

use std::time::{Duration, Instant};

/// Status indicator state for the streaming "Working" line.
#[derive(Debug, Clone, Default)]
pub struct StatusIndicator {
    /// When the current streaming turn started (`None` when idle).
    pub start_time: Option<Instant>,
    /// Whether the agent is currently streaming.
    pub is_streaming: bool,
}

impl StatusIndicator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start tracking a streaming turn.
    pub fn start(&mut self) {
        self.start_time = Some(Instant::now());
        self.is_streaming = true;
    }

    /// Stop tracking — called when streaming ends.
    pub fn stop(&mut self) {
        self.is_streaming = false;
        self.start_time = None;
    }

    /// Elapsed duration since `start_time`, or zero if not streaming.
    pub fn elapsed(&self) -> Duration {
        self.start_time.map(|t| t.elapsed()).unwrap_or_default()
    }

    /// Format elapsed seconds into a compact human-friendly string.
    ///
    /// - Under 1 minute: `"47s"`
    /// - Under 1 hour: `"4m 07s"`
    /// - 1 hour+: `"1h 02m 09s"`
    pub fn format_elapsed(&self) -> String {
        format_duration(self.elapsed())
    }

    /// Render the status line text (without styling).
    pub fn status_text(&self, spinner: char) -> String {
        if !self.is_streaming {
            return String::new();
        }
        format!(
            " {} Working · {} · (esc to interrupt)",
            spinner,
            self.format_elapsed()
        )
    }
}

/// Format a duration into compact elapsed string (Codex parity).
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m {s:02}s")
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        format!("{h}h {m:02}m {s:02}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_seconds() {
        assert_eq!(format_duration(Duration::from_secs(47)), "47s");
    }

    #[test]
    fn test_format_minutes() {
        assert_eq!(format_duration(Duration::from_secs(247)), "4m 07s");
    }

    #[test]
    fn test_format_hours() {
        assert_eq!(format_duration(Duration::from_secs(3729)), "1h 02m 09s");
    }

    #[test]
    fn test_format_zero() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
    }

    #[test]
    fn test_indicator_start_stop() {
        let mut si = StatusIndicator::new();
        assert!(!si.is_streaming);
        assert!(si.start_time.is_none());

        si.start();
        assert!(si.is_streaming);
        assert!(si.start_time.is_some());

        si.stop();
        assert!(!si.is_streaming);
        assert!(si.start_time.is_none());
    }

    #[test]
    fn test_status_text_idle() {
        let si = StatusIndicator::new();
        assert_eq!(si.status_text('⏺'), "");
    }

    #[test]
    fn test_status_text_streaming() {
        let mut si = StatusIndicator::new();
        si.start();
        let text = si.status_text('⏺');
        assert!(text.contains("Working"));
        assert!(text.contains("esc to interrupt"));
        assert!(text.contains("⏺"));
    }
}
