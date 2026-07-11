//! Output style rendering for `zz exec` headless mode .
//!
//! `run_turn` emits a stream of `zerozero_exec::Event` values via its
//! `emit` closure. The binary collects those events into a `Vec` and then
//! renders them to a `String` according to the requested `--output` style.
//!
//! This module holds the *pure* rendering logic so it can be unit-tested
//! without a network/LLM test the real code path, no network).
//!
//! ## Styles
//!
//! * `jsonl` (default) — one compact JSON object per line (the historical
//!   `zz exec` behavior).
//! * `json-pretty` — one pretty-printed JSON object per event, separated by
//!   blank lines.
//! * `snippet` — the final human-readable answer only: concatenated text of
//!   the `item.completed`/`item.updated` `agent_message` events, with NO
//!   JSON framing and a single trailing blank line.
//! * `parser` — a stable, machine-parseable `EVENT <type> <fields>` line per
//!   event, documented below.
//!
//! ## `parser` line format
//!
//! Each event becomes exactly one line of the form `EVENT <type> <fields>`
//! where `<fields>` is a compact, stable, space-separated set of values:
//!
//! ```text
//! EVENT session.started <session_id>
//! EVENT prompt <len>
//! EVENT item.started <id> <kind>
//! EVENT item.updated <id> <kind> <len>
//! EVENT item.completed <id> <kind> <len>
//! EVENT turn.completed
//! EVENT tool.started <tool_call_id> <tool_name>
//! EVENT tool.completed <tool_call_id> <tool_name> <len>
//! EVENT approval.requested <tool_call_id> <tool_name> <danger_level>
//! EVENT approval.result <tool_call_id> <approved>
//! EVENT session.resumed <session_id> <message_count>
//! EVENT compaction.started <before_messages> <before_tokens>
//! EVENT compaction.completed <after_messages> <after_tokens>
//! EVENT error <len>
//! ```
//!
//! `<len>` is the byte length of the relevant text field (utf-8). All values
//! are shell-safe (no spaces inside a field; `<len>` and counts are base-10
//! integers). Tool names and session ids are taken verbatim from the event.

use zerozero_exec::Event;

/// Render a collected event stream into a single `String` for the given
/// output `style` (e.g. `"jsonl"`, `"json-pretty"`, `"snippet"`,
/// `"parser"`). Unknown styles fall back to `jsonl`.
///
/// The returned string ends with a trailing newline (and, for `snippet`, an
/// additional blank line).
pub fn render_events(events: &[Event], style: &str) -> String {
    match style {
        "json-pretty" => render_json_pretty(events),
        "snippet" => render_snippet(events),
        "parser" => render_parser(events),
        // Default / "jsonl" / anything else.
        _ => render_jsonl(events),
    }
}

/// Default style name. Exposed so callers can compute the effective style
/// (e.g. when `--json-pretty` is used as an alias).
pub const DEFAULT_STYLE: &str = "jsonl";

/// One compact JSON object per line.
fn render_jsonl(events: &[Event]) -> String {
    let mut out = String::new();
    for ev in events {
        // serde_json::to_string cannot fail for these serializable enums.
        if let Ok(line) = serde_json::to_string(ev) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// One pretty-printed JSON object per event, separated by blank lines.
fn render_json_pretty(events: &[Event]) -> String {
    let mut out = String::new();
    for ev in events {
        if let Ok(pretty) = serde_json::to_string_pretty(ev) {
            out.push_str(&pretty);
            out.push_str("\n\n");
        }
    }
    out
}

/// Concatenated final answer text only — no JSON framing. One blank line
/// after (the trailing `"\n\n"`).
fn render_snippet(events: &[Event]) -> String {
    let mut text = String::new();
    for ev in events {
        match ev {
            Event::ItemCompleted { item } => {
                if matches!(item.kind, zerozero_exec::ItemKind::AgentMessage) {
                    text.push_str(&item.text);
                }
            }
            Event::ItemUpdated { item, .. }
                if item.kind == zerozero_exec::ItemUpdateKind::Message =>
            {
                text.push_str(&item.text);
            }
            _ => {}
        }
    }
    // Strip a single trailing newline from the agent text, then add exactly
    // one blank line (per spec: "One blank line after.").
    let trimmed = text.trim_end_matches('\n');
    let mut out = String::new();
    out.push_str(trimmed);
    out.push_str("\n\n");
    out
}

/// Stable machine-parseable `EVENT <type> <fields>` lines.
fn render_parser(events: &[Event]) -> String {
    let mut out = String::new();
    for ev in events {
        let line = match ev {
            Event::SessionStarted { session_id } => {
                format!("EVENT session.started {session_id}")
            }
            Event::Prompt { text } => {
                format!("EVENT prompt {}", text.len())
            }
            Event::ItemStarted { item } => {
                format!("EVENT item.started {} {}", item.id, kind_str(&item.kind))
            }
            Event::ItemUpdated { item, .. } => {
                format!(
                    "EVENT item.updated {} {} {}",
                    item.id,
                    update_kind_str(&item.kind),
                    item.text.len()
                )
            }
            Event::ItemCompleted { item } => {
                format!(
                    "EVENT item.completed {} {} {}",
                    item.id,
                    kind_str(&item.kind),
                    item.text.len()
                )
            }
            Event::TurnCompleted => "EVENT turn.completed".to_string(),
            Event::ToolStarted {
                tool_call_id,
                tool_name,
                ..
            } => {
                format!("EVENT tool.started {tool_call_id} {tool_name}")
            }
            Event::ToolCompleted {
                tool_call_id,
                tool_name,
                result,
            } => {
                format!(
                    "EVENT tool.completed {tool_call_id} {tool_name} {}",
                    result.len()
                )
            }
            Event::ApprovalRequested {
                tool_call_id,
                tool_name,
                danger_level,
                ..
            } => {
                format!("EVENT approval.requested {tool_call_id} {tool_name} {danger_level}")
            }
            Event::ApprovalResult {
                tool_call_id,
                approved,
            } => {
                format!("EVENT approval.result {tool_call_id} {approved}")
            }
            Event::SessionResumed {
                session_id,
                message_count,
            } => {
                format!("EVENT session.resumed {session_id} {message_count}")
            }
            Event::CompactionStarted {
                before_messages,
                before_tokens,
            } => {
                format!("EVENT compaction.started {before_messages} {before_tokens}")
            }
            Event::CompactionCompleted {
                after_messages,
                after_tokens,
            } => {
                format!("EVENT compaction.completed {after_messages} {after_tokens}")
            }
            Event::Error { message } => {
                format!("EVENT error {}", message.len())
            }
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

const fn kind_str(kind: &zerozero_exec::ItemKind) -> &'static str {
    match kind {
        zerozero_exec::ItemKind::AgentMessage => "agent_message",
    }
}

const fn update_kind_str(kind: &zerozero_exec::ItemUpdateKind) -> &'static str {
    match kind {
        zerozero_exec::ItemUpdateKind::Message => "message",
        zerozero_exec::ItemUpdateKind::Reasoning => "reasoning",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerozero_exec::{Event, Item, ItemKind, ItemStarted, ItemUpdateKind, ItemUpdated};

    fn sample_events() -> Vec<Event> {
        vec![
            Event::SessionStarted {
                session_id: "s-1".to_string(),
            },
            Event::Prompt {
                text: "hi".to_string(),
            },
            Event::ItemStarted {
                item: ItemStarted {
                    id: "item_0".to_string(),
                    kind: ItemKind::AgentMessage,
                },
            },
            Event::ItemUpdated {
                item: ItemUpdated {
                    id: "item_0".to_string(),
                    text: "Hello".to_string(),
                    kind: ItemUpdateKind::Message,
                },
            },
            Event::ItemCompleted {
                item: Item {
                    id: "item_0".to_string(),
                    kind: ItemKind::AgentMessage,
                    text: "Hello world".to_string(),
                },
            },
            Event::ToolStarted {
                tool_call_id: "call_1".to_string(),
                tool_name: "bash".to_string(),
                args: serde_json::json!({"command": "ls"}),
            },
            Event::ToolCompleted {
                tool_call_id: "call_1".to_string(),
                tool_name: "bash".to_string(),
                result: "ok".to_string(),
            },
            Event::TurnCompleted,
        ]
    }

    #[test]
    fn test_jsonl_default_one_object_per_line() {
        let s = render_events(&sample_events(), "jsonl");
        let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 8, "one compact JSON object per line");
        assert!(lines[0].starts_with("{\"type\":\"session.started\""));
        assert!(
            !lines[0].contains('\n'),
            "compact form has no inner newline"
        );
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn test_jsonl_is_default_for_unknown_style() {
        // Unknown style falls back to jsonl.
        let s = render_events(&sample_events(), "bogus");
        assert!(
            s.lines()
                .next()
                .unwrap()
                .starts_with("{\"type\":\"session.started\"")
        );
    }

    #[test]
    fn test_json_pretty_multiline() {
        let s = render_events(&sample_events(), "json-pretty");
        // Pretty print indents with 2 spaces.
        assert!(s.contains("  \"type\":"), "pretty JSON has indented fields");
        assert!(s.contains("\"type\": \"session.started\""));
        // Two events separated by blank lines -> at least one empty line.
        assert!(s.contains("\n\n"));
    }

    #[test]
    fn test_snippet_no_json_framing() {
        let s = render_events(&sample_events(), "snippet");
        assert!(
            !s.contains("session.started"),
            "snippet has no JSON framing"
        );
        assert!(!s.contains("item.completed"), "snippet has no JSON framing");
        assert!(
            s.contains("Hello world"),
            "snippet contains final answer text"
        );
        // Exactly one blank line after (trailing "\n\n").
        assert!(s.ends_with("\n\n"), "snippet ends with one blank line");
        // No event-type markers.
        assert!(!s.contains("EVENT "));
    }

    #[test]
    fn test_snippet_strips_inner_newlines_then_blank_line() {
        let events = vec![Event::ItemCompleted {
            item: Item {
                id: "item_0".to_string(),
                kind: ItemKind::AgentMessage,
                text: "line1\nline2\n".to_string(),
            },
        }];
        let s = render_events(&events, "snippet");
        assert_eq!(s, "line1\nline2\n\n");
    }

    #[test]
    fn test_parser_event_lines() {
        let s = render_events(&sample_events(), "parser");
        let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines[0], "EVENT session.started s-1");
        assert_eq!(lines[1], "EVENT prompt 2");
        assert_eq!(lines[2], "EVENT item.started item_0 agent_message");
        assert_eq!(lines[3], "EVENT item.updated item_0 message 5");
        assert_eq!(lines[4], "EVENT item.completed item_0 agent_message 11");
        assert_eq!(lines[5], "EVENT tool.started call_1 bash");
        assert_eq!(lines[6], "EVENT tool.completed call_1 bash 2");
        assert_eq!(lines[7], "EVENT turn.completed");
        // No JSON framing in parser output.
        assert!(!s.contains("session.started\":"), "parser has no JSON");
    }

    #[test]
    fn test_parser_error_len() {
        let events = vec![Event::Error {
            message: "boom".to_string(),
        }];
        let s = render_events(&events, "parser");
        assert_eq!(s, "EVENT error 4\n");
    }

    #[test]
    fn test_parser_approval_and_compaction() {
        let events = vec![
            Event::ApprovalRequested {
                tool_call_id: "c1".to_string(),
                tool_name: "bash".to_string(),
                args: serde_json::Value::Null,
                danger_level: "warning".to_string(),
            },
            Event::ApprovalResult {
                tool_call_id: "c1".to_string(),
                approved: true,
            },
            Event::CompactionStarted {
                before_messages: 20,
                before_tokens: 5000,
            },
            Event::CompactionCompleted {
                after_messages: 8,
                after_tokens: 2000,
            },
        ];
        let s = render_events(&events, "parser");
        let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines[0], "EVENT approval.requested c1 bash warning");
        assert_eq!(lines[1], "EVENT approval.result c1 true");
        assert_eq!(lines[2], "EVENT compaction.started 20 5000");
        assert_eq!(lines[3], "EVENT compaction.completed 8 2000");
    }
}
