//! `glob` tool — find files matching an extended glob pattern.
//!
//! Upgraded from the original `*`-only implementation:
//! - `**` — recursive wildcard (matches across directory separators).
//! - `?` — single character.
//! - `{a,b,c}` — alternation (match any of the comma-separated options).
//! - `max_results` (int, default 100) — caller-controlled cap.
//! - Skips common junk dirs (.git, target, node_modules) automatically.
//! - `pattern` may include `/` to constrain directory segments, e.g.
//!   `src/**/*.rs` matches `src/a.rs`, `src/foo/b.rs`, etc.

use crate::{Tool, ToolCategory, get_str};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_RESULTS: usize = 100;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".next", "dist", ".cache"];

pub struct GlobTool;

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn is_read_only(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern. Supports `*` (within one path \
         segment), `**` (recursive, across directories), `?` (single char), \
         and `{a,b}` alternation. Searches recursively under `path`. Use `/` \
         in the pattern to constrain directory segments (e.g. `src/**/*.rs`). \
         Common build/VCS dirs (.git, target, node_modules) are skipped."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern. Supports *, **, ?, and {a,b}. e.g. \"*.rs\", \"src/**/*.rs\", \"test_?.{rs,txt}\""
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of file paths to return. Default: 100.",
                    "minimum": 1
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
            "you need to find files by NAME/extension pattern (not by content — use `grep` for content)",
        )
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some("you need to search file CONTENTS — use `grep` instead")
    }

    fn examples(&self) -> Vec<String> {
        vec![
            r#"{"pattern": "*.rs", "path": "src"}"#.to_string(),
            r#"{"pattern": "src/**/*.rs", "path": "."}"#.to_string(),
            r#"{"pattern": "test_?.{rs,txt}", "path": "tests"}"#.to_string(),
        ]
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let pattern = get_str(args, "pattern")?;
        let path = get_str(args, "path")?;
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_RESULTS);

        let segments = parse_pattern(&pattern);
        let mut results = Vec::new();
        walk(
            Path::new(&path),
            &[],
            &segments,
            0,
            &mut results,
            max_results,
        );
        if results.is_empty() {
            Ok("(no matches)".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }
}

/// A parsed glob pattern is a sequence of path segments, each either a
/// recursive `**` or a single-segment mini-glob.
#[derive(Debug, Clone)]
enum Segment {
    DoubleStar,
    Single(SingleGlob),
}

#[derive(Debug, Clone)]
struct SingleGlob {
    parts: Vec<SinglePart>,
}

#[derive(Clone, Debug)]
enum SinglePart {
    Star,
    Question,
    Literal(String),
    Alternation(Vec<SingleGlob>),
}

fn parse_pattern(pattern: &str) -> Vec<Segment> {
    pattern
        .split('/')
        .map(|seg| {
            if seg == "**" {
                Segment::DoubleStar
            } else {
                Segment::Single(parse_single(seg))
            }
        })
        .collect()
}

fn parse_single(s: &str) -> SingleGlob {
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                flush_lit(&mut buf, &mut parts);
                parts.push(SinglePart::Star);
            }
            '?' => {
                flush_lit(&mut buf, &mut parts);
                parts.push(SinglePart::Question);
            }
            '{' => {
                flush_lit(&mut buf, &mut parts);
                // Collect until matching '}'.
                let mut inner = String::new();
                let mut depth = 1;
                for ic in chars.by_ref() {
                    if ic == '{' {
                        depth += 1;
                        inner.push(ic);
                    } else if ic == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        inner.push(ic);
                    } else {
                        inner.push(ic);
                    }
                }
                let alts: Vec<SingleGlob> = inner.split(',').map(parse_single).collect();
                parts.push(SinglePart::Alternation(alts));
            }
            _ => buf.push(c),
        }
    }
    flush_lit(&mut buf, &mut parts);
    SingleGlob { parts }
}

fn flush_lit(buf: &mut String, parts: &mut Vec<SinglePart>) {
    if !buf.is_empty() {
        parts.push(SinglePart::Literal(std::mem::take(buf)));
    }
}

impl SingleGlob {
    fn matches(&self, name: &str) -> bool {
        single_match(&self.parts, name.as_bytes(), 0)
    }
}

fn single_match(parts: &[SinglePart], s: &[u8], i: usize) -> bool {
    if parts.is_empty() {
        return i == s.len();
    }
    match &parts[0] {
        SinglePart::Star => {
            for k in i..=s.len() {
                if single_match(&parts[1..], s, k) {
                    return true;
                }
            }
            false
        }
        SinglePart::Question => {
            if i < s.len() {
                single_match(&parts[1..], s, i + 1)
            } else {
                false
            }
        }
        SinglePart::Literal(lit) => {
            let lb = lit.as_bytes();
            if i + lb.len() <= s.len() && &s[i..i + lb.len()] == lb {
                single_match(&parts[1..], s, i + lb.len())
            } else {
                false
            }
        }
        SinglePart::Alternation(alts) => {
            for alt in alts {
                if alt_matches_rest(alt, &parts[1..], s, i) {
                    return true;
                }
            }
            false
        }
    }
}

/// Try matching `alt` at position `i`, then continue with `rest`.
fn alt_matches_rest(alt: &SingleGlob, rest: &[SinglePart], s: &[u8], i: usize) -> bool {
    // alt consumes some prefix; for each possible end position where alt
    // matches, try matching rest.
    // We do this by checking all prefix matches of alt.
    for end in alt_prefix_matches(alt, s, i) {
        if single_match(rest, s, end) {
            return true;
        }
    }
    false
}

/// Return all end positions where `alt` matches a prefix of s[i..].
fn alt_prefix_matches(alt: &SingleGlob, s: &[u8], i: usize) -> Vec<usize> {
    let mut ends = Vec::new();
    collect_prefix_matches(&alt.parts, s, i, &mut ends);
    ends
}

fn collect_prefix_matches(parts: &[SinglePart], s: &[u8], i: usize, ends: &mut Vec<usize>) {
    if parts.is_empty() {
        ends.push(i);
        return;
    }
    match &parts[0] {
        SinglePart::Star => {
            for k in i..=s.len() {
                collect_prefix_matches(&parts[1..], s, k, ends);
            }
        }
        SinglePart::Question => {
            if i < s.len() {
                collect_prefix_matches(&parts[1..], s, i + 1, ends);
            }
        }
        SinglePart::Literal(lit) => {
            let lb = lit.as_bytes();
            if i + lb.len() <= s.len() && &s[i..i + lb.len()] == lb {
                collect_prefix_matches(&parts[1..], s, i + lb.len(), ends);
            }
        }
        SinglePart::Alternation(alts) => {
            for alt in alts {
                collect_prefix_matches(&alt.parts, s, i, ends);
            }
        }
    }
}

/// Walk the filesystem, matching the relative path against the segment list.
fn walk(
    dir: &Path,
    rel: &[String],
    segments: &[Segment],
    seg_idx: usize,
    results: &mut Vec<String>,
    max: usize,
) {
    if results.len() >= max {
        return;
    }
    if seg_idx >= segments.len() {
        // All segments consumed — `rel` must point to a file.
        let full = join_rel(dir, rel);
        if full.is_file() {
            results.push(full.display().to_string());
        }
        return;
    }

    match &segments[seg_idx] {
        Segment::DoubleStar => {
            // `**` matches zero or more directories.
            // Zero dirs: try matching current dir against the next segment.
            walk(dir, rel, segments, seg_idx + 1, results, max);
            // One+ dirs: descend into every subdir and re-offer `**`.
            let current = join_rel(dir, rel);
            let entries = match std::fs::read_dir(&current) {
                Ok(e) => e,
                Err(_) => return,
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let p = entry.path();
                if p.is_dir() {
                    let name = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    if SKIP_DIRS.contains(&name.as_str()) {
                        continue;
                    }
                    let mut next_rel = rel.to_vec();
                    next_rel.push(name);
                    walk(dir, &next_rel, segments, seg_idx, results, max);
                }
                if results.len() >= max {
                    return;
                }
            }
        }
        Segment::Single(sg) => {
            let current = join_rel(dir, rel);
            let entries = match std::fs::read_dir(&current) {
                Ok(e) => e,
                Err(_) => return,
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let p = entry.path();
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if p.is_dir() {
                    if SKIP_DIRS.contains(&name.as_str()) {
                        continue;
                    }
                    if sg.matches(&name) {
                        let mut next_rel = rel.to_vec();
                        next_rel.push(name);
                        walk(dir, &next_rel, segments, seg_idx + 1, results, max);
                    }
                } else if p.is_file() && sg.matches(&name) {
                    let mut next_rel = rel.to_vec();
                    next_rel.push(name);
                    walk(dir, &next_rel, segments, seg_idx + 1, results, max);
                }
                if results.len() >= max {
                    return;
                }
            }
        }
    }
}

/// Build a full path from `dir` + the relative segment list.
fn join_rel(dir: &Path, rel: &[String]) -> PathBuf {
    let mut p = dir.to_path_buf();
    for r in rel {
        p.push(r);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_glob_match_rs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("c.rs"), "fn test() {}").unwrap();
        let tool = GlobTool;
        let args = serde_json::json!({
            "pattern": "*.rs",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("a.rs"));
        assert!(result.contains("c.rs"));
        assert!(!result.contains("b.txt"));
    }

    #[tokio::test]
    async fn test_glob_no_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tool = GlobTool;
        let args = serde_json::json!({
            "pattern": "*.rs",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "(no matches)");
    }

    #[tokio::test]
    async fn test_glob_double_star() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/foo")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/foo/b.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/foo/c.txt"), "").unwrap();
        let tool = GlobTool;
        let args = serde_json::json!({
            "pattern": "src/**/*.rs",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("a.rs"));
        assert!(result.contains("b.rs"));
        assert!(!result.contains("c.txt"));
    }

    #[tokio::test]
    async fn test_glob_alternation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("c.md"), "").unwrap();
        let tool = GlobTool;
        let args = serde_json::json!({
            "pattern": "*.{rs,txt}",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("a.rs"));
        assert!(result.contains("b.txt"));
        assert!(!result.contains("c.md"));
    }

    #[tokio::test]
    async fn test_glob_question() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t_a.rs"), "").unwrap();
        std::fs::write(dir.path().join("t_ab.rs"), "").unwrap();
        let tool = GlobTool;
        let args = serde_json::json!({
            "pattern": "t_?.rs",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("t_a.rs"));
        assert!(!result.contains("t_ab.rs"));
    }

    #[tokio::test]
    async fn test_glob_skips_junk_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/built.rs"), "").unwrap();
        std::fs::write(dir.path().join("real.rs"), "").unwrap();
        let tool = GlobTool;
        let args = serde_json::json!({
            "pattern": "**/*.rs",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("real.rs"));
        assert!(!result.contains("built.rs"));
    }

    #[test]
    fn test_single_glob_basic() {
        let g = parse_single("*.rs");
        assert!(g.matches("foo.rs"));
        assert!(!g.matches("foo.txt"));
    }

    #[test]
    fn test_single_glob_alternation() {
        let g = parse_single("*.{rs,txt}");
        assert!(g.matches("a.rs"));
        assert!(g.matches("a.txt"));
        assert!(!g.matches("a.md"));
    }
}
