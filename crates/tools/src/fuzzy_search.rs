//! Fuzzy file-path search.
//!
//! Provides a lightweight subsequence-based fuzzy matcher over file paths
//! (no external dependencies). Used by the TUI `/find` slash command to
//! emulate Codex's fuzzy file-path picker (F11 parity).

use std::path::Path;

/// Maximum number of files to walk before giving up (safety bound).
const MAX_WALK_ENTRIES: usize = 50_000;

/// Score returned when `needle` is not a subsequence of `haystack`.
const NO_MATCH: i32 = -1;

/// Subsequence fuzzy score: higher is better, `-1` means no match.
///
/// Rewards consecutive matches and matches at word boundaries. Case-insensitive.
pub fn fuzzy_score(needle: &str, haystack: &str) -> i32 {
    let needle_chars: Vec<char> = needle
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if needle_chars.is_empty() {
        return 0;
    }
    let hay: Vec<char> = haystack.to_ascii_lowercase().chars().collect();
    let mut ni = 0;
    let mut score = 0i32;
    let mut prev: Option<usize> = None;
    for (hi, hc) in hay.iter().enumerate() {
        if ni < needle_chars.len() && *hc == needle_chars[ni] {
            score += 1;
            // Bonus for consecutive (adjacent) matches.
            if prev.map(|p| p + 1 == hi).unwrap_or(false) {
                score += 3;
            }
            // Bonus for matches at start or after a path separator / whitespace.
            let boundary = hi == 0
                || hay
                    .get(hi - 1)
                    .map(|c| *c == '/' || c.is_whitespace())
                    .unwrap_or(true);
            if boundary {
                score += 2;
            }
            prev = Some(hi);
            ni += 1;
        }
    }
    if ni < needle_chars.len() {
        return NO_MATCH;
    }
    score
}

/// Recursively collect all file paths under `root` (directories skipped).
///
/// Hidden entries (names starting with `.`) and the `.git` directory are
/// pruned to keep results relevant and bounded. Operates on `std` only.
fn walk_files(root: &Path, out: &mut Vec<String>) {
    if out.len() >= MAX_WALK_ENTRIES {
        return;
    }
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_files(&path, out);
        } else if path.is_file() {
            out.push(path.display().to_string());
        }
        if out.len() >= MAX_WALK_ENTRIES {
            return;
        }
    }
}

/// Fuzzy-find files under `root` matching `query`, returning the top `limit`
/// file paths ranked by match score.
///
/// Scoring: the query is matched as a subsequence against both the full
/// (relative) path and the basename. Basename matches receive a boost so that
/// `src/main.rs` outranks `src/vendor/lib/main.rs` for a query like `main`.
pub fn fuzzy_find_files(root: &Path, query: &str, limit: usize) -> Vec<String> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let mut files: Vec<String> = Vec::new();
    walk_files(root, &mut files);

    let mut scored: Vec<(i32, String)> = files
        .into_iter()
        .filter_map(|path| {
            let full = path.to_ascii_lowercase();
            let basename = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let full_score = fuzzy_score(query, &full);
            let base_score = fuzzy_score(query, &basename);
            // Take the best of full-path vs basename match; boost basename hits.
            let best = match (full_score, base_score) {
                (NO_MATCH, NO_MATCH) => return None,
                (f, NO_MATCH) => f,
                (NO_MATCH, b) => b + 50,
                (f, b) => f.max(b + 50),
            };
            Some((best, path))
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_score_basic() {
        assert!(fuzzy_score("main", "main.rs") > 0);
        assert!(fuzzy_score("m", "main.rs") >= 0);
        assert_eq!(fuzzy_score("zzz", "main.rs"), NO_MATCH);
        assert_eq!(fuzzy_score("", "anything"), 0);
    }

    #[test]
    fn test_fuzzy_score_consecutive_bonus() {
        // Consecutive match "ma" beats spread-out "m...a".
        let consecutive = fuzzy_score("ma", "main.rs");
        let spread = fuzzy_score("m", "main.rs");
        assert!(consecutive > spread);
    }

    #[test]
    fn test_fuzzy_find_files_basename_boost() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::create_dir(dir.path().join("vendor")).unwrap();
        std::fs::write(dir.path().join("vendor/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("other.rs"), "fn other() {}").unwrap();
        let results = fuzzy_find_files(dir.path(), "main", 10);
        assert_eq!(results.len(), 2);
        // Top result should be the shallow main.rs (basename boost).
        assert!(results[0].ends_with("main.rs"));
        assert!(!results[0].contains("vendor"));
    }

    #[test]
    fn test_fuzzy_find_files_no_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        assert!(fuzzy_find_files(dir.path(), "zzzz", 10).is_empty());
    }

    #[test]
    fn test_fuzzy_find_files_empty_query() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        assert!(fuzzy_find_files(dir.path(), "  ", 10).is_empty());
    }

    #[test]
    fn test_fuzzy_find_files_limit() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::write(dir.path().join(format!("file{i}.rs")), "x").unwrap();
        }
        let results = fuzzy_find_files(dir.path(), "file", 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_fuzzy_find_files_skips_hidden() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden.rs"), "x").unwrap();
        std::fs::write(dir.path().join("visible.rs"), "x").unwrap();
        let results = fuzzy_find_files(dir.path(), "rs", 10);
        assert!(results.iter().any(|p| p.ends_with("visible.rs")));
        assert!(!results.iter().any(|p| p.contains(".hidden.rs")));
    }
}
