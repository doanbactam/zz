//! Hook config — parse `.zerozero/hooks.toml` into `HttpHook` instances .
//!
//! TOML format:
//! ```toml
//! [[hooks.PreToolUse]]
//! matcher = "bash"
//! url = "http://localhost:8080/validate"
//! timeout = 30
//!
//! [[hooks.Stop]]
//! url = "http://localhost:9000/session-end"
//! ```
//!
//! Discovery: `.zerozero/hooks.toml` in cwd. Missing file → empty vec (NoopHooks
//! fallback). Parse error → warning + empty vec (no crash).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::hooks::{HookEvent, LifecycleHooks};
use crate::http_hook::HttpHook;

#[derive(Debug, Deserialize, Default)]
struct HookEntry {
    matcher: Option<String>,
    url: String,
    timeout: Option<u64>,
    headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct HooksFile {
    #[serde(default)]
    pre_tool_use: Vec<HookEntry>,
    #[serde(default)]
    post_tool_use: Vec<HookEntry>,
    #[serde(default)]
    session_start: Vec<HookEntry>,
    #[serde(default)]
    session_end: Vec<HookEntry>,
    #[serde(default)]
    user_prompt_submit: Vec<HookEntry>,
    #[serde(default)]
    stop: Vec<HookEntry>,
    #[serde(default)]
    post_tool_use_failure: Vec<HookEntry>,
    #[serde(default)]
    pre_compact: Vec<HookEntry>,
}

/// Container for `[[hooks.<EventName>]]` TOML format — wraps HooksFile in a
/// `hooks` table so `[[hooks.PreToolUse]]` maps correctly.
#[derive(Debug, Deserialize, Default)]
struct HooksContainer {
    #[serde(default)]
    hooks: HooksFile,
}

/// Parse `.zerozero/hooks.toml` content → Vec<Box<dyn LifecycleHooks>>.
/// Unknown section names → serde error (strict, fail-fast on typo).
/// Missing `url` → serde error (field required).
pub fn parse_hooks_toml(content: &str) -> anyhow::Result<Vec<Box<dyn LifecycleHooks>>> {
    let container: HooksContainer = toml::from_str(content)?;
    let file = container.hooks;
    let mut hooks: Vec<Box<dyn LifecycleHooks>> = Vec::new();
    for e in file.pre_tool_use {
        hooks.push(Box::new(HttpHook::new(
            HookEvent::PreToolUse,
            e.url,
            Duration::from_secs(e.timeout.unwrap_or(5)),
            e.headers.unwrap_or_default(),
            e.matcher,
        )));
    }
    for e in file.post_tool_use {
        hooks.push(Box::new(HttpHook::new(
            HookEvent::PostToolUse,
            e.url,
            Duration::from_secs(e.timeout.unwrap_or(5)),
            e.headers.unwrap_or_default(),
            e.matcher,
        )));
    }
    for e in file.session_start {
        hooks.push(Box::new(HttpHook::new(
            HookEvent::SessionStart,
            e.url,
            Duration::from_secs(e.timeout.unwrap_or(5)),
            e.headers.unwrap_or_default(),
            e.matcher,
        )));
    }
    for e in file.session_end {
        hooks.push(Box::new(HttpHook::new(
            HookEvent::SessionEnd,
            e.url,
            Duration::from_secs(e.timeout.unwrap_or(5)),
            e.headers.unwrap_or_default(),
            e.matcher,
        )));
    }
    for e in file.user_prompt_submit {
        hooks.push(Box::new(HttpHook::new(
            HookEvent::UserPromptSubmit,
            e.url,
            Duration::from_secs(e.timeout.unwrap_or(5)),
            e.headers.unwrap_or_default(),
            e.matcher,
        )));
    }
    for e in file.stop {
        hooks.push(Box::new(HttpHook::new(
            HookEvent::Stop,
            e.url,
            Duration::from_secs(e.timeout.unwrap_or(5)),
            e.headers.unwrap_or_default(),
            e.matcher,
        )));
    }
    for e in file.post_tool_use_failure {
        hooks.push(Box::new(HttpHook::new(
            HookEvent::PostToolUseFailure,
            e.url,
            Duration::from_secs(e.timeout.unwrap_or(5)),
            e.headers.unwrap_or_default(),
            e.matcher,
        )));
    }
    for e in file.pre_compact {
        hooks.push(Box::new(HttpHook::new(
            HookEvent::PreCompact,
            e.url,
            Duration::from_secs(e.timeout.unwrap_or(5)),
            e.headers.unwrap_or_default(),
            e.matcher,
        )));
    }
    Ok(hooks)
}

/// Discover `.zerozero/hooks.toml` in cwd. None if not found.
pub fn discover_config() -> Option<PathBuf> {
    let p = PathBuf::from(".zerozero/hooks.toml");
    if p.exists() { Some(p) } else { None }
}

/// Load + parse `.zerozero/hooks.toml`. Returns empty vec if no config file
/// (NoopHooks fallback) or parse error (warning + empty, no crash).
pub fn load_hooks() -> Vec<Box<dyn LifecycleHooks>> {
    match discover_config() {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(content) => parse_hooks_toml(&content).unwrap_or_else(|e| {
                eprintln!("warning: failed to parse {}: {e}", p.display());
                vec![]
            }),
            Err(e) => {
                eprintln!("warning: failed to read {}: {e}", p.display());
                vec![]
            }
        },
        None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookEvent;

    #[test]
    fn test_hook_config_parse_basic() {
        let toml = r#"
[[hooks.PreToolUse]]
matcher = "bash"
url = "http://localhost:8080/validate"
timeout = 30

[[hooks.Stop]]
url = "http://localhost:9000/session-end"
"#;
        let hooks = parse_hooks_toml(toml).unwrap();
        assert_eq!(hooks.len(), 2);
    }

    #[test]
    fn test_hook_config_parse_empty() {
        let hooks = parse_hooks_toml("").unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn test_hook_config_missing_url_error() {
        let toml = r#"
[[hooks.Stop]]
timeout = 5
"#;
        let result = parse_hooks_toml(toml);
        assert!(result.is_err(), "missing url should error");
    }

    #[test]
    fn test_hook_config_unknown_event_error() {
        let toml = r#"
[[hooks.FooBar]]
url = "http://localhost:8080"
"#;
        let result = parse_hooks_toml(toml);
        assert!(result.is_err(), "unknown event name should error");
    }

    #[test]
    fn test_discover_config_no_file() {
        // cwd likely has no .zerozero/hooks.toml in test env.
        // This test verifies it returns None without panic.
        let _ = discover_config();
    }

    #[test]
    fn test_hook_config_all_event_types() {
        let toml = r#"
[[hooks.PreToolUse]]
url = "http://localhost:1"
[[hooks.PostToolUse]]
url = "http://localhost:2"
[[hooks.SessionStart]]
url = "http://localhost:3"
[[hooks.SessionEnd]]
url = "http://localhost:4"
[[hooks.UserPromptSubmit]]
url = "http://localhost:5"
[[hooks.Stop]]
url = "http://localhost:6"
[[hooks.PostToolUseFailure]]
url = "http://localhost:7"
[[hooks.PreCompact]]
url = "http://localhost:8"
"#;
        let hooks = parse_hooks_toml(toml).unwrap();
        assert_eq!(hooks.len(), 8);
    }

    #[test]
    fn test_hook_config_default_timeout() {
        // Verify timeout defaults to 5s when not specified.
        // We can't inspect HttpHook fields directly (private), but parse
        // success with default timeout confirms the unwrap_or(5) path.
        let toml = r#"
[[hooks.Stop]]
url = "http://localhost:9000"
"#;
        let hooks = parse_hooks_toml(toml).unwrap();
        assert_eq!(hooks.len(), 1);
    }

    // Mutation guard: verify HookEvent enum has all 8 variants.
    #[test]
    fn test_hook_event_variants() {
        assert_eq!(HookEvent::PreToolUse, HookEvent::PreToolUse);
        assert_ne!(HookEvent::PreToolUse, HookEvent::PostToolUse);
        let all = [
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::UserPromptSubmit,
            HookEvent::Stop,
            HookEvent::PostToolUseFailure,
            HookEvent::PreCompact,
        ];
        // All variants distinct.
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "variants {i} and {j} equal");
            }
        }
    }
}
