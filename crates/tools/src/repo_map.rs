//! Repo map tool for ZeroZero .
//!
//! Builds a structured overview of the repository: file tree with sizes,
//! language detection by extension, and key file identification. This
//! gives the agent context about the project structure without reading
//! every file.
//!
//! Design: walks the directory tree using `std::fs`, skips common ignore
//! patterns (.git, target, node_modules, etc.), classifies files by
//! extension, and produces a compact text summary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{Tool, ToolCategory};

/// Maximum depth to walk. Prevents runaway traversal on huge repos.
const MAX_DEPTH: usize = 10;
/// Maximum number of files to include in the map.
const MAX_FILES: usize = 500;
/// Maximum file size (in bytes) to consider as "source" vs "data".
const MAX_SOURCE_FILE_SIZE: u64 = 1_000_000;

/// File extensions → language name mapping.
fn language_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("Rust"),
        "py" => Some("Python"),
        "js" | "jsx" => Some("JavaScript"),
        "ts" | "tsx" => Some("TypeScript"),
        "go" => Some("Go"),
        "java" => Some("Java"),
        "c" | "h" => Some("C"),
        "cpp" | "hpp" | "cc" => Some("C++"),
        "rb" => Some("Ruby"),
        "php" => Some("PHP"),
        "swift" => Some("Swift"),
        "kt" => Some("Kotlin"),
        "scala" => Some("Scala"),
        "sh" | "bash" => Some("Shell"),
        "yaml" | "yml" => Some("YAML"),
        "toml" => Some("TOML"),
        "json" => Some("JSON"),
        "md" => Some("Markdown"),
        "html" => Some("HTML"),
        "css" => Some("CSS"),
        "sql" => Some("SQL"),
        "dockerfile" => Some("Dockerfile"),
        _ => None,
    }
}

/// Directories to skip during traversal.
fn is_ignored_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | "__pycache__"
            | ".next"
            | "dist"
            | "build"
            | ".cache"
            | ".venv"
            | "venv"
            | ".idea"
            | ".vscode"
            | "vendor"
            | ".hg"
            | ".svn"
            | "coverage"
            | ".turbo"
            | ".parcel-cache"
    )
}

/// A compiled .gitignore pattern. Supports basic glob patterns:
/// - `*` matches any sequence except `/`
/// - `?` matches any single char except `/`
/// - trailing `/` matches directories only
/// - leading `/` anchors to root
/// - `!` negates (un-ignores)
#[derive(Clone, Debug)]
struct GitignorePattern {
    pattern: String,
    negate: bool,
    dir_only: bool,
    #[allow(dead_code)]
    anchored: bool,
}

/// Parse a .gitignore file into a list of patterns.
/// Comments (#) and blank lines are skipped.
fn parse_gitignore(content: &str) -> Vec<GitignorePattern> {
    let mut patterns = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (line, negate) = if let Some(rest) = line.strip_prefix('!') {
            (rest, true)
        } else {
            (line, false)
        };
        let dir_only = line.ends_with('/');
        let pattern = line.trim_end_matches('/');
        let anchored = pattern.starts_with('/');
        let pattern = pattern.trim_start_matches('/').to_string();
        patterns.push(GitignorePattern {
            pattern,
            negate,
            dir_only,
            anchored,
        });
    }
    patterns
}

/// Load .gitignore from a directory, if present.
fn load_gitignore(dir: &Path) -> Vec<GitignorePattern> {
    let gitignore_path = dir.join(".gitignore");
    match std::fs::read_to_string(&gitignore_path) {
        Ok(content) => parse_gitignore(&content),
        Err(_) => Vec::new(),
    }
}

/// Check if a path matches a gitignore pattern using simple glob matching.
fn matches_pattern(path: &str, pattern: &GitignorePattern) -> bool {
    let pat = &pattern.pattern;
    if pat == path || pat == path.split('/').next_back().unwrap_or(path) {
        return true;
    }
    // Simple glob: * matches any sequence (except /)
    if pat.contains('*') {
        glob_match(path, pat)
    } else {
        // Check if pattern matches any suffix component
        path.ends_with(pat) || path.contains(&format!("/{pat}"))
    }
}

/// Simple glob matcher: * matches any chars except /, ? matches one char.
fn glob_match(text: &str, pattern: &str) -> bool {
    let text_chars: Vec<char> = text.chars().collect();
    let pat_chars: Vec<char> = pattern.chars().collect();
    glob_match_inner(&text_chars, &pat_chars)
}

fn glob_match_inner(text: &[char], pattern: &[char]) -> bool {
    let mut ti = 0;
    let mut pi = 0;
    let mut star_ti = None;
    let mut star_pi = None;

    while ti < text.len() {
        if pi < pattern.len() && pattern[pi] == '*' {
            star_pi = Some(pi);
            star_ti = Some(ti);
            pi += 1;
        } else if pi < pattern.len() && (pattern[pi] == text[ti] || pattern[pi] == '?') {
            ti += 1;
            pi += 1;
        } else if let Some(spi) = star_pi {
            pi = spi + 1;
            star_ti = Some(star_ti.unwrap() + 1);
            ti = star_ti.unwrap();
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

/// Check if a file/dir should be ignored based on gitignore patterns.
/// `path` is relative to the root where .gitignore was loaded.
fn is_ignored_by_gitignore(path: &str, is_dir: bool, patterns: &[GitignorePattern]) -> bool {
    let mut ignored = false;
    for p in patterns {
        if p.dir_only && !is_dir {
            continue;
        }
        if matches_pattern(path, p) {
            ignored = !p.negate;
        }
    }
    ignored
}

/// A file entry in the repo map.
#[derive(Clone)]
struct FileEntry {
    path: PathBuf,
    size: u64,
    language: Option<&'static str>,
}

/// Walk the directory tree and collect file entries.
/// Respects .gitignore files found at each directory level .
fn walk_dir(root: &Path, entries: &mut Vec<FileEntry>, depth: usize) {
    walk_dir_with_gitignore(root, root, entries, depth, &[]);
}

fn walk_dir_with_gitignore(
    root: &Path,
    current: &Path,
    entries: &mut Vec<FileEntry>,
    depth: usize,
    parent_patterns: &[GitignorePattern],
) {
    if depth > MAX_DEPTH || entries.len() >= MAX_FILES {
        return;
    }
    // Load .gitignore from current directory (merge with parent patterns).
    let local_patterns = load_gitignore(current);
    let mut all_patterns = parent_patterns.to_vec();
    all_patterns.extend(local_patterns);

    let Ok(items) = std::fs::read_dir(current) else {
        return;
    };
    for item in items {
        if entries.len() >= MAX_FILES {
            return;
        }
        let Ok(entry) = item else {
            continue;
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Skip hidden files/dirs (starting with '.') except .github
        if name.starts_with('.') && name != ".github" {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        // Compute relative path for gitignore matching.
        let rel_path = path
            .strip_prefix(root)
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or(name);

        if metadata.is_dir() {
            if is_ignored_dir(name) {
                continue;
            }
            if is_ignored_by_gitignore(rel_path, true, &all_patterns) {
                continue;
            }
            walk_dir_with_gitignore(root, &path, entries, depth + 1, &all_patterns);
        } else if metadata.is_file() {
            if is_ignored_by_gitignore(rel_path, false, &all_patterns) {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let language = language_for_extension(ext);
            if metadata.len() <= MAX_SOURCE_FILE_SIZE {
                entries.push(FileEntry {
                    path: path.clone(),
                    size: metadata.len(),
                    language,
                });
            }
        }
    }
}

/// Build the repo map text from file entries.
fn build_map(entries: &[FileEntry], root: &Path) -> String {
    let mut map = String::new();

    // 1. Language statistics
    let mut lang_counts: HashMap<&str, (usize, u64)> = HashMap::new();
    for entry in entries {
        if let Some(lang) = entry.language {
            let e = lang_counts.entry(lang).or_insert((0, 0));
            e.0 += 1;
            e.1 += entry.size;
        }
    }
    let mut langs: Vec<(&str, (usize, u64))> = lang_counts.into_iter().collect();
    langs.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));

    map.push_str("## Languages\n\n");
    if langs.is_empty() {
        map.push_str("(no recognized source files)\n");
    } else {
        for (lang, (count, size)) in &langs {
            map.push_str(&format!("- {lang}: {count} files, {size} bytes\n"));
        }
    }

    // 2. File tree (relative paths, sorted)
    map.push_str("\n## Files\n\n");
    let mut paths: Vec<&PathBuf> = entries.iter().map(|e| &e.path).collect();
    paths.sort();
    for path in &paths {
        if let Ok(rel) = path.strip_prefix(root) {
            map.push_str(&format!("{}", rel.display()));
        } else {
            map.push_str(&format!("{}", path.display()));
        }
        if let Some(entry) = entries.iter().find(|e| &e.path == *path) {
            if let Some(lang) = entry.language {
                map.push_str(&format!(" [{lang}, {}B]", entry.size));
            } else if entry.size > 0 {
                map.push_str(&format!(" [{}B]", entry.size));
            }
        }
        map.push('\n');
    }

    // 3. Key files (commonly important)
    map.push_str("\n## Key Files\n\n");
    let key_names = [
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "setup.py",
        "Makefile",
        "Dockerfile",
        ".github/workflows",
        "README.md",
        "CLAUDE.md",
        "AGENTS.md",
    ];
    let mut found_key = false;
    for key in &key_names {
        if let Some(entry) = entries.iter().find(|e| {
            e.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == *key || e.path.ends_with(key))
                .unwrap_or(false)
        }) {
            if let Ok(rel) = entry.path.strip_prefix(root) {
                map.push_str(&format!("- {} ({} bytes)\n", rel.display(), entry.size));
                found_key = true;
            }
        }
    }
    if !found_key {
        map.push_str("(no standard key files found)\n");
    }

    // 4. Summary stats
    map.push_str("\n## Summary\n\n");
    map.push_str(&format!("- Total files mapped: {}\n", entries.len()));
    let total_size: u64 = entries.iter().map(|e| e.size).sum();
    map.push_str(&format!("- Total size: {} bytes\n", total_size));
    if entries.len() >= MAX_FILES {
        map.push_str(&format!("- (truncated at {MAX_FILES} files)\n"));
    }

    map
}

pub struct RepoMapTool;

impl RepoMapTool {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for RepoMapTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for RepoMapTool {
    fn is_read_only(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "repo_map"
    }

    fn description(&self) -> &str {
        "Build a structured overview of the repository: file tree with sizes, \
         language statistics, and key file identification. Use this to \
         understand project structure without reading every file. \
         Optional `path` parameter to map a subdirectory (default: current dir)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to map (default: current working directory)"
                }
            }
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn when_to_use(&self) -> Option<&str> {
        Some(
            "you are new to a codebase and need a structural overview before diving into specific files",
        )
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some("you already know which file you need — use `read_file` directly")
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = Path::new(path);
        if !root.exists() {
            anyhow::bail!("path does not exist: {}", root.display());
        }
        if !root.is_dir() {
            anyhow::bail!("path is not a directory: {}", root.display());
        }
        let mut entries = Vec::new();
        walk_dir(root, &mut entries, 0);
        Ok(build_map(&entries, root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_language_for_extension() {
        assert_eq!(language_for_extension("rs"), Some("Rust"));
        assert_eq!(language_for_extension("py"), Some("Python"));
        assert_eq!(language_for_extension("unknown"), None);
    }

    #[test]
    fn test_is_ignored_dir() {
        assert!(is_ignored_dir(".git"));
        assert!(is_ignored_dir("target"));
        assert!(is_ignored_dir("node_modules"));
        assert!(!is_ignored_dir("src"));
        assert!(!is_ignored_dir("crates"));
    }

    #[tokio::test]
    async fn test_repo_map_basic() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# Test").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn foo() {}").unwrap();

        let tool = RepoMapTool::new();
        let result = tool
            .execute(&serde_json::json!({"path": root.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(result.contains("Rust"), "Should detect Rust language");
        assert!(result.contains("main.rs"), "Should list main.rs");
        assert!(result.contains("src/lib.rs"), "Should list src/lib.rs");
        assert!(result.contains("README.md"), "Should list README.md");
        assert!(
            result.contains("## Languages"),
            "Should have languages section"
        );
        assert!(result.contains("## Files"), "Should have files section");
        assert!(result.contains("## Summary"), "Should have summary section");
    }

    #[tokio::test]
    async fn test_repo_map_ignores_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/debug.rs"), "should be ignored").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "should be ignored").unwrap();

        let tool = RepoMapTool::new();
        let result = tool
            .execute(&serde_json::json!({"path": root.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(!result.contains("target/debug.rs"), "Should ignore target/");
        assert!(!result.contains(".git/config"), "Should ignore .git/");
        assert!(result.contains("main.rs"), "Should include main.rs");
    }

    #[tokio::test]
    async fn test_repo_map_key_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("README.md"), "# Project").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let tool = RepoMapTool::new();
        let result = tool
            .execute(&serde_json::json!({"path": root.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(
            result.contains("## Key Files"),
            "Should have key files section"
        );
        assert!(
            result.contains("Cargo.toml"),
            "Should identify Cargo.toml as key"
        );
        assert!(
            result.contains("README.md"),
            "Should identify README.md as key"
        );
    }

    #[tokio::test]
    async fn test_repo_map_nonexistent_path() {
        let tool = RepoMapTool::new();
        let result = tool
            .execute(&serde_json::json!({"path": "/nonexistent/path/xyz"}))
            .await;
        assert!(result.is_err(), "Should error on nonexistent path");
    }

    #[tokio::test]
    async fn test_repo_map_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let tool = RepoMapTool::new();
        let result = tool
            .execute(&serde_json::json!({"path": tmp.path().to_str().unwrap()}))
            .await
            .unwrap();

        assert!(
            result.contains("Total files mapped: 0"),
            "Should report 0 files for empty dir. Got: {result}"
        );
    }

    #[tokio::test]
    async fn test_repo_map_default_path() {
        let tool = RepoMapTool::new();
        // Default path "." should work (current directory)
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_ok(), "Should work with default path");
    }

    #[test]
    fn test_tool_name_and_description() {
        let tool = RepoMapTool::new();
        assert_eq!(tool.name(), "repo_map");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
    }
}
