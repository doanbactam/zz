//! Session persistence for ZeroZero .
//!
//! SQLite-backed session store using rusqlite (bundled).
//! Stores conversation history per session for resume + audit.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;
use zerozero_llm::ChatMessage;

/// A checkpoint = each user message in a session .
/// Per Claude Code semantics: "every user prompt creates a new checkpoint".
/// Used by `zz rewind` to list rewind points.
#[derive(Debug, Clone, Serialize)]
pub struct Checkpoint {
    pub seq: i64,
    pub role: String,
    /// First 80 characters of the message content (+ "..." if truncated).
    pub content_preview: String,
    pub created_at: String,
}

/// Session metadata for listing.
#[derive(Debug, Clone, Serialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_at: String,
    pub prompt: String,
    pub model: Option<String>,
    pub message_count: i64,
    /// Parent thread ID (None for root thread, Some for subagent).
    /// Added by  schema migration.
    pub parent_thread_id: Option<String>,
    /// Agent path in the thread tree (e.g. "root", "root.0").
    /// Added by  schema migration.
    pub agent_path: Option<String>,
    /// Depth in the agent tree (0 = root, 1 = first-level subagent).
    /// Added by  schema migration.
    pub depth: i64,
    /// Agent status: "running", "stopped", "completed", "failed".
    /// Added by  schema migration.
    pub agent_status: Option<String>,
    /// Nickname (e.g. "root", "agent-0").
    /// Added by  schema migration.
    pub nickname: Option<String>,
}

/// SQLite-backed session store.
pub struct SessionStore {
    conn: Connection,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    prompt TEXT NOT NULL,
    model TEXT,
    message_count INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    tool_call_id TEXT,
    tool_calls TEXT,
    created_at TEXT NOT NULL,
    seq INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
"#;

impl SessionStore {
    /// Open or create a SQLite database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(SCHEMA)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Create an in-memory database (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Run schema migration for  thread metadata columns.
    ///
    /// Adds 5 columns to the `sessions` table:
    /// - `parent_thread_id TEXT` — parent thread ID (NULL for root)
    /// - `agent_path TEXT` — tree position (e.g. "root", "root.0")
    /// - `depth INTEGER DEFAULT 0` — tree depth (0 = root)
    /// - `agent_status TEXT DEFAULT 'running'` — thread status
    /// - `nickname TEXT` — display name (e.g. "root", "agent-0")
    ///
    /// Also creates index `idx_sessions_parent` for parent lookup.
    ///
    /// **Idempotent:** Checks `PRAGMA table_info(sessions)` before each
    /// `ALTER TABLE`. If a column already exists, the ALTER is skipped.
    /// Running migration multiple times is safe (no panic, no error).
    fn migrate(&self) -> Result<()> {
        // Get existing column names from PRAGMA table_info.
        let existing_columns: std::collections::HashSet<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(sessions)")?;
            let rows = stmt.query_map([], |row| {
                let name: String = row.get(1)?;
                Ok(name)
            })?;
            let mut cols = std::collections::HashSet::new();
            for row in rows {
                cols.insert(row?);
            }
            cols
        };

        // Add columns if they don't exist (idempotent).
        let migrations: &[(&str, &str)] = &[
            (
                "parent_thread_id",
                "ALTER TABLE sessions ADD COLUMN parent_thread_id TEXT",
            ),
            (
                "agent_path",
                "ALTER TABLE sessions ADD COLUMN agent_path TEXT",
            ),
            (
                "depth",
                "ALTER TABLE sessions ADD COLUMN depth INTEGER DEFAULT 0",
            ),
            (
                "agent_status",
                "ALTER TABLE sessions ADD COLUMN agent_status TEXT DEFAULT 'running'",
            ),
            ("nickname", "ALTER TABLE sessions ADD COLUMN nickname TEXT"),
            ("plan", "ALTER TABLE sessions ADD COLUMN plan TEXT"),
            (
                "approvals",
                "ALTER TABLE sessions ADD COLUMN approvals TEXT",
            ),
        ];

        for (col_name, sql) in migrations {
            if !existing_columns.contains(*col_name) {
                self.conn.execute_batch(sql)?;
            }
        }

        // Create index for parent lookup (tree traversal).
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_thread_id)",
        )?;

        Ok(())
    }

    /// Create a new session record.
    pub fn create_session(&self, id: &str, prompt: &str, model: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, created_at, prompt, model, message_count) VALUES (?, ?, ?, ?, 0)",
            rusqlite::params![id, iso8601_now(), prompt, model],
        )?;
        Ok(())
    }

    /// Append a message to a session. Increments message_count.
    pub fn append_message(&self, session_id: &str, msg: &ChatMessage) -> Result<()> {
        let tool_calls_json = msg
            .tool_calls
            .as_ref()
            .map(|tc| serde_json::to_string(tc).unwrap_or_default());

        let next_seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(seq), -1) + 1 FROM messages WHERE session_id = ?",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        self.conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, created_at, seq) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                session_id,
                msg.role,
                msg.content,
                msg.tool_call_id,
                tool_calls_json,
                iso8601_now(),
                next_seq,
            ],
        )?;

        self.conn.execute(
            "UPDATE sessions SET message_count = message_count + 1 WHERE id = ?",
            rusqlite::params![session_id],
        )?;

        Ok(())
    }

    /// Get all messages for a session, ordered by seq.
    pub fn get_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, tool_call_id, tool_calls FROM messages \
             WHERE session_id = ? ORDER BY seq ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            let role: String = row.get(0)?;
            let content: String = row.get(1)?;
            let tool_call_id: Option<String> = row.get(2)?;
            let tool_calls_json: Option<String> = row.get(3)?;

            let tool_calls = tool_calls_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());

            Ok(ChatMessage {
                role,
                content,
                tool_call_id,
                tool_calls,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            })
        })?;

        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    }

    /// List all sessions, ordered by created_at DESC.
    pub fn list_sessions(&self) -> Result<Vec<SessionMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, prompt, model, message_count, \
             parent_thread_id, agent_path, depth, agent_status, nickname \
             FROM sessions ORDER BY created_at DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(SessionMeta {
                id: row.get(0)?,
                created_at: row.get(1)?,
                prompt: row.get(2)?,
                model: row.get(3)?,
                message_count: row.get(4)?,
                parent_thread_id: row.get(5)?,
                agent_path: row.get(6)?,
                depth: row.get(7).unwrap_or(0),
                agent_status: row.get(8)?,
                nickname: row.get(9)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Create a new session record with thread metadata .
    ///
    /// This is used by `ThreadRegistry::spawn_agent` to create a session
    /// for a subagent thread with parent/depth/path/nickname metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_thread(
        &self,
        id: &str,
        prompt: &str,
        model: Option<&str>,
        parent_thread_id: Option<&str>,
        agent_path: &str,
        depth: i32,
        nickname: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions \
             (id, created_at, prompt, model, message_count, \
              parent_thread_id, agent_path, depth, agent_status, nickname) \
             VALUES (?, ?, ?, ?, 0, ?, ?, ?, 'running', ?)",
            rusqlite::params![
                id,
                iso8601_now(),
                prompt,
                model,
                parent_thread_id,
                agent_path,
                depth,
                nickname
            ],
        )?;
        Ok(())
    }

    /// Update the agent status for a session .
    ///
    /// Called when a thread completes, fails, or is stopped.
    /// Status values: "running", "stopped", "completed", "failed".
    pub fn update_agent_status(&self, id: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET agent_status = ? WHERE id = ?",
            rusqlite::params![status, id],
        )?;
        Ok(())
    }

    /// List child sessions of a parent thread .
    ///
    /// Returns all sessions where `parent_thread_id` matches the given ID.
    /// Used for tree view traversal.
    pub fn list_child_sessions(&self, parent_thread_id: &str) -> Result<Vec<SessionMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, prompt, model, message_count, \
             parent_thread_id, agent_path, depth, agent_status, nickname \
             FROM sessions WHERE parent_thread_id = ? ORDER BY created_at ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![parent_thread_id], |row| {
            Ok(SessionMeta {
                id: row.get(0)?,
                created_at: row.get(1)?,
                prompt: row.get(2)?,
                model: row.get(3)?,
                message_count: row.get(4)?,
                parent_thread_id: row.get(5)?,
                agent_path: row.get(6)?,
                depth: row.get(7).unwrap_or(0),
                agent_status: row.get(8)?,
                nickname: row.get(9)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Delete a session and all its messages (cascade).
    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM messages WHERE session_id = ?",
            rusqlite::params![id],
        )?;
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?", rusqlite::params![id])?;
        Ok(())
    }

    /// Compare two sessions and return a human-readable diff summary.
    pub fn compare_sessions(&self, id_a: &str, id_b: &str) -> Result<String> {
        let msgs_a = self.get_messages(id_a)?;
        let msgs_b = self.get_messages(id_b)?;

        let mut result = format!(
            "Session A: {} ({} messages)\nSession B: {} ({} messages)\n\n",
            &id_a[..8.min(id_a.len())],
            msgs_a.len(),
            &id_b[..8.min(id_b.len())],
            msgs_b.len(),
        );

        let max = msgs_a.len().max(msgs_b.len());
        let mut diffs = 0;
        for i in 0..max {
            let a = msgs_a.get(i);
            let b = msgs_b.get(i);
            match (a, b) {
                (Some(a), Some(b)) if a.content != b.content => {
                    diffs += 1;
                    result.push_str(&format!(
                        "[{}] msg[{}]: {} != {}\n",
                        i,
                        i,
                        truncate(&a.content, 60),
                        truncate(&b.content, 60),
                    ));
                }
                (Some(a), None) => {
                    diffs += 1;
                    result.push_str(&format!(
                        "[A only] msg[{}]: {}\n",
                        i,
                        truncate(&a.content, 60),
                    ));
                }
                (None, Some(b)) => {
                    diffs += 1;
                    result.push_str(&format!(
                        "[B only] msg[{}]: {}\n",
                        i,
                        truncate(&b.content, 60),
                    ));
                }
                _ => {}
            }
        }

        if diffs == 0 {
            result.push_str("Sessions are identical.\n");
        } else {
            result.push_str(&format!("\n{diffs} difference(s) found.\n"));
        }

        Ok(result)
    }

    /// Delete all messages with `seq > seq` in the given session .
    /// Updates `message_count` to reflect remaining messages.
    /// No-op if `seq` >= current max seq (nothing to delete).
    /// Errors if the session does not exist.
    pub fn truncate_after(&self, session_id: &str, seq: i64) -> Result<()> {
        // Verify session exists. Distinguish "not found" from DB errors
        // (don't mask DB errors as "not found").
        let exists: bool = match self.conn.query_row(
            "SELECT 1 FROM sessions WHERE id = ?",
            rusqlite::params![session_id],
            |_| Ok(true),
        ) {
            Ok(true) => true,
            Ok(_) => false,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(e) => return Err(e.into()),
        };
        if !exists {
            anyhow::bail!("session not found: {session_id}");
        }

        self.conn.execute(
            "DELETE FROM messages WHERE session_id = ? AND seq > ?",
            rusqlite::params![session_id, seq],
        )?;

        self.conn.execute(
            "UPDATE sessions SET message_count = \
             (SELECT COUNT(*) FROM messages WHERE session_id = ?) WHERE id = ?",
            rusqlite::params![session_id, session_id],
        )?;

        Ok(())
    }

    /// List checkpoints for a session .
    /// A checkpoint = each message with `role = "user"` (per Claude Code
    /// semantics: "every user prompt creates a new checkpoint").
    /// Returns `seq`, `role`, `content_preview` (≤80 chars + ellipsis),
    /// `created_at`, ordered by `seq` ASC. Errors if session not found.
    pub fn checkpoint_list(&self, session_id: &str) -> Result<Vec<Checkpoint>> {
        // Verify session exists. Distinguish "not found" from DB errors
        // (don't mask DB errors as "not found").
        let exists: bool = match self.conn.query_row(
            "SELECT 1 FROM sessions WHERE id = ?",
            rusqlite::params![session_id],
            |_| Ok(true),
        ) {
            Ok(true) => true,
            Ok(_) => false,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(e) => return Err(e.into()),
        };
        if !exists {
            anyhow::bail!("session not found: {session_id}");
        }

        let mut stmt = self.conn.prepare(
            "SELECT seq, role, content, created_at FROM messages \
             WHERE session_id = ? AND role = 'user' ORDER BY seq ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            let seq: i64 = row.get(0)?;
            let role: String = row.get(1)?;
            let content: String = row.get(2)?;
            let created_at: String = row.get(3)?;
            Ok(Checkpoint {
                seq,
                role,
                content_preview: truncate(&content, 80),
                created_at,
            })
        })?;

        let mut checkpoints = Vec::new();
        for row in rows {
            checkpoints.push(row?);
        }
        Ok(checkpoints)
    }

    /// Persist the agent's plan alongside the transcript F14).
    pub fn update_plan(&self, id: &str, plan: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET plan = ? WHERE id = ?",
            rusqlite::params![plan, id],
        )?;
        Ok(())
    }

    /// Get the persisted plan, if any.
    pub fn get_plan(&self, id: &str) -> Result<Option<String>> {
        let plan: Option<String> = self.conn.query_row(
            "SELECT plan FROM sessions WHERE id = ?",
            rusqlite::params![id],
            |row| row.get(0),
        )?;
        Ok(plan.filter(|p| !p.is_empty()))
    }

    /// Persist approval decisions (JSON array of strings) alongside transcript.
    pub fn update_approvals(&self, id: &str, approvals: &[String]) -> Result<()> {
        let json = serde_json::to_string(approvals)?;
        self.conn.execute(
            "UPDATE sessions SET approvals = ? WHERE id = ?",
            rusqlite::params![json, id],
        )?;
        Ok(())
    }

    /// Get persisted approvals, if any.
    pub fn get_approvals(&self, id: &str) -> Result<Vec<String>> {
        let raw: Option<String> = self.conn.query_row(
            "SELECT approvals FROM sessions WHERE id = ?",
            rusqlite::params![id],
            |row| row.get(0),
        )?;
        match raw.filter(|r| !r.is_empty()) {
            Some(json) => Ok(serde_json::from_str::<Vec<String>>(json.as_str())?),
            None => Ok(Vec::new()),
        }
    }

    /// Export a session (messages + metadata) as JSON for backup/transfer.
    pub fn export_session(&self, id: &str) -> Result<String> {
        let meta = self
            .list_sessions()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))?;
        let messages = self.get_messages(id)?;
        let payload = serde_json::json!({
            "id": meta.id,
            "created_at": meta.created_at,
            "prompt": meta.prompt,
            "model": meta.model,
            "parent_thread_id": meta.parent_thread_id,
            "agent_path": meta.agent_path,
            "depth": meta.depth,
            "nickname": meta.nickname,
            "plan": self.get_plan(id)?,
            "approvals": self.get_approvals(id)?,
            "messages": messages,
        });
        Ok(serde_json::to_string_pretty(&payload)?)
    }

    /// Import a previously exported session JSON (regenerates id if missing).
    pub fn import_session(&self, json: &str) -> Result<String> {
        let v: serde_json::Value = serde_json::from_str(json)?;
        let id = v["id"].as_str().unwrap_or("imported").to_string();
        let prompt = v["prompt"].as_str().unwrap_or("").to_string();
        let model = v["model"].as_str().map(|s| s.to_string());
        self.create_session(&id, &prompt, model.as_deref())?;
        if let Some(plan) = v["plan"].as_str().filter(|p| !p.is_empty()) {
            self.update_plan(&id, plan)?;
        }
        if let Some(approvals) = v["approvals"].as_array() {
            let apps: Vec<String> = approvals
                .iter()
                .filter_map(|a| a.as_str().map(|s| s.to_string()))
                .collect();
            if !apps.is_empty() {
                self.update_approvals(&id, &apps)?;
            }
        }
        if let Some(messages) = v["messages"].as_array() {
            for m in messages {
                if let Ok(msg) = serde_json::from_value::<ChatMessage>(m.clone()) {
                    self.append_message(&id, &msg)?;
                }
            }
        }
        Ok(id)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Find a safe UTF-8 char boundary at or before `max` bytes.
        // Slicing at a non-char boundary panics (UTF-8 safe).
        let boundary = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= max)
            .last()
            .unwrap_or(0);
        format!("{}...", &s[..boundary])
    }
}

/// Current time as ISO 8601 UTC string.
fn iso8601_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let sod = secs % 86400;
    let h = sod / 3600;
    let m = (sod % 3600) / 60;
    let s = sod % 60;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

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
    use zerozero_llm::{ToolCall, ToolCallFunction};

    #[test]
    fn test_open_in_memory() {
        let store = SessionStore::open_in_memory().expect("open in memory");
        let _ = store.conn; // just verify it exists
    }

    #[test]
    fn test_create_session() {
        let store = SessionStore::open_in_memory().unwrap();
        store
            .create_session("s1", "hello world", Some("gpt-4o-mini"))
            .unwrap();
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "s1");
        assert_eq!(sessions[0].prompt, "hello world");
        assert_eq!(sessions[0].model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(sessions[0].message_count, 0);
    }

    #[test]
    fn test_append_and_get_messages() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "hello", None).unwrap();

        let msg1 = ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        };
        store.append_message("s1", &msg1).unwrap();

        let msg2 = ChatMessage {
            role: "assistant".to_string(),
            content: "hi there".to_string(),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: ToolCallFunction {
                    name: "bash".to_string(),
                    arguments: r#"{"command":"echo hi"}"#.to_string(),
                },
            }]),
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        };
        store.append_message("s1", &msg2).unwrap();

        let messages = store.get_messages("s1").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "hi there");
        assert!(messages[1].tool_calls.is_some());
        let tc = messages[1].tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "bash");

        // Check message_count was incremented.
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions[0].message_count, 2);
    }

    #[test]
    fn test_get_messages_empty() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "hello", None).unwrap();
        let messages = store.get_messages("s1").unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_list_sessions_order() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "first", None).unwrap();
        store.create_session("s2", "second", None).unwrap();
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        // Both have same timestamp (second resolution), order may vary
        // but both should be present
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"s1"));
        assert!(ids.contains(&"s2"));
    }

    #[test]
    fn test_delete_session_cascade() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "hello", None).unwrap();
        let msg = ChatMessage {
            role: "user".to_string(),
            content: "test".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        };
        store.append_message("s1", &msg).unwrap();
        assert_eq!(store.get_messages("s1").unwrap().len(), 1);

        store.delete_session("s1").unwrap();
        assert!(store.get_messages("s1").unwrap().is_empty());
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn test_append_multiple_sessions() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "first", None).unwrap();
        store.create_session("s2", "second", None).unwrap();

        let msg = ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        };
        store.append_message("s1", &msg).unwrap();
        store.append_message("s2", &msg).unwrap();

        assert_eq!(store.get_messages("s1").unwrap().len(), 1);
        assert_eq!(store.get_messages("s2").unwrap().len(), 1);
    }

    // --- : Mutation coverage fix ---

    #[test]
    fn test_prd8_ac1_civil_from_days_epoch_zero() {
        // epoch seconds=0 → 1970-01-01 → days=0
        let (y, m, d) = civil_from_days(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn test_prd8_ac1_civil_from_days_known_values() {
        // Each days value verified via: epoch_seconds = days * 86400;
        // date -u -d @epoch_seconds +%Y-%m-%d
        // Verification date: 2026-07-05 (date -u).
        //
        // days=20638 → 2026-07-04 (date -u -d @1783267200)
        //   (1783189826 / 86400 = 20637.6...; floor 20637 → 2026-07-03
        //    but actual calculation gives 20638 for 2026-07-04 UTC.
        //    Let's verify: 20638 * 86400 = 1783123200 → date -u -d @1783123200
        //    → 2026-07-04 00:00 UTC. Wait, that doesn't match the
        //    research epoch. Let me re-derive: research said epoch
        //    1783189826 → 2026-07-04. 1783189826 / 86400 = 20638.30...
        //    floor = 20638. 20638 * 86400 = 1783123200. Hmm, mismatch.
        //    Actually: date -u -d @1783189826 +%Y-%m-%d should give the
        //    correct date. 1783189826 - 20638*86400 = 66626 seconds into
        //    the day = 18h30m26s. So day is correct: 2026-07-04.
        //    My arithmetic: 20638 * 86400 = 1,783,123,200.
        //    1,783,123,200 + 66626 = 1,783,189,826. ✓
        //
        // days=11574 → 2001-09-09 (1_000_000_000 / 86400 = 11574.07...,
        //   floor 11574; 11574*86400=999_993_600, diff 6400 sec=1h46m40s.
        //   date -u -d @1000000000 +%Y-%m-%d → 2001-09-09. ✓)
        //
        // days=19675 → 2023-11-14 (1_700_000_000 / 86400 = 19675.92...,
        //   floor 19675; date -u -d @1700000000 +%Y-%m-%d → 2023-11-14. ✓)
        //
        // days=23148 → 2033-05-18 (2_000_000_000 / 86400 = 23148.14...,
        //   floor 23148; date -u -d @2000000000 +%Y-%m-%d → 2033-05-18. ✓)
        let cases: &[(i64, i64, u32, u32)] = &[
            (20638, 2026, 7, 4),
            (11574, 2001, 9, 9),
            (19675, 2023, 11, 14),
            (23148, 2033, 5, 18),
        ];
        for &(days, y, m, d) in cases {
            let (cy, cm, cd) = civil_from_days(days);
            assert_eq!(
                (cy, cm, cd),
                (y, m, d),
                "days={} expected {}-{:02}-{:02} but got {}-{:02}-{:02}",
                days,
                y,
                m,
                d,
                cy,
                cm,
                cd
            );
        }
    }

    #[test]
    fn test_prd8_ac1_civil_from_days_year_boundaries() {
        // Days for first day of each year from 1970 to 1975 (verified externally).
        // 1970-01-01 = day 0.
        // 1971-01-01 = day 365.
        // 1972-01-01 = day 730.
        // 1973-01-01 = day 1096 (1972 was leap, so +366).
        // 1974-01-01 = day 1461.
        // 1975-01-01 = day 1826.
        let cases: &[(i64, i64, u32, u32)] = &[
            (365, 1971, 1, 1),
            (730, 1972, 1, 1),
            (1096, 1973, 1, 1),
            (1461, 1974, 1, 1),
            (1826, 1975, 1, 1),
        ];
        for &(days, y, m, d) in cases {
            assert_eq!(civil_from_days(days), (y, m, d), "days={}", days);
        }
    }

    #[test]
    fn test_prd8_ac1_civil_from_days_leap_year() {
        // 1972-02-29 = day 730 + 31 + 28 = 789.
        // Verify: 1972 is leap, Feb 29 exists.
        let (y, m, d) = civil_from_days(789);
        assert_eq!((y, m, d), (1972, 2, 29));
        // 1972-03-01 = day 790.
        let (y, m, d) = civil_from_days(790);
        assert_eq!((y, m, d), (1972, 3, 1));
    }

    #[test]
    fn test_prd8_ac1_civil_from_days_negative() {
        // Day before 1970-01-01: -1 should give 1969-12-31.
        let (y, m, d) = civil_from_days(-1);
        assert_eq!((y, m, d), (1969, 12, 31));
    }

    // ----  Conversation Rewind tests ----

    fn append_msg(store: &SessionStore, sid: &str, role: &str, content: &str) {
        store
            .append_message(
                sid,
                &ChatMessage {
                    role: role.to_string(),
                    content: content.to_string(),
                    tool_call_id: None,
                    tool_calls: None,
                    attachments: None,
                    thinking_signature: None,
                    redacted_thinking: None,
                    thinking: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn test_prd98_truncate_after_basic() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        // seq 0=user, 1=assistant, 2=user, 3=assistant, 4=user
        append_msg(&store, "s1", "user", "hello");
        append_msg(&store, "s1", "assistant", "hi");
        append_msg(&store, "s1", "user", "do thing");
        append_msg(&store, "s1", "assistant", "ok");
        append_msg(&store, "s1", "user", "more");

        store.truncate_after("s1", 2).unwrap();
        let msgs = store.get_messages("s1").unwrap();
        assert_eq!(msgs.len(), 3, "should keep seq 0,1,2");
        assert_eq!(msgs[2].content, "do thing");

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions[0].message_count, 3);
    }

    #[test]
    fn test_prd98_truncate_after_edge_max() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        append_msg(&store, "s1", "user", "a");
        append_msg(&store, "s1", "assistant", "b");
        // seq max = 1; truncate at 1 = no-op
        store.truncate_after("s1", 1).unwrap();
        let msgs = store.get_messages("s1").unwrap();
        assert_eq!(msgs.len(), 2, "truncate at max seq = no-op");
    }

    #[test]
    fn test_prd98_truncate_after_edge_all() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        append_msg(&store, "s1", "user", "a");
        append_msg(&store, "s1", "assistant", "b");
        // seq = -1 → delete all (seq > -1)
        store.truncate_after("s1", -1).unwrap();
        let msgs = store.get_messages("s1").unwrap();
        assert_eq!(msgs.len(), 0, "truncate at -1 = delete all");
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions[0].message_count, 0);
    }

    #[test]
    fn test_prd98_truncate_after_nonexistent() {
        let store = SessionStore::open_in_memory().unwrap();
        let err = store.truncate_after("nope", 0).unwrap_err();
        assert!(err.to_string().contains("session not found"), "got: {err}");
    }

    #[test]
    fn test_prd98_checkpoint_list() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        append_msg(&store, "s1", "user", "first prompt");
        append_msg(&store, "s1", "assistant", "reply");
        append_msg(&store, "s1", "user", "second prompt here");
        append_msg(&store, "s1", "assistant", "reply2");
        append_msg(&store, "s1", "user", "third");

        let cps = store.checkpoint_list("s1").unwrap();
        assert_eq!(cps.len(), 3, "3 user messages = 3 checkpoints");
        assert_eq!(cps[0].seq, 0);
        assert_eq!(cps[0].content_preview, "first prompt");
        assert_eq!(cps[1].seq, 2);
        assert_eq!(cps[1].content_preview, "second prompt here");
        assert_eq!(cps[2].seq, 4);
        assert_eq!(cps[2].content_preview, "third");
        assert_eq!(cps[0].role, "user");
        assert!(!cps[0].created_at.is_empty());
    }

    #[test]
    fn test_prd98_checkpoint_list_empty() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        let cps = store.checkpoint_list("s1").unwrap();
        assert!(cps.is_empty());
    }

    #[test]
    fn test_prd98_checkpoint_list_nonexistent() {
        let store = SessionStore::open_in_memory().unwrap();
        let err = store.checkpoint_list("nope").unwrap_err();
        assert!(err.to_string().contains("session not found"), "got: {err}");
    }

    #[test]
    fn test_prd98_checkpoint_list_preview_truncation() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        let long = "x".repeat(200);
        append_msg(&store, "s1", "user", &long);
        let cps = store.checkpoint_list("s1").unwrap();
        assert_eq!(cps.len(), 1);
        // truncate() returns first 80 chars + "..." = 83 when content > 80.
        assert!(
            cps[0].content_preview.len() <= 83,
            "preview must be <= 83 (80 + ellipsis), got {}",
            cps[0].content_preview.len()
        );
        assert!(cps[0].content_preview.ends_with("..."));
        assert!(cps[0].content_preview.starts_with('x'));
    }

    /// R11 fix #1: UTF-8 safe truncate — multi-byte chars must not panic.
    #[test]
    fn test_prd98_checkpoint_list_preview_utf8_multibyte() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        // Emoji + CJK: each char is 3-4 bytes. 80-byte cut likely lands
        // mid-character. Must NOT panic.
        let content = "🎉你好世界".repeat(30); // ~630 bytes
        append_msg(&store, "s1", "user", &content);
        let cps = store.checkpoint_list("s1").unwrap();
        assert_eq!(cps.len(), 1);
        assert!(cps[0].content_preview.ends_with("..."));
        // Preview must be valid UTF-8 (no panic, no replacement char).
        assert!(cps[0].content_preview.chars().all(|c| c != '\u{FFFD}'));
    }

    /// R11 fix #3: truncate_after at assistant seq is allowed at API level
    /// (CLI layer enforces checkpoint validation). Document this behavior.
    #[test]
    fn test_prd98_truncate_after_at_assistant_seq() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        append_msg(&store, "s1", "user", "q"); // seq 0
        append_msg(&store, "s1", "assistant", "a"); // seq 1
        append_msg(&store, "s1", "user", "q2"); // seq 2
        // API allows truncating at seq 1 (assistant) — CLI rejects this
        // but the low-level method is generic (truncate to any seq).
        store.truncate_after("s1", 1).unwrap();
        let msgs = store.get_messages("s1").unwrap();
        assert_eq!(msgs.len(), 2, "truncate at seq 1 keeps seq 0,1");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[test]
    fn test_prd113_plan_roundtrip() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        assert!(store.get_plan("s1").unwrap().is_none());
        store.update_plan("s1", "1. do x\n2. do y").unwrap();
        assert_eq!(store.get_plan("s1").unwrap().unwrap(), "1. do x\n2. do y");
    }

    #[test]
    fn test_prd113_approvals_roundtrip() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("s1", "p", None).unwrap();
        assert!(store.get_approvals("s1").unwrap().is_empty());
        store
            .update_approvals("s1", &["ok:edit".into(), "deny:rm".into()])
            .unwrap();
        let apps = store.get_approvals("s1").unwrap();
        assert_eq!(apps, vec!["ok:edit".to_string(), "deny:rm".to_string()]);
    }

    #[test]
    fn test_prd113_export_import_roundtrip() {
        let store = SessionStore::open_in_memory().unwrap();
        store
            .create_session("s1", "my prompt", Some("gpt-4"))
            .unwrap();
        append_msg(&store, "s1", "user", "hello");
        append_msg(&store, "s1", "assistant", "hi there");
        store.update_plan("s1", "plan A").unwrap();
        store.update_approvals("s1", &["ok:read".into()]).unwrap();
        let json = store.export_session("s1").unwrap();
        // Import into a fresh store.
        let store2 = SessionStore::open_in_memory().unwrap();
        let id = store2.import_session(&json).unwrap();
        assert_eq!(id, "s1");
        let msgs = store2.get_messages("s1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(store2.get_plan("s1").unwrap().unwrap(), "plan A");
        assert_eq!(
            store2.get_approvals("s1").unwrap(),
            vec!["ok:read".to_string()]
        );
    }
}
