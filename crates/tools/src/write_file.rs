//! `write_file` tool — creates or overwrites a file.
//!
//! validates write path against SandboxPolicy before writing.

use crate::snapshot;
use crate::{Tool, ToolCategory, get_str};
use std::sync::Arc;
use zerozero_sandbox::{SandboxPolicy, validate_write_path};

pub struct WriteFileTool {
    sandbox: Arc<SandboxPolicy>,
}

impl WriteFileTool {
    pub const fn new(sandbox: Arc<SandboxPolicy>) -> Self {
        Self { sandbox }
    }
}

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it does not exist, \
         or overwrites it if it does. Parent directories are created \
         automatically. This is a FULL overwrite — existing contents are \
         replaced, not appended."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Full content to write to the file (replaces existing content)"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn when_to_use(&self) -> Option<&str> {
        Some("you are creating a new file or rewriting most/all of an existing file")
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some(
            "you only need a small surgical change to an existing file (use \
             `edit_file`), or many hunks at once (use `apply_patch`)",
        )
    }

    fn examples(&self) -> Vec<String> {
        vec![r#"{"path": "src/new.rs", "content": "fn main() {}\n"}"#.to_string()]
    }

    fn error_hints(&self) -> Vec<&str> {
        vec![
            "this overwrites the whole file — read it first if you need to preserve parts",
            "parent directories are created automatically, no need to mkdir",
        ]
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = get_str(args, "path")?;
        let content = get_str(args, "content")?;
        let p = std::path::Path::new(&path);
        validate_write_path(&self.sandbox, p)?;
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        snapshot::capture(p);
        tokio::fs::write(p, &content).await?;
        Ok("OK".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_write_file_create_full_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        let tool = WriteFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": "hello"});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "OK");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn test_write_file_overwrite_full_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        std::fs::write(&path, "old content").unwrap();
        let tool = WriteFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": "new content"});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "OK");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new content");
    }

    #[tokio::test]
    async fn test_write_file_create_parent_dirs_full_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/file.txt");
        let tool = WriteFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": "nested"});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "OK");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "nested");
    }

    #[tokio::test]
    async fn test_write_file_readonly_deny() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blocked.txt");
        let tool = WriteFileTool::new(Arc::new(SandboxPolicy::ReadOnly));
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": "x"});
        let result = tool.execute(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("read-only"));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn test_write_file_workspace_allow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("allowed.txt");
        let tool = WriteFileTool::new(Arc::new(SandboxPolicy::WorkspaceWrite {
            workspace_dir: dir.path().to_path_buf(),
        }));
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": "ok"});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "OK");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ok");
    }

    #[tokio::test]
    async fn test_write_file_workspace_deny_outside() {
        let dir = tempfile::tempdir().unwrap();
        // Try to write to /etc (outside workspace) — should be denied.
        let path = std::path::PathBuf::from("/etc/zerozero_sandbox_test_block");
        let tool = WriteFileTool::new(Arc::new(SandboxPolicy::WorkspaceWrite {
            workspace_dir: dir.path().to_path_buf(),
        }));
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": "blocked"});
        let result = tool.execute(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("write denied"));
    }

    // ---B : write_file captures snapshot of existing file ---
    #[tokio::test]
    async fn test_write_file_captures_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        std::fs::write(&path, "original").unwrap();
        let tool = WriteFileTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": "mutated"});
        tool.execute(&args).await.unwrap();
        // Snapshot of the pre-write content must exist.
        assert!(
            crate::snapshot::has(&path),
            "snapshot captured before overwrite"
        );
        // Rewind restores original.
        crate::snapshot::rewind(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }
}
