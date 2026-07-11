//! Tools for ZeroZero — Tool trait, ToolRegistry, and standard tools.
//!
//! scope : 6 standard tools (read_file, write_file, edit_file,
//! bash, grep, glob). Tool trait with async execute. ToolRegistry for dispatch.
//!
//! BashTool/WriteFileTool/EditFileTool now hold Arc<SandboxPolicy>
//! for sandbox enforcement (Landlock pre_exec + path validation).

mod apply_patch;
mod bash;
mod edit_file;
mod fuzzy_search;
mod git_commit;
mod git_pr;
mod git_push;
mod git_worktree;
mod glob;
mod grep;
mod mcp_adapter;
mod mcp_server;
mod read_file;
mod repo_map;
pub mod snapshot;
mod web_fetch;
mod web_search;
mod write_file;

pub use apply_patch::{
    ApplyPatchTool, Hunk, HunkLine, ParsedPatch, PatchFile, apply_hunks, parse_patch,
};
pub use bash::BashTool;
pub use edit_file::EditFileTool;
pub use fuzzy_search::{fuzzy_find_files, fuzzy_score};
pub use git_commit::GitCommitTool;
pub use git_pr::{GitPrTool, build_pr_args, create_branch_per_task, slugify_task_name};
pub use git_push::GitPushTool;
pub use git_worktree::GitWorktreeTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use mcp_adapter::{McpToolAdapter, register_mcp_tools};
pub use mcp_server::{McpServer, standard_server};
pub use read_file::ReadFileTool;
pub use repo_map::RepoMapTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
pub use write_file::WriteFileTool;

use std::sync::Arc;
use zerozero_sandbox::ApprovalPolicy;
use zerozero_sandbox::NetPolicy;
use zerozero_sandbox::SandboxPolicy;

/// Tool category — used for grouping, filtering, and tool-search routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    /// Read-only inspection: read_file, grep, glob, repo_map.
    Read,
    /// Mutating file ops: write_file, edit_file, apply_patch.
    Write,
    /// Shell / process execution: bash.
    Exec,
    /// Search / navigation: grep, glob, fuzzy_search.
    Search,
    /// Git operations: git_commit, git_push, git_pr, git_worktree.
    Git,
    /// Network: web_search, web_fetch.
    Web,
    /// MCP-bridged external tools.
    Mcp,
    /// Fallback for tools that don't fit a standard bucket.
    Other,
}

impl ToolCategory {
    /// Stable string label (used in schema metadata + logs).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Exec => "exec",
            Self::Search => "search",
            Self::Git => "git",
            Self::Web => "web",
            Self::Mcp => "mcp",
            Self::Other => "other",
        }
    }
}

/// Abstract tool trait. Each tool implements this and is registered in
/// `ToolRegistry`. The agent loop dispatches tool calls by name.
///
/// Metadata methods (`category`, `when_to_use`, `when_not_to_use`,
/// `examples`, `error_hints`) have default impls returning "no data" so
/// existing tools keep compiling; tools opt in by overriding. The
/// registry merges this metadata into the description sent to the LLM
/// (see [`ToolRegistry::definitions`]) so models pick the right tool and
/// call it with the right arguments.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String>;

    fn is_read_only(&self) -> bool {
        false
    }

    /// Coarse category for grouping / tool-search routing.
    fn category(&self) -> ToolCategory {
        ToolCategory::Other
    }

    /// Short "use this when …" hint. Merged into the LLM-facing description.
    fn when_to_use(&self) -> Option<&str> {
        None
    }

    /// Short "don't use this when …" hint. Reduces mis-selection
    /// (e.g. use `grep` not `read_file` to find a string).
    fn when_not_to_use(&self) -> Option<&str> {
        None
    }

    /// Concrete call examples (JSON arg strings). Merged into the
    /// LLM-facing description so the model sees valid invocations.
    fn examples(&self) -> Vec<String> {
        Vec::new()
    }

    /// Hints returned to the LLM when `execute` fails, helping it recover
    /// (e.g. "old_text must match exactly — use grep first to locate it").
    /// Default: none; tools that have common failure modes should override.
    fn error_hints(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// Registry of available tools. Dispatches by name.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    index: std::collections::HashMap<String, usize>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            index: std::collections::HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.index.insert(name, self.tools.len());
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.index.get(name).map(|&i| self.tools[i].as_ref())
    }

    /// Build the LLM-facing description for a tool by merging its base
    /// description with metadata (`when_to_use`, `when_not_to_use`,
    /// `examples`, `error_hints`). This is what makes the model pick the
    /// right tool and call it with valid arguments.
    fn rich_description(t: &dyn Tool) -> String {
        let mut out = t.description().to_string();
        if let Some(w) = t.when_to_use() {
            out.push_str("\n\nUse when: ");
            out.push_str(w);
        }
        if let Some(w) = t.when_not_to_use() {
            out.push_str("\n\nDon't use when: ");
            out.push_str(w);
        }
        let ex = t.examples();
        if !ex.is_empty() {
            out.push_str("\n\nExamples:");
            for e in ex {
                out.push_str(&format!("\n  {e}"));
            }
        }
        let hints = t.error_hints();
        if !hints.is_empty() {
            out.push_str("\n\nCommon errors:");
            for h in hints {
                out.push_str(&format!("\n  - {h}"));
            }
        }
        out
    }

    /// Enforce `additionalProperties: false` on an object schema so the
    /// LLM cannot hallucinate parameters that don't exist. No-op for
    /// non-object schemas.
    fn strictify(schema: serde_json::Value) -> serde_json::Value {
        if let Some(obj) = schema.as_object() {
            if obj.get("type").and_then(|t| t.as_str()) == Some("object") {
                let mut m = obj.clone();
                m.insert(
                    "additionalProperties".to_string(),
                    serde_json::Value::Bool(false),
                );
                return serde_json::Value::Object(m);
            }
        }
        schema
    }

    /// Return OpenAI `tools` array — one entry per tool. The description
    /// is enriched with metadata and the parameters schema is hardened
    /// with `additionalProperties: false`.
    pub fn definitions(&self) -> Vec<serde_json::Value> {
        self.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": Self::rich_description(t.as_ref()),
                        "parameters": Self::strictify(t.parameters_schema()),
                    }
                })
            })
            .collect()
    }

    /// Return an MCP `tools/list` snapshot — one entry per tool, shaped
    /// for the in-repo `zerozero-mcp` client : `name`,
    /// `description`, and **`input_schema`** (snake_case, matching
    /// `crates/mcp/src/lib.rs::McpTool`'s serde deserialization — NOT the
    /// spec's camelCase `inputSchema`).
    pub fn tools_snapshot(&self) -> Vec<serde_json::Value> {
        self.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name(),
                    "description": Self::rich_description(t.as_ref()),
                    "input_schema": Self::strictify(t.parameters_schema()),
                })
            })
            .collect()
    }

    /// Like `standard`, but applies a network policy to the BashTool
    /// (network namespace isolation).
    pub fn standard_with_net(sandbox: Arc<SandboxPolicy>, net: Arc<NetPolicy>) -> Self {
        let mut reg = Self::new();
        reg.register(Box::new(ReadFileTool));
        reg.register(Box::new(WriteFileTool::new(sandbox.clone())));
        reg.register(Box::new(EditFileTool::new(sandbox.clone())));
        reg.register(Box::new(ApplyPatchTool::new(
            sandbox.clone(),
            Arc::new(ApprovalPolicy::OnRequest),
        )));
        reg.register(Box::new(BashTool::new(sandbox).with_net_policy(net)));
        reg.register(Box::new(GrepTool));
        reg.register(Box::new(GlobTool));
        reg.register(Box::new(RepoMapTool::new()));
        reg.register(Box::new(WebSearchTool::new()));
        reg.register(Box::new(WebFetchTool::new()));
        reg.register(Box::new(GitCommitTool::new()));
        reg.register(Box::new(GitPushTool::new()));
        reg.register(Box::new(GitPrTool::new()));
        reg.register(Box::new(GitWorktreeTool::new()));
        reg
    }

    /// Standard set: all tools. BashTool/WriteFileTool/EditFileTool
    /// receive the sandbox policy for enforcement .
    ///+: repo_map, web_search, git_commit added .
    /// defaults to `NetPolicy::None` (isolated network namespace).
    pub fn standard(sandbox: Arc<SandboxPolicy>) -> Self {
        Self::standard_with_net(sandbox, Arc::new(NetPolicy::None))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Return the names of tools whose name, description, or parameter-field names
/// contain a fuzzy (subsequence) match for `query`, ranked by descending score
/// (tool-search router, Codex parity).
///
/// An empty `query` returns all tool names, preserving registry order. No
/// network or LLM access is performed (safe).
pub fn filter_tools_by_query(registry: &ToolRegistry, query: &str) -> Vec<String> {
    let query = query.trim();
    if query.is_empty() {
        return registry
            .tools
            .iter()
            .map(|t| t.name().to_string())
            .collect();
    }
    let mut scored: Vec<(i32, String)> = registry
        .tools
        .iter()
        .filter_map(|t| {
            let name = t.name();
            let desc = t.description();
            let params = t
                .parameters_schema()
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|o| o.keys().cloned().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            let blob = format!("{name} {desc} {params}");
            let s = fuzzy_score(query, &blob);
            if s < 0 {
                None
            } else {
                Some((s, name.to_string()))
            }
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, n)| n).collect()
}

/// Helper to extract a string field from JSON args.
fn get_str(args: &serde_json::Value, field: &str) -> anyhow::Result<String> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing or invalid field '{field}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_get() {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("write_file").is_some());
        assert!(reg.get("edit_file").is_some());
        assert!(reg.get("bash").is_some());
        assert!(reg.get("grep").is_some());
        assert!(reg.get("glob").is_some());
        assert!(reg.get("repo_map").is_some());
        assert!(reg.get("web_search").is_some());
        assert!(reg.get("git_commit").is_some());
        assert!(reg.get("git_push").is_some());
        assert!(reg.get("git_pr").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn test_registry_definitions() {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
        let defs = reg.definitions();
        assert_eq!(defs.len(), 14);
        for def in &defs {
            assert_eq!(def["type"], "function");
            assert!(def["function"]["name"].is_string());
            assert!(def["function"]["description"].is_string());
            assert!(def["function"]["parameters"].is_object());
        }
    }

    #[test]
    fn test_standard_with_readonly_sandbox() {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::ReadOnly));
        assert!(reg.get("bash").is_some());
        assert!(reg.get("write_file").is_some());
    }

    #[test]
    fn test_is_read_only_readonly_tools() {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
        for name in ["read_file", "grep", "glob", "repo_map", "web_search"] {
            assert!(
                reg.get(name).unwrap().is_read_only(),
                "{name} should be read-only"
            );
        }
    }

    #[test]
    fn test_filter_tools_by_query_empty_returns_all() {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
        let all = filter_tools_by_query(&reg, "");
        // Empty query returns every registered tool (no hard-coded count — the
        // registry grows as features land).
        assert!(all.len() >= 11, "expected all tools, got {}", all.len());
        assert!(all.iter().any(|n| n == "read_file"));
    }

    #[test]
    fn test_filter_tools_by_query_matching() {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
        // "grep" should fuzzy-match the grep tool by name.
        let matched = filter_tools_by_query(&reg, "grep");
        assert!(matched.iter().any(|n| n == "grep"));
        // It should not match a tool name/description entirely unrelated.
        let none = filter_tools_by_query(&reg, "zzzzqqq");
        assert!(none.is_empty());
    }

    #[test]
    fn test_filter_tools_by_query_ranked() {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
        // Query "git" ranks git_* tools; all returned names should contain the
        // subsequence somewhere in name/description.
        let matched = filter_tools_by_query(&reg, "git");
        assert!(matched.iter().any(|n| n == "git_commit"));
        assert!(matched.iter().any(|n| n == "git_push"));
    }

    #[test]
    fn test_is_read_only_mutating_tools() {
        let reg = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
        for name in [
            "write_file",
            "edit_file",
            "bash",
            "git_commit",
            "git_push",
            "git_pr",
            "git_worktree",
        ] {
            assert!(
                !reg.get(name).unwrap().is_read_only(),
                "{name} should be mutating"
            );
        }
    }
}
