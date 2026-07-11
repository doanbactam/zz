//! `grep` tool — regex/substring search in files under a directory.
//!
//! Upgraded from the original substring-only implementation:
//! - `is_regex` (bool, default false) — treat `pattern` as a regex.
//! - `case_insensitive` (bool, default false).
//! - `context_lines` (int) — show N lines before/after each match.
//! - `output_mode` — "content" (default, file:line: text),
//!   "files_with_matches" (just file list), or "count" (per-file counts).
//! - `max_results` (int, default 100) — caller-controlled cap.
//! - `glob_pattern` — only search files whose name matches a simple glob.
//! - Skips common junk dirs (.git, target, node_modules) automatically.

use crate::{Tool, ToolCategory, get_str};
use regex::Regex;
use std::path::Path;

const DEFAULT_MAX_RESULTS: usize = 100;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".next", "dist", ".cache"];

pub struct GrepTool;

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn is_read_only(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for a pattern in files under a directory. By default the \
         pattern is a literal substring; set `is_regex: true` to use a regex. \
         Returns matching lines as `file:line: text` (content mode), a file \
         list (files_with_matches), or per-file counts (count). Use \
         `context_lines` to show surrounding lines and `case_insensitive` \
         for case-insensitive search. Common build/VCS dirs (.git, target, \
         node_modules) are skipped automatically."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Pattern to search for (substring by default, regex when is_regex=true)"
                },
                "path": {
                    "type": "string",
                    "description": "Directory (or file) to search in"
                },
                "is_regex": {
                    "type": "boolean",
                    "description": "Treat pattern as a regex. Default: false (literal substring)."
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case-insensitive match. Default: false."
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Lines of context to show before and after each match. Default: 0.",
                    "minimum": 0
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Output shape. Default: content."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of matches to return. Default: 100.",
                    "minimum": 1
                },
                "glob_pattern": {
                    "type": "string",
                    "description": "Only search files whose name matches this simple glob (e.g. \"*.rs\"). Default: all files."
                }
            },
            "required": ["pattern", "path"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Search
    }

    fn when_to_use(&self) -> Option<&str> {
        Some(
            "you need to find WHERE a string/regex occurs across files (returns file:line, not whole files)",
        )
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some("you already know the file — use `read_file` to see its contents")
    }

    fn examples(&self) -> Vec<String> {
        vec![
            r#"{"pattern": "TODO", "path": "src"}"#.to_string(),
            r#"{"pattern": "fn \\w+\\(", "path": "src", "is_regex": true, "glob_pattern": "*.rs"}"#
                .to_string(),
            r#"{"pattern": "error", "path": "logs", "output_mode": "count"}"#.to_string(),
        ]
    }

    fn error_hints(&self) -> Vec<&str> {
        vec![
            "if is_regex=true and you get a regex parse error, escape regex metacharacters or use is_regex=false",
            "binary/non-UTF-8 files are skipped silently — if you expected matches there, they won't appear",
        ]
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let pattern = get_str(args, "pattern")?;
        let path = get_str(args, "path")?;
        let is_regex = args
            .get("is_regex")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let case_insensitive = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let context_lines = args
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(0);
        let output_mode = args
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("content");
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_RESULTS);
        let glob_pattern = args.get("glob_pattern").and_then(|v| v.as_str());

        let matcher = build_matcher(&pattern, is_regex, case_insensitive)?;
        let glob = glob_pattern.map(SimpleGlob::new);

        let mut acc = MatchAccumulator::new(output_mode, max_results, context_lines);
        search_path(Path::new(&path), &matcher, glob.as_ref(), &mut acc)?;
        Ok(acc.finish())
    }
}

/// Build either a regex or a literal substring matcher.
enum Matcher {
    Literal { needle: String, lower: bool },
    Regex(Regex),
}

impl Matcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal { needle, lower } => {
                if *lower {
                    line.to_lowercase().contains(&needle.to_lowercase())
                } else {
                    line.contains(needle)
                }
            }
            Self::Regex(re) => re.is_match(line),
        }
    }
}

fn build_matcher(pattern: &str, is_regex: bool, case_insensitive: bool) -> anyhow::Result<Matcher> {
    if is_regex {
        let re = if case_insensitive {
            regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
        } else {
            Regex::new(pattern)
        };
        Ok(Matcher::Regex(re?))
    } else {
        Ok(Matcher::Literal {
            needle: pattern.to_string(),
            lower: case_insensitive,
        })
    }
}

/// Very small glob matcher: supports `*` (any chars) and `?` (one char),
/// matched against the file NAME (not full path).
struct SimpleGlob {
    parts: Vec<GlobPart>,
}

enum GlobPart {
    Star,
    Question,
    Literal(String),
}

impl SimpleGlob {
    fn new(pat: &str) -> Self {
        let mut parts = Vec::new();
        let mut buf = String::new();
        for c in pat.chars() {
            match c {
                '*' => {
                    if !buf.is_empty() {
                        parts.push(GlobPart::Literal(std::mem::take(&mut buf)));
                    }
                    parts.push(GlobPart::Star);
                }
                '?' => {
                    if !buf.is_empty() {
                        parts.push(GlobPart::Literal(std::mem::take(&mut buf)));
                    }
                    parts.push(GlobPart::Question);
                }
                _ => buf.push(c),
            }
        }
        if !buf.is_empty() {
            parts.push(GlobPart::Literal(buf));
        }
        Self { parts }
    }

    fn matches(&self, name: &str) -> bool {
        glob_match(&self.parts, name.as_bytes(), 0)
    }
}

fn glob_match(parts: &[GlobPart], s: &[u8], i: usize) -> bool {
    if parts.is_empty() {
        return i == s.len();
    }
    match &parts[0] {
        GlobPart::Star => {
            // Try to consume 0..=rest bytes.
            for k in i..=s.len() {
                if glob_match(&parts[1..], s, k) {
                    return true;
                }
            }
            false
        }
        GlobPart::Question => {
            if i < s.len() {
                glob_match(&parts[1..], s, i + 1)
            } else {
                false
            }
        }
        GlobPart::Literal(lit) => {
            let lb = lit.as_bytes();
            if i + lb.len() <= s.len() && &s[i..i + lb.len()] == lb {
                glob_match(&parts[1..], s, i + lb.len())
            } else {
                false
            }
        }
    }
}

struct MatchAccumulator {
    mode: String,
    max: usize,
    context: usize,
    matches: Vec<String>,
    files: std::collections::BTreeSet<String>,
    counts: std::collections::BTreeMap<String, usize>,
    total: usize,
}

impl MatchAccumulator {
    fn new(mode: &str, max: usize, context: usize) -> Self {
        Self {
            mode: mode.to_string(),
            max,
            context,
            matches: Vec::new(),
            files: std::collections::BTreeSet::new(),
            counts: std::collections::BTreeMap::new(),
            total: 0,
        }
    }

    fn at_cap(&self) -> bool {
        self.total >= self.max
    }

    fn record(&mut self, path: &str, lineno: usize, line: &str, all_lines: &[&str], start: usize) {
        self.total += 1;
        match self.mode.as_str() {
            "files_with_matches" => {
                self.files.insert(path.to_string());
            }
            "count" => {
                *self.counts.entry(path.to_string()).or_insert(0) += 1;
            }
            _ => {
                // content
                if self.context == 0 {
                    self.matches.push(format!("{path}:{lineno}: {line}"));
                } else {
                    let lo = start.saturating_sub(self.context);
                    let hi = (start + 1 + self.context).min(all_lines.len());
                    let mut block = format!("{path}:{lineno}: {line}");
                    for (idx, l) in all_lines[lo..hi].iter().enumerate() {
                        let real = lo + idx + 1;
                        if real == lineno {
                            continue;
                        }
                        block.push_str(&format!("\n{path}-{real}- {l}"));
                    }
                    self.matches.push(block);
                }
            }
        }
    }

    fn finish(self) -> String {
        if self.total == 0 {
            return "(no matches)".to_string();
        }
        match self.mode.as_str() {
            "files_with_matches" => self.files.into_iter().collect::<Vec<_>>().join("\n"),
            "count" => self
                .counts
                .into_iter()
                .map(|(f, c)| format!("{f}: {c}"))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => {
                let mut out = self.matches.join("\n");
                if self.total >= self.max {
                    out.push_str(&format!(
                        "\n(showing {}/{})",
                        self.matches.len(),
                        self.total
                    ));
                }
                out
            }
        }
    }
}

fn search_path(
    path: &Path,
    matcher: &Matcher,
    glob: Option<&SimpleGlob>,
    acc: &mut MatchAccumulator,
) -> anyhow::Result<()> {
    if acc.at_cap() {
        return Ok(());
    }
    if path.is_dir() {
        search_dir(path, matcher, glob, acc)?;
    } else if path.is_file() {
        search_file(path, matcher, glob, acc)?;
    }
    Ok(())
}

fn search_dir(
    dir: &Path,
    matcher: &Matcher,
    glob: Option<&SimpleGlob>,
    acc: &mut MatchAccumulator,
) -> anyhow::Result<()> {
    if acc.at_cap() {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            search_dir(&path, matcher, glob, acc)?;
        } else if path.is_file() {
            search_file(&path, matcher, glob, acc)?;
        }
        if acc.at_cap() {
            return Ok(());
        }
    }
    Ok(())
}

fn search_file(
    path: &Path,
    matcher: &Matcher,
    glob: Option<&SimpleGlob>,
    acc: &mut MatchAccumulator,
) -> anyhow::Result<()> {
    // Apply glob filter on the file name.
    if let Some(g) = glob {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !g.matches(name) {
            return Ok(());
        }
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(()), // Skip binary/non-UTF8 files
    };
    let path_str = path.display().to_string();
    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if matcher.is_match(line) {
            acc.record(&path_str, i + 1, line, &lines, i);
            if acc.at_cap() {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grep_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\nfoo bar").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello rust\nbaz").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "hello",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("hello world"));
        assert!(result.contains("hello rust"));
    }

    #[tokio::test]
    async fn test_grep_no_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "nonexistent_pattern",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "(no matches)");
    }

    #[tokio::test]
    async fn test_grep_regex() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\nfn bar() {}\n").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "fn \\w+\\(\\)",
            "path": dir.path().to_str().unwrap(),
            "is_regex": true
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("fn foo()"));
        assert!(result.contains("fn bar()"));
    }

    #[tokio::test]
    async fn test_grep_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "Hello World\n").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "hello",
            "path": dir.path().to_str().unwrap(),
            "case_insensitive": true
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("Hello World"));
    }

    #[tokio::test]
    async fn test_grep_files_with_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello\n").unwrap();
        std::fs::write(dir.path().join("c.txt"), "bye\n").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "hello",
            "path": dir.path().to_str().unwrap(),
            "output_mode": "files_with_matches"
        });
        let result = tool.execute(&args).await.unwrap();
        let files: Vec<&str> = result.lines().collect();
        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn test_grep_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\nx\ny\n").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "x",
            "path": dir.path().to_str().unwrap(),
            "output_mode": "count"
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains(": 2"));
    }

    #[tokio::test]
    async fn test_grep_glob_filter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "hello\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello\n").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "hello",
            "path": dir.path().to_str().unwrap(),
            "glob_pattern": "*.rs"
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("a.rs"));
        assert!(!result.contains("b.txt"));
    }

    #[tokio::test]
    async fn test_grep_context_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "l1\nl2\nMATCH\nl4\nl5\n").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "MATCH",
            "path": dir.path().to_str().unwrap(),
            "context_lines": 1
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("MATCH"));
        assert!(result.contains("l2"));
        assert!(result.contains("l4"));
    }

    #[tokio::test]
    async fn test_grep_skips_junk_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/built.rs"), "hello\n").unwrap();
        std::fs::write(dir.path().join("src.rs"), "hello\n").unwrap();
        let tool = GrepTool;
        let args = serde_json::json!({
            "pattern": "hello",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("src.rs"));
        assert!(!result.contains("target"));
    }

    #[test]
    fn test_simple_glob() {
        let g = SimpleGlob::new("*.rs");
        assert!(g.matches("foo.rs"));
        assert!(!g.matches("foo.txt"));
        let g2 = SimpleGlob::new("test_?.rs");
        assert!(g2.matches("test_a.rs"));
        assert!(!g2.matches("test_ab.rs"));
    }
}
