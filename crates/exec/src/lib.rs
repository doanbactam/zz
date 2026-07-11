//! Headless engine for `zz exec`: emits JSON line events on stdout.
//!
//! scope : a single turn that echoes the prompt as a
//! placeholder agent message. Real LLM calls arrive in.
//!
//! Schema (one JSON object per line):
//! ```jsonc
//! {"type":"session.started","session_id":"2026-07-04T18:30:26Z"}
//! {"type":"prompt","text":"<user prompt>"}
//! {"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"echo: <prompt>"}}
//! {"type":"turn.completed"}
//! ```

use serde::{Deserialize, Serialize};

/// Top-level JSON line event emitted on stdout.
#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "session.started")]
    SessionStarted { session_id: String },
    #[serde(rename = "prompt")]
    Prompt { text: String },
    #[serde(rename = "item.started")]
    ItemStarted { item: ItemStarted },
    #[serde(rename = "item.updated")]
    ItemUpdated { item: ItemUpdated },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: Item },
    #[serde(rename = "turn.completed")]
    TurnCompleted,
    #[serde(rename = "tool.started")]
    ToolStarted {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    #[serde(rename = "tool.completed")]
    ToolCompleted {
        tool_call_id: String,
        tool_name: String,
        result: String,
    },
    #[serde(rename = "approval.requested")]
    ApprovalRequested {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
        danger_level: String,
    },
    #[serde(rename = "approval.result")]
    ApprovalResult {
        tool_call_id: String,
        approved: bool,
    },
    #[serde(rename = "session.resumed")]
    SessionResumed {
        session_id: String,
        message_count: usize,
    },
    #[serde(rename = "compaction.started")]
    CompactionStarted {
        before_messages: usize,
        before_tokens: usize,
    },
    #[serde(rename = "compaction.completed")]
    CompactionCompleted {
        after_messages: usize,
        after_tokens: usize,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

/// Item metadata for `item.started` events — id and type only, no text yet.
#[derive(Serialize, Deserialize, Clone)]
pub struct ItemStarted {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ItemKind,
}

/// Which part of the assistant item a delta belongs to.
#[derive(Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemUpdateKind {
    #[default]
    Message,
    Reasoning,
}

/// Item delta for `item.updated` events — id and text delta.
#[derive(Serialize, Deserialize, Clone)]
pub struct ItemUpdated {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub kind: ItemUpdateKind,
}

/// A single agent item, nested inside `Event::ItemCompleted`.
#[derive(Serialize, Deserialize, Clone)]
pub struct Item {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ItemKind,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    AgentMessage,
}

/// Run one headless turn.
///
/// Emits `session.started` -> `prompt` -> `item.completed` (placeholder
/// `agent_message` echoing the prompt) -> `turn.completed` on stdout, one
/// JSON object per line. Returns `Ok(())` on success.
///
/// The placeholder agent message is explicit: replaces it with a
/// real LLM call (see §7 assumption #1).
pub fn run(prompt: String) -> anyhow::Result<()> {
    let session_id = session_id_now();
    emit(Event::SessionStarted { session_id })?;
    emit(Event::Prompt {
        text: prompt.clone(),
    })?;
    emit(Event::ItemCompleted {
        item: Item {
            id: "item_0".to_string(),
            kind: ItemKind::AgentMessage,
            text: format!("echo: {prompt}"),
        },
    })?;
    emit(Event::TurnCompleted)?;
    Ok(())
}

fn emit(event: Event) -> anyhow::Result<()> {
    let line = serde_json::to_string(&event)?;
    println!("{line}");
    Ok(())
}

fn session_id_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso8601_utc(secs)
}

/// Convert Unix seconds to an ISO 8601 UTC timestamp like
/// `2026-07-04T18:30:26Z`. Hand-rolled to avoid a `chrono` dependency.
/// Valid for the proleptic Gregorian calendar.
fn iso8601_utc(secs: u64) -> String {
    let days = secs / 86400;
    let sod = secs % 86400;
    let h = sod / 3600;
    let m = (sod % 3600) / 60;
    let s = sod % 60;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's `civil_from_days` algorithm. Converts days since the
/// Unix epoch (1970-01-01) to a `(year, month, day)` triple. Reference:
/// <https://howardhinnant.github.io/date_algorithms.html>.
const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_serialization_session_started() {
        let e = Event::SessionStarted {
            session_id: "2026-07-04T18:30:26Z".to_string(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            json,
            r#"{"type":"session.started","session_id":"2026-07-04T18:30:26Z"}"#
        );
    }

    #[test]
    fn test_event_serialization_turn_completed() {
        let e = Event::TurnCompleted;
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"turn.completed"}"#);
    }

    #[test]
    fn test_event_serialization_prompt() {
        let e = Event::Prompt {
            text: "hello".to_string(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"prompt","text":"hello"}"#);
    }

    #[test]
    fn test_event_serialization_item_completed() {
        let e = Event::ItemCompleted {
            item: Item {
                id: "item_0".to_string(),
                kind: ItemKind::AgentMessage,
                text: "echo: hi".to_string(),
            },
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            json,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"echo: hi"}}"#
        );
    }

    #[test]
    fn test_iso8601_known_epoch() {
        // 2026-07-04T18:30:26Z. Cross-checked with
        // `date -u -d @1783189826` (python: datetime(2026,7,4,18,30,26,utc).
        // timestamp()).
        assert_eq!(iso8601_utc(1783189826), "2026-07-04T18:30:26Z");
    }

    #[test]
    fn test_iso8601_epoch_start() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn test_iso8601_y2k() {
        // 2000-01-01T00:00:00Z = 946684800 seconds.
        assert_eq!(iso8601_utc(946684800), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn test_civil_from_days_unix_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn test_civil_from_days_known() {
        // 1783189826 / 86400 = 20638.something -> day 20638.
        // 2026-07-04 = 20638 days after 1970-01-01.
        assert_eq!(civil_from_days(20638), (2026, 7, 4));
    }

    #[test]
    fn test_event_serialization_approval_requested() {
        let e = Event::ApprovalRequested {
            tool_call_id: "call_1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({"command": "rm -rf /tmp/test"}),
            danger_level: "warning".to_string(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"approval.requested""#));
        assert!(json.contains(r#""tool_call_id":"call_1""#));
        assert!(json.contains(r#""danger_level":"warning""#));
    }

    #[test]
    fn test_event_serialization_approval_result() {
        let e = Event::ApprovalResult {
            tool_call_id: "call_1".to_string(),
            approved: false,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            json,
            r#"{"type":"approval.result","tool_call_id":"call_1","approved":false}"#
        );
    }

    #[test]
    fn test_event_serialization_session_resumed() {
        let e = Event::SessionResumed {
            session_id: "s123".to_string(),
            message_count: 5,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"session.resumed""#));
        assert!(json.contains(r#""session_id":"s123""#));
        assert!(json.contains(r#""message_count":5"#));
    }

    #[test]
    fn test_event_serialization_compaction_started() {
        let e = Event::CompactionStarted {
            before_messages: 25,
            before_tokens: 50000,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"compaction.started""#));
        assert!(json.contains(r#""before_messages":25"#));
        assert!(json.contains(r#""before_tokens":50000"#));
    }

    #[test]
    fn test_event_serialization_compaction_completed() {
        let e = Event::CompactionCompleted {
            after_messages: 8,
            after_tokens: 2000,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"compaction.completed""#));
        assert!(json.contains(r#""after_messages":8"#));
        assert!(json.contains(r#""after_tokens":2000"#));
    }
}
