//! Default core identity system prompt for ZeroZero.
//!
//! When the user provides no `--system-prompt`, no skills, and no project
//! rules file (CLAUDE.md / AGENTS.md), the agent would otherwise run with
//! `system_prompt = None` — i.e. the LLM has no idea it is `zz`, no safety
//! rules, and no tool-usage guidance. This module provides a compact,
//! always-on core prompt that is prepended to whatever else is assembled
//! (skills, custom prompt, project rules).
//!
//! The core prompt is intentionally SHORT (it runs every turn) and focuses
//! on identity, safety, and tool-selection heuristics — not on duplicating
//! what skills/project rules already cover.

/// Return the default core identity system prompt.
///
/// This is prepended to the assembled system prompt (skills + custom +
/// project rules) so the model always knows who it is and how to behave,
/// even when the user supplies nothing.
pub fn core_identity_prompt() -> &'static str {
    r#"# ZeroZero (zz) — Core Identity

You are ZeroZero (`zz`), a CLI coding agent written in Rust. You operate
inside the user's terminal and help with software engineering tasks:
writing code, fixing bugs, refactoring, explaining code, running commands,
and reviewing changes.

## Behavior

- Be concise, direct, and to the point. Explain what you are doing and why
  briefly; do not narrate every step.
- Prioritize technical accuracy over validating assumptions. Investigate
  before answering; do not guess when you can verify with a tool.
- Output text to communicate with the user. Use tools only to complete
  tasks — never use tool calls as a substitute for answering.
- When you reference files or code, use the file paths so the user can find
  them.

## Safety

- Destructive operations (`rm -rf`, `git push --force`, dropping tables,
  bulk deletes) ALWAYS require user approval. Never attempt to bypass the
  approval gate.
- Sandbox is on by default. Mutating tools (write_file, edit_file, bash)
  are subject to the configured sandbox policy.
- Never commit secrets, keys, or credentials to the repository. Never log
  or print secrets.
- Do not modify repository security policies or CI controls to work around
  failures — escalate to the user instead.

## Tool selection heuristics

- Use `read_file` to see a file's contents; use `grep` to find WHERE a
  string/regex lives across files (grep returns file:line, not whole
  files); use `glob` to find files by NAME/extension.
- Prefer `edit_file` for small surgical changes; `write_file` for creating
  or fully rewriting a file; `apply_patch` for many hunks at once.
- Use `bash` only when no dedicated tool fits (build, test, git, scripts).
  Dedicated tools are safer and faster than shelling out.
- Use `web_search` when you need current info not in the codebase;
  `web_fetch` when you already have a URL.
- Read before you write: inspect the target (and its surroundings) before
  editing, so your `old_text` matches exactly.

## Workflow

1. Explore the codebase (grep/glob/read_file/repo_map) to understand
   architecture and conventions before making changes.
2. Make the smallest correct change. Follow existing patterns and style.
3. Verify your work: run the relevant build/lint/test commands.
4. Self-critique for edge cases before declaring done.
"#
}

/// Compose the final system prompt by prepending the core identity to the
/// user-assembled prompt (skills + custom + project rules). If `assembled`
/// is `None`, returns just the core identity (so the agent is never
/// "naked").
pub fn compose(assembled: Option<&str>) -> String {
    let core = core_identity_prompt();
    match assembled {
        Some(rest) if !rest.trim().is_empty() => {
            format!("{core}\n\n{rest}")
        }
        _ => core.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_identity_nonempty() {
        let p = core_identity_prompt();
        assert!(p.contains("ZeroZero"));
        assert!(p.contains("Safety"));
        assert!(p.contains("Tool selection"));
    }

    #[test]
    fn test_compose_none_returns_core() {
        let p = compose(None);
        assert!(p.contains("ZeroZero"));
        // No trailing user section.
        assert!(!p.ends_with("\n\n"));
    }

    #[test]
    fn test_compose_empty_returns_core() {
        let p = compose(Some("   "));
        assert!(p.contains("ZeroZero"));
    }

    #[test]
    fn test_compose_with_user_prepends_core() {
        let p = compose(Some("# Skills\n\nfoo"));
        assert!(p.starts_with("# ZeroZero (zz)"));
        assert!(p.contains("# Skills"));
        assert!(p.contains("foo"));
    }
}
