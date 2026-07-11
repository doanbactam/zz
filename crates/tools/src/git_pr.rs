//! Git PR creation tool for ZeroZero).
//!
//! Creates a GitHub pull request via the `gh` CLI, with an optional
//! autopush policy (push the current branch before creating the PR) and
//! branch-per-task helpers (parity with F12 vs Codex).
//!
//! Safety: this tool is mutating (it opens a PR / pushes), so it is gated
//! by the same approval path as `git_push`/`git_commit` (it is NOT
//! read-only). It never force-pushes and never deletes branches.

use crate::Tool;
use std::path::PathBuf;

pub struct GitPrTool {
    working_dir: Option<PathBuf>,
}

impl GitPrTool {
    pub const fn new() -> Self {
        Self { working_dir: None }
    }

    /// Create a tool that runs git/gh commands in the specified directory.
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

    /// Push the current branch to the given remote before opening the PR.
    async fn autopush(&self, remote: &str) -> anyhow::Result<String> {
        // Determine current branch.
        let branch = self.current_branch().await?;
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["push", remote, &branch]);
        let output = self
            .apply_dir(&mut cmd)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run git push: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            anyhow::bail!("git push failed:\n{stderr}\n{stdout}");
        }
        let mut result = format!("Pushed {remote}/{branch}\n");
        result.push_str(&stdout);
        result.push_str(&stderr); // git push writes progress to stderr (normal).
        Ok(result)
    }

    /// Resolve the current branch name via `git rev-parse --abbrev-ref HEAD`.
    async fn current_branch(&self) -> anyhow::Result<String> {
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["rev-parse", "--abbrev-ref", "HEAD"]);
        let output = self
            .apply_dir(&mut cmd)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to get current branch: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("not in a git repo or failed to get branch: {stderr}");
        }
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() {
            anyhow::bail!("could not determine branch to push");
        }
        Ok(branch)
    }
}

impl Default for GitPrTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Parsed arguments for `GitPrTool` — used to build `gh pr create` args
/// without performing any side effects (so it can be unit-tested in
/// isolation, no network calls in tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPrArgs {
    pub title: String,
    pub body: Option<String>,
    pub base: String,
    pub draft: bool,
    pub head: Option<String>,
    pub autopush: bool,
    pub remote: String,
}

/// Build the `gh pr create` argument vector from parsed args.
///
/// Pure function — no I/O — so it is trivially testable. The produced
/// command is always `gh pr create --title <t> --body <b> --base <branch>`
/// with optional `--draft` and `--head <h>` flags.
pub fn build_pr_args(args: &GitPrArgs) -> Vec<String> {
    let mut cmd: Vec<String> = vec![
        "pr".to_string(),
        "create".to_string(),
        "--title".to_string(),
        args.title.clone(),
    ];
    if let Some(body) = &args.body {
        cmd.push("--body".to_string());
        cmd.push(body.clone());
    }
    cmd.push("--base".to_string());
    cmd.push(args.base.clone());
    if args.draft {
        cmd.push("--draft".to_string());
    }
    if let Some(head) = &args.head {
        cmd.push("--head".to_string());
        cmd.push(head.clone());
    }
    cmd
}

/// Slugify a task name into a git-safe branch slug, e.g.
/// `"Git PR creation"` -> `"bl-111-git-pr-creation"`.
pub fn slugify_task_name(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// Create a branch-per-task: `git checkout -b task/<slug>`.
///
/// Returns the resolved branch name (e.g. `"task/bl-111-git-pr-creation"`).
/// Pure in the sense that it performs no PR/network action — it only
/// creates a local branch.
pub async fn create_branch_per_task(
    working_dir: Option<&PathBuf>,
    name: &str,
) -> anyhow::Result<String> {
    let slug = slugify_task_name(name);
    let branch = format!("task/{slug}");
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["checkout", "-b", &branch]);
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run git checkout -b: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git checkout -b {branch} failed: {stderr}");
    }
    Ok(branch)
}

#[async_trait::async_trait]
impl Tool for GitPrTool {
    fn name(&self) -> &str {
        "git_pr"
    }

    fn description(&self) -> &str {
        "Open a GitHub pull request via the `gh` CLI. \\\n         Required: `title` (PR title). \\\n         Optional: `body` (PR description), `base` (target branch, default 'main'), \\\n         `draft` (bool — open as draft), `head` (source branch), \\\n         `autopush` (bool — push current branch to `remote` before creating PR, \\\n         default true), `remote` (remote name, default 'origin'). \\\n         Runs `gh pr create --title <t> --body <b> --base <branch>`. \\\n         Approval required (mutating). No force-push, no branch deletion.\""
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "PR title (required)"
                },
                "body": {
                    "type": "string",
                    "description": "PR body / description"
                },
                "base": {
                    "type": "string",
                    "description": "Base branch to target (default: main)",
                    "default": "main"
                },
                "draft": {
                    "type": "boolean",
                    "description": "Open as a draft PR",
                    "default": false
                },
                "head": {
                    "type": "string",
                    "description": "Source branch for the PR (defaults to current branch)"
                },
                "autopush": {
                    "type": "boolean",
                    "description": "Push current branch to remote before creating the PR (default: true)",
                    "default": true
                },
                "remote": {
                    "type": "string",
                    "description": "Remote name for autopush (default: origin)",
                    "default": "origin"
                }
            },
            "required": ["title"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field 'title'"))?;
        if title.trim().is_empty() {
            anyhow::bail!("PR title must not be empty");
        }

        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let base = args
            .get("base")
            .and_then(|v| v.as_str())
            .unwrap_or("main")
            .to_string();
        let draft = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
        let head = args
            .get("head")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let autopush = args
            .get("autopush")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let remote = args
            .get("remote")
            .and_then(|v| v.as_str())
            .unwrap_or("origin")
            .to_string();

        // Optional autopush before opening the PR.
        let mut result = String::new();
        if autopush {
            result.push_str(&self.autopush(&remote).await?);
            result.push('\n');
        }

        // Build and run the gh pr create command.
        let parsed = GitPrArgs {
            title: title.to_string(),
            body,
            base,
            draft,
            head,
            autopush,
            remote,
        };
        let pr_args = build_pr_args(&parsed);

        let mut cmd = tokio::process::Command::new("gh");
        cmd.args(&pr_args);
        let output = self
            .apply_dir(&mut cmd)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run gh pr create: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            anyhow::bail!("gh pr create failed:\n{stderr}\n{stdout}");
        }

        result.push_str(&format!("PR created:\n{stdout}"));
        if !stderr.is_empty() {
            result.push('\n');
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
        let tool = GitPrTool::new();
        assert_eq!(tool.name(), "git_pr");
        assert!(!tool.description().is_empty());
        // Safety: mutating (requires approval), like git_push.
        assert!(!tool.is_read_only());
    }

    #[test]
    fn test_parameters_schema_title_required() {
        let tool = GitPrTool::new();
        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        assert!(
            required.contains(&serde_json::json!("title")),
            "title must be a required parameter"
        );
        assert!(schema["properties"]["title"].is_object());
        assert!(
            schema["properties"]["base"].get("default").is_some(),
            "base should have a default"
        );
    }

    #[test]
    fn test_build_pr_args_minimal() {
        let args = GitPrArgs {
            title: "My PR".to_string(),
            body: None,
            base: "main".to_string(),
            draft: false,
            head: None,
            autopush: false,
            remote: "origin".to_string(),
        };
        let cmd = build_pr_args(&args);
        assert_eq!(
            cmd,
            vec![
                "pr".to_string(),
                "create".to_string(),
                "--title".to_string(),
                "My PR".to_string(),
                "--base".to_string(),
                "main".to_string(),
            ]
        );
    }

    #[test]
    fn test_build_pr_args_with_body_draft_head() {
        let args = GitPrArgs {
            title: "Feature X".to_string(),
            body: Some("Implementation of X".to_string()),
            base: "develop".to_string(),
            draft: true,
            head: Some("task/bl-111".to_string()),
            autopush: true,
            remote: "origin".to_string(),
        };
        let cmd = build_pr_args(&args);
        assert_eq!(
            cmd,
            vec![
                "pr".to_string(),
                "create".to_string(),
                "--title".to_string(),
                "Feature X".to_string(),
                "--body".to_string(),
                "Implementation of X".to_string(),
                "--base".to_string(),
                "develop".to_string(),
                "--draft".to_string(),
                "--head".to_string(),
                "task/bl-111".to_string(),
            ]
        );
    }

    #[test]
    fn test_slugify_task_name() {
        assert_eq!(
            slugify_task_name("Git PR creation"),
            "bl-111-git-pr-creation"
        );
        assert_eq!(slugify_task_name("Hello World!!"), "hello-world");
        assert_eq!(slugify_task_name("  spaced  out  "), "spaced-out");
    }

    #[tokio::test]
    async fn test_create_branch_per_task() {
        // Uses the real git binary in a temp repo (no network).
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().to_path_buf();
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        // Create an initial commit so the branch has a ref to point at.
        std::fs::write(repo.join("seed.txt"), "seed").unwrap();
        tokio::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        let branch = create_branch_per_task(Some(&repo), "Git PR creation")
            .await
            .unwrap();
        assert_eq!(branch, "task/bl-111-git-pr-creation");

        // Verify the branch actually exists and is checked out.
        let out = tokio::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        let current = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(current, branch);
    }

    #[tokio::test]
    async fn test_execute_missing_title() {
        let tool = GitPrTool::new();
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("title"));
    }
}
