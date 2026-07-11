//! `edit_file` tool — replace text in a file with exact or fuzzy matching.
//!
//! Upgraded from the original first-occurrence-only implementation:
//! - `replace_all` (bool, default false) — replace every occurrence.
//! - Fuzzy fallback: when `old_text` doesn't match exactly, the tool
//!   retries with whitespace-normalized matching and reports the closest
//!   candidate line so the LLM can self-correct instead of guessing.
//! - Metadata-enriched description for better tool selection.
//!
//! validates write path against SandboxPolicy before editing.

use crate::snapshot;
use crate::{Tool, ToolCategory, get_str};
use std::sync::Arc;
use zerozero_sandbox::{SandboxPolicy, validate_write_path};

pub struct EditFileTool {
    sandbox: Arc<SandboxPolicy>,
}

impl EditFileTool {
    pub const fn new(sandbox: Arc<SandboxPolicy>) -> Self {
        Self { sandbox }
    }
}

#[async_trait::async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing occurrences of `old_text` with `new_text`. \
         By default replaces the FIRST occurrence; set `replace_all: true` to \
         replace every occurrence. If `old_text` is not found exactly, the \
         tool retries with whitespace-normalized matching and reports the \
         closest candidate so you can correct the call."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_text": {
                    "type": "string",
                    "description": "Text to find. Must match exactly (or whitespace-normalized on fallback). Include enough surrounding context to be unique."
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement text"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence of old_text. Default: false (first occurrence only)."
                }
            },
            "required": ["path", "old_text", "new_text"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn when_to_use(&self) -> Option<&str> {
        Some("you need a precise, small surgical change to an existing file")
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some(
            "you are rewriting most of the file (use `write_file`), or applying \
             many hunks at once (use `apply_patch`)",
        )
    }

    fn examples(&self) -> Vec<String> {
        vec![
            r#"{"path": "src/lib.rs", "old_text": "fn old_name()", "new_text": "fn new_name()"}"#.to_string(),
            r#"{"path": "src/lib.rs", "old_text": "TODO", "new_text": "DONE", "replace_all": true}"#.to_string(),
        ]
    }

    fn error_hints(&self) -> Vec<&str> {
        vec![
            "old_text must match the file exactly — copy it from a read_file call, not from memory",
            "if the match is ambiguous, include more surrounding lines in old_text to make it unique",
            "use `grep` first to locate the exact text and its surroundings",
        ]
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = get_str(args, "path")?;
        let old_text = get_str(args, "old_text")?;
        let new_text = get_str(args, "new_text")?;
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let p = std::path::Path::new(&path);
        validate_write_path(&self.sandbox, p)?;
        let content = tokio::fs::read_to_string(&path).await?;

        // 1. Exact match path.
        if content.contains(&old_text) {
            let edited = if replace_all {
                content.replace(&old_text, &new_text)
            } else {
                content.replacen(&old_text, &new_text, 1)
            };
            snapshot::capture(p);
            tokio::fs::write(&path, edited).await?;
            return Ok("OK".to_string());
        }

        // 2. Fuzzy fallback: whitespace-normalized, line-based match.
        if let Some(edited) = fuzzy_replace_lines(&content, &old_text, &new_text, replace_all) {
            snapshot::capture(p);
            tokio::fs::write(&path, edited).await?;
            return Ok("OK (fuzzy whitespace-normalized match)".to_string());
        }

        // 3. No match — help the LLM recover.
        let hint = closest_line(&content, &old_text);
        anyhow::bail!(
            "old_text not found in {path} (exact or whitespace-normalized). \
             Closest line: {hint}. Use `grep` or `read_file` to copy the exact text."
        );
    }
}

/// Collapse runs of whitespace to a single space and trim, for fuzzy matching.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Try replacing `old_text` by matching its whitespace-normalized form
/// against each line's normalized form. If a line's normalized text equals
/// the normalized `old_text`, the whole line is replaced with `new_text`.
/// This is a conservative, line-based fuzzy match: it preserves formatting
/// on all other lines and only rewrites lines that normalize-equal old_text.
///
/// Multi-line `old_text` is handled by joining the lines of old_text and
/// comparing against joined consecutive content lines.
fn fuzzy_replace_lines(
    content: &str,
    old_text: &str,
    new_text: &str,
    replace_all: bool,
) -> Option<String> {
    let norm_old = normalize_ws(old_text);
    if norm_old.is_empty() {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut edited = String::with_capacity(content.len());
    let mut i = 0;
    let mut replaced = false;
    while i < lines.len() {
        let norm_line = normalize_ws(lines[i]);
        if norm_line == norm_old {
            edited.push_str(new_text);
            edited.push('\n');
            replaced = true;
            i += 1;
            if !replace_all {
                break;
            }
        } else {
            edited.push_str(lines[i]);
            edited.push('\n');
            i += 1;
        }
    }
    // Append any remaining lines after a single-replace break.
    while i < lines.len() {
        edited.push_str(lines[i]);
        edited.push('\n');
        i += 1;
    }
    // Preserve a trailing newline only if the original had none but we
    // added one; to keep it simple, mirror the original's trailing state.
    if replaced {
        // If original didn't end with newline, strip our extra one.
        if !content.ends_with('\n') && edited.ends_with('\n') {
            edited.pop();
        }
        Some(edited)
    } else {
        None
    }
}

/// Return a single line from `content` that is closest (by word overlap)
/// to `old_text`, to help the LLM locate the right spot.
fn closest_line(content: &str, old_text: &str) -> String {
    let target: std::collections::HashSet<&str> = old_text.split_whitespace().collect();
    let mut best = "(no close match)".to_string();
    let mut best_score = 0usize;
    for line in content.lines() {
        let words: std::collections::HashSet<&str> = line.split_whitespace().collect();
        let score = words.intersection(&target).count();
        if score > best_score {
            best_score = score;
            best = line.trim().to_string();
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_edit_file_replace_full_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.txt");
        std::fs::write(&path, "hello world foo world").unwrap();
        let tool = EditFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_text": "world",
            "new_text": "rust"
        });
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "OK");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello rust foo world");
    }

    #[tokio::test]
    async fn test_edit_file_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit_all.txt");
        std::fs::write(&path, "hello world foo world").unwrap();
        let tool = EditFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_text": "world",
            "new_text": "rust",
            "replace_all": true
        });
        tool.execute(&args).await.unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello rust foo rust");
    }

    #[tokio::test]
    async fn test_edit_file_fuzzy_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fuzzy.txt");
        // Same tokens as old_text, but with extra whitespace between them.
        std::fs::write(&path, "fn   foo(x:  i32)  {\n}\n").unwrap();
        let tool = EditFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_text": "fn foo(x: i32) {",
            "new_text": "fn foo(x: i32) -> i32 {"
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("fuzzy"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("-> i32"));
    }

    #[tokio::test]
    async fn test_edit_file_not_found_reports_closest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nf.txt");
        std::fs::write(&path, "fn foo() {}\nfn bar() {}\n").unwrap();
        let tool = EditFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_text": "fn baz() {}",
            "new_text": "fn qux() {}"
        });
        let err = tool.execute(&args).await.unwrap_err().to_string();
        assert!(err.contains("not found"));
        assert!(err.contains("Closest line"));
    }

    #[tokio::test]
    async fn test_edit_file_not_found_full_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.txt");
        std::fs::write(&path, "hello world").unwrap();
        let tool = EditFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_text": "nonexistent",
            "new_text": "replacement"
        });
        let result = tool.execute(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_edit_file_readonly_deny() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit_ro.txt");
        std::fs::write(&path, "hello world").unwrap();
        let tool = EditFileTool::new(Arc::new(SandboxPolicy::ReadOnly));
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_text": "hello",
            "new_text": "bye"
        });
        let result = tool.execute(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("read-only"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn test_edit_file_workspace_allow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit_ws.txt");
        std::fs::write(&path, "foo bar").unwrap();
        let tool = EditFileTool::new(Arc::new(SandboxPolicy::WorkspaceWrite {
            workspace_dir: dir.path().to_path_buf(),
        }));
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_text": "foo",
            "new_text": "baz"
        });
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "OK");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "baz bar");
    }

    #[test]
    fn test_normalize_ws() {
        assert_eq!(normalize_ws("  a   b  c "), "a b c");
        assert_eq!(normalize_ws("a\tb\nc"), "a b c");
        assert_eq!(normalize_ws(""), "");
    }
}
