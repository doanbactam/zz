//! `apply_patch` tool — apply a unified diff patch with hunk-level
//! preview / approve / reject parity round 4 F20).
//!
//! The agent can propose file changes as a unified diff string instead of
//! calling `edit_file` directly. The tool parses the diff into per-file
//! hunks, validates write paths against the sandbox, and — depending on the
//! approval policy — either auto-applies, or returns a structured JSON
//! preview of every hunk so the user (TUI) / exec caller can approve or
//! reject individual hunks, then re-invokes with an explicit selection.
//!
//! Pure, deterministic, no LLM / network access (§7.1 safe).

use crate::snapshot;
use crate::{Tool, get_str};
use std::path::Path;
use std::sync::Arc;
use zerozero_sandbox::{ApprovalPolicy, SandboxPolicy, validate_write_path};

/// A single line within a patch hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    /// Unchanged context line (prefixed with ' ').
    Context(String),
    /// Line added by the patch (prefixed with '+').
    Add(String),
    /// Line removed by the patch (prefixed with '-').
    Remove(String),
}

impl HunkLine {
    /// Text of the line without the diff prefix.
    pub fn text(&self) -> &str {
        match self {
            Self::Context(s) | Self::Add(s) | Self::Remove(s) => s,
        }
    }

    /// The original (old-side) content contributed by this line: context and
    /// removed lines; added lines contribute nothing to the old side.
    fn old_side(&self) -> Option<&str> {
        match self {
            Self::Context(s) | Self::Remove(s) => Some(s),
            Self::Add(_) => None,
        }
    }

    /// The new-side content contributed by this line: context and added
    /// lines; removed lines contribute nothing to the new side.
    fn new_side(&self) -> Option<&str> {
        match self {
            Self::Context(s) | Self::Add(s) => Some(s),
            Self::Remove(_) => None,
        }
    }
}

/// A single unified-diff hunk (`@@ -a,b +c,d @@` plus body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 1-indexed start line in the *original* file (old side).
    pub old_start: usize,
    /// Number of lines on the old side.
    pub old_count: usize,
    /// 1-indexed start line in the *new* file (new side).
    pub new_start: usize,
    /// Number of lines on the new side.
    pub new_count: usize,
    /// Optional section heading trailing the hunk header.
    pub section: Option<String>,
    /// Body lines in order.
    pub lines: Vec<HunkLine>,
}

/// A file entry in a parsed patch: old/new paths + hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFile {
    /// Old path (`---` line), after stripping git `a/` prefix, or `/dev/null`.
    pub old_path: String,
    /// New path (`+++` line), after stripping git `b/` prefix, or `/dev/null`.
    pub new_path: String,
    /// Hunks for this file, in file order.
    pub hunks: Vec<Hunk>,
}

/// A parsed unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPatch {
    pub files: Vec<PatchFile>,
}

/// Strip a leading `a/` or `b/` git prefix (and bare `/dev/null`) from a
/// diff path component.
fn strip_diff_prefix(p: &str) -> String {
    let p = p.trim();
    if p == "/dev/null" {
        return "/dev/null".to_string();
    }
    let no_tab = p.split('\t').next().unwrap_or(p).trim();
    if let Some(rest) = no_tab.strip_prefix("a/") {
        // Only treat `a/` as a git prefix when the remainder is not itself an
        // absolute path (git emits `a/<abspath>` for files outside the repo,
        // and we must keep the leading slash).
        if rest.starts_with('/') {
            return no_tab.to_string();
        }
        return rest.to_string();
    }
    if let Some(rest) = no_tab.strip_prefix("b/") {
        if rest.starts_with('/') {
            return no_tab.to_string();
        }
        return rest.to_string();
    }
    no_tab.to_string()
}

/// Parse a unified-diff string into a [`ParsedPatch`].
pub fn parse_patch(diff: &str) -> anyhow::Result<ParsedPatch> {
    let lines: Vec<&str> = diff.lines().collect();
    let mut files: Vec<PatchFile> = Vec::new();
    let mut cur: Option<PatchFile> = None;
    let mut cur_hunk: Option<Hunk> = None;

    let flush_hunk = |cur: &mut Option<PatchFile>, h: &mut Option<Hunk>| {
        if let (Some(f), Some(hk)) = (cur.as_mut(), h.take()) {
            f.hunks.push(hk);
        }
    };
    let flush_file =
        |files: &mut Vec<PatchFile>, cur: &mut Option<PatchFile>, h: &mut Option<Hunk>| {
            flush_hunk(cur, h);
            if let Some(f) = cur.take() {
                files.push(f);
            }
        };

    for raw in lines {
        let line = raw.strip_prefix('\r').unwrap_or(raw);
        if line.starts_with("--- ") {
            flush_file(&mut files, &mut cur, &mut cur_hunk);
            // Hold the old path; the next `+++ ` line completes the file.
            let old_path = strip_diff_prefix(line.strip_prefix("--- ").unwrap_or(""));
            cur = Some(PatchFile {
                old_path,
                new_path: String::new(),
                hunks: Vec::new(),
            });
            continue;
        }
        if line.starts_with("+++ ") {
            if let Some(f) = cur.as_mut() {
                f.new_path = strip_diff_prefix(line.strip_prefix("+++ ").unwrap_or(""));
            }
            continue;
        }
        if line.starts_with("@@") {
            flush_hunk(&mut cur, &mut cur_hunk);
            let hk = parse_hunk_header(line)
                .ok_or_else(|| anyhow::anyhow!("malformed hunk header: {line}"))?;
            cur_hunk = Some(hk);
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            if let Some(h) = cur_hunk.as_mut() {
                h.lines.push(HunkLine::Add(rest.to_string()));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix('-') {
            if let Some(h) = cur_hunk.as_mut() {
                h.lines.push(HunkLine::Remove(rest.to_string()));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(h) = cur_hunk.as_mut() {
                h.lines.push(HunkLine::Context(rest.to_string()));
            }
            continue;
        }
        // '\' continuation lines (e.g. "\ No newline at end of file") — ignore.
        if line.starts_with('\\') {
            continue;
        }
        // Blank line or anything else between hunks: end current hunk.
        flush_hunk(&mut cur, &mut cur_hunk);
    }
    flush_file(&mut files, &mut cur, &mut cur_hunk);

    if files.is_empty() {
        anyhow::bail!("no parseable file diffs found in patch");
    }
    Ok(ParsedPatch { files })
}

/// Parse an `@@ -old,count +new,count @@ optional section` header.
fn parse_hunk_header(header: &str) -> Option<Hunk> {
    // Find the opening @@ and the closing @@.
    let open = header.find("@@")?;
    let rest = &header[open + 2..];
    let close = rest.find("@@")?;
    let spec = rest[..close].trim();
    let section = {
        let after = rest[close + 2..].trim();
        if after.is_empty() {
            None
        } else {
            Some(after.to_string())
        }
    };

    // spec looks like "-old,oldc +new,newc" (count defaults to 1).
    let mut old_start = 0usize;
    let mut old_count = 1usize;
    let mut new_start = 0usize;
    let mut new_count = 1usize;

    let mut parts = spec.split_whitespace();
    if let Some(old_spec) = parts.next() {
        let o = old_spec.strip_prefix('-')?;
        let (s, c) = split_range(o)?;
        old_start = s;
        old_count = c;
    }
    if let Some(new_spec) = parts.next() {
        let n = new_spec.strip_prefix('+')?;
        let (s, c) = split_range(n)?;
        new_start = s;
        new_count = c;
    }

    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        section,
        lines: Vec::new(),
    })
}

/// Split a "start,count" range; count defaults to 1 when omitted.
fn split_range(s: &str) -> Option<(usize, usize)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// Apply the approved hunks of a single file to its original content,
/// returning the new content. Hunks must be supplied in file order.
pub fn apply_hunks(original: &str, hunks: &[Hunk]) -> anyhow::Result<String> {
    // Preserve a trailing newline if the original ended with one; unify line
    // iteration without losing that final newline.
    let trailing_nl = original.ends_with('\n');
    let mut lines: Vec<String> = original.lines().map(|s| s.to_string()).collect();
    // Track the cumulative line offset caused by previously applied hunks,
    // because hunk `old_start` values are relative to the *original* file.
    let mut applied_offset: isize = 0;

    for (hi, hunk) in hunks.iter().enumerate() {
        // Build the old-side block (context + removed) and new-side block
        // (context + added).
        let old_block: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|l| l.old_side().map(|s| s.to_string()))
            .collect();
        let new_block: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|l| l.new_side().map(|s| s.to_string()))
            .collect();

        // Resolve the position to splice at.
        let at = resolve_position(&lines, &old_block, hunk.old_start, applied_offset).ok_or_else(
            || {
                anyhow::anyhow!(
                    "hunk {} does not apply (context mismatch at expected line {})",
                    hi + 1,
                    hunk.old_start
                )
            },
        )?;

        let replace_len = old_block.len();
        lines.splice(at..at + replace_len, new_block.iter().cloned());

        applied_offset += new_block.len() as isize - old_block.len() as isize;
    }

    let mut out = lines.join("\n");
    if trailing_nl && !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

/// Find the 0-based splice index for a hunk: prefer the anchored expected
/// position, then fall back to a full scan, then (for pure insertions) clamp
/// to the end of the file.
fn resolve_position(
    lines: &[String],
    old_block: &[String],
    old_start: usize,
    applied_offset: isize,
) -> Option<usize> {
    let expected = (old_start as isize - 1 + applied_offset).max(0) as usize;
    if old_block.is_empty() {
        // Pure insertion: clamp to a valid index.
        return Some(expected.min(lines.len()));
    }
    if expected + old_block.len() <= lines.len()
        && lines[expected..expected + old_block.len()] == *old_block
    {
        return Some(expected);
    }
    if lines.is_empty() {
        return None;
    }
    (0..=lines.len().saturating_sub(old_block.len()))
        .find(|&i| lines[i..i + old_block.len()] == *old_block)
}

/// Decision for a single hunk, keyed by file index + hunk index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum HunkDecision {
    /// Approve this hunk for application.
    Approve,
    /// Reject this hunk (leave file unchanged for this hunk).
    Reject,
}

/// The `apply_patch` tool.
pub struct ApplyPatchTool {
    sandbox: Arc<SandboxPolicy>,
    approval: Arc<ApprovalPolicy>,
}

impl ApplyPatchTool {
    pub const fn new(sandbox: Arc<SandboxPolicy>, approval: Arc<ApprovalPolicy>) -> Self {
        Self { sandbox, approval }
    }

    /// Parse the patch (exposed for preview / TUI reuse).
    pub fn parse(&self, diff: &str) -> anyhow::Result<ParsedPatch> {
        parse_patch(diff)
    }

    /// Determine the default decision for hunks given the approval policy when
    /// the caller does not supply an explicit selection.
    ///
    /// * `Never` → auto-approve everything (no prompts).
    /// * `Untrusted` → deny everything (patches are powerful, treat like
    ///   critical).
    /// * `OnRequest` → preview only: no hunk applied until the caller makes an
    ///   explicit per-hunk selection.
    fn default_approved(&self) -> bool {
        matches!(*self.approval, ApprovalPolicy::Never)
    }
}

#[async_trait::async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a unified-diff patch to files, with hunk-level preview/approve. \
         Provide `patch` (unified diff string). Returns a JSON preview of all \
         hunks. To apply, pass `approve`: true (all hunks), false (none), or \
         an array of {\"file\": <index>, \"hunk\": <index>} to apply only those \
         hunks. Without `approve` under OnRequest/Untrusted policy, no file is \
         modified — only the preview is returned."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Unified-diff patch string to apply"
                },
                "approve": {
                    "description": "Approval selection. true = apply all hunks; \
                        false = preview only; or an array of \
                        {\"file\": int, \"hunk\": int} to apply specific hunks.",
                    "oneOf": [
                        { "type": "boolean" },
                        {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file": { "type": "integer" },
                                    "hunk": { "type": "integer" }
                                },
                                "required": ["file", "hunk"]
                            }
                        }
                    ]
                }
            },
            "required": ["patch"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let diff = get_str(args, "patch")?;
        let parsed = self.parse(&diff)?;

        // Count hunks and build a preview array.
        let mut preview_files = Vec::new();
        let mut total_hunks = 0usize;
        for (fi, f) in parsed.files.iter().enumerate() {
            let mut hunk_previews = Vec::new();
            for (hi, h) in f.hunks.iter().enumerate() {
                total_hunks += 1;
                hunk_previews.push(serde_json::json!({
                    "index": hi,
                    "old_start": h.old_start,
                    "old_count": h.old_count,
                    "new_start": h.new_start,
                    "new_count": h.new_count,
                    "section": h.section,
                    "adds": h.lines.iter().filter(|l| matches!(l, HunkLine::Add(_))).count(),
                    "removes": h.lines.iter().filter(|l| matches!(l, HunkLine::Remove(_))).count(),
                }));
            }
            preview_files.push(serde_json::json!({
                "index": fi,
                "old_path": f.old_path,
                "new_path": f.new_path,
                "hunks": hunk_previews,
            }));
        }

        // Resolve the approval selection.
        let default_approved = self.default_approved();
        let selection: Vec<(usize, usize)> = match args.get("approve") {
            Some(serde_json::Value::Bool(true)) => {
                // Apply all hunks.
                parsed
                    .files
                    .iter()
                    .enumerate()
                    .flat_map(|(fi, f)| (0..f.hunks.len()).map(move |hi| (fi, hi)))
                    .collect()
            }
            Some(serde_json::Value::Bool(false)) => Vec::new(),
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| {
                    let fi = v.get("file")?.as_u64()? as usize;
                    let hi = v.get("hunk")?.as_u64()? as usize;
                    Some((fi, hi))
                })
                .collect(),
            // No `approve` arg: fall back to policy default.
            None => {
                if default_approved {
                    parsed
                        .files
                        .iter()
                        .enumerate()
                        .flat_map(|(fi, f)| (0..f.hunks.len()).map(move |hi| (fi, hi)))
                        .collect()
                } else {
                    Vec::new()
                }
            }
            Some(_) => anyhow::bail!("invalid `approve` argument"),
        };

        // Anything to apply?
        if selection.is_empty() {
            return Ok(serde_json::json!({
                "applied": false,
                "policy": format!("{:?}", *self.approval),
                "files": preview_files.len(),
                "hunks_total": total_hunks,
                "hunks_applied": 0,
                "hunks_rejected": total_hunks,
                "preview": preview_files,
                "note": "No hunks applied. Re-invoke with `approve` to apply."
            })
            .to_string());
        }

        // Validate every target path against the sandbox before touching disk.
        for f in &parsed.files {
            let target = if f.new_path == "/dev/null" {
                &f.old_path
            } else {
                &f.new_path
            };
            if target == "/dev/null" {
                continue;
            }
            let p = Path::new(target);
            validate_write_path(&self.sandbox, p)?;
        }

        // Apply the selected hunks per file, grouping by file index.
        let mut applied_count = 0usize;
        for (fi, f) in parsed.files.iter().enumerate() {
            let chosen: Vec<Hunk> = f
                .hunks
                .iter()
                .enumerate()
                .filter(|(hi, _)| selection.contains(&(fi, *hi)))
                .map(|(_, h)| h.clone())
                .collect();
            if chosen.is_empty() {
                continue;
            }
            let target = if f.new_path == "/dev/null" {
                f.old_path.clone()
            } else {
                f.new_path.clone()
            };
            if target == "/dev/null" {
                // Deletion hunk: remove the file (snapshot first).
                let p = Path::new(&f.old_path);
                snapshot::capture(p);
                std::fs::remove_file(p)
                    .map_err(|e| anyhow::anyhow!("failed to delete {}: {e}", f.old_path))?;
                applied_count += chosen.len();
                continue;
            }
            let p = Path::new(&target);
            let original = if p.exists() {
                tokio::fs::read_to_string(p).await?
            } else {
                String::new()
            };
            let new_content = apply_hunks(&original, &chosen)?;
            snapshot::capture(p);
            tokio::fs::write(p, new_content).await?;
            applied_count += chosen.len();
        }

        Ok(serde_json::json!({
            "applied": true,
            "policy": format!("{:?}", *self.approval),
            "files": preview_files.len(),
            "hunks_total": total_hunks,
            "hunks_applied": applied_count,
            "hunks_rejected": total_hunks - applied_count,
            "preview": preview_files,
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@
 line one
-old line two
+new line two
 line three
@@ -5,2 +5,3 @@
 line five
-line six
+line six updated
+line six b
";

    #[test]
    fn test_parse_patch_structure() {
        let p = parse_patch(SAMPLE).unwrap();
        assert_eq!(p.files.len(), 1);
        let f = &p.files[0];
        assert_eq!(f.old_path, "foo.txt");
        assert_eq!(f.new_path, "foo.txt");
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(f.hunks[0].old_start, 1);
        assert_eq!(f.hunks[0].old_count, 3);
        assert_eq!(f.hunks[0].new_count, 3);
        // First hunk: context, remove, add, context.
        assert!(matches!(f.hunks[0].lines[0], HunkLine::Context(_)));
        assert!(matches!(f.hunks[0].lines[1], HunkLine::Remove(_)));
        assert!(matches!(f.hunks[0].lines[2], HunkLine::Add(_)));
    }

    #[test]
    fn test_apply_hunks_correctly() {
        let p = parse_patch(SAMPLE).unwrap();
        let original = "line one\nold line two\nline three\nline four\nline five\nline six\n";
        let out = apply_hunks(original, &p.files[0].hunks).unwrap();
        let expected = "line one\nnew line two\nline three\nline four\nline five\nline six updated\nline six b\n";
        assert_eq!(out, expected);
    }

    #[tokio::test]
    async fn test_apply_patch_tool_creates_and_applies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.txt");
        std::fs::write(
            &path,
            "line one\nold line two\nline three\nline four\nline five\nline six\n",
        )
        .unwrap();
        let tool = ApplyPatchTool::new(
            Arc::new(SandboxPolicy::FullAccess),
            Arc::new(ApprovalPolicy::Never),
        );
        let diff = format!(
            "--- {0}\n+++ {0}\n@@ -1,3 +1,3 @@\n line one\n-old line two\n+new line two\n line three\n",
            path.display()
        );
        let args = serde_json::json!({
            "patch": diff,
            "approve": true
        });
        let result = tool.execute(&args).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["applied"], true);
        assert_eq!(v["hunks_applied"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line one\nnew line two\nline three\nline four\nline five\nline six\n"
        );
    }

    #[tokio::test]
    async fn test_apply_patch_preview_only_when_not_approved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let tool = ApplyPatchTool::new(
            Arc::new(SandboxPolicy::FullAccess),
            Arc::new(ApprovalPolicy::OnRequest),
        );
        let diff = format!(
            "--- a{0}\n+++ b{0}\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n",
            path.display()
        );
        let args = serde_json::json!({ "patch": diff });
        let result = tool.execute(&args).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["applied"], false);
        assert_eq!(v["hunks_applied"], 0);
        // File unchanged.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\nc\n");
    }

    #[tokio::test]
    async fn test_apply_patch_partial_hunk_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.txt");
        std::fs::write(
            &path,
            "line one\nold line two\nline three\nline five\nline six\n",
        )
        .unwrap();
        let tool = ApplyPatchTool::new(
            Arc::new(SandboxPolicy::FullAccess),
            Arc::new(ApprovalPolicy::OnRequest),
        );
        // Two hunks; approve only the second.
        let diff = format!(
            "\
--- {0}
+++ {0}
@@ -1,3 +1,3 @@
 line one
-old line two
+new line two
 line three
@@ -4,2 +4,2 @@
 line five
-line six
+line six updated
",
            path.display()
        );
        let args = serde_json::json!({
            "patch": diff,
            "approve": [ { "file": 0, "hunk": 1 } ]
        });
        let result = tool.execute(&args).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["hunks_applied"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line one\nold line two\nline three\nline five\nline six updated\n"
        );
    }

    #[tokio::test]
    async fn test_apply_patch_readonly_deny() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let tool = ApplyPatchTool::new(
            Arc::new(SandboxPolicy::ReadOnly),
            Arc::new(ApprovalPolicy::Never),
        );
        let diff = format!(
            "--- a{0}\n+++ b{0}\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n",
            path.display()
        );
        let args = serde_json::json!({ "patch": diff, "approve": true });
        let result = tool.execute(&args).await;
        assert!(result.is_err());
        // File unchanged.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\nc\n");
    }

    #[tokio::test]
    async fn test_apply_patch_untrusted_denies_without_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let tool = ApplyPatchTool::new(
            Arc::new(SandboxPolicy::FullAccess),
            Arc::new(ApprovalPolicy::Untrusted),
        );
        let diff = format!(
            "--- a{0}\n+++ b{0}\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n",
            path.display()
        );
        // No explicit approve under Untrusted → preview only, nothing applied.
        let args = serde_json::json!({ "patch": diff });
        let result = tool.execute(&args).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["applied"], false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\nc\n");
    }

    #[test]
    fn test_parse_patch_rejects_garbage() {
        let r = parse_patch("this is not a diff\njust text\n");
        assert!(r.is_err());
    }
}
