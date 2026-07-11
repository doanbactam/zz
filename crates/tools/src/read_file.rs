//! `read_file` tool — reads file contents with optional line range,
//! line numbers, binary detection, and metadata.
//!
//! Upgraded from the original 1MB-truncate-only implementation:
//! - `offset` / `limit` (1-based line numbers) for range reads.
//! - `line_numbers` — prefix each line with `N\t` (like `cat -n`).
//! - Binary detection — returns a summary instead of garbled bytes.
//! - `metadata` — return size, line count, modified time without body.
//! - Truncation now reports the byte offset and suggests `offset`.

use crate::{Tool, ToolCategory, get_str};

pub struct ReadFileTool;

const MAX_BYTES: usize = 1_048_576; // 1 MB

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn is_read_only(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. By default returns the full text \
         (truncated at 1MB). Use `offset`/`limit` (1-based line numbers) \
         to read a range, `line_numbers: true` to prefix lines with their \
         number, and `metadata: true` to get size/line-count/mtime without \
         the body. Binary files return a summary instead of garbled output."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "1-based line number to start reading from (inclusive). Default: 1.",
                    "minimum": 1
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read. Default: all (up to 1MB).",
                    "minimum": 1
                },
                "line_numbers": {
                    "type": "boolean",
                    "description": "Prefix each line with its 1-based line number and a tab. Default: false."
                },
                "metadata": {
                    "type": "boolean",
                    "description": "Return only file metadata (size, line count, mtime, is_binary) without the body. Default: false."
                }
            },
            "required": ["path"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn when_to_use(&self) -> Option<&str> {
        Some("you need the contents of a specific file, or a line range within it")
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some(
            "you only need to find WHERE a string/regex lives — use `grep` instead (it reports file:line without loading whole files)",
        )
    }

    fn examples(&self) -> Vec<String> {
        vec![
            r#"{"path": "src/main.rs"}"#.to_string(),
            r#"{"path": "src/main.rs", "offset": 100, "limit": 50, "line_numbers": true}"#
                .to_string(),
            r#"{"path": "Cargo.lock", "metadata": true}"#.to_string(),
        ]
    }

    fn error_hints(&self) -> Vec<&str> {
        vec![
            "if the file is too large, pass `offset` and `limit` to read a slice",
            "if old_text matching fails elsewhere, use `metadata: true` first to learn the line count",
        ]
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = get_str(args, "path")?;
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|n| n.max(1) as usize)
            .unwrap_or(1);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let line_numbers = args
            .get("line_numbers")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let metadata_only = args
            .get("metadata")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let meta = file_metadata(&path).await?;
        if metadata_only {
            return Ok(format!(
                "path: {}\nsize: {} bytes\nlines: {}\nmtime: {}\nbinary: {}",
                path, meta.size, meta.line_count, meta.mtime, meta.is_binary,
            ));
        }

        if meta.is_binary {
            return Ok(format!(
                "[binary file — {} bytes, {} lines not meaningful]\npath: {}",
                meta.size, meta.line_count, path,
            ));
        }

        let content = tokio::fs::read_to_string(&path).await?;

        // Apply line range + truncation.
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = offset.saturating_sub(1);
        if start >= total {
            return Ok(format!(
                "[offset {offset} past end of file — file has {total} lines]"
            ));
        }
        let end = match limit {
            Some(n) => (start + n).min(total),
            None => total,
        };
        let slice = &lines[start..end];

        // Truncation by byte budget on the sliced output.
        let mut out = String::new();
        let mut bytes = 0usize;
        for (i, line) in slice.iter().enumerate() {
            let lineno = start + i + 1;
            let prefix = if line_numbers {
                format!("{lineno}\t")
            } else {
                String::new()
            };
            let candidate_bytes = prefix.len() + line.len() + 1;
            if bytes + candidate_bytes > MAX_BYTES {
                out.push_str(&format!(
                    "\n[truncated at ~{MAX_BYTES} bytes — use offset {} to continue]",
                    lineno
                ));
                break;
            }
            out.push_str(&prefix);
            out.push_str(line);
            out.push('\n');
            bytes += candidate_bytes;
        }
        Ok(out)
    }
}

#[derive(Debug)]
struct FileMeta {
    size: u64,
    line_count: usize,
    mtime: String,
    is_binary: bool,
}

async fn file_metadata(path: &str) -> anyhow::Result<FileMeta> {
    let md = tokio::fs::metadata(path).await?;
    let size = md.len();
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Binary detection: read up to 8KB and check for NUL bytes or
    // invalid UTF-8 (heuristic, like git's binary check).
    let bytes = tokio::fs::read(path).await?;
    let probe = &bytes[..bytes.len().min(8192)];
    let is_binary = probe.contains(&0u8) || std::str::from_utf8(probe).is_err();
    let line_count = if is_binary {
        0
    } else {
        bytes.iter().filter(|&&b| b == b'\n').count().max(1)
    };
    Ok(FileMeta {
        size,
        line_count,
        mtime,
        is_binary,
    })
}

/// Return the largest byte index `<= index` that is a UTF-8 char boundary.
///
/// Replaces `str::floor_char_boundary` (stabilized in Rust 1.91) so the
/// crate stays compatible with the workspace MSRV (1.86). In valid UTF-8 a
/// multi-byte character is at most 4 bytes, so we walk back at most 3.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        s.len()
    } else if index == 0 || s.is_char_boundary(index) {
        index
    } else {
        let mut i = index;
        for _ in 0..3 {
            if s.is_char_boundary(i) {
                return i;
            }
            i -= 1;
        }
        0
    }
}

// Keep the helper exported for other modules that may use it; suppress
// dead-code warning when the new path-based truncation doesn't call it.
#[allow(dead_code)]
fn _ensure_floor_char_boundary_used(s: &str, i: usize) -> usize {
    floor_char_boundary(s, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": path.to_str().unwrap()});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "hello world\n");
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": "/nonexistent/path/file.txt"});
        let result = tool.execute(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_file_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lines.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": path.to_str().unwrap(), "line_numbers": true});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "1\ta\n2\tb\n3\tc\n");
    }

    #[tokio::test]
    async fn test_read_file_offset_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        std::fs::write(&path, "l1\nl2\nl3\nl4\nl5\n").unwrap();
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": path.to_str().unwrap(), "offset": 2, "limit": 2, "line_numbers": true});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result, "2\tl2\n3\tl3\n");
    }

    #[tokio::test]
    async fn test_read_file_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.txt");
        std::fs::write(&path, "x\ny\n").unwrap();
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": path.to_str().unwrap(), "metadata": true});
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("size: 4 bytes"));
        assert!(result.contains("binary: false"));
    }

    #[tokio::test]
    async fn test_read_file_binary_detection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, b"abc\x00def").unwrap();
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": path.to_str().unwrap()});
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("[binary file"));
    }

    #[tokio::test]
    async fn test_read_file_offset_past_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "only one line\n").unwrap();
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": path.to_str().unwrap(), "offset": 999});
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("past end of file"));
    }

    #[tokio::test]
    async fn test_read_file_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        // Many short lines so the byte budget triggers, not the line budget.
        let big_content = "x\n".repeat(MAX_BYTES);
        std::fs::write(&path, &big_content).unwrap();
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": path.to_str().unwrap()});
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("[truncated"));
    }

    #[test]
    fn test_floor_char_boundary_helper() {
        // ASCII: boundary at every byte.
        assert_eq!(floor_char_boundary("hello", 3), 3);
        assert_eq!(floor_char_boundary("hello", 0), 0);
        assert_eq!(floor_char_boundary("hello", 100), 5);

        // "€" = 3 bytes (0xE2 0x82 0xAC), "é" = 2 bytes (0xC3 0xA9).
        let s = "€é";
        assert_eq!(floor_char_boundary(s, 1), 0);
        assert_eq!(floor_char_boundary(s, 2), 0);
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 4), 3);
        assert_eq!(floor_char_boundary(s, 5), 5);
    }
}
