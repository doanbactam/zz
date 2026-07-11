//! Git worktree management tool for ZeroZero .
//!
//! Manages git worktrees for isolated file changes:
//! - `create`: create a new worktree from a branch or commit
//! - `list`: list all worktrees
//! - `remove`: remove a worktree
//!
//! Safety: only works within the current repository.

use crate::Tool;
use std::path::PathBuf;

pub struct GitWorktreeTool {
    working_dir: Option<PathBuf>,
}

impl GitWorktreeTool {
    pub const fn new() -> Self {
        Self { working_dir: None }
    }

    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: Some(dir.into()),
        }
    }

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

impl Default for GitWorktreeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for GitWorktreeTool {
    fn name(&self) -> &str {
        "git_worktree"
    }

    fn description(&self) -> &str {
        "Manage git worktrees for isolated file changes. \
         Actions: `create` (new worktree), `list` (show all), `remove` (delete one). \
         Required: `action` (create|list|remove). \
         For create: `branch` (name) and `path` (directory). \
         For remove: `path` (worktree directory)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "remove"],
                    "description": "Action to perform"
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name for create action"
                },
                "path": {
                    "type": "string",
                    "description": "Worktree path (for create/remove)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: action"))?;

        match action {
            "create" => {
                let branch = args["branch"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing required field: branch"))?;
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?;

                let output = self
                    .apply_dir(
                        tokio::process::Command::new("git")
                            .args(["worktree", "add", "-b", branch, path]),
                    )
                    .output()
                    .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("git worktree add failed: {stderr}");
                }

                let stdout = String::from_utf8_lossy(&output.stdout);
                Ok(format!("Created worktree '{branch}' at {path}\n{stdout}"))
            }
            "list" => {
                let output = self
                    .apply_dir(tokio::process::Command::new("git").args(["worktree", "list"]))
                    .output()
                    .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("git worktree list failed: {stderr}");
                }

                let stdout = String::from_utf8_lossy(&output.stdout);
                Ok(stdout.trim().to_string())
            }
            "remove" => {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing required field: path"))?;

                let output = self
                    .apply_dir(
                        tokio::process::Command::new("git").args(["worktree", "remove", path]),
                    )
                    .output()
                    .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("git worktree remove failed: {stderr}");
                }

                Ok(format!("Removed worktree at {path}"))
            }
            other => anyhow::bail!("unknown action: {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_metadata() {
        let tool = GitWorktreeTool::new();
        assert_eq!(tool.name(), "git_worktree");
        assert!(tool.description().contains("worktree"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = GitWorktreeTool::new();
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["action"]["type"], "string");
    }

    #[test]
    fn test_default() {
        let tool = GitWorktreeTool::default();
        assert_eq!(tool.name(), "git_worktree");
    }
}
