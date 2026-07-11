//! Semi-auto git commit tool for ZeroZero.
//!
//! The agent proposes a commit, and this tool executes `git commit` with
//! a user-provided message. By default it does NOT push. The tool:
//! 1. Stages specified files (or all changes if `stage_all` is true)
//! 2. Runs `git commit -m "<message>"`
//! 3. Returns the commit hash and summary
//!
//! Safety: this tool does NOT push, does NOT force-push, does NOT amend.
//! It refuses to commit if there are no staged changes.

use crate::Tool;
use std::path::PathBuf;

pub struct GitCommitTool {
    working_dir: Option<PathBuf>,
}

impl GitCommitTool {
    pub const fn new() -> Self {
        Self { working_dir: None }
    }

    /// Create a tool that runs git commands in the specified directory.
    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: Some(dir.into()),
        }
    }

    /// Apply `.current_dir()` to a command if working_dir is set.
    fn apply_dir<'a>(
        &'a self,
        cmd: &'a mut tokio::process::Command,
    ) -> &'a mut tokio::process::Command {
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }
        cmd
    }
}

impl Default for GitCommitTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn description(&self) -> &str {
        "Stage files and create a git commit. Does NOT push. \
         Required: `message` (commit message). \
         Optional: `files` (list of file paths to stage). \
         Optional: `stage_all` (bool, default false — stage all tracked changes). \
         The commit is created with `git commit -m`. No push, no force, no amend."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Commit message"
                },
                "files": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Files to stage (git add). If omitted, no explicit staging."
                },
                "stage_all": {
                    "type": "boolean",
                    "description": "If true, run `git add -A` to stage all changes.",
                    "default": false
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field 'message'"))?;
        if message.trim().is_empty() {
            anyhow::bail!("commit message must not be empty");
        }

        let stage_all = args
            .get("stage_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let files: Vec<String> = args
            .get("files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Stage files if requested.
        if stage_all {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["add", "-A"]);
            let output = self
                .apply_dir(&mut cmd)
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("failed to run git add -A: {e}"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git add -A failed: {stderr}");
            }
        } else if !files.is_empty() {
            let mut cmd = tokio::process::Command::new("git");
            cmd.arg("add");
            for f in &files {
                cmd.arg(f);
            }
            let output = self
                .apply_dir(&mut cmd)
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("failed to run git add: {e}"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git add failed: {stderr}");
            }
        }

        // Check if there are staged changes to commit.
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["diff", "--cached", "--quiet"]);
        let status_output = self
            .apply_dir(&mut cmd)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to check staged changes: {e}"))?;
        // git diff --cached --quiet exits 0 if no staged changes, 1 if there are.
        if status_output.status.success() {
            return Ok(
                "No staged changes to commit. Stage files first with `files` or `stage_all`."
                    .to_string(),
            );
        }

        // Create the commit.
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["commit", "-m", message]);
        let output = self
            .apply_dir(&mut cmd)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run git commit: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!("git commit failed: {stderr}\n{stdout}");
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        // Get the commit hash.
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["rev-parse", "HEAD"]);
        let hash_output = self
            .apply_dir(&mut cmd)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("commit succeeded but failed to get hash: {e}"))?;
        let hash = if hash_output.status.success() {
            String::from_utf8_lossy(&hash_output.stdout)
                .trim()
                .to_string()
        } else {
            "(unknown)".to_string()
        };

        // Get short summary of what was committed.
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["show", "--stat", "--oneline", "HEAD"]);
        let short_output = self
            .apply_dir(&mut cmd)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("commit succeeded but failed to get summary: {e}"))?;
        let summary = if short_output.status.success() {
            String::from_utf8_lossy(&short_output.stdout)
                .lines()
                .take(10)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            stdout.clone()
        };

        Ok(format!(
            "Commit created: {hash}\n\n{summary}\n\nNot pushed. Use `git push` when ready."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_git_commit_missing_message() {
        let tool = GitCommitTool::new();
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("message"));
    }

    #[tokio::test]
    async fn test_git_commit_empty_message() {
        let tool = GitCommitTool::new();
        let result = tool.execute(&serde_json::json!({"message": "   "})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[tokio::test]
    async fn test_git_commit_no_staged_changes() {
        // Create a temp git repo with no changes.
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();

        // Init git repo.
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        // Configure git (needed for commit).
        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        // Create an initial commit so HEAD exists.
        std::fs::write(repo.join("initial.txt"), "initial").unwrap();
        tokio::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();

        // Now try to commit with no staged changes.
        let tool = GitCommitTool::with_dir(repo);
        let result = tool
            .execute(&serde_json::json!({"message": "test commit"}))
            .await
            .unwrap();

        assert!(
            result.contains("No staged changes"),
            "Should report no staged changes. Got: {result}"
        );
    }

    #[tokio::test]
    async fn test_git_commit_with_staged_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();

        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();

        // Create a file and commit it.
        std::fs::write(repo.join("file.txt"), "hello world").unwrap();
        let tool = GitCommitTool::with_dir(repo);
        let result = tool
            .execute(&serde_json::json!({
                "message": "add file.txt",
                "stage_all": true
            }))
            .await
            .unwrap();

        assert!(
            result.contains("Commit created:"),
            "Should report commit created. Got: {result}"
        );
        assert!(
            result.contains("Not pushed"),
            "Should mention not pushed. Got: {result}"
        );
        // Verify the commit actually happened.
        let log_output = tokio::process::Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        let log = String::from_utf8_lossy(&log_output.stdout);
        assert!(
            log.contains("add file.txt"),
            "Commit should be in log. Got: {log}"
        );
    }

    #[test]
    fn test_tool_name_and_description() {
        let tool = GitCommitTool::new();
        assert_eq!(tool.name(), "git_commit");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["message"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("message"))
        );
    }
}
