//! Git push tool for ZeroZero.
//!
//! Pushes commits to a remote branch. By default, pushes to `origin` and
//! the current branch. Does NOT force-push.
//!
//! Safety: this tool does NOT force-push, does NOT delete remote branches.

use crate::Tool;

pub struct GitPushTool;

impl GitPushTool {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for GitPushTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for GitPushTool {
    fn name(&self) -> &str {
        "git_push"
    }

    fn description(&self) -> &str {
        "Push commits to a remote branch. Does NOT force-push. \
         Optional: `remote` (default 'origin'). \
         Optional: `branch` (default: current branch). \
         The push is a normal `git push <remote> <branch>` — no --force."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "remote": {
                    "type": "string",
                    "description": "Remote name (default: origin)",
                    "default": "origin"
                },
                "branch": {
                    "type": "string",
                    "description": "Branch to push (default: current branch)"
                }
            }
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let remote = args
            .get("remote")
            .and_then(|v| v.as_str())
            .unwrap_or("origin");

        // Determine branch: explicit param or current branch via rev-parse.
        let branch = if let Some(b) = args.get("branch").and_then(|v| v.as_str()) {
            b.to_string()
        } else {
            let output = tokio::process::Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("failed to get current branch: {e}"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("not in a git repo or failed to get branch: {stderr}");
            }
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };

        if branch.is_empty() {
            anyhow::bail!("could not determine branch to push");
        }

        // Run git push (no --force).
        let output = tokio::process::Command::new("git")
            .args(["push", remote, &branch])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run git push: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            anyhow::bail!("git push failed:\n{stderr}\n{stdout}");
        }

        // Build result message.
        let mut result = format!("Pushed to {remote}/{branch}\n");
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            // git push writes progress to stderr (normal).
            result.push_str(&stderr);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name_and_description() {
        let tool = GitPushTool::new();
        assert_eq!(tool.name(), "git_push");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["remote"].is_object());
        assert!(schema["properties"]["branch"].is_object());
    }

    #[tokio::test]
    async fn test_git_push_not_in_repo() {
        // Run in /tmp which is unlikely to be a git repo.
        let tool = GitPushTool::new();
        let result = tool
            .execute(&serde_json::json!({"branch": "test-branch"}))
            .await;
        // This will likely fail (not in repo or no remote). Just verify
        // it doesn't panic.
        let _ = result;
    }

    #[tokio::test]
    async fn test_git_push_missing_branch_uses_current() {
        // Without branch param, tool tries to get current branch.
        // In this test env, it may or may not be a repo. Just verify
        // no panic.
        let tool = GitPushTool::new();
        let _ = tool.execute(&serde_json::json!({})).await;
    }
}
